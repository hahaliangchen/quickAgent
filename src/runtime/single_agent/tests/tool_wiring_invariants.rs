//! Check the production catalog and the separate code-mode ABI together.
use super::TestWorkspace;
use crate::{
    runtime::agent_program::{declarations, libraries},
    server::agent_service::{self, PermissionMode},
    server::builtin_plugins::{
        ModelSettings, ModelSettingsPlugin, ToolCatalog, ToolCatalogPlugin, WorkspacePlugin,
    },
    server::plugin_runtime::{PluginRuntime, ROOT},
    tools::Workspace,
};
use serde_json::Value;
use std::sync::Arc;

fn catalog() -> (
    TestWorkspace,
    PluginRuntime,
    Arc<Workspace>,
    Arc<ToolCatalog>,
) {
    let dir = TestWorkspace::new("tool-wiring");
    let workspace = Arc::new(Workspace::new(dir.path()).unwrap());
    let runtime = PluginRuntime::new();
    runtime
        .mount(ROOT, WorkspacePlugin(workspace.clone()))
        .unwrap();
    runtime
        .mount(
            ROOT,
            ModelSettingsPlugin(Arc::new(ModelSettings {
                path: dir.path().join("settings.json"),
                config: Default::default(),
            })),
        )
        .unwrap();
    let scope = runtime.mount(ROOT, ToolCatalogPlugin).unwrap();
    let catalog = runtime.service(scope, "tools").unwrap();
    (dir, runtime, workspace, catalog)
}

fn dedicated_dispatch(name: &str) -> bool {
    // Session/visual context or a special sandbox is required; these calls
    // are dispatched by tool_proxy/host_tools rather than the local catalog.
    matches!(
        name,
        "ask_user"
            | "read_asset"
            | "snapshot"
            | "read_session_history"
            | "read_attachment"
            | "generate_image"
            | "register_image"
            | "view_image"
            | "analyze_images"
            | "image_analysis_results"
            | "read_tool_contracts"
            | "find_library_methods"
            | "read_program_guide"
            | "list_skills"
            | "read_skill"
            | "run_readonly_command"
    )
}

fn definition<'a>(defs: &'a [Value], name: &str) -> &'a Value {
    let mut found = defs
        .iter()
        .map(|v| &v["function"])
        .filter(|v| v["name"] == name);
    let def = found
        .next()
        .unwrap_or_else(|| panic!("{name} is missing from local_tools"));
    assert!(
        found.next().is_none(),
        "duplicate local_tools definition for {name}"
    );
    def
}

#[test]
fn builtin_host_tools_are_installed_and_published() {
    let (_dir, _runtime, workspace, catalog) = catalog();
    let defs = agent_service::local_tools(&workspace, Some(&catalog), false);
    let fixture_defs = agent_service::local_tools(&workspace, None, false);
    for (lib, methods) in libraries::builtin() {
        for (method, name) in methods {
            if dedicated_dispatch(&name) {
                continue;
            }
            assert!(
                catalog.contains(&name),
                "{lib}.{method}: {name} is not registered in production"
            );
            for tool in [definition(&defs, &name), definition(&fixture_defs, &name)] {
                assert!(tool["parameters"].is_object(), "{name} has no input schema");
                assert!(
                    tool["description"]
                        .as_str()
                        .is_some_and(|s| !s.trim().is_empty()),
                    "{name} has no description"
                );
            }
        }
    }
}

#[test]
fn read_only_rejects_all_tracked_workspace_mutations() {
    let defs = crate::server::mcp::tool_definitions();
    let libs = libraries::builtin();
    let names = defs
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["name"].as_str())
        .chain(
            libs.values()
                .flat_map(|methods| methods.values().map(String::as_str)),
        );
    for name in names.filter(|name| crate::tools::workspace_changes::is_mutation(name)) {
        assert!(
            !PermissionMode::ReadOnly.allows(name),
            "ReadOnly allows tracked workspace mutation {name}"
        );
    }
    for name in [
        "write_file",
        "edit_file",
        "replace_range",
        "rename_path",
        "delete_path",
        "create_dir",
        "run_program",
    ] {
        assert!(
            crate::tools::workspace_changes::is_mutation(name),
            "lost change tracking for {name}"
        );
    }
}

#[test]
fn all_builtin_methods_have_explicit_declarations() {
    for (library, methods) in libraries::builtin() {
        for (method, tool) in methods {
            let scope =
                libraries::Libraries::from([(library.clone(), [(method.clone(), tool)].into())]);
            let header = declarations::static_library_header(&scope);
            let prefix = format!("  function {method}(");
            let signature = header
                .lines()
                .find_map(|line| line.strip_prefix(&prefix))
                .unwrap_or_else(|| panic!("missing declaration for {library}.{method}"));
            assert!(
                signature.contains("): ")
                    && signature.ends_with(';')
                    && !signature.contains("args?: Record<string, unknown>): unknown"),
                "{library}.{method} has no explicit declaration: {signature}"
            );
        }
    }
}

