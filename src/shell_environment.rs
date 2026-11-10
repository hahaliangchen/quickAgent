//! PowerShell 7 on Windows, Bash elsewhere; environment changes are child-local.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Debug)]
pub struct ShellEnvironment {
    pub executable: PathBuf,
    pub rg_directory: Option<PathBuf>,
    pub node_directory: Option<PathBuf>,
}

pub fn resolve() -> Result<ShellEnvironment> {
    let settings = read_settings(&crate::ai_proxy::config_path())?;
    resolve_with_path(settings.active_path())
}

#[derive(Default, serde::Deserialize)]
pub(crate) struct ShellSettings {
    pub bash_path: Option<PathBuf>,
    pub pwsh_path: Option<PathBuf>,
}

impl ShellSettings {
    pub fn active_path(&self) -> Option<&Path> {
        if cfg!(windows) {
            self.pwsh_path.as_deref()
        } else {
            self.bash_path.as_deref()
        }
    }
}

pub(crate) fn read_settings(config_path: &Path) -> Result<ShellSettings> {
    let path = config_path.with_file_name("agent-generation.yml");
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ShellSettings::default());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Could not read shell settings {}", path.display()));
        }
    };
    serde_yaml::from_str(&raw)
        .with_context(|| format!("Invalid shell settings in {}", path.display()))
}

#[cfg(test)]
pub(crate) fn read_bash_path(config_path: &Path) -> Result<Option<PathBuf>> {
    Ok(read_settings(config_path)?.bash_path)
}

/// Empty input clears the override. Windows explicitly probes PowerShell's major.
pub(crate) fn validate_pwsh_path(raw: &str) -> Result<Option<PathBuf>> {
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let path = Path::new(raw.trim());
    ensure!(path.is_absolute(), "PowerShell 7 path must be absolute");
    #[cfg(windows)]
    {
        detect_shell(Some(path), false)?;
    }
    #[cfg(not(windows))]
    ensure!(
        executable_file(path),
        "PowerShell executable is missing: {}",
        path.display()
    );
    Ok(Some(path.to_path_buf()))
}

pub(crate) fn validate_bash_path(raw: &str) -> Result<Option<PathBuf>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let path = Path::new(raw);
    ensure!(
        path.is_absolute() && executable_file(path),
        "Bash path must be an absolute executable path: {}",
        path.display()
    );
    Ok(Some(path.to_path_buf()))
}

fn resolve_with_path(configured: Option<&Path>) -> Result<ShellEnvironment> {
    let shell = detect_shell(configured, false)?;
    let executable = shell.executable;
    let bundled = bundled_rg()?;
    let rg_directory = bundled.or_else(|| on_path(rg_name()).and_then(parent));
    ensure!(
        rg_directory.is_some(),
        "ripgrep (rg) is missing. Build the desktop app to prepare its verified bundled rg, or install ripgrep and add its directory to PATH"
    );
    Ok(ShellEnvironment {
        executable,
        rg_directory,
        node_directory: crate::project_process::resolve_node_executable()
            .ok()
            .and_then(parent),
    })
}

impl ShellEnvironment {
    /// Use for Bash and for other commands that need the same tool PATH.
    /// Keeps the caller's explicit PATH, when present, and changes only the child.
    pub fn apply_environment(&self, command: &mut tokio::process::Command) -> Result<()> {
        let inherited = command
            .as_std()
            .get_envs()
            .find(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("PATH"))
            .map(|(_, value)| value.map(OsString::from))
            .unwrap_or_else(|| env::var_os("PATH"));
        let mut directories = Vec::new();
        if let Some(path) = &self.rg_directory {
            directories.push(path.clone());
        }
        if let Some(path) = &self.node_directory {
            directories.push(path.clone());
        }
        if let Some(directory) = self.executable.parent() {
            directories.push(directory.to_path_buf());
        }
        if let Some(path) = inherited {
            directories.extend(env::split_paths(&path));
        }
        let mut unique = Vec::new();
        for path in directories {
            if !unique.contains(&path) {
                unique.push(path);
            }
        }
        command.env(
            "PATH",
            env::join_paths(unique).context("could not construct shell tool PATH")?,
        );
        Ok(())
    }
}

