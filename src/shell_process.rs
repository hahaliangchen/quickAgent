//! Piped platform-shell sessions. The task owns the process tree until it exits.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, OnceLock, atomic::{AtomicBool, AtomicUsize, Ordering}},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    process::ChildStdin,
    sync::{Mutex, Notify, mpsc},
};
use tokio_util::sync::CancellationToken;
use crate::{agent_service::AgentServiceState, process_sandbox::ExecutionBoundary};

pub const TOOLS: &[&str] = &["exec_command", "write_stdin", "kill_session"];
pub const PROGRAM_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
#[derive(Debug)]
pub struct OutputCapacityError(pub Value);
impl std::fmt::Display for OutputCapacityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.0) }
}
impl std::error::Error for OutputCapacityError {}
pub fn is_tool(name: &str) -> bool { TOOLS.contains(&name) }

struct Completion {
    exit_code: Option<i32>,
    error: Option<String>,
    reason: Option<String>,
}
struct StartupLease(Option<CancellationToken>);
impl Drop for StartupLease {
    fn drop(&mut self) { if let Some(cancel) = &self.0 { cancel.cancel(); } }
}
#[derive(Default)]
struct Cursor {
    stdout: usize,
    stderr: usize,
    out_decoder: Decoder,
    err_decoder: Decoder,
}
struct Session {
    id: String,
    host: PathBuf,
    workspace: PathBuf,
    task: String,
    turn: usize,
    owner: Option<String>,
    command: String,
    cwd: PathBuf,
    dir: PathBuf,
    started: Instant,
    stdin: Mutex<Option<ChildStdin>>,
    cursor: Mutex<Cursor>,
    operation: Mutex<()>,
    completion: Mutex<Completion>,
    finished: AtomicBool,
    changed: Notify,
    stop: mpsc::UnboundedSender<String>,
    interval: Arc<Interval>,
    had_truncation: AtomicBool,
    archive: Mutex<Option<(u64,u64,bool,Value)>>,
}
// Commands from one task/turn share a before/after interval while their
// lifetimes overlap. A background server can therefore be probed by another
// command without falsely attributing the server's writes to that probe.
struct Interval {
    id: String,
    host: PathBuf,
    task: String,
    turn: usize,
    remaining: AtomicUsize,
    commands: AtomicUsize,
    settled: AtomicBool,
    resources: Mutex<Option<(crate::workspace_changes::MutationGuard, crate::workspace_changes::ShellSnapshot)>>,
    slot: IntervalSlot,
    owners: std::sync::Mutex<BTreeSet<String>>,
}
type IntervalSlot = Arc<Mutex<Option<Arc<Interval>>>>;
static INTERVALS: OnceLock<std::sync::Mutex<BTreeMap<PathBuf, IntervalSlot>>> = OnceLock::new();
fn interval_slot(root: &Path) -> IntervalSlot {
    INTERVALS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
        .entry(root.to_path_buf()).or_default().clone()
}

async fn begin_interval(state: &AgentServiceState, workspace: &crate::tools::Workspace,
    task: &str, turn: usize, owner:Option<&str>, cancel: &CancellationToken) -> Result<Arc<Interval>> {
    let root = workspace.root().canonicalize()?;
    let host = state.workspace.root().canonicalize()?;
    let owner = if let Some(id) = owner {
        let conn = crate::agent_service::open_db(&host)?;
        let frame = crate::function_frames::get(&conn, task, id)?;
        frame.metadata["parallel_owner"].as_str().unwrap_or("@main").to_owned()
    } else { "@main".into() };
    let slot = interval_slot(&root);
    let mut current = slot.lock().await;
    if let Some(interval) = current.as_ref() {
        ensure!(interval.host == host && interval.task == task && interval.turn == turn,
            "SHELL_BUSY: another task or turn has running commands in this workspace");
        interval.remaining.fetch_add(1, Ordering::AcqRel);
        interval.commands.fetch_add(1, Ordering::AcqRel);
        interval.owners.lock().unwrap_or_else(|e| e.into_inner()).insert(owner);
        return Ok(interval.clone());
    }
    let guard = crate::workspace_changes::mutation_guard(&root, "exec_command", &json!({}), cancel).await?;
    let before = crate::workspace_changes::shell_begin(state, workspace, task, turn).await?;
    let interval = Arc::new(Interval {
        id: format!("interval_{}", crate::agent_service::uuid_like()), host,
        task:task.into(), turn, remaining:AtomicUsize::new(1), commands:AtomicUsize::new(1),
        settled:AtomicBool::new(false),
        resources:Mutex::new(Some((guard,before))), slot:slot.clone(),
        owners:std::sync::Mutex::new(BTreeSet::from([owner])),
    });
    *current = Some(interval.clone());
    Ok(interval)
}

async fn finish_interval(state: &AgentServiceState, workspace: &crate::tools::Workspace, interval: &Interval) -> Result<()> {
    let mut current = interval.slot.lock().await;
    if interval.remaining.fetch_sub(1, Ordering::AcqRel) != 1 { return Ok(()); }
    let (guard, before) = interval.resources.lock().await.take().context("shell interval already finished")?;
    let owners = interval.owners.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect::<Vec<_>>();
    let result = crate::workspace_changes::shell_finish(state, workspace, &interval.task, interval.turn, before, &interval.id, &owners).await;
    drop(guard);
    *current = None;
    interval.settled.store(true, Ordering::Release);
    result.map(|_| ())
}
static SESSIONS: OnceLock<Mutex<BTreeMap<String, Arc<Session>>>> = OnceLock::new();
fn sessions() -> &'static Mutex<BTreeMap<String, Arc<Session>>> { SESSIONS.get_or_init(Default::default) }

pub async fn active(workspace: &Path) -> Option<String> {
    let root = workspace.canonicalize().ok()?;
    sessions().lock().await.values()
        .find(|s| s.workspace == root && !s.finished.load(Ordering::Acquire)).map(|s| s.id.clone())
}

pub async fn ensure_idle(workspace: &Path) -> Result<()> {
    if let Some(id) = active(workspace).await {
        anyhow::bail!("SHELL_BUSY: session {id} is still running. Poll it with write_stdin or stop it with kill_session before host file changes. Commands from the same task/turn may coexist; other tasks must wait.");
    }
    Ok(())
}

async fn owned(host: &Path, task: &str, id: &str) -> Result<Arc<Session>> {
    let s = sessions().lock().await.get(id).cloned()
        .context("SHELL_SESSION_NOT_FOUND: the session ended on a previous host or the id is invalid")?;
    ensure!(s.host == host.canonicalize()? && s.task == task, "SHELL_SESSION_SCOPE_DENIED");
    Ok(s)
}

pub fn boundary(mode: crate::agent_service::PermissionMode, ban_execution: bool) -> ExecutionBoundary {
    if cfg!(target_os = "linux") && ban_execution { ExecutionBoundary::ReadOnly }
    else { ExecutionBoundary::for_mode(mode) }
}

/// Native shell calls and flow-library calls enter the same session implementation.
#[allow(clippy::too_many_arguments)]
pub async fn execute(
    state: &AgentServiceState, workspace: &crate::tools::Workspace,
    task: &str, turn: usize, owner: Option<&str>, name: &str, args: &Value,
    cancel: &CancellationToken, ban_execution: bool,
) -> Result<Value> {
    execute_with_delivery(state,workspace,task,turn,owner,name,args,cancel,ban_execution,false).await
}

#[allow(clippy::too_many_arguments)]
pub async fn execute_program(
    state:&AgentServiceState,workspace:&crate::tools::Workspace,task:&str,turn:usize,
    owner:Option<&str>,name:&str,args:&Value,cancel:&CancellationToken,ban_execution:bool,
) -> Result<Value> {
    execute_with_delivery(state,workspace,task,turn,owner,name,args,cancel,ban_execution,true).await
}

