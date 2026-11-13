//! Linux process file-write confinement, credential masking, and local control-plane guards.
use anyhow::{Context, Result, ensure};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    future::Future,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
use tokio::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionBoundary {
    Host,
    ReadOnly,
    Workspace,
}

impl ExecutionBoundary {
    pub fn for_mode(mode: crate::agent_service::PermissionMode) -> Self {
        use crate::agent_service::PermissionMode;
        if !cfg!(target_os = "linux") {
            return Self::Host;
        }
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
    let normalized = normalize_extra_roots(None, &extra_writable_roots);
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
    if !canon.is_dir() || canon == Path::new("/") || canon == Path::new("/tmp") {
        return false;
    }
    if let Some(home) = home_dir().and_then(|h| h.canonicalize().ok())
        && canon == home
    {
        return false;
    }
    true
}

fn normalize_extra_roots(primary_root: Option<&Path>, raw: &[PathBuf]) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for path in raw {
        let Ok(canon) = path.canonicalize() else {
            continue;
        };
        if !is_allowed_extra_root(&canon) {
            continue;
        }
        if primary_root.is_some_and(|root| canon.starts_with(root)) {
            continue;
        }
        if !candidates.contains(&canon) {
            candidates.push(canon);
        }
    }
    candidates.sort();
    let mut collapsed: Vec<PathBuf> = Vec::new();
    for dir in candidates {
        if primary_root.is_some_and(|root| root.starts_with(&dir) && root != dir) {
            // If an extra root is a strict ancestor of primary_root, binding extra
            // alone would replace primary_root; keep extra and let primary_root skip.
        }
        if !collapsed.iter().any(|existing| dir.starts_with(existing)) {
            collapsed.push(dir);
        }
    }
    collapsed
}

/// Wrap the actual argv, never a command string. A missing or broken runner
/// must fail the operation: no fallback to the unrestricted command.
pub fn command(
    program: impl AsRef<OsStr>,
    root: &Path,
    cwd: &Path,
    boundary: ExecutionBoundary,
) -> Result<Command> {
    let extras = current_extra_writable_roots();
    command_with_roots(program, root, &extras, cwd, boundary)
}

pub fn command_with_roots(
    program: impl AsRef<OsStr>,
    root: &Path,
    extra_writable_roots: &[PathBuf],
    cwd: &Path,
    boundary: ExecutionBoundary,
) -> Result<Command> {
    build(program, root, extra_writable_roots, cwd, boundary, false)
}

/// Set in every command the host sandboxes, so programs inside can tell.
pub const SANDBOX_MARKER: &str = "GROW_SANDBOXED";

/// Whether this process runs inside the host's command sandbox, where a
/// nested sandbox (bwrap, Chrome's own) cannot create its namespaces.
pub fn inside_sandbox() -> bool {
    std::env::var_os(SANDBOX_MARKER).is_some()
}

/// The main AI's own commands: the whole filesystem read-only, credentials
/// masked and no network, so a command cannot change the project's files or
/// anything elsewhere. `outputs` are the project's build-output folders (what
/// git ignores), writable so builds and tests can run; `/tmp` is private and
/// discarded. There is no unsandboxed fallback.
pub fn read_only_offline_command(program: impl AsRef<OsStr>, root: &Path, cwd: &Path, outputs: &[PathBuf]) -> Result<Command> {
    build_with_outputs(program, root, &[], cwd, ExecutionBoundary::ReadOnly, true, outputs)
}

fn build(
    program: impl AsRef<OsStr>,
    root: &Path,
    extra_writable_roots: &[PathBuf],
    cwd: &Path,
    boundary: ExecutionBoundary,
    offline: bool,
) -> Result<Command> {
    build_with_outputs(program, root, extra_writable_roots, cwd, boundary, offline, &[])
}

