//! Code Flow's own backend contributions; the host knows no Code Flow methods.
mod backend;
use crate::{
    server::plugin_protocol::{NativeContributions, NativeTool, ToolPermission},
    server::skill_packages::Package,
};
use anyhow::Result;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

pub fn package() -> Result<Package> {
    let definition = backend::tool_definition();
    let name = definition["name"].as_str().unwrap().to_owned();
    let handler: crate::server::builtin_plugins::ToolHandler = Arc::new(|workspace, _, args| {
        Box::pin(async move {
            let root = args["workspace_root"]
                .as_str()
                .map(PathBuf::from)
                .unwrap_or_else(|| workspace.root().to_owned());
            tokio::task::spawn_blocking(move || backend::read(&root, serde_json::from_value(args)?))
                .await?
        })
    });
    let contributions = Arc::new(NativeContributions {
        tools: BTreeMap::from([(
            name,
            NativeTool {
                definition,
                handler,
                permission: ToolPermission::ReadOnly,
            },
        )]),
        frontend_entries: vec!["code-flow.architecture".into()],
        on_unmount: None,
    });
    let root = std::env::var_os("GROW_RESOURCES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    crate::server::skill_packages::load_builtin(&root.join("src/server/builtin_plugins/code_flow"), contributions)
}