/// Local settings/diagnostics projection; a missing dependency is actionable data.
pub fn description() -> Value {
    let settings = read_settings(&crate::ai_proxy::config_path());
    match settings {
        Ok(settings) => cached_description(settings.active_path()),
        Err(error) => status_json(Err(format!("{error:#}")), false, None, false),
    }
}

fn parent(path: PathBuf) -> Option<PathBuf> {
    path.parent().map(Path::to_path_buf)
}
fn rg_name() -> &'static str {
    if cfg!(windows) { "rg.exe" } else { "rg" }
}

fn executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return path
            .metadata()
            .is_ok_and(|m| m.permissions().mode() & 0o111 != 0);
    }
    #[cfg(not(unix))]
    true
}

fn on_path(name: &str) -> Option<PathBuf> {
    env::var_os("PATH")
        .into_iter()
        .flat_map(|value| env::split_paths(&value).collect::<Vec<_>>())
        // Do not resolve tools relative to an untrusted project's working directory.
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(name))
        .find(|path| executable_file(path))
}

fn bundled_rg() -> Result<Option<PathBuf>> {
    let exe = env::current_exe().context("could not locate the running agent executable")?;
    if let Some(directory) = exe.parent() {
        if executable_file(&directory.join(rg_name())) {
            return Ok(Some(directory.to_path_buf()));
        }
    }
    // compile-time source path is only a development fallback, never the cwd.
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("desktop/binaries");
    let target = if cfg!(windows) {
        format!(
            "{}-pc-windows-{}",
            env::consts::ARCH,
            if cfg!(target_env = "gnu") {
                "gnu"
            } else {
                "msvc"
            }
        )
    } else if cfg!(target_os = "macos") {
        format!("{}-apple-darwin", env::consts::ARCH)
    } else {
        format!(
            "{}-unknown-linux-{}",
            env::consts::ARCH,
            if cfg!(target_env = "musl") {
                "musl"
            } else {
                "gnu"
            }
        )
    };
    let suffixed = directory.join(format!(
        "rg-{target}{}",
        if cfg!(windows) { ".exe" } else { "" }
    ));
    if executable_file(&suffixed) {
        ensure!(
            executable_file(&directory.join(rg_name())),
            "Bundled ripgrep exists at {}, but its development PATH alias {} is missing. Rebuild desktop/Cargo.toml to prepare both files",
            suffixed.display(),
            directory.join(rg_name()).display()
        );
        return Ok(Some(directory));
    }
    Ok(None)
}

#[derive(Clone, Debug)]
struct DetectedShell {
    executable: PathBuf,
    version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Candidate {
    path: PathBuf,
    stamp: Option<(u64, Option<SystemTime>, bool)>,
}

struct ProbeCache {
    candidates: Vec<Candidate>,
    checked: Instant,
    checked_at: u64,
    result: std::result::Result<DetectedShell, String>,
}

static SHELL_CACHE: OnceLock<Mutex<Option<ProbeCache>>> = OnceLock::new();
fn shell_cache() -> &'static Mutex<Option<ProbeCache>> {
    SHELL_CACHE.get_or_init(Default::default)
}

fn candidate_exists(path: &Path) -> bool {
    // Store execution aliases are reparse points: following their target can
    // fail with EACCES even though CreateProcess can execute the alias.
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() || m.file_type().is_symlink())
}

