//! Bounded, argv-only adapters for the local Linux desktop.
use super::*;
use std::{collections::BTreeSet, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

#[derive(Default)]
pub(super) struct Linux {
    portal: super::portal::Portal,
}

fn kde_wayland() -> bool {
    std::env::var_os("SWAYSOCK").is_none()
        && std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|names| {
            names
                .split(':')
                .any(|name| name.eq_ignore_ascii_case("KDE"))
        })
}
const MAX_IMAGE: usize = visual_artifacts::MAX_ORIGINAL_BYTES;
const MAX_TEXT: usize = 64 * 1024;
const MAX_LIST: usize = 1024 * 1024;

#[derive(Clone, Copy)]
enum Display {
    X11,
    Wayland,
}
fn display() -> Result<Display> {
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    if session == "wayland" || std::env::var_os("WAYLAND_DISPLAY").is_some() {
        Ok(Display::Wayland)
    } else if std::env::var_os("DISPLAY").is_some() {
        Ok(Display::X11)
    } else {
        Err(unavailable())
    }
}
fn missing(tool: &str) -> anyhow::Error {
    fault(
        "DEPENDENCY_MISSING",
        format!("Install {tool} and run Grow inside a supported local graphical session"),
    )
}
fn command(tool: &'static str) -> Command {
    let mut command = Command::new(tool);
    command
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}
fn spawn_error(tool: &str, error: std::io::Error) -> anyhow::Error {
    if error.kind() == std::io::ErrorKind::NotFound {
        missing(tool)
    } else {
        fault(
            "DESKTOP_BACKEND_FAILED",
            format!("could not start {tool}: {error}"),
        )
    }
}
async fn output(tool: &'static str, args: &[&str], max: usize) -> Result<Vec<u8>> {
    let mut child = command(tool)
        .args(args)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| spawn_error(tool, e))?;
    let stdout = child
        .stdout
        .take()
        .context("helper stdout is unavailable")?;
    tokio::time::timeout(Duration::from_secs(4), async {
        let mut data = Vec::new();
        stdout.take(max as u64 + 1).read_to_end(&mut data).await?;
        if data.len() > max {
            return Err(fault(
                "DESKTOP_OUTPUT_LIMIT",
                format!("{tool} exceeded its output budget"),
            ));
        }
        if !child.wait().await?.success() {
            return Err(fault(
                "DESKTOP_BACKEND_FAILED",
                format!("{tool} failed; verify that the helper can access the desktop"),
            ));
        }
        Ok(data)
    })
    .await
    .map_err(|_| fault("DESKTOP_TIMEOUT", format!("{tool} did not finish in time")))?
}
async fn query(tool: &'static str, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&output(tool, args, MAX_LIST).await?).into_owned())
}
async fn mutate(
    tool: &'static str,
    args: &[&str],
    input: Option<&str>,
    cancel: &CancellationToken,
) -> Result<()> {
    guard::cancelled(cancel)?;
    let mut child = command(tool)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| spawn_error(tool, e))?;
    tokio::time::timeout(Duration::from_secs(4), async {
        if let Some(data) = input {
            let mut stdin = child.stdin.take().context("helper stdin is unavailable")?;
            stdin.write_all(data.as_bytes()).await?;
        }
        if !child.wait().await?.success() {
            return Err(fault(
                "DESKTOP_BACKEND_FAILED",
                format!("{tool} failed; check the desktop session and helper permissions"),
            ));
        }
        Ok(())
    })
    .await
    .map_err(|_| fault("DESKTOP_TIMEOUT", format!("{tool} did not finish in time")))?
}
fn xid(value: &str) -> Option<u64> {
    let raw = value.trim();
    raw.strip_prefix("0x")
        .map_or_else(|| raw.parse().ok(), |hex| u64::from_str_radix(hex, 16).ok())
        .filter(|n| (1..=u64::from(u32::MAX)).contains(n))
}
fn trim(value: &str, limit: usize) -> String {
    value[..value.floor_char_boundary(limit)].to_owned()
}
fn split_columns(mut line: &str, count: usize) -> Option<(Vec<&str>, &str)> {
    let mut columns = Vec::with_capacity(count);
    for _ in 0..count {
        line = line.trim_start();
        if line.is_empty() {
            return None;
        }
        let end = line.find(char::is_whitespace).unwrap_or(line.len());
        columns.push(&line[..end]);
        line = &line[end..];
    }
    Some((columns, line.trim_start()))
}
fn wmctrl_windows(raw: &str, active: Option<u64>) -> Vec<Window> {
    raw.lines()
        .filter_map(|line| {
            let (cols, title) = split_columns(line, 9)?;
            let id = xid(cols[0])?;
            Some(Window {
                window_id: format!("0x{id:x}"),
                title: trim(title, 240),
                app_class: trim(cols[7], 240),
                x: cols[3].parse().ok()?,
                y: cols[4].parse().ok()?,
                width: cols[5].parse().ok()?,
                height: cols[6].parse().ok()?,
                active: Some(id) == active,
                visible: cols[1] != "-2",
                pid: cols[2].parse().ok().filter(|pid| *pid > 0),
            })
        })
        .take(200)
        .collect()
}
async fn x11_windows() -> Result<Vec<Window>> {
    let (active, list) = tokio::try_join!(
        query("xprop", &["-root", "_NET_ACTIVE_WINDOW"]),
        query("wmctrl", &["-lGpx"])
    )?;
    Ok(wmctrl_windows(
        &list,
        active.split_whitespace().last().and_then(xid),
    ))
}
async fn sway_command(args: &[&str]) -> Result<String> {
    if std::env::var_os("SWAYSOCK").is_none() {
        return Err(missing("Sway IPC (swaymsg) or an X11 session"));
    }
    query("swaymsg", args).await
}
fn sway_windows(root: &Value) -> Vec<Window> {
    fn collect(node: &Value, visible: bool, out: &mut Vec<Window>, depth: usize) {
        if depth > 80 || out.len() >= 200 || node["name"] == "__i3_scratch" {
            return;
        }
        let visible = node["visible"].as_bool().unwrap_or(visible);
        let class = node["app_id"]
            .as_str()
            .or_else(|| node["window_properties"]["class"].as_str());
        if let (Some(class), Some(id), Some(pid)) =
            (class, node["id"].as_u64(), node["pid"].as_u64())
        {
            let rect = &node["rect"];
            if let (Some(x), Some(y), Some(width), Some(height)) = (
                rect["x"].as_i64(),
                rect["y"].as_i64(),
                rect["width"].as_u64(),
                rect["height"].as_u64(),
            ) {
                out.push(Window {
                    window_id: format!("sway-{id}"),
                    title: trim(node["name"].as_str().unwrap_or(""), 240),
                    app_class: trim(class, 240),
                    x: x as i32,
                    y: y as i32,
                    width: width as u32,
                    height: height as u32,
                    active: node["focused"] == true,
                    visible: node["visible"].as_bool().unwrap_or(false) && visible,
                    pid: u32::try_from(pid).ok().filter(|p| *p > 0),
                });
            }
        }
        for child in node["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(node["floating_nodes"].as_array().into_iter().flatten())
        {
            collect(child, visible, out, depth + 1);
        }
    }
    let mut out = Vec::new();
    collect(root, false, &mut out, 0);
    out
}
async fn x11_live(window: &Window, input: bool) -> Result<Window> {
    let id = window.window_id.as_str();
    let live = guard::window(&x11_windows().await?, Some(id), input)?;
    if live.pid != window.pid || live.app_class != window.app_class {
        return Err(fault("WINDOW_CHANGED", "window identity changed"));
    }
    let info = query("xwininfo", &["-id", id, "-stats"]).await?;
    let prop = query("xprop", &["-id", id, "_NET_WM_STATE"]).await?;
    if !info
        .lines()
        .any(|line| line.trim() == "Map State: IsViewable")
        || prop.contains("_NET_WM_STATE_HIDDEN")
    {
        return Err(fault(
            "WINDOW_UNAVAILABLE",
            "window is minimized or not viewable",
        ));
    }
    let field = |name: &str| {
        info.lines()
            .find_map(|line| line.trim().strip_prefix(name)?.trim().parse::<i32>().ok())
    };
    let value = |name| field(name).context("cannot verify current window geometry");
    let real = (
        value("Absolute upper-left X:")?,
        value("Absolute upper-left Y:")?,
        value("Width:")?,
        value("Height:")?,
    );
    if input {
        let actual = query("xdotool", &["getwindowfocus"]).await?;
        if xid(actual.trim()) != xid(id) || live != *window {
            return Err(fault(
                "WINDOW_CHANGED",
                "target focus or geometry changed; refresh desktop.list_windows",
            ));
        }
    }
    Ok(Window {
        x: real.0,
        y: real.1,
        width: real.2.try_into()?,
        height: real.3.try_into()?,
        ..live
    })
}
async fn verify_sway(window: &Window) -> Result<()> {
    let tree: Value = serde_json::from_str(&sway_command(&["-t", "get_tree", "-r"]).await?)?;
    let current = guard::window(&sway_windows(&tree), Some(&window.window_id), true)?;
    if current != *window {
        return Err(fault(
            "WINDOW_CHANGED",
            "Sway focus or window identity changed",
        ));
    }
    // A shared session can have multiple seats. Only input to the verified default focus.
    let seats: Value = serde_json::from_str(&sway_command(&["-t", "get_seats", "-r"]).await?)?;
    let seat = seats
        .as_array()
        .filter(|all| all.len() == 1)
        .and_then(|all| all.first())
        .filter(|seat| seat["name"] == "seat0");
    if seat.and_then(|seat| seat["focus"].as_u64())
        != window
            .window_id
            .strip_prefix("sway-")
            .and_then(|id| id.parse().ok())
    {
        return Err(fault(
            "WINDOW_UNAVAILABLE",
            "cannot verify the Wayland input seat",
        ));
    }
    Ok(())
}
async fn x11_move(point: Point, cancel: &CancellationToken) -> Result<()> {
    mutate(
        "xdotool",
        &[
            "mousemove",
            "--",
            &point.x.to_string(),
            &point.y.to_string(),
        ],
        None,
        cancel,
    )
    .await
}
async fn x11_hover_is(window: &Window, point: Point) -> Result<()> {
    let live = x11_live(window, true).await?;
    let pointer = query("xdotool", &["getmouselocation", "--shell"]).await?;
    let get = |key: &str| pointer.lines().find_map(|line| line.strip_prefix(key));
    let id = get("WINDOW=").and_then(xid);
    // Competing overlays or an unmapped/off-screen pointer must not receive the click.
    if id != xid(&window.window_id)
        || get("X=").and_then(|v| v.parse().ok()) != Some(point.x)
        || get("Y=").and_then(|v| v.parse().ok()) != Some(point.y)
        || i64::from(point.x) < i64::from(live.x)
        || i64::from(point.y) < i64::from(live.y)
        || i64::from(point.x) >= i64::from(live.x) + i64::from(live.width)
        || i64::from(point.y) >= i64::from(live.y) + i64::from(live.height)
    {
        return Err(fault(
            "WINDOW_CHANGED",
            "pointer is not over the verified target window",
        ));
    }
    Ok(())
}
struct ReleaseButton(bool);
impl Drop for ReleaseButton {
    fn drop(&mut self) {
        if self.0 {
            // Release only the synthetic button we may have pressed, even on cancellation.
            if let Ok(mut child) = command("xdotool")
                .args(["mouseup", "1"])
                .stdout(Stdio::null())
                .spawn()
            {
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
                });
            }
        }
    }
}
async fn screenshot_to_file(
    tool: &'static str,
    prefix_args: &[&str],
    output_flag: &str,
) -> Result<Vec<u8>> {
    let temp = tempfile::Builder::new()
        .prefix("quickagent-desktop-")
        .tempdir()?;
    let path = temp.path().join("capture.png");
    let path_str = path
        .to_str()
        .context("temporary screenshot path is not valid UTF-8")?;
    let mut child = command(tool)
        .args(prefix_args)
        .arg(output_flag)
        .arg(path_str)
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| spawn_error(tool, e))?;
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .map_err(|_| fault("DESKTOP_TIMEOUT", format!("{tool} did not finish in time")))??;
    if !status.success() {
        return Err(fault(
            "DESKTOP_BACKEND_FAILED",
            format!("{tool} failed; verify that the helper can access the desktop"),
        ));
    }
    let meta = tokio::fs::metadata(&path).await.map_err(|e| {
        fault(
            "DESKTOP_BACKEND_FAILED",
            format!("{tool} did not write screenshot: {e}"),
        )
    })?;
    if meta.len() > MAX_IMAGE as u64 {
        return Err(fault(
            "DESKTOP_OUTPUT_LIMIT",
            format!("{tool} exceeded its output budget"),
        ));
    }
    let data = tokio::fs::read(&path).await.map_err(|e| {
        fault(
            "DESKTOP_BACKEND_FAILED",
            format!("could not read {tool} output: {e}"),
        )
    })?;
    if data.is_empty() {
        return Err(fault(
            "DESKTOP_BACKEND_FAILED",
            format!("{tool} produced an empty screenshot"),
        ));
    }
    Ok(data)
}

