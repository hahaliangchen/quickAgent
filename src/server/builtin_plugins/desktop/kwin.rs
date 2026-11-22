//! KWin's session-bus bridge. All scripts use fixed code and typed operands.
use super::*;
use serde::Deserialize;
use std::{io::Write, time::Duration};
use tokio::sync::oneshot;
use zbus::{Connection, Proxy, fdo::DBusProxy, message::Header, names::UniqueName};

const BUS: &str = "org.kde.KWin";
const PATH: &str = "/Scripting";
const API: &str = "org.kde.kwin.Scripting";
const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub(super) struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub(super) fn from_window(w: &Window) -> Self {
        Self {
            x: w.x,
            y: w.y,
            width: w.width,
            height: w.height,
        }
    }

    pub(super) fn local(self, point: Point) -> Result<(f64, f64)> {
        let x = i64::from(point.x) - i64::from(self.x);
        let y = i64::from(point.y) - i64::from(self.y);
        if !(0..i64::from(self.width)).contains(&x) || !(0..i64::from(self.height)).contains(&y) {
            return Err(fault(
                "COORDINATE_OUT_OF_BOUNDS",
                "pointer is outside the authorized monitor",
            ));
        }
        Ok((x as f64, y as f64))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    id: String,
    title: String,
    app_class: String,
    pid: Option<u32>,
    geometry: Rect,
    normal: bool,
    visible: bool,
    active: bool,
}

