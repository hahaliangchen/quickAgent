//! Library facade over the existing syn/SWC indexes. No parser lives here.
use crate::{plugin_protocol::{NativeContributions, NativeTool, ToolPermission}, skill_packages::Package};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

fn execute(workspace: &crate::tools::Workspace, method: &str, mut args: Value) -> Result<Value> {
    let language = args["language"].as_str().context("language must be rust or typescript")?.to_owned();
    if !matches!(language.as_str(), "rust" | "typescript") {
        bail!("unsupported index language {language}; use rust or typescript (includes JS/JSX/TSX)");
    }
    let requested = args["workspace_root"].as_str().map(PathBuf::from);
    let root = match requested {
        Some(path) if path.is_absolute() => path,
        Some(path) => workspace.root().join(path),
        None => workspace.root().to_owned(),
    };
    args["workspace_root"] = Value::String(root.canonicalize()?.to_string_lossy().into_owned());
    args.as_object_mut().context("expected an object")?.remove("language");
    macro_rules! call {
        ($name:ident) => { serde_json::to_value(workspace.$name(serde_json::from_value(args)?)?).map_err(Into::into) };
    }
    match (language.as_str(), method) {
        ("rust", "index") => call!(index_rust_workspace),
        ("rust", "status") => call!(rust_index_status),
        ("rust", "list") => call!(list_rust_symbols),
        ("rust", "search") => call!(search_rust_symbols),
        ("rust", "read") => call!(read_rust_symbol),
        ("typescript", "index") => call!(index_ts_workspace),
        ("typescript", "status") => call!(ts_index_status),
        ("typescript", "list") => call!(list_ts_symbols),
        ("typescript", "search") => call!(search_ts_symbols),
        ("typescript", "read") => call!(read_ts_symbol),
        _ => bail!("unknown code_index method {method}"),
    }
}

pub fn package() -> Result<Package> {
    let definitions: Vec<Value> = serde_json::from_str(include_str!("tools.json"))?;
    let mut tools = BTreeMap::new();
    for definition in definitions {
        let name = definition["name"].as_str().context("tool name missing")?.to_owned();
        let handler: crate::builtin_plugins::ToolHandler = Arc::new(|workspace, name, args| {
            Box::pin(async move {
                let method = name.strip_prefix("code_index__").context("invalid index method")?.to_owned();
                tokio::task::spawn_blocking(move || execute(&workspace, &method, args)).await?
            })
        });
        tools.insert(name, NativeTool { definition, handler, permission: ToolPermission::ReadOnly });
    }
    let root = std::env::var_os("GROW_RESOURCES_DIR").map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    crate::skill_packages::load_builtin(&root.join("src/builtin_plugins/code_index"), Arc::new(NativeContributions {
        tools, frontend_entries: vec![],
    }))
}

#[cfg(test)]
mod tests;
