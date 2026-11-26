//! Task-scoped native desktop tools. Calls without the host's task context fail closed.
use crate::{
    media::visual_artifacts::{self, VisualContext},
    server::{
        plugin_protocol::{NativeContributions, NativeTool, ToolPermission},
        skill_packages::Package,
    },
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

mod grid;
mod guard;
#[cfg(target_os = "linux")]
mod kwin;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod portal;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(test)]
pub(crate) mod tests;

pub(crate) fn permission(name: &str) -> Option<ToolPermission> {
    use ToolPermission::*;
    Some(match name {
        "desktop__list_windows"
        | "desktop__screenshot_window"
        | "desktop__read_tree"
        | "desktop__clipboard_read" => ReadOnly,
        "desktop__focus_window"
        | "desktop__clipboard_write"
        | "desktop__notify"
        | "desktop__open_app"
        | "desktop__invoke"
        | "desktop__mouse"
        | "desktop__keyboard" => Execution,
        _ => return None,
    })
}
pub(crate) fn requires_confirmation(name: &str) -> bool {
    matches!(permission(name), Some(ToolPermission::Execution))
}

struct Task {
    visual: VisualContext,
    cancel: CancellationToken,
}
tokio::task_local! { static TASK: Task; }

pub(crate) async fn scoped<F: Future<Output = Result<Value>>>(
    visual: VisualContext,
    cancel: CancellationToken,
    call: F,
) -> Result<Value> {
    ensure!(!visual.task_id().is_empty(), "DESKTOP_TASK_REQUIRED");
    TASK.scope(
        Task {
            visual,
            cancel: cancel.clone(),
        },
        async {
            tokio::select! { biased;
                _ = cancel.cancelled() => Err(fault("CANCELLED", "desktop task cancelled")),
                result = call => result,
            }
        },
    )
    .await
}

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
struct Fault {
    code: &'static str,
    message: String,
}
fn fault(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    Fault {
        code,
        message: message.into(),
    }
    .into()
}
fn failure(error: anyhow::Error) -> Result<Value> {
    match error.downcast::<Fault>() {
        Ok(error) => Ok(json!({"ok":false,"error_code":error.code,"reason":error.message})),
        Err(error) => Err(error),
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct Window {
    window_id: String,
    title: String,
    app_class: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    active: bool,
    visible: bool,
    pid: Option<u32>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Point {
    x: i32,
    y: i32,
}
#[derive(Clone, Copy, Debug)]
enum Pointer {
    Click(u8),
    DoubleClick,
    Move,
    Drag(Point),
    Scroll(i32),
}
#[derive(Debug)]
enum Keyboard {
    Text(String),
    Key {
        modifiers: Vec<&'static str>,
        key: String,
    },
}
#[derive(Clone)]
struct SemanticRef {
    bus: String,
    sender: String,
    path: String,
}
struct SemanticNode {
    name: String,
    role: String,
    depth: u16,
    action: Option<SemanticRef>,
}

#[allow(dead_code)] // Unsupported platforms use the default degradation for each capability.
trait DesktopBackend: Send + Sync + 'static {
    fn stop(&self) {}
    fn windows(&self) -> impl Future<Output = Result<Vec<Window>>> + Send {
        async { Err(unavailable()) }
    }
    fn screenshot(&self, _window: Option<&Window>) -> impl Future<Output = Result<Vec<u8>>> + Send {
        async { Err(unavailable()) }
    }
    fn clipboard_read(&self) -> impl Future<Output = Result<String>> + Send {
        async { Err(unavailable()) }
    }
    fn focus_target(&self, id: &str) -> impl Future<Output = Result<Window>> + Send {
        async move { guard::window(&self.windows().await?, Some(id), false) }
    }
    fn focus(
        &self,
        _window: &Window,
        _cancel: &CancellationToken,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Err(unavailable()) }
    }
    fn clipboard_write(
        &self,
        _text: &str,
        _cancel: &CancellationToken,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Err(unavailable()) }
    }
    fn notify(
        &self,
        _title: &str,
        _body: &str,
        _cancel: &CancellationToken,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Err(unavailable()) }
    }
    fn open(
        &self,
        _target: &guard::OpenTarget,
        _cancel: &CancellationToken,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Err(unavailable()) }
    }
    fn read_tree(
        &self,
        _window: &Window,
        _limit: usize,
    ) -> impl Future<Output = Result<Vec<SemanticNode>>> + Send {
        async { Err(unavailable()) }
    }
    fn invoke(
        &self,
        _window: &Window,
        _node: &SemanticRef,
        _cancel: &CancellationToken,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Err(unavailable()) }
    }
    fn mouse(
        &self,
        _window: &Window,
        _point: Point,
        _action: Pointer,
        _cancel: &CancellationToken,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Err(unavailable()) }
    }
    fn keyboard(
        &self,
        _window: &Window,
        _input: &Keyboard,
        _cancel: &CancellationToken,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Err(unavailable()) }
    }
}
#[cfg(not(target_os = "windows"))]
fn unavailable() -> anyhow::Error {
    fault(
        "DEPENDENCY_MISSING",
        "Requires a supported local Linux or Windows desktop session and its helper tools",
    )
}
#[cfg(target_os = "windows")]
#[allow(dead_code)]
fn unavailable() -> anyhow::Error {
    fault(
        "DEPENDENCY_MISSING",
        "Requires a supported local Linux or Windows desktop session and its helper tools",
    )
}
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
struct Unsupported;
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
impl DesktopBackend for Unsupported {}

