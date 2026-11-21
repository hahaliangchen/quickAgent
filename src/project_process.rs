//! Structured npm operations and host-owned processes. No terminal UI or shell command construction.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::{Mutex, Notify, mpsc},
};

pub const TOOLS: &[&str] = &[
    "install_dependencies",
    "run_project_script",
    "get_project_process",
    "stop_project_process",
];
pub const PROCESS_OBSERVATION_TTL_MS: u64 = 30_000;
static PROCESS_EVENT_GENERATION: AtomicU64 = AtomicU64::new(1);
static HOST_INSTANCE_ID: OnceLock<String> = OnceLock::new();

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessObservationCoverage {
    WorkspaceList,
    #[default]
    ProjectList,
    SingleProcess,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ProcessObservationScope {
    pub coverage: ProcessObservationCoverage,
    pub project_path: Option<String>,
    pub script: Option<String>,
    pub ready_url: Option<String>,
    pub ready_port: Option<u16>,
    pub process_id: Option<String>,
    pub operation: Option<String>,
    pub script_args: Vec<String>,
}

pub(crate) fn host_instance_id() -> &'static str {
    HOST_INSTANCE_ID.get_or_init(|| format!("host_{}_{}", std::process::id(), now()))
}

pub(crate) fn process_event_generation() -> u64 {
    PROCESS_EVENT_GENERATION.load(Ordering::Relaxed)
}

fn bump_process_event_generation() -> u64 {
    PROCESS_EVENT_GENERATION
        .fetch_add(1, Ordering::Relaxed)
        .saturating_add(1)
}