#[test]
fn new_tool_contracts_cover_cross_layer_wiring() {
    let (_dir, _runtime, workspace, catalog) = catalog();
    let defs = agent_service::local_tools(&workspace, Some(&catalog), false);
    let libs = libraries::builtin();
    let prompt = include_str!("../model_call.rs");
    let visuals = include_str!("../../../../frontend/src/components/toolVisuals.tsx");
    let summary = include_str!("../../../../frontend/src/components/toolSummary.ts");
    let tools = [
        (
            "browser",
            "fill",
            "browser_fill",
            PermissionMode::FullAccess,
            true,
            false,
        ),
        (
            "browser",
            "set_viewport",
            "browser_set_viewport",
            PermissionMode::FullAccess,
            true,
            false,
        ),
        (
            "browser",
            "scroll",
            "browser_scroll",
            PermissionMode::FullAccess,
            true,
            false,
        ),
        (
            "browser",
            "screenshot",
            "browser_screenshot",
            PermissionMode::FullAccess,
            true,
            false,
        ),
        (
            "verify",
            "check",
            "verify_check",
            PermissionMode::WorkspaceWrite,
            true,
            true,
        ),
        (
            "verify",
            "test",
            "verify_test",
            PermissionMode::WorkspaceWrite,
            true,
            true,
        ),
        (
            "plan",
            "set",
            "plan_set",
            PermissionMode::ReadOnly,
            false,
            false,
        ),
        (
            "plan",
            "update",
            "plan_update",
            PermissionMode::ReadOnly,
            false,
            false,
        ),
        (
            "git",
            "blame",
            "git_blame",
            PermissionMode::ReadOnly,
            false,
            false,
        ),
        (
            "git",
            "commit",
            "git_commit",
            PermissionMode::WorkspaceWrite,
            true,
            true,
        ),
        (
            "git",
            "restore",
            "git_restore",
            PermissionMode::WorkspaceWrite,
            true,
            true,
        ),
        (
            "io",
            "apply_edits",
            "apply_edits",
            PermissionMode::WorkspaceWrite,
            false,
            true,
        ),
        // New phase entries above this marker.
    ];
    for (lib, method, tool, minimum, approval, tracked) in tools {
        let qualified = format!("{lib}.{method}");
        assert_eq!(
            libs.get(lib)
                .and_then(|m| m.get(method))
                .map(String::as_str),
            Some(tool)
        );
        assert!(
            {
                let schema = &definition(&defs, tool)["outputSchema"];
                let item = if schema["type"] == "array" {
                    &schema["items"]
                } else {
                    schema
                };
                item["properties"]
                    .as_object()
                    .is_some_and(|o| !o.is_empty())
            },
            "{qualified}: missing output schema"
        );
        assert!(
            prompt.contains(&format!("\x60{qualified}")),
            "{qualified}: no system prompt guidance"
        );
        assert!(
            visuals.contains(&format!("{tool}:")),
            "{qualified}: no explicit visual"
        );
        assert!(
            summary.contains(&format!("'{tool}'")),
            "{qualified}: no summary"
        );
        assert_eq!(
            crate::tools::workspace_changes::is_mutation(tool),
            tracked,
            "{qualified}: tracking"
        );
        for mode in [
            PermissionMode::ReadOnly,
            PermissionMode::WorkspaceWrite,
            PermissionMode::Ask,
            PermissionMode::AutoReview,
            PermissionMode::FullAccess,
        ] {
            let allowed = match minimum {
                PermissionMode::ReadOnly => true,
                PermissionMode::WorkspaceWrite => mode != PermissionMode::ReadOnly,
                PermissionMode::FullAccess => matches!(
                    mode,
                    PermissionMode::Ask | PermissionMode::AutoReview | PermissionMode::FullAccess
                ),
                _ => unreachable!(),
            };
            assert_eq!(mode.allows(tool), allowed, "{qualified}: {mode:?}");
            assert_eq!(
                crate::tools::tool_approvals::needs_approval(mode, tool, Some(&catalog), false),
                approval && matches!(mode, PermissionMode::Ask | PermissionMode::AutoReview),
                "{qualified}: approval {mode:?}"
            );
        }
    }
}

const MODES: [PermissionMode; 5] = [
    PermissionMode::ReadOnly,
    PermissionMode::WorkspaceWrite,
    PermissionMode::Ask,
    PermissionMode::AutoReview,
    PermissionMode::FullAccess,
];

