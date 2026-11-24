mod impact;
mod model;
mod package;
pub(crate) mod routes;
mod store;
#[cfg(test)]
mod tests;
mod verify;

pub(super) use package::package;

use std::path::Path;
use store::Store;

pub(crate) fn workspace_has_blueprint(root: &Path) -> bool {
    std::fs::symlink_metadata(root.join(".quickagent/blueprint/manifest.json"))
        .is_ok_and(|m| m.is_file())
}

pub(crate) fn workspace_blueprint_hint(root: &Path) -> Option<String> {
    if !workspace_has_blueprint(root) {
        return None;
    }
    let store = Store::open(root).ok()?;
    let manifest = store.manifest().ok()??;
    Some(format!(
        "[Project Blueprint] Available (.quickagent/blueprint, {} graphs). Before editing cross-layer code or shared modules, inspect `blueprint.overview({{}})` or `blueprint.impact({{ file_path }})` for dependencies, invariants, and test guards; use `blueprint.verify_sync({{}})` for freshness.",
        manifest.graphs.len()
    ))
}