#[allow(clippy::too_many_arguments)]
async fn execute_with_delivery(
    state:&AgentServiceState,workspace:&crate::tools::Workspace,task:&str,turn:usize,
    owner:Option<&str>,name:&str,args:&Value,cancel:&CancellationToken,ban_execution:bool,program:bool,
) -> Result<Value> {
    ensure!(args.is_object(), "shell arguments must be an object");
    ensure!(!cancel.is_cancelled(), "shell task cancelled");
    let mut request_lease = StartupLease(None);
    let s = match name {
        "exec_command" => {
            // Startup must reach its supervisor or rollback even if the request
            // future is dropped halfway through registration or snapshotting.
            let child_cancel = cancel.child_token();
            request_lease.0 = Some(child_cancel.clone());
            let state = state.clone();
            let workspace = crate::tools::Workspace::new(workspace.root())?;
            let (task, owner, args) = (task.to_owned(), owner.map(str::to_owned), args.clone());
            let sandbox_ctx = crate::process_sandbox::current_tool_sandbox();
            let result = tokio::spawn(async move {
                crate::process_sandbox::scope_optional_tool_sandbox(
                    sandbox_ctx,
                    launch(&state,&workspace,&task,turn,owner.as_deref(),&args,&child_cancel,ban_execution),
                ).await
            }).await.context("shell startup task failed")??;
            result
        },
        "write_stdin" | "kill_session" => {
            let id = args["session_id"].as_str().context("session_id is required")?;
            owned(state.workspace.root(), task, id).await?
        }
        _ => anyhow::bail!("unknown shell tool"),
    };
    if name == "kill_session" {
        let _ = s.stop.send("requested_stop".into());
    }
    let _operation = s.operation.lock().await;
    if name == "write_stdin" {
        if let Some(chars) = args["chars"].as_str().filter(|v| !v.is_empty()) {
            ensure!(!s.finished.load(Ordering::Acquire), "shell session has already exited");
            let mut input = s.stdin.lock().await;
            let input = input.as_mut().context("SHELL_STDIN_CLOSED: this command was started without standard input. To send it chars, start it with exec_command {stdin:true}.")?;
            tokio::select! {
                result = async { input.write_all(chars.as_bytes()).await?; input.flush().await } => result?,
                _ = cancel.cancelled() => anyhow::bail!("shell task cancelled"),
            }
        }
    }
    let duration = if name == "kill_session" { 10_000 } else {
        args["yield_time_ms"].as_u64().unwrap_or(1000).min(30_000)
    };
    tokio::select! {
        _ = wait_finished(&s, Duration::from_millis(duration)) => {},
        _ = cancel.cancelled() => { let _ = s.stop.send("task_cancelled".into()); },
    }
    let result = response(&s, args, program).await;
    if result.is_ok() { request_lease.0.take(); }
    result
}

async fn wait_finished(s: &Session, duration: Duration) {
    let _ = tokio::time::timeout(duration, async {
        loop {
            let notified = s.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if s.finished.load(Ordering::Acquire) { break; }
            notified.await;
        }
    }).await;
}

#[allow(clippy::too_many_arguments)]
async fn launch(
    state: &AgentServiceState, workspace: &crate::tools::Workspace, task: &str,
    turn: usize, owner: Option<&str>, args: &Value, cancel: &CancellationToken, ban: bool,
) -> Result<Arc<Session>> {
    let cmd_text = args["cmd"].as_str().filter(|s| !s.trim().is_empty()).context("cmd is required")?;
    crate::process_sandbox::check_secret_reference(cmd_text)?;
    let root = workspace.root().canonicalize()?;
    let cwd_raw = args["workdir"].as_str().unwrap_or(".");
    let cwd = if Path::new(cwd_raw).is_absolute() { PathBuf::from(cwd_raw) } else { root.join(cwd_raw) };
    let cwd = cwd.canonicalize().context("shell workdir does not exist")?;
    ensure!(cwd.is_dir(), "shell workdir must be a directory");
    let boundary = boundary(state.permission_mode, ban);
    ensure!(
        boundary == ExecutionBoundary::Host
            || cwd.starts_with(&root)
            || (boundary == ExecutionBoundary::Workspace
                && crate::process_sandbox::is_extra_writable_dir(&cwd)),
        "sandbox workdir is outside the workspace"
    );
    let environment = crate::shell_environment::resolve()?;
    let tracking_workspace = crate::tools::Workspace::new(workspace.root())?;
    let id = format!("shell_{}", crate::agent_service::uuid_like());
    let host = state.workspace.root().canonicalize()?;
    let dir = host.join(".codex-workspace-mcp/shell-sessions").join(&id);
    tokio::fs::create_dir_all(&dir).await?;
    for stream in ["stdout", "stderr"] { tokio::fs::File::create(dir.join(stream)).await?; }
    let mut command = crate::process_sandbox::command(&environment.executable, &root, &cwd, boundary)?;
    #[cfg(windows)]
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &pwsh_command(cmd_text)]);
    #[cfg(not(windows))]
    command.args(["--noprofile", "--norc", "-c", cmd_text]);
    // A command gets no standard input unless it asked to be sent some. With
    // a pipe left open, tools that read stdin when given no file waited on it
    // for good (2026-11-13: `rg pattern` with no path slept 9 minutes while
    // its module polled every 30 seconds).
    let input = if args["stdin"].as_bool().unwrap_or(false) { Stdio::piped() } else { Stdio::null() };
    command.current_dir(&cwd).stdin(input).stdout(Stdio::piped()).stderr(Stdio::piped())
        .kill_on_drop(true);
    environment.apply_environment(&mut command)?;
    command.env_remove("BASH_ENV").env_remove("ENV")
        .env("PYTHONUTF8", "1").env("PYTHONIOENCODING", "utf-8")
        .env("GIT_TERMINAL_PROMPT", "0").env("NO_COLOR", "1");
    #[cfg(not(windows))]
    command.env("LANG", "C.UTF-8").env("PAGER", "cat");
    crate::project_process::prepare_process_group(&mut command);
    ensure!(!cancel.is_cancelled(), "shell task cancelled");
    // Finish the tracking interval even when process startup fails.
    let interval = begin_interval(state, workspace, task, turn, owner, cancel).await?;
    if cancel.is_cancelled() {
        finish_interval(state, workspace, &interval).await?;
        anyhow::bail!("shell task cancelled");
    }
    let mut child = match command.spawn().context("could not launch platform shell") {
        Ok(child) => child,
        Err(error) => {
            finish_interval(state, workspace, &interval).await?;
            return Err(error);
        }
    };
    let group = match crate::project_process::ProcessGroup::attach(&child) {
        Ok(group) => group,
        Err(error) => {
            let _ = child.kill().await;
            finish_interval(state, workspace, &interval).await?;
            return Err(error);
        }
    };
    let (stop, mut stopped) = mpsc::unbounded_channel();
    let record = match crate::project_process::ShellRecord::register(
        &root, &cwd, &id, child.id().unwrap_or_default(), cmd_text,
        boundary != ExecutionBoundary::Host, stop.clone(),
    ).await {
        Ok(record) => Arc::new(record),
        Err(error) => {
            let _ = group.terminate();
            let _ = child.wait().await;
            finish_interval(state, workspace, &interval).await?;
            return Err(error);
        }
    };
    let s = Arc::new(Session {
        id:id.clone(), host, workspace:root, task:task.into(), turn,
        owner:Some(owner.unwrap_or("main").to_owned()), command:cmd_text.into(), cwd, dir,
        started:Instant::now(), stdin:Mutex::new(child.stdin.take()), cursor:Mutex::new(Cursor::default()),
        operation:Mutex::new(()), completion:Mutex::new(Completion { exit_code:None,error:None,reason:None }),
        finished:AtomicBool::new(false), changed:Notify::new(), stop, interval,
        had_truncation:AtomicBool::new(false), archive:Mutex::new(None),
    });
    sessions().lock().await.insert(id, s.clone());
    let out = child.stdout.take().context("shell stdout missing")?;
    let err = child.stderr.take().context("shell stderr missing")?;
    let out = tokio::spawn(pump(out, s.clone(), record.clone(), "stdout"));
    let err = tokio::spawn(pump(err, s.clone(), record.clone(), "stderr"));
    let session = s.clone();
    let state = state.clone();
    let workspace = tracking_workspace;
    let cancel = cancel.clone();
    tokio::spawn(async move {
        let (status, reason) = tokio::select! {
            status = child.wait() => (status, None),
            reason = stopped.recv() => {
                let reason = reason.unwrap_or_else(|| "host_shutdown".into());
                let _ = group.terminate();
                (child.wait().await, Some(reason))
            }
            _ = cancel.cancelled() => {
                let _ = group.terminate();
                (child.wait().await, Some("task_cancelled".into()))
            }
        };
        // Reap descendants even when the shell itself has already exited.
        let _ = group.terminate();
        drop(group);
        let mut errors = Vec::new();
        for reader in [out, err] {
            match reader.await {
                Ok(Ok(())) => {},
                other => errors.push(format!("shell output capture failed: {other:?}")),
            }
        }
        let exit_code = match status { Ok(status) => status.code(), Err(error) => { errors.push(error.to_string()); None } };
        if let Err(error) = finish_interval(&state, &workspace, &session.interval).await {
            errors.push(format!("change tracking failed: {error:#}"));
        }
        // Completion must publish the final output even if nobody polls again.
        // Raw logs remain recoverable when archival itself fails.
        let archived = async {
            let out_bytes = tokio::fs::metadata(session.dir.join("stdout")).await?.len();
            let err_bytes = tokio::fs::metadata(session.dir.join("stderr")).await?.len();
            archive_output(&session,out_bytes,err_bytes,true).await
        }.await;
        let final_asset = match archived {
            Ok(reference) => reference,
            Err(error) => { errors.push(format!("output archival failed: {error:#}")); Value::Null }
        };
        if let Err(error) = (|| -> Result<()> {
            let mut conn = crate::agent_service::open_db(&session.host)?;
            let tx = conn.transaction()?;
            crate::agent_service::append_event_tx(&tx,&session.task,"shell/end",
                &json!({"session_id":session.id,"turn":session.turn,"exit_code":exit_code,"task_node_id":session.owner.as_deref().filter(|id|*id!="main"),
                    "termination_reason":reason,"output_asset":final_asset,"errors":errors}),None)?;
            tx.commit()?;
            Ok(())
        })() { errors.push(format!("shell end record failed: {error:#}")); }
        record.finish(exit_code, reason.clone()).await;
        *session.completion.lock().await = Completion {
            exit_code, reason, error:(!errors.is_empty()).then(|| errors.join("\n")),
        };
        session.stdin.lock().await.take();
        session.finished.store(true, Ordering::Release);
        session.changed.notify_waiters();
    });
    Ok(s)
}

