//! Project-owned plugin roster. Filesystem location never determines origin.
pub mod code_flow;
pub mod code_index;
pub mod git;
pub mod web;
mod services;
pub use services::*;

/// What the user still has to set up before a built-in library's calls can
/// work, if anything. Shown wherever the AI learns about the library, so it
/// does not plan around methods that are bound to fail.
pub fn setup_needed(library: &str) -> Option<String> {
    match library {
        "web" => web::setup_needed(&crate::ai_proxy::config_path()),
        _ => None,
    }
}

pub fn packages() -> anyhow::Result<Vec<crate::skill_packages::Package>> {
    Ok(vec![code_flow::package()?, git::package()?, code_index::package()?, web::package()?])
}