fn scope_from_args(root: &Path, args: &Value) -> Result<ProcessObservationScope> {
    let (ready_url, ready_port) = probe_target(args)?;
    let explicit_coverage = args.get("coverage").and_then(Value::as_str);
    let coverage = match explicit_coverage {
        Some("workspace_list") => ProcessObservationCoverage::WorkspaceList,
        Some("project_list") => ProcessObservationCoverage::ProjectList,
        Some("single_process") => ProcessObservationCoverage::SingleProcess,
        Some(other) => anyhow::bail!("unknown process observation coverage '{other}'"),
        None if args["process_id"].as_str().is_some() => ProcessObservationCoverage::SingleProcess,
        None if args.get("project_path").and_then(Value::as_str).is_some() => {
            ProcessObservationCoverage::ProjectList
        }
        None => ProcessObservationCoverage::WorkspaceList,
    };
    ensure!(
        coverage != ProcessObservationCoverage::WorkspaceList
            || args.get("project_path").is_none_or(Value::is_null),
        "workspace_list observations cannot be restricted to project_path"
    );
    ensure!(
        coverage != ProcessObservationCoverage::SingleProcess
            || args["process_id"].as_str().is_some(),
        "single_process observations require process_id"
    );
    let project_path = match coverage {
        ProcessObservationCoverage::WorkspaceList => None,
        ProcessObservationCoverage::ProjectList | ProcessObservationCoverage::SingleProcess => {
            let (_, relative) = project(root, args["project_path"].as_str().unwrap_or("."))?;
            Some(relative)
        }
    };
    Ok(ProcessObservationScope {
        coverage,
        project_path,
        script: args
            .get("script")
            .and_then(Value::as_str)
            .map(str::to_owned),
        ready_url,
        ready_port,
        process_id: args
            .get("process_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        operation: args
            .get("operation")
            .and_then(Value::as_str)
            .map(str::to_owned),
        script_args: args
            .get("script_args")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn project_input_version(root: &Path, scope: &ProcessObservationScope) -> Result<String> {
    let canonical_root = root
        .canonicalize()
        .context("workspace root is unavailable to process observations")?;
    let mut digest = Sha256::new();
    digest.update(canonical_root.to_string_lossy().as_bytes());
    digest.update([0]);
    digest.update(serde_json::to_vec(scope)?);
    if let Some(project_path) = scope.project_path.as_deref() {
        let (dir, _) = project(&canonical_root, project_path)
            .context("could not resolve project observation scope")?;
        for name in ["package.json", "package-lock.json", "npm-shrinkwrap.json"] {
            digest.update(name.as_bytes());
            digest.update([0]);
            match std::fs::read(dir.join(name)) {
                Ok(bytes) => digest.update(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    digest.update(b"<missing>")
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("could not fingerprint {name}"));
                }
            }
            digest.update([0]);
        }
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

#[cfg(test)]
pub fn observation_is_fresh(root: &Path, args: &Value, sample: &Value) -> bool {
    if !can_reuse_process_observation(args) {
        return false;
    }
    let Ok(expected_scope) = scope_from_args(root, args) else {
        return false;
    };
    let Ok(scope) = serde_json::from_value::<ProcessObservationScope>(sample["scope"].clone())
    else {
        return false;
    };
    scope == expected_scope && observation_sample_is_current(root, sample)
}

#[cfg(test)]
pub fn observation_sample_is_current(root: &Path, sample: &Value) -> bool {
    let Ok(scope) = serde_json::from_value::<ProcessObservationScope>(sample["scope"].clone())
    else {
        return false;
    };
    let Ok(version) = project_input_version(root, &scope) else {
        return false;
    };
    let now = now();
    sample["input_version"] == version
        && sample["host_instance_id"] == host_instance_id()
        && sample["process_event_generation"].as_u64() == Some(process_event_generation())
        && sample["sampled_at"]
            .as_u64()
            .is_some_and(|sampled| now.saturating_sub(sampled) <= PROCESS_OBSERVATION_TTL_MS)
}

#[cfg(test)]
pub fn can_reuse_process_observation(args: &Value) -> bool {
    args["process_id"].as_str().is_none()
        && args["force_refresh"] != true
        && args["wait_seconds"].as_u64().unwrap_or(0) == 0
        && args.get("ready_url").is_none_or(Value::is_null)
        && args.get("ready_port").is_none_or(Value::is_null)
}

fn observation_metadata(root: &Path, scope: &ProcessObservationScope) -> Result<Value> {
    Ok(json!({
        "scope":scope,
        "input_version":project_input_version(root, &scope)?,
        "sampled_at":now(),
        "expires_at":now().saturating_add(PROCESS_OBSERVATION_TTL_MS),
        "host_instance_id":host_instance_id(),
        "process_event_generation":process_event_generation(),
        "ttl_ms":PROCESS_OBSERVATION_TTL_MS
    }))
}

pub fn build_process_observation(root: &Path, args: &Value, processes: &[Value]) -> Result<Value> {
    let scope = scope_from_args(root, args)?;
    build_observation(root, scope, processes)
}

fn build_single_process_observation(
    root: &Path,
    args: &Value,
    processes: &[Value],
) -> Result<Value> {
    let fact = processes.first().cloned().unwrap_or(Value::Null);
    let process_id = fact["process_id"]
        .as_str()
        .or_else(|| args["process_id"].as_str())
        .context("single-process observations require a process_id")?
        .to_owned();
    let project_path = fact["project_path"]
        .as_str()
        .or_else(|| args["project_path"].as_str())
        .unwrap_or(".");
    let (_, project_path) = project(root, project_path)?;
    let (fallback_url, fallback_port) = probe_target(args)?;
    let scope = ProcessObservationScope {
        coverage: ProcessObservationCoverage::SingleProcess,
        project_path: Some(project_path),
        script: fact["script"]
            .as_str()
            .or_else(|| args["script"].as_str())
            .map(str::to_owned),
        ready_url: fact["ready_url"]
            .as_str()
            .map(str::to_owned)
            .or(fallback_url),
        ready_port: fact["ready_port"]
            .as_u64()
            .map(|port| port as u16)
            .or(fallback_port),
        process_id: Some(process_id),
        operation: fact["operation"]
            .as_str()
            .or_else(|| args["operation"].as_str())
            .map(str::to_owned),
        script_args: fact["args"]
            .as_array()
            .or_else(|| args["script_args"].as_array())
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
    };
    build_observation(root, scope, processes)
}

fn build_observation(
    root: &Path,
    scope: ProcessObservationScope,
    processes: &[Value],
) -> Result<Value> {
    let mut metadata = observation_metadata(root, &scope)?;
    metadata["processes"] = json!(processes);
    Ok(metadata)
}

fn process_fact(value: &Value) -> Value {
    json!({"process_id":value["process_id"],"project_path":value["project_path"],"script":value["script"],
        "args":value["args"],"operation":value["operation"],"state":value["state"],"running":value["running"],"ready":value["ready"],
        "listener_owned":value["listener_owned"],"page_ready":value["page_ready"],"ready_url":value["ready_url"],
        "ready_port":value["ready_port"],"exit_code":value["exit_code"]})
}

pub fn is_tool(name: &str) -> bool {
    TOOLS.contains(&name)
}
pub fn is_execution(name: &str) -> bool {
    matches!(
        name,
        "run_program" | "install_dependencies" | "run_project_script" | "stop_project_process"
            | "exec_command" | "write_stdin" | "kill_session"
    )
}
pub fn may_change_files(name: &str) -> bool {
    matches!(
        name,
        "run_program" | "install_dependencies" | "run_project_script" | "exec_command" | "write_stdin"
    )
}
pub fn check_key(name: &str, args: &Value) -> String {
    if name == "http_probe" {
        return crate::http_probe::check_key(args);
    }
    if name == "run_program" {
        return crate::program_execution::check_key(args);
    }
    let project = args["project_path"]
        .as_str()
        .unwrap_or(".")
        .replace('\\', "/");
    if name == "install_dependencies" {
        return format!("npm-install:{project}");
    }
    let prefix = if args["background"] == true {
        "npm-start"
    } else {
        "npm"
    };
    let mut key = format!(
        "{prefix}:{}:{}",
        project,
        args["script"].as_str().unwrap_or("")
    );
    if args["args"]
        .as_array()
        .is_some_and(|items| !items.is_empty())
    {
        key.push(':');
        key.push_str(&args["args"].to_string());
    }
    key
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Clone, Serialize, Deserialize)]
struct Meta {
    process_id: String,
    pid: u32,
    root: PathBuf,
    project_path: String,
    script: String,
    args: Vec<String>,
    operation: String,
    check_key: String,
    running: bool,
    ready: bool,
    #[serde(default)]
    listener_owned: bool,
    #[serde(default)]
    page_ready: bool,
    #[serde(default)]
    readiness_detail: String,
    #[serde(default)]
    script_command: String,
    #[serde(default)]
    node_executable: String,
    #[serde(default)]
    npm_cli: String,
    ready_url: Option<String>,
    ready_port: Option<u16>,
    started_at: u64,
    ended_at: Option<u64>,
    exit_code: Option<i32>,
    termination_reason: Option<String>,
    background: bool,
    #[serde(default)]
    sandboxed: bool,
}
#[derive(Default)]
struct Logs {
    chunks: VecDeque<Value>,
    sequence: u64,
    chars: usize,
    disk_bytes: usize,
    truncated: bool,
    error: Option<String>,
}
struct Record {
    meta: Mutex<Meta>,
    logs: Mutex<Logs>,
    persist_guard: Mutex<()>,
    dir: PathBuf,
    stop: mpsc::UnboundedSender<String>,
    ended: Notify,
}

/// Shell sessions share the process panel and durable logs with project scripts.
pub(crate) struct ShellRecord(Arc<Record>);
impl ShellRecord {
    pub(crate) async fn register(root: &Path, cwd: &Path, id: &str, pid: u32, command: &str,
        sandboxed: bool, stop: mpsc::UnboundedSender<String>) -> Result<Self> {
        let root = root.canonicalize()?;
        let dir = root.join(".codex-workspace-mcp/project-processes");
        tokio::fs::create_dir_all(&dir).await?;
        let record = Arc::new(Record {
            meta: Mutex::new(Meta {
                process_id:id.into(), pid, root:root.clone(),
                project_path:cwd.strip_prefix(&root).unwrap_or(cwd).to_string_lossy().replace('\\',"/"),
                script:command.into(), args:vec![], operation:"shell".into(), check_key:id.into(),
                running:true, ready:false, listener_owned:false, page_ready:false,
                readiness_detail:String::new(), script_command:command.into(),
                node_executable:String::new(), npm_cli:String::new(), ready_url:None, ready_port:None,
                started_at:now(), ended_at:None, exit_code:None, termination_reason:None,
                background:true, sandboxed,
            }),
            logs:Mutex::new(Logs::default()), persist_guard:Mutex::new(()),
            dir, stop, ended:Notify::new(),
        });
        registry().lock().await.records.insert(id.into(), record.clone());
        bump_process_event_generation();
        record.persist().await;
        Ok(Self(record))
    }
    pub(crate) async fn push(&self, stream: &str, text: String) {
        self.0.push(stream, text).await;
    }
    pub(crate) async fn finish(&self, exit_code: Option<i32>, reason: Option<String>) {
        {
            let mut meta = self.0.meta.lock().await;
            meta.running = false;
            meta.exit_code = exit_code;
            meta.ended_at = Some(now());
            meta.termination_reason = reason;
        }
        bump_process_event_generation();
        self.0.persist().await;
        self.0.ended.notify_waiters();
    }
}
#[derive(Default)]
struct Registry {
    records: BTreeMap<String, Arc<Record>>,
}
static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
static SERIAL: AtomicU64 = AtomicU64::new(1);
fn registry() -> &'static Mutex<Registry> {
    REGISTRY.get_or_init(Default::default)
}

// The guard kills foreground operations when their requesting task is cancelled.
// Successful background launches deliberately transfer ownership to the host.
struct RequestLease(Option<mpsc::UnboundedSender<String>>);
impl Drop for RequestLease {
    fn drop(&mut self) {
        if let Some(stop) = self.0.take() {
            let _ = stop.send("request_cancelled".into());
        }
    }
}

impl Record {
    async fn persist(&self) {
        let _guard = self.persist_guard.lock().await;
        let meta = self.meta.lock().await.clone();
        if let Ok(bytes) = serde_json::to_vec(&meta) {
            if let Err(error) =
                tokio::fs::write(self.dir.join(format!("{}.json", meta.process_id)), bytes).await
            {
                tracing::warn!(%error,"could not persist project process metadata");
            }
        }
    }
    async fn push(&self, stream: &str, text: String) {
        if text.is_empty() {
            return;
        }
        let mut logs = self.logs.lock().await;
        logs.sequence += 1;
        let seq = logs.sequence;
        logs.chars += text.chars().count();
        logs.chunks
            .push_back(json!({"seq":seq,"stream":stream,"text":text,"time":now()}));
        while logs.chars > 64_000 {
            if let Some(old) = logs.chunks.pop_front() {
                logs.chars = logs
                    .chars
                    .saturating_sub(old["text"].as_str().unwrap_or("").chars().count());
            } else {
                break;
            }
        }
        if logs.disk_bytes < 8 * 1024 * 1024 {
            use tokio::io::AsyncWriteExt;
            let id = self.meta.lock().await.process_id.clone();
            let chunk = format!("[{stream}] {text}");
            let written = async {
                let mut file = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.dir.join(format!("{id}.log")))
                    .await?;
                file.write_all(chunk.as_bytes()).await
            }
            .await;
            match written {
                Ok(()) => logs.disk_bytes += chunk.len(),
                Err(error) => logs.error = Some(error.to_string()),
            }
        } else {
            logs.truncated = true;
        }
    }
    async fn snapshot(&self, args: &Value) -> Value {
        let meta = self.meta.lock().await.clone();
        let logs = self.logs.lock().await;
        let limit = args["max_chars"]
            .as_u64()
            .unwrap_or(12_000)
            .clamp(4096, 60_000) as usize;
        let after = args["after_seq"].as_u64();
        let cursor_reset = logs
            .chunks
            .front()
            .is_some_and(|chunk| chunk["cursor_reset"] == true);
        let start = if cursor_reset && after.is_some_and(|seq| seq > logs.sequence) {
            0
        } else {
            after.unwrap_or_else(|| logs.sequence.saturating_sub(8))
        };
        let mut selected = Vec::new();
        let mut count = 0;
        let mut next = start.min(logs.sequence);
        for chunk in logs
            .chunks
            .iter()
            .filter(|chunk| chunk["seq"].as_u64().unwrap_or(0) > start)
        {
            let chars = chunk["text"].as_str().unwrap_or("").chars().count();
            if count + chars > limit && !selected.is_empty() {
                break;
            }
            count += chars;
            next = chunk["seq"].as_u64().unwrap_or(next);
            selected.push(chunk.clone());
        }
        let stdout = selected
            .iter()
            .filter(|chunk| chunk["stream"] == "stdout")
            .filter_map(|chunk| chunk["text"].as_str())
            .collect::<String>();
        let recent_stderr = selected
            .iter()
            .filter(|chunk| chunk["stream"] == "stderr")
            .filter_map(|chunk| chunk["text"].as_str())
            .collect::<String>();
        let complete_stderr = logs
            .chunks
            .iter()
            .filter(|chunk| chunk["stream"] == "stderr")
            .filter_map(|chunk| chunk["text"].as_str())
            .collect::<String>();
        let stderr = if !meta.running && meta.exit_code.is_some_and(|code| code != 0) {
            complete_stderr
        } else {
            recent_stderr.clone()
        };
        json!({"process_id":meta.process_id,"pid":meta.pid,"project_path":meta.project_path,"script":meta.script,"args":meta.args,"operation":meta.operation,
            "sandbox": if meta.sandboxed { "workspace-write" } else { "none" },
            "state":if meta.running{"running"}else{"exited"},"running":meta.running,"process_running":meta.running,
            "ready":meta.running&&meta.ready,"listener_owned":meta.listener_owned,"page_ready":meta.page_ready,
            "readiness_detail":meta.readiness_detail,"script_command":meta.script_command,
            "host_runtime":{"node_executable":meta.node_executable,"npm_cli":meta.npm_cli,"host_pid":std::process::id(),"host_instance_id":host_instance_id()},
            "readiness":if !meta.running{"exited"}else if meta.ready{"ready"}else if meta.ready_url.is_none()&&meta.ready_port.is_none(){"not_configured"}else{"pending"},
            "ready_url":meta.ready_url,"ready_port":meta.ready_port,"status":meta.exit_code,"exit_code":meta.exit_code,
            "background":meta.background,"check_key":meta.check_key,"started_at":meta.started_at,"ended_at":meta.ended_at,"termination_reason":meta.termination_reason,
            "logs":selected,"stdout":stdout,"stderr":stderr,"stderr_recent":recent_stderr,"next_seq":next,"latest_seq":logs.sequence,
            "has_more":next<logs.sequence,"cursor_reset":cursor_reset,"log_gap":logs.chunks.front().is_some_and(|chunk|start.saturating_add(1)<chunk["seq"].as_u64().unwrap_or(0)),
            "log_file":self.dir.join(format!("{}.log",meta.process_id)),"disk_log_truncated":logs.truncated,"log_error":logs.error})
    }
    async fn wait(&self) {
        loop {
            let notified = self.ended.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.meta.lock().await.running {
                return;
            }
            notified.await;
        }
    }
}

async fn pump<R: tokio::io::AsyncRead + Unpin>(mut reader: R, record: Arc<Record>, stream: &str) {
    let mut buf = [0u8; 4096];
    let mut pending = Vec::new();
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => {
                if !pending.is_empty() {
                    record
                        .push(stream, String::from_utf8_lossy(&pending).into_owned())
                        .await;
                }
                return;
            }
            Ok(n) => pending.extend_from_slice(&buf[..n]),
            Err(error) => {
                record
                    .push("host", format!("log read failed: {error}\n"))
                    .await;
                return;
            }
        }
        let mut text = String::new();
        loop {
            match std::str::from_utf8(&pending) {
                Ok(valid) => {
                    text.push_str(valid);
                    pending.clear();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    text.push_str(std::str::from_utf8(&pending[..valid]).unwrap());
                    pending.drain(..valid);
                    if let Some(n) = error.error_len() {
                        text.push('\u{fffd}');
                        pending.drain(..n);
                    } else {
                        break;
                    }
                }
            }
        }
        record.push(stream, text).await;
    }
}