#[cfg(windows)]
fn pwsh_command(command: &str) -> String {
    // Escaping order follows DSH's quoteForPwsh. See third_party/DSH-LICENSE.txt.
    let body = command.replace('`', "``").replace('"', "`\"").replace('$', "`$")
        .replace('\r', "").replace('\n', "`n").replace('\u{1b}', "`e");
    format!("[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $OutputEncoding = [System.Text.UTF8Encoding]::new($false); $LASTEXITCODE = $null; try {{ Invoke-Expression \"{body}\"; $__grow_ok = $? }} catch {{ Write-Error -ErrorRecord $_; exit 1 }}; if ($null -ne $LASTEXITCODE) {{ exit ([int]$LASTEXITCODE) }}; if ($__grow_ok) {{ exit 0 }} else {{ exit 1 }}")
}

async fn pump(
    mut reader: impl tokio::io::AsyncRead + Unpin, s: Arc<Session>,
    record: Arc<crate::project_process::ShellRecord>, stream: &'static str,
) -> Result<()> {
    let mut file = tokio::fs::OpenOptions::new().append(true).open(s.dir.join(stream)).await?;
    let mut decoder = Decoder::default();
    let mut buffer = [0u8; 16_384];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 { break; }
        file.write_all(&buffer[..count]).await?;
        file.flush().await?;
        record.push(stream, decoder.decode(&buffer[..count], false)).await;
        s.changed.notify_waiters();
    }
    record.push(stream, decoder.decode(&[], true)).await;
    Ok(())
}

#[derive(Default, Clone)]
pub(crate) struct Decoder {
    pending: Vec<u8>,
    fallback: bool,
    trailing_cr: bool,
}
impl Decoder {
    pub(crate) fn decode(&mut self, bytes: &[u8], ended: bool) -> String {
        let mut text = self.decode_bytes(bytes, ended);
        if self.trailing_cr { text.insert(0, '\r'); self.trailing_cr = false; }
        if !ended && text.ends_with('\r') { text.pop(); self.trailing_cr = true; }
        text.replace("\r\n", "\n")
    }
    fn decode_bytes(&mut self, bytes: &[u8], ended: bool) -> String {
        self.pending.extend_from_slice(bytes);
        if !self.fallback {
            match std::str::from_utf8(&self.pending) {
                Ok(text) => { let text = text.to_owned(); self.pending.clear(); return text; }
                Err(error) if error.error_len().is_none() && !ended => {
                    let prefix = error.valid_up_to();
                    let text = String::from_utf8(self.pending.drain(..prefix).collect()).unwrap();
                    return text;
                }
                Err(_) => self.fallback = true,
            }
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::Globalization::{IsDBCSLeadByteEx, MultiByteToWideChar};
            let codepage = 936; // Legacy native programs can still emit GBK.
            let mut count = self.pending.len();
            if !ended {
                let mut index = 0;
                while index < count {
                    let lead = unsafe { IsDBCSLeadByteEx(codepage, self.pending[index]) } != 0;
                    if lead && index + 1 == count { count -= 1; break; }
                    index += if lead { 2 } else { 1 };
                }
            }
            if count == 0 { return String::new(); }
            let bytes: Vec<_> = self.pending.drain(..count).collect();
            let size = unsafe { MultiByteToWideChar(codepage, 0, bytes.as_ptr(), bytes.len() as i32, std::ptr::null_mut(), 0) };
            if size > 0 {
                let mut wide = vec![0u16; size as usize];
                unsafe { MultiByteToWideChar(codepage, 0, bytes.as_ptr(), bytes.len() as i32, wide.as_mut_ptr(), size); }
                return String::from_utf16_lossy(&wide);
            }
            String::from_utf8_lossy(&bytes).into_owned()
        }
        #[cfg(not(windows))]
        { String::from_utf8_lossy(&std::mem::take(&mut self.pending)).into_owned() }
    }
    fn encoding(&self) -> String {
        if !self.fallback { return "utf-8".into(); }
        #[cfg(windows)]
        { "gbk".into() }
        #[cfg(not(windows))]
        { "utf-8-lossy".into() }
    }
}

struct Preview {
    full: Option<String>, head:String, tail:VecDeque<char>,
    head_units:usize, tail_units:usize, total_units:usize, limit:usize,
}
impl Preview {
    fn new(tokens:usize) -> Self {
        Self { full:Some(String::new()),head:String::new(),tail:VecDeque::new(),
            head_units:0,tail_units:0,total_units:0,limit:tokens.saturating_mul(3) }
    }
    fn push(&mut self,text:&str) {
        for ch in text.chars() {
            let units = if ch.is_ascii() {1} else {6};
            self.total_units += units;
            if self.total_units <= self.limit {
                if let Some(full) = &mut self.full { full.push(ch); }
            } else { self.full = None; }
            if self.head_units + units <= self.limit/2 && self.total_units <= self.limit/2 {
                self.head.push(ch); self.head_units += units;
            }
            self.tail.push_back(ch); self.tail_units += units;
            while self.tail_units > self.limit/2 {
                if let Some(ch) = self.tail.pop_front() { self.tail_units -= if ch.is_ascii() {1} else {6}; }
            }
        }
    }
    fn finish(self) -> (String,bool) {
        match self.full {
            Some(text) => (text,false),
            None => (format!("{}\n[… output omitted …]\n{}",self.head,self.tail.iter().collect::<String>()),true),
        }
    }
}

async fn new_output(path:&Path, offset:usize, decoder:&mut Decoder, ended:bool, tokens:usize) -> Result<(String,bool,usize)> {
    let mut file = tokio::fs::File::open(path).await?;
    let size = file.metadata().await?.len();
    file.seek(std::io::SeekFrom::Start(offset as u64)).await?;
    let mut reader = file.take(size.saturating_sub(offset as u64));
    let mut preview = Preview::new(tokens);
    let mut buffer = [0u8;16_384];
    let mut consumed = offset;
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 { break; }
        consumed += count;
        preview.push(&decoder.decode(&buffer[..count],false));
    }
    if ended { preview.push(&decoder.decode(&[],true)); }
    let (text,cut) = preview.finish();
    Ok((text,cut,consumed))
}

async fn output_page(path:&Path, length:u64, offset:usize, amount:usize, ended:bool) -> Result<(String,usize,String)> {
    let mut reader = tokio::fs::File::open(path).await?.take(length);
    let mut decoder = Decoder::default();
    let mut buffer = [0u8;16_384];
    let (mut page,mut total) = (String::new(),0usize);
    loop {
        let count = reader.read(&mut buffer).await?;
        let text = decoder.decode(&buffer[..count],count == 0 && ended);
        for ch in text.chars() {
            if total >= offset && total-offset < amount { page.push(ch); }
            total += 1;
        }
        if count == 0 { break; }
    }
    ensure!(offset <= total,"char_offset exceeds total_chars={total}");
    Ok((page,total,decoder.encoding()))
}

async fn program_output(path:&Path,offset:usize,decoder:&mut Decoder,ended:bool,remaining:&mut usize) -> Result<(String,bool,usize,usize)> {
    let mut file = tokio::fs::File::open(path).await?;
    let size = file.metadata().await?.len();
    file.seek(std::io::SeekFrom::Start(offset as u64)).await?;
    let mut reader = file.take(size.saturating_sub(offset as u64));
    let mut output = String::new();
    let mut buffer = [0u8;16_384];
    let (mut consumed,mut cut,mut decoded_bytes) = (offset,false,0usize);
    loop {
        let count = reader.read(&mut buffer).await?;
        consumed += count;
        let text = decoder.decode(&buffer[..count],count==0 && ended);
        decoded_bytes = decoded_bytes.saturating_add(text.len());
        let mut take = text.len().min(*remaining);
        while !text.is_char_boundary(take) { take -= 1; }
        output.push_str(&text[..take]);
        *remaining -= take;
        cut |= take < text.len();
        if count==0 { break; }
    }
    Ok((output,cut,consumed,decoded_bytes))
}

