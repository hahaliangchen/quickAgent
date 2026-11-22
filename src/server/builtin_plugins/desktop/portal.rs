//! Consent-gated XDG RemoteDesktop input with compositor coordinates.
use super::{kwin, *};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    time::Duration,
};
use zbus::{
    Connection, Proxy,
    fdo::DBusProxy,
    zvariant::{OwnedObjectPath, OwnedValue, Value as Variant},
};

const SERVICE: &str = "org.freedesktop.portal.Desktop";
const DESKTOP: &str = "/org/freedesktop/portal/desktop";
const REMOTE: &str = "org.freedesktop.portal.RemoteDesktop";
const SCREENCAST: &str = "org.freedesktop.portal.ScreenCast";
const REQUEST: &str = "org.freedesktop.portal.Request";
const SESSION: &str = "org.freedesktop.portal.Session";
const DEVICES: u32 = 3;
const TIMEOUT: Duration = Duration::from_secs(3);
const START_TIMEOUT: Duration = Duration::from_secs(60);
const LEFT: i32 = 0x110;
const RIGHT: i32 = 0x111;

type Options<'a> = HashMap<&'a str, Variant<'a>>;
type Results = HashMap<String, OwnedValue>;

fn identifier() -> String {
    format!(
        "quickagent_{}",
        crate::server::agent_service::uuid_like().replace('-', "")
    )
}

fn path(conn: &Connection, kind: &str, token: &str) -> Result<OwnedObjectPath> {
    let sender = conn
        .unique_name()
        .context("portal requires the session bus")?;
    let sender = sender.as_str().trim_start_matches(':').replace('.', "_");
    Ok(OwnedObjectPath::try_from(format!(
        "{DESKTOP}/{kind}/{sender}/{token}"
    ))?)
}

fn backend_error(error: impl std::fmt::Display) -> anyhow::Error {
    fault(
        "PORTAL_UNAVAILABLE",
        format!("RemoteDesktop portal: {error}"),
    )
}

struct Handle {
    conn: Connection,
    owner: String,
    path: OwnedObjectPath,
    interface: &'static str,
    armed: bool,
}
impl Handle {
    fn new(conn: &Connection, owner: &str, path: OwnedObjectPath, interface: &'static str) -> Self {
        Self {
            conn: conn.clone(),
            owner: owner.to_owned(),
            path,
            interface,
            armed: true,
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        if self.armed {
            close(
                self.conn.clone(),
                self.owner.clone(),
                self.path.clone(),
                self.interface,
            );
        }
    }
}

fn close(conn: Connection, owner: String, path: OwnedObjectPath, interface: &'static str) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move {
            let _ = tokio::time::timeout(
                TIMEOUT,
                conn.call_method(
                    Some(owner.as_str()),
                    path.as_str(),
                    Some(interface),
                    "Close",
                    &(),
                ),
            )
            .await;
        });
    }
}

