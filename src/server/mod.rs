//! HTTP, MCP, and plugin services.

pub(crate) mod agent_service;
pub(crate) mod agent_web;
pub(crate) mod ai_proxy;
pub(crate) mod builtin_plugins;
pub(crate) mod composer_catalog;
pub(crate) mod file_stream;
mod local_api_proxy;
pub(crate) mod mcp;
pub(crate) mod plugin_io;
pub(crate) mod plugin_jobs;
pub(crate) mod plugin_protocol;
pub(crate) mod plugin_runtime;
pub(crate) mod plugin_sessions;
pub(crate) mod remote_sessions;
pub(crate) mod skill_packages;
pub(crate) mod workflow_drafts;
