#![recursion_limit = "512"]

mod index;
mod media;
mod model;
mod context;
mod tools;
mod runtime;
mod server;

use std::{env, fmt as std_fmt, net::SocketAddr, path::PathBuf, sync::Arc};

use axum::Router;
use tokio::net::TcpListener;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::{error, info, warn};
use tracing_subscriber::fmt::{format::Writer, time::FormatTime};

use crate::{
    server::ai_proxy,
    tools::process_sandbox,
    server::mcp::{handle_mcp, handle_mcp_delete, handle_mcp_get},
    tools::Workspace,
};

struct ChinaTime;

impl FormatTime for ChinaTime {
    fn format_time(&self, writer: &mut Writer<'_>) -> std_fmt::Result {
        let offset = chrono::FixedOffset::east_opt(8 * 60 * 60).expect("valid China time offset");
        let now = chrono::Utc::now().with_timezone(&offset);
        write!(writer, "{}", now.format("%Y-%m-%dT%H:%M:%S%.6f%:z"))
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

fn build_app(workspace: Arc<Workspace>) -> Router {
    use axum::routing::get;

    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route(
            "/mcp",
            get(handle_mcp_get)
                .post(handle_mcp)
                .delete(handle_mcp_delete),
        )
        .with_state(workspace)
        .layer(TraceLayer::new_for_http())
        .layer(
            CorsLayer::permissive()
                .expose_headers([axum::http::HeaderName::from_static("mcp-session-id")]),
        )
}

async fn run_server(listener: TcpListener, workspace: Arc<Workspace>) -> anyhow::Result<()> {
    let addr = listener.local_addr()?;
    process_sandbox::register_control_port(addr.port());
    info!(%addr, root = %workspace.root().display(), "starting HTTP server");
    let app = build_app(workspace).layer(axum::middleware::from_fn_with_state(
        addr,
        process_sandbox::local_http_guard,
    ));
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Isolated verification mode (--isolated)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct IsolatedOptions {
    root: PathBuf,
    cleanup_on_exit: bool,
    workspace_dir: PathBuf,
    config_dir: PathBuf,
    daily_report_dir: PathBuf,
    web_dir: Option<PathBuf>,
    resources_dir: Option<PathBuf>,
    bind: SocketAddr,
    copy_providers: bool,
}

fn parse_isolated_options(args: &[String], cwd: &std::path::Path) -> anyhow::Result<Option<IsolatedOptions>> {
    if !args.iter().any(|arg| arg == "--isolated") {
        return Ok(None);
    }
    let mut explicit_dir: Option<PathBuf> = None;
    let mut web_dir: Option<PathBuf> = None;
    let mut bind_raw = String::from("127.0.0.1:0");
    let mut copy_providers = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--isolated-dir" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--isolated-dir requires a directory path"))?;
                explicit_dir = Some(PathBuf::from(value));
            }
            "--web-dir" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--web-dir requires a directory path"))?;
                web_dir = Some(PathBuf::from(value));
            }
            "--bind" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--bind requires a loopback ip:port"))?;
                bind_raw = value.clone();
            }
            "--copy-providers" => copy_providers = true,
            _ => {}
        }
        i += 1;
    }
    let bind: SocketAddr = bind_raw
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid --bind address `{bind_raw}`: {e}"))?;
    anyhow::ensure!(
        bind.ip().is_loopback(),
        "--isolated mode only allows loopback --bind addresses (got {bind})"
    );
    let (root, cleanup_on_exit) = match explicit_dir {
        Some(dir) => (dir, false),
        None => {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            (
                env::temp_dir().join(format!(
                    "quickagent-isolated-{}-{stamp}",
                    std::process::id()
                )),
                true,
            )
        }
    };
    let web_dir = web_dir.or_else(|| {
        let local = cwd.join("frontend/dist");
        local.join("index.html").is_file().then_some(local)
    });
    let resources_dir = cwd
        .join("src/server/builtin_plugins")
        .is_dir()
        .then(|| cwd.to_path_buf());
    Ok(Some(IsolatedOptions {
        workspace_dir: root.join("workspace"),
        config_dir: root.join("config"),
        daily_report_dir: root.join("daily-report"),
        root,
        cleanup_on_exit,
        web_dir,
        resources_dir,
        bind,
        copy_providers,
    }))
}

