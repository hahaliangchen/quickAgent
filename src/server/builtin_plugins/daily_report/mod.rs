//! Internal team daily report. The module is compiled only with the
//! non-default `internal-daily-report` feature, so customer and desktop builds
//! never contain its routes, tools or company endpoints.
mod activity;
mod client;
mod crypto;
mod package;
pub(crate) mod routes;
mod store;
#[cfg(test)]
mod tests;

pub(crate) use package::{package, requires_approval, scoped};