fn candidate_paths(
    configured: Option<&Path>,
    read_env: impl Fn(&str) -> Option<OsString>,
) -> Vec<PathBuf> {
    let override_name = if cfg!(windows) {
        "GROW_PWSH_PATH"
    } else {
        "GROW_BASH_PATH"
    };
    let explicit = configured
        .map(Path::to_path_buf)
        .or_else(|| read_env(override_name).map(PathBuf::from));
    let paths = if let Some(explicit) = explicit {
        vec![explicit]
    } else {
        let mut paths = Vec::new();
        #[cfg(windows)]
        paths.push(
            PathBuf::from(read_env("ProgramFiles").unwrap_or_else(|| r"C:\Program Files".into()))
                .join("PowerShell/7/pwsh.exe"),
        );
        #[cfg(not(windows))]
        paths.push(PathBuf::from("/bin/bash"));
        let name = if cfg!(windows) { "pwsh.exe" } else { "bash" };
        if let Some(path) = read_env("PATH") {
            for directory in env::split_paths(&path) {
                let directory = PathBuf::from(
                    directory
                        .as_os_str()
                        .to_string_lossy()
                        .trim()
                        .trim_matches('"'),
                );
                if directory.is_absolute() {
                    paths.push(directory.join(name));
                }
            }
        }
        #[cfg(windows)]
        if let Some(local) = read_env("LOCALAPPDATA") {
            // Installation can add this alias without changing this process's PATH.
            paths.push(PathBuf::from(local).join("Microsoft/WindowsApps/pwsh.exe"));
        }
        paths
    };
    paths
}

fn candidates(configured: Option<&Path>) -> Vec<Candidate> {
    let mut unique = Vec::new();
    for path in candidate_paths(configured, |name| env::var_os(name)) {
        if unique
            .iter()
            .any(|candidate: &Candidate| candidate.path == path)
        {
            continue;
        }
        let stamp = std::fs::symlink_metadata(&path)
            .ok()
            .map(|m| (m.len(), m.modified().ok(), m.file_type().is_symlink()));
        unique.push(Candidate { path, stamp });
    }
    unique
}

fn cache_is_fresh(cache: &ProbeCache, candidates: &[Candidate]) -> bool {
    let ttl = if cache.result.is_ok() { 300 } else { 5 };
    cache.candidates == candidates && cache.checked.elapsed() < Duration::from_secs(ttl)
}

fn detect_shell(configured: Option<&Path>, force: bool) -> Result<DetectedShell> {
    let candidates = candidates(configured);
    let mut guard = shell_cache().lock().unwrap_or_else(|e| e.into_inner());
    if !force {
        if let Some(cache) = guard.as_ref() {
            if cache_is_fresh(cache, &candidates) {
                return cache.result.clone().map_err(anyhow::Error::msg);
            }
        }
    }
    let result = probe_candidates(&candidates).map_err(|error| format!("{error:#}"));
    *guard = Some(ProbeCache {
        candidates,
        checked: Instant::now(),
        checked_at: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        result: result.clone(),
    });
    result.map_err(anyhow::Error::msg)
}

fn probe_candidates(candidates: &[Candidate]) -> Result<DetectedShell> {
    let mut errors = Vec::new();
    for candidate in candidates {
        if !candidate.path.is_absolute() {
            errors.push(format!(
                "Shell path must be absolute: {}",
                candidate.path.display()
            ));
            continue;
        }
        if !candidate_exists(&candidate.path) {
            continue;
        }
        #[cfg(windows)]
        match probe_pwsh(&candidate.path) {
            Ok(version) => {
                return Ok(DetectedShell {
                    executable: candidate.path.clone(),
                    version: Some(version),
                });
            }
            Err(error) => errors.push(format!("{}: {error:#}", candidate.path.display())),
        }
        #[cfg(not(windows))]
        if executable_file(&candidate.path) {
            return Ok(DetectedShell {
                executable: candidate.path.clone(),
                version: None,
            });
        }
    }
    let guidance = if cfg!(windows) {
        "PowerShell 7+ is required. Install PowerShell from Microsoft Store, select pwsh.exe in Settings, or set GROW_PWSH_PATH. Windows PowerShell 5.1 is unsupported."
    } else {
        "Bash is missing. Install bash, choose its absolute path in Settings, or set GROW_BASH_PATH."
    };
    anyhow::bail!("{guidance} {}", errors.join("; "))
}