#[derive(Deserialize)]
pub(super) struct Snapshot {
    entries: Vec<Entry>,
    screen: Option<Rect>,
    hit: Option<String>,
    cursor: Option<[i32; 2]>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 59 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

impl Snapshot {
    pub(super) fn windows(&self) -> Vec<Window> {
        self.entries
            .iter()
            .filter(|w| w.normal && valid_id(&w.id))
            .map(|w| Window {
                window_id: format!("kwin-{}", w.id),
                title: w.title.clone(),
                app_class: w.app_class.clone(),
                x: w.geometry.x,
                y: w.geometry.y,
                width: w.geometry.width,
                height: w.geometry.height,
                active: w.active,
                visible: w.visible,
                pid: w.pid.filter(|&pid| pid > 0),
            })
            .collect()
    }
}

struct Reply {
    kwin: UniqueName<'static>,
    sender: Mutex<Option<oneshot::Sender<String>>>,
}

#[zbus::interface(name = "com.grow.agent.KWinReply")]
impl Reply {
    fn report(&self, payload: String, #[zbus(header)] header: Header<'_>) -> zbus::fdo::Result<()> {
        if header.sender() != Some(&self.kwin) || payload.len() > 512 * 1024 {
            return Err(zbus::fdo::Error::AccessDenied(
                "unexpected KWin callback".into(),
            ));
        }
        if let Some(sender) = self.sender.lock().unwrap().take() {
            let _ = sender.send(payload);
        }
        Ok(())
    }
}

// Only the callback address, a validated canonical ID, and i32 coordinates are interpolated.
const SCRIPT: &str = r#"
(function() {
    var modern = typeof workspace.windowList === 'function';
    var list = modern ? workspace.windowList() : workspace.clientList();
    function active() { return modern ? workspace.activeWindow : workspace.activeClient; }
    function id(w) {
        if (!w) return null;
        var raw = w.internalId || w.windowId || w.window;
        return raw ? String(raw).replace(/^\{|\}$/g, '').toLowerCase() : null;
    }
    function rect(g) {
        return g ? {x:Math.round(g.x), y:Math.round(g.y), width:Math.round(g.width), height:Math.round(g.height)} : null;
    }
    function visible(w) {
        // KDE's on-current-desktop / activity flags are not exposed by every API version.
        var desktop = w.onAllDesktops || (w.desktops && w.desktops.indexOf(workspace.currentDesktop) >= 0)
            || (typeof w.desktop === 'number' && w.desktop === workspace.currentDesktop);
        var activity = !w.activities || !w.activities.length || w.activities.indexOf(workspace.currentActivity) >= 0;
        return !w.minimized && (w === active() || (desktop && activity));
    }
    if (focus !== null) {
        var target = list.find(function(w) { return id(w) === focus[0] && w.pid === focus[1] && (w.normalWindow || w.dialog); });
        if (target) {
            function canonical(s) { return String(s || '').toLowerCase().replace(/[-_. ]/g, ''); }
            var title = canonical(target.caption), app = canonical(target.resourceClass || target.resourceName);
            if (app && !protectedWindows.some(function(term) { return title.indexOf(term) >= 0 || app.indexOf(term) >= 0; })) {
                target.minimized = false;
                if (modern) workspace.activeWindow = target;
                else workspace.activeClient = target;
            }
        }
    }
    var hit = null;
    var c = workspace.cursorPos;
    var stack = workspace.stackingOrder;
    var atCursor = point && c && c.x === point[0] && c.y === point[1];
    if (atCursor && typeof workspace.windowAt === 'function') {
        try {
            var matches = workspace.windowAt(c, 1);
            if (matches.length) hit = id(matches[0]);
        } catch (e) {
            // No verified hit is safer than clicking through an overlay.
        }
    } else if (point && stack && stack.length) {
        for (var i = stack.length - 1; i >= 0; --i) {
            var w = stack[i], g = rect(w.frameGeometry);
            // An unknown overlay must not be silently treated as the target.
            if (!g || w.minimized || point[0] < g.x || point[1] < g.y ||
                point[0] >= g.x + g.width || point[1] >= g.y + g.height) continue;
            if (visible(w) || !(w.normalWindow || w.dialog)) { hit = id(w); break; }
        }
    }
    var entries = [];
    for (var j = 0; j < list.length && entries.length < 200; ++j) {
        var w = list[j], key = id(w), geometry = rect(w.frameGeometry);
        if (!key || !geometry || !(w.normalWindow || w.dialog)) continue;
        entries.push({id:key, geometry:geometry, normal:true, visible:!!visible(w), active:w === active(),
            title:String(w.caption || '').slice(0,1024), appClass:String(w.resourceClass || w.resourceName || '').slice(0,512),
            pid:w.pid > 0 ? w.pid : null});
    }
    callDBus(replyBus, replyPath, 'com.grow.agent.KWinReply', 'Report', JSON.stringify({
        entries:entries, screen:rect(workspace.virtualScreenGeometry), hit:hit,
        cursor:c ? [Math.round(c.x), Math.round(c.y)] : null
    }));
})();
"#;

async fn run(
    focus: Option<(String, u32)>,
    point: Option<Point>,
    cancel: CancellationToken,
) -> Result<Snapshot> {
    let conn = tokio::time::timeout(TIMEOUT, Connection::session()).await??;
    let name = format!(
        "quickagent_{}",
        crate::server::agent_service::uuid_like().replace('-', "")
    );
    let path = format!("/com/grow/agent/KWinReply/{name}");
    let sender = conn
        .unique_name()
        .context("KWin callback requires a session bus")?;
    let kwin = tokio::time::timeout(TIMEOUT, async {
        Ok::<_, anyhow::Error>(
            DBusProxy::new(&conn)
                .await?
                .get_name_owner(BUS.try_into()?)
                .await?,
        )
    })
    .await??;
    let (tx, rx) = oneshot::channel();
    conn.object_server()
        .at(
            path.as_str(),
            Reply {
                kwin: kwin.clone().into(),
                sender: Mutex::new(Some(tx)),
            },
        )
        .await?;
    let mut file = tempfile::Builder::new().suffix(".js").tempfile()?;
    writeln!(
        file,
        "var replyBus = {}; var replyPath = {}; var focus = {}; var point = {}; var protectedWindows = {};\n{SCRIPT}",
        json!(sender.as_str()),
        json!(path),
        json!(focus),
        json!(point.map(|p| [p.x, p.y])),
        json!(guard::SENSITIVE_WINDOWS)
    )?;
    file.flush()?;
    let filename = file.path().to_str().context("invalid KWin script path")?;
    let proxy = Proxy::new(&conn, kwin.as_str(), PATH, API).await?;
    guard::cancelled(&cancel)?;
    let result = tokio::time::timeout(TIMEOUT, async {
        let id: i32 = proxy.call("loadScript", &(filename, &name)).await?;
        ensure!(id >= 0, "KWin rejected the temporary script");
        guard::cancelled(&cancel)?;
        let script = format!("/Scripting/Script{id}");
        let legacy_script = format!("/{id}");
        let run = |path| {
            conn.call_method(
                Some(kwin.as_str()),
                path,
                Some("org.kde.kwin.Script"),
                "run",
                &(),
            )
        };
        match run(script.as_str()).await {
            Err(zbus::Error::MethodError(error, _, _))
                if matches!(
                    error.as_str(),
                    "org.freedesktop.DBus.Error.UnknownObject"
                        | "org.freedesktop.DBus.Error.UnknownMethod"
                        | "org.freedesktop.DBus.Error.UnknownInterface"
                ) =>
            {
                guard::cancelled(&cancel)?;
                run(legacy_script.as_str()).await?;
            }
            result => {
                result?;
            }
        }
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(fault("CANCELLED", "desktop task cancelled")),
            result = rx => Ok(serde_json::from_str(&result?)?),
        }
    })
    .await;
    // Unload even if the load reply was lost or the script/caller failed.
    let cleanup =
        tokio::time::timeout(TIMEOUT, proxy.call::<_, _, bool>("unloadScript", &(&name,))).await;
    let snapshot = result.map_err(|_| fault("DESKTOP_TIMEOUT", "KWin script did not finish"))??;
    ensure!(cleanup??, "KWin did not unload its temporary script");
    Ok(snapshot)
}

struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

async fn query(
    focus: Option<&Window>,
    point: Option<Point>,
    cancel: &CancellationToken,
) -> Result<Snapshot> {
    let focus = focus
        .map(|window| {
            window
                .window_id
                .strip_prefix("kwin-")
                .filter(|key| valid_id(key))
                .zip(window.pid)
                .map(|(id, pid)| (id.to_owned(), pid))
                .ok_or_else(|| fault("WINDOW_UNAVAILABLE", "cannot verify the KWin focus target"))
        })
        .transpose()?;
    // Let the worker finish unloadScript when the outer tool future is dropped.
    let child = CancelOnDrop(cancel.child_token());
    tokio::spawn(run(focus, point, child.0.clone())).await?
}

pub(super) async fn windows() -> Result<Vec<Window>> {
    Ok(query(None, None, &CancellationToken::new())
        .await?
        .windows())
}

pub(super) async fn verify(
    window: &Window,
    point: Option<Point>,
    cursor: bool,
    cancel: &CancellationToken,
) -> Result<Snapshot> {
    let data = query(None, point, cancel).await?;
    unlocked(cancel).await?;
    let live = guard::window(&data.windows(), Some(&window.window_id), true)?;
    if live != *window
        || point.is_some_and(|p| {
            data.hit.as_deref() != window.window_id.strip_prefix("kwin-")
                || (cursor && data.cursor != Some([p.x, p.y]))
        })
    {
        return Err(fault(
            "WINDOW_CHANGED",
            "KWin could not verify the foreground window, geometry or pointer target",
        ));
    }
    Ok(data)
}

async fn unlocked(cancel: &CancellationToken) -> Result<()> {
    guard::cancelled(cancel)?;
    let active = tokio::time::timeout(TIMEOUT, async {
        let conn = Connection::session().await?;
        let lock = Proxy::new(
            &conn,
            "org.freedesktop.ScreenSaver",
            "/ScreenSaver",
            "org.freedesktop.ScreenSaver",
        )
        .await?;
        lock.call::<_, _, bool>("GetActive", &()).await
    })
    .await
    .map_err(|_| fault("WINDOW_UNAVAILABLE", "KDE screen lock could not be checked"))?
    .map_err(|_| fault("WINDOW_UNAVAILABLE", "KDE screen lock could not be checked"))?;
    ensure!(!active, fault("SENSITIVE_WINDOW", "the desktop is locked"));
    guard::cancelled(cancel)
}

pub(super) async fn focus(window: &Window, cancel: &CancellationToken) -> Result<()> {
    let snapshot = query(None, None, cancel).await?;
    let live = guard::focus_window(&snapshot.windows(), &window.window_id)?;
    ensure!(
        live == *window,
        fault("WINDOW_CHANGED", "KWin window identity changed")
    );
    unlocked(cancel).await?;
    query(Some(window), None, cancel).await?;
    let snapshot = query(None, None, cancel).await?;
    let live = guard::window(&snapshot.windows(), Some(&window.window_id), true)?;
    ensure!(
        live.pid == window.pid && live.app_class == window.app_class,
        fault("WINDOW_CHANGED", "KWin window identity changed")
    );
    Ok(())
}

pub(super) fn crop_png(bytes: &[u8], screen: Rect, window: Rect) -> Result<Vec<u8>> {
    use image::{ImageFormat, ImageReader, Limits};
    let mut reader = ImageReader::with_format(std::io::Cursor::new(bytes), ImageFormat::Png);
    let mut limits = Limits::default();
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?;
    let (x, y, width, height) = crop_rect(screen, window, image.width(), image.height())?;
    let mut output = std::io::Cursor::new(Vec::new());
    image
        .crop_imm(x, y, width, height)
        .write_to(&mut output, ImageFormat::Png)?;
    Ok(output.into_inner())
}

