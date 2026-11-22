//! Workspace and host tool capabilities.

pub(crate) mod approval_review;
pub(crate) mod batch_edits;
pub(crate) mod browser_control;
pub(crate) mod file_edit;
pub(crate) mod git_tools;
pub(crate) mod http_probe;
pub(crate) mod page_tools;
pub(crate) mod plan_tools;
pub(crate) mod process_sandbox;
pub(crate) mod program_execution;
pub(crate) mod project_process;
pub(crate) mod shell_environment;
pub(crate) mod shell_process;
pub(crate) mod tool_approvals;
pub(crate) mod tool_registry;
pub(crate) mod workspace;
pub(crate) mod verify_tools;
pub(crate) mod workspace_changes;

pub(crate) use workspace::*;