fn write_output_json(path:&Path,dir:&Path,out_bytes:u64,err_bytes:u64,ended:bool) -> Result<()> {
    use std::io::{Read,Write};
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut encodings = serde_json::Map::new();
    file.write_all(b"{")?;
    for (index,(stream,bytes)) in [("stdout",out_bytes),("stderr",err_bytes)].into_iter().enumerate() {
        if index>0 { file.write_all(b",")?; }
        write!(file,"\"{stream}\":\"")?;
        let mut reader = std::fs::File::open(dir.join(stream))?.take(bytes);
        let mut decoder = Decoder::default();
        let mut buffer = [0u8;16_384];
        loop {
            let count = reader.read(&mut buffer)?;
            let text = decoder.decode(&buffer[..count],count==0 && ended);
            let escaped = serde_json::to_vec(&text)?;
            file.write_all(&escaped[1..escaped.len()-1])?;
            if count==0 { break; }
        }
        encodings.insert(stream.into(),json!(decoder.encoding()));
        file.write_all(b"\"")?;
    }
    file.write_all(b",\"encoding\":")?;
    serde_json::to_writer(&mut file,&encodings)?;
    file.write_all(b"}")?;
    file.flush()?;
    file.get_ref().sync_all()?;
    Ok(())
}

async fn archive_output(s:&Session,out_bytes:u64,err_bytes:u64,ended:bool) -> Result<Value> {
    let mut archive = s.archive.lock().await;
    if let Some((out,err,done,reference)) = archive.as_ref()
        && (*out,*err,*done)==(out_bytes,err_bytes,ended) { return Ok(reference.clone()); }
    let (host,dir,task,id,turn,owner) = (s.host.clone(),s.dir.clone(),s.task.clone(),s.id.clone(),s.turn,s.owner.clone().filter(|id|id!="main"));
    let reference = tokio::task::spawn_blocking(move || -> Result<Value> {
        let temporary = dir.join(format!("output-{}.json",crate::agent_service::uuid_like()));
        let result = (|| -> Result<Value> {
            write_output_json(&temporary,&dir,out_bytes,err_bytes,ended)?;
            let mut conn = crate::agent_service::open_db(&host)?;
            let tx = conn.transaction()?;
            let seq = crate::agent_service::append_event_tx(&tx,&task,"shell/output",
                &json!({"session_id":id,"turn":turn,"stdout_bytes":out_bytes,"stderr_bytes":err_bytes,"task_node_id":owner}),None)?;
            tx.commit()?;
            crate::context_assets::register_file(&host,&task,seq,"shell_output",&json!({"session_id":id}),&temporary)
        })();
        let _ = std::fs::remove_file(&temporary);
        result
    }).await??;
    *archive = Some((out_bytes,err_bytes,ended,reference.clone()));
    Ok(reference)
}

async fn response(s: &Session, args: &Value, program:bool) -> Result<Value> {
    let ended = s.finished.load(Ordering::Acquire);
    let max = args["max_output_tokens"].as_u64().unwrap_or(4000).clamp(128, 24_000) as usize;
    let mut cursor = s.cursor.lock().await;
    let paging = args["stream"].as_str().is_some();
    let (out_offset, err_offset) = (cursor.stdout, cursor.stderr);
    let (stdout, out_cut, out_bytes, stderr, err_cut, err_bytes) = if paging {
        (String::new(),false,tokio::fs::metadata(s.dir.join("stdout")).await?.len(),
         String::new(),false,tokio::fs::metadata(s.dir.join("stderr")).await?.len())
    } else if program {
        let mut out_decoder = cursor.out_decoder.clone();
        let mut err_decoder = cursor.err_decoder.clone();
        let mut remaining = PROGRAM_OUTPUT_LIMIT;
        let (stdout,out_cut,out_bytes,out_size) = program_output(&s.dir.join("stdout"),out_offset,&mut out_decoder,ended,&mut remaining).await?;
        let (stderr,err_cut,err_bytes,err_size) = program_output(&s.dir.join("stderr"),err_offset,&mut err_decoder,ended,&mut remaining).await?;
        if out_cut || err_cut {
            drop(cursor);
            let reference = archive_output(s,out_bytes as u64,err_bytes as u64,ended).await?;
            return Err(OutputCapacityError(json!({"name":"ResourceLimitError","code":"SHELL_OUTPUT_OVER_CAPACITY",
                "boundary":"program_heap","message":"Shell output exceeds this delivery's program-memory budget. No stdout/stderr prefix was delivered and the read cursor was not advanced. The command already ran; reuse its saved output or page this session instead of executing it again.",
                "limit_bytes":PROGRAM_OUTPUT_LIMIT,"actual_bytes":out_size.saturating_add(err_size),
                "session_id":s.id,"original_saved":true,"delivered":false,"output_asset":reference,
                "read_output":{"tool":"write_stdin","arguments":{"session_id":s.id,"stream":"stdout","char_offset":0}}})).into());
        }
        cursor.out_decoder = out_decoder;
        cursor.err_decoder = err_decoder;
        cursor.stdout = out_bytes;
        cursor.stderr = err_bytes;
        (stdout,out_cut,out_bytes as u64,stderr,err_cut,err_bytes as u64)
    } else {
        let (stdout,out_cut,out_bytes) = new_output(&s.dir.join("stdout"),out_offset,&mut cursor.out_decoder,ended,max/2).await?;
        let (stderr,err_cut,err_bytes) = new_output(&s.dir.join("stderr"),err_offset,&mut cursor.err_decoder,ended,max/2).await?;
        cursor.stdout = out_bytes;
        cursor.stderr = err_bytes;
        (stdout,out_cut,out_bytes as u64,stderr,err_cut,err_bytes as u64)
    };
    let encodings = json!({"stdout":cursor.out_decoder.encoding(),"stderr":cursor.err_decoder.encoding()});
    drop(cursor);
    let completion = s.completion.lock().await;
    let mut result = json!({
        "session_id":s.id,"running":!ended,"exit_code":completion.exit_code,
        "duration_ms":s.started.elapsed().as_millis() as u64,"workdir":s.cwd.to_string_lossy(),
        "cmd":s.command,"encoding":encodings,"capture_error":completion.error,"termination_reason":completion.reason,
        "change_interval":{"id":s.interval.id,"task":s.interval.task,"turn":s.interval.turn,
            "commands":s.interval.commands.load(Ordering::Acquire),"merged":s.interval.commands.load(Ordering::Acquire)>1},
        "changes_pending":!s.interval.settled.load(Ordering::Acquire),
        "delivery":if program {"program"} else {"preview"},
        "output_limit_bytes":if program {Some(PROGRAM_OUTPUT_LIMIT)} else {None},
        "stdout_bytes":out_bytes,"stderr_bytes":err_bytes,
    });
    if let Some(stream) = args["stream"].as_str() {
        ensure!(matches!(stream,"stdout"|"stderr"), "stream must be stdout or stderr");
        let offset = args["char_offset"].as_u64().unwrap_or(0) as usize;
        let amount = max / 2;
        let (page,total,encoding) = output_page(&s.dir.join(stream),if stream=="stdout" {out_bytes} else {err_bytes},offset,amount,ended).await?;
        result["encoding"][stream] = json!(encoding);
        result["stream"] = json!(stream);
        result["char_offset"] = json!(offset);
        result["next_char_offset"] = json!((offset.saturating_add(amount) < total).then_some(offset.saturating_add(amount)));
        result["total_chars"] = json!(total);
        result["output"] = json!(page);
    }
    if out_cut || err_cut { s.had_truncation.store(true,Ordering::Release); }
    if out_cut || err_cut || (ended && s.had_truncation.load(Ordering::Acquire)) {
        result["output_asset"] = archive_output(s,out_bytes,err_bytes,ended).await?;
        result["read_output"] = json!({"tool":"write_stdin","arguments":{"session_id":s.id,"stream":"stdout","char_offset":0}});
    }
    result["stdout"] = json!(stdout);
    result["stderr"] = json!(stderr);
    result["truncated"] = json!(out_cut || err_cut);
    Ok(result)
}

pub async fn cleanup_task(host: &Path, task: &str) {
    let Ok(host) = host.canonicalize() else { return };
    let owned = sessions().lock().await.values()
        .filter(|s| s.host == host && s.task == task).cloned().collect::<Vec<_>>();
    for s in &owned { let _ = s.stop.send("task_finished".into()); }
    for s in owned { wait_finished(&s, Duration::from_secs(10)).await; }
}

pub async fn cleanup_frame(host: &Path, task: &str, owner: &str) {
    let Ok(host) = host.canonicalize() else { return };
    let records = sessions().lock().await.values()
        .filter(|s| s.host == host && s.task == task && s.owner.as_deref() == Some(owner))
        .cloned().collect::<Vec<_>>();
    for s in &records { let _ = s.stop.send("program_failed".into()); }
    for s in records { wait_finished(&s, Duration::from_secs(10)).await; }
}

