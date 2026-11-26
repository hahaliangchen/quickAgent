//! Capability entry rules: directory approvals, credential references and the local control plane.
use anyhow::{Result, ensure};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
#[cfg(target_os = "linux")]
use std::net::Ipv6Addr;
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    future::Future,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
use tokio::process::Command;

/// Tool-entry directory and mutation policy; never an OS process sandbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionBoundary {
    Host,
    ReadOnly,
    Workspace,
}

impl ExecutionBoundary {
    pub fn for_mode(mode: crate::server::agent_service::PermissionMode) -> Self {
        use crate::server::agent_service::PermissionMode;
        match mode {
            PermissionMode::ReadOnly => Self::ReadOnly,
            PermissionMode::WorkspaceWrite | PermissionMode::Ask | PermissionMode::AutoReview => {
                Self::Workspace
            }
            PermissionMode::FullAccess => Self::Host,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToolSandboxContext {
    pub boundary: ExecutionBoundary,
    pub extra_writable_roots: Vec<PathBuf>,
}

tokio::task_local! {
    static TOOL_SANDBOX_CONTEXT: ToolSandboxContext;
}

pub async fn scope_tool_sandbox<F, T>(
    boundary: ExecutionBoundary,
    extra_writable_roots: Vec<PathBuf>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    let normalized = normalize_extra_roots(&extra_writable_roots);
    TOOL_SANDBOX_CONTEXT
        .scope(
            ToolSandboxContext {
                boundary,
                extra_writable_roots: normalized,
            },
            future,
        )
        .await
}

pub async fn scope_optional_tool_sandbox<F, T>(ctx: Option<ToolSandboxContext>, future: F) -> T
where
    F: Future<Output = T>,
{
    match ctx {
        Some(ctx) => TOOL_SANDBOX_CONTEXT.scope(ctx, future).await,
        None => future.await,
    }
}

pub fn current_tool_sandbox() -> Option<ToolSandboxContext> {
    TOOL_SANDBOX_CONTEXT.try_with(Clone::clone).ok()
}

pub fn current_extra_writable_roots() -> Vec<PathBuf> {
    current_tool_sandbox()
        .map(|ctx| ctx.extra_writable_roots)
        .unwrap_or_default()
}

pub fn is_extra_writable_dir(path: &Path) -> bool {
    let Ok(canon) = path.canonicalize() else {
        return false;
    };
    current_extra_writable_roots()
        .iter()
        .any(|extra| canon.starts_with(extra))
}

pub fn allows_external_dir(root: &Path, target: &Path) -> bool {
    let Ok(canon_target) = target.canonicalize() else {
        return false;
    };
    if let Ok(canon_root) = root.canonicalize()
        && canon_target.starts_with(&canon_root)
    {
        return true;
    }
    current_tool_sandbox().is_some_and(|ctx| {
        ctx.boundary == ExecutionBoundary::Host
            || ctx
                .extra_writable_roots
                .iter()
                .any(|extra| canon_target.starts_with(extra))
    })
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

pub(crate) fn is_allowed_extra_root(canon: &Path) -> bool {
    if !canon.is_dir() || canon.parent().is_none() || canon == Path::new("/tmp") {
        return false;
    }
    if let Some(home) = home_dir().and_then(|h| h.canonicalize().ok())
        && canon == home
    {
        return false;
    }
    true
}

fn normalize_extra_roots(raw: &[PathBuf]) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for path in raw {
        let Ok(canon) = path.canonicalize() else {
            continue;
        };
        if !is_allowed_extra_root(&canon) {
            continue;
        }
        if !candidates.contains(&canon) {
            candidates.push(canon);
        }
    }
    candidates.sort();
    let mut collapsed: Vec<PathBuf> = Vec::new();
    for dir in candidates {
        if !collapsed.iter().any(|existing| dir.starts_with(existing)) {
            collapsed.push(dir);
        }
    }
    collapsed
}

/// Construct a native command with the parent agent's internal environment removed.
/// Directory and execution permissions are enforced by the calling tool, not the OS.
pub fn command(
    program: impl AsRef<OsStr>,
    root: &Path,
    cwd: &Path,
    boundary: ExecutionBoundary,
) -> Result<Command> {
    command_with_roots(program, root, &[], cwd, boundary)
}

pub fn command_with_roots(
    program: impl AsRef<OsStr>,
    _root: &Path,
    _extra_writable_roots: &[PathBuf],
    cwd: &Path,
    _boundary: ExecutionBoundary,
) -> Result<Command> {
    let mut cmd = Command::new(program);
    cmd.current_dir(cwd);
    strip_parent_agent_internal_envs(&mut cmd);
    Ok(cmd)
}

/// Internal wiring env vars of the running agent daemon that must never leak
/// into child shells (otherwise a child test server launched without explicit
/// `WORKSPACE_ROOT` inherits the live desktop DB and corrupts active tasks).
const HOST_AGENT_INTERNAL_ENVS: &[&str] = &[
    "WORKSPACE_ROOT",
    "GROW_CONFIG_DIR",
    "GROW_RESOURCES_DIR",
    "GROW_DESKTOP_BIND",
    "AGENT_WEB_DIR",
    "AGENT_BIND",
    "DAILY_REPORT_DATA_DIR",
];

fn strip_parent_agent_internal_envs(cmd: &mut Command) {
    for key in HOST_AGENT_INTERNAL_ENVS {
        cmd.env_remove(key);
    }
}

/// Reject explicit references to protected host paths at the command entry point.
/// This is a text check, not a filesystem restriction on the child process.
pub fn check_secret_reference(command: &str) -> Result<()> {
    let home = home_dir();
    let normalize = |text: &str| {
        let text = text.replace('\\', "/");
        if cfg!(windows) { text.to_ascii_lowercase() } else { text }
    };
    let normalized = normalize(command);
    static RESERVED_PATH: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?:^|[\s/'"=:(])(?:\.ssh(?:$|[/\s'";)&|])|codex_state\.db(?:-(?:wal|shm|journal))?(?:$|[\s'";)&|]))"#).unwrap()
    });
    ensure!(
        !RESERVED_PATH.is_match(&normalized),
        "SECRET_FILE: the command references SSH credentials or the host state database"
    );
    let mut secrets = crate::server::ai_proxy::secret_files();
    secrets.push(crate::server::ai_proxy::config_path());
    for secret in secrets {
        let (Some(dir), Some(name)) = (secret.parent(), secret.file_name().and_then(OsStr::to_str))
        else {
            continue;
        };
        if !normalized.contains(&normalize(name)) {
            continue;
        }
        let mut spellings = vec![normalize(&dir.to_string_lossy())];
        if let Ok(canonical) = dir.canonicalize() {
            spellings.push(normalize(&canonical.to_string_lossy()));
        }
        if let Some(relative) = home.as_deref().and_then(|home| dir.strip_prefix(home).ok()) {
            spellings.push(normalize(&relative.to_string_lossy()));
        }
        if spellings
            .iter()
            .any(|dir| !dir.is_empty() && normalized.contains(dir.as_str()))
        {
            anyhow::bail!("SECRET_FILE: the command names protected host file {}", secret.display());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Host control-plane port registry and HTTP request guard
// ---------------------------------------------------------------------------

static CONTROL_PORTS: OnceLock<Mutex<BTreeSet<u16>>> = OnceLock::new();

fn control_ports() -> &'static Mutex<BTreeSet<u16>> {
    CONTROL_PORTS.get_or_init(Default::default)
}

pub fn register_control_port(port: u16) {
    if port > 0 {
        control_ports().lock().unwrap().insert(port);
    }
}

pub(super) fn host_control_ports() -> Vec<u16> {
    control_ports().lock().unwrap().iter().copied().collect()
}

pub fn is_host_control_port(port: u16) -> bool {
    control_ports().lock().unwrap().contains(&port)
}

fn host_looks_local(host: &str) -> bool {
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if stripped.eq_ignore_ascii_case("localhost")
        || stripped.to_ascii_lowercase().ends_with(".localhost")
    {
        return true;
    }
    if let Ok(ip) = stripped.parse::<IpAddr>() {
        return ip.is_loopback() || ip.is_unspecified();
    }
    false
}

pub fn check_not_host_control_url(raw_url: &str) -> Result<()> {
    let Ok(url) = reqwest::Url::parse(raw_url) else {
        return Ok(());
    };
    if !matches!(url.scheme(), "http" | "https" | "ws" | "wss") {
        return Ok(());
    }
    let Some(port) = url.port_or_known_default() else {
        return Ok(());
    };
    if !is_host_control_port(port) {
        return Ok(());
    }
    if url.host_str().is_some_and(host_looks_local) {
        anyhow::bail!(
            "CONTROL_PORT_BLOCKED: refusing to access the host control service on port {port}"
        );
    }
    Ok(())
}

pub async fn check_not_host_control_url_async(raw_url: &str) -> Result<()> {
    check_not_host_control_url(raw_url)?;
    let Ok(url) = reqwest::Url::parse(raw_url) else {
        return Ok(());
    };
    if !matches!(url.scheme(), "http" | "https" | "ws" | "wss") {
        return Ok(());
    }
    let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
        return Ok(());
    };
    if !is_host_control_port(port) {
        return Ok(());
    }
    if let Ok(Ok(addrs)) = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::net::lookup_host((host, port)),
    )
    .await
    {
        for addr in addrs {
            if addr.ip().is_loopback() || addr.ip().is_unspecified() {
                anyhow::bail!(
                    "CONTROL_PORT_BLOCKED: refusing to access the host control service on port {port}"
                );
            }
        }
    }
    Ok(())
}

