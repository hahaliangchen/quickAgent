use super::*;
use crate::{
    runtime::agent_program::{declarations, libraries},
    server::{
        agent_service::PermissionMode,
        builtin_plugins::ToolCatalog,
        plugin_runtime::{Plugin, PluginContext, PluginRuntime, ROOT},
        skill_packages,
    },
};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(visual_artifacts::new_id());
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn window() -> Window {
    Window {
        window_id: "x1".into(),
        title: "Editor".into(),
        app_class: "TextEditor".into(),
        x: -10,
        y: 20,
        width: 150,
        height: 130,
        active: true,
        visible: true,
        pid: Some(123),
    }
}
#[derive(Clone)]
struct Mock {
    window: Arc<Mutex<Window>>,
    inputs: Arc<AtomicUsize>,
    stops: Arc<AtomicUsize>,
    pause: Option<Arc<tokio::sync::Notify>>,
}
impl Mock {
    fn new() -> Self {
        Self {
            window: Arc::new(Mutex::new(window())),
            inputs: Arc::default(),
            stops: Arc::default(),
            pause: None,
        }
    }
}
impl DesktopBackend for Mock {
    fn stop(&self) {
        self.stops.fetch_add(1, Ordering::SeqCst);
    }
    async fn windows(&self) -> Result<Vec<Window>> {
        Ok(vec![self.window.lock().unwrap().clone()])
    }
    async fn screenshot(&self, _: Option<&Window>) -> Result<Vec<u8>> {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(150, 130).write_to(&mut png, image::ImageFormat::Png)?;
        Ok(png.into_inner())
    }
    async fn mouse(
        &self,
        _: &Window,
        _: Point,
        _: Pointer,
        cancel: &CancellationToken,
    ) -> Result<()> {
        if let Some(pause) = &self.pause {
            pause.notify_one();
            cancel.cancelled().await;
        }
        guard::cancelled(cancel)?;
        self.inputs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn read_tree(&self, _: &Window, _: usize) -> Result<Vec<SemanticNode>> {
        Ok(vec![SemanticNode {
            name: "Save".into(),
            role: "push button".into(),
            depth: 0,
            action: Some(SemanticRef {
                bus: "unix:mock".into(),
                sender: ":1.2".into(),
                path: "/mock".into(),
            }),
        }])
    }
    async fn invoke(&self, _: &Window, _: &SemanticRef, cancel: &CancellationToken) -> Result<()> {
        guard::cancelled(cancel)?;
        self.inputs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct Packages {
    catalog: Arc<ToolCatalog>,
    package: Package,
    enabled: Option<bool>,
}
impl Plugin for Packages {
    fn id(&self) -> &str {
        "skill-packages"
    }
    fn start(&self, ctx: &PluginContext<'_>) -> Result<()> {
        ctx.provide("tools", self.catalog.clone())?;
        ctx.provide("skill-packages", self.catalog.packages.clone())?;
        skill_packages::register_builtin(ctx, &self.catalog, self.package.clone(), self.enabled)
    }
}
fn install(mock: Mock, enabled: Option<bool>) -> (PluginRuntime, Arc<ToolCatalog>) {
    let runtime = PluginRuntime::new();
    let catalog = Arc::new(ToolCatalog::default());
    runtime
        .mount(
            ROOT,
            Packages {
                catalog: catalog.clone(),
                package: with_backend(mock).unwrap(),
                enabled,
            },
        )
        .unwrap();
    (runtime, catalog)
}

pub(crate) fn mock_catalog() -> (PluginRuntime, Arc<ToolCatalog>) {
    install(Mock::new(), Some(true))
}

fn vision(task: &str) -> VisualContext {
    VisualContext {
        identity: json!({"task_id":task,"runtime":"single_agent","request_id":1}),
        source_tool_call_id: "shot-call".into(),
        ..Default::default()
    }
}
fn workspace(root: &Path) -> Arc<crate::tools::Workspace> {
    Arc::new(crate::tools::Workspace::new(root).unwrap())
}

#[tokio::test]
async fn desktop_opt_in_mount_contracts_permissions_and_stop() -> Result<()> {
    let tmp = Scratch::new();
    let backend = Mock::new();
    let stops = backend.stops.clone();
    let (runtime, catalog) = install(backend, None);
    let registry = &catalog.packages;
    let namespace =
        || libraries::catalog(&registry.tools(&registry.enabled()), &registry.catalog());
    assert!(!catalog.contains("desktop__list_windows"));
    assert!(!namespace().contains_key("desktop"));
    assert_eq!(registry.catalog()[0]["enabled"], false);
    assert_eq!(registry.catalog()[0]["toggleable"], true);
    assert!(registry.skills().as_array().unwrap().is_empty());
    skill_packages::set_enabled(&runtime, registry, "desktop", true, false)?;
    let definitions = registry.tools(&registry.enabled());
    let libs = namespace();
    assert_eq!(libs["desktop"].len(), 11);
    assert!(declarations::headers(&libs, &definitions).contains("screenshot_window"));
    let header = declarations::static_library_header(&libs);
    assert!(header.contains("desktop"));
    assert!(!header.contains("screenshot_window"));
    for tool in definitions {
        let name = tool["function"]["name"].as_str().unwrap();
        let execution = !matches!(
            name,
            "desktop__list_windows"
                | "desktop__screenshot_window"
                | "desktop__read_tree"
                | "desktop__clipboard_read"
        );
        for (mode, allowed, approval) in [
            (PermissionMode::ReadOnly, !execution, false),
            (PermissionMode::WorkspaceWrite, !execution, false),
            (PermissionMode::Ask, true, execution),
            (PermissionMode::AutoReview, true, execution),
            (PermissionMode::FullAccess, true, false),
        ] {
            assert_eq!(registry.allows(name, mode, false), allowed, "{name}");
            assert_eq!(
                crate::tools::tool_approvals::needs_approval(mode, name, Some(&catalog), false),
                approval,
                "{name}"
            );
        }
        assert_eq!(
            registry.allows(name, PermissionMode::FullAccess, true),
            !execution
        );
    }
    let workspace = workspace(&tmp.0);
    let raw = catalog
        .execute(workspace.clone(), "desktop__list_windows", json!({}))
        .await
        .unwrap_err();
    assert!(raw.to_string().contains("DESKTOP_TASK_REQUIRED"));
    let handler = catalog.handler("desktop__list_windows")?;
    let result = scoped(
        vision("fixture"),
        CancellationToken::new(),
        handler(workspace.clone(), "desktop__list_windows".into(), json!({})),
    )
    .await?;
    assert_eq!(result["windows"][0]["window_id"], "x1");
    let tree = scoped(
        vision("fixture"),
        CancellationToken::new(),
        catalog.execute(
            workspace.clone(),
            "desktop__read_tree",
            json!({"window_id":"x1"}),
        ),
    )
    .await?;
    skill_packages::set_enabled(&runtime, registry, "desktop", false, false)?;
    assert_eq!(stops.load(Ordering::SeqCst), 1);
    assert!(catalog.handler("desktop__list_windows").is_err());
    assert!(!namespace().contains_key("desktop"));
    skill_packages::set_enabled(&runtime, registry, "desktop", true, false)?;
    let stopped = scoped(
        vision("fixture"),
        CancellationToken::new(),
        handler(workspace.clone(), "desktop__list_windows".into(), json!({})),
    )
    .await
    .unwrap_err();
    assert!(stopped.to_string().contains("stopped"));
    let stale = scoped(
        vision("fixture"),
        CancellationToken::new(),
        catalog.execute(
            workspace,
            "desktop__invoke",
            json!({"window_id":"x1","ref":tree["nodes"][0]["ref"]}),
        ),
    )
    .await?;
    assert_eq!(stale["error_code"], "STALE_REF");
    let (_, enabled) = install(Mock::new(), Some(true));
    assert!(enabled.contains("desktop__list_windows"));
    Ok(())
}

#[test]
fn desktop_blocks_sensitive_windows_dangerous_chords_and_untrusted_launch_targets() {
    for protected in [
        "PolKit",
        "pinentry-gtk",
        "GNOME Keyring",
        "1Password",
        "KeePassXC",
        "consent.exe",
        "CredentialUIBroker.exe",
        "Windows Security",
        "LockApp.exe",
    ] {
        for require_active in [false, true] {
            let w = Window {
                app_class: protected.into(),
                ..window()
            };
            assert!(
                guard::window(&[w], Some("x1"), require_active)
                    .unwrap_err()
                    .to_string()
                    .contains("SENSITIVE_WINDOW")
            );
        }
    }
    for keys in [
        json!(["CONTROL", "Alt", "Del"]),
        json!(["ctrl", "alt", "F12"]),
        json!(["ctrl+alt+Delete"]),
        json!(["ctrl", "alt", "BackSpace"]),
        json!(["super", "r"]),
    ] {
        assert!(guard::keyboard(&json!({"keys":keys})).is_err());
    }
    for i in 1..=12 {
        assert!(guard::keyboard(&json!({"keys":["Ctrl","Alt",format!("F{i}")]})).is_err());
    }
    assert!(guard::keyboard(&json!({"keys":["ctrl","s"]})).is_ok());
    assert!(guard::keyboard(&json!({"text":"two\nlines"})).is_err());
    for target in [
        "javascript:alert(1)",
        "file:///tmp/app",
        "--help",
        "sh -c something",
        "foo.desktop",
        "https://a:b@example.org/",
        "http://localhost:3000/",
    ] {
        assert!(guard::open_target(target).is_err(), "{target}");
    }
    assert!(guard::open_target("https://example.org/path?q=one%20two").is_ok());
}

#[test]
fn desktop_enforces_relative_absolute_and_drag_bounds() {
    let w = window();
    assert_eq!(
        guard::point(&json!({"x":5,"y":9}), "x", "y", &w, true).unwrap(),
        Point { x: -5, y: 29 }
    );
    for args in [
        json!({"x":150,"y":129}),
        json!({"x":-1,"y":5}),
        json!({"x":5,"y":130}),
        json!({"x":1.2,"y":0}),
        json!({"x":i64::MAX,"y":0}),
    ] {
        assert!(guard::point(&args, "x", "y", &w, true).is_err());
    }
    assert!(guard::point(&json!({"x":-5,"y":19}), "x", "y", &w, false).is_err());
    assert!(
        guard::pointer(
            &json!({"action":"drag","x":5,"y":5,"to_x":5,"to_y":130}),
            &w,
            true
        )
        .is_err()
    );
    assert!(
        guard::window(
            &[Window {
                visible: false,
                ..w
            }],
            Some("x1"),
            true
        )
        .is_err()
    );
}

#[tokio::test(start_paused = true)]
async fn desktop_serializes_and_rate_limits_input_and_honors_cancel() -> Result<()> {
    let gate = guard::InputGate::default();
    let cancel = CancellationToken::new();
    let hold = gate.acquire(&cancel).await?;
    assert!(
        gate.acquire(&cancel)
            .await
            .unwrap_err()
            .to_string()
            .contains("RATE_LIMITED")
    );
    drop(hold);
    assert!(gate.acquire(&cancel).await.is_err());
    tokio::time::advance(std::time::Duration::from_millis(150)).await;
    drop(gate.acquire(&cancel).await?);
    cancel.cancel();
    assert!(
        gate.acquire(&cancel)
            .await
            .unwrap_err()
            .to_string()
            .contains("CANCELLED")
    );
    Ok(())
}

#[tokio::test]
async fn desktop_running_input_is_aborted_before_emitting_when_task_is_cancelled() -> Result<()> {
    let tmp = Scratch::new();
    let pause = Arc::new(tokio::sync::Notify::new());
    let mock = Mock {
        pause: Some(pause.clone()),
        ..Mock::new()
    };
    let inputs = mock.inputs.clone();
    let (_runtime, catalog) = install(mock, Some(true));
    let cancel = CancellationToken::new();
    let work = workspace(&tmp.0);
    let call = scoped(
        vision("fixture"),
        cancel.clone(),
        catalog.execute(
            work,
            "desktop__mouse",
            json!({"window_id":"x1","action":"click","x":7,"y":9}),
        ),
    );
    let (result, ()) = tokio::join!(call, async {
        pause.notified().await;
        cancel.cancel();
    });
    assert!(result.unwrap_err().to_string().contains("CANCELLED"));
    assert_eq!(inputs.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn desktop_grid_png_is_saved_with_visual_ownership_and_original_coordinates() -> Result<()> {
    let tmp = Scratch::new();
    let (_runtime, catalog) = install(Mock::new(), Some(true));
    let ctx = vision("fixture");
    let result = scoped(
        ctx.clone(),
        CancellationToken::new(),
        catalog.execute(
            workspace(&tmp.0),
            "desktop__screenshot_window",
            json!({"window_id":"x1","grid":true}),
        ),
    )
    .await?;
    let id = result["artifact_id"].as_str().unwrap();
    let (artifact, png) = visual_artifacts::read(&tmp.0, "fixture", id)?;
    visual_artifacts::authorize(&artifact, &ctx)?;
    assert!(visual_artifacts::read(&tmp.0, "other", id).is_err());
    assert_eq!(artifact["source_tool_call_id"], "shot-call");
    assert_eq!(artifact["grid_step_px"], 100);
    let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png)?.into_rgba8();
    assert_eq!(image.dimensions(), (150, 130));
    assert_eq!(image.get_pixel(50, 50).0, [0, 0, 0, 255]);
    assert!(image.get_pixel(100, 50).0[2] > 0);
    assert!(image.get_pixel(105, 3).0[0] > 0); // First digit of the 100px tick.
    let (block, _) = visual_artifacts::image_block(&tmp.0, &ctx, id, false)?;
    assert!(
        block["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
    Ok(())
}

#[tokio::test]
async fn desktop_semantic_refs_are_scoped_and_absent_backend_degrades() -> Result<()> {
    struct NoTree;
    impl DesktopBackend for NoTree {
        async fn windows(&self) -> Result<Vec<Window>> {
            Ok(vec![window()])
        }
    }
    let tmp = Scratch::new();
    let token = CancellationToken::new();
    let task = Task {
        visual: vision("one"),
        cancel: token.clone(),
    };
    let args = json!({"window_id":"x1"});
    let result = Desktop::new(NoTree)
        .execute(&tmp.0, &task, "desktop__read_tree", &args)
        .await?;
    assert_eq!(result["available"], false);
    assert!(
        result["fallback"]
            .as_str()
            .unwrap()
            .contains("screenshot_window")
    );
    let mock = Mock::new();
    let inputs = mock.inputs.clone();
    let desktop = Desktop::new(mock);
    let tree = desktop
        .execute(&tmp.0, &task, "desktop__read_tree", &args)
        .await?;
    let invoke = json!({"window_id":"x1","ref":tree["nodes"][0]["ref"]});
    let other = Task {
        visual: vision("two"),
        cancel: token,
    };
    assert!(
        desktop
            .execute(&tmp.0, &other, "desktop__invoke", &invoke)
            .await
            .unwrap_err()
            .to_string()
            .contains("STALE_REF")
    );
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(
        desktop
            .execute(&tmp.0, &task, "desktop__invoke", &invoke)
            .await?["ok"],
        true
    );
    assert_eq!(inputs.load(Ordering::SeqCst), 1);
    Ok(())
}