pub fn definitions() -> Vec<Value> {
    let limit = json!({"type":"integer","minimum":128,"maximum":24000,"default":4000});
    let wait = json!({"type":"integer","minimum":0,"maximum":30000,"default":1000});
    let tool = |name: &str, description: &str, properties: Value, required: Value| {
        json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},"outputSchema":output_schema()})
    };
    vec![
        tool("exec_command", if cfg!(windows) {
            "Run PowerShell 7 with pipes, no profile or PTY. Use PowerShell syntax; each call starts a fresh shell. Module code receives full stdout/stderr (LF normalized) and exit_code, up to the combined program-memory budget per delivery. Excess output fails with SHELL_OUTPUT_OVER_CAPACITY and a saved-output reference; no partial prefix is returned to code. max_output_tokens limits external MCP previews only. The command gets no standard input unless started with stdin:true. Long commands return session_id. Same-task/turn commands may overlap in one change interval. Oversized output is fully saved as an asset and can be paged. In Grow, this method is for module deterministic code only."
        } else {
            "Run Bash with pipes, no profile or PTY. Use Bash syntax; each call starts a fresh shell. Module code receives full stdout/stderr and exit_code, up to the combined program-memory budget per delivery. Excess output fails with SHELL_OUTPUT_OVER_CAPACITY and a saved-output reference; no partial prefix is returned to code. max_output_tokens limits external MCP previews only. The command gets no standard input unless started with stdin:true. Long commands return session_id. Same-task/turn commands may overlap in one change interval. Oversized output is fully saved as an asset and can be paged. In Grow, this method is for module deterministic code only."
        },
            json!({"cmd":{"type":"string","minLength":1},"workdir":{"type":"string","default":"."},"yield_time_ms":wait,"max_output_tokens":limit,
                "stdin":{"type":"boolean","default":false,"description":"Keep standard input open so write_stdin can send chars to this command. Off by default: the command reads end-of-input at once, so tools that fall back to stdin when given no file do not wait."}}),json!(["cmd"])),
        tool("write_stdin", "Read new output from your shell session, or send chars to the stdin of a command started with stdin:true. For full output paging use stream and char_offset; this works in modules too. Reading output never re-executes the command.",
            json!({"session_id":{"type":"string"},"chars":{"type":"string","default":""},"yield_time_ms":wait,
                "max_output_tokens":limit,"stream":{"type":"string","enum":["stdout","stderr"]},
                "char_offset":{"type":"integer","minimum":0}}),json!(["session_id"])),
        tool("kill_session", "Stop your shell session's entire process tree and return its final output.",json!({"session_id":{"type":"string"}}),json!(["session_id"])),
    ]
}

/// The main AI's own read-only command (design guide §4). One-shot, no
/// session or change tracking: the whole filesystem is mounted read-only and
/// the network is cut, so there is nothing to track. Linux only, where that
/// sandbox exists; elsewhere the tool is not published.
pub const READ_ONLY: &str = "run_readonly_command";
const READ_ONLY_DEFAULT_MS: u64 = 30_000;
const READ_ONLY_MAX_MS: u64 = 900_000;

pub fn read_only_available() -> bool {
    crate::process_sandbox::bwrap_installed() && crate::shell_environment::resolve().is_ok()
}

pub fn read_only_definition() -> Value {
    json!({"type":"function","function":{"name":READ_ONLY,
        "description":"Run one Bash command yourself to look at the project or check it: git status/log/diff/show, rg, ls, wc, sed -n, and builds and tests such as cargo build/test, npm test or pytest. The project's own files are read-only and there is no network; only its build-output folders (the ones git ignores, such as target/ or node_modules/) and a private /tmp, discarded afterwards, are writable. So builds and tests run, but a command cannot change source files, install or fetch anything: make changes with the file-editing tools, and use module code for commands that must change project files or reach the network. Each call starts a fresh shell in workdir (default: the project root), without stdin, profile or PTY, and returns full stdout/stderr with exit_code once the command exits (rg: 0 matches, 1 none, 2 error). For a long build or test run, send its whole output to a file in a build-output folder once (cmd > target/run.log 2>&1) and read what you need from that file with later commands; do not run it again to see another part. More than 8 MiB of delivered output fails without a prefix. A command still running at timeout_ms is stopped; give builds and test suites a longer timeout_ms.",
        "parameters":{"type":"object","properties":{
            "cmd":{"type":"string","minLength":1},
            "workdir":{"type":"string","description":"Directory inside the project, relative to its root. Default: the root."},
            "timeout_ms":{"type":"integer","minimum":1000,"maximum":READ_ONLY_MAX_MS,"default":READ_ONLY_DEFAULT_MS}},
            "required":["cmd"],"additionalProperties":false}}})
}

/// The project's build-output folders: directories git ignores, except git's
/// own data, the host's state and nested checkouts such as other agents'
/// worktrees. Outside a git repository there are none.
fn build_outputs(root: &Path) -> Vec<PathBuf> {
    let Ok(output) = std::process::Command::new("git")
        .args(["ls-files", "--others", "--ignored", "--exclude-standard", "--directory"])
        .current_dir(root).stdin(Stdio::null()).stderr(Stdio::null()).output()
    else { return Vec::new() };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_suffix('/'))
        .filter(|dir| {
            let first = dir.split('/').next().unwrap_or_default();
            !matches!(first, ".git" | ".codex-workspace-mcp" | ".claude")
        })
        .map(|dir| root.join(dir))
        .filter(|dir| !dir.join(".git").exists())
        .collect()
}