fn session_secret() -> &'static str {
    static SECRET: OnceLock<String> = OnceLock::new();
    SECRET.get_or_init(|| {
        let mut bytes = [0u8; 32];
        if getrandom::fill(&mut bytes).is_err() {
            panic!("OS CSPRNG unavailable for local session secret");
        }
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    })
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn valid_session_cookie(headers: &axum::http::HeaderMap) -> bool {
    let expected = session_secret().as_bytes();
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|raw| raw.split(';'))
        .filter_map(|pair| pair.trim().strip_prefix("grow_session="))
        .any(|val| constant_time_eq(val.trim().as_bytes(), expected))
}

fn valid_bearer_token(headers: &axum::http::HeaderMap) -> bool {
    let Some(raw) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| v.len() >= 32)
    else {
        return false;
    };
    if constant_time_eq(raw.as_bytes(), session_secret().as_bytes()) {
        return true;
    }
    let token_path = crate::server::ai_proxy::config_dir().join("local_api_token");
    if let Ok(disk) = std::fs::read_to_string(token_path) {
        let disk = disk.trim();
        if disk.len() >= 32 && constant_time_eq(raw.as_bytes(), disk.as_bytes()) {
            return true;
        }
    }
    false
}

pub(crate) fn is_loopback_origin(origin: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(origin) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https" | "tauri") {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if stripped.eq_ignore_ascii_case("localhost")
        || stripped.eq_ignore_ascii_case("tauri.localhost")
    {
        return true;
    }
    stripped
        .parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

fn is_loopback_host_header(raw_host: &str) -> bool {
    let trimmed = raw_host.trim();
    if trimmed.is_empty() {
        return false;
    }
    let host_part = if let Some(rest) = trimmed.strip_prefix('[') {
        let Some((inside, after)) = rest.split_once(']') else {
            return false;
        };
        if !after.is_empty()
            && !after
                .strip_prefix(':')
                .is_some_and(|p| p.parse::<u16>().is_ok())
        {
            return false;
        }
        inside
    } else if let Some((h, port)) = trimmed.rsplit_once(':') {
        if h.contains(':') {
            trimmed
        } else if port.parse::<u16>().is_ok() {
            h
        } else {
            return false;
        }
    } else {
        trimmed
    };
    if host_part.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host_part
        .parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// Protects local HTTP control servers (Agent UI/API and MCP HTTP) against:
/// 1. Cross-site browser requests (`Sec-Fetch-Site: cross-site` or non-loopback `Origin`).
/// 2. DNS-rebinding attacks (`Host` header spoofing on loopback listeners).
/// 3. On Linux, unverified local peers and agent descendants in a different PID namespace.
pub async fn local_http_guard(
    State(server_addr): State<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    if headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("cross-site"))
    {
        return (
            StatusCode::FORBIDDEN,
            "FORBIDDEN: cross-site request to local service rejected",
        )
            .into_response();
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let Ok(origin_str) = origin.to_str() else {
            return (StatusCode::FORBIDDEN, "FORBIDDEN: invalid Origin header").into_response();
        };
        if !is_loopback_origin(origin_str) {
            return (
                StatusCode::FORBIDDEN,
                "FORBIDDEN: untrusted Origin for local service",
            )
                .into_response();
        }
    }
    if server_addr.ip().is_loopback()
        && let Some(host) = headers.get(header::HOST)
    {
        let Ok(host_str) = host.to_str() else {
            return (StatusCode::FORBIDDEN, "FORBIDDEN: invalid Host header").into_response();
        };
        if !is_loopback_host_header(host_str) {
            return (
                StatusCode::FORBIDDEN,
                "FORBIDDEN: non-loopback Host header rejected",
            )
                .into_response();
        }
    }

    if valid_session_cookie(headers) || valid_bearer_token(headers) {
        return next.run(request).await;
    }

    let peer_addr = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0);

    let mut issue_cookie = false;
    if let Some(peer) = peer_addr {
        if server_addr.ip().is_loopback() && !peer.ip().is_loopback() {
            return (
                StatusCode::FORBIDDEN,
                "FORBIDDEN: non-loopback peer on loopback service",
            )
                .into_response();
        }
        #[cfg(target_os = "linux")]
        {
            match verify_linux_peer_not_sandboxed(peer, server_addr) {
                PeerVerdict::TrustedLocal => issue_cookie = true,
                PeerVerdict::RemotePeer if !server_addr.ip().is_loopback() && !peer.ip().is_loopback() => {}
                PeerVerdict::RemotePeer | PeerVerdict::Rejected => {
                    return (
                        StatusCode::FORBIDDEN,
                        "FORBIDDEN: unverified local process cannot access host control API",
                    )
                        .into_response();
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            if peer.ip().is_loopback() {
                issue_cookie = true;
            }
        }
    }

    let mut response = next.run(request).await;
    if issue_cookie {
        let cookie = format!(
            "grow_session={}; Path=/; HttpOnly; SameSite=Strict",
            session_secret()
        );
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }
    response
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PeerVerdict {
    TrustedLocal,
    RemotePeer,
    Rejected,
}

#[cfg(target_os = "linux")]
fn verify_linux_peer_not_sandboxed(peer: SocketAddr, server: SocketAddr) -> PeerVerdict {
    use std::os::unix::fs::MetadataExt;
    let Some((inode, socket_uid)) = find_client_socket(peer, server) else {
        return if peer.ip().is_loopback() {
            PeerVerdict::Rejected
        } else {
            PeerVerdict::RemotePeer
        };
    };
    if inode == 0 {
        return PeerVerdict::Rejected;
    }
    let Ok(self_meta) = std::fs::metadata("/proc/self") else {
        return PeerVerdict::Rejected;
    };
    let my_uid = self_meta.uid();
    if socket_uid != my_uid {
        return PeerVerdict::Rejected;
    }
    let target = PathBuf::from(format!("socket:[{inode}]"));
    if proc_dir_holds_socket(Path::new("/proc/self/fd"), &target) {
        return PeerVerdict::TrustedLocal;
    }
    let Ok(self_ns) = std::fs::read_link("/proc/self/ns/pid") else {
        return PeerVerdict::Rejected;
    };
    let self_pid = std::process::id();
    let self_exe = std::fs::read_link("/proc/self/exe").ok();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return PeerVerdict::Rejected;
    };
    let mut found_trusted_owner = false;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid_str) = name.to_str() else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if pid == self_pid {
            continue;
        }
        let proc_path = entry.path();
        let Ok(meta) = std::fs::metadata(&proc_path) else {
            continue;
        };
        if meta.uid() != my_uid {
            continue;
        }
        if !proc_dir_holds_socket(&proc_path.join("fd"), &target) {
            continue;
        }
        let Ok(pid_ns) = std::fs::read_link(proc_path.join("ns/pid")) else {
            return PeerVerdict::Rejected;
        };
        if std::fs::metadata(proc_path.join("stat")).is_err() {
            return PeerVerdict::Rejected;
        }
        if pid_ns != self_ns
            && is_descendant_of_agent(pid, self_pid, self_exe.as_deref())
        {
            return PeerVerdict::Rejected;
        }
        found_trusted_owner = true;
        if pid_ns == self_ns {
            break;
        }
    }
    if found_trusted_owner {
        PeerVerdict::TrustedLocal
    } else {
        PeerVerdict::Rejected
    }
}

