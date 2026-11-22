pub(crate) fn prepare_process_group(command: &mut Command) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
    }
    #[cfg(unix)]
    command.process_group(0);
}

#[cfg(windows)]
pub(crate) struct ProcessGroup(std::os::windows::io::OwnedHandle);
#[cfg(windows)]
impl ProcessGroup {
    pub(crate) fn attach(child: &Child) -> Result<Self> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        use windows_sys::Win32::{
            Foundation::INVALID_HANDLE_VALUE,
            System::{Diagnostics::ToolHelp::*, JobObjects::*, Threading::*},
        };
        // The child starts suspended: assign the job BEFORE npm can spawn a child.
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            ensure!(
                !handle.is_null(),
                "CreateJobObject failed: {}",
                std::io::Error::last_os_error()
            );
            let owned = std::os::windows::io::OwnedHandle::from_raw_handle(handle);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            ensure!(
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of_val(&info) as u32
                ) != 0,
                "SetInformationJobObject failed: {}",
                std::io::Error::last_os_error()
            );
            ensure!(
                AssignProcessToJobObject(
                    handle,
                    child.raw_handle().context("missing process handle")?
                ) != 0,
                "AssignProcessToJobObject failed: {}",
                std::io::Error::last_os_error()
            );
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            ensure!(
                snapshot != INVALID_HANDLE_VALUE,
                "thread snapshot failed: {}",
                std::io::Error::last_os_error()
            );
            let snapshot = std::os::windows::io::OwnedHandle::from_raw_handle(snapshot);
            let mut thread: THREADENTRY32 = std::mem::zeroed();
            thread.dwSize = std::mem::size_of_val(&thread) as u32;
            let pid = child.id().context("missing process id")?;
            let mut found = Thread32First(snapshot.as_raw_handle(), &mut thread) != 0;
            while found {
                if thread.th32OwnerProcessID == pid {
                    let handle = OpenThread(THREAD_SUSPEND_RESUME, 0, thread.th32ThreadID);
                    ensure!(
                        !handle.is_null(),
                        "OpenThread failed: {}",
                        std::io::Error::last_os_error()
                    );
                    let handle = std::os::windows::io::OwnedHandle::from_raw_handle(handle);
                    ensure!(
                        ResumeThread(handle.as_raw_handle()) != u32::MAX,
                        "ResumeThread failed: {}",
                        std::io::Error::last_os_error()
                    );
                    return Ok(Self(owned));
                }
                found = Thread32Next(snapshot.as_raw_handle(), &mut thread) != 0;
            }
            anyhow::bail!("suspended process thread not found")
        }
    }
    pub(crate) fn terminate(&self) -> Result<()> {
        use std::os::windows::io::AsRawHandle;
        ensure!(
            unsafe {
                windows_sys::Win32::System::JobObjects::TerminateJobObject(
                    self.0.as_raw_handle(),
                    1,
                )
            } != 0,
            "TerminateJobObject failed: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }
}
#[cfg(unix)]
pub(crate) struct ProcessGroup(i32);
#[cfg(unix)]
impl ProcessGroup {
    pub(crate) fn attach(child: &Child) -> Result<Self> {
        Ok(Self(child.id().context("missing process id")? as i32))
    }
    pub(crate) fn terminate(&self) -> Result<()> {
        let status = unsafe { libc::kill(-self.0, libc::SIGKILL) };
        ensure!(
            status == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
            "could not stop process group: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }
}
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

async fn supervise(
    mut child: Child,
    group: ProcessGroup,
    record: Arc<Record>,
    mut stop: mpsc::UnboundedReceiver<String>,
) {
    let out = child
        .stdout
        .take()
        .map(|reader| tokio::spawn(pump(reader, record.clone(), "stdout")));
    let err = child
        .stderr
        .take()
        .map(|reader| tokio::spawn(pump(reader, record.clone(), "stderr")));
    let (status, reason) = tokio::select! {
        result=child.wait()=>(result,None),
        reason=stop.recv()=>{
            let reason=reason.unwrap_or_else(||"host_shutdown".into());
            if let Err(error)=group.terminate(){record.push("host",format!("could not terminate process group: {error}\n")).await;let _=child.start_kill();}
            (child.wait().await,Some(reason))
        }
    };
    drop(group); // Also closes surviving descendants' output pipes.
    for reader in [out, err].into_iter().flatten() {
        let _ = reader.await;
    }
    let (exit_code, reason) = match status {
        Ok(status) => (status.code(), reason),
        Err(error) => {
            record
                .push("host", format!("process wait failed: {error}\n"))
                .await;
            (None, Some("wait_error".into()))
        }
    };
    {
        let mut meta = record.meta.lock().await;
        meta.running = false;
        meta.ready = false;
        meta.ended_at = Some(now());
        meta.exit_code = exit_code;
        meta.termination_reason = reason;
    }
    record.persist().await;
    record.ended.notify_waiters();
    bump_process_event_generation();
}

#[cfg(test)]
async fn launch(root: &Path, args: &Value, install: bool, cancel: &tokio_util::sync::CancellationToken) -> Result<Value> {
    launch_with_boundary(root, args, install, cancel, crate::tools::process_sandbox::ExecutionBoundary::Host).await
}

async fn launch_with_boundary(
    root: &Path,
    args: &Value,
    install: bool,
    cancel: &tokio_util::sync::CancellationToken,
    boundary: crate::tools::process_sandbox::ExecutionBoundary,
) -> Result<Value> {
    let root = root.canonicalize()?;
    ensure!(
        args.get("project_path")
            .is_none_or(|value| value.is_string()),
        "project_path must be a string"
    );
    let (dir, relative) = project(&root, args["project_path"].as_str().unwrap_or("."))?;
    let manifest_path = dir.join("package.json");
    ensure!(
        tokio::fs::metadata(&manifest_path)
            .await
            .context("project_path has no readable package.json")?
            .len()
            <= 4 * 1024 * 1024,
        "package.json exceeds 4 MiB"
    );
    let manifest: Value = serde_json::from_slice(&tokio::fs::read(manifest_path).await?)?;
    let script = if install {
        "install".to_owned()
    } else {
        args["script"]
            .as_str()
            .filter(|s| !s.is_empty() && !s.starts_with('-') && s.len() <= 160)
            .context("script is required")?
            .to_owned()
    };
    if !install {
        ensure!(
            manifest["scripts"][&script].is_string(),
            "script '{script}' is absent in package.json; available scripts: {}",
            manifest["scripts"]
                .as_object()
                .map(|scripts| scripts.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
                .join(", ")
        );
        for invoked_script in [
            format!("pre{script}"),
            script.clone(),
            format!("post{script}"),
        ] {
            if manifest["scripts"][&invoked_script]
                .as_str()
                .is_some_and(script_invokes_powershell)
            {
                anyhow::bail!(
                    "unsupported_script_shell: package script '{invoked_script}' directly invokes PowerShell or a .ps1 file; PowerShell execution is disabled"
                );
            }
        }
    } else if let Some(scripts) = manifest["scripts"].as_object() {
        for lifecycle in [
            "preinstall",
            "install",
            "postinstall",
            "prepublish",
            "preprepare",
            "prepare",
            "postprepare",
        ] {
            if scripts
                .get(lifecycle)
                .and_then(Value::as_str)
                .is_some_and(script_invokes_powershell)
            {
                anyhow::bail!(
                    "unsupported_script_shell: npm lifecycle script '{lifecycle}' directly invokes PowerShell or a .ps1 file; PowerShell execution is disabled"
                );
            }
        }
    }
    let extra: Vec<String> = args
        .get("args")
        .map(|args| serde_json::from_value(args.clone()))
        .transpose()?
        .unwrap_or_default();
    ensure!(
        extra.len() <= 32 && extra.iter().all(|s| s.len() <= 2000 && !s.contains('\0')),
        "script args are too large or invalid"
    );
    ensure!(
        !install || extra.is_empty(),
        "installation does not accept script args"
    );
    let background = args["background"].as_bool().unwrap_or(false);
    let (ready_url, ready_port) = probe_target(args)?;
    ensure!(
        !install || ready_url.is_none() && ready_port.is_none(),
        "dependency installation has an exit result, not a server readiness endpoint"
    );
    let (node, npm) = tokio::task::spawn_blocking(node_npm)
        .await
        .map_err(runtime_bootstrap)?
        .map_err(runtime_bootstrap)?;
    let configured_shell = effective_npm_script_shell(&node, &npm, &dir, cancel, &root, boundary)
        .await
        .map_err(runtime_bootstrap)?;
    if configured_shell
        .as_deref()
        .is_some_and(powershell_script_shell)
    {
        anyhow::bail!(
            "unsupported_script_shell: effective npm script-shell '{}' is PowerShell; PowerShell execution is disabled",
            configured_shell.as_deref().unwrap_or_default()
        );
    }
    let node_arg = external_runtime_path(&node).map_err(runtime_bootstrap)?;
    let npm_arg = external_runtime_path(&npm).map_err(runtime_bootstrap)?;
    let cwd_arg = external_runtime_path(&dir).map_err(runtime_bootstrap)?;
    let disk_dir =
        crate::tools::file_edit::workspace_path(&root, ".codex-workspace-mcp/project-processes")?;
    tokio::fs::create_dir_all(&disk_dir).await?;
    let mut registry = registry().lock().await;
    if background {
        for existing in registry.records.values() {
            let meta = existing.meta.lock().await.clone();
            if meta.running
                && meta.root == root
                && meta.sandboxed == (boundary != crate::tools::process_sandbox::ExecutionBoundary::Host)
                && meta.project_path == relative
                && meta.script == script
                && meta.args == extra
                && meta.operation == if install { "install" } else { "script" }
            {
                ensure!(
                    ready_url.is_none() || meta.ready_url == ready_url,
                    "existing process uses a different readiness URL"
                );
                ensure!(
                    ready_port.is_none() || meta.ready_port == ready_port,
                    "existing process uses a different readiness port"
                );
                let existing = existing.clone();
                drop(registry);
                probe(&existing, args).await?;
                let mut result = existing.snapshot(args).await;
                result["reused"] = json!(true);
                let fact = process_fact(&result);
                result["process_observation"] =
                    build_single_process_observation(&root, args, &[fact])?;
                return Ok(result);
            }
        }
    }
    let mut running = 0;
    for record in registry.records.values() {
        if record.meta.lock().await.running {
            running += 1;
        }
    }
    ensure!(
        running < 16,
        "host already manages sixteen running project processes; stop unused ones"
    );
    if registry.records.len() >= 128 {
        let mut oldest = None;
        for (id, record) in &registry.records {
            if !record.meta.lock().await.running {
                oldest = Some(id.clone());
                break;
            }
        }
        if let Some(id) = oldest {
            registry.records.remove(&id);
        }
    }
    // Reject an already-open endpoint before launch; it may belong to another app.
    let port = ready_port.or_else(|| {
        ready_url
            .as_ref()
            .and_then(|url| reqwest::Url::parse(url).ok())
            .and_then(|url| url.port_or_known_default())
    });
    if let Some(port) = port {
        let host = ready_url
            .as_ref()
            .and_then(|url| reqwest::Url::parse(url).ok())
            .and_then(|url| {
                url.host_str()
                    .map(|host| host.trim_matches(['[', ']']).to_owned())
            })
            .unwrap_or_else(|| "127.0.0.1".into());
        let occupied = tokio::time::timeout(
            Duration::from_millis(300),
            tokio::net::TcpStream::connect((host.as_str(), port)),
        )
        .await
        .is_ok_and(|v| v.is_ok());
        if occupied {
            let owner = listener_ownership(port, &host, 0);
            anyhow::bail!(
                "readiness port {port} is already occupied ({}); choose a free port supported by the script that actually listens, or report this conflict. Do not stop an unrelated host process and do not keep probing this address",
                owner.detail
            );
        }
    }
    let mut command = crate::tools::process_sandbox::command(node_arg, &root, &dir, boundary)?;
    command.arg(npm_arg);
    let mode = args["mode"].as_str().unwrap_or("auto");
    if install {
        ensure!(
            matches!(mode, "auto" | "install" | "ci"),
            "mode must be auto/install/ci"
        );
        let locked =
            dir.join("package-lock.json").is_file() || dir.join("npm-shrinkwrap.json").is_file();
        ensure!(
            mode != "ci" || locked,
            "npm ci requires an existing npm lockfile"
        );
        command
            .arg(if mode == "ci" || mode == "auto" && locked {
                "ci"
            } else {
                "install"
            })
            .args(["--no-audit", "--no-fund"]);
    } else {
        command.args(["run", &script]);
        if !extra.is_empty() {
            command.arg("--").args(&extra);
        }
    }
    command
        .current_dir(cwd_arg)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if !install {
        command.env("FORCE_COLOR", "0");
    }
    prepare_process_group(&mut command);
    ensure!(!cancel.is_cancelled(), "project execution cancelled");
    let mut child = command
        .spawn()
        .context("runtime_bootstrap: could not launch Node/npm")?;
    let group = match ProcessGroup::attach(&child) {
        Ok(group) => group,
        Err(error) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(runtime_bootstrap(error));
        }
    };
    let pid = child.id().context("missing child id")?;
    let id = format!(
        "project_{}_{}",
        now(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    );
    let (stop, receiver) = mpsc::unbounded_channel();
    let check_key = check_key(
        if install {
            "install_dependencies"
        } else {
            "run_project_script"
        },
        &json!({"project_path":relative,"script":script,"args":extra,"background":background}),
    );
    let script_command = if install {
        String::new()
    } else {
        manifest["scripts"][&script]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(500)
            .collect()
    };
    let record = Arc::new(Record {
        meta: Mutex::new(Meta {
            process_id: id.clone(),
            pid,
            root: root.clone(),
            project_path: relative,
            script,
            args: extra,
            operation: if install { "install" } else { "script" }.into(),
            check_key,
            running: true,
            ready: false,
            listener_owned: false,
            page_ready: false,
            readiness_detail: String::new(),
            script_command,
            node_executable: node.to_string_lossy().into_owned(),
            npm_cli: npm.to_string_lossy().into_owned(),
            ready_url,
            ready_port,
            started_at: now(),
            ended_at: None,
            exit_code: None,
            termination_reason: None,
            background,
            sandboxed: boundary != crate::tools::process_sandbox::ExecutionBoundary::Host,
        }),
        logs: Mutex::new(Logs::default()),
        persist_guard: Mutex::new(()),
        dir: disk_dir,
        stop: stop.clone(),
        ended: Notify::new(),
    });
    registry.records.insert(id, record.clone());
    bump_process_event_generation();
    tokio::spawn(supervise(child, group, record.clone(), receiver));
    drop(registry);
    let mut lease = RequestLease(Some(stop));
    record.persist().await;
    if background {
        let mut probe_args = args.clone();
        probe_args["wait_seconds"] = json!(args["wait_seconds"].as_u64().unwrap_or(15).min(60));
        let no_endpoint = {
            let meta = record.meta.lock().await;
            meta.ready_url.is_none() && meta.ready_port.is_none()
        };
        if no_endpoint {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        tokio::select! {
            result=probe(&record,&probe_args)=>result?,
            _=cancel.cancelled()=>{
                let _=record.stop.send("request_cancelled".into());
                let _=tokio::time::timeout(Duration::from_secs(10),record.wait()).await;
                anyhow::bail!("project launch cancelled");
            }
        }
        lease.0 = None;
        let mut result = record.snapshot(args).await;
        let fact = process_fact(&result);
        result["process_observation"] = build_single_process_observation(&root, args, &[fact])?;
        return Ok(result);
    }
    let timeout = args["timeout_seconds"]
        .as_u64()
        .unwrap_or(if install { 600 } else { 120 })
        .clamp(1, 1800);
    let stop_reason = tokio::select! {
        result=tokio::time::timeout(Duration::from_secs(timeout),record.wait())=>if result.is_err(){Some("timeout")}else{None},
        _=cancel.cancelled()=>Some("request_cancelled"),
    };
    if let Some(reason) = stop_reason {
        let _ = record.stop.send(reason.into());
        let _ = tokio::time::timeout(Duration::from_secs(10), record.wait()).await;
    }
    let mut result = record.snapshot(args).await;
    let fact = process_fact(&result);
    result["process_observation"] = build_single_process_observation(&root, args, &[fact])?;
    lease.0 = None;
    if let Some(reason) = stop_reason {
        result["error"] = json!(if reason == "timeout" {
            format!("operation timed out after {timeout}s")
        } else {
            "operation cancelled".into()
        });
    }
    Ok(result)
}

async fn find(root: &Path, id: &str) -> Result<Arc<Record>> {
    ensure!(
        id.starts_with("project_")
            && id.len() <= 100
            && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "invalid process_id"
    );
    let root = root.canonicalize()?;
    let record = registry().lock().await.records.get(id).cloned();
    if let Some(record) = record {
        ensure!(
            record.meta.lock().await.root == root,
            "process belongs to a different workspace"
        );
        return Ok(record);
    }
    // After host restart, old PIDs are never treated as owned processes.
    let dir = crate::tools::file_edit::workspace_path(&root, ".codex-workspace-mcp/project-processes")?;
    let mut meta: Meta = serde_json::from_slice(
        &tokio::fs::read(dir.join(format!("{id}.json")))
            .await
            .context("managed process not found")?,
    )?;
    ensure!(
        meta.root == root,
        "process belongs to a different workspace"
    );
    if meta.running {
        meta.running = false;
        meta.ready = false;
        meta.termination_reason = Some("host_restart".into());
    }
    let (stop, _receiver) = mpsc::unbounded_channel();
    let record = Arc::new(Record {
        meta: Mutex::new(meta),
        logs: Mutex::new(Logs::default()),
        persist_guard: Mutex::new(()),
        dir: dir.clone(),
        stop,
        ended: Notify::new(),
    });
    if let Ok(mut file) = tokio::fs::File::open(dir.join(format!("{id}.log"))).await {
        use tokio::io::AsyncSeekExt;
        if let Ok(meta) = file.metadata().await {
            let _ = file
                .seek(std::io::SeekFrom::Start(meta.len().saturating_sub(60_000)))
                .await;
        }
        let mut bytes = Vec::new();
        file.take(60_000).read_to_end(&mut bytes).await?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut logs = record.logs.lock().await;
        logs.chars = text.chars().count();
        let chars = text.chars().collect::<Vec<_>>();
        for chunk in chars.chunks(4096) {
            logs.sequence += 1;
            let seq = logs.sequence;
            logs.chunks.push_back(json!({"seq":seq,"stream":"history","text":chunk.iter().collect::<String>(),"cursor_reset":true}));
        }
    }
    Ok(record)
}

pub async fn execute(
    workspace: &crate::tools::Workspace,
    name: &str,
    args: Value,
) -> Result<Value> {
    execute_cancellable(
        workspace,
        name,
        args,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
}
pub async fn execute_cancellable(
    workspace: &crate::tools::Workspace,
    name: &str,
    args: Value,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<Value> {
    execute_with_boundary(workspace, name, args, cancel, crate::tools::process_sandbox::ExecutionBoundary::Host).await
}
pub async fn execute_with_boundary(
    workspace: &crate::tools::Workspace,
    name: &str,
    args: Value,
    cancel: &tokio_util::sync::CancellationToken,
    boundary: crate::tools::process_sandbox::ExecutionBoundary,
) -> Result<Value> {
    ensure!(args.is_object(), "tool arguments must be an object");
    ensure!(
        args.get("process_id").is_none_or(|id| id.is_string()),
        "process_id must be a string"
    );
    ensure!(!cancel.is_cancelled(), "project execution cancelled");
    if name == "get_project_process" && args["force_refresh"] == true {
        ensure!(
            args["observation_purpose"]
                .as_str()
                .is_some_and(|reason| !reason.trim().is_empty() && reason.chars().count() <= 300),
            "force_refresh requires a concrete observation_purpose (maximum 300 characters)"
        );
    }
    match name {
        "install_dependencies" => launch_with_boundary(workspace.root(), &args, true, cancel, boundary).await,
        "run_project_script" => launch_with_boundary(workspace.root(), &args, false, cancel, boundary).await,
        "get_project_process" => {
            if let Some(id) = args["process_id"].as_str() {
                let record = find(workspace.root(), id).await?;
                let meta = record.meta.lock().await.clone();
                ensure!(
                    args.get("project_path")
                        .and_then(Value::as_str)
                        .is_none_or(|path| meta.project_path == path.replace('\\', "/")),
                    "process_id does not belong to the requested project_path scope"
                );
                ensure!(
                    args.get("script")
                        .and_then(Value::as_str)
                        .is_none_or(|script| meta.script == script),
                    "process_id does not belong to the requested script scope"
                );
                probe(&record, &args).await?;
                let mut result = record.snapshot(&args).await;
                let fact = process_fact(&result);
                let mut scoped_args = args.clone();
                if scoped_args.get("project_path").is_none() {
                    scoped_args["project_path"] = json!(meta.project_path);
                }
                if scoped_args.get("script").is_none() {
                    scoped_args["script"] = json!(meta.script);
                }
                result["process_observation"] =
                    build_single_process_observation(workspace.root(), &scoped_args, &[fact])?;
                Ok(result)
            } else {
                let root = workspace.root().canonicalize()?;
                let records = registry()
                    .lock()
                    .await
                    .records
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
                let mut list = Vec::new();
                let project_filter = args
                    .get("project_path")
                    .and_then(Value::as_str)
                    .map(|path| path.replace('\\', "/"));
                for record in records {
                    if record.meta.lock().await.root == root {
                        let meta = record.meta.lock().await.clone();
                        if project_filter
                            .as_ref()
                            .is_some_and(|path| meta.project_path != *path)
                            || args
                                .get("script")
                                .and_then(Value::as_str)
                                .is_some_and(|script| meta.script != script)
                        {
                            continue;
                        }
                        if args.get("ready_url").is_some()
                            || args.get("ready_port").is_some()
                            || args["wait_seconds"].as_u64().unwrap_or(0) > 0
                        {
                            probe(&record, &args).await?;
                        }
                        list.push(record.snapshot(&json!({"after_seq":u64::MAX})).await);
                    }
                }
                let facts = list.iter().map(process_fact).collect::<Vec<_>>();
                let mut result = json!({"processes":list,"guidance":"Use process_id and after_seq=next_seq to read incremental logs; provide ready_url or ready_port to verify a running service."});
                result["process_observation"] =
                    build_process_observation(workspace.root(), &args, &facts)?;
                Ok(result)
            }
        }
        "stop_project_process" => {
            let id = args["process_id"]
                .as_str()
                .context("process_id is required")?;
            let record = find(workspace.root(), id).await?;
            if record.meta.lock().await.running {
                record
                    .stop
                    .send("requested_stop".into())
                    .context("process supervisor unavailable")?;
                tokio::time::timeout(Duration::from_secs(10), record.wait())
                    .await
                    .context("process stop is still pending; query its process_id")?;
            }
            Ok(record.snapshot(&args).await)
        }
        _ => anyhow::bail!("unknown project process tool"),
    }
}