pub async fn run_read_only(workspace: &crate::tools::Workspace, args: &Value, cancel: &CancellationToken) -> Result<Value> {
    ensure!(cfg!(target_os = "linux"), "READ_ONLY_SHELL_UNAVAILABLE: the read-only sandbox exists only on Linux; use the file tools, or module code");
    let cmd = args["cmd"].as_str().filter(|s| !s.trim().is_empty()).context("cmd is required")?;
    crate::process_sandbox::check_secret_reference(cmd)?;
    let root = workspace.root().canonicalize()?;
    let cwd = root.join(args["workdir"].as_str().unwrap_or(".")).canonicalize().context("workdir does not exist")?;
    ensure!(cwd.is_dir() && cwd.starts_with(&root), "workdir must be a directory inside the project");
    let timeout = Duration::from_millis(args["timeout_ms"].as_u64().unwrap_or(READ_ONLY_DEFAULT_MS).clamp(1000, READ_ONLY_MAX_MS));
    let environment = crate::shell_environment::resolve()?;
    let outputs = build_outputs(&root);
    let mut command = crate::process_sandbox::read_only_offline_command(&environment.executable, &root, &cwd, &outputs)?;
    command.args(["--noprofile", "--norc", "-c", cmd]);
    command.current_dir(&cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    environment.apply_environment(&mut command)?;
    command.env_remove("BASH_ENV").env_remove("ENV")
        .env("PYTHONUTF8", "1").env("PYTHONIOENCODING", "utf-8")
        .env("GIT_TERMINAL_PROMPT", "0").env("GIT_OPTIONAL_LOCKS", "0").env("NO_COLOR", "1")
        .env("LANG", "C.UTF-8").env("PAGER", "cat")
        // There is no network: package managers use what is already cached.
        .env("CARGO_NET_OFFLINE", "true").env("npm_config_offline", "true")
        .env("PIP_NO_INDEX", "1").env("PYTHONDONTWRITEBYTECODE", "1");
    crate::project_process::prepare_process_group(&mut command);
    let started = Instant::now();
    let mut child = command.spawn().context("could not launch the read-only shell")?;
    let group = crate::project_process::ProcessGroup::attach(&child)?;
    let total = Arc::new(AtomicUsize::new(0));
    let over = CancellationToken::new();
    let collect = |reader: Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>| {
        let (total, over) = (total.clone(), over.clone());
        tokio::spawn(async move {
            let mut bytes = Vec::new();
            let Some(mut reader) = reader else { return bytes };
            let mut buffer = [0u8; 16_384];
            while let Ok(n) = reader.read(&mut buffer).await {
                if n == 0 { break; }
                if total.fetch_add(n, Ordering::AcqRel) + n > PROGRAM_OUTPUT_LIMIT { over.cancel(); break; }
                bytes.extend_from_slice(&buffer[..n]);
            }
            bytes
        })
    };
    let out = collect(child.stdout.take().map(|r| Box::new(r) as _));
    let err = collect(child.stderr.take().map(|r| Box::new(r) as _));
    let stopped = tokio::select! {
        status = child.wait() => {
            let _ = group.terminate();
            let code = status?.code();
            let delivered = finish(code, None, out, err, started).await?;
            // The sandbox itself did not start (inside another sandbox, or a
            // kernel that refuses its namespaces): say so, not "command failed".
            if code != Some(0) && delivered["stdout"] == "" && delivered["stderr"].as_str().is_some_and(|e| e.starts_with("bwrap:")) {
                anyhow::bail!("SANDBOX_UNAVAILABLE: {}", delivered["stderr"].as_str().unwrap_or_default().trim());
            }
            ensure!(!over.is_cancelled(), "SHELL_OUTPUT_OVER_CAPACITY: the command wrote more than {} bytes; nothing is delivered. Narrow it: scope the path, filter with grep, or count first with wc -l.", PROGRAM_OUTPUT_LIMIT);
            return Ok(delivered);
        }
        _ = tokio::time::sleep(timeout) => "timeout",
        _ = over.cancelled() => "output_over_capacity",
        _ = cancel.cancelled() => "task_cancelled",
    };
    let _ = group.terminate();
    let _ = child.wait().await;
    ensure!(stopped != "task_cancelled", "task cancelled");
    ensure!(stopped != "output_over_capacity",
        "SHELL_OUTPUT_OVER_CAPACITY: the command wrote more than {} bytes and was stopped; nothing is delivered. Narrow it: scope the path, filter with grep, or count first with wc -l.", PROGRAM_OUTPUT_LIMIT);
    finish(None, Some(stopped), out, err, started).await
}

async fn finish(exit_code: Option<i32>, stopped: Option<&str>, out: tokio::task::JoinHandle<Vec<u8>>,
    err: tokio::task::JoinHandle<Vec<u8>>, started: Instant) -> Result<Value> {
    let decode = |bytes: Vec<u8>| Decoder::default().decode(&bytes, true);
    let (stdout, stderr) = (decode(out.await?), decode(err.await?));
    let mut result = json!({"exit_code":exit_code,"stdout":stdout,"stderr":stderr,"duration_ms":started.elapsed().as_millis() as u64});
    if let Some(reason) = stopped {
        result["termination_reason"] = json!(reason);
        result["note"] = json!("The command did not finish within timeout_ms and was stopped; stdout/stderr hold what it wrote before that. Narrow the command or raise timeout_ms.");
    }
    Ok(result)
}

pub fn output_schema() -> Value {
    json!({"type":"object","description":"Fields are returned directly at the top level, never nested under data or result. Append stdout/stderr chunks when polling a long session.",
        "required":["session_id","running","exit_code","stdout","stderr","truncated","duration_ms","encoding","capture_error","termination_reason","delivery","output_limit_bytes","stdout_bytes","stderr_bytes","changes_pending","change_interval"],
        "properties":{
            "session_id":{"type":"string","description":"Always present, including after exit; poll only while running is true."},
            "running":{"type":"boolean","description":"True until process exit, capture and finalization finish; do not parse a partial JSON document as complete."},
            "exit_code":{"type":["integer","null"],"description":"Actual exit status; null while pending or terminated without a numeric exit code. rg uses 0=matches, 1=no matches, 2=error."},
            "stdout":{"type":"string","description":"New decoded stdout since the previous normal delivery. In module code, complete text or a SHELL_OUTPUT_OVER_CAPACITY failure with the original saved; never a successful partial prefix. Paging does not consume this cursor."},
            "stderr":{"type":"string","description":"New decoded stderr; append it across polls separately from stdout."},
            "truncated":{"type":"boolean","description":"Only external MCP preview delivery can truncate. Program delivery exceeds its memory budget by failing explicitly with SHELL_OUTPUT_OVER_CAPACITY and the saved output reference. Explicit stream pages provide next_char_offset."},
            "duration_ms":{"type":"integer"},
            "encoding":{"type":"object","required":["stdout","stderr"],"properties":{"stdout":{"type":"string"},"stderr":{"type":"string"}}},
            "capture_error":{"type":["string","null"]},"termination_reason":{"type":["string","null"]},
            "delivery":{"enum":["program","preview"]},"output_limit_bytes":{"type":["integer","null"]},
            "stdout_bytes":{"type":"integer","description":"Cumulative raw bytes captured; not just this chunk."},"stderr_bytes":{"type":"integer"},
            "changes_pending":{"type":"boolean","description":"Other overlapping commands may keep the shared change interval open after this session exits."},
            "change_interval":{"type":"object","required":["id","task","turn","commands","merged"],"properties":{
                "id":{"type":"string"},"task":{"type":"string"},"turn":{"type":"integer"},"commands":{"type":"integer"},"merged":{"type":"boolean"}}},
            "output_asset":{"type":"object","description":"Stable reference to an immutable full stdout/stderr snapshot."},
            "read_output":{"type":"object"},
            "stream":{"enum":["stdout","stderr"]},"output":{"type":"string","description":"Requested stream page; only present with stream."},
            "char_offset":{"type":"integer"},"next_char_offset":{"type":["integer","null"]},"total_chars":{"type":"integer"}
        }})
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture { root: PathBuf, state: AgentServiceState, cancel: CancellationToken }
    impl Fixture {
        fn new() -> Result<Self> {
            let root = std::env::temp_dir().join(format!("grow-shell-test-{}", crate::agent_service::uuid_like()));
            std::fs::create_dir_all(&root)?;
            let mut state = crate::agent_service::tests::test_state(&root, "127.0.0.1:9".parse()?);
            state.permission_mode = crate::agent_service::PermissionMode::FullAccess;
            crate::agent_service::open_db(&root)?.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('shell-test','','fake','running',0,0)", [])?;
            Ok(Self { root, state, cancel: CancellationToken::new() })
        }
        async fn call(&self, name: &str, args: Value) -> Result<Value> {
            let result = execute(&self.state, &self.state.workspace, "shell-test", 1, None, name, &args, &self.cancel, false).await?;
            let validator = jsonschema::validator_for(&output_schema())?;
            let errors = validator.iter_errors(&result).map(|error| error.to_string()).collect::<Vec<_>>();
            ensure!(errors.is_empty(),"shell output contract mismatch: {errors:?}");
            Ok(result)
        }
        async fn run(&self, cmd: &str) -> Result<Value> {
            self.call("exec_command", json!({"cmd":cmd,"yield_time_ms":10_000})).await
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) { self.cancel.cancel(); /* Keep failed-test artifacts for diagnosis. */ }
    }

    /// The main AI's own command looks without changing anything: reads and
    /// git work, a write fails and leaves no file, there is no network
    /// interface but loopback, a hung command is stopped at its timeout, and
    /// output past the budget fails without a prefix.
    #[tokio::test]
    async fn a_read_only_command_reads_and_cannot_write_or_reach_the_network() -> Result<()> {
        let f = Fixture::new()?;
        std::fs::write(f.root.join("seed.txt"), "seed-content\n")?;
        let workspace = crate::tools::Workspace::new(&f.root)?;
        let (workspace, cancel) = (&workspace, &f.cancel);
        let run = |cmd: &str, timeout: u64| {
            let args = json!({"cmd":cmd,"timeout_ms":timeout});
            async move { run_read_only(workspace, &args, cancel).await }
        };
        let first = match run("cat seed.txt", 10_000).await {
            Ok(value) => value,
            Err(error) if !cfg!(target_os = "linux") || format!("{error:#}").contains("SANDBOX_UNAVAILABLE") => {
                eprintln!("skipped: read-only sandbox unavailable: {error:#}");
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        assert_eq!(first["exit_code"], 0, "{first}");
        assert_eq!(first["stdout"], "seed-content\n");
        let wrote = run("printf x > made.txt; echo status=$?", 10_000).await?;
        assert!(wrote["stdout"].as_str().unwrap().contains("status=1"), "{wrote}");
        assert!(!f.root.join("made.txt").exists(), "the workspace is read-only");
        let net = run("grep -c : /proc/net/dev", 10_000).await?;
        assert_eq!(net["stdout"].as_str().unwrap().trim(), "1", "only loopback: {net}");
        let slow = run("echo started; sleep 30", 1000).await?;
        assert_eq!(slow["termination_reason"], "timeout", "{slow}");
        assert_eq!(slow["stdout"], "started\n");
        assert!(slow["duration_ms"].as_u64().unwrap() < 10_000, "{slow}");
        let flood = run("head -c 9000000 /dev/zero", 30_000).await.unwrap_err();
        assert!(format!("{flood:#}").contains("SHELL_OUTPUT_OVER_CAPACITY"), "{flood:#}");
        let outside = run_read_only(workspace, &json!({"cmd":"pwd","workdir":".."}), cancel).await.unwrap_err();
        assert!(format!("{outside:#}").contains("inside the project"), "{outside:#}");
        Ok(())
    }

    /// Builds and tests run from the main AI's own command: what git ignores
    /// (target/) and a private /tmp are writable, sources are not.
    #[tokio::test]
    async fn builds_and_tests_write_only_their_outputs() -> Result<()> {
        let f = Fixture::new()?;
        let git = |args: &[&str]| std::process::Command::new("git").args(args).current_dir(&f.root).output();
        if git(&["init", "-q"]).is_err() { eprintln!("skipped: git missing"); return Ok(()); }
        std::fs::write(f.root.join(".gitignore"), "target/\n")?;
        std::fs::create_dir_all(f.root.join("target"))?;
        std::fs::create_dir_all(f.root.join("src"))?;
        // One registry dependency: cargo reads its cache, which stays read-only.
        std::fs::write(f.root.join("Cargo.toml"), "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\nitoa = \"1\"\n")?;
        std::fs::write(f.root.join("src/lib.rs"), "#[test]\nfn adds() { assert_eq!(itoa::Buffer::new().format(2), \"2\"); }\n")?;
        let workspace = crate::tools::Workspace::new(&f.root)?;
        let run = |cmd: &str| { let args = json!({"cmd":cmd,"timeout_ms":300_000}); let (w, c) = (&workspace, &f.cancel); async move { run_read_only(w, &args, c).await } };
        let first = match run("echo out > target/made && cat target/made && echo tmp > /tmp/x && cat /tmp/x").await {
            Ok(value) => value,
            Err(error) if format!("{error:#}").contains("SANDBOX_UNAVAILABLE") || !cfg!(target_os = "linux") => {
                eprintln!("skipped: {error:#}"); return Ok(());
            }
            Err(error) => return Err(error),
        };
        assert_eq!(first["stdout"], "out\ntmp\n", "{first}");
        let source = run("printf x >> src/lib.rs; echo status=$?").await?;
        assert!(source["stdout"].as_str().unwrap().contains("status=1"), "sources stay read-only: {source}");
        if std::process::Command::new("cargo").arg("--version").output().is_ok() {
            // A checked-out project has its lockfile; cargo does not rewrite it.
            std::process::Command::new("cargo").args(["generate-lockfile", "--offline"]).current_dir(&f.root).output()?;
            let tested = run("cargo test --quiet 2>&1 | tail -15").await?;
            assert!(tested["stdout"].as_str().unwrap().contains("1 passed"), "{tested}");
        }
        assert!(!std::path::Path::new("/tmp/x").exists() || std::fs::read_to_string("/tmp/x").map(|x| x != "tmp\n").unwrap_or(true),
            "the command's /tmp is its own");
        Ok(())
    }

    #[test]
    fn decoder_keeps_split_utf8_and_system_codepage_bytes() {
        let mut decoder = Decoder::default();
        assert_eq!(decoder.decode(&[b'a', 0xe4], false), "a");
        assert_eq!(decoder.decode(&[0xb8], false), "");
        assert_eq!(decoder.decode(&[0xad, 0xe6, 0x96], false), "中");
        assert_eq!(decoder.decode(&[0x87], true), "文");
        assert_eq!(decoder.encoding(), "utf-8");
        let mut decoder = Decoder::default();
        let mut result = decoder.decode(&[0xd6], false);
        result += &decoder.decode(&[0xd0, 0xce], false);
        result += &decoder.decode(&[0xc4], true);
        assert!(decoder.fallback);
        #[cfg(windows)]
        {
            assert_eq!(result, "中文");
            assert_eq!(decoder.encoding(), "gbk");
        }
        let mut decoder = Decoder::default();
        assert_eq!(decoder.decode(b"first\r", false), "first");
        assert_eq!(decoder.decode(b"\nnext\r", false), "\nnext");
        assert_eq!(decoder.decode(b"\n", true), "\n");
    }

    #[tokio::test]
    async fn platform_shell_preserves_exit_status_encoding_and_environment() -> Result<()> {
        let f = Fixture::new()?;
        let v = f.run("exit 9").await?;
        assert_eq!(v["running"], false, "{v}");
        assert_eq!(v["exit_code"], 9);
        let v = f.run(if cfg!(windows) { "cmd /c exit 9; Write-Output ok" } else { "false; echo ok" }).await?;
        assert_eq!(v["exit_code"], if cfg!(windows) { 9 } else { 0 });
        assert_eq!(v["stdout"], "ok\n");
        let v = f.run(if cfg!(windows) { "cmd /c exit 0; Write-Output ok" } else { "true; echo ok" }).await?;
        assert_eq!(v["exit_code"], 0);
        assert_eq!(v["stdout"], "ok\n");
        let v = f.run(if cfg!(windows) { "throw 'x'" } else { "exit 1" }).await?;
        assert_eq!(v["exit_code"], 1, "{v}");
        let v = f.run("python -c \"print('中文')\"").await?;
        assert_eq!(v["exit_code"], 0, "{v}");
        assert_eq!(v["stdout"].as_str().unwrap().trim(), "中文");
        let v = f.run(if cfg!(windows) { "Get-Command rg,git,node,npm,cargo,python -ErrorAction Stop | ForEach-Object { $_.Source }" } else { "command -v rg git node npm cargo python" }).await?;
        assert_eq!(v["exit_code"], 0, "{v}");
        assert_eq!(v["stdout"].as_str().unwrap().lines().count(), 6, "{v}");
        let v = f.run("node -e 'process.stdout.write(Buffer.from([214,208,206,196]))'").await?;
        assert_eq!(v["encoding"]["stdout"], if cfg!(windows) { "gbk" } else { "utf-8-lossy" }, "{v}");
        #[cfg(windows)]
        {
            let v = f.run("$value = '中文$literal'; Write-Output \"$value`\"quote\"\nWrite-Output 'next'").await?;
            assert_eq!(v["exit_code"], 0, "{v}");
            assert_eq!(v["stdout"], "中文$literal\"quote\nnext\n");
        }
        Ok(())
    }

    /// 2026-11-13: `rg -n 'anthropic' --stats -l` with no path searched its
    /// open stdin pipe instead of the directory and never returned.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn a_command_gets_no_standard_input_unless_it_asks_for_it() -> Result<()> {
        let f = Fixture::new()?;
        // cat with no file reads stdin: it meets end-of-input and exits.
        let v = f.call("exec_command", json!({"cmd":"cat; echo done","yield_time_ms":10_000})).await?;
        assert_eq!((v["running"].clone(), v["exit_code"].clone(), v["stdout"].clone()), (json!(false), json!(0), json!("done\n")), "{v}");
        // A command still running without stdin says how to be sent input.
        let v = f.call("exec_command", json!({"cmd":"sleep 30","yield_time_ms":20})).await?;
        assert_eq!(v["running"], true, "{v}");
        let id = v["session_id"].as_str().unwrap();
        let refused = f.call("write_stdin", json!({"session_id":id,"chars":"hello\n"})).await.unwrap_err().to_string();
        assert!(refused.contains("SHELL_STDIN_CLOSED") && refused.contains("stdin:true"), "{refused}");
        let v = f.call("write_stdin", json!({"session_id":id,"yield_time_ms":20})).await?;
        assert_eq!(v["running"], true, "polling for output still works: {v}");
        f.call("kill_session", json!({"session_id":id})).await?;
        Ok(())
    }

    #[tokio::test]
    async fn sessions_poll_stdin_kill_tree_cancel_and_enforce_ownership() -> Result<()> {
        let f = Fixture::new()?;
        let read = if cfg!(windows) { "$line = [Console]::ReadLine(); Write-Output \"received:$line\"" } else { "read -r line; echo received:$line" };
        let v = f.call("exec_command", json!({"cmd":read,"yield_time_ms":20,"stdin":true})).await?;
        assert_eq!(v["running"], true, "{v}");
        let id = v["session_id"].as_str().unwrap();
        let foreign = execute(&f.state, &f.state.workspace, "foreign-task", 1, None, "write_stdin", &json!({"session_id":id}), &f.cancel, false).await.unwrap_err();
        assert!(foreign.to_string().contains("SCOPE_DENIED"));
        assert!(execute(&f.state, &f.state.workspace, "foreign-task", 1, None, "exec_command", &json!({"cmd":"echo should-not-run"}), &f.cancel, false).await.unwrap_err().to_string().contains("SHELL_BUSY"));
        let v = f.run(if cfg!(windows) { "Write-Output parallel" } else { "echo parallel" }).await?;
        assert_eq!(v["change_interval"]["merged"], true);
        assert_eq!(v["changes_pending"], true);
        let v = f.call("write_stdin", json!({"session_id":id,"chars":"hello\n","yield_time_ms":10_000})).await?;
        assert_eq!(v["running"], false, "{v}");
        assert_eq!(v["stdout"], "received:hello\n");

        let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        std::fs::write(f.root.join("server.cjs"), format!("require('http').createServer((q,s)=>s.end('ok')).listen({port},'127.0.0.1',()=>console.log('ready'));"))?;
        std::fs::write(f.root.join("package.json"), r#"{"private":true,"scripts":{"dev":"node server.cjs"}}"#)?;
        let v = f.call("exec_command", json!({"cmd":"npm run dev","yield_time_ms":300})).await?;
        let id = v["session_id"].as_str().unwrap();
        let mut ready = v["stdout"].as_str().unwrap().contains("ready");
        for _ in 0..30 {
            if ready { break; }
            let v = f.call("write_stdin", json!({"session_id":id,"yield_time_ms":100})).await?;
            ready |= v["stdout"].as_str().unwrap().contains("ready");
        }
        assert!(ready);
        assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_err());
        let curl = if cfg!(windows) { "curl.exe" } else { "curl" };
        let probe = f.run(&format!("{curl} --fail --silent http://127.0.0.1:{port}/")).await?;
        assert_eq!(probe["exit_code"], 0, "{probe}");
        assert_eq!(probe["stdout"], "ok");
        assert_eq!(probe["change_interval"]["merged"], true);
        assert_eq!(probe["changes_pending"], true);
        let v = f.call("kill_session", json!({"session_id":id})).await?;
        assert_eq!(v["running"], false, "{v}");
        assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());

        let v = f.call("exec_command", json!({"cmd":"npm run dev","yield_time_ms":200})).await?;
        let id = v["session_id"].as_str().unwrap();
        f.cancel.cancel();
        let session = owned(&f.root, "shell-test", id).await?;
        wait_finished(&session, Duration::from_secs(10)).await;
        assert!(session.finished.load(Ordering::Acquire));
        assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn large_output_is_saved_and_can_be_paged_from_a_module() -> Result<()> {
        let f = Fixture::new()?;
        let v = f.run("node -e 'process.stdout.write(\"BEGIN\"+\"x\".repeat(5*1024*1024)+\"END\");process.stderr.write(\"error-stream\")'").await?;
        assert_eq!(v["running"], false, "{v}");
        assert_eq!(v["exit_code"], 0);
        assert_eq!(v["truncated"], true);
        let preview = v["stdout"].as_str().unwrap();
        assert!(preview.starts_with("BEGIN") && preview.ends_with("END"));
        assert_eq!(v["stderr"], "error-stream");
        let path = f.root.join(v["output_asset"]["snapshot_path"].as_str().context("missing full asset")?);
        let full: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        assert_eq!(full["stdout"].as_str().unwrap().len(), 5*1024*1024+8);
        let args = json!({"session_id":v["session_id"],"stream":"stdout","char_offset":5*1024*1024+5,"yield_time_ms":0});
        let page = execute(&f.state,&f.state.workspace,"shell-test",1,Some("module-reader"),"write_stdin",&args,&f.cancel,false).await?;
        assert_eq!(page["output"], "END");
        assert!(page["next_char_offset"].is_null());
        Ok(())
    }

    #[tokio::test]
    async fn program_delivery_preserves_parseable_json_and_bounds_only_bytes() -> Result<()> {
        let f = Fixture::new()?;
        let args = json!({"cmd":"node -e 'process.stdout.write(JSON.stringify({payload:\"x\".repeat(1000000),n:7}))'",
            "yield_time_ms":10_000,"max_output_tokens":128});
        let value = execute_program(&f.state,&f.state.workspace,"shell-test",1,None,"exec_command",&args,&f.cancel,false).await?;
        assert_eq!(value["truncated"],false,"programs do not receive AI-sized previews");
        let parsed:Value = serde_json::from_str(value["stdout"].as_str().unwrap())?;
        assert_eq!(parsed["payload"].as_str().unwrap().len(),1_000_000);
        assert_eq!(parsed["n"],7);
        assert_eq!(value["delivery"],"program");
        let args = json!({"cmd":"node -e 'process.stdout.write(\"x\".repeat(9*1024*1024)+\"END\")'","yield_time_ms":10_000});
        let error = execute_program(&f.state,&f.state.workspace,"shell-test",1,None,"exec_command",&args,&f.cancel,false).await.unwrap_err();
        let value = &error.downcast_ref::<OutputCapacityError>().context("expected structured capacity error")?.0;
        assert_eq!(value["code"],"SHELL_OUTPUT_OVER_CAPACITY");
        assert_eq!(value["delivered"],false);
        assert!(value.get("stdout").is_none(),"no prefix masquerades as a successful delivery");
        assert!(value["output_asset"].is_object());
        let full:Value=serde_json::from_slice(&std::fs::read(f.root.join(value["output_asset"]["snapshot_path"].as_str().unwrap()))?)?;
        assert_eq!(full["stdout"].as_str().unwrap().len(),9*1024*1024+3);
        let session=owned(&f.root,"shell-test",value["session_id"].as_str().unwrap()).await?;
        assert_eq!(session.cursor.lock().await.stdout,0,"failed delivery must not consume output");
        let page = execute_program(&f.state,&f.state.workspace,"shell-test",1,None,"write_stdin",
            &json!({"session_id":value["session_id"],"stream":"stdout","char_offset":9*1024*1024,"yield_time_ms":0}),&f.cancel,false).await?;
        assert_eq!(page["output"],"END");
        assert_eq!(page["total_chars"],9*1024*1024+3);
        assert_eq!(session.cursor.lock().await.stdout,0,"explicit pages do not consume the normal cursor");
        let again=execute_program(&f.state,&f.state.workspace,"shell-test",1,None,"write_stdin",
            &json!({"session_id":value["session_id"],"yield_time_ms":0}),&f.cancel,false).await.unwrap_err();
        assert_eq!(again.downcast_ref::<OutputCapacityError>().unwrap().0["code"],"SHELL_OUTPUT_OVER_CAPACITY");
        Ok(())
    }

    #[tokio::test]
    async fn dropping_a_start_request_releases_its_process_tree_and_interval() -> Result<()> {
        let f = Fixture::new()?;
        for delay in [0,5,40,100] {
            let state = f.state.clone();
            let cancel = f.cancel.clone();
            let run = tokio::spawn(async move {
                execute(&state,&state.workspace,"shell-test",1,None,"exec_command",
                    &json!({"cmd":if cfg!(windows) {"Start-Sleep -Seconds 120"} else {"sleep 120"},"yield_time_ms":10_000}),&cancel,false).await
            });
            tokio::time::sleep(Duration::from_millis(delay)).await;
            run.abort();
            let _ = run.await;
            let slot = interval_slot(&f.root.canonicalize()?);
            tokio::time::timeout(Duration::from_secs(10),async {
                loop {
                    if slot.lock().await.is_none() && active(&f.root).await.is_none() { break; }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await?;
        }
        let v = f.run("echo recovered").await?;
        assert_eq!(v["exit_code"],0,"{v}");
        Ok(())
    }

    #[tokio::test]
    async fn final_output_is_archived_without_a_final_poll() -> Result<()> {
        let f = Fixture::new()?;
        let value = f.call("exec_command",json!({"cmd":"node -e 'setTimeout(()=>process.stdout.write(\"LATE\"+\"x\".repeat(100000)),200)'","yield_time_ms":0})).await?;
        assert_eq!(value["running"],true);
        let s = owned(&f.root,"shell-test",value["session_id"].as_str().unwrap()).await?;
        wait_finished(&s,Duration::from_secs(10)).await;
        assert!(s.finished.load(Ordering::Acquire));
        let archive = s.archive.lock().await;
        let (_,_,ended,reference) = archive.as_ref().context("no final asset was published")?;
        assert!(*ended);
        let full:Value = serde_json::from_slice(&std::fs::read(f.root.join(reference["snapshot_path"].as_str().unwrap()))?)?;
        assert_eq!(full["stdout"].as_str().unwrap().len(),100004);
        assert!(full["stdout"].as_str().unwrap().starts_with("LATE"));
        let conn = crate::agent_service::open_db(&f.root)?;
        let recorded:String = conn.query_row("SELECT json_extract(data,'$.output_asset.asset_id') FROM agent_task_events WHERE task_id='shell-test' AND kind='shell/end'",[],|row|row.get(0))?;
        assert_eq!(recorded,reference["asset_id"].as_str().unwrap());
        Ok(())
    }

    #[tokio::test]
    async fn sed_edits_are_tracked_and_undoable_and_program_failure_stops_shell() -> Result<()> {
        let f = Fixture::new()?;
        std::fs::write(f.root.join("sample.txt"), "before\n")?;
        let v = f.run(if cfg!(windows) { "(Get-Content -Raw sample.txt).Replace('before','after') | Set-Content -NoNewline sample.txt" } else { "sed -i 's/before/after/' sample.txt" }).await?;
        assert_eq!(v["exit_code"], 0, "{v}");
        assert_eq!(std::fs::read_to_string(f.root.join("sample.txt"))?, "after\n");
        let summary = crate::workspace_changes::summary(&f.root,"shell-test",1)?;
        assert_eq!(summary["total"], 1, "{summary}");
        assert_eq!(summary["undo_available"], true);
        let conn = crate::agent_service::open_db(&f.root)?;
        conn.execute("UPDATE agent_tasks SET status='completed' WHERE id='shell-test'", [])?;
        crate::workspace_changes::undo(&f.root,"shell-test",1)?;
        conn.execute("UPDATE agent_tasks SET status='running' WHERE id='shell-test'", [])?;
        assert_eq!(std::fs::read_to_string(f.root.join("sample.txt"))?, "before\n");
        let v = f.call("exec_command",json!({"cmd":if cfg!(windows) {"[Console]::ReadLine()"} else {"read -r line"},"yield_time_ms":20,"stdin":true})).await?;
        cleanup_frame(&f.root,"shell-test","main").await;
        let s = owned(&f.root,"shell-test",v["session_id"].as_str().unwrap()).await?;
        assert!(s.finished.load(Ordering::Acquire));
        let v = f.run("echo recovered").await?;
        assert_eq!(v["exit_code"], 0, "{v}");
        Ok(())
    }
}
