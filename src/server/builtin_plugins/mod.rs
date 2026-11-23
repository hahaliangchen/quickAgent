//! Project-owned plugin roster. Filesystem location never determines origin.
pub mod code_flow;
pub mod code_index;
pub(crate) mod desktop;
pub mod git;
pub(crate) mod ssh;
pub mod web;
mod services;
pub use services::*;


pub fn packages() -> anyhow::Result<Vec<crate::server::skill_packages::Package>> {
    Ok(vec![code_flow::package()?, git::package()?, code_index::package()?, web::package()?, desktop::package()?, ssh::package()?])
}