// Stage 1 baseline, independent of the registry and the production classifiers.
const LEGACY_FILE_WRITES: &[&str] = &[
    "write_file",
    "replace_range",
    "edit_file",
    "delete_path",
    "rename_path",
    "create_dir",
];
const LEGACY_GIT_WRITES: &[&str] = &["git_commit", "git_restore"];
const LEGACY_VERIFY: &[&str] = &["verify_check", "verify_test"];
const LEGACY_PROCESS_EXECUTION: &[&str] = &[
    "run_program",
    "install_dependencies",
    "run_project_script",
    "stop_project_process",
];
const BROWSER_TOOLS: &[&str] = &[
    "browser_open",
    "browser_read",
    "browser_wait",
    "browser_diagnostics",
    "browser_hover",
    "browser_network",
    "browser_tabs",
    "browser_switch_tab",
    "browser_fill",
    "browser_set_viewport",
    "browser_scroll",
    "browser_click",
    "browser_press_key",
    "browser_upload",
    "browser_screenshot",
    "browser_close",
];

#[test]
fn builtin_permissions_approvals_and_tracking_match_legacy_behavior() {
    use crate::{tools::tool_approvals::needs_approval, tools::tool_registry::ToolRegistry};
    use PermissionMode::*;

    let (_dir, _runtime, _workspace, catalog) = catalog();
    for spec in ToolRegistry::all() {
        let name = spec.name;
        let file_write = LEGACY_FILE_WRITES.contains(&name);
        let git_write = LEGACY_GIT_WRITES.contains(&name);
        let verify = LEGACY_VERIFY.contains(&name);
        let project_execution = LEGACY_PROCESS_EXECUTION.contains(&name);
        let browser = BROWSER_TOOLS.contains(&name);
        let batch_edit = name == "apply_edits";
        let tracked = file_write
            || git_write
            || verify
            || batch_edit
            || (project_execution && name != "stop_project_process");
        assert_eq!(spec.tracks_mutation, tracked, "{name}: registry tracking");
        assert_eq!(
            crate::tools::workspace_changes::is_mutation(name),
            tracked,
            "{name}: tracking"
        );

        for mode in MODES {
            let asks = matches!(mode, Ask | AutoReview);
            let interactive = asks || mode == FullAccess;
            let allowed = if file_write || git_write || verify || batch_edit {
                mode != ReadOnly
            } else if project_execution {
                interactive || (cfg!(target_os = "linux") && mode == WorkspaceWrite)
            } else if browser {
                interactive
            } else {
                true
            };
            assert_eq!(mode.allows(name), allowed, "{name}: {mode:?} permission");

            for ban_execution in [false, true] {
                // The name-only check is used after the caller exempts empty stdin.
                let approval = match name {
                    "kill_session" => false,
                    "exec_command" | "write_stdin" if cfg!(windows) => {
                        ban_execution || mode != FullAccess
                    }
                    "exec_command" | "write_stdin" => asks,
                    _ => asks && (project_execution || browser || verify || git_write),
                };
                for catalog in [None, Some(catalog.as_ref())] {
                    assert_eq!(
                        needs_approval(mode, name, catalog, ban_execution),
                        approval,
                        "{name}: {mode:?}, ban_execution={ban_execution}, catalog={}",
                        catalog.is_some(),
                    );
                }
            }
        }
    }
}

#[test]
fn package_tools_still_use_dynamic_approval_policy() {
    use crate::{
        server::plugin_runtime::{Plugin, PluginContext},
        server::skill_packages::PackagePlugin,
        tools::tool_approvals::needs_approval,
        tools::tool_registry::ToolRegistry,
    };
    use PermissionMode::*;

    struct PackageServices(Arc<ToolCatalog>);
    impl Plugin for PackageServices {
        fn id(&self) -> &str {
            "test-package-services"
        }
        fn start(&self, ctx: &PluginContext<'_>) -> anyhow::Result<()> {
            ctx.provide("tools", self.0.clone())?;
            ctx.provide("skill-packages", self.0.packages.clone())
        }
    }

    let runtime = PluginRuntime::new();
    let catalog = Arc::new(ToolCatalog::default());
    let scope = runtime
        .mount(ROOT, PackageServices(catalog.clone()))
        .unwrap();
    runtime
        .mount(
            scope,
            PackagePlugin(crate::server::builtin_plugins::git::package().unwrap()),
        )
        .unwrap();

    for name in ["read_git", "sync_git"] {
        assert!(ToolRegistry::find(name).is_none());
        assert!(catalog.packages.declares_tool(name));
        for mode in MODES {
            assert!(mode.allows(name), "{name}: dynamic permission is separate");
            assert!(!needs_approval(mode, name, None, false));
            assert_eq!(
                needs_approval(mode, name, Some(&catalog), false),
                name == "sync_git" && matches!(mode, Ask | AutoReview),
                "{name}: {mode:?}",
            );
        }
    }
    runtime.shutdown().unwrap();
}
