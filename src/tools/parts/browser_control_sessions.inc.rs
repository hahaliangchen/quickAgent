type TargetIds = std::collections::BTreeSet<String>;

struct BrowserTargets {
    endpoint: reqwest::Url,
    attached: bool,
    owned: TargetIds,
}

impl BrowserTargets {
    fn socket(&self, target: &str) -> Result<String> {
        ensure!(
            !target.is_empty()
                && target
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "invalid browser target ID"
        );
        let mut url = self.endpoint.clone();
        url.set_path(&format!("/devtools/page/{target}"));
        Ok(url.into())
    }

    async fn list(&mut self) -> Result<Vec<Value>> {
        let reply = call(self.endpoint.as_str(), "Target.getTargets", json!({})).await?;
        let infos = reply["result"]["targetInfos"]
            .as_array()
            .context("Chrome did not return target information")?;
        loop {
            let mut added = false;
            for info in infos.iter().filter(|info| page_target(info)) {
                if let Some(id) = info["targetId"].as_str()
                    && (!self.attached
                        || info["openerId"]
                            .as_str()
                            .is_some_and(|opener| self.owned.contains(opener)))
                {
                    added |= self.owned.insert(id.to_owned());
                }
            }
            if !added {
                break;
            }
        }
        let mut pages: Vec<_> = infos
            .iter()
            .filter(|info| {
                info["targetId"]
                    .as_str()
                    .is_some_and(|id| self.owned.contains(id))
                    && page_target(info)
            })
            .cloned()
            .collect();
        pages.sort_by(|a, b| a["targetId"].as_str().cmp(&b["targetId"].as_str()));
        Ok(pages)
    }

    async fn close_owned(&mut self) -> Result<()> {
        let _ = self.list().await;
        let mut cdp = connect_cdp(self.endpoint.as_str()).await?;
        for id in &self.owned {
            // A target may already have been closed by the page or the user.
            let reply = cdp.call("Target.closeTarget", json!({"targetId":id})).await;
            if let Err(error) = reply
                && !error.to_string().contains("No target with given id")
            {
                return Err(error);
            }
        }
        Ok(())
    }
}

fn cdp_url(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw).context("invalid configured CDP endpoint")?;
    ensure!(
        matches!(url.scheme(), "http" | "ws")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "CDP endpoint must be an http:// discovery URL or ws:// browser endpoint without credentials"
    );
    check_browser_port(&url).map(|_| url)
}

async fn browser_endpoint(raw: &str) -> Result<reqwest::Url> {
    let mut authority = cdp_url(raw)?;
    if authority.scheme() == "ws" && authority.path().starts_with("/devtools/browser/") {
        return Ok(authority);
    }
    authority
        .set_scheme("http")
        .map_err(|_| anyhow::anyhow!("invalid CDP scheme"))?;
    authority.set_path("/json/version");
    let version: Value = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()?
        .get(authority.clone())
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let announced = reqwest::Url::parse(
        version["webSocketDebuggerUrl"]
            .as_str()
            .context("Chrome did not announce a browser socket")?,
    )?;
    ensure!(
        announced.path().starts_with("/devtools/browser/"),
        "invalid Chrome browser socket path"
    );
    authority
        .set_scheme("ws")
        .map_err(|_| anyhow::anyhow!("invalid CDP scheme"))?;
    authority.set_path(announced.path());
    Ok(authority)
}

fn check_browser_port(url: &reqwest::Url) -> Result<()> {
    let Some(port) = url.port_or_known_default() else {
        return Ok(());
    };
    ensure!(
        !matches!(port, 3000 | 3001) && !crate::tools::process_sandbox::is_host_control_port(port),
        "CONTROL_PORT_BLOCKED: refusing browser access on port {port}"
    );
    Ok(())
}

fn check_link_destination(href: &Value) -> Result<()> {
    if let Some(url) = href
        .as_str()
        .and_then(|href| reqwest::Url::parse(href).ok())
    {
        check_browser_port(&url)?;
    }
    Ok(())
}

fn check_page_url(raw: &str, internal: bool) -> Result<()> {
    ensure!(raw.len() <= 2000, "browser URL is too long");
    let url = reqwest::Url::parse(raw).context("invalid browser URL")?;
    if internal && matches!(raw, "about:blank" | "about:srcdoc") {
        return Ok(());
    }
    ensure!(
        matches!(url.scheme(), "http" | "https" | "file"),
        "url must be http(s) or file"
    );
    check_browser_port(&url)
}