fn crop_rect(screen: Rect, w: Rect, width: u32, height: u32) -> Result<(u32, u32, u32, u32)> {
    let x = i64::from(w.x) - i64::from(screen.x);
    let y = i64::from(w.y) - i64::from(screen.y);
    let (right, bottom) = (x + i64::from(w.width), y + i64::from(w.height));
    if x < 0
        || y < 0
        || w.width == 0
        || w.height == 0
        || screen.width == 0
        || screen.height == 0
        || width == 0
        || height == 0
        || right > i64::from(screen.width)
        || bottom > i64::from(screen.height)
    {
        return Err(fault(
            "COORDINATE_OUT_OF_BOUNDS",
            "window is not fully on the captured desktop",
        ));
    }
    // Spectacle's composite uses a uniform device scale (allow one pixel of rounding).
    let discrepancy = (u64::from(width) * u64::from(screen.height))
        .abs_diff(u64::from(height) * u64::from(screen.width));
    ensure!(
        discrepancy <= u64::from(screen.width.max(screen.height)),
        fault(
            "DESKTOP_BACKEND_FAILED",
            "cannot verify the Spectacle screenshot scale"
        )
    );
    let project = |n: i64, pixels: u32, logical: u32, ceil: bool| {
        let n = n as u64 * u64::from(pixels);
        let d = u64::from(logical);
        (if ceil { n.div_ceil(d) } else { n / d }) as u32
    };
    let x0 = project(x, width, screen.width, false);
    let y0 = project(y, height, screen.height, false);
    Ok((
        x0,
        y0,
        project(right, width, screen.width, true) - x0,
        project(bottom, height, screen.height, true) - y0,
    ))
}

pub(super) fn same_screen(a: &Snapshot, b: &Snapshot) -> Result<Rect> {
    a.screen
        .filter(|screen| Some(*screen) == b.screen)
        .ok_or_else(|| {
            fault(
                "WINDOW_CHANGED",
                "KWin desktop layout is unavailable or changed during capture",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kwin_identity_and_rejects_unknown_or_protected_targets() {
        let row = json!({"id":"aa-12","title":"PIN entry","appClass":"org.kde.pinentry","pid":42,
            "geometry":{"x":-5,"y":12,"width":500,"height":200},"normal":true,"active":true,"visible":true});
        let entry = |id, normal, visible| {
            let mut row = row.clone();
            row["id"] = json!(id);
            row["normal"] = json!(normal);
            row["visible"] = json!(visible);
            row
        };
        let data: Snapshot =
            serde_json::from_value(json!({"entries":[row,entry("panel",false,true),
            entry("quote\"",true,true),entry("minimized",true,false)]}))
            .unwrap();
        let windows = data.windows();
        assert_eq!(windows.len(), 2);
        assert_eq!(
            (windows[0].window_id.as_str(), windows[0].x, windows[0].pid),
            ("kwin-aa-12", -5, Some(42))
        );
        assert!(!windows[1].visible);
        assert!(
            guard::window(&windows, Some("kwin-aa-12"), true)
                .unwrap_err()
                .to_string()
                .contains("SENSITIVE_WINDOW")
        );
    }

    #[tokio::test]
    #[ignore = "requires a local KDE compositor"]
    async fn kde_live_snapshot_smoke() -> Result<()> {
        let cancel = CancellationToken::new();
        let data = query(None, None, &cancel).await?;
        ensure!(
            data.screen.is_some_and(|r| r.width > 0 && r.height > 0),
            "missing virtual desktop geometry"
        );
        let windows = data.windows();
        ensure!(!windows.is_empty(), "no ordinary windows reported by KWin");
        if let Some(w) = windows
            .iter()
            .find(|w| w.active && !guard::sensitive(&w.title, &w.app_class))
        {
            let p = Point {
                x: w.x + (w.width / 2) as i32,
                y: w.y + (w.height / 2) as i32,
            };
            verify(w, None, false, &cancel).await?;
            let pointed = query(None, Some(p), &cancel)
                .await
                .context("KWin point query")?;
            ensure!(
                pointed.cursor.is_some(),
                "KWin could not read cursor position"
            );
            ensure!(
                pointed.hit.is_some(),
                "KWin could not resolve the hit window"
            );
            let png = super::super::linux::Linux::default()
                .screenshot(Some(w))
                .await
                .context("KDE window crop")?;
            ensure!(!png.is_empty(), "KWin cropped screenshot is empty");
        }
        Ok(())
    }

    #[test]
    fn fractional_hidpi_crops_from_negative_origin_and_checks_bounds() {
        let screen = Rect {
            x: -800,
            y: -400,
            width: 1600,
            height: 800,
        };
        let window = Rect {
            x: -799,
            y: -399,
            width: 99,
            height: 49,
        };
        assert_eq!(
            crop_rect(screen, window, 2400, 1200).unwrap(),
            (1, 1, 149, 74)
        );
        assert!(crop_rect(screen, Rect { x: -801, ..window }, 2400, 1200).is_err());
        assert!(crop_rect(screen, window, 2400, 1600).is_err());
    }
}