#[derive(Clone, Copy)]
enum Request<'a> {
    Create(&'a str),
    Devices(&'a OwnedObjectPath, Option<&'a str>),
    Sources(&'a OwnedObjectPath),
    Start(&'a OwnedObjectPath),
}
impl<'a> Request<'a> {
    fn options(self, request_token: &'a str) -> Options<'a> {
        let mut options = Options::from([("handle_token", Variant::from(request_token))]);
        match self {
            Self::Create(session) => {
                options.insert("session_handle_token", Variant::from(session));
            }
            Self::Devices(_, saved) => {
                options.insert("persist_mode", Variant::from(2u32));
                if let Some(saved) = saved {
                    options.insert("restore_token", Variant::from(saved));
                }
                options.insert("types", Variant::from(DEVICES));
            }
            Self::Sources(_) => {
                // In a combined session only RemoteDesktop manages persistence.
                options.insert("types", Variant::from(1u32));
                options.insert("multiple", Variant::from(false));
                options.insert("cursor_mode", Variant::from(2u32));
            }
            Self::Start(_) => {}
        }
        options
    }
}

async fn request(
    conn: &Connection,
    owner: &str,
    call: Request<'_>,
    cancel: &CancellationToken,
) -> Result<Results> {
    let token = identifier();
    let expected = path(conn, "request", &token)?;
    let mut handle = Handle::new(conn, owner, expected.clone(), REQUEST);
    let work = async {
        let proxy = Proxy::new(conn, owner, expected.as_str(), REQUEST)
            .await
            .map_err(backend_error)?;
        let mut responses = proxy
            .receive_signal("Response")
            .await
            .map_err(backend_error)?;
        let options = call.options(&token);
        let remote = Proxy::new(conn, owner, DESKTOP, REMOTE)
            .await
            .map_err(backend_error)?;
        let returned: OwnedObjectPath = match call {
            Request::Create(_) => remote.call("CreateSession", &(options,)).await,
            Request::Devices(session, _) => remote.call("SelectDevices", &(session, options)).await,
            Request::Sources(session) => {
                let screen = Proxy::new(conn, owner, DESKTOP, SCREENCAST)
                    .await
                    .map_err(backend_error)?;
                screen.call("SelectSources", &(session, options)).await
            }
            Request::Start(session) => remote.call("Start", &(session, "", options)).await,
        }
        .map_err(backend_error)?;
        if returned != expected {
            close(conn.clone(), owner.to_owned(), returned, REQUEST);
            return Err(backend_error("unexpected request path"));
        }
        let signal = responses
            .next()
            .await
            .context("portal response stream disconnected")?;
        let (code, results): (u32, Results) = signal.body().deserialize().map_err(backend_error)?;
        handle.armed = false;
        response(code, results)
    };
    let duration = if matches!(call, Request::Start(_)) {
        START_TIMEOUT
    } else {
        TIMEOUT
    };
    tokio::select! { biased;
        _ = cancel.cancelled() => Err(fault("CANCELLED", "desktop task cancelled")),
        outcome = tokio::time::timeout(duration, work) => outcome.map_err(|_| fault("PORTAL_TIMEOUT", "RemoteDesktop portal request timed out"))?,
    }
}

fn response(code: u32, results: Results) -> Result<Results> {
    if code != 0 {
        return Err(fault(
            "PORTAL_DENIED",
            "Remote desktop permission was denied or cancelled by the user",
        ));
    }
    Ok(results)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stream {
    id: u32,
    region: kwin::Rect,
}

fn grant(mut results: Results) -> Result<(Stream, Option<String>)> {
    let devices = results
        .remove("devices")
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);
    ensure!(
        devices & DEVICES == DEVICES,
        fault(
            "PORTAL_DENIED",
            "Portal did not grant both keyboard and pointer"
        )
    );
    let mut streams: Vec<(u32, Results)> = results
        .remove("streams")
        .context("portal omitted the monitor stream")?
        .try_into()?;
    ensure!(
        streams.len() == 1,
        backend_error("expected exactly one consented monitor stream")
    );
    let (id, mut properties) = streams.pop().context("portal omitted the monitor stream")?;
    let rect = |properties: &mut Results, field| -> Result<(i32, i32)> {
        Ok(properties
            .remove(field)
            .context("portal omitted logical monitor geometry")?
            .try_into()?)
    };
    let (x, y) = rect(&mut properties, "position")?;
    let field = if properties.contains_key("logical_size") {
        "logical_size"
    } else {
        "size"
    };
    let (width, height) = rect(&mut properties, field)?;
    ensure!(
        width > 0 && height > 0,
        backend_error("invalid logical monitor geometry")
    );
    let stream = Stream {
        id,
        region: kwin::Rect {
            x,
            y,
            width: width as u32,
            height: height as u32,
        },
    };
    let token = results
        .remove("restore_token")
        .and_then(|v| String::try_from(v).ok())
        .filter(|s| valid_token(s));
    Ok((stream, token))
}

#[derive(Deserialize, Serialize)]
struct SavedToken {
    restore_token: String,
}
fn token_file() -> Option<PathBuf> {
    std::env::var_os("QUICKAGENT_WAYLAND_PORTAL_TOKEN")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::home_dir().map(|dir| dir.join(".config/grow/wayland-remote-token.json"))
        })
}
fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 1024 && !token.chars().any(char::is_control)
}
fn load_token(path: &Path) -> Result<Option<String>> {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        result => result?,
    };
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.permissions().mode() & 0o777 == 0o600 && meta.len() <= 4096,
        "portal token file must be a regular private 0600 file"
    );
    let mut data = Vec::new();
    file.take(4097).read_to_end(&mut data)?;
    let token: SavedToken =
        serde_json::from_slice(&data).map_err(|_| anyhow::anyhow!("invalid portal token file"))?;
    ensure!(valid_token(&token.restore_token), "invalid portal token");
    Ok(Some(token.restore_token))
}
fn save_token(path: &Path, token: String) -> Result<()> {
    ensure!(valid_token(&token), "invalid portal token");
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer(
        &mut file,
        &SavedToken {
            restore_token: token,
        },
    )?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

struct Session {
    lease: Handle,
    stream: Stream,
    closed: CancellationToken,
    close_once: std::sync::atomic::AtomicBool,
}
impl Session {
    fn stop(&self) {
        self.closed.cancel();
        if !self
            .close_once
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            close(
                self.lease.conn.clone(),
                self.lease.owner.clone(),
                self.lease.path.clone(),
                SESSION,
            );
        }
    }

    async fn emit(&self, event: Event) -> Result<()> {
        let opts = Options::new();
        let path = &self.lease.path;
        let work = async {
            let remote =
                Proxy::new(&self.lease.conn, self.lease.owner.as_str(), DESKTOP, REMOTE).await?;
            match event {
                Event::Move(x, y) => {
                    remote
                        .call(
                            "NotifyPointerMotionAbsolute",
                            &(path, opts, self.stream.id, x, y),
                        )
                        .await
                }
                Event::Button(code, down) => {
                    remote
                        .call("NotifyPointerButton", &(path, opts, code, u32::from(down)))
                        .await
                }
                Event::Scroll(steps) => {
                    remote
                        .call("NotifyPointerAxisDiscrete", &(path, opts, 0u32, steps))
                        .await
                }
                Event::Key(code, down) => {
                    remote
                        .call("NotifyKeyboardKeysym", &(path, opts, code, u32::from(down)))
                        .await
                }
            }
        };
        tokio::time::timeout(TIMEOUT, work)
            .await
            .map_err(|_| backend_error("input timed out"))?
            .map_err(backend_error)
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

struct State<S> {
    epoch: CancellationToken,
    current: Option<(CancellationToken, Arc<S>)>,
}
impl<S> Default for State<S> {
    fn default() -> Self {
        Self {
            epoch: CancellationToken::new(),
            current: None,
        }
    }
}
impl<S> State<S> {
    fn live(&mut self) -> Option<Arc<S>> {
        if self
            .current
            .as_ref()
            .is_some_and(|(closed, _)| closed.is_cancelled())
        {
            self.current = None;
        }
        self.current.as_ref().map(|(_, session)| session.clone())
    }
    fn store(
        &mut self,
        epoch: &CancellationToken,
        closed: CancellationToken,
        session: Arc<S>,
    ) -> bool {
        if epoch.is_cancelled() || closed.is_cancelled() {
            return false;
        }
        self.current = Some((closed, session));
        true
    }
    fn stop(&mut self) -> Option<Arc<S>> {
        self.epoch.cancel();
        self.epoch = CancellationToken::new();
        self.current.take().map(|(_, session)| session)
    }
}

#[derive(Default)]
pub(super) struct Portal {
    state: Arc<Mutex<State<Session>>>,
    connect: tokio::sync::Mutex<()>,
}
impl Portal {
    pub(super) fn stop(&self) {
        if let Some(session) = self.state.lock().unwrap().stop() {
            session.stop();
        }
    }

    async fn session(&self, cancel: &CancellationToken) -> Result<Arc<Session>> {
        let _connect = self.connect.lock().await;
        guard::cancelled(cancel)?;
        let epoch = {
            let mut state = self.state.lock().unwrap();
            if let Some(session) = state.live() {
                return Ok(session);
            }
            state.epoch.clone()
        };
        // The caller and mount can independently stop consent; dropping the handshake closes pending handles.
        let session = tokio::select! { biased;
            _ = epoch.cancelled() => return Err(fault("CANCELLED", "desktop plugin was disabled")),
            _ = cancel.cancelled() => return Err(fault("CANCELLED", "desktop task cancelled")),
            result = handshake(cancel, Arc::downgrade(&self.state)) => result?,
        };
        ensure!(
            self.state
                .lock()
                .unwrap()
                .store(&epoch, session.closed.clone(), session.clone()),
            fault("PORTAL_UNAVAILABLE", "portal session closed during consent")
        );
        Ok(session)
    }
}

async fn handshake(
    cancel: &CancellationToken,
    state: std::sync::Weak<Mutex<State<Session>>>,
) -> Result<Arc<Session>> {
    let conn = tokio::time::timeout(TIMEOUT, Connection::session())
        .await
        .map_err(backend_error)?
        .map_err(backend_error)?;
    handshake_on(conn, cancel, state, token_file()).await
}

async fn handshake_on(
    conn: Connection,
    cancel: &CancellationToken,
    state: std::sync::Weak<Mutex<State<Session>>>,
    file: Option<PathBuf>,
) -> Result<Arc<Session>> {
    let owner = tokio::time::timeout(TIMEOUT, async {
        let owner = DBusProxy::new(&conn)
            .await?
            .get_name_owner(SERVICE.try_into()?)
            .await?
            .to_string();
        for (api, min) in [(REMOTE, 2u32), (SCREENCAST, 5u32)] {
            let proxy = Proxy::new(&conn, owner.as_str(), DESKTOP, api).await?;
            let version: u32 = proxy.get_property("version").await?;
            ensure!(version >= min, "{api} v{min} is required");
        }
        Ok::<_, anyhow::Error>(owner)
    })
    .await
    .map_err(backend_error)?
    .map_err(backend_error)?;
    let token = identifier();
    let session_path = path(&conn, "session", &token)?;
    let mut lease = Handle::new(&conn, &owner, session_path.clone(), SESSION);
    let session_proxy =
        Proxy::new_owned(conn.clone(), owner.clone(), session_path.clone(), SESSION)
            .await
            .map_err(backend_error)?;
    // Install the signal/owner watches before CreateSession can create the object.
    let mut closed = session_proxy
        .receive_signal("Closed")
        .await
        .map_err(backend_error)?;
    let owner_proxy = Proxy::new_owned(conn.clone(), SERVICE, DESKTOP, REMOTE)
        .await
        .map_err(backend_error)?;
    let mut owner_changes = owner_proxy
        .receive_owner_changed()
        .await
        .map_err(backend_error)?;
    let mut created = request(&conn, &owner, Request::Create(&token), cancel).await?;
    // The v2 interface historically encoded session_handle as a string, despite it being a path.
    let returned = created
        .remove("session_handle")
        .context("portal omitted session handle")?;
    let returned = match OwnedObjectPath::try_from(returned.try_clone()?) {
        Ok(path) => path.as_str().to_owned(),
        Err(_) => String::try_from(returned)?,
    };
    ensure!(
        returned == session_path.as_str(),
        backend_error("unexpected session handle")
    );
    let saved = file.as_deref().and_then(|path| match load_token(path) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "could not read Wayland portal token");
            None
        }
    });
    request(
        &conn,
        &owner,
        Request::Devices(&session_path, saved.as_deref()),
        cancel,
    )
    .await?;
    request(&conn, &owner, Request::Sources(&session_path), cancel).await?;
    let (stream, token) =
        grant(request(&conn, &owner, Request::Start(&session_path), cancel).await?)?;
    if let (Some(path), Some(token)) = (file, token)
        && let Err(error) = save_token(&path, token)
    {
        tracing::warn!(%error, "could not save Wayland portal token");
    }
    lease.armed = false;
    let session = Arc::new(Session {
        lease,
        stream,
        closed: CancellationToken::new(),
        close_once: false.into(),
    });
    let stop = session.closed.clone();
    tokio::spawn(async move {
        tokio::select! { biased;
            _ = stop.cancelled() => {},
            _ = closed.next() => {},
            _ = owner_changes.next() => {},
        }
        stop.cancel();
        if let Some(state) = state.upgrade() {
            // Remove only a closed cached session, not one installed by a later mount.
            let expired = {
                let mut state = state.lock().unwrap();
                if state
                    .current
                    .as_ref()
                    .is_some_and(|(closed, _)| closed.is_cancelled())
                {
                    state.current.take()
                } else {
                    None
                }
            };
            drop(expired);
        }
    });
    Ok(session)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Event {
    Move(f64, f64),
    Button(i32, bool),
    Scroll(i32),
    Key(i32, bool),
}