fn build_with_outputs(
    program: impl AsRef<OsStr>,
    root: &Path,
    extra_writable_roots: &[PathBuf],
    cwd: &Path,
    boundary: ExecutionBoundary,
    offline: bool,
    outputs: &[PathBuf],
) -> Result<Command> {
    if boundary == ExecutionBoundary::Host {
        return Ok(Command::new(program));
    }
    ensure!(
        cfg!(target_os = "linux"),
        "SANDBOX_UNAVAILABLE: workspace sandbox is only implemented on Linux"
    );
    let root = root
        .canonicalize()
        .context("SANDBOX_UNAVAILABLE: workspace does not exist")?;
    ensure!(
        root.is_dir() && root != Path::new("/"),
        "SANDBOX_UNAVAILABLE: refusing to grant the filesystem root"
    );
    let extras = normalize_extra_roots(Some(&root), extra_writable_roots);
    let cwd = cwd
        .canonicalize()
        .context("SANDBOX_UNAVAILABLE: working directory does not exist")?;
    ensure!(
        cwd.is_dir()
            && (cwd.starts_with(&root) || extras.iter().any(|extra| cwd.starts_with(extra))),
        "SANDBOX_UNAVAILABLE: working directory is outside the workspace"
    );
    let runner = find_bwrap(&root, &extras).context(
        "SANDBOX_UNAVAILABLE: install bubblewrap (bwrap) outside the workspace before using workspace execution",
    )?;
    let mut cmd = Command::new(runner);
    cmd.args([
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--unshare-pid",
        "--proc",
        "/proc",
        "--die-with-parent",
    ]);
    let mut bound_roots = vec![root.clone()];
    if offline {
        cmd.arg("--unshare-net");
        // A private /tmp for compilers and test runs; a workspace that lives
        // under /tmp is mounted again, still read-only.
        cmd.args(["--tmpfs", "/tmp"]);
        if let Ok(relative) = root.strip_prefix("/tmp") {
            let mut parent = PathBuf::from("/tmp");
            for component in relative.parent().into_iter().flat_map(Path::components) {
                parent.push(component.as_os_str());
                cmd.arg("--dir").arg(&parent);
            }
            cmd.arg("--ro-bind").arg(&root).arg(&root);
        }
        for output in outputs {
            if let Ok(output) = output.canonicalize()
                && output.is_dir()
                && output.starts_with(&root)
                && output != root
            {
                cmd.arg("--bind").arg(&output).arg(&output);
                bound_roots.push(output);
            }
        }
    }
    if boundary == ExecutionBoundary::Workspace {
        // Keep a private writable /tmp without hiding workspaces nested under /tmp.
        cmd.args(["--tmpfs", "/tmp"]);
        let mut writable_mounts = Vec::with_capacity(1 + extras.len());
        if !extras.iter().any(|extra| root.starts_with(extra)) {
            writable_mounts.push(root.clone());
        }
        writable_mounts.extend(extras.iter().cloned());
        for dir in &writable_mounts {
            bind_writable_dir(&mut cmd, dir);
        }
        bound_roots = writable_mounts;
    }
    // ReadOnly keeps both the workspace and /tmp on the read-only root mount.
    // Sensitive directories and files are masked last so a workspace or extra
    // mount cannot expose host credentials again.
    let dir_masks = secret_dir_masks(&bound_roots, boundary);
    for dir in &dir_masks {
        cmd.arg("--tmpfs").arg(dir);
        if !is_browser_profile(dir) {
            cmd.arg("--remount-ro").arg(dir);
        }
    }
    for secret in secret_file_masks(&dir_masks, &bound_roots, boundary) {
        cmd.arg("--ro-bind").arg("/dev/null").arg(secret);
    }
    cmd.arg("--chdir").arg(&cwd).arg("--").arg(program);
    for (key, _) in std::env::vars_os() {
        if let Some(name) = key.to_str()
            && is_sensitive_env_name(name)
        {
            cmd.env_remove(&key);
        }
    }
    cmd.env(SANDBOX_MARKER, "1");
    cmd.env("TMPDIR", "/tmp")
        .env("TMP", "/tmp")
        .env("TEMP", "/tmp");
    Ok(cmd)
}

