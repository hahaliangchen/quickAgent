//! Validate untrusted arguments and fail closed on unknown input targets.
use super::*;
use std::time::Duration;

pub(super) fn cancelled(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(fault("CANCELLED", "desktop task cancelled"));
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct InputGate(tokio::sync::Mutex<Option<tokio::time::Instant>>);
impl InputGate {
    pub(super) async fn acquire(
        &self,
        cancel: &CancellationToken,
    ) -> Result<tokio::sync::MutexGuard<'_, Option<tokio::time::Instant>>> {
        cancelled(cancel)?;
        let mut guard = self
            .0
            .try_lock()
            .map_err(|_| fault("RATE_LIMITED", "another desktop action is in progress"))?;
        let now = tokio::time::Instant::now();
        if guard.is_some_and(|last| now.duration_since(last) < Duration::from_millis(150)) {
            return Err(fault(
                "RATE_LIMITED",
                "wait at least 150 ms between desktop actions",
            ));
        }
        *guard = Some(now);
        Ok(guard)
    }
}

pub(super) fn text<'a>(args: &'a Value, key: &str, max_bytes: usize) -> Result<&'a str> {
    args[key]
        .as_str()
        .filter(|s| s.len() <= max_bytes && !s.contains('\0'))
        .ok_or_else(|| {
            fault(
                "INVALID_ARGUMENT",
                format!("{key} must be text of at most {max_bytes} bytes, without NUL"),
            )
        })
}
pub(super) fn optional_id(args: &Value) -> Result<Option<&str>> {
    if args["window_id"].is_null() {
        return Ok(None);
    }
    let id = text(args, "window_id", 64)?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(fault(
            "INVALID_ARGUMENT",
            "use a window ID from desktop.list_windows",
        ));
    }
    Ok(Some(id))
}

pub(super) const SENSITIVE_WINDOWS: &[&str] = &[
    "polkit",
    "policykit",
    "pinentry",
    "gnomekeyring",
    "1password",
    "keepass",
    "bitwarden",
    "kwallet",
    "passwordmanager",
    "authenticationrequired",
    "authenticate",
    "sudoprompt",
    "screenlocker",
    "gnomescreensaver",
    "kscreenlocker",
    "seahorse",
    "gcrprompter",
    "xdgdesktopportalkde",
];

pub(super) fn sensitive(title: &str, class: &str) -> bool {
    let canonical = |s: &str| s.to_ascii_lowercase().replace(['-', '_', ' ', '.'], "");
    let title = canonical(title);
    let class = canonical(class);
    SENSITIVE_WINDOWS
        .iter()
        .any(|term| title.contains(term) || class.contains(term))
}

pub(super) fn focus_window(windows: &[Window], id: &str) -> Result<Window> {
    select_window(windows, Some(id), false, false)
}

pub(super) fn window(windows: &[Window], id: Option<&str>, require_active: bool) -> Result<Window> {
    select_window(windows, id, true, require_active)
}

fn select_window(
    windows: &[Window],
    id: Option<&str>,
    require_visible: bool,
    require_active: bool,
) -> Result<Window> {
    let matches = |w: &&Window| id.map_or(w.active, |id| w.window_id == id);
    let mut candidates = windows.iter().filter(matches);
    let window = candidates
        .next()
        .filter(|_| candidates.next().is_none())
        .ok_or_else(|| {
            fault(
                "WINDOW_UNAVAILABLE",
                "window disappeared or focused window cannot be verified",
            )
        })?;
    if sensitive(&window.title, &window.app_class) {
        return Err(fault(
            "SENSITIVE_WINDOW",
            "desktop operation refused for a protected window",
        ));
    }
    if (require_visible && !window.visible)
        || window.width == 0
        || window.height == 0
        || (require_active && !window.active)
    {
        return Err(fault(
            "WINDOW_UNAVAILABLE",
            "window must be visible and foreground for input",
        ));
    }
    if require_active && (window.app_class.is_empty() || window.pid.is_none()) {
        return Err(fault(
            "WINDOW_UNAVAILABLE",
            "foreground application identity could not be verified",
        ));
    }
    Ok(window.clone())
}

pub(super) fn point(
    args: &Value,
    x: &str,
    y: &str,
    window: &Window,
    relative: bool,
) -> Result<Point> {
    let number = |key: &str| {
        args[key]
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .map(i64::from)
            .ok_or_else(|| {
                fault(
                    "INVALID_ARGUMENT",
                    format!("{key} must be an integer coordinate"),
                )
            })
    };
    let x = number(x)?;
    let y = number(y)?;
    let dx = if relative { x } else { x - i64::from(window.x) };
    let dy = if relative { y } else { y - i64::from(window.y) };
    if !(0..i64::from(window.width)).contains(&dx) || !(0..i64::from(window.height)).contains(&dy) {
        return Err(fault(
            "COORDINATE_OUT_OF_BOUNDS",
            "pointer position falls outside the target window",
        ));
    }
    Ok(Point {
        x: i32::try_from(i64::from(window.x) + dx)?,
        y: i32::try_from(i64::from(window.y) + dy)?,
    })
}
pub(super) fn pointer(args: &Value, window: &Window, relative: bool) -> Result<(Point, Pointer)> {
    let from = point(args, "x", "y", window, relative)?;
    let action = match args["action"].as_str() {
        Some("click") => Pointer::Click(1),
        Some("right_click") => Pointer::Click(3),
        Some("double_click") => Pointer::DoubleClick,
        Some("move") => Pointer::Move,
        Some("drag") => Pointer::Drag(point(args, "to_x", "to_y", window, relative)?),
        Some("scroll") => Pointer::Scroll(
            args["delta_y"]
                .as_i64()
                .filter(|n| *n != 0 && n.unsigned_abs() <= 600)
                .ok_or_else(|| {
                    fault(
                        "INVALID_ARGUMENT",
                        "delta_y must be a nonzero pixel delta between -600 and 600",
                    )
                })? as i32,
        ),
        _ => return Err(fault("INVALID_ARGUMENT", "unknown pointer action")),
    };
    Ok((from, action))
}