fn project(root: &Path, raw: &str) -> Result<(PathBuf, String)> {
    let root = root
        .canonicalize()
        .context("workspace root is unavailable to process tools")?;
    let trimmed = raw.trim();
    let raw_path = Path::new(trimmed);
    if raw_path.is_absolute()
        && !raw_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        && crate::process_sandbox::allows_external_dir(&root, raw_path)
    {
        let path = raw_path
            .canonicalize()
            .context("project directory is unavailable to process tools")?;
        ensure!(path.is_dir(), "project_path is not a directory");
        if let Ok(rel) = path.strip_prefix(&root) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            return Ok((path, if rel.is_empty() { ".".into() } else { rel }));
        }
        let display = path.to_string_lossy().replace('\\', "/");
        return Ok((path, display));
    }
    let checked = crate::file_edit::workspace_path(&root, raw)
        .context("project path failed workspace boundary validation")?;
    let path = checked
        .canonicalize()
        .context("project directory is unavailable to process tools")?;
    ensure!(path.is_dir(), "project_path is not a directory");
    let rel = path
        .strip_prefix(&root)?
        .to_string_lossy()
        .replace('\\', "/");
    Ok((path, if rel.is_empty() { ".".into() } else { rel }))
}

#[cfg(any(windows, test))]
fn normalize_windows_runtime_path(raw: &str) -> Result<String> {
    if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        ensure!(
            unc.contains('\\') && !unc.starts_with('\\'),
            "runtime_bootstrap: malformed extended UNC path cannot be passed to Node/npm"
        );
        return Ok(format!(r"\\{unc}"));
    }
    if let Some(path) = raw.strip_prefix(r"\\?\") {
        let bytes = path.as_bytes();
        ensure!(
            bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'\\' | b'/'),
            "runtime_bootstrap: unsupported Windows device path cannot be safely converted for Node/npm"
        );
        return Ok(path.to_owned());
    }
    ensure!(
        !raw.starts_with(r"\\.\") && !raw.starts_with(r"\??\"),
        "runtime_bootstrap: unsupported Windows device path cannot be safely converted for Node/npm"
    );
    Ok(raw.to_owned())
}