fn bind_writable_dir(cmd: &mut Command, dir: &Path) {
    if dir == Path::new("/tmp") {
        return;
    }
    if let Ok(relative) = dir.strip_prefix("/tmp") {
        let mut parent = PathBuf::from("/tmp");
        if let Some(parts) = relative.parent() {
            for component in parts.components() {
                parent.push(component.as_os_str());
                cmd.arg("--dir").arg(&parent);
            }
        }
    }
    cmd.arg("--bind").arg(dir).arg(dir);
}

fn is_sensitive_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "SSH_AUTH_SOCK"
            | "GPG_AGENT_INFO"
            | "DBUS_SESSION_BUS_ADDRESS"
            | "GIT_ASKPASS"
            | "SSH_ASKPASS"
            | "SUDO_ASKPASS"
    ) {
        return true;
    }
    if matches!(upper.as_str(), "LESSKEY" | "XKEYSYMDB") {
        return false;
    }
    if upper.ends_with("_AUTH") || upper.ends_with("_PAT") {
        return true;
    }
    upper.split('_').any(|part| {
        matches!(
            part,
            "KEY"
                | "KEYS"
                | "APIKEY"
                | "APIKEYS"
                | "TOKEN"
                | "TOKENS"
                | "SECRET"
                | "SECRETS"
                | "PASSWORD"
                | "PASSWD"
                | "CREDENTIAL"
                | "CREDENTIALS"
                | "PRIVATE"
                | "COOKIE"
                | "COOKIES"
                | "BEARER"
        )
    })
}

fn visible_in_sandbox(path: &Path, bound_roots: &[PathBuf], boundary: ExecutionBoundary) -> bool {
    if boundary == ExecutionBoundary::Workspace && path.starts_with("/tmp") {
        return bound_roots.iter().any(|root| path.starts_with(root));
    }
    true
}

/// A browser's own folder under `$HOME`. It is hidden like the other
/// credential folders, but its empty stand-in stays writable: Chrome's crash
/// reporter must create its database there, and where it could not Chrome
/// died at startup (2026-11-13: every browser test run in the sandbox ended
/// in SIGTRAP after `chrome_crashpad_handler: --database is required`). What
/// is written lands in the sandbox's own tmpfs and is discarded with it.
const BROWSER_PROFILES: [&str; 3] = [".mozilla", ".config/google-chrome", ".config/chromium"];
fn is_browser_profile(dir: &Path) -> bool {
    home_dir().is_some_and(|home| {
        BROWSER_PROFILES.iter().any(|rel| home.join(rel).canonicalize().is_ok_and(|profile| profile == dir))
    })
}

/// Sensitive `$HOME` credential directories replaced with an empty tmpfs,
/// read-only except for browser profiles.
fn secret_dir_masks(bound_roots: &[PathBuf], boundary: ExecutionBoundary) -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let candidates = [
        ".ssh",
        ".gnupg",
        ".aws",
        ".kube",
        ".config/gh",
        ".config/gcloud",
        ".mozilla",
        ".config/google-chrome",
        ".config/chromium",
        ".pki",
    ];
    let mut masks = Vec::new();
    for rel in candidates {
        let Ok(canon) = home.join(rel).canonicalize() else {
            continue;
        };
        if !canon.is_dir() {
            continue;
        }
        if bound_roots.iter().any(|root| root.starts_with(&canon)) {
            continue;
        }
        if !visible_in_sandbox(&canon, bound_roots, boundary) {
            continue;
        }
        if !masks.iter().any(|existing: &PathBuf| canon.starts_with(existing)) {
            masks.push(canon);
        }
    }
    masks
}

