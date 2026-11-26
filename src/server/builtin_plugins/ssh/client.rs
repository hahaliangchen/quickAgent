use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result, ensure};
use russh::{
    client,
    keys::{PrivateKeyWithHashAlg, PublicKey, agent::client::AgentClient},
};

use super::{
    credentials::Secret,
    store::{AuthType, HostKeyFingerprint, Store},
};

pub(super) struct VerifyKey {
    directory: PathBuf,
    id: String,
    observed: Arc<std::sync::Mutex<Option<HostKeyFingerprint>>>,
    failure: Arc<std::sync::Mutex<Option<anyhow::Error>>>,
}

impl client::Handler for VerifyKey {
    type Error = anyhow::Error;

    async fn check_server_key(&mut self, key: &PublicKey) -> Result<bool> {
        let (directory, id, key) = (self.directory.clone(), self.id.clone(), key.clone());
        let result = tokio::task::spawn_blocking(move || {
            let fingerprint = HostKeyFingerprint::of_key_blob(&key.to_bytes()?);
            Store::open(&directory)?.observe_host_key(&id, fingerprint)?;
            Ok::<_, anyhow::Error>(fingerprint)
        })
        .await?;
        match result {
            Ok(fingerprint) => {
                *self.observed.lock().unwrap() = Some(fingerprint);
                Ok(true)
            }
            Err(error) => {
                *self.failure.lock().unwrap() = Some(error);
                Ok(false)
            }
        }
    }
}

pub(super) type Connection = client::Handle<VerifyKey>;

pub(super) async fn connect(directory: PathBuf, id: String) -> Result<Connection> {
    Ok(connect_for_tool(directory, id, false).await?.0)
}

pub(super) async fn connect_for_tool(
    directory: PathBuf,
    id: String,
    retain_secret: bool,
) -> Result<(Connection, Option<Secret>)> {
    tokio::time::timeout(
        Duration::from_secs(20),
        connect_inner(directory, id, retain_secret),
    )
    .await
    .context("SSH connection or authentication timed out")?
}

async fn connect_inner(
    directory: PathBuf,
    id: String,
    retain_secret: bool,
) -> Result<(Connection, Option<Secret>)> {
    let profile = {
        let (directory, id) = (directory.clone(), id.clone());
        tokio::task::spawn_blocking(move || {
            Store::open(&directory)?
                .get(&id)?
                .context("SSH server not found")
        })
        .await??
    };
    let observed = Arc::default();
    let failure = Arc::default();
    let handler = VerifyKey {
        directory: directory.clone(),
        id: id.clone(),
        observed: Arc::clone(&observed),
        failure: Arc::clone(&failure),
    };
    let config = client::Config {
        keepalive_interval: Some(Duration::from_secs(20)),
        keepalive_max: 3,
        ..Default::default()
    };
    let mut session = match client::connect(
        Arc::new(config),
        (profile.host.as_str(), profile.port),
        handler,
    )
    .await
    {
        Ok(session) => session,
        Err(error) => {
            return Err(failure
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| error.context("SSH handshake failed")));
        }
    };
    let key = observed
        .lock()
        .unwrap()
        .context("SSH server key was not verified")?;
    let secret = {
        let (directory, id) = (directory.clone(), id.clone());
        tokio::task::spawn_blocking(move || Store::open(&directory)?.auth_secret(&id)).await??
    };
    let authenticated = match profile.auth_type {
        AuthType::Password => {
            let secret = secret.as_ref().context("No SSH password is saved")?;
            session
                .authenticate_password(&profile.username, secret.expose())
                .await?
                .success()
        }
        AuthType::KeyFile => {
            let path = profile
                .key_path
                .context("No SSH private key path is configured")?;
            let secret = secret.as_ref().map(|value| value.expose().to_owned());
            let private_key = tokio::task::spawn_blocking(move || {
                russh::keys::load_secret_key(path, secret.as_deref())
            })
            .await?
            .map_err(|_| {
                anyhow::anyhow!("Cannot load SSH private key; verify the key path and passphrase")
            })?;
            let hash = rsa_hash(&session, private_key.public_key()).await?;
            session
                .authenticate_publickey(
                    &profile.username,
                    PrivateKeyWithHashAlg::new(Arc::new(private_key), hash),
                )
                .await?
                .success()
        }
        AuthType::Agent => authenticate_agent(&mut session, &profile.username).await?,
    };
    ensure!(authenticated, "SSH authentication was rejected");
    tokio::task::spawn_blocking(move || Store::open(&directory)?.mark_connected(&id, key))
        .await??;
    Ok((session, if retain_secret { secret } else { None }))
}