struct CachedTree {
    root: PathBuf,
    task: String,
    mount: Option<crate::server::plugin_runtime::ScopeId>,
    window: Window,
    refs: BTreeMap<String, SemanticRef>,
    expires: std::time::Instant,
}
struct Desktop<B> {
    backend: B,
    tree: Mutex<VecDeque<CachedTree>>,
    input: guard::InputGate,
}
impl<B: DesktopBackend> Desktop<B> {
    fn new(backend: B) -> Self {
        Self {
            backend,
            tree: Mutex::default(),
            input: guard::InputGate::default(),
        }
    }

    async fn execute(&self, root: &Path, task: &Task, name: &str, args: &Value) -> Result<Value> {
        guard::cancelled(&task.cancel)?;
        let mount = crate::server::skill_packages::current_native_mount();
        let window_id = guard::optional_id(args)?;
        let target = async { guard::window(&self.backend.windows().await?, window_id, false) };
        match name {
            "desktop__list_windows" => {
                return Ok(json!({"ok":true,"windows":self.backend.windows().await?}));
            }
            "desktop__screenshot_window" => {
                let window = if window_id.is_some() {
                    Some(target.await?)
                } else {
                    None
                };
                let grid = args["grid"].as_bool().unwrap_or(false);
                let bytes = self.backend.screenshot(window.as_ref()).await?;
                let context = task.visual.clone();
                let root = root.to_owned();
                return tokio::task::spawn_blocking(move || {
                    let bytes = if grid {grid::overlay(&bytes)?} else {bytes};
                    let artifact = visual_artifacts::save(&root, &context,
                        json!({"source":"desktop.screenshot_window", "window_id":window.as_ref().map(|w| &w.window_id),"grid":grid,"grid_step_px":grid.then_some(grid::STEP)}), &bytes)?;
                    Ok(json!({"ok":true,"artifact_id":artifact["artifact_id"],"width":artifact["width"],"height":artifact["height"],"visual_artifact":artifact}))
                }).await?;
            }
            "desktop__clipboard_read" => {
                return Ok(json!({"ok":true,"text":self.backend.clipboard_read().await?}));
            }
            "desktop__read_tree" => {
                let id = window_id.context("window_id is required")?;
                let nodes = async {
                    let window = target.await?;
                    let nodes = self
                        .backend
                        .read_tree(
                            &window,
                            args["max_nodes"].as_u64().unwrap_or(80).clamp(1, 200) as usize,
                        )
                        .await?;
                    Ok::<_, anyhow::Error>((window, nodes))
                }
                .await;
                let mut trees = self.tree.lock().unwrap();
                trees.retain(|old| {
                    old.expires > std::time::Instant::now()
                        && !(old.mount == mount
                            && old.root == root
                            && old.task == task.visual.task_id()
                            && old.window.window_id == id)
                });
                let (window, nodes) = match nodes {
                    Ok(tree) => tree,
                    Err(error) => {
                        return Ok(json!({"available":false,"reason":error.to_string(),
                        "fallback":"Use desktop.screenshot_window({grid:true}) and desktop.mouse/keyboard when the target can be verified."}));
                    }
                };
                let prefix = visual_artifacts::new_id();
                let mut refs = BTreeMap::new();
                let nodes: Vec<_> = nodes.into_iter().enumerate().map(|(i,node)| {
                    let reference = node.action.map(|action| { let key = format!("{prefix}-{i}"); refs.insert(key.clone(),action); key });
                    json!({"ref":reference,"role":node.role,"name":node.name,"depth":node.depth})
                }).collect();
                if trees.len() >= 16 {
                    trees.pop_front();
                }
                trees.push_back(CachedTree {
                    root: root.to_owned(),
                    task: task.visual.task_id().to_owned(),
                    mount,
                    window,
                    refs,
                    expires: std::time::Instant::now() + std::time::Duration::from_secs(60),
                });
                return Ok(json!({"ok":true,"available":true,"window_id":id,"nodes":nodes}));
            }
            _ => {}
        }
        let cancel = &task.cancel;
        let _input = self.input.acquire(cancel).await?;
        match name {
            "desktop__focus_window" => {
                let id = window_id.context("window_id is required")?;
                self.backend
                    .focus(&self.backend.focus_target(id).await?, cancel)
                    .await?;
            }
            "desktop__clipboard_write" => {
                self.backend
                    .clipboard_write(guard::text(args, "text", 16384)?, cancel)
                    .await?
            }
            "desktop__notify" => {
                self.backend
                    .notify(
                        guard::text(args, "title", 128)?,
                        guard::text(args, "body", 1024)?,
                        cancel,
                    )
                    .await?
            }
            "desktop__open_app" => {
                self.backend
                    .open(
                        &guard::open_target(guard::text(args, "target", 2048)?)?,
                        cancel,
                    )
                    .await?
            }
            "desktop__invoke" => {
                window_id.context("window_id is required")?;
                let window = guard::window(&self.backend.windows().await?, window_id, true)?;
                let reference = guard::text(args, "ref", 128)?;
                let node = self
                    .tree
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|old| {
                        old.mount == mount
                            && old.root == root
                            && old.task == task.visual.task_id()
                            && old.window == window
                            && old.expires > std::time::Instant::now()
                    })
                    .and_then(|old| old.refs.get(reference))
                    .cloned()
                    .ok_or_else(|| {
                        fault("STALE_REF", "Call desktop.read_tree for this window again")
                    })?;
                guard::cancelled(cancel)?;
                self.backend.invoke(&window, &node, cancel).await?;
            }
            "desktop__mouse" => {
                let window = guard::window(&self.backend.windows().await?, window_id, true)?;
                let (point, action) = guard::pointer(args, &window, window_id.is_some())?;
                guard::cancelled(cancel)?;
                self.backend.mouse(&window, point, action, cancel).await?;
            }
            "desktop__keyboard" => {
                let input = guard::keyboard(args)?;
                let window = guard::window(&self.backend.windows().await?, None, true)?;
                guard::cancelled(cancel)?;
                self.backend.keyboard(&window, &input, cancel).await?;
            }
            _ => return Err(fault("INVALID_ARGUMENT", "unknown desktop method")),
        }
        Ok(json!({"ok":true}))
    }
}