async fn wayland_fullscreen_screenshot() -> Result<Vec<u8>> {
    match output("grim", &["-"], MAX_IMAGE).await {
        Ok(bytes) => return Ok(bytes),
        Err(err) if err.to_string().contains("DEPENDENCY_MISSING") => {}
        Err(err) => return Err(err),
    }
    match screenshot_to_file("spectacle", &["-b", "-n", "-f"], "-o").await {
        Ok(bytes) => return Ok(bytes),
        Err(err) if err.to_string().contains("DEPENDENCY_MISSING") => {}
        Err(err) => return Err(err),
    }
    match screenshot_to_file("gnome-screenshot", &[], "-f").await {
        Ok(bytes) => Ok(bytes),
        Err(err) if err.to_string().contains("DEPENDENCY_MISSING") => {
            Err(missing("grim, spectacle, or gnome-screenshot"))
        }
        Err(err) => Err(err),
    }
}

impl DesktopBackend for Linux {
    fn stop(&self) {
        self.portal.stop();
    }

    async fn windows(&self) -> Result<Vec<Window>> {
        match display()? {
            Display::X11 => {
                let mut windows = x11_windows().await?;
                for window in &mut windows {
                    // Input always rechecks the actual X window before injecting.
                    window.active &= window.visible;
                }
                Ok(windows)
            }
            Display::Wayland if kde_wayland() => kwin::windows().await,
            Display::Wayland => Ok(sway_windows(&serde_json::from_str(
                &sway_command(&["-t", "get_tree", "-r"]).await?,
            )?)),
        }
    }
    async fn screenshot(&self, window: Option<&Window>) -> Result<Vec<u8>> {
        match display()? {
            Display::X11 => {
                let id = if let Some(window) = window {
                    x11_live(window, false).await?.window_id
                } else {
                    "root".to_owned()
                };
                output("import", &["-window", &id, "png:-"], MAX_IMAGE).await
            }
            Display::Wayland if kde_wayland() => {
                let Some(window) = window else {
                    return screenshot_to_file("spectacle", &["-b", "-n", "-f"], "-o").await;
                };
                // Screenshot is read-only: never focus a window as a side effect.
                let cancel = CancellationToken::new();
                let before = kwin::verify(window, None, false, &cancel).await?;
                let pixels = screenshot_to_file("spectacle", &["-b", "-n", "-f"], "-o").await?;
                let after = kwin::verify(window, None, false, &cancel).await?;
                let screen = kwin::same_screen(&before, &after)?;
                let rect = kwin::Rect::from_window(window);
                tokio::task::spawn_blocking(move || kwin::crop_png(&pixels, screen, rect)).await?
            }
            Display::Wayland => {
                if let Some(window) = window {
                    verify_sway(window).await?;
                    let geometry = format!(
                        "{},{} {}x{}",
                        window.x, window.y, window.width, window.height
                    );
                    output("grim", &["-g", &geometry, "-"], MAX_IMAGE).await
                } else {
                    wayland_fullscreen_screenshot().await
                }
            }
        }
    }
    async fn clipboard_read(&self) -> Result<String> {
        let bytes = match display()? {
            Display::X11 => output("xclip", &["-selection", "clipboard", "-out"], MAX_TEXT).await?,
            Display::Wayland => {
                output("wl-paste", &["--type", "text", "--no-newline"], MAX_TEXT).await?
            }
        };
        Ok(String::from_utf8(bytes).context("clipboard is not UTF-8 text")?)
    }
    async fn clipboard_write(&self, text: &str, cancel: &CancellationToken) -> Result<()> {
        match display()? {
            Display::X11 => {
                mutate(
                    "xclip",
                    &["-selection", "clipboard", "-in"],
                    Some(text),
                    cancel,
                )
                .await
            }
            Display::Wayland => {
                mutate(
                    "wl-copy",
                    &["--type", "text/plain;charset=utf-8"],
                    Some(text),
                    cancel,
                )
                .await
            }
        }
    }
    async fn focus_target(&self, id: &str) -> Result<Window> {
        let windows = self.windows().await?;
        if matches!(display()?, Display::Wayland) && kde_wayland() {
            guard::focus_window(&windows, id)
        } else {
            guard::window(&windows, Some(id), false)
        }
    }
    async fn focus(&self, window: &Window, cancel: &CancellationToken) -> Result<()> {
        match display()? {
            Display::X11 => {
                let id = x11_live(window, false).await?.window_id;
                mutate("wmctrl", &["-ia", &id], None, cancel).await
            }
            Display::Wayland if kde_wayland() => kwin::focus(window, cancel).await,
            Display::Wayland => {
                let current =
                    guard::window(&self.windows().await?, Some(&window.window_id), false)?;
                if current != *window {
                    return Err(fault(
                        "WINDOW_CHANGED",
                        "window identity or geometry changed",
                    ));
                }
                let id = current
                    .window_id
                    .strip_prefix("sway-")
                    .and_then(|id| id.parse::<u64>().ok())
                    .context("invalid compositor window ID")?;
                mutate(
                    "swaymsg",
                    &[&format!("[con_id={id}]"), "focus"],
                    None,
                    cancel,
                )
                .await
            }
        }
    }
    async fn notify(&self, title: &str, body: &str, cancel: &CancellationToken) -> Result<()> {
        display()?;
        mutate(
            "notify-send",
            &["-a", "QuickAgent", "-i", "quickagent", "--", title, body],
            None,
            cancel,
        )
        .await
    }
    async fn open(&self, target: &guard::OpenTarget, cancel: &CancellationToken) -> Result<()> {
        display()?;
        match target {
            guard::OpenTarget::Url(url) => mutate("xdg-open", &[url.as_str()], None, cancel).await,
            guard::OpenTarget::App(app) => mutate("gtk-launch", &[app], None, cancel).await,
        }
    }
    async fn mouse(
        &self,
        window: &Window,
        point: Point,
        action: Pointer,
        cancel: &CancellationToken,
    ) -> Result<()> {
        if matches!(display()?, Display::Wayland) {
            if kde_wayland() {
                return self.portal.mouse(window, point, action, cancel).await;
            }
            return Err(missing(
                "verified Wayland pointer routing (ydotool cannot verify a compositor target); use AT-SPI or an X11 session",
            ));
        }
        let live = x11_live(window, true).await?;
        // wmctrl and XGetGeometry can disagree about WM borders. Translate to client coordinates now.
        let translate = |point: Point| -> Result<Point> {
            let args = json!({"x":i64::from(point.x)-i64::from(window.x),"y":i64::from(point.y)-i64::from(window.y)});
            guard::point(&args, "x", "y", &live, true)
        };
        let point = translate(point)?;
        let drag = match action {
            Pointer::Drag(end) => Some(translate(end)?),
            _ => None,
        };
        if let Some(end) = drag {
            x11_move(end, cancel).await?;
            x11_hover_is(window, end).await?;
        }
        x11_move(point, cancel).await?;
        x11_hover_is(window, point).await?;
        match action {
            Pointer::Move => Ok(()),
            Pointer::Click(button) => {
                mutate("xdotool", &["click", &button.to_string()], None, cancel).await
            }
            Pointer::DoubleClick => {
                mutate("xdotool", &["click", "1"], None, cancel).await?;
                tokio::time::sleep(Duration::from_millis(60)).await;
                x11_hover_is(window, point).await?;
                mutate("xdotool", &["click", "1"], None, cancel).await
            }
            Pointer::Scroll(delta) => {
                for i in 0..delta.unsigned_abs().div_ceil(120) {
                    if i > 0 {
                        x11_hover_is(window, point).await?;
                    }
                    mutate(
                        "xdotool",
                        &["click", if delta < 0 { "4" } else { "5" }],
                        None,
                        cancel,
                    )
                    .await?;
                }
                Ok(())
            }
            Pointer::Drag(_) => {
                let end = drag.context("missing drag end")?;
                let mut release = ReleaseButton(true);
                mutate("xdotool", &["mousedown", "1"], None, cancel).await?;
                x11_move(end, cancel).await?;
                x11_hover_is(window, end).await?;
                mutate("xdotool", &["mouseup", "1"], None, cancel).await?;
                release.0 = false;
                Ok(())
            }
        }
    }
    async fn keyboard(
        &self,
        window: &Window,
        input: &Keyboard,
        cancel: &CancellationToken,
    ) -> Result<()> {
        match display()? {
            Display::X11 => {
                x11_live(window, true).await?;
                match input {
                    Keyboard::Text(text) => {
                        mutate(
                            "xdotool",
                            &["type", "--delay", "0", "--clearmodifiers", "--", text],
                            None,
                            cancel,
                        )
                        .await
                    }
                    Keyboard::Key { modifiers, key } => {
                        let chord = modifiers
                            .iter()
                            .copied()
                            .chain([key.as_str()])
                            .collect::<Vec<_>>()
                            .join("+");
                        mutate(
                            "xdotool",
                            &["key", "--delay", "0", "--clearmodifiers", &chord],
                            None,
                            cancel,
                        )
                        .await
                    }
                }
            }
            Display::Wayland if kde_wayland() => self.portal.keyboard(window, input, cancel).await,
            Display::Wayland => {
                verify_sway(window).await?;
                match input {
                    Keyboard::Text(text) => mutate("wtype", &["--", text], None, cancel).await,
                    Keyboard::Key { modifiers, key } => {
                        let mut args = Vec::new();
                        for modifier in modifiers {
                            args.extend(["-M", *modifier]);
                        }
                        args.extend(["-k", key]);
                        for modifier in modifiers.iter().rev() {
                            args.extend(["-m", *modifier]);
                        }
                        mutate("wtype", &args, None, cancel).await
                    }
                }
            }
        }
    }
    async fn read_tree(&self, window: &Window, limit: usize) -> Result<Vec<SemanticNode>> {
        tokio::time::timeout(Duration::from_secs(8), atspi_tree(window, limit))
            .await
            .map_err(|_| {
                fault(
                    "DESKTOP_TIMEOUT",
                    "AT-SPI did not return a tree within its time budget",
                )
            })?
    }
    async fn invoke(
        &self,
        window: &Window,
        node: &SemanticRef,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let pid = atspi_pid(&node.bus, &node.sender).await?;
        if Some(pid) != window.pid {
            return Err(fault(
                "STALE_REF",
                "AT-SPI application no longer matches the window",
            ));
        }
        let role = role(node).await?;
        if guard::sensitive(&role, &name(node).await?) || !action_role(&role) {
            return Err(fault(
                "SENSITIVE_WINDOW",
                "AT-SPI target is not an allowed control",
            ));
        }
        let action = call_atspi(node, "Action", "GetName", &["i", "0"]).await?;
        if !matches!(
            value(&action).as_str(),
            Some("click" | "press" | "activate" | "toggle" | "open")
        ) {
            return Err(fault(
                "STALE_REF",
                "AT-SPI control has no supported default action",
            ));
        }
        match display()? {
            Display::X11 => {
                x11_live(window, true).await?;
            }
            Display::Wayland if kde_wayland() => {
                kwin::verify(window, None, false, cancel).await?;
            }
            Display::Wayland => verify_sway(window).await?,
        }
        guard::cancelled(cancel)?;
        let result = call_atspi(node, "Action", "DoAction", &["i", "0"]).await?;
        if value(&result) != true {
            return Err(fault(
                "DESKTOP_BACKEND_FAILED",
                "AT-SPI rejected the control action",
            ));
        }
        Ok(())
    }
}

