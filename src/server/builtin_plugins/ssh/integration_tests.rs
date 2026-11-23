use std::{
    collections::HashMap,
    io::SeekFrom,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, ensure};
use futures::{SinkExt, StreamExt};
use russh::{
    Channel, ChannelId, Pty,
    keys::{Algorithm, PrivateKey},
    server::{self, Auth, Msg, Session},
};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode,
};
use serde_json::{Value, json};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
    task::JoinHandle,
};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

use super::{
    client,
    credentials::Secret,
    package, routes,
    store::{AuthType, EnvTier, HostKeyMismatch, NewServer, Store},
};

const PASSWORD: &str = "loopback-test-password";
const MIB: usize = 1024 * 1024;

struct Fixture {
    port: u16,
    config: Arc<RwLock<Arc<server::Config>>>,
    auth: Arc<AtomicUsize>,
    listener: JoinHandle<()>,
    finish_exec: Arc<Notify>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

impl Fixture {
    async fn start(root: PathBuf) -> Result<Self> {
        let socket = TcpListener::bind("127.0.0.1:0").await?;
        let port = socket.local_addr()?.port();
        let config = Arc::new(RwLock::new(Self::config()));
        let auth = Arc::new(AtomicUsize::new(0));
        let finish_exec = Arc::new(Notify::new());
        let listener = tokio::spawn({
            let (config, auth, finish_exec) = (config.clone(), auth.clone(), finish_exec.clone());
            async move {
                while let Ok((stream, _)) = socket.accept().await {
                    let config = config.read().unwrap().clone();
                    let handler = MockSsh {
                        root: root.clone(),
                        auth: auth.clone(),
                        channels: HashMap::new(),
                        shell: None,
                        input: Vec::new(),
                        finish_exec: finish_exec.clone(),
                    };
                    tokio::spawn(async move {
                        if let Ok(session) = server::run_stream(config, stream, handler).await {
                            let _ = session.await;
                        }
                    });
                }
            }
        });
        Ok(Self {
            port,
            config,
            auth,
            listener,
            finish_exec,
        })
    }
    fn config() -> Arc<server::Config> {
        Arc::new(server::Config {
            keys: vec![PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()],
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        })
    }
    fn rotate_key(&self) {
        *self.config.write().unwrap() = Self::config();
    }
}

struct MockSsh {
    root: PathBuf,
    auth: Arc<AtomicUsize>,
    channels: HashMap<ChannelId, Channel<Msg>>,
    shell: Option<ChannelId>,
    input: Vec<u8>,
    finish_exec: Arc<Notify>,
}
impl server::Handler for MockSsh {
    type Error = anyhow::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth> {
        self.auth.fetch_add(1, Ordering::SeqCst);
        Ok(if user == "tester" && password == PASSWORD {
            Auth::Accept
        } else {
            Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            }
        })
    }
    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _: &mut Session,
    ) -> Result<bool> {
        self.channels.insert(channel.id(), channel);
        Ok(true)
    }
    async fn pty_request(
        &mut self,
        id: ChannelId,
        term: &str,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<()> {
        ensure!(term == "xterm-256color");
        session.channel_success(id)?;
        Ok(())
    }
    async fn shell_request(&mut self, id: ChannelId, session: &mut Session) -> Result<()> {
        self.shell = Some(id);
        session.channel_success(id)?;
        session.data(id, b"local fixture ready\r\n$ ".to_vec())?;
        Ok(())
    }
    async fn data(&mut self, id: ChannelId, data: &[u8], session: &mut Session) -> Result<()> {
        if self.shell == Some(id) {
            self.input.extend_from_slice(data);
            ensure!(self.input.len() < 4096);
            if self.input.contains(&b'\r') || self.input.contains(&b'\n') {
                let command = String::from_utf8_lossy(&self.input);
                let output = if command.trim() == "printf fixture-ok" {
                    "fixture-ok\r\n$ "
                } else {
                    "fixture accepts only its fixed test input\r\n$ "
                };
                session.data(id, output.as_bytes().to_vec())?;
                self.input.clear();
            }
        }
        Ok(())
    }
    async fn window_change_request(
        &mut self,
        id: ChannelId,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        session: &mut Session,
    ) -> Result<()> {
        if cols == 92 && rows == 33 {
            session.data(id, b"fixture resized\r\n".to_vec())?;
        }
        Ok(())
    }
    async fn exec_request(
        &mut self,
        id: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<()> {
        session.channel_success(id)?;
        let raw = String::from_utf8_lossy(data);
        let (cwd, cmd) = if let Some(rest) = raw.strip_prefix("cd -- '")
            && let Some((dir, after)) = rest.split_once("' && ")
        {
            (Some(dir.to_owned()), after.trim())
        } else {
            (None, raw.trim())
        };
        if cmd.starts_with("stream-check ") {
            session.data(id, b"live stdout\nloopback-test-".to_vec())?;
            let handle = session.handle();
            let finish = self.finish_exec.clone();
            tokio::spawn(async move {
                finish.notified().await;
                let _ = handle.data(id, b"password\n-----BEGIN OPENSSH PRIVATE KEY-----\nprivate-key-body\n-----END OPENSSH PRIVATE KEY-----\n".to_vec()).await;
                let _ = handle.extended_data(id, 1, b"live stderr\n".to_vec()).await;
                let _ = handle.exit_status_request(id, 42).await;
                let _ = handle.eof(id).await;
                let _ = handle.close(id).await;
            });
            return Ok(());
        }
        match cmd {
            "pwd" => {
                let out = format!("{}\n", cwd.as_deref().unwrap_or("/"));
                session.data(id, out.into_bytes())?;
                session.exit_status_request(id, 0)?;
            }
            "echo deploy-ok" => {
                session.data(id, b"deploy-ok\n".to_vec())?;
                session.exit_status_request(id, 0)?;
            }
            "fail-cmd" => {
                session.extended_data(id, 1, b"simulated stderr\n".to_vec())?;
                session.exit_status_request(id, 42)?;
            }
            _ => {
                session.data(id, format!("ran:{cmd}\n").into_bytes())?;
                session.exit_status_request(id, 0)?;
            }
        }
        session.eof(id)?;
        session.close(id)?;
        Ok(())
    }
    async fn subsystem_request(
        &mut self,
        id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<()> {
        if name != "sftp" {
            session.channel_failure(id)?;
            return Ok(());
        }
        let channel = self.channels.remove(&id).unwrap();
        session.channel_success(id)?;
        tokio::spawn(russh_sftp::server::run(
            channel.into_stream(),
            MockFs {
                root: self.root.clone(),
                files: HashMap::new(),
                dirs: HashMap::new(),
            },
        ));
        Ok(())
    }
}

struct MockFs {
    root: PathBuf,
    files: HashMap<String, fs::File>,
    dirs: HashMap<String, Option<PathBuf>>,
}
type FsResult<T> = std::result::Result<T, StatusCode>;
impl MockFs {
    fn path(&self, path: &str) -> FsResult<PathBuf> {
        let mut result = self.root.clone();
        for part in Path::new(path).components() {
            match part {
                Component::RootDir | Component::CurDir => (),
                Component::Normal(part) => result.push(part),
                _ => return Err(StatusCode::PermissionDenied),
            }
        }
        Ok(result)
    }
}
fn status(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "OK".into(),
        language_tag: "".into(),
    }
}
fn io_error(error: std::io::Error) -> StatusCode {
    if error.kind() == std::io::ErrorKind::NotFound {
        StatusCode::NoSuchFile
    } else {
        StatusCode::Failure
    }
}

impl russh_sftp::server::Handler for MockFs {
    type Error = StatusCode;
    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }
    async fn stat(&mut self, id: u32, path: String) -> FsResult<Attrs> {
        let meta = fs::metadata(self.path(&path)?).await.map_err(io_error)?;
        Ok(Attrs {
            id,
            attrs: FileAttributes::from(&meta),
        })
    }
    async fn lstat(&mut self, id: u32, path: String) -> FsResult<Attrs> {
        let meta = fs::symlink_metadata(self.path(&path)?)
            .await
            .map_err(io_error)?;
        Ok(Attrs {
            id,
            attrs: FileAttributes::from(&meta),
        })
    }
    async fn realpath(&mut self, id: u32, path: String) -> FsResult<Name> {
        let path = self.path(&path)?;
        let path = path.strip_prefix(&self.root).unwrap().to_string_lossy();
        Ok(Name {
            id,
            files: vec![File::dummy(format!("/{path}"))],
        })
    }
    async fn open(
        &mut self,
        id: u32,
        filename: String,
        flags: OpenFlags,
        _: FileAttributes,
    ) -> FsResult<Handle> {
        let path = self.path(&filename)?;
        let file = fs::OpenOptions::from(std::fs::OpenOptions::from(flags))
            .open(path)
            .await
            .map_err(io_error)?;
        let handle = id.to_string();
        self.files.insert(handle.clone(), file);
        Ok(Handle { id, handle })
    }
    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> FsResult<Data> {
        let file = self.files.get_mut(&handle).ok_or(StatusCode::Failure)?;
        file.seek(SeekFrom::Start(offset)).await.map_err(io_error)?;
        let mut data = vec![0; len.min(256 * 1024) as usize];
        let read = file.read(&mut data).await.map_err(io_error)?;
        if read == 0 {
            return Err(StatusCode::Eof);
        }
        data.truncate(read);
        Ok(Data { id, data })
    }
    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> FsResult<Status> {
        let file = self.files.get_mut(&handle).ok_or(StatusCode::Failure)?;
        file.seek(SeekFrom::Start(offset)).await.map_err(io_error)?;
        file.write_all(&data).await.map_err(io_error)?;
        Ok(status(id))
    }
    async fn close(&mut self, id: u32, handle: String) -> FsResult<Status> {
        self.dirs.remove(&handle);
        if let Some(mut file) = self.files.remove(&handle) {
            file.flush().await.map_err(io_error)?;
        }
        Ok(status(id))
    }
    async fn opendir(&mut self, id: u32, path: String) -> FsResult<Handle> {
        let handle = id.to_string();
        self.dirs.insert(handle.clone(), Some(self.path(&path)?));
        Ok(Handle { id, handle })
    }
    async fn readdir(&mut self, id: u32, handle: String) -> FsResult<Name> {
        let path = self
            .dirs
            .get_mut(&handle)
            .and_then(Option::take)
            .ok_or(StatusCode::Eof)?;
        let mut dir = fs::read_dir(path).await.map_err(io_error)?;
        let mut files = Vec::new();
        while let Some(entry) = dir.next_entry().await.map_err(io_error)? {
            files.push(File::new(
                entry.file_name().to_string_lossy(),
                FileAttributes::from(&entry.metadata().await.map_err(io_error)?),
            ));
        }
        Ok(Name { id, files })
    }
    async fn rename(&mut self, id: u32, old: String, new: String) -> FsResult<Status> {
        let destination = self.path(&new)?;
        if fs::try_exists(&destination).await.map_err(io_error)? {
            return Err(StatusCode::Failure);
        }
        fs::rename(self.path(&old)?, destination)
            .await
            .map_err(io_error)?;
        Ok(status(id))
    }
    async fn remove(&mut self, id: u32, path: String) -> FsResult<Status> {
        fs::remove_file(self.path(&path)?).await.map_err(io_error)?;
        Ok(status(id))
    }
    async fn mkdir(&mut self, id: u32, path: String, _: FileAttributes) -> FsResult<Status> {
        fs::create_dir(self.path(&path)?).await.map_err(io_error)?;
        Ok(status(id))
    }
    async fn rmdir(&mut self, id: u32, path: String) -> FsResult<Status> {
        fs::remove_dir(self.path(&path)?).await.map_err(io_error)?;
        Ok(status(id))
    }
}