#[cfg(windows)]
fn parse_pwsh_version(stdout: &[u8]) -> Result<String> {
    let text = String::from_utf8_lossy(stdout);
    let mut lines = text
        .trim_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let major = lines
        .next()
        .unwrap_or("")
        .parse::<u32>()
        .context("PowerShell returned an invalid major version")?;
    ensure!(
        major >= 7,
        "PowerShell {major} is unsupported; PowerShell 7+ is required (no 5.1 fallback)"
    );
    Ok(lines.next().unwrap_or_else(|| text.trim()).to_owned())
}

#[cfg(windows)]
fn probe_pwsh(path: &Path) -> Result<String> {
    use std::{
        io::Read,
        os::windows::process::CommandExt,
        process::{Command, Stdio},
    };
    ensure!(
        path.is_absolute() && candidate_exists(path),
        "PowerShell 7 path must be an absolute executable path: {}",
        path.display()
    );
    let mut child = Command::new(path)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$PSVersionTable.PSVersion.Major; $PSVersionTable.PSVersion.ToString()",
        ])
        .creation_flags(0x08000000)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("Could not start {}", path.display()))?;
    let stdout = child
        .stdout
        .take()
        .context("PowerShell version stdout unavailable")?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(4096).read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(anyhow::anyhow!(
                    "PowerShell version probe failed or timed out: {result:?}"
                ));
            }
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| anyhow::anyhow!("PowerShell version reader failed"))??;
    ensure!(
        status?.success(),
        "PowerShell version check exited unsuccessfully"
    );
    parse_pwsh_version(&bytes)
}

pub const INSTALL_COMMAND: &str = "winget install --id 9MZ1SNWT0N5D --source msstore --accept-source-agreements --accept-package-agreements --disable-interactivity";
pub const STORE_URL: &str = "ms-windows-store://pdp/?ProductId=9MZ1SNWT0N5D";

fn status_json(
    result: std::result::Result<DetectedShell, String>,
    cached: bool,
    checked_at: Option<u64>,
    validated: bool,
) -> Value {
    let (executable, version, mut error) = match result {
        Ok(shell) => (Some(shell.executable), shell.version, None),
        Err(error) => (None, None, Some(error)),
    };
    let detected = executable.is_some();
    let rg = bundled_rg().and_then(|directory| {
        directory.or_else(|| on_path(rg_name()).and_then(parent))
            .context("ripgrep (rg) is missing. Build the desktop app to prepare its verified bundled rg, or install ripgrep in PATH")
    });
    if detected {
        if let Err(rg_error) = &rg {
            error = Some(format!("{rg_error:#}"));
        }
    }
    json!({
        "available": detected && rg.is_ok(), "pwsh_detected": cfg!(windows) && detected,
        "platform": {"os":env::consts::OS,"arch":env::consts::ARCH},
        "executable":executable,"version":version,"error":error,
        "rg_directory":rg.ok(),
        "node_directory":crate::project_process::resolve_node_executable().ok().and_then(parent),
        "cache":{"validated":validated,"cached":cached,"checked_at":checked_at},
        "configuration":{"setting":if cfg!(windows) {"pwsh_path"} else {"bash_path"},"environment_override":if cfg!(windows) {"GROW_PWSH_PATH"} else {"GROW_BASH_PATH"}},
        "install":{"command":INSTALL_COMMAND,"store_url":STORE_URL,"supported":cfg!(windows)},
    })
}

pub(crate) fn cached_description(configured: Option<&Path>) -> Value {
    let candidates = candidates(configured);
    let guard = shell_cache().lock().unwrap_or_else(|e| e.into_inner());
    match guard
        .as_ref()
        .filter(|cache| cache.candidates == candidates)
    {
        Some(cache) => status_json(cache.result.clone(), true, Some(cache.checked_at), true),
        None => {
            let mut status = status_json(
                Err("Shell has not been checked in this code session".into()),
                false,
                None,
                false,
            );
            status["error"] = Value::Null;
            status
        }
    }
}

