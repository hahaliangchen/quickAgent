#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{mpsc, Mutex},
    time::{Duration, Instant},
};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

struct Agent(Mutex<Child>);

impl Drop for Agent {
    fn drop(&mut self) {
        if let Ok(child) = self.0.get_mut() {
            // Closing the pipe lets the backend exit even if the shell crashes.
            drop(child.stdin.take());
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn agent_binary() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let name = if cfg!(windows) {
        "grow-agent.exe"
    } else {
        "grow-agent"
    };
    let beside = std::env::current_exe()?
        .parent()
        .ok_or("missing executable directory")?
        .join(name);
    if beside.is_file() {
        return Ok(beside);
    }
    // `cargo run` uses the target-suffixed sidecar prepared by the build script.
    let target = option_env!("GROW_BUILD_TARGET").unwrap_or("");
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let local = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join(format!("grow-agent-{target}{suffix}"));
    if local.is_file() {
        Ok(local)
    } else {
        Err(format!("找不到后端程序：{}", local.display()).into())
    }
}

// WebView storage is scoped to the origin. Keep the chosen port across restarts
// so preferences survive; if occupied, fail rather than attach to another app.
fn desktop_port(data: &std::path::Path) -> Result<u16, Box<dyn std::error::Error>> {
    let path = data.join("port");
    if path.exists() {
        let port: u16 = std::fs::read_to_string(path)?.trim().parse()?;
        if port == 0 {
            return Err("桌面端口无效".into());
        }
        return Ok(port);
    }
    let socket = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = socket.local_addr()?.port();
    std::fs::write(path, port.to_string())?;
    Ok(port)
}

fn main() {
    tauri::Builder::default()
        // The agent page asks the OS for a project folder through this plugin;
        // `capabilities/agent-page.json` limits it to that one command.
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let resources = if cfg!(debug_assertions) {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .unwrap()
                    .to_path_buf()
            } else {
                app.path().resource_dir()?
            };
            let data = app.path().app_data_dir()?;
            let workspace = data.join("workspace");
            std::fs::create_dir_all(&workspace)?;
            let port = desktop_port(&data)?;
            let mut command = Command::new(agent_binary()?);
            command
                .arg("--desktop-agent")
                .env("WORKSPACE_ROOT", &workspace)
                .env("AGENT_WEB_DIR", resources.join("frontend/dist"))
                .env("GROW_RESOURCES_DIR", &resources)
                .env("GROW_DESKTOP_BIND", format!("127.0.0.1:{port}"))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000); // CREATE_NO_WINDOW
            }
            let mut child = command.spawn()?;
            let output = child.stdout.take().ok_or("后端输出不可用")?;
            let (tx, rx) = mpsc::sync_channel(1);
            std::thread::spawn(move || {
                let mut tx = Some(tx);
                for line in BufReader::new(output).lines().map_while(Result::ok) {
                    if let Some(value) = line.strip_prefix("GROW_DESKTOP_PORT=") {
                        if let (Some(sender), Ok(port)) = (tx.take(), value.parse::<u16>()) {
                            let _ = sender.send(port);
                        }
                    }
                }
            });
            // Manage the child before any fallible startup work so failure cleans it up.
            app.manage(Agent(Mutex::new(child)));
            let port = rx
                .recv_timeout(Duration::from_secs(30))
                .map_err(|_| "Agent 后端未能启动")?;
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err("Agent 服务启动超时".into());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let url = format!("http://127.0.0.1:{port}/agent/").parse()?;
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title("Grow")
                .inner_size(1440.0, 900.0)
                .min_inner_size(800.0, 600.0)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Grow 桌面端启动失败");
}