async fn start_http(directory: PathBuf) -> Result<(String, JoinHandle<()>)> {
    let http = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", http.local_addr()?);
    let job = tokio::spawn(async move {
        axum::serve(http, routes::with_directory(directory))
            .await
            .unwrap();
    });
    Ok((url, job))
}

#[tokio::test]
async fn real_ssh_pty_sftp_and_host_key_mismatch() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(60), exercise_transport()).await?
}

async fn exercise_transport() -> Result<()> {
    let root = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let fixture = Fixture::start(root.path().to_owned()).await?;
    let (http, http_job) = start_http(config.path().to_owned()).await?;
    let base = format!("{http}/agent/ssh/servers");
    let client = reqwest::Client::builder()
        .default_headers({
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert("sec-fetch-site", "same-origin".parse()?);
            headers
        })
        .build()?;
    assert_eq!(reqwest::get(&base).await?.status(), 403);
    assert_eq!(
        client
            .get(&base)
            .header("origin", "http://localhost:9")
            .send()
            .await?
            .status(),
        403
    );
    let created = client.post(&base).json(&json!({"name":"Fixture","host":"127.0.0.1","port":fixture.port,"username":"tester","auth_type":"password","credential":PASSWORD,"env_tier":"prod","require_approval":false})).send().await?.error_for_status()?.json::<Value>().await?;
    assert!(!created.to_string().contains(PASSWORD));
    assert!(created["require_approval"].as_bool().unwrap());
    let id = created["id"].as_str().unwrap();
    let item = format!("{base}/{id}");
    let ticket_api = format!("{item}/terminal-ticket");
    assert_eq!(
        client
            .post(&ticket_api)
            .json(&json!({}))
            .send()
            .await?
            .status(),
        403
    );
    assert_eq!(fixture.auth.load(Ordering::SeqCst), 0);
    let context = json!({"approved":true,"task_id":"test-task","workspace_root":"/test/project"});
    let ticket = client
        .post(&ticket_api)
        .json(&context)
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    let mut ws_request = format!(
        "{}/agent/ssh/servers/{id}/terminal?ticket={}",
        http.replacen("http:", "ws:", 1),
        ticket["ticket"].as_str().unwrap()
    )
    .into_client_request()?;
    ws_request
        .headers_mut()
        .insert("sec-fetch-site", "same-origin".parse()?);
    ws_request.headers_mut().insert("origin", http.parse()?);
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_request.clone()).await?;
    assert!(tokio_tungstenite::connect_async(ws_request).await.is_err());
    assert_eq!(ws.next().await.unwrap()?.to_text()?, r#"{"type":"ready"}"#);
    ws.send(Message::Binary(b"printf fixture-ok\r".as_slice().into()))
        .await?;
    ws.send(Message::Text(
        r#"{"type":"resize","cols":92,"rows":33}"#.into(),
    ))
    .await?;
    let mut output = Vec::new();
    while !String::from_utf8_lossy(&output).contains("fixture resized") {
        if let Message::Binary(bytes) = ws.next().await.unwrap()? {
            output.extend_from_slice(&bytes);
        }
    }
    assert!(String::from_utf8_lossy(&output).contains("fixture-ok"));
    ws.close(None).await?;
    let upload = format!("{item}/sftp/upload?path=/large.bin&approved=true");
    assert_eq!(
        client
            .put(upload.replace("true", "false"))
            .body("denied")
            .send()
            .await?
            .status(),
        403
    );
    let chunks =
        futures::stream::iter((0..24u8).map(|chunk| Ok::<_, std::io::Error>(vec![chunk; MIB])));
    let uploaded = client
        .put(&upload)
        .header("content-length", (24 * MIB).to_string())
        .body(reqwest::Body::wrap_stream(chunks))
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    assert_eq!(uploaded["bytes"], 24 * MIB);
    let mut downloaded = client
        .get(format!("{item}/sftp/download?path=/large.bin"))
        .send()
        .await?
        .error_for_status()?
        .bytes_stream();
    let mut count = 0;
    while let Some(bytes) = downloaded.next().await {
        for byte in bytes? {
            assert_eq!(byte as usize, count / MIB);
            count += 1;
        }
    }
    assert_eq!(count, 24 * MIB);
    let history = Store::open(config.path())?.history(id, 20)?;
    assert!(
        history.iter().any(
            |event| event.action == "terminal" && event.task_id.as_deref() == Some("test-task")
        )
    );
    assert!(history.iter().any(|event| event.action == "upload"));
    assert!(history.iter().any(|event| event.action == "download"));
    let prior = fixture.auth.load(Ordering::SeqCst);
    fixture.rotate_key();
    let mismatch = client.post(format!("{item}/test")).send().await?;
    assert_eq!(mismatch.status(), 409);
    let mismatch = mismatch.json::<Value>().await?;
    assert_eq!(mismatch["code"], "HOST_KEY_MISMATCH");
    assert_eq!(prior, fixture.auth.load(Ordering::SeqCst));
    let err = client::connect(config.path().to_owned(), id.to_owned())
        .await
        .err()
        .unwrap();
    assert!(err.is::<HostKeyMismatch>());
    assert_ne!(mismatch["trusted"], mismatch["received"]);
    client
        .put(format!("{item}/host-key"))
        .json(&json!({"trusted":mismatch["trusted"],"received":mismatch["received"]}))
        .send()
        .await?
        .error_for_status()?;
    client
        .post(format!("{item}/test"))
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(prior + 1, fixture.auth.load(Ordering::SeqCst));
    http_job.abort();
    Ok(())
}