pub(crate) fn detect_description(configured: Option<&Path>, force: bool) -> Value {
    let previous = shell_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|cache| cache.checked_at);
    let result = detect_shell(configured, force).map_err(|error| format!("{error:#}"));
    let guard = shell_cache().lock().unwrap_or_else(|e| e.into_inner());
    let checked_at = guard.as_ref().map(|cache| cache.checked_at);
    status_json(
        result,
        !force && previous.is_some() && previous == checked_at,
        checked_at,
        true,
    )
}

#[derive(Clone, serde::Serialize)]
struct InstallState {
    running: bool,
    status: &'static str,
    command: &'static str,
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    error: Option<String>,
    #[serde(skip)]
    cancel: Option<tokio_util::sync::CancellationToken>,
}

impl Default for InstallState {
    fn default() -> Self {
        Self {
            running: false,
            status: "idle",
            command: INSTALL_COMMAND,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            error: None,
            cancel: None,
        }
    }
}

static INSTALL_STATE: OnceLock<Mutex<InstallState>> = OnceLock::new();
fn install_state() -> &'static Mutex<InstallState> {
    INSTALL_STATE.get_or_init(Default::default)
}

pub(crate) fn installation_status() -> Value {
    json!(
        install_state()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    )
}

pub(crate) fn cancel_installation() -> Value {
    let mut state = install_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cancel) = &state.cancel {
        cancel.cancel();
        state.status = "cancelling";
    }
    json!(state.clone())
}

/// Called only by the explicit install-button POST. Discovery never calls this.
pub(crate) fn start_installation(config_path: &Path) -> Result<Value> {
    ensure!(
        cfg!(windows),
        "PowerShell Store installation is only supported on Windows"
    );
    let mut state = install_state().lock().unwrap_or_else(|e| e.into_inner());
    if state.running {
        return Ok(json!(state.clone()));
    }
    let cancel = tokio_util::sync::CancellationToken::new();
    *state = InstallState {
        running: true,
        status: "running",
        cancel: Some(cancel.clone()),
        ..Default::default()
    };
    let config_path = config_path.to_path_buf();
    tokio::spawn(async move {
        let result = run_installation(&cancel).await;
        // The UI also detects after completion; invalidate both success/failure
        // cache entries so a freshly registered Store alias is immediately seen.
        *shell_cache().lock().unwrap_or_else(|e| e.into_inner()) = None;
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(settings) = read_settings(&config_path) {
                detect_description(settings.active_path(), true);
            }
        })
        .await;
        let mut state = install_state().lock().unwrap_or_else(|e| e.into_inner());
        state.running = false;
        state.cancel = None;
        match result {
            Ok(code) => {
                state.exit_code = Some(code);
                state.status = if code == 0 { "succeeded" } else { "failed" };
                if code != 0 {
                    state.error = Some(format!(
                        "winget exited with code {code}; open Microsoft Store or specify an installed pwsh.exe"
                    ));
                }
            }
            Err(error) => {
                state.status = if cancel.is_cancelled() {
                    "cancelled"
                } else {
                    "failed"
                };
                state.error = Some(format!("{error:#}"));
            }
        }
    });
    Ok(json!(state.clone()))
}

fn append_install_log(stderr: bool, text: &str) {
    let mut state = install_state().lock().unwrap_or_else(|e| e.into_inner());
    let buffer = if stderr {
        &mut state.stderr
    } else {
        &mut state.stdout
    };
    buffer.push_str(text);
    const LIMIT: usize = 64 * 1024;
    if buffer.len() > LIMIT {
        let mut boundary = buffer.len() - LIMIT;
        while !buffer.is_char_boundary(boundary) {
            boundary += 1;
        }
        buffer.drain(..boundary);
    }
}

async fn install_log(mut reader: impl tokio::io::AsyncRead + Unpin, stderr: bool) {
    use tokio::io::AsyncReadExt;
    let mut buffer = [0u8; 2048];
    let mut decoder = crate::shell_process::Decoder::default();
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(size) => append_install_log(stderr, &decoder.decode(&buffer[..size], false)),
        }
    }
    append_install_log(stderr, &decoder.decode(&[], true));
}