fn prepare_isolated_dirs(
    opts: &IsolatedOptions,
    source_config_dir: &std::path::Path,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&opts.workspace_dir)?;
    std::fs::create_dir_all(&opts.config_dir)?;
    std::fs::create_dir_all(&opts.daily_report_dir)?;
    if opts.copy_providers && source_config_dir != opts.config_dir {
        for file in ["providers.yml", "credentials.yaml", ".env", "settings.json"] {
            let src = source_config_dir.join(file);
            if src.is_file() {
                let dst = opts.config_dir.join(file);
                std::fs::copy(&src, &dst)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o600));
                }
            }
        }
    }
    Ok(())
}

struct IsolatedDirGuard {
    root: PathBuf,
    cleanup: bool,
}

impl Drop for IsolatedDirGuard {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();
    let cwd = env::current_dir()?;
    let isolated = parse_isolated_options(&args, &cwd)?;
    let _isolated_guard = if let Some(ref opts) = isolated {
        let source_config = ai_proxy::config_dir();
        prepare_isolated_dirs(opts, &source_config)?;
        // Apply isolated paths while strictly single-threaded before Tokio starts.
        unsafe {
            env::set_var("WORKSPACE_ROOT", &opts.workspace_dir);
            env::set_var("GROW_CONFIG_DIR", &opts.config_dir);
            env::set_var("DAILY_REPORT_DATA_DIR", &opts.daily_report_dir);
            if let Some(ref web) = opts.web_dir {
                env::set_var("AGENT_WEB_DIR", web);
            }
            if let Some(ref resources) = opts.resources_dir {
                env::set_var("GROW_RESOURCES_DIR", resources);
            }
        }
        Some(IsolatedDirGuard {
            root: opts.root.clone(),
            cleanup: opts.cleanup_on_exit,
        })
    } else {
        None
    };

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main(isolated))
}

async fn async_main(isolated: Option<IsolatedOptions>) -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_timer(ChinaTime).init();
    if isolated.is_none() {
        match ai_proxy::import_dsh_config(&ai_proxy::config_dir()) {
            Ok(Some(from)) => info!(from = %from.display(), to = %ai_proxy::config_dir().display(), "imported the DSH configuration"),
            Ok(None) => {}
            Err(error) => tracing::warn!("could not import the DSH configuration: {error:#}"),
        }
    }

    if env::args().any(|arg| arg == "--probe-image-input") {
        let result = ai_proxy::probe_default_image_input(&ai_proxy::config_path()).await?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    let workspace_root = env::var("WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or(env::current_dir()?);

    let workspace = Arc::new(Workspace::new(workspace_root)?);
    info!(root = %workspace.root().display(), "workspace initialized");

    // Start the local agent and settings UI even before a model is configured.
    let ai_config_path = ai_proxy::config_path();
    let ai_config_path = if ai_config_path.is_absolute() {
        ai_config_path
    } else {
        env::current_dir()?.join(ai_config_path)
    };
    if let Some(opts) = isolated {
        use std::io::Write;
        let listener = TcpListener::bind(opts.bind).await?;
        let bound = listener.local_addr()?;
        println!("GROW_ISOLATED_DIR={}", opts.root.display());
        println!("GROW_DESKTOP_PORT={}", bound.port());
        println!("GROW_READY_URL=http://{}:{}/agent/", bound.ip(), bound.port());
        std::io::stdout().flush()?;
        return ai_proxy::run(listener, &ai_config_path, workspace).await;
    }
    // The desktop shell owns this process and requires an exclusive loopback
    // listener. Report the bound address only after bind succeeds.
    if env::args().any(|arg| arg == "--desktop-agent") {
        use std::io::Write;
        let bind = env::var("GROW_DESKTOP_BIND").unwrap_or_else(|_| "127.0.0.1:0".to_owned());
        let addr: SocketAddr = bind.parse()?;
        anyhow::ensure!(addr.ip().is_loopback(), "desktop listener must use loopback");
        let listener = TcpListener::bind(addr).await?;
        println!("GROW_DESKTOP_PORT={}", listener.local_addr()?.port());
        std::io::stdout().flush()?;
        std::thread::spawn(|| {
            use std::io::Read;
            let mut input = std::io::stdin().lock();
            let mut buf = [0u8; 64];
            while let Ok(n) = input.read(&mut buf) {
                if n == 0 { break; }
            }
            std::process::exit(0);
        });
        return ai_proxy::run(listener, &ai_config_path, workspace).await;
    }
    {
        let agent_bind = env::var("AGENT_BIND").unwrap_or_else(|_| "127.0.0.1:41201".to_string());
        match TcpListener::bind(&agent_bind).await {
            Ok(listener) => {
                info!(path = %ai_config_path.display(), bind = %agent_bind, "Agent service starting");
                let workspace_for_proxy = workspace.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        ai_proxy::run(listener, &ai_config_path, workspace_for_proxy).await
                    {
                        error!(%e, "Agent service exited with error");
                    }
                });
            }
            Err(e) => warn!(%e, bind = %agent_bind, "could not start Agent service"),
        }
    }

    let bind = env::var("MCP_BIND").unwrap_or_else(|_| "127.0.0.1:41200".to_string());
    let addr: SocketAddr = bind.parse()?;
    let listener = TcpListener::bind(addr).await?;
    info!(%addr, mode = "http", "listening");

    run_server(listener, workspace).await
}