#[cfg(windows)]
fn external_runtime_path(path: &Path) -> Result<OsString> {
    let raw = path.as_os_str().to_string_lossy();
    let normalized = normalize_windows_runtime_path(&raw)?;
    // An ordinary path that is too long for the Win32 API cannot be made safe
    // by stripping the extended prefix; stop before invoking the runtime.
    ensure!(
        normalized.encode_utf16().count() < 32_760,
        "runtime_bootstrap: converted Windows path exceeds the runtime path limit"
    );
    Ok(OsString::from(normalized))
}
#[cfg(not(windows))]
fn external_runtime_path(path: &Path) -> Result<OsString> {
    Ok(path.as_os_str().to_owned())
}

fn node_executable() -> Result<PathBuf> {
    let paths = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    let executable = if cfg!(windows) { "node.exe" } else { "node" };
    paths
        .iter()
        .map(|dir| dir.join(executable))
        .find(|path| path.is_file())
        .context("Node.js is not installed or not visible in the host PATH")
}

fn node_npm() -> Result<(PathBuf, PathBuf)> {
    let paths = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    let node = node_executable()?;
    let mut directories = vec![node.parent().unwrap().to_path_buf()];
    if let Ok(actual) = node.canonicalize() {
        if let Some(parent) = actual.parent() {
            directories.push(parent.to_path_buf());
        }
    }
    directories.extend(paths);
    if let Some(appdata) = std::env::var_os("APPDATA") {
        directories.push(PathBuf::from(appdata).join("npm"));
    }
    for dir in directories {
        for relative in [
            "node_modules/npm/bin/npm-cli.js",
            "../lib/node_modules/npm/bin/npm-cli.js",
            "../share/nodejs/npm/bin/npm-cli.js",
        ] {
            let cli = dir.join(relative);
            if cli.is_file() {
                return Ok((node, cli.canonicalize()?));
            }
        }
    }
    anyhow::bail!(
        "npm-cli.js was not found beside Node.js or in PATH; install/configure npm in the host environment"
    )
}

fn script_invokes_powershell(script: &str) -> bool {
    script
        .split(|character: char| {
            !character.is_ascii_alphanumeric() && !matches!(character, '_' | '.' | '-')
        })
        .any(|token| {
            let token = token.to_ascii_lowercase();
            matches!(
                token.as_str(),
                "powershell"
                    | "powershell.exe"
                    | "powershell_ise"
                    | "powershell_ise.exe"
                    | "pwsh"
                    | "pwsh.exe"
            ) || token.ends_with(".ps1")
        })
}

fn powershell_script_shell(configured: &str) -> bool {
    let configured = configured.trim();
    let token = match configured.chars().next() {
        Some(quote @ ('\'' | '"')) => configured[quote.len_utf8()..]
            .split_once(quote)
            .map_or(configured, |(path, _)| path),
        Some(_) => configured.split_whitespace().next().unwrap_or(configured),
        None => return false,
    };
    let executable = token.rsplit(['\\', '/']).next().unwrap_or(token);
    script_invokes_powershell(executable) || script_invokes_powershell(configured)
}