async fn run_installation(cancel: &tokio_util::sync::CancellationToken) -> Result<i32> {
    use std::process::Stdio;
    let windows_apps = env::var_os("LOCALAPPDATA")
        .map(|local| PathBuf::from(local).join("Microsoft/WindowsApps/winget.exe"));
    let winget = windows_apps.filter(|path| candidate_exists(path)).or_else(|| on_path("winget.exe"))
        .context("winget is unavailable. Open Microsoft Store to install PowerShell, then choose Already installed / Detect again")?;
    let mut command = tokio::process::Command::new(winget);
    command
        .args([
            "install",
            "--id",
            "9MZ1SNWT0N5D",
            "--source",
            "msstore",
            "--accept-source-agreements",
            "--accept-package-agreements",
            "--disable-interactivity",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::project_process::prepare_process_group(&mut command);
    let mut child = command
        .spawn()
        .context("Could not start winget; open Microsoft Store instead")?;
    let group = match crate::project_process::ProcessGroup::attach(&child) {
        Ok(group) => group,
        Err(error) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(error);
        }
    };
    let stdout = tokio::spawn(install_log(
        child.stdout.take().context("winget stdout missing")?,
        false,
    ));
    let stderr = tokio::spawn(install_log(
        child.stderr.take().context("winget stderr missing")?,
        true,
    ));
    let result = tokio::select! {
        status = child.wait() => status.map_err(anyhow::Error::from),
        _ = cancel.cancelled() => Err(anyhow::anyhow!("PowerShell installation cancelled")),
        _ = tokio::time::sleep(Duration::from_secs(900)) => Err(anyhow::anyhow!("PowerShell installation timed out after 15 minutes")),
    };
    // The suspended child joined a kill-on-close Job before it could spawn;
    // success, cancellation, timeout, and backend exit all clean up its tree.
    let _ = group.terminate();
    if result.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    drop(group);
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        let _ = stdout.await;
        let _ = stderr.await;
    })
    .await;
    Ok(result?.code().unwrap_or(-1))
}