impl Event {
    fn release(self) -> Option<Self> {
        match self {
            Self::Key(key, true) => Some(Self::Key(key, false)),
            Self::Button(code, true) => Some(Self::Button(code, false)),
            _ => None,
        }
    }
}

// A dropped future may already have submitted a press. Record its release before awaiting the bus.
struct Input {
    session: Arc<Session>,
    pending: Vec<Event>,
    failed: bool,
}
impl Input {
    fn new(session: Arc<Session>) -> Self {
        Self {
            session,
            pending: Vec::new(),
            failed: false,
        }
    }
    async fn send(&mut self, event: Event, cancel: &CancellationToken) -> Result<()> {
        guard::cancelled(cancel)?;
        ensure!(
            !self.session.closed.is_cancelled(),
            backend_error("session closed; retry the action to request consent")
        );
        if let Some(up) = event.release() {
            self.pending.push(up);
        }
        let result = self.session.emit(event).await;
        self.failed |= result.is_err();
        if result.is_ok() && self.pending.last() == Some(&event) {
            self.pending.pop();
        }
        result
    }
    async fn release(&mut self) -> Result<()> {
        let mut error = None;
        for event in release_order(&self.pending) {
            if let Err(fail) = self.release_one(event).await {
                error.get_or_insert(fail);
            }
        }
        error.map_or(Ok(()), Err)
    }
    async fn finish(mut self, result: Result<()>) -> Result<()> {
        let cleanup = self.release().await;
        result.and(cleanup)
    }
}
impl Drop for Input {
    fn drop(&mut self) {
        if self.pending.is_empty() {
            if self.failed {
                self.session.stop();
            }
            return;
        }
        let (session, pending) = (self.session.clone(), std::mem::take(&mut self.pending));
        // Tear down after releasing on interruption; no subsequent action can reuse this session.
        session.closed.cancel();
        tokio::spawn(async move {
            for event in release_order(&pending) {
                let _ = session.emit(event).await;
            }
            session.stop();
        });
    }
}