#[cfg(target_os = "linux")]
fn proc_dir_holds_socket(fd_dir: &Path, target: &Path) -> bool {
    let Ok(fds) = std::fs::read_dir(fd_dir) else {
        return false;
    };
    for fd in fds.flatten() {
        if std::fs::read_link(fd.path()).ok().as_deref() == Some(target) {
            return true;
        }
    }
    false
}

#[cfg(target_os = "linux")]
fn is_descendant_of_agent(pid: u32, self_pid: u32, self_exe: Option<&Path>) -> bool {
    let mut cur = pid;
    for _ in 0..64 {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{cur}/stat")) else {
            // Ancestor vanished mid-check inside a foreign pidns: fail closed.
            return true;
        };
        let Some(end) = stat.rfind(')') else {
            return true;
        };
        let Some(ppid) = stat[end + 2..]
            .split_ascii_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u32>().ok())
        else {
            return true;
        };
        if ppid <= 1 || ppid == cur {
            return false;
        }
        if ppid == self_pid {
            return true;
        }
        if let Some(exe) = self_exe
            && std::fs::read_link(format!("/proc/{ppid}/exe"))
                .ok()
                .as_deref()
                == Some(exe)
        {
            return true;
        }
        cur = ppid;
    }
    false
}

#[cfg(target_os = "linux")]
fn find_client_socket(peer: SocketAddr, server: SocketAddr) -> Option<(u64, u32)> {
    let want_rem_port = format!(":{:04X}", server.port());
    if let SocketAddr::V4(v4) = peer {
        let want_local = format!(
            "{:08X}:{:04X}",
            u32::from_ne_bytes(v4.ip().octets()),
            v4.port()
        );
        if let Some(hit) = scan_proc_tcp("/proc/net/tcp", &want_local, &want_rem_port) {
            return Some(hit);
        }
        let mapped = v4.ip().to_ipv6_mapped();
        let want_local6 = format_proc_ipv6(mapped, v4.port());
        if let Some(hit) = scan_proc_tcp("/proc/net/tcp6", &want_local6, &want_rem_port) {
            return Some(hit);
        }
    } else if let SocketAddr::V6(v6) = peer {
        let want_local6 = format_proc_ipv6(*v6.ip(), v6.port());
        if let Some(hit) = scan_proc_tcp("/proc/net/tcp6", &want_local6, &want_rem_port) {
            return Some(hit);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn format_proc_ipv6(ip: Ipv6Addr, port: u16) -> String {
    let o = ip.octets();
    format!(
        "{:08X}{:08X}{:08X}{:08X}:{:04X}",
        u32::from_ne_bytes([o[0], o[1], o[2], o[3]]),
        u32::from_ne_bytes([o[4], o[5], o[6], o[7]]),
        u32::from_ne_bytes([o[8], o[9], o[10], o[11]]),
        u32::from_ne_bytes([o[12], o[13], o[14], o[15]]),
        port
    )
}

#[cfg(target_os = "linux")]
fn scan_proc_tcp(path: &str, want_local: &str, want_rem_port: &str) -> Option<(u64, u32)> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines().skip(1) {
        let mut cols = line.split_ascii_whitespace();
        let _slot = cols.next()?;
        let local = cols.next()?;
        if !local.eq_ignore_ascii_case(want_local) {
            continue;
        }
        let rem = cols.next()?;
        if !rem
            .get(rem.len().saturating_sub(want_rem_port.len())..)
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case(want_rem_port))
            || local.eq_ignore_ascii_case(rem)
        {
            continue;
        }
        let _st = cols.next()?;
        let _queues = cols.next()?;
        let _tr = cols.next()?;
        let _retrnsmt = cols.next()?;
        let uid = cols.next()?.parse::<u32>().ok()?;
        let _timeout = cols.next()?;
        let inode = cols.next()?.parse::<u64>().ok()?;
        if inode > 0 {
            return Some((inode, uid));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_naming_a_secret_file_are_refused() {
        let secret = crate::server::ai_proxy::secret_files().remove(0);
        let path = secret.to_string_lossy().into_owned();
        let dir = secret.parent().unwrap().to_string_lossy().into_owned();
        let name = secret.file_name().unwrap().to_string_lossy().into_owned();
        for command in [format!("cat {path}"), format!("cd '{dir}' && head -5 {name}")] {
            let error = check_secret_reference(&command).unwrap_err().to_string();
            assert!(error.starts_with("SECRET_FILE"), "{command}: {error}");
        }
        if let Some(relative) = std::env::var_os("HOME")
            .map(PathBuf::from)
            .and_then(|home| secret.strip_prefix(home).ok().map(Path::to_path_buf))
        {
            let command = format!("base64 ~/{}", relative.display());
            assert!(check_secret_reference(&command).is_err(), "{command}");
        }
        // The file name alone, or a project's own .env, is someone else's file.
        for command in [
            format!("cat {name}"),
            "cat .env".to_owned(),
            "ls ~/.config".to_owned(),
        ] {
            assert!(check_secret_reference(&command).is_ok(), "{command}");
        }
    }

    #[test]
    fn direct_ssh_provider_and_state_paths_are_refused() {
        let providers = crate::server::ai_proxy::config_path();
        for command in [
            "cat ~/.ssh/id_ed25519".to_owned(),
            r"Get-Content $HOME\.ssh\config".to_owned(),
            "sqlite3 .codex-workspace-mcp/codex_state.db-wal".to_owned(),
            format!("cat '{}'", providers.display()),
        ] {
            assert!(check_secret_reference(&command).unwrap_err().to_string().starts_with("SECRET_FILE"));
        }
        assert!(check_secret_reference("cat providers.yml.example").is_ok());
        assert!(check_secret_reference("cat .ssh-notes").is_ok());
    }

    #[test]
    fn permission_modes_select_the_same_entry_policy_on_every_platform() {
        use crate::server::agent_service::PermissionMode::*;
        for (mode, boundary) in [
            (ReadOnly, ExecutionBoundary::ReadOnly),
            (WorkspaceWrite, ExecutionBoundary::Workspace),
            (Ask, ExecutionBoundary::Workspace),
            (AutoReview, ExecutionBoundary::Workspace),
            (FullAccess, ExecutionBoundary::Host),
        ] {
            assert_eq!(ExecutionBoundary::for_mode(mode), boundary);
        }
    }

    #[tokio::test]
    async fn local_http_guard_rejects_cross_site_and_rebinding_and_blocks_control_urls() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new()
            .route("/ping", axum::routing::get(|| async { "pong" }))
            .layer(axum::middleware::from_fn_with_state(addr, local_http_guard));
        let server = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await;
        });

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let ok_resp = client
            .get(format!("http://{addr}/ping"))
            .send()
            .await
            .unwrap();
        assert_eq!(ok_resp.status(), StatusCode::OK);
        assert!(
            ok_resp
                .headers()
                .get(header::SET_COOKIE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|c| c.starts_with("grow_session="))
        );

        let csrf_resp = client
            .get(format!("http://{addr}/ping"))
            .header("Origin", "https://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(csrf_resp.status(), StatusCode::FORBIDDEN);

        let rebind_resp = client
            .get(format!("http://{addr}/ping"))
            .header("Host", "rebind.evil.example:3001")
            .send()
            .await
            .unwrap();
        assert_eq!(rebind_resp.status(), StatusCode::FORBIDDEN);

        register_control_port(addr.port());
        assert!(is_host_control_port(addr.port()));
        for url in [format!("http://{addr}/ping"), format!("ws://localhost:{}/", addr.port())] {
            assert!(check_not_host_control_url(&url).unwrap_err().to_string().contains("CONTROL_PORT_BLOCKED"));
            assert!(check_not_host_control_url_async(&url).await.is_err());
        }
        assert!(check_not_host_control_url(&format!("https://example.invalid:{}/", addr.port())).is_ok());
        server.abort();
    }

    #[test]
    fn host_boundary_strips_parent_agent_internal_envs() {
        let work = std::env::temp_dir().join(format!(
            "grow-env-strip-test-{}-{}",
            std::process::id(),
            crate::server::agent_service::uuid_like()
        ));
        std::fs::create_dir_all(&work).unwrap();
        let cmd = command("sh", &work, &work, ExecutionBoundary::Workspace).unwrap();
        assert_eq!(cmd.as_std().get_program(), "sh");
        assert_eq!(cmd.as_std().get_current_dir(), Some(work.as_path()));
        let removed: Vec<&str> = cmd
            .as_std()
            .get_envs()
            .filter_map(|(k, v)| v.is_none().then(|| k.to_str()).flatten())
            .collect();
        let _ = std::fs::remove_dir_all(&work);
        for key in HOST_AGENT_INTERNAL_ENVS {
            assert!(
                removed.contains(key),
                "expected {key} to be stripped from child command, got {removed:?}"
            );
        }
    }
}
