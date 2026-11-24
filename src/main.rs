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
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_timer(ChinaTime).init();
    match ai_proxy::import_dsh_config(&ai_proxy::config_dir()) {
        Ok(Some(from)) => info!(from = %from.display(), to = %ai_proxy::config_dir().display(), "imported the DSH configuration"),
        Ok(None) => {}
        Err(error) => tracing::warn!("could not import the DSH configuration: {error:#}"),
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