pub(super) struct ExecOutput {
    pub(super) exit_code: Option<u32>,
    pub(super) stdout: String,
    pub(super) stderr: String,
    pub(super) stdout_truncated: bool,
    pub(super) stderr_truncated: bool,
    pub(super) idle_timed_out: bool,
}

fn posix_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

fn append_bounded(buf: &mut Vec<u8>, chunk: &[u8], limit: usize, truncated: &mut bool) {
    if buf.len() >= limit {
        if !chunk.is_empty() {
            *truncated = true;
        }
        return;
    }
    let room = limit - buf.len();
    if chunk.len() > room {
        buf.extend_from_slice(&chunk[..room]);
        *truncated = true;
    } else {
        buf.extend_from_slice(chunk);
    }
}

pub(super) type ChunkCallback<'a> = dyn FnMut(bool, &[u8]) + Send + 'a;

pub(super) async fn exec(
    connection: &Connection,
    command: &str,
    cwd: Option<&str>,
    timeout: Duration,
    idle_timeout: Option<Duration>,
    mut on_chunk: Option<&mut ChunkCallback<'_>>,
) -> Result<ExecOutput> {
    const OUTPUT_LIMIT: usize = 128 * 1024;
    ensure!(!command.trim().is_empty(), "command cannot be empty");
    ensure!(!command.contains('\0'), "command cannot contain NUL bytes");
    let wire_command = match cwd.map(str::trim).filter(|s| !s.is_empty()) {
        Some(dir) => {
            ensure!(!dir.contains('\0'), "cwd cannot contain NUL bytes");
            format!("cd -- {} && {command}", posix_quote(dir))
        }
        None => command.to_owned(),
    };
    tokio::time::timeout(timeout, async {
        let mut channel = connection.channel_open_session().await?;
        channel.exec(true, wire_command).await?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdout_truncated = false;
        let mut stderr_truncated = false;
        let mut exit_code = None;
        let mut idle_timed_out = false;
        let idle_sleep = tokio::time::sleep(idle_timeout.unwrap_or(Duration::from_secs(86_400)));
        tokio::pin!(idle_sleep);
        loop {
            let maybe_msg = if idle_timeout.is_some() {
                tokio::select! {
                    msg = channel.wait() => msg,
                    () = &mut idle_sleep => {
                        idle_timed_out = true;
                        let _ = channel.eof().await;
                        let _ = channel.close().await;
                        break;
                    }
                }
            } else {
                channel.wait().await
            };
            let Some(msg) = maybe_msg else {
                break;
            };
            match msg {
                russh::ChannelMsg::Data { data } => {
                    if !data.is_empty() {
                        if let Some(idle_dur) = idle_timeout {
                            idle_sleep
                                .as_mut()
                                .reset(tokio::time::Instant::now() + idle_dur);
                        }
                        if let Some(callback) = on_chunk.as_deref_mut() {
                            callback(false, &data);
                        }
                        append_bounded(&mut stdout, &data, OUTPUT_LIMIT, &mut stdout_truncated);
                    }
                }
                russh::ChannelMsg::ExtendedData { data, ext } if ext == 1 => {
                    if !data.is_empty() {
                        if let Some(idle_dur) = idle_timeout {
                            idle_sleep
                                .as_mut()
                                .reset(tokio::time::Instant::now() + idle_dur);
                        }
                        if let Some(callback) = on_chunk.as_deref_mut() {
                            callback(true, &data);
                        }
                        append_bounded(&mut stderr, &data, OUTPUT_LIMIT, &mut stderr_truncated);
                    }
                }
                russh::ChannelMsg::ExitStatus { exit_status } => {
                    exit_code = Some(exit_status);
                }
                russh::ChannelMsg::Close => break,
                _ => {}
            }
        }
        Ok(ExecOutput {
            exit_code,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            stdout_truncated,
            stderr_truncated,
            idle_timed_out,
        })
    })
    .await
    .context("SSH remote command timed out")?
}