/// The secret files that exist, as the sandbox sees them: each one is
/// replaced by an empty read-only file, so a command reads nothing.
fn secret_file_masks(
    masked_dirs: &[PathBuf],
    bound_roots: &[PathBuf],
    boundary: ExecutionBoundary,
) -> Vec<PathBuf> {
    let mut raw = crate::ai_proxy::secret_files();
    if let Some(home) = home_dir() {
        raw.extend(
            [
                ".git-credentials",
                ".netrc",
                ".npmrc",
                ".pypirc",
                ".cargo/credentials",
                ".cargo/credentials.toml",
                ".docker/config.json",
            ]
            .into_iter()
            .map(|rel| home.join(rel)),
        );
    }
    let mut masks: Vec<PathBuf> = Vec::new();
    for path in raw {
        let Ok(canon) = path.canonicalize() else {
            continue;
        };
        if !canon.is_file() {
            continue;
        }
        if masked_dirs.iter().any(|dir| canon.starts_with(dir)) {
            continue;
        }
        if !visible_in_sandbox(&canon, bound_roots, boundary) {
            continue;
        }
        if !masks.contains(&canon) {
            masks.push(canon);
        }
    }
    masks
}

/// Commands that name a secret file are refused before they start. The
/// sandbox masks the files anyway; this covers execution outside it (full
/// access, other platforms) and tells the AI why instead of handing it an
/// empty file. A name alone is not enough: `.env` is common in projects, so
/// the file's own directory must be named too.
pub fn check_secret_reference(command: &str) -> Result<()> {
    let home = home_dir();
    let normalized = command.replace('\\', "/");
    for secret in crate::ai_proxy::secret_files() {
        let (Some(dir), Some(name)) = (secret.parent(), secret.file_name().and_then(OsStr::to_str))
        else {
            continue;
        };
        if !normalized.contains(name) {
            continue;
        }
        let mut spellings = vec![dir.to_string_lossy().replace('\\', "/")];
        if let Ok(canonical) = dir.canonicalize() {
            spellings.push(canonical.to_string_lossy().replace('\\', "/"));
        }
        if let Some(relative) = home.as_deref().and_then(|home| dir.strip_prefix(home).ok()) {
            let relative = relative.to_string_lossy().replace('\\', "/");
            if !relative.is_empty() {
                spellings.push(relative);
            }
        }
        if spellings
            .iter()
            .any(|dir| !dir.is_empty() && normalized.contains(dir.as_str()))
        {
            anyhow::bail!(
                "SECRET_FILE: the command names {}, which holds API keys; commands never read it. The host applies credentials itself through its libraries (web, model calls). Model settings are in providers.yml and agent-generation.yml beside it",
                secret.display()
            );
        }
    }
    Ok(())
}

/// Whether a sandbox runner is installed at all; a kernel that refuses its
/// namespaces still fails the command with SANDBOX_UNAVAILABLE.
pub fn bwrap_installed() -> bool {
    cfg!(target_os = "linux") && find_bwrap(Path::new("/nonexistent-workspace"), &[]).is_some()
}

fn find_bwrap(root: &Path, extras: &[PathBuf]) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("bwrap"))
        .filter_map(|path| path.canonicalize().ok())
        .find(|path| {
            path.is_file()
                && !path.starts_with(root)
                && !extras.iter().any(|extra| path.starts_with(extra))
        })
}