/// Ask the same npm CLI to resolve project, user, and environment config before
/// spawning a package lifecycle process. This is a config read, not a shell call.
async fn effective_npm_script_shell(
    node: &Path,
    npm: &Path,
    dir: &Path,
    cancel: &tokio_util::sync::CancellationToken,
    root: &Path,
    boundary: crate::process_sandbox::ExecutionBoundary,
) -> Result<Option<String>> {
    let mut command = crate::process_sandbox::command(external_runtime_path(node)?, root, dir, boundary)?;
    command
        .arg(external_runtime_path(npm)?)
        .args(["config", "get", "script-shell"])
        .current_dir(external_runtime_path(dir)?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::select! {
        _ = cancel.cancelled() => anyhow::bail!("project execution cancelled before checking npm script-shell"),
        result = tokio::time::timeout(Duration::from_secs(15), command.output()) =>
            result.context("runtime_bootstrap: timed out reading effective npm script-shell")?
                .context("runtime_bootstrap: could not read effective npm script-shell")?,
    };
    ensure!(
        output.status.success(),
        "{}runtime_bootstrap: could not verify effective npm script-shell; refusing to start package scripts ({} {})",
        if boundary != crate::process_sandbox::ExecutionBoundary::Host && String::from_utf8_lossy(&output.stderr).lines().any(|line| line.starts_with("bwrap:")) { "SANDBOX_UNAVAILABLE: " } else { "" },
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let configured = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let unwrapped = configured.trim_matches(['\'', '"']).trim();
    Ok((!configured.is_empty() && unwrapped != "null").then(|| configured.to_owned()))
}

pub(crate) fn resolve_node_executable() -> Result<PathBuf> {
    node_executable()
}

pub(crate) fn native_runtime_path(path: &Path) -> Result<OsString> {
    external_runtime_path(path)
}

fn runtime_bootstrap(error: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("runtime_bootstrap: {error}")
}
fn probe_target(args: &Value) -> Result<(Option<String>, Option<u16>)> {
    let url = args
        .get("ready_url")
        .filter(|v| !v.is_null())
        .map(|value| -> Result<String> {
            let raw = value.as_str().context("ready_url must be a URL")?;
            ensure!(raw.len() <= 2000, "ready_url is too long");
            let parsed = reqwest::Url::parse(raw)?;
            ensure!(
                matches!(parsed.scheme(), "http" | "https")
                    && parsed.username().is_empty()
                    && parsed.password().is_none(),
                "ready_url must be local HTTP(S) without credentials"
            );
            ensure!(
                matches!(
                    parsed.host_str(),
                    Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
                ),
                "ready_url must address localhost"
            );
            Ok(raw.to_owned())
        })
        .transpose()?;
    let port = args
        .get("ready_port")
        .filter(|v| !v.is_null())
        .map(|value| {
            value
                .as_u64()
                .filter(|port| *port > 0 && *port <= 65535)
                .map(|port| port as u16)
                .context("ready_port must be 1..65535")
        })
        .transpose()?;
    Ok((url, port))
}
async fn probe(record: &Record, args: &Value) -> Result<()> {
    let (url, port) = probe_target(args)?;
    let endpoint_changed = {
        let mut meta = record.meta.lock().await;
        if url.is_some() || port.is_some() {
            let changed = meta.ready_url != url || meta.ready_port != port || meta.ready;
            meta.ready_url = url;
            meta.ready_port = port;
            meta.ready = false;
            changed
        } else {
            false
        }
    };
    if endpoint_changed {
        bump_process_event_generation();
    }
    let deadline = tokio::time::Instant::now()
        + Duration::from_secs(args["wait_seconds"].as_u64().unwrap_or(0).min(60));
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(750))
        .build()?;
    loop {
        let meta = record.meta.lock().await.clone();
        if !meta.running {
            break;
        }
        let port = meta.ready_port.or_else(|| {
            meta.ready_url
                .as_ref()
                .and_then(|url| reqwest::Url::parse(url).ok())
                .and_then(|url| url.port_or_known_default())
        });
        let Some(port) = port else { break };
        let host = meta
            .ready_url
            .as_ref()
            .and_then(|url| reqwest::Url::parse(url).ok())
            .and_then(|url| {
                url.host_str()
                    .map(|host| host.trim_matches(['[', ']']).to_owned())
            })
            .unwrap_or_else(|| "127.0.0.1".into());
        let owner_host = host.clone();
        let ownership =
            tokio::task::spawn_blocking(move || listener_ownership(port, &owner_host, meta.pid))
                .await?;
        let page = if ownership.owned {
            if let Some(url) = meta.ready_url.clone() {
                page_status(&client, &url).await
            } else {
                let connected = tokio::time::timeout(
                    Duration::from_millis(750),
                    tokio::net::TcpStream::connect((host.as_str(), port)),
                )
                .await
                .is_ok_and(|result| result.is_ok());
                PageStatus {
                    ready: connected,
                    detail: if connected {
                        "TCP listener belongs to the managed process group and accepts connections"
                            .into()
                    } else {
                        format!("managed listener does not accept connections on {host}:{port}")
                    },
                }
            }
        } else {
            PageStatus {
                ready: false,
                detail: ownership.detail.clone(),
            }
        };
        let ready = ownership.owned && page.ready;
        let readiness_changed = {
            let mut state = record.meta.lock().await;
            let previous = (state.ready, state.listener_owned, state.page_ready);
            state.listener_owned = ownership.owned;
            state.page_ready = page.ready;
            state.readiness_detail = page.detail;
            state.ready = ready && state.running;
            previous != (state.ready, state.listener_owned, state.page_ready)
        };
        if readiness_changed {
            bump_process_event_generation();
        }
        if ready || tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    record.persist().await;
    Ok(())
}

struct ListenerOwnership {
    owned: bool,
    detail: String,
}
struct PageStatus {
    ready: bool,
    detail: String,
}

enum EndpointFilter {
    V4(u32),
    V6([u8; 16]),
    Localhost,
    Any,
}

impl EndpointFilter {
    fn parse(host: &str) -> Self {
        let host = host.trim().trim_matches(|ch| ch == '[' || ch == ']');
        if host.eq_ignore_ascii_case("localhost") {
            return Self::Localhost;
        }
        if let Ok(std::net::IpAddr::V4(addr)) = host.parse() {
            return Self::V4(u32::from(addr));
        }
        if let Ok(std::net::IpAddr::V6(addr)) = host.parse() {
            return Self::V6(addr.octets());
        }
        Self::Any
    }

    fn match_v4(&self, address: u32) -> BindMatch {
        let loopback = u32::from(std::net::Ipv4Addr::LOCALHOST);
        match self {
            Self::V4(expected) if address == *expected => BindMatch::Exact,
            Self::V4(_) if address == 0 => BindMatch::Wildcard,
            Self::Localhost if address == loopback => BindMatch::Exact,
            Self::Localhost if address == 0 => BindMatch::Wildcard,
            Self::Any => BindMatch::Exact,
            _ => BindMatch::None,
        }
    }

    fn match_v6(&self, address: &[u8; 16]) -> BindMatch {
        let unspecified = [0u8; 16];
        let mut loopback = [0u8; 16];
        loopback[15] = 1;
        // Node commonly listens on :: with dual-stack enabled. A subsequent
        // HTTP/TCP probe checks that the requested IPv4 address is reachable.
        if matches!(self, Self::V4(_) | Self::Localhost) {
            if address == &unspecified {
                return BindMatch::Wildcard;
            }
            if address[..12] == [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255] {
                return self.match_v4(u32::from_be_bytes(address[12..].try_into().unwrap()));
            }
        }
        match self {
            Self::V6(expected) if address == expected => BindMatch::Exact,
            Self::V6(_) if address == &unspecified => BindMatch::Wildcard,
            Self::Localhost if address == &loopback => BindMatch::Exact,
            Self::Localhost if address == &unspecified => BindMatch::Wildcard,
            Self::Any => BindMatch::Exact,
            _ => BindMatch::None,
        }
    }
}

enum BindMatch {
    Exact,
    Wildcard,
    None,
}

fn prefer_exact(exact: Vec<u32>, wildcard: Vec<u32>) -> Vec<u32> {
    let chosen = if exact.is_empty() { wildcard } else { exact };
    let mut chosen = chosen;
    chosen.sort_unstable();
    chosen.dedup();
    chosen
}

fn listener_ownership(port: u16, host: &str, root_pid: u32) -> ListenerOwnership {
    match listener_pids(port, host) {
        Err(error) => ListenerOwnership {
            owned: false,
            detail: format!("could not identify the process listening on {host}:{port}: {error}"),
        },
        Ok(pids) if pids.is_empty() => ListenerOwnership {
            owned: false,
            detail: format!("nothing is listening on {host}:{port}"),
        },
        Ok(pids) => {
            let descriptions = pids
                .iter()
                .map(|pid| format!("pid {pid} {}", process_name(*pid)))
                .collect::<Vec<_>>()
                .join("; ");
            let host_pid = std::process::id();
            if pids.iter().any(|pid| *pid == host_pid) {
                ListenerOwnership {
                    owned: false,
                    detail: format!(
                        "port {port} on {host} belongs to this workspace host ({descriptions}), not the target application"
                    ),
                }
            } else if root_pid != 0
                && pids
                    .iter()
                    .all(|pid| *pid == root_pid || process_is_descendant(*pid, root_pid))
            {
                ListenerOwnership {
                    owned: true,
                    detail: format!(
                        "listener on {host}:{port} belongs to managed pid {root_pid}: {descriptions}"
                    ),
                }
            } else {
                ListenerOwnership {
                    owned: false,
                    detail: format!(
                        "port {port} on {host} is owned by another process ({descriptions})"
                    ),
                }
            }
        }
    }
}

async fn page_status(client: &reqwest::Client, url: &str) -> PageStatus {
    match client.get(url).send().await {
        Err(error) => PageStatus {
            ready: false,
            detail: format!("page request failed: {error}"),
        },
        Ok(response) => {
            let status = response.status();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_owned();
            let body = response.text().await.unwrap_or_default();
            let excerpt: String = body.chars().take(400).collect();
            let lower = excerpt.to_lowercase();
            let broken = lower.contains("cannot get")
                || lower.contains("wasm") && (lower.contains("error") || lower.contains("failed"))
                || lower.contains("econnrefused");
            if !status.is_success() || broken {
                PageStatus {
                    ready: false,
                    detail: format!("HTTP {status} content-type {content_type}; body: {excerpt}"),
                }
            } else {
                PageStatus {
                    ready: true,
                    detail: format!("HTTP {status} content-type {content_type}"),
                }
            }
        }
    }
}

#[cfg(windows)]
fn listener_pids(port: u16, host: &str) -> Result<Vec<u32>> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
        MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};
    fn table<T>(family: u32) -> Result<Vec<u8>> {
        unsafe {
            let mut size = 0u32;
            let _ = GetExtendedTcpTable(
                std::ptr::null_mut(),
                &mut size,
                0,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            );
            let mut buffer = vec![0u8; size as usize];
            let status = GetExtendedTcpTable(
                buffer.as_mut_ptr().cast(),
                &mut size,
                0,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            );
            ensure!(status == 0, "GetExtendedTcpTable failed: {status}");
            Ok(buffer)
        }
    }
    let mut exact = Vec::new();
    let mut wildcard = Vec::new();
    let target = EndpointFilter::parse(host);
    let v4 = table::<()>(AF_INET as u32)?;
    if v4.len() >= std::mem::size_of::<u32>() {
        let count =
            usize::try_from(u32::from_ne_bytes(v4[..4].try_into().unwrap_or([0; 4]))).unwrap_or(0);
        let rows = unsafe {
            std::slice::from_raw_parts(
                v4.as_ptr().add(4) as *const MIB_TCPROW_OWNER_PID,
                count.min(4096),
            )
        };
        for row in rows {
            if u16::from_be(row.dwLocalPort as u16) != port {
                continue;
            }
            match target.match_v4(u32::from_be(row.dwLocalAddr)) {
                BindMatch::Exact => exact.push(row.dwOwningPid),
                BindMatch::Wildcard => wildcard.push(row.dwOwningPid),
                BindMatch::None => {}
            }
        }
    }
    let v6 = table::<()>(AF_INET6 as u32)?;
    if v6.len() >= std::mem::size_of::<u32>() {
        let count =
            usize::try_from(u32::from_ne_bytes(v6[..4].try_into().unwrap_or([0; 4]))).unwrap_or(0);
        let rows = unsafe {
            std::slice::from_raw_parts(
                v6.as_ptr().add(4) as *const MIB_TCP6ROW_OWNER_PID,
                count.min(4096),
            )
        };
        for row in rows {
            if u16::from_be(row.dwLocalPort as u16) != port {
                continue;
            }
            match target.match_v6(&row.ucLocalAddr) {
                BindMatch::Exact => exact.push(row.dwOwningPid),
                BindMatch::Wildcard => wildcard.push(row.dwOwningPid),
                BindMatch::None => {}
            }
        }
    }
    let _ = std::mem::size_of::<MIB_TCPTABLE_OWNER_PID>()
        + std::mem::size_of::<MIB_TCP6TABLE_OWNER_PID>();
    Ok(prefer_exact(exact, wildcard))
}
#[cfg(target_os = "linux")]
fn linux_tcp_listener(line: &str) -> Option<(std::net::IpAddr, u16, u64)> {
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.get(3)? != &"0A" {
        return None;
    }
    let (address, port) = fields.get(1)?.rsplit_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    // procfs encodes each address word in native byte order, including tcp6.
    let address = match address.len() {
        8 => std::net::IpAddr::V4(std::net::Ipv4Addr::from(
            u32::from_str_radix(address, 16).ok()?.to_ne_bytes(),
        )),
        32 => {
            let mut bytes = [0; 16];
            for (index, word) in address.as_bytes().chunks_exact(8).enumerate() {
                bytes[index * 4..index * 4 + 4].copy_from_slice(
                    &u32::from_str_radix(std::str::from_utf8(word).ok()?, 16)
                        .ok()?
                        .to_ne_bytes(),
                );
            }
            std::net::IpAddr::V6(std::net::Ipv6Addr::from(bytes))
        }
        _ => return None,
    };
    Some((address, port, fields.get(9)?.parse().ok()?))
}

