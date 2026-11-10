//! Project-owned plugin roster. Filesystem location never determines origin.
pub mod code_flow;
pub mod code_index;
pub mod git;
pub mod web;
mod services;
pub use services::*;

pub fn packages() -> anyhow::Result<Vec<crate::skill_packages::Package>> {
    Ok(vec![code_flow::package()?, git::package()?, code_index::package()?, web::package()?])
}