fn release_order(pending: &[Event]) -> Vec<Event> {
    pending.iter().rev().copied().collect()
}

fn char_keysym(ch: char) -> i32 {
    let cp = ch as i32;
    if ch.is_ascii() { cp } else { 0x0100_0000 | cp }
}
fn key(name: &str) -> Result<i32> {
    let code = match name {
        "ctrl" => 0xffe3,
        "shift" => 0xffe1,
        "alt" => 0xffe9,
        "super" | "meta" => 0xffeb,
        "Return" => 0xff0d,
        "Tab" => 0xff09,
        "Escape" => 0xff1b,
        "BackSpace" => 0xff08,
        "Delete" => 0xffff,
        "Left" => 0xff51,
        "Up" => 0xff52,
        "Right" => 0xff53,
        "Down" => 0xff54,
        "Home" => 0xff50,
        "End" => 0xff57,
        "Page_Up" => 0xff55,
        "Page_Down" => 0xff56,
        "space" => 0x20,
        n if n.len() == 1 && n.as_bytes()[0].is_ascii_alphanumeric() => i32::from(n.as_bytes()[0]),
        n => n
            .strip_prefix('F')
            .and_then(|n| n.parse::<i32>().ok())
            .filter(|n| (1..=12).contains(n))
            .map(|n| 0xffbd + n)
            .ok_or_else(|| fault("UNSAFE_KEY", "unsupported Portal keysym"))?,
    };
    Ok(code)
}