const ATSPI: &str = "org.a11y.atspi";
fn value(reply: &Value) -> &Value {
    &reply["data"][0]
}
async fn bus(address: Option<&str>, args: &[&str]) -> Result<Value> {
    let addr = address.map(|address| format!("--address={address}"));
    let mut argv = vec![
        "--json=short",
        "--timeout=2s",
        addr.as_deref().unwrap_or("--user"),
    ];
    argv.extend_from_slice(args);
    Ok(serde_json::from_slice(
        &output("busctl", &argv, MAX_LIST).await?,
    )?)
}
async fn call_atspi(node: &SemanticRef, iface: &str, method: &str, args: &[&str]) -> Result<Value> {
    let interface = format!("{ATSPI}.{iface}");
    let mut argv = vec!["call", &node.sender, &node.path, &interface, method];
    argv.extend_from_slice(args);
    bus(Some(&node.bus), &argv).await
}
async fn role(node: &SemanticRef) -> Result<String> {
    Ok(
        value(&call_atspi(node, "Accessible", "GetRoleName", &[]).await?)
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase(),
    )
}
async fn name(node: &SemanticRef) -> Result<String> {
    let reply = bus(
        Some(&node.bus),
        &[
            "get-property",
            &node.sender,
            &node.path,
            "org.a11y.atspi.Accessible",
            "Name",
        ],
    )
    .await?;
    Ok(trim(
        reply["data"]
            .as_str()
            .or_else(|| value(&reply).as_str())
            .unwrap_or(""),
        240,
    ))
}
async fn atspi_pid(bus_addr: &str, sender: &str) -> Result<u32> {
    let reply = bus(
        Some(bus_addr),
        &[
            "call",
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetConnectionUnixProcessID",
            "s",
            sender,
        ],
    )
    .await?;
    Ok(u32::try_from(
        value(&reply)
            .as_u64()
            .context("unknown AT-SPI application process")?,
    )?)
}
async fn children(node: &SemanticRef) -> Result<Vec<SemanticRef>> {
    let reply = call_atspi(node, "Accessible", "GetChildren", &[]).await?;
    let refs = value(&reply)
        .as_array()
        .context("unexpected AT-SPI child list")?;
    Ok(refs
        .iter()
        .take(200)
        .filter_map(|pair| {
            let sender = pair[0].as_str()?;
            let path = pair[1].as_str()?;
            if !sender.starts_with(':')
                || !path.starts_with("/org/a11y/atspi/")
                || path.ends_with("/null")
            {
                return None;
            }
            Some(SemanticRef {
                bus: node.bus.clone(),
                sender: sender.to_owned(),
                path: path.to_owned(),
            })
        })
        .collect())
}
fn action_role(role: &str) -> bool {
    matches!(
        role,
        "push button"
            | "toggle button"
            | "check box"
            | "radio button"
            | "link"
            | "menu item"
            | "check menu item"
            | "radio menu item"
            | "page tab"
    )
}
async fn atspi_tree(window: &Window, limit: usize) -> Result<Vec<SemanticNode>> {
    let pid = window
        .pid
        .context("window process cannot be mapped to AT-SPI")?;
    let address = bus(
        None,
        &[
            "call",
            "org.a11y.Bus",
            "/org/a11y/bus",
            "org.a11y.Bus",
            "GetAddress",
        ],
    )
    .await?;
    let address = value(&address)
        .as_str()
        .filter(|addr| addr.starts_with("unix:") && !addr.contains(';'))
        .context("no local AT-SPI bus")?;
    let desktop = SemanticRef {
        bus: address.into(),
        sender: "org.a11y.atspi.Registry".into(),
        path: "/org/a11y/atspi/accessible/root".into(),
    };
    let mut match_windows = Vec::new();
    for app in children(&desktop).await? {
        if atspi_pid(address, &app.sender).await.ok() != Some(pid) {
            continue;
        }
        for candidate in children(&app).await? {
            if !window.title.is_empty() && name(&candidate).await? == window.title {
                match_windows.push(candidate);
            }
        }
    }
    if match_windows.len() != 1 {
        return Err(fault(
            "WINDOW_UNAVAILABLE",
            "AT-SPI could not uniquely identify the requested window",
        ));
    }
    let mut queue = VecDeque::from([(match_windows.pop().unwrap(), 0u16)]);
    let mut seen = BTreeSet::new();
    let mut nodes = Vec::new();
    while let Some((item, depth)) = queue.pop_front() {
        if nodes.len() == limit {
            break;
        }
        if !seen.insert((item.sender.clone(), item.path.clone())) {
            continue;
        }
        let role = role(&item).await?;
        if matches!(role.as_str(), "password text" | "password") {
            continue;
        }
        let name = name(&item).await?;
        if guard::sensitive(&name, &role) {
            continue;
        }
        if depth < 24 && nodes.len() + queue.len() + 1 < limit {
            for child in children(&item)
                .await?
                .into_iter()
                .filter(|child| child.sender == item.sender)
                .take(limit - nodes.len() - queue.len() - 1)
            {
                queue.push_back((child, depth + 1));
            }
        }
        let action = action_role(&role).then_some(item);
        nodes.push(SemanticNode {
            name,
            role,
            depth,
            action,
        });
    }
    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_window_class_pid_and_titles_without_shell_splitting() {
        let rows = wmctrl_windows(
            "0x000001cb  0 44   10 12  640 480 editor.Editor host A title with spaces\n",
            Some(459),
        );
        assert_eq!(rows.len(), 1);
        let window = &rows[0];
        assert!(window.active && window.visible);
        assert_eq!(
            (
                &*window.window_id,
                &*window.title,
                &*window.app_class,
                window.pid
            ),
            ("0x1cb", "A title with spaces", "editor.Editor", Some(44))
        );
    }
}
