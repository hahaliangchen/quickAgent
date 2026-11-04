//! Git's own backend contributions; the host knows no git methods.
mod backend;
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
    let mut tools = BTreeMap::new();
    // Fetch, pull and push reach the network and pull rewrites files: they run
    // only where commands may run.
    for definition in backend::tool_definitions() {
        let name = definition["name"].as_str().unwrap().to_owned();
        let (handler, permission) = match name.as_str() {
            "read_git" => (read.clone(), ToolPermission::ReadOnly),
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