async fn rsa_hash(session: &Connection, key: &PublicKey) -> Result<Option<russh::keys::HashAlg>> {
    use russh::keys::{Algorithm, HashAlg};
    if !matches!(key.algorithm(), Algorithm::Rsa { .. }) {
        return Ok(None);
    }
    match session.best_supported_rsa_hash().await? {
        Some(Some(hash)) => Ok(Some(hash)),
        Some(None) => anyhow::bail!("SSH server requires legacy RSA/SHA-1 authentication"),
        None => Ok(Some(HashAlg::Sha256)),
    }
}

async fn authenticate_agent(session: &mut Connection, user: &str) -> Result<bool> {
    #[cfg(unix)]
    let mut agent = AgentClient::connect_env()
        .await
        .context("Cannot connect to SSH_AUTH_SOCK")?;
    #[cfg(windows)]
    let mut agent = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent")
        .await
        .context("Cannot connect to Windows OpenSSH agent")?;
    for identity in agent.request_identities().await?.into_iter().take(16) {
        let key = identity.public_key().into_owned();
        let hash = rsa_hash(session, &key).await?;
        if session
            .authenticate_publickey_with(user, key, hash, &mut agent)
            .await?
            .success()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::{
        Channel, ChannelId,
        keys::{Algorithm, PrivateKey},
        server::{self, Auth, Msg, Session},
    };
    use tokio::net::TcpListener;

    struct IdleMockServer;

    impl server::Handler for IdleMockServer {
        type Error = anyhow::Error;

        async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth> {
            Ok(if user == "tester" && password == "pw" {
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
            _channel: Channel<Msg>,
            _session: &mut Session,
        ) -> Result<bool> {
            Ok(true)
        }

        async fn exec_request(
            &mut self,
            id: ChannelId,
            data: &[u8],
            session: &mut Session,
        ) -> Result<()> {
            session.channel_success(id)?;
            let cmd = String::from_utf8_lossy(data);
            let handle = session.handle();
            if cmd.trim() == "echo start; sleep 10" {
                session.data(id, b"start\n".to_vec())?;
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    let _ = handle.exit_status_request(id, 0).await;
                    let _ = handle.eof(id).await;
                    let _ = handle.close(id).await;
                });
            } else {
                tokio::spawn(async move {
                    for i in 1..=3 {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        let _ = handle.data(id, format!("tick-{i}\n").into_bytes()).await;
                    }
                    let _ = handle.exit_status_request(id, 0).await;
                    let _ = handle.eof(id).await;
                    let _ = handle.close(id).await;
                });
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn exec_idle_timeout_aborts_silent_command_and_resets_on_output() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let config = Arc::new(server::Config {
            keys: vec![PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?],
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        let server_task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let cfg = config.clone();
                tokio::spawn(async move {
                    if let Ok(session) = server::run_stream(cfg, stream, IdleMockServer).await {
                        let _ = session.await;
                    }
                });
            }
        });

        let dir = tempfile::tempdir()?;
        let server_id = {
            let store = Store::open(dir.path())?;
            store.create(super::super::store::NewServer {
                name: "idle-unit".into(),
                host: "127.0.0.1".into(),
                port: Some(port),
                username: "tester".into(),
                auth_type: AuthType::Password,
                key_path: None,
                env_tier: super::super::store::EnvTier::Dev,
                require_approval: false,
                default_dir: None,
                tags: vec![],
                notes: None,
            })?
        };
        let handler = VerifyKey {
            directory: dir.path().to_path_buf(),
            id: server_id,
            observed: Arc::default(),
            failure: Arc::default(),
        };
        let mut conn = client::connect(
            Arc::new(client::Config::default()),
            ("127.0.0.1", port),
            handler,
        )
        .await?;
        assert!(conn.authenticate_password("tester", "pw").await?.success());

        // 1. Silent command after initial output triggers idle timeout around 1s.
        let started = std::time::Instant::now();
        let out = exec(
            &conn,
            "echo start; sleep 10",
            None,
            Duration::from_secs(15),
            Some(Duration::from_secs(1)),
            None,
        )
        .await?;
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(out.idle_timed_out);
        assert_eq!(out.exit_code, None);
        assert_eq!(out.stdout, "start\n");

        // 2. Periodic output keeps resetting the idle timer and completes normally.
        let ok = exec(
            &conn,
            "tick",
            None,
            Duration::from_secs(10),
            Some(Duration::from_millis(600)),
            None,
        )
        .await?;
        assert!(!ok.idle_timed_out);
        assert_eq!(ok.exit_code, Some(0));
        assert_eq!(ok.stdout, "tick-1\ntick-2\ntick-3\n");

        server_task.abort();
        Ok(())
    }
}
