//! Runtime boundary for VM tool traps and permission-checked host calls.

use super::{Session, Window, host_tools, tool_proxy};
use crate::agent_program::{self, libraries::Libraries};
use anyhow::Result;
use serde_json::Value;

pub(super) struct ToolExecutor<'s, 'a> {
    session: &'s Session<'a>,
}

impl<'s, 'a> ToolExecutor<'s, 'a> {
    pub(super) fn new(session: &'s Session<'a>) -> Self {
        Self { session }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn proxy_vm_tool(
        &self,
        libs: &Libraries,
        node: &str,
        step: usize,
        call_id: &str,
        name: &str,
        args: Value,
        allow_shell: bool,
    ) -> Result<(Value, bool)> {
        tool_proxy::proxy_rust_tool(
            self.session,
            libs,
            node,
            step,
            call_id,
            name,
            args,
            allow_shell,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn dispatch_host_call(
        &self,
        node: &str,
        name: &str,
        args: Value,
        call_id: &str,
        allow_shell: bool,
        program_read: bool,
    ) -> Result<Value> {
        host_tools::business_tool_inner(
            self.session,
            node,
            name,
            args,
            call_id,
            allow_shell,
            program_read,
        )
        .await
    }
}

/// Static host library scope available to a compiled JS program / flow library.
pub(crate) fn library_compile_scope(
    s: &Session<'_>,
    w: &Window,
) -> agent_program::libraries::Libraries {
    let allow_shell = s.plugin.is_none()
        && !s.ban_execution
        && w.program_task
            .as_ref()
            .is_none_or(|t| t["role"].as_str() != Some("leaf"));
    let mut libs = agent_program::libraries::builtin();
    if !allow_shell {
        libs.remove("process");
    } else if !crate::shell_process::read_only_available()
        && let Some(process_lib) = libs.get_mut("process")
    {
        process_lib.remove("run_readonly");
    }
    if s.plugin.is_some() {
        for methods in libs.values_mut() {
            methods.retain(|_, tool| host_tools::page_allows(s, tool));
        }
        libs.retain(|_, methods| !methods.is_empty());
    }
    if let Some(catalog) = s.state.tool_catalog.as_ref() {
        let mut packages = catalog.packages.catalog();
        if let Some(owner) = s.plugin.as_deref() {
            if let Some(entries) = packages.as_array_mut() {
                entries.retain(|package| package["id"] == owner);
            }
        }
        let active = packages
            .as_array()
            .into_iter()
            .flatten()
            .filter(|package| package["enabled"] == true)
            .filter_map(|package| package["id"].as_str().map(str::to_owned))
            .collect::<std::collections::BTreeSet<_>>();
        let plugin_definitions = catalog.packages.tools(&active);
        for (name, methods) in agent_program::libraries::catalog(&plugin_definitions, &packages) {
            if !libs.contains_key(&name) {
                libs.insert(name, methods);
            }
        }
    }
    libs
}
