#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{mpsc, Mutex},
    time::{Duration, Instant},
};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

struct Agent {
    child: Mutex<Child>,
    #[cfg(windows)]
    _job: Option<std::os::windows::io::OwnedHandle>,
}

#[cfg(windows)]
fn attach_kill_on_close_job(child: &Child) -> Option<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    unsafe {
        let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if handle.is_null() {
            return None;
        }
        let owned = OwnedHandle::from_raw_handle(handle);
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of_val(&info) as u32,
        ) == 0
        {
            return None;
        }
        if AssignProcessToJobObject(handle, child.as_raw_handle()) == 0 {
            return None;
        }
        Some(owned)
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        if let Ok(child) = self.child.get_mut() {
            // Closing the pipe lets the backend exit gracefully first so plugins shut down cleanly.
            drop(child.stdin.take());
            let _ = child.kill();
            let _ = child.wait();
        }
        // Dropping `_job` closes the Job Object handle, which on Windows atomically
        // terminates the entire process tree (grow-agent + all plugin local services).
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

#[cfg(target_os = "linux")]
fn sanitize_linux_desktop_env() {
    // Strip Snap confinement env vars inherited from Snap-packaged IDEs/terminals
    // so Ubuntu 26.04's `libglycin-2` does not misdetect Snap confinement and
    // abort inside `gly_loader_load` during GTK3 icon/image decoding.
    let snap_keys: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .filter(|k| k == "SNAP" || k.starts_with("SNAP_"))
        .collect();
    for key in snap_keys {
        std::env::remove_var(key);
    }
    // Prefer XDG Desktop Portal (e.g. KDE Portal) for native file/folder dialogs
    // so icon/thumbnail loading stays out-of-process instead of running inside
    // GTK3/tao's main thread.
    if std::env::var_os("GTK_USE_PORTAL").is_none() {
        std::env::set_var("GTK_USE_PORTAL", "1");
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    sanitize_linux_desktop_env();

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
            #[cfg(windows)]
            let job = attach_kill_on_close_job(&child);
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
            app.manage(Agent {
                child: Mutex::new(child),
                #[cfg(windows)]
                _job: job,
            });
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
            let mut builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title("QuickAgent")
                .inner_size(1440.0, 900.0)
                .min_inner_size(800.0, 600.0);
            if let Some(icon) = app.default_window_icon().cloned() {
                builder = builder.icon(icon)?;
            }
            builder.build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("QuickAgent 桌面端启动失败");
}