#[cfg(target_os = "linux")]
fn listener_pids(port: u16, host: &str) -> Result<Vec<u32>> {
    let target = EndpointFilter::parse(host);
    let mut exact = Vec::new();
    let mut wildcard = Vec::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let text = match std::fs::read_to_string(table) {
            Ok(text) => text,
            Err(error)
                if table.ends_with("tcp6") && error.kind() == std::io::ErrorKind::NotFound =>
            {
                continue;
            }
            Err(error) => return Err(error).with_context(|| format!("could not read {table}")),
        };
        for line in text.lines().skip(1) {
            let Some((address, local_port, inode)) = linux_tcp_listener(line) else {
                continue;
            };
            if local_port != port {
                continue;
            }
            let matched = match address {
                std::net::IpAddr::V4(address) => target.match_v4(u32::from(address)),
                std::net::IpAddr::V6(address) => target.match_v6(&address.octets()),
            };
            match matched {
                BindMatch::Exact => exact.push(inode),
                BindMatch::Wildcard => wildcard.push(inode),
                BindMatch::None => {}
            }
        }
    }
    let inodes: std::collections::BTreeSet<_> = if exact.is_empty() { wildcard } else { exact }
        .into_iter()
        .collect();
    if inodes.is_empty() {
        return Ok(Vec::new());
    }
    let mut resolved = std::collections::BTreeSet::new();
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc")?.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(descriptors) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for descriptor in descriptors.flatten() {
            let Ok(link) = std::fs::read_link(descriptor.path()) else {
                continue;
            };
            let Some(inode) = link.to_str().and_then(|value| {
                value
                    .strip_prefix("socket:[")?
                    .strip_suffix(']')?
                    .parse::<u64>()
                    .ok()
            }) else {
                continue;
            };
            if inodes.contains(&inode) {
                resolved.insert(inode);
                pids.push(pid);
            }
        }
    }
    ensure!(
        resolved == inodes,
        "listener socket owners are unavailable in /proc (permission denied or process exited)"
    );
    Ok(prefer_exact(pids, Vec::new()))
}