impl Portal {
    pub(super) async fn keyboard(
        &self,
        window: &Window,
        input: &Keyboard,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let session = self.session(cancel).await?;
        let mut tx = Input::new(session);
        let chords = match input {
            Keyboard::Text(text) => text.chars().map(|ch| vec![char_keysym(ch)]).collect(),
            Keyboard::Key {
                modifiers,
                key: main,
            } => vec![
                modifiers
                    .iter()
                    .copied()
                    .chain([main.as_str()])
                    .map(key)
                    .collect::<Result<_>>()?,
            ],
        };
        let result = async {
            for keys in chords {
                guard::cancelled(cancel)?;
                kwin::verify(window, None, false, cancel).await?;
                for code in keys {
                    tx.send(Event::Key(code, true), cancel).await?;
                }
                tx.release().await?;
            }
            Ok(())
        }
        .await;
        tx.finish(result).await
    }

    pub(super) async fn mouse(
        &self,
        window: &Window,
        point: Point,
        action: Pointer,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let session = self.session(cancel).await?;
        let region = session.stream.region;
        region.local(point)?;
        if let Pointer::Drag(end) = action {
            region.local(end)?;
        }
        let mut tx = Input::new(session);
        let result = async {
            move_pointer(&mut tx, window, point, cancel).await?;
            if matches!(action, Pointer::Move) {
                return Ok(());
            }
            kwin::verify(window, Some(point), true, cancel).await?;
            match action {
                Pointer::Move => {}
                Pointer::Click(button) => {
                    click(&mut tx, if button == 3 { RIGHT } else { LEFT }, cancel).await?
                }
                Pointer::DoubleClick => {
                    click(&mut tx, LEFT, cancel).await?;
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    kwin::verify(window, Some(point), true, cancel).await?;
                    click(&mut tx, LEFT, cancel).await?;
                }
                Pointer::Scroll(delta) => {
                    let steps = delta.unsigned_abs().div_ceil(120) as i32 * delta.signum();
                    tx.send(Event::Scroll(steps), cancel).await?;
                }
                Pointer::Drag(end) => {
                    kwin::verify(window, Some(end), false, cancel).await?;
                    kwin::verify(window, Some(point), true, cancel).await?;
                    tx.send(Event::Button(LEFT, true), cancel).await?;
                    for i in 1..=4 {
                        let blend = |a: i32, b: i32| {
                            ((i64::from(a) * (4 - i) + i64::from(b) * i) / 4) as i32
                        };
                        move_pointer(
                            &mut tx,
                            window,
                            Point {
                                x: blend(point.x, end.x),
                                y: blend(point.y, end.y),
                            },
                            cancel,
                        )
                        .await?;
                    }
                    kwin::verify(window, Some(end), true, cancel).await?;
                    tx.release_one(Event::Button(LEFT, false)).await?;
                }
            }
            Ok(())
        }
        .await;
        tx.finish(result).await
    }
}