fn with_backend<B: DesktopBackend>(backend: B) -> Result<Package> {
    let desktop = Arc::new(Desktop::new(backend));
    let on_unmount = {
        let desktop = desktop.clone();
        Arc::new(move || desktop.backend.stop())
    };
    let definitions: Vec<Value> = serde_json::from_str(include_str!("tools.json"))?;
    let mut tools = BTreeMap::new();
    for definition in definitions {
        let name = definition["name"]
            .as_str()
            .context("desktop tool name missing")?
            .to_owned();
        let permission = permission(&name).context("desktop tool has no permission policy")?;
        let desktop = desktop.clone();
        let handler: super::ToolHandler = Arc::new(move |workspace, name, args| {
            let desktop = desktop.clone();
            Box::pin(async move {
                let task = TASK.try_with(|task| Task { visual: task.visual.clone(), cancel: task.cancel.clone() })
                    .context("DESKTOP_TASK_REQUIRED: desktop tools require an Agent task and its permission policy")?;
                let result = desktop.execute(workspace.root(), &task, &name, &args).await;
                match result {
                    Ok(output) => Ok(output),
                    Err(error) => failure(error),
                }
            })
        });
        tools.insert(
            name,
            NativeTool {
                definition,
                permission,
                handler,
            },
        );
    }
    let root = std::env::var_os("GROW_RESOURCES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    crate::server::skill_packages::load_builtin(
        &root.join("src/server/builtin_plugins/desktop"),
        Arc::new(NativeContributions {
            tools,
            frontend_entries: vec![],
            on_unmount: Some(on_unmount),
        }),
    )
}
pub(super) fn package() -> Result<Package> {
    #[cfg(target_os = "linux")]
    return with_backend(linux::Linux::default());
    #[cfg(target_os = "windows")]
    return with_backend(windows::Windows);
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    return with_backend(Unsupported);
}