pub(super) fn keyboard(args: &Value) -> Result<Keyboard> {
    if args["keys"].is_null() {
        let text = text(args, "text", 256)?;
        if text.is_empty() || text.chars().any(char::is_control) {
            return Err(fault(
                "INVALID_ARGUMENT",
                "text must be nonempty printable text; use keys for Enter or Tab",
            ));
        }
        return Ok(Keyboard::Text(text.to_owned()));
    }
    let keys = args["keys"]
        .as_array()
        .filter(|keys| (1..=4).contains(&keys.len()))
        .ok_or_else(|| {
            fault(
                "INVALID_ARGUMENT",
                "keys must contain one key and up to three modifiers",
            )
        })?;
    if !args["text"].is_null() {
        return Err(fault("INVALID_ARGUMENT", "choose text or keys"));
    }
    let mut modifiers = Vec::new();
    for key in &keys[..keys.len() - 1] {
        let key = key
            .as_str()
            .filter(|k| k.len() <= 16)
            .unwrap_or("")
            .to_ascii_lowercase();
        let modifier = match key.as_str() {
            "ctrl" | "control" => "ctrl",
            "alt" => "alt",
            "shift" => "shift",
            _ => return Err(fault("UNSAFE_KEY", "unsupported modifier")),
        };
        if modifiers.contains(&modifier) {
            return Err(fault("UNSAFE_KEY", "duplicate modifier"));
        }
        modifiers.push(modifier);
    }
    let key = keys
        .last()
        .and_then(Value::as_str)
        .filter(|k| k.len() <= 16)
        .unwrap_or("")
        .to_ascii_lowercase();
    let key = match key.as_str() {
        "delete" | "del" => "Delete",
        "enter" | "return" => "Return",
        "esc" | "escape" => "Escape",
        "tab" => "Tab",
        "backspace" => "BackSpace",
        "space" => "space",
        "left" => "Left",
        "right" => "Right",
        "up" => "Up",
        "down" => "Down",
        "home" => "Home",
        "end" => "End",
        "pageup" => "Page_Up",
        "pagedown" => "Page_Down",
        k if k.len() == 1 && k.as_bytes()[0].is_ascii_alphanumeric() => k,
        k if matches!(
            k,
            "f1" | "f2" | "f3" | "f4" | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12"
        ) =>
        {
            k
        }
        _ => return Err(fault("UNSAFE_KEY", "unsupported key")),
    };
    let function = key.starts_with('f') && key.len() > 1;
    if modifiers.contains(&"ctrl")
        && modifiers.contains(&"alt")
        && (key == "Delete" || function || key == "BackSpace")
    {
        return Err(fault("UNSAFE_KEY", "system shortcuts are blocked"));
    }
    let key = if function {
        key.to_ascii_uppercase()
    } else {
        key.to_owned()
    };
    Ok(Keyboard::Key { modifiers, key })
}

#[derive(Debug)]
pub(super) enum OpenTarget {
    Url(url::Url),
    App(&'static str),
}
pub(super) fn open_target(target: &str) -> Result<OpenTarget> {
    // No arbitrary file, executable, or desktop entry supplied by the model.
    const APPS: &[&str] = &[
        "org.gnome.Calculator.desktop",
        "org.gnome.TextEditor.desktop",
        "org.kde.kcalc.desktop",
        "org.kde.kate.desktop",
        "firefox.desktop",
        "chromium.desktop",
    ];
    if let Some(app) = APPS.iter().find(|app| **app == target) {
        return Ok(OpenTarget::App(app));
    }
    let url = url::Url::parse(target).map_err(|_| {
        fault(
            "INVALID_TARGET",
            "use HTTP(S) or an allowlisted registered application ID",
        )
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || target.chars().any(char::is_control)
    {
        return Err(fault(
            "INVALID_TARGET",
            "only HTTP(S) URLs without credentials are accepted",
        ));
    }
    if url.port_or_known_default().is_some_and(|port| {
        matches!(port, 3000 | 3001 | 41200 | 41201)
            || crate::tools::process_sandbox::is_host_control_port(port)
    }) {
        return Err(fault(
            "CONTROL_PORT_BLOCKED",
            "cannot open protected control port",
        ));
    }
    Ok(OpenTarget::Url(url))
}