// lsof's field output avoids dependence on localized column headings and
// preserves bracketed IPv6 addresses. Also compiled in tests on Linux.
#[cfg(any(target_os = "macos", test))]
fn lsof_listener_pids(text: &str, port: u16, host: &str) -> Vec<u32> {
    let target = EndpointFilter::parse(host);
    let mut pid = None;
    let mut exact = Vec::new();
    let mut wildcard = Vec::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse::<u32>().ok();
        }
        let Some((pid, name)) = pid.zip(line.strip_prefix('n')) else {
            continue;
        };
        let Some((address, local_port)) = name.rsplit_once(':') else {
            continue;
        };
        if local_port.parse::<u16>().ok() != Some(port) {
            continue;
        }
        let matched = if address == "*" {
            BindMatch::Wildcard
        } else if let Ok(address) = address.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
            match address {
                std::net::IpAddr::V4(address) => target.match_v4(u32::from(address)),
                std::net::IpAddr::V6(address) => target.match_v6(&address.octets()),
            }
        } else {
            BindMatch::None
        };
        match matched {
            BindMatch::Exact => exact.push(pid),
            BindMatch::Wildcard => wildcard.push(pid),
            BindMatch::None => {}
        }
    }
    prefer_exact(exact, wildcard)
}

#[cfg(target_os = "macos")]
fn listener_pids(port: u16, host: &str) -> Result<Vec<u32>> {
    let output = std::process::Command::new("/usr/sbin/lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fpn"])
        .output()
        .context("could not run system lsof")?;
    // lsof uses exit 1 with empty output when there are no matching sockets.
    ensure!(
        output.status.success()
            || output.status.code() == Some(1)
                && output.stdout.is_empty()
                && output.stderr.is_empty(),
        "lsof failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(lsof_listener_pids(
        &String::from_utf8_lossy(&output.stdout),
        port,
        host,
    ))
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn listener_pids(_port: u16, _host: &str) -> Result<Vec<u32>> {
    anyhow::bail!("listener ownership is unsupported on this operating system")
}

