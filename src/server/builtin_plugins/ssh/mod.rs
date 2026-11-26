//! Interactive SSH workbench and AI remote operations tools.
mod client;
mod credentials;
pub(crate) mod live;
mod package;
mod pty;
pub(crate) mod routes;
mod sftp;
mod store;

pub(super) use package::package;
pub(crate) use package::{is_tool, requires_approval, scoped};

#[cfg(all(test, unix))]
mod integration_tests;