fn profile_dir(home: &Path, root: &Path, name: &str) -> Result<std::path::PathBuf> {
    ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')),
        "invalid browser profile_name"
    );
    let home = home
        .canonicalize()
        .context("browser home directory is unavailable")?;
    let root = root
        .canonicalize()
        .context("browser workspace directory is unavailable")?;
    let mut dir = home;
    for part in [".config", "grow", "browser-profile", name] {
        dir.push(part);
        ensure!(
            !dir.starts_with(&root),
            "persistent browser profiles cannot be stored in the workspace"
        );
        create_private_directory(&dir)?;
        let actual = dir.canonicalize()?;
        ensure!(
            actual == dir,
            "browser profile path must not follow symlinks"
        );
    }
    Ok(dir)
}

fn create_private_directory(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure!(
                std::fs::symlink_metadata(path)?.file_type().is_dir(),
                "browser storage directory is not a real directory"
            );
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

struct Lease(String);
fn browser_leases() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static LEASES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    LEASES.get_or_init(Default::default)
}
impl Lease {
    fn take(key: String) -> Result<Self> {
        ensure!(
            browser_leases().lock().unwrap().insert(key.clone()),
            "browser profile or attach endpoint is already in use by another task"
        );
        Ok(Self(key))
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        browser_leases().lock().unwrap().remove(&self.0);
    }
}

async fn attach_browser(endpoint: reqwest::Url, visible: bool) -> Result<Session> {
    // The default Chrome context shares the logged-in profile across all tabs.
    let lease = Lease::take(format!("attach:{endpoint}"))?;
    let created = call(
        endpoint.as_str(),
        "Target.createTarget",
        json!({"url":"about:blank"}),
    )
    .await?;
    let id = created["result"]["targetId"]
        .as_str()
        .context("Chrome did not create a task tab")?
        .to_owned();
    let mut targets = BrowserTargets {
        endpoint,
        attached: true,
        owned: TargetIds::from([id.clone()]),
    };
    let result = session_for_tab(targets.socket(&id)?, id, visible).await;
    match result {
        Ok(mut session) => {
            session.targets = Some(targets);
            session.lease = Some(lease);
            Ok(session)
        }
        Err(error) => {
            let _ = targets.close_owned().await;
            Err(error)
        }
    }
}

async fn session_for_tab(ws: String, page_id: String, visible: bool) -> Result<Session> {
    let network = NetworkLog::default();
    let frames = Default::default();
    let (dialog, dialog_commands, dialog_watch) =
        watch_dialogs(&ws, None, network.clone(), std::sync::Arc::clone(&frames)).await?;
    Ok(Session {
        ws,
        emulation: None,
        refs: Default::default(),
        child: None,
        profile: None,
        session_id: crate::media::visual_artifacts::new_id(),
        page_id,
        owner: Value::Null,
        epoch: 0,
        capture_seq: 0,
        signature: Value::Null,
        visible,
        dialog,
        dialog_commands,
        dialog_watch,
        targets: None,
        lease: None,
        host_launched: false,
        persistent: false,
        profile_name: None,
        network,
        frames,
        inactive_watch: Default::default(),
        downloads: None,
    })
}

async fn tab_list(session: &mut Session) -> Result<Value> {
    let targets = session
        .targets
        .as_mut()
        .context("browser target management is unavailable for this connection")?;
    let tabs: Vec<_> = targets
        .list()
        .await?
        .into_iter()
        .map(|tab| {
            json!({"target_id":tab["targetId"],"title":tab["title"],
        "url":tab["url"],"active":tab["targetId"]==session.page_id})
        })
        .collect();
    Ok(json!({"ok":true,"tabs":tabs,"target_id":session.page_id}))
}

async fn select_tab(session: &mut Session, target: &str) -> Result<Value> {
    if target != session.page_id {
        let targets = session
            .targets
            .as_mut()
            .context("browser target management is unavailable for this connection")?;
        let tabs = targets.list().await?;
        let tab = tabs
            .iter()
            .find(|tab| tab["targetId"] == target)
            .context("target is not an open task-owned tab")?;
        check_page_url(tab["url"].as_str().context("browser tab has no URL")?, true)?;
        let ws = targets.socket(target)?;
        let mut previous = if let Some(page) = session.inactive_watch.remove(target) {
            page
        } else {
            let (dialog, commands, watch) =
                watch_dialogs(&ws, None, session.network.clone(), session.frames.clone()).await?;
            InactiveWatch {
                dialog,
                commands,
                watch,
            }
        };
        call(
            targets.endpoint.as_str(),
            "Target.activateTarget",
            json!({"targetId":target}),
        )
        .await?;
        std::mem::swap(&mut session.dialog_watch, &mut previous.watch);
        std::mem::swap(&mut session.dialog, &mut previous.dialog);
        std::mem::swap(&mut session.dialog_commands, &mut previous.commands);
        session
            .inactive_watch
            .insert(session.page_id.clone(), previous);
        session.ws = ws;
        session.page_id = target.to_owned();
        session.emulation = None;
        session.signature = Value::Null;
        session.refs.observe(Value::Null);
        session.epoch += 1;
    }
    Ok(json!({"ok":true,"target_id":target}))
}