#[cfg(windows)]
fn process_is_descendant(pid: u32, ancestor: u32) -> bool {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return false;
        }
        let snapshot = std::os::windows::io::OwnedHandle::from_raw_handle(snapshot);
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut parents = std::collections::BTreeMap::new();
        let mut found = Process32FirstW(snapshot.as_raw_handle(), &mut entry) != 0;
        while found {
            parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
            found = Process32NextW(snapshot.as_raw_handle(), &mut entry) != 0;
        }
        let mut cursor = pid;
        for _ in 0..12 {
            let Some(parent) = parents.get(&cursor).copied() else {
                return false;
            };
            if parent == ancestor {
                return true;
            }
            if parent == 0 || parent == cursor {
                return false;
            }
            cursor = parent;
        }
        false
    }
}
#[cfg(target_os = "linux")]
fn process_parent(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm may contain spaces and parentheses; fields after its final ')' start
    // with state, then ppid.
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[cfg(target_os = "macos")]
fn process_parent(pid: u32) -> Option<u32> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "ppid="])
        .output()
        .ok()?;
    output.status.success().then_some(())?;
    std::str::from_utf8(&output.stdout)
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn process_is_descendant(pid: u32, ancestor: u32) -> bool {
    let mut cursor = pid;
    let mut seen = std::collections::BTreeSet::new();
    while cursor != 0 && seen.len() < 128 && seen.insert(cursor) {
        let Some(parent) = process_parent(cursor) else {
            return false;
        };
        if parent == ancestor {
            return true;
        }
        cursor = parent;
    }
    false
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn process_is_descendant(_pid: u32, _ancestor: u32) -> bool {
    false
}

#[cfg(windows)]
fn process_name(pid: u32) -> String {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return String::new();
        }
        let snapshot = std::os::windows::io::OwnedHandle::from_raw_handle(snapshot);
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = Process32FirstW(snapshot.as_raw_handle(), &mut entry) != 0;
        while found {
            if entry.th32ProcessID == pid {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|ch| *ch == 0)
                    .unwrap_or(entry.szExeFile.len());
                return String::from_utf16_lossy(&entry.szExeFile[..end]);
            }
            found = Process32NextW(snapshot.as_raw_handle(), &mut entry) != 0;
        }
        String::new()
    }
}
#[cfg(target_os = "linux")]
fn process_name(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .unwrap_or_default()
        .trim()
        .to_owned()
}
#[cfg(target_os = "macos")]
fn process_name(pid: u32) -> String {
    std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default()
}
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn process_name(_pid: u32) -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    include!("parts/project_process_tests.inc.rs");
}

include!("parts/project_process_runtime.inc.rs");

pub fn definitions() -> Vec<Value> {
    let project = json!({"type":"string","default":".","description":"Project directory inside this workspace containing package.json."});
    let process_project = json!({"type":"string","description":"Restrict this process list to one project directory. Omit to list processes across the workspace, including subprojects."});
    let background = json!({"type":"boolean","default":false,"description":"Keep the process managed in the background; return process_id for status/logs/stop."});
    let wait = json!({"type":"integer","minimum":0,"maximum":60,"description":"Seconds to wait for the readiness endpoint; background launch defaults to 15, status query to 0."});
    let url = json!({"type":"string","description":"Local HTTP(S) URL for process-scoped startup readiness. Use http_probe for generic URL/API reachability independent of process ownership."});
    let port = json!({"type":"integer","minimum":1,"maximum":65535,"description":"Local TCP readiness port, used when ready_url is omitted."});
    let logs = json!({"type":"integer","minimum":4096,"maximum":60000,"default":12000});
    vec![
        json!({"name":"install_dependencies","description":"Install this npm project's dependencies through Node/npm directly. auto uses ci with an npm lockfile, otherwise install. Before starting, reject direct PowerShell calls in project lifecycle scripts and reject an effective npm script-shell configured as PowerShell. Returns check_key=npm-install:<project_path>, exit code or a managed background process. No shell command or terminal window required.","inputSchema":{"type":"object","properties":{"project_path":project,"mode":{"type":"string","enum":["auto","install","ci"],"default":"auto"},"background":background,"timeout_seconds":{"type":"integer","minimum":1,"maximum":1800,"default":600},"max_chars":logs}}}),
        json!({"name":"run_project_script","description":"Run a named package.json npm script via Node/npm. Before starting, reject direct PowerShell or .ps1 invocation in the selected script and its npm pre/post hooks, and reject an effective npm script-shell configured as PowerShell. Use foreground for build/check and background=true for dev/start. A running process is NOT proof of readiness: supply ready_url/ready_port to verify that this Agent-managed listener belongs to the process group, or query get_project_process later. Use http_probe for generic URL/API reachability independent of process ownership. Returns check_key=npm:<project_path>:<script> for foreground or npm-start:<project_path>:<script> for background (args append their JSON), exit code/logs or process_id.","inputSchema":{"type":"object","required":["script"],"properties":{"project_path":project,"script":{"type":"string"},"args":{"type":"array","maxItems":32,"items":{"type":"string"}},"background":background,"timeout_seconds":{"type":"integer","minimum":1,"maximum":1800,"default":120},"ready_url":url,"ready_port":port,"wait_seconds":wait,"max_chars":logs}}}),
        json!({"name":"get_project_process","description":"List/query only Agent-managed process status, ownership and incremental logs. Without project_path this lists managed processes across the workspace, including subprojects; with project_path it lists that project. An empty list means no process is managed here; it does not establish whether any URL is reachable. Use http_probe for general local URL/API reachability, including services started outside Agent. Every list result carries an observation whose coverage distinguishes workspace_list from project_list; a single_process result cannot stand in for either list. ready_url/ready_port performs a process-scoped startup readiness check and verifies listener ownership; it is not a generic endpoint probe. Reuse a fresh matching list observation from an earlier work packet for status-only queries; set force_refresh=true only when a new process observation is needed for a concrete reason. Pass after_seq=previous next_seq for incremental logs. Old process IDs after host restart are historical and never used to terminate reused OS PIDs.","inputSchema":{"type":"object","properties":{"process_id":{"type":"string"},"project_path":process_project,"script":{"type":"string"},"after_seq":{"type":"integer","minimum":0},"max_chars":logs,"ready_url":url,"ready_port":port,"wait_seconds":wait,"force_refresh":{"type":"boolean","default":false,"description":"Bypass fresh process state reuse only for a concrete new observation purpose."},"observation_purpose":{"type":"string","maxLength":300,"description":"Why this is a new process observation rather than reuse, required when force_refresh=true."}}}}),
        json!({"name":"stop_project_process","description":"Stop only an owned project process by process_id, including its descendant processes. Repeating stop on an exited process returns its actual exit state.","inputSchema":{"type":"object","required":["process_id"],"properties":{"process_id":{"type":"string"},"max_chars":logs}}}),
    ]
}