// ---------------------------------------------------------------------------
// Host control-plane port registry & HTTP sandbox-escape / CSRF guard
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
    let token_path = crate::ai_proxy::config_dir().join("local_api_token");
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
/// 3. Loopback sandbox escapes from processes running inside `bwrap --unshare-pid`.
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
                        "FORBIDDEN: sandboxed or unverified local process cannot access host control API",
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
        let secret = crate::ai_proxy::secret_files().remove(0);
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
    fn permission_modes_select_execution_boundary() {
        use crate::agent_service::PermissionMode;
        for (mode, linux_boundary) in [
            (PermissionMode::ReadOnly, ExecutionBoundary::ReadOnly),
            (PermissionMode::WorkspaceWrite, ExecutionBoundary::Workspace),
            (PermissionMode::Ask, ExecutionBoundary::Workspace),
            (PermissionMode::AutoReview, ExecutionBoundary::Workspace),
            (PermissionMode::FullAccess, ExecutionBoundary::Host),
        ] {
            assert_eq!(
                ExecutionBoundary::for_mode(mode),
                if cfg!(target_os = "linux") {
                    linux_boundary
                } else {
                    ExecutionBoundary::Host
                },
                "{mode:?}"
            );
        }
    }

    #[test]
    fn host_execution_keeps_argv_and_workspace_root_is_not_granted() {
        assert!(
            command(
                "node",
                Path::new("."),
                Path::new("."),
                ExecutionBoundary::Host
            )
            .is_ok()
        );
        #[cfg(target_os = "linux")]
        for boundary in [ExecutionBoundary::ReadOnly, ExecutionBoundary::Workspace] {
            assert!(
                format!(
                    "{:#}",
                    command("node", Path::new("/"), Path::new("/"), boundary).unwrap_err()
                )
                .contains("filesystem root")
            );
        }
    }

    /// 2026-11-13: Chrome started inside the read-only sandbox died with
    /// SIGTRAP, because its profile folder was replaced by an empty folder it
    /// could not write its crash database into.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_browser_profile_is_hidden_but_its_stand_in_can_be_written() {
        let work = std::env::temp_dir().join(format!("grow-browser-mask-{}", crate::agent_service::uuid_like()));
        std::fs::create_dir_all(&work).unwrap();
        let work = work.canonicalize().unwrap();
        let profile = home_dir().and_then(|home| {
            BROWSER_PROFILES.iter().find_map(|rel| home.join(rel).canonicalize().ok().filter(|dir| dir.is_dir()))
        });
        let (Some(profile), true) = (profile, find_bwrap(&work, &[]).is_some() && !inside_sandbox()) else {
            eprintln!("skipped: needs bwrap outside a sandbox and an existing browser profile folder");
            std::fs::remove_dir_all(work).unwrap();
            return;
        };
        assert!(is_browser_profile(&profile) && !is_browser_profile(&work));
        let marker = format!("grow-mask-probe-{}", crate::agent_service::uuid_like());
        let output = read_only_offline_command("/bin/sh", &work, &work, &[])
            .unwrap()
            .args(["-c", r#"test -z "$(ls -A "$1")" && mkdir "$1/$2" && test -d "$1/$2""#, "sh"])
            .arg(&profile)
            .arg(&marker)
            .output()
            .await
            .unwrap();
        let leaked = profile.join(&marker).exists();
        std::fs::remove_dir_all(work).unwrap();
        assert!(output.status.success(), "the stand-in is empty and writable: {}", String::from_utf8_lossy(&output.stderr));
        assert!(!leaked, "what the command wrote stayed inside the sandbox");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn readonly_keeps_root_readonly_and_preserves_process_isolation() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        if find_bwrap(&root, &[]).is_none() {
            eprintln!("skipped: bwrap is not installed outside the workspace");
            return;
        }
        let cmd = command("/bin/sh", &root, &root, ExecutionBoundary::ReadOnly).unwrap();
        let args: Vec<_> = cmd.as_std().get_args().collect();
        let mut expected: Vec<&OsStr> = [
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--unshare-pid",
            "--proc",
            "/proc",
            "--die-with-parent",
        ]
        .into_iter()
        .map(OsStr::new)
        .collect();
        let bound = vec![root.clone()];
        let dir_masks = secret_dir_masks(&bound, ExecutionBoundary::ReadOnly);
        for dir in &dir_masks {
            expected.extend([OsStr::new("--tmpfs"), dir.as_os_str()]);
            if !is_browser_profile(dir) {
                expected.extend([OsStr::new("--remount-ro"), dir.as_os_str()]);
            }
        }
        let file_masks = secret_file_masks(&dir_masks, &bound, ExecutionBoundary::ReadOnly);
        for mask in &file_masks {
            expected.extend([
                OsStr::new("--ro-bind"),
                OsStr::new("/dev/null"),
                mask.as_os_str(),
            ]);
        }
        expected.push(OsStr::new("--chdir"));
        expected.extend([root.as_os_str(), OsStr::new("--"), OsStr::new("/bin/sh")]);
        assert_eq!(args, expected);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn readonly_rejects_cwd_outside_workspace() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let error = command(
            "/bin/sh",
            &root,
            Path::new("/"),
            ExecutionBoundary::ReadOnly,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("working directory is outside the workspace"));
    }

    #[cfg(target_os = "linux")]
    fn bwrap_available() -> bool {
        use std::process::Stdio;
        let runner = match find_bwrap(Path::new("/nonexistent-workspace"), &[]) {
            Some(path) => path,
            None => {
                eprintln!("skipped: bwrap is not installed");
                return false;
            }
        };
        let probe = std::process::Command::new(runner)
            .args([
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--unshare-pid",
                "--proc",
                "/proc",
                "--die-with-parent",
                "--",
                "/bin/true",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !probe.is_ok_and(|result| result.success()) {
            eprintln!("skipped: bwrap cannot create its namespaces on this host");
            return false;
        }
        true
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn real_bwrap_readonly_allows_reads_and_blocks_workspace_tmp_and_child_writes() {
        if !bwrap_available() {
            return;
        }
        let base = std::env::temp_dir().join(format!(
            "grow-readonly-sandbox-test-{}-{}",
            std::process::id(),
            crate::agent_service::uuid_like()
        ));
        let work = base.join("work");
        let outside = base.join("outside");
        let mut tmp_name = base.file_name().unwrap().to_os_string();
        tmp_name.push(".tmp-write");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(work.join("existing"), "workspace").unwrap();
        std::fs::write(outside.join("existing"), "outside").unwrap();
        let output = command("/bin/sh", &work, &work, ExecutionBoundary::ReadOnly)
            .unwrap()
            .args([
                "-c",
                r#"
                set -eu
                test "$PWD" = "$1"
                test "$(cat existing)" = workspace
                test "$(cat "$2/existing")" = outside
                /bin/sh -c 'test "$(cat existing)" = workspace'
                for target in existing inside "$2/existing" "$2/escape" "/tmp/$3" "$TMPDIR/$3" "$TMP/$3" "$TEMP/$3"; do
                    if (printf bad > "$target") 2>/dev/null; then exit 42; fi
                    if /bin/sh -c 'printf child > "$1"' sh "$target" 2>/dev/null; then exit 43; fi
                done
                "#,
                "sh",
            ])
            .arg(work.canonicalize().unwrap())
            .arg(&outside)
            .arg(&tmp_name)
            .current_dir(&work)
            .output()
            .await
            .unwrap();
        let existing = std::fs::read_to_string(work.join("existing")).unwrap();
        let external = std::fs::read_to_string(outside.join("existing")).unwrap();
        let inside_created = work.join("inside").exists();
        let escape_created = outside.join("escape").exists();
        let tmp_created = Path::new("/tmp").join(&tmp_name).exists();
        std::fs::remove_dir_all(base).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(existing, "workspace");
        assert_eq!(external, "outside");
        assert!(!inside_created);
        assert!(!escape_created);
        assert!(!tmp_created);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn real_bwrap_preserves_workspace_and_blocks_external_writes_from_children() {
        if !bwrap_available() {
            return;
        }
        let base = std::env::temp_dir().join(format!(
            "grow-sandbox-test-{}-{}",
            std::process::id(),
            crate::agent_service::uuid_like()
        ));
        let work = base.join("work");
        let outside = base.join("outside");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let result = async {
            let mut cmd = command("/bin/sh", &work, &work, ExecutionBoundary::Workspace).unwrap();
            let output = cmd.args(["-c", "printf ok > inside; /bin/sh -c 'printf child > child'; if printf bad > \"$1/escape\" 2>/dev/null; then exit 42; fi; printf temp > /tmp/grow-temp-check", "sh"])
                .arg(&outside).current_dir(&work).output().await.unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            assert_eq!(std::fs::read_to_string(work.join("inside")).unwrap(), "ok");
            assert_eq!(std::fs::read_to_string(work.join("child")).unwrap(), "child");
            assert!(!outside.join("escape").exists());
        }.await;
        std::fs::remove_dir_all(base).unwrap();
        result
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn real_bwrap_allows_approved_extra_root_and_blocks_unapproved_dirs() {
        if !bwrap_available() {
            return;
        }
        let base = std::env::temp_dir().join(format!(
            "grow-extra-root-test-{}-{}",
            std::process::id(),
            crate::agent_service::uuid_like()
        ));
        let work = base.join("work");
        let related = base.join("related");
        let forbidden = base.join("forbidden");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&related).unwrap();
        std::fs::create_dir_all(&forbidden).unwrap();
        let result = scope_tool_sandbox(
            ExecutionBoundary::Workspace,
            vec![related.clone()],
            async {
                let mut cmd =
                    command("/bin/sh", &work, &related, ExecutionBoundary::Workspace).unwrap();
                let output = cmd
                    .args([
                        "-c",
                        "set -eu; printf linked > shared.txt; printf main > \"$1/main.txt\"; if printf nope > \"$2/escape\" 2>/dev/null; then exit 42; fi",
                        "sh",
                    ])
                    .arg(work.canonicalize().unwrap())
                    .arg(forbidden.canonicalize().unwrap())
                    .current_dir(&related)
                    .output()
                    .await
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(
                    std::fs::read_to_string(related.join("shared.txt")).unwrap(),
                    "linked"
                );
                assert_eq!(
                    std::fs::read_to_string(work.join("main.txt")).unwrap(),
                    "main"
                );
                assert!(!forbidden.join("escape").exists());
            },
        )
        .await;
        std::fs::remove_dir_all(base).unwrap();
        result
    }

    #[test]
    fn sensitive_env_names_are_detected_without_breaking_runtime_vars() {
        for blocked in [
            "OPENAI_API_KEY",
            "DEEPSEEK_API_KEY",
            "GITHUB_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "SSH_AUTH_SOCK",
            "DBUS_SESSION_BUS_ADDRESS",
            "GIT_ASKPASS",
            "MY_SERVICE_PASSWORD",
        ] {
            assert!(is_sensitive_env_name(blocked), "{blocked}");
        }
        for allowed in [
            "PATH",
            "HOME",
            "LANG",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "NODE_PATH",
            "LESSKEY",
            "XDG_SESSION_TYPE",
        ] {
            assert!(!is_sensitive_env_name(allowed), "{allowed}");
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn local_http_guard_blocks_bwrap_escape_and_cross_site_while_allowing_host_client() {
        if !bwrap_available() {
            return;
        }
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

        let work = std::env::temp_dir().join(format!(
            "grow-http-guard-test-{}-{}",
            std::process::id(),
            crate::agent_service::uuid_like()
        ));
        std::fs::create_dir_all(&work).unwrap();
        let mut sandbox_cmd =
            command("python3", &work, &work, ExecutionBoundary::Workspace).unwrap();
        let output = sandbox_cmd
            .args([
                "-c",
                r#"
import sys, urllib.request, urllib.error
try:
    with urllib.request.urlopen(sys.argv[1], timeout=5) as r:
        print(r.status)
except urllib.error.HTTPError as e:
    print(e.code)
"#,
                &format!("http://{addr}/ping"),
            ])
            .current_dir(&work)
            .output()
            .await
            .unwrap();
        std::fs::remove_dir_all(&work).unwrap();
        server.abort();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "403");
    }
}