pub(crate) async fn open_store() -> Result<Value> {
    ensure!(
        cfg!(windows),
        "Microsoft Store is only available on Windows"
    );
    let explorer = PathBuf::from(env::var_os("SystemRoot").context("SystemRoot is unavailable")?)
        .join("explorer.exe");
    let mut command = tokio::process::Command::new(explorer);
    command.arg(STORE_URL);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command.spawn().context("Could not open Microsoft Store")?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(json!({"opened":true,"store_url":STORE_URL}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_selection_has_priority_over_environment() {
        let manual = env::temp_dir().join("manual shell/pwsh.exe");
        let selected = candidate_paths(Some(&manual), |_| {
            Some("ignored environment override".into())
        });
        assert_eq!(selected, [manual]);
    }

    #[cfg(windows)]
    #[test]
    fn automatic_candidates_use_program_files_path_then_store_without_legacy_shell() {
        let paths = candidate_paths(None, |key| match key {
            "ProgramFiles" => Some(r"C:\Program Files".into()),
            "PATH" => Some(r#""D:\quoted shell";E:\tools;;relative"#.into()),
            "LOCALAPPDATA" => Some(r"C:\Users\test\AppData\Local".into()),
            _ => None,
        });
        assert_eq!(
            paths,
            [
                PathBuf::from(r"C:\Program Files\PowerShell\7\pwsh.exe"),
                PathBuf::from(r"D:\quoted shell\pwsh.exe"),
                PathBuf::from(r"E:\tools\pwsh.exe"),
                PathBuf::from(r"C:\Users\test\AppData\Local\Microsoft\WindowsApps\pwsh.exe"),
            ]
        );
    }

    #[test]
    fn probe_cache_invalidates_for_changed_files_and_expired_results() {
        let candidate = Candidate {
            path: env::temp_dir().join("pwsh.exe"),
            stamp: Some((1, None, false)),
        };
        let mut cache = ProbeCache {
            candidates: vec![candidate.clone()],
            checked: Instant::now(),
            checked_at: 0,
            result: Ok(DetectedShell {
                executable: candidate.path.clone(),
                version: Some("7.6.5".into()),
            }),
        };
        assert!(cache_is_fresh(&cache, &[candidate.clone()]));
        let mut replaced = candidate.clone();
        replaced.stamp = Some((2, None, false));
        assert!(!cache_is_fresh(&cache, &[replaced]));
        cache.checked = Instant::now() - Duration::from_secs(301);
        assert!(!cache_is_fresh(&cache, &[candidate.clone()]));
        cache.result = Err("missing".into());
        cache.checked = Instant::now() - Duration::from_secs(6);
        assert!(!cache_is_fresh(&cache, &[candidate]));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires a preinstalled Microsoft Store PowerShell alias; never installs anything"]
    fn store_alias_is_probed_without_following_its_target() {
        let alias = PathBuf::from(env::var_os("LOCALAPPDATA").unwrap())
            .join("Microsoft/WindowsApps/pwsh.exe");
        assert!(
            candidate_exists(&alias),
            "Store alias must be discovered with symlink_metadata"
        );
        let version = probe_pwsh(&alias).unwrap();
        eprintln!("Store PowerShell: {} version {}", alias.display(), version);
        assert!(
            version.starts_with('7')
                || version.split('.').next().unwrap().parse::<u32>().unwrap() > 7
        );
    }

    #[test]
    fn relative_explicit_path_is_rejected() {
        let candidate = Candidate {
            path: PathBuf::from("relative/pwsh.exe"),
            stamp: None,
        };
        assert!(
            probe_candidates(&[candidate])
                .unwrap_err()
                .to_string()
                .contains("absolute")
        );
    }

    #[cfg(windows)]
    #[test]
    fn version_check_rejects_legacy_and_invalid_output() {
        assert!(parse_pwsh_version(b"5\r\n5.1.0\r\n").is_err());
        assert!(parse_pwsh_version(b"not a version").is_err());
        assert_eq!(parse_pwsh_version(b"7\r\n7.6.5\r\n").unwrap(), "7.6.5");
    }

    #[test]
    fn environment_preserves_child_path_without_changing_parent() {
        let before = env::var_os("PATH");
        let base = env::temp_dir();
        let tools = base.join("grow test tools");
        let node = base.join("grow test node");
        let original = base.join("grow test original");
        let shell = ShellEnvironment {
            executable: base.join("Git/bin/bash.exe"),
            rg_directory: Some(tools.clone()),
            node_directory: Some(node.clone()),
        };
        let mut command = tokio::process::Command::new(&shell.executable);
        command.env("PATH", env::join_paths([&original, &tools]).unwrap());
        shell.apply_environment(&mut command).unwrap();
        let path = command
            .as_std()
            .get_envs()
            .find(|(key, _)| *key == "PATH")
            .unwrap()
            .1
            .unwrap()
            .to_owned();
        let entries: Vec<_> = env::split_paths(&path).collect();
        assert_eq!(&entries[..2], &[tools.clone(), node]);
        assert!(entries.contains(&original));
        assert_eq!(entries.iter().filter(|path| **path == tools).count(), 1);
        assert_eq!(env::var_os("PATH"), before);
    }

    #[test]
    #[ignore = "requires installed PowerShell 7/Bash and ripgrep"]
    fn resolved_shell_runs_rg_and_optional_node() {
        let shell = resolve().expect("smoke test requires PowerShell 7/Bash and ripgrep");
        let mut command = tokio::process::Command::new(&shell.executable);
        #[cfg(windows)]
        command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; rg --version; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }; if (Get-Command node -ErrorAction SilentlyContinue) { node --version; exit $LASTEXITCODE }"]);
        #[cfg(not(windows))]
        command.args([
            "--noprofile",
            "--norc",
            "-c",
            "rg --version && if command -v node >/dev/null; then node --version; fi",
        ]);
        shell.apply_environment(&mut command).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let output = runtime.block_on(command.output()).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("ripgrep"));
    }
}
