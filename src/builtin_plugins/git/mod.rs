//! Git's own backend contributions; the host knows no git methods.
mod backend;
mod remote;
use crate::{
    plugin_protocol::{NativeContributions, NativeTool, ToolPermission},
    skill_packages::Package,
};
use anyhow::Result;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

fn root_of(workspace: &crate::tools::Workspace, args: &serde_json::Value) -> PathBuf {
    args["workspace_root"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.root().to_owned())
}

pub fn package() -> Result<Package> {
    let read: crate::builtin_plugins::ToolHandler = Arc::new(|workspace, _, args| {
        Box::pin(async move {
            let root = root_of(&workspace, &args);
            backend::read(&root, serde_json::from_value(args)?).await
        })
    });
    let sync: crate::builtin_plugins::ToolHandler = Arc::new(|workspace, _, args| {
        Box::pin(async move {
            let root = root_of(&workspace, &args);
            backend::sync(&root, serde_json::from_value(args)?).await
        })
    });
    let remote: crate::builtin_plugins::ToolHandler = Arc::new(|_, _, args| {
        Box::pin(async move { remote::read(serde_json::from_value(args)?).await })
    });
    let mut tools = BTreeMap::new();
    // Fetch, pull and push reach the network and pull rewrites files: they run
    // only where commands may run.
    for definition in backend::tool_definitions() {
        let name = definition["name"].as_str().unwrap().to_owned();
        let (handler, permission) = match name.as_str() {
            "read_git" => (read.clone(), ToolPermission::ReadOnly),
            "read_remote_git" => (remote.clone(), ToolPermission::ReadOnly),
            _ => (sync.clone(), ToolPermission::Execution),
        };
        tools.insert(
            name,
            NativeTool {
                definition,
                handler,
                permission,
            },
        );
    }
    let contributions = Arc::new(NativeContributions {
        tools,
        frontend_entries: vec!["git.history".into()],
    });
    let root = std::env::var_os("GROW_RESOURCES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    crate::skill_packages::load_builtin(&root.join("src/builtin_plugins/git"), contributions)
}

#[cfg(test)]
mod tests {
    #[test]
    fn remote_tool_is_declared_and_has_a_registered_handler() {
        super::package().expect("Git plugin manifest, skill and tool handlers must agree");
        let definitions = super::backend::tool_definitions();
        let remote = definitions.iter().find(|d| d["name"] == "read_remote_git").unwrap();
        assert!(remote["description"].as_str().unwrap().contains("without cloning"));
    }

    #[tokio::test]
    async fn remote_tool_is_available_to_the_agent_library() {
        use crate::plugin_runtime::{Plugin, PluginContext, PluginRuntime, ROOT};
        use std::sync::Arc;
        struct Services(Arc<crate::builtin_plugins::ToolCatalog>);
        impl Plugin for Services {
            fn id(&self) -> &str { "test-services" }
            fn start(&self, ctx: &PluginContext<'_>) -> anyhow::Result<()> {
                ctx.provide("tools", self.0.clone())?;
                ctx.provide("skill-packages", self.0.packages.clone())
            }
        }
        let runtime = PluginRuntime::new();
        let tools = Arc::new(crate::builtin_plugins::ToolCatalog::default());
        let scope = runtime.mount(ROOT, Services(tools.clone())).unwrap();
        runtime.mount(scope, crate::skill_packages::PackagePlugin(super::package().unwrap())).unwrap();
        let definitions = tools.packages.tools(&tools.packages.enabled());
        let libraries = crate::agent_program::libraries::catalog(&definitions, &tools.packages.catalog());
        assert_eq!(libraries["git"]["read_remote_git"], "read_remote_git");
        assert!(tools.packages.allows("read_remote_git", crate::agent_service::PermissionMode::ReadOnly, false));
        let workspace = Arc::new(crate::tools::Workspace::new(std::env::current_dir().unwrap()).unwrap());
        let error = tools.execute(workspace, "read_remote_git", serde_json::json!({"repository":"https://example.com/a/b", "view":"overview"})).await.unwrap_err();
        assert!(error.to_string().contains("github.com"));
        runtime.shutdown().unwrap();
    }
}