#[tokio::test]
#[ignore = "Playwright starts this loopback-only fixture"]
async fn playwright_fixture() -> Result<()> {
    let root = tempfile::tempdir()?;
    fs::write(root.path().join("hello.txt"), "SFTP fixture text\n").await?;
    fs::create_dir(root.path().join("nested")).await?;
    let fixture = Fixture::start(root.path().to_owned()).await?;
    let config = crate::server::ai_proxy::config_dir();
    ensure!(
        std::env::var_os("GROW_CONFIG_DIR").is_some(),
        "Playwright requires an isolated configuration directory"
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    println!(
        "SSH_E2E_READY http://{} {}",
        listener.local_addr()?,
        fixture.port
    );
    let workspace = Arc::new(crate::tools::Workspace::new(root.path().to_owned())?);
    crate::server::ai_proxy::run(listener, &config.join("config.yml"), workspace).await
}

#[tokio::test]
async fn ai_tools_cover_remote_ops_prod_approval_and_upload_sandbox() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(60), exercise_ai_tools()).await?
}

async fn exercise_ai_tools() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let fixture = Fixture::start(remote_root.path().to_owned()).await?;

    let (dev_id, prod_id) = {
        let mut store = Store::open(config.path())?;
        let dev_id = store.create(NewServer {
            name: "Z-Dev".into(),
            host: "127.0.0.1".into(),
            port: Some(fixture.port),
            username: "tester".into(),
            auth_type: AuthType::Password,
            key_path: None,
            env_tier: EnvTier::Dev,
            require_approval: false,
            default_dir: Some("/srv/dev".into()),
            tags: vec!["dev".into()],
            notes: Some("Dev node".into()),
        })?;
        store.set_credential(&dev_id, Some(Secret::new(PASSWORD.into())?))?;
        let prod_id = store.create(NewServer {
            name: "A-Prod".into(),
            host: "127.0.0.1".into(),
            port: Some(fixture.port),
            username: "tester".into(),
            auth_type: AuthType::Password,
            key_path: None,
            env_tier: EnvTier::Prod,
            require_approval: false,
            default_dir: Some("/srv/prod".into()),
            tags: vec!["prod".into()],
            notes: None,
        })?;
        store.set_credential(&prod_id, Some(Secret::new(PASSWORD.into())?))?;
        (dev_id, prod_id)
    };

    // Approval classification check
    assert!(!package::requires_approval_in(
        config.path(),
        "ssh__exec",
        &json!({ "server_id": dev_id })
    ));
    assert!(package::requires_approval_in(
        config.path(),
        "ssh__exec",
        &json!({ "server_id": prod_id })
    ));
    assert!(package::requires_approval_in(
        config.path(),
        "ssh__write_file",
        &json!({ "server_id": prod_id })
    ));
    assert!(package::requires_approval_in(
        config.path(),
        "ssh__upload",
        &json!({ "server_id": prod_id })
    ));
    assert!(!package::requires_approval_in(
        config.path(),
        "ssh__read_file",
        &json!({ "server_id": prod_id })
    ));

    // Prepare workspace and outside files for upload & boundary checks
    fs::create_dir_all(workspace.path().join("dist")).await?;
    fs::write(workspace.path().join("dist/app.bin"), b"release-v1").await?;
    fs::create_dir_all(workspace.path().join(".codex-workspace-mcp")).await?;
    fs::write(
        workspace.path().join(".codex-workspace-mcp/secret.db"),
        b"host-state",
    )
    .await?;
    let outside_secret = outside.path().join("secret.txt");
    fs::write(&outside_secret, b"do-not-leak").await?;
    std::os::unix::fs::symlink(&outside_secret, workspace.path().join("dist/escape.link"))?;

    let cfg = config.path().to_path_buf();
    let ws = workspace.path().to_path_buf();

    // 1. ssh__list_servers (zero credential leakage)
    let listed =
        package::execute_tool(cfg.clone(), ws.clone(), "ssh__list_servers", json!({})).await?;
    assert!(!listed.to_string().contains(PASSWORD));
    assert_eq!(listed["servers"].as_array().unwrap().len(), 2);

    // 2. ssh__exec on dev server (with cwd & nonzero stderr)
    let exec_pwd = package::scoped(
        "task-deploy-1".into(),
        false,
        package::execute_tool(
            cfg.clone(),
            ws.clone(),
            "ssh__exec",
            json!({ "server_id": dev_id, "command": "pwd", "cwd": "/srv/app" }),
        ),
    )
    .await?;
    assert_eq!(exec_pwd["exit_code"], 0);
    assert_eq!(exec_pwd["stdout"], "/srv/app\n");

    let exec_fail = package::scoped(
        "task-deploy-1".into(),
        false,
        package::execute_tool(
            cfg.clone(),
            ws.clone(),
            "ssh__exec",
            json!({ "server_id": dev_id, "command": "fail-cmd" }),
        ),
    )
    .await?;
    assert_eq!(exec_fail["exit_code"], 42);
    assert_eq!(exec_fail["stderr"], "simulated stderr\n");

    // 3. ssh__write_file (create_parents + backup)
    let w1 = package::scoped(
        "task-deploy-1".into(),
        false,
        package::execute_tool(
            cfg.clone(),
            ws.clone(),
            "ssh__write_file",
            json!({
                "server_id": dev_id,
                "path": "/deploy/config.toml",
                "content": "line1\nline2\nline3\nline4\n",
                "create_parents": true,
            }),
        ),
    )
    .await?;
    assert_eq!(w1["bytes_written"], 24);

    package::scoped(
        "task-deploy-1".into(),
        false,
        package::execute_tool(
            cfg.clone(),
            ws.clone(),
            "ssh__write_file",
            json!({
                "server_id": dev_id,
                "path": "/deploy/config.toml",
                "content": "alpha\nbeta\ngamma\ndelta\n",
                "backup": true,
            }),
        ),
    )
    .await?;
    assert_eq!(
        fs::read_to_string(remote_root.path().join("deploy/config.toml.bak")).await?,
        "line1\nline2\nline3\nline4\n"
    );

    // 4. ssh__list_dir
    let dir = package::execute_tool(
        cfg.clone(),
        ws.clone(),
        "ssh__list_dir",
        json!({ "server_id": dev_id, "path": "/deploy", "limit": 10 }),
    )
    .await?;
    let names: Vec<_> = dir["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"config.toml"));
    assert!(names.contains(&"config.toml.bak"));

    // 5. ssh__read_file (1-based line slicing)
    let read = package::scoped(
        "task-deploy-1".into(),
        false,
        package::execute_tool(
            cfg.clone(),
            ws.clone(),
            "ssh__read_file",
            json!({
                "server_id": dev_id,
                "path": "/deploy/config.toml",
                "start_line": 2,
                "end_line": 3,
            }),
        ),
    )
    .await?;
    assert_eq!(read["content"], "beta\ngamma");
    assert_eq!(read["returned_lines"], 2);

    // 6. ssh__tail_log (case-insensitive filter)
    fs::write(
        remote_root.path().join("deploy/app.log"),
        "INFO boot\nWARN retry\nERROR db timeout\nINFO ok\nerror cache miss\n",
    )
    .await?;
    let tail = package::scoped(
        "task-deploy-1".into(),
        false,
        package::execute_tool(
            cfg.clone(),
            ws.clone(),
            "ssh__tail_log",
            json!({
                "server_id": dev_id,
                "path": "/deploy/app.log",
                "lines": 2,
                "filter": "ERROR",
            }),
        ),
    )
    .await?;
    assert_eq!(tail["content"], "ERROR db timeout\nerror cache miss");
    assert_eq!(tail["matched_lines"], 2);

    // 7. ssh__upload (success, conflict when overwrite=false, overwrite=true, and sandbox rejections)
    let up = package::scoped(
        "task-deploy-1".into(),
        false,
        package::execute_tool(
            cfg.clone(),
            ws.clone(),
            "ssh__upload",
            json!({
                "server_id": dev_id,
                "local_path": "dist/app.bin",
                "remote_path": "/releases/v1/app.bin",
                "create_parents": true,
                "overwrite": false,
            }),
        ),
    )
    .await?;
    assert_eq!(up["bytes_uploaded"], 10);
    assert_eq!(
        fs::read(remote_root.path().join("releases/v1/app.bin")).await?,
        b"release-v1"
    );

    let conflict = package::execute_tool(
        cfg.clone(),
        ws.clone(),
        "ssh__upload",
        json!({
            "server_id": dev_id,
            "local_path": "dist/app.bin",
            "remote_path": "/releases/v1/app.bin",
            "overwrite": false,
        }),
    )
    .await;
    assert!(conflict.is_err());

    fs::write(workspace.path().join("dist/app.bin"), b"release-v2-bin").await?;
    package::execute_tool(
        cfg.clone(),
        ws.clone(),
        "ssh__upload",
        json!({
            "server_id": dev_id,
            "local_path": "dist/app.bin",
            "remote_path": "/releases/v1/app.bin",
            "overwrite": true,
        }),
    )
    .await?;
    assert_eq!(
        fs::read(remote_root.path().join("releases/v1/app.bin")).await?,
        b"release-v2-bin"
    );

    for bad_path in [
        "../outside.txt".to_owned(),
        outside_secret.to_string_lossy().into_owned(),
        "dist/escape.link".to_owned(),
        ".codex-workspace-mcp/secret.db".to_owned(),
    ] {
        let err = package::execute_tool(
            cfg.clone(),
            ws.clone(),
            "ssh__upload",
            json!({
                "server_id": dev_id,
                "local_path": bad_path,
                "remote_path": "/releases/leak.bin",
                "overwrite": true,
            }),
        )
        .await
        .expect_err("workspace boundary violation must fail");
        assert!(
            err.to_string().contains("WORKSPACE_BOUNDARY_DENIED"),
            "unexpected error: {err:#}"
        );
    }

    // 8. prod mandatory approval defense-in-depth
    for (tool, args) in [
        (
            "ssh__exec",
            json!({ "server_id": prod_id, "command": "echo deploy-ok" }),
        ),
        (
            "ssh__write_file",
            json!({ "server_id": prod_id, "path": "/prod.txt", "content": "x" }),
        ),
        (
            "ssh__upload",
            json!({ "server_id": prod_id, "local_path": "dist/app.bin", "remote_path": "/prod.bin" }),
        ),
    ] {
        let err = package::scoped(
            "task-prod-1".into(),
            false,
            package::execute_tool(cfg.clone(), ws.clone(), tool, args),
        )
        .await
        .expect_err("unapproved prod mutation must fail");
        assert!(
            err.to_string().contains("APPROVAL_REQUIRED"),
            "unexpected error for {tool}: {err:#}"
        );
    }

    let prod_ok = package::scoped(
        "task-prod-1".into(),
        true,
        package::execute_tool(
            cfg.clone(),
            ws.clone(),
            "ssh__exec",
            json!({ "server_id": prod_id, "command": "echo deploy-ok" }),
        ),
    )
    .await?;
    assert_eq!(prod_ok["stdout"], "deploy-ok\n");
    assert_eq!(prod_ok["exit_code"], 0);

    // 9. ssh__server_history & workspace recency ranking
    let history = package::execute_tool(
        cfg.clone(),
        ws.clone(),
        "ssh__server_history",
        json!({ "server_id": dev_id, "limit": 20 }),
    )
    .await?;
    let events = history["history"].as_array().unwrap();
    assert!(
        events
            .iter()
            .any(|e| e["action"] == "exec" && e["task_id"] == "task-deploy-1")
    );
    assert!(
        events
            .iter()
            .any(|e| e["action"] == "write_file" && e["remote_path"] == "/deploy/config.toml")
    );
    assert!(
        events
            .iter()
            .any(|e| e["action"] == "upload" && e["remote_path"] == "/releases/v1/app.bin")
    );
    assert!(events.iter().any(|e| e["action"] == "read_file"));

    let reranked =
        package::execute_tool(cfg.clone(), ws.clone(), "ssh__list_servers", json!({})).await?;
    assert_eq!(reranked["servers"][0]["id"], prod_id);
    assert!(
        !reranked["servers"][0]["recent_usage"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    Ok(())
}

#[tokio::test]
async fn ai_stream_replays_history_streams_live_exec_and_enforces_ticket() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let remote = tempfile::tempdir()?;
        let config = tempfile::tempdir()?;
        let workspace = tempfile::tempdir()?;
        fs::write(
            workspace.path().join("payload"),
            b"do-not-broadcast-file-body",
        )
        .await?;
        let fixture = Fixture::start(remote.path().to_owned()).await?;
        let (http, http_task) = start_http(config.path().to_owned()).await?;
        let base = format!("{http}/agent/ssh/servers");
        let browser = reqwest::Client::builder()
            .default_headers({
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert("sec-fetch-site", "same-origin".parse()?);
                headers
            })
            .build()?;
        let create = |tier: &'static str| {
            let browser = &browser;
            let base = &base;
            async move {
                let profile = browser
                    .post(base)
                    .json(&json!({"name":tier,"host":"127.0.0.1","port":fixture.port,
                "username":"tester","auth_type":"password","credential":PASSWORD,"env_tier":tier,"require_approval":false}))
                    .send()
                    .await?
                    .error_for_status()?
                    .json::<Value>()
                    .await?;
                Ok::<_, anyhow::Error>(profile["id"].as_str().unwrap().to_owned())
            }
        };
        let dev = create("dev").await?;
        let prod = create("prod").await?;
        let dev_url = format!("{base}/{dev}");
        let prod_url = format!("{base}/{prod}");
        let task = "test-ai-live-follow";
        let issue = |url: String, task: &str| {
            browser
                .post(format!("{url}/ai-stream-ticket"))
                .json(&json!({"task_id":task}))
        };
        assert_eq!(issue(dev_url.clone(), "   ").send().await?.status(), 400);
        let ticket = issue(dev_url.clone(), task)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        let ai_ticket = ticket["ticket"].as_str().unwrap();
        let connect =
            |url: &str, path: &str, ticket: Option<&str>, origin: Option<&str>| -> Result<_> {
                let mut url = format!("{}/{path}", url.replacen("http:", "ws:", 1));
                if let Some(ticket) = ticket {
                    url.push_str(&format!("?ticket={ticket}"));
                }
                let mut request = url.into_client_request()?;
                if let Some(origin) = origin {
                    request.headers_mut().insert("origin", origin.parse()?);
                }
                Ok(request)
            };
        async fn rejected(
            request: tokio_tungstenite::tungstenite::handshake::client::Request,
            status: u16,
        ) {
            use tokio_tungstenite::tungstenite::Error;
            assert!(matches!(tokio_tungstenite::connect_async(request).await,
                Err(Error::Http(response)) if response.status() == status));
        }
        let mut no_origin = connect(&dev_url, "ai-stream", Some(ai_ticket), None)?;
        no_origin
            .headers_mut()
            .insert("sec-fetch-site", "same-origin".parse()?);
        rejected(no_origin, 403).await;
        rejected(
            connect(
                &dev_url,
                "ai-stream",
                Some(ai_ticket),
                Some("http://localhost:9"),
            )?,
            403,
        )
        .await;
        let other_scheme = http.replacen("http:", "https:", 1);
        rejected(connect(&dev_url, "ai-stream", Some(ai_ticket), Some(&other_scheme))?, 403).await;
        rejected(connect(&dev_url, "ai-stream", None, Some(&http))?, 401).await;
        rejected(
            connect(&prod_url, "ai-stream", Some(ai_ticket), Some(&http))?,
            403,
        )
        .await;
        rejected(
            connect(&dev_url, "terminal", Some(ai_ticket), Some(&http))?,
            403,
        )
        .await;
        let terminal_ticket = browser
            .post(format!("{dev_url}/terminal-ticket"))
            .json(&json!({}))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        rejected(
            connect(
                &dev_url,
                "ai-stream",
                terminal_ticket["ticket"].as_str(),
                Some(&http),
            )?,
            403,
        )
        .await;
        let request = connect(&dev_url, "ai-stream", Some(ai_ticket), Some(&http))?;
        let (mut live, _) = tokio_tungstenite::connect_async(request.clone()).await?;
        rejected(request, 403).await;
        assert_eq!(
            live.next().await.unwrap()?.to_text()?,
            r#"{"type":"ready"}"#
        );
        live.send(Message::Text(
            r#"{"type":"resize","cols":91,"rows":30}"#.into(),
        ))
        .await?;
        live.send(Message::Binary(b"attempted-input\r".as_slice().into()))
            .await?;

        let ticket = issue(prod_url.clone(), task)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        let (mut protected, _) = tokio_tungstenite::connect_async(connect(
            &prod_url,
            "ai-stream",
            ticket["ticket"].as_str(),
            Some(&http),
        )?)
        .await?;
        assert_eq!(
            protected.next().await.unwrap()?.to_text()?,
            r#"{"type":"ready"}"#
        );
        let cfg = config.path().to_owned();
        let ws = workspace.path().to_owned();
        let denied = package::scoped(
            task.into(),
            false,
            package::execute_tool(
                cfg.clone(),
                ws.clone(),
                "ssh__exec",
                json!({"server_id":prod,"command":"echo denied"}),
            ),
        )
        .await
        .unwrap_err();
        assert!(denied.to_string().contains("APPROVAL_REQUIRED"));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), protected.next())
                .await
                .is_err()
        );
        assert_eq!(
            fixture.auth.load(Ordering::SeqCst),
            0,
            "read-only subscribers and rejected tools must not connect to SSH"
        );

        let command = format!("stream-check {PASSWORD}");
        let job = tokio::spawn(package::scoped(
            task.into(),
            false,
            package::execute_tool(
                cfg.clone(),
                ws.clone(),
                "ssh__exec",
                json!({"server_id":dev,"cwd":"/","command":command}),
            ),
        ));
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"live stdout\r\n") {
            let message = live.next().await.unwrap()?;
            assert!(message.is_binary());
            bytes.extend_from_slice(&message.into_data());
        }
        assert!(
            !job.is_finished(),
            "stdout must arrive before exec completes"
        );
        fixture.finish_exec.notify_one();
        let result = job.await??;
        assert_eq!(result["exit_code"], 42);
        assert!(
            result["stdout"].as_str().unwrap().contains(PASSWORD),
            "AI result is not truncated by the display filter"
        );

        package::scoped(
            task.into(),
            false,
            package::execute_tool(
                cfg,
                ws,
                "ssh__upload",
                json!({"server_id":dev,"local_path":"payload","remote_path":"/uploaded.bin"}),
            ),
        )
        .await?;
        let mut activity = Vec::new();
        while activity.len() < 2 {
            match live.next().await.unwrap()? {
                Message::Binary(chunk) => bytes.extend_from_slice(&chunk),
                Message::Text(text) => activity.push(serde_json::from_str::<Value>(&text)?),
                _ => {}
            }
        }
        assert_eq!(activity[0]["type"], "activity");
        assert_eq!(activity[0]["action"], "exec");
        assert_eq!(activity[0]["remote_path"], "/");
        assert_eq!(activity[1]["action"], "upload");
        assert_eq!(activity[1]["remote_path"], "/uploaded.bin");
        let text = String::from_utf8(bytes.clone())?;
        for expected in [
            "[AI · /]",
            "stream-check [redacted]",
            "live stdout\r\n",
            "live stderr\r\n",
            "[exit 42",
            "upload payload -> /uploaded.bin",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in {text:?}");
        }
        for private in [
            PASSWORD,
            "private-key-body",
            "do-not-broadcast-file-body",
            "attempted-input",
        ] {
            assert!(!text.contains(private) && !json!(activity).to_string().contains(private));
        }
        let ticket = issue(dev_url.clone(), task)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        let (mut replay, _) = tokio_tungstenite::connect_async(connect(
            &dev_url,
            "ai-stream",
            ticket["ticket"].as_str(),
            Some(&http),
        )?)
        .await?;
        assert_eq!(
            replay.next().await.unwrap()?.to_text()?,
            r#"{"type":"ready"}"#
        );
        assert_eq!(&replay.next().await.unwrap()?.into_data()[..], &bytes);
        let ticket = issue(dev_url.clone(), "other-task")
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        let (mut other, _) = tokio_tungstenite::connect_async(connect(
            &dev_url,
            "ai-stream",
            ticket["ticket"].as_str(),
            Some(&http),
        )?)
        .await?;
        assert_eq!(
            other.next().await.unwrap()?.to_text()?,
            r#"{"type":"ready"}"#
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), other.next())
                .await
                .is_err()
        );
        http_task.abort();
        Ok(())
    })
    .await?
}