#[cfg(test)]
mod isolated_mode_tests {
    use super::*;

    #[test]
    fn isolated_options_enforce_loopback_and_separate_subdirectories() {
        let cwd = env::temp_dir().join(format!("qa-iso-cwd-{}", std::process::id()));
        std::fs::create_dir_all(cwd.join("frontend/dist")).unwrap();
        std::fs::write(cwd.join("frontend/dist/index.html"), "<html></html>").unwrap();
        std::fs::create_dir_all(cwd.join("src/server/builtin_plugins")).unwrap();

        let args: Vec<String> = ["codex-workspace-mcp", "--isolated", "--copy-providers"]
            .into_iter()
            .map(String::from)
            .collect();
        let opts = parse_isolated_options(&args, &cwd)
            .unwrap()
            .expect("isolated enabled");
        assert!(opts.cleanup_on_exit);
        assert!(opts.copy_providers);
        assert_eq!(opts.bind.ip().to_string(), "127.0.0.1");
        assert_eq!(opts.bind.port(), 0);
        assert_eq!(opts.web_dir.as_deref(), Some(cwd.join("frontend/dist").as_path()));
        assert_eq!(opts.resources_dir.as_deref(), Some(cwd.as_path()));

        let non_loopback: Vec<String> = [
            "codex-workspace-mcp",
            "--isolated",
            "--bind",
            "0.0.0.0:41299",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert!(parse_isolated_options(&non_loopback, &cwd).is_err());

        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn prepare_isolated_dirs_copies_only_when_requested_and_leaves_source_untouched() {
        let base = env::temp_dir().join(format!("qa-iso-copy-{}", std::process::id()));
        let src_cfg = base.join("real-cfg");
        let iso_root = base.join("iso");
        std::fs::create_dir_all(&src_cfg).unwrap();
        std::fs::write(src_cfg.join("providers.yml"), "providers: []\n").unwrap();
        std::fs::write(src_cfg.join("credentials.yaml"), "key: secret\n").unwrap();

        let opts = IsolatedOptions {
            workspace_dir: iso_root.join("workspace"),
            config_dir: iso_root.join("config"),
            daily_report_dir: iso_root.join("daily-report"),
            root: iso_root.clone(),
            cleanup_on_exit: false,
            web_dir: None,
            resources_dir: None,
            bind: "127.0.0.1:0".parse().unwrap(),
            copy_providers: true,
        };
        prepare_isolated_dirs(&opts, &src_cfg).unwrap();
        assert!(opts.workspace_dir.is_dir());
        assert!(opts.daily_report_dir.is_dir());
        assert_eq!(
            std::fs::read_to_string(opts.config_dir.join("providers.yml")).unwrap(),
            "providers: []\n"
        );
        assert_eq!(
            std::fs::read_to_string(opts.config_dir.join("credentials.yaml")).unwrap(),
            "key: secret\n"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
