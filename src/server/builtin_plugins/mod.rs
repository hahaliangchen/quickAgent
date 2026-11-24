//! Project-owned plugin roster. Filesystem location never determines origin.
pub(crate) mod blueprint;
pub mod code_index;
#[cfg(feature = "internal-daily-report")]
pub(crate) mod daily_report;
pub(crate) mod desktop;
pub mod git;
pub(crate) mod ssh;
pub mod web;
mod services;
pub use services::*;

use std::future::Future;

/// Submitting a team daily report always asks a human first. The feature gate
/// keeps that question out of every build that has no such plugin.
pub(crate) fn daily_report_requires_approval(name: &str, args: &serde_json::Value) -> bool {
    #[cfg(feature = "internal-daily-report")]
    {
        return daily_report::requires_approval(name, args);
    }
    #[cfg(not(feature = "internal-daily-report"))]
    {
        let _ = (name, args);
        false
    }
}

/// Plugin tools that need the calling task inside their handler scope (the
/// daily report uses it to bring its page forward). Plain tools just run.
pub(crate) async fn scoped_plugin<F: Future>(task: String, approved: bool, action: F) -> F::Output {
    #[cfg(feature = "internal-daily-report")]
    return daily_report::scoped(task, approved, action).await;
    #[cfg(not(feature = "internal-daily-report"))]
    {
        let _ = (task, approved);
        action.await
    }
}

#[allow(unused_mut)]
pub fn packages() -> anyhow::Result<Vec<crate::server::skill_packages::Package>> {
    let mut roster = vec![blueprint::package()?, git::package()?, code_index::package()?, web::package()?, desktop::package()?, ssh::package()?];
    #[cfg(feature = "internal-daily-report")]
    roster.push(daily_report::package()?);
    Ok(roster)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The company plugin ships as source only. A build without the feature
    /// must expose neither its page nor its tools, so this test fails loudly
    /// the day someone turns it on by accident.
    #[test]
    fn roster_matches_the_internal_daily_report_feature() {
        let packages = packages().expect("builtin packages load");
        let ids = packages.iter().map(|package| package.manifest.id.clone()).collect::<Vec<_>>();
        assert!(ids.iter().any(|id| id == "ssh"), "the roster still carries its public plugins: {ids:?}");
        assert_eq!(
            ids.iter().any(|id| id == "daily-report"),
            cfg!(feature = "internal-daily-report"),
            "daily-report must follow the feature switch: {ids:?}"
        );
    }

    /// Tool names are visible to the model even when the page is closed, so a
    /// leaked name would still advertise the company system.
    #[test]
    fn default_roster_offers_no_daily_report_tools() {
        if cfg!(feature = "internal-daily-report") {
            return;
        }
        let leaked = packages()
            .expect("builtin packages load")
            .into_iter()
            .flat_map(|package| package.manifest.tools)
            .filter(|name| name.starts_with("daily_report__"))
            .collect::<Vec<_>>();
        assert!(leaked.is_empty(), "default builds advertise {leaked:?}");
    }
}