impl Input {
    async fn release_one(&mut self, event: Event) -> Result<()> {
        if !self.pending.contains(&event) {
            return Ok(());
        }
        let result = self.session.emit(event).await;
        if result.is_ok() {
            self.pending.retain(|up| *up != event);
        } else {
            self.failed = true;
        }
        result
    }
}
async fn click(tx: &mut Input, code: i32, cancel: &CancellationToken) -> Result<()> {
    tx.send(Event::Button(code, true), cancel).await?;
    tx.release_one(Event::Button(code, false)).await
}
async fn move_pointer(
    tx: &mut Input,
    window: &Window,
    point: Point,
    cancel: &CancellationToken,
) -> Result<()> {
    let (x, y) = tx.session.stream.region.local(point)?;
    kwin::verify(window, Some(point), false, cancel).await?;
    tx.send(Event::Move(x, y), cancel).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::{LE, Structure, serialized::Context, to_bytes};

    #[test]
    fn settings_use_portal_wire_types_and_single_monitor() {
        let path = OwnedObjectPath::try_from("/test/session").unwrap();
        let create = Request::Create("s1").options("r1");
        let devices = Request::Devices(&path, Some("saved")).options("r2");
        let sources = Request::Sources(&path).options("r3");
        let start = Request::Start(&path).options("r4");
        assert_eq!(create["session_handle_token"], Variant::from("s1"));
        assert_eq!(devices["types"], Variant::from(3u32));
        assert_eq!(sources["types"], Variant::from(1u32));
        assert_eq!(devices["persist_mode"], Variant::from(2u32));
        assert_eq!(devices["restore_token"], Variant::from("saved"));
        assert_eq!(sources["multiple"], Variant::from(false));
        assert!(!sources.contains_key("restore_token"));
        assert!(!sources.contains_key("persist_mode"));
        assert_eq!(start.len(), 1);
    }

    #[test]
    fn parses_dbus_grant_and_translates_monitor_local_coordinates() -> Result<()> {
        let properties: Results = [
            (
                "position".into(),
                OwnedValue::try_from(Variant::from(Structure::from((-1920i32, 80i32))))?,
            ),
            (
                "size".into(),
                OwnedValue::try_from(Variant::from(Structure::from((1920i32, 1080i32))))?,
            ),
        ]
        .into();
        let streams = vec![(73u32, properties)];
        let result: Results = [
            ("devices".into(), OwnedValue::from(DEVICES)),
            (
                "streams".into(),
                OwnedValue::try_from(Variant::from(streams))?,
            ),
            (
                "restore_token".into(),
                OwnedValue::try_from(Variant::from("saved"))?,
            ),
        ]
        .into();
        let bytes = to_bytes(Context::new_dbus(LE, 0), &(0u32, result))?;
        let ((code, decoded), _): ((u32, Results), _) = bytes.deserialize()?;
        let (stream, token) = grant(response(code, decoded)?)?;
        assert_eq!(stream.id, 73);
        assert_eq!(token.as_deref(), Some("saved"));
        assert_eq!(stream.region.local(Point { x: -1920, y: 80 })?, (0.0, 0.0));
        assert_eq!(
            stream.region.local(Point { x: -1, y: 1159 })?,
            (1919.0, 1079.0)
        );
        assert!(stream.region.local(Point { x: 0, y: 1159 }).is_err());
        let other = kwin::Rect {
            x: 200,
            y: 400,
            width: 40,
            height: 30,
        };
        assert_eq!(other.local(Point { x: 209, y: 401 })?, (9.0, 1.0));
        assert!(
            grant([("devices".into(), OwnedValue::from(1u32))].into())
                .unwrap_err()
                .to_string()
                .contains("PORTAL_DENIED")
        );
        Ok(())
    }

    #[test]
    fn keysym_and_chord_release_on_press_failure() -> Result<()> {
        assert_eq!(char_keysym('A'), 65);
        assert_eq!(char_keysym('\u{4e2d}'), 0x0100_4e2d);
        assert_eq!(key("F12")?, 0xffc9);
        assert_eq!(key("Delete")?, 0xffff);
        let keys = [key("ctrl")?, key("shift")?, key("a")?];
        // A reply can fail after the main key is pressed, so clean up that key too.
        let pending = keys.map(|key| Event::Key(key, true).release().unwrap());
        assert_eq!(
            release_order(&pending),
            [
                Event::Key(keys[2], false),
                Event::Key(keys[1], false),
                Event::Key(keys[0], false)
            ]
        );
        assert_eq!(
            release_order(&pending[..2]),
            [Event::Key(keys[1], false), Event::Key(keys[0], false)]
        );
        assert!(guard::keyboard(&json!({"keys":["ctrl","alt","F12"]})).is_err());
        Ok(())
    }

    #[test]
    fn restore_token_is_atomic_private_and_does_not_follow_symlinks() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("private/wayland-token.json");
        assert!(load_token(&path)?.is_none());
        save_token(&path, "first".into())?;
        save_token(&path, "second".into())?;
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(load_token(&path)?.as_deref(), Some("second"));
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link)?;
        assert!(load_token(&link).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        assert!(load_token(&path).is_err());
        Ok(())
    }

    #[derive(Clone, Default)]
    struct FakePortal {
        calls: Arc<Mutex<Vec<&'static str>>>,
        closed: Arc<tokio::sync::Notify>,
        start_response: Arc<std::sync::atomic::AtomicU32>,
    }

    struct FakeSession(Arc<tokio::sync::Notify>);
    #[zbus::interface(name = "org.freedesktop.portal.Session")]
    impl FakeSession {
        fn close(&self) {
            self.0.notify_one();
        }
    }

    fn client_path(sender: &str, kind: &str, token: &str) -> zbus::fdo::Result<OwnedObjectPath> {
        let sender = sender.trim_start_matches(':').replace('.', "_");
        OwnedObjectPath::try_from(format!("{DESKTOP}/{kind}/{sender}/{token}"))
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    fn opt<'a>(options: &'a Results, field: &str) -> zbus::fdo::Result<&'a str> {
        options
            .get(field)
            .and_then(|s| <&str>::try_from(s).ok())
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("missing {field}")))
    }

    async fn early_reply(
        conn: &Connection,
        sender: &str,
        options: &Results,
        code: u32,
        result: Results,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let request = client_path(sender, "request", opt(options, "handle_token")?)?;
        if code < 2 {
            // Respond before returning the method result to catch late subscriptions.
            conn.emit_signal(
                Some(sender),
                request.as_str(),
                REQUEST,
                "Response",
                &(code, result),
            )
            .await?;
        }
        Ok(request)
    }

    #[zbus::interface(name = "org.freedesktop.portal.RemoteDesktop")]
    impl FakePortal {
        #[zbus(property, name = "version")]
        fn version(&self) -> u32 {
            2
        }

        async fn create_session(
            &self,
            options: Results,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(connection)] conn: &Connection,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            let sender = header.sender().unwrap().as_str();
            let session = client_path(sender, "session", opt(&options, "session_handle_token")?)?;
            self.calls.lock().unwrap().push("CreateSession");
            conn.object_server()
                .at(session.as_str(), FakeSession(self.closed.clone()))
                .await?;
            let value = OwnedValue::try_from(Variant::from(session.as_str())).unwrap();
            early_reply(
                conn,
                sender,
                &options,
                0,
                [("session_handle".into(), value)].into(),
            )
            .await
        }

        async fn select_devices(
            &self,
            _session: OwnedObjectPath,
            options: Results,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(connection)] conn: &Connection,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            self.calls.lock().unwrap().push("SelectDevices");
            early_reply(
                conn,
                header.sender().unwrap().as_str(),
                &options,
                0,
                Results::new(),
            )
            .await
        }

        async fn start(
            &self,
            _session: OwnedObjectPath,
            _parent: &str,
            options: Results,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(connection)] conn: &Connection,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            self.calls.lock().unwrap().push("Start");
            let geometry: Results = [
                ("position".into(), (0i32, 0i32)),
                ("size".into(), (1920, 1080)),
            ]
            .into_iter()
            .map(|(key, pt)| {
                (
                    key,
                    OwnedValue::try_from(Variant::from(Structure::from(pt))).unwrap(),
                )
            })
            .collect();
            let result = [
                ("devices".into(), OwnedValue::from(DEVICES)),
                (
                    "streams".into(),
                    OwnedValue::try_from(Variant::from(vec![(17u32, geometry)])).unwrap(),
                ),
                (
                    "restore_token".into(),
                    OwnedValue::try_from(Variant::from("fake-token")).unwrap(),
                ),
            ]
            .into();
            let code = self
                .start_response
                .load(std::sync::atomic::Ordering::SeqCst);
            early_reply(
                conn,
                header.sender().unwrap().as_str(),
                &options,
                code,
                result,
            )
            .await
        }
    }

    struct FakeScreenCast(FakePortal);
    #[zbus::interface(name = "org.freedesktop.portal.ScreenCast")]
    impl FakeScreenCast {
        #[zbus(property, name = "version")]
        fn version(&self) -> u32 {
            5
        }

        async fn select_sources(
            &self,
            _session: OwnedObjectPath,
            options: Results,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(connection)] conn: &Connection,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            if options.contains_key("persist_mode") || options.contains_key("restore_token") {
                return Err(zbus::fdo::Error::InvalidArgs(
                    "combined session persistence belongs to RemoteDesktop".into(),
                ));
            }
            self.0.calls.lock().unwrap().push("SelectSources");
            early_reply(
                conn,
                header.sender().unwrap().as_str(),
                &options,
                0,
                Results::new(),
            )
            .await
        }
    }

    #[tokio::test]
    async fn mock_portal_early_reply_denial_cancellation_and_session_closed() -> Result<()> {
        use std::sync::atomic::Ordering::SeqCst;
        use tokio::io::AsyncBufReadExt;
        let mut bus = match tokio::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("skipping private D-Bus integration: dbus-daemon is not installed");
                return Ok(());
            }
            other => other?,
        };
        let mut reader = tokio::io::BufReader::new(bus.stdout.take().unwrap());
        let mut address = String::new();
        tokio::time::timeout(TIMEOUT, reader.read_line(&mut address)).await??;
        let backend = FakePortal::default();
        let builder = || zbus::connection::Builder::address(address.trim());
        let server = builder()?
            .name(SERVICE)?
            .serve_at(DESKTOP, backend.clone())?
            .serve_at(DESKTOP, FakeScreenCast(backend.clone()))?
            .build()
            .await?;
        let portal = Portal::default();
        let scratch = tempfile::tempdir()?;
        let token = scratch.path().join("token.json");
        let cancel = CancellationToken::new();

        backend.start_response.store(1, SeqCst);
        let denied = handshake_on(
            builder()?.build().await?,
            &cancel,
            Arc::downgrade(&portal.state),
            None,
        )
        .await;
        assert!(matches!(denied, Err(e) if e.to_string().contains("PORTAL_DENIED")));
        tokio::time::timeout(TIMEOUT, backend.closed.notified()).await?;
        assert!(!token.exists());

        backend.start_response.store(2, SeqCst);
        backend.calls.lock().unwrap().clear();
        let cancel_wait = CancellationToken::new();
        let wait_for_start = async {
            while !backend.calls.lock().unwrap().contains(&"Start") {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            cancel_wait.cancel();
        };
        let client = builder()?.build().await?;
        let (aborted, _) = tokio::time::timeout(TIMEOUT, async {
            tokio::join!(
                handshake_on(client, &cancel_wait, Arc::downgrade(&portal.state), None),
                wait_for_start
            )
        })
        .await?;
        assert!(matches!(aborted, Err(e) if e.to_string().contains("CANCELLED")));
        tokio::time::timeout(TIMEOUT, backend.closed.notified()).await?;

        backend.start_response.store(0, SeqCst);
        backend.calls.lock().unwrap().clear();
        let session = handshake_on(
            builder()?.build().await?,
            &cancel,
            Arc::downgrade(&portal.state),
            Some(token.clone()),
        )
        .await?;
        assert_eq!(
            *backend.calls.lock().unwrap(),
            ["CreateSession", "SelectDevices", "SelectSources", "Start"]
        );
        assert_eq!(load_token(&token)?.as_deref(), Some("fake-token"));
        let epoch = portal.state.lock().unwrap().epoch.clone();
        assert!(portal.state.lock().unwrap().store(
            &epoch,
            session.closed.clone(),
            session.clone()
        ));
        server
            .emit_signal(
                Some(session.lease.conn.unique_name().unwrap().as_str()),
                session.lease.path.as_str(),
                SESSION,
                "Closed",
                &(Results::new(),),
            )
            .await?;
        tokio::time::timeout(TIMEOUT, session.closed.cancelled()).await?;
        assert!(portal.state.lock().unwrap().live().is_none());
        session.stop();
        tokio::time::timeout(TIMEOUT, backend.closed.notified()).await?;

        let new = handshake_on(
            builder()?.build().await?,
            &cancel,
            Arc::downgrade(&portal.state),
            Some(token),
        )
        .await?;
        assert_ne!(new.lease.path, session.lease.path);
        assert!(
            portal
                .state
                .lock()
                .unwrap()
                .store(&epoch, new.closed.clone(), new.clone())
        );
        portal.stop();
        assert!(new.closed.is_cancelled());
        tokio::time::timeout(TIMEOUT, backend.closed.notified()).await?;
        bus.kill().await?;
        Ok(())
    }

    #[test]
    fn cached_session_is_replaced_after_close_and_disable_cancels_pending_consent() {
        let mut state = State::default();
        let epoch = state.epoch.clone();
        let closed = CancellationToken::new();
        assert!(state.live().is_none());
        assert!(
            response(1, Results::new())
                .unwrap_err()
                .to_string()
                .contains("PORTAL_DENIED")
        );
        assert!(response(2, Results::new()).is_err());
        assert!(state.store(&epoch, closed.clone(), Arc::new(1)));
        assert_eq!(*state.live().unwrap(), 1);
        closed.cancel();
        assert!(state.live().is_none());
        assert!(state.store(&epoch, CancellationToken::new(), Arc::new(2)));
        assert_eq!(*state.stop().unwrap(), 2);
        assert!(epoch.is_cancelled());
        assert!(!state.store(&epoch, CancellationToken::new(), Arc::new(3)));
        assert!(state.live().is_none());
        assert!(state.store(&state.epoch.clone(), CancellationToken::new(), Arc::new(4)));
    }
}
