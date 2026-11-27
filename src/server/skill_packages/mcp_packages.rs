//! Lifecycle and catalog adapter for host-configured standard MCP servers.
use super::{
    Package, PackagePlugin, Registry,
    mcp_client::{self, Config, Connection, Diagnostics},
};
use crate::runtime::agent_program::libraries;
use crate::server::{
    plugin_protocol::{Manifest, NativeContributions, NativeTool, Origin, ToolPermission},
    plugin_runtime::PluginRuntime,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub(super) struct Server {
    name: String,
    config: Option<Config>,
    operation: tokio::sync::Mutex<()>,
    state: Mutex<State>,
}

struct State {
    enabled: bool,
    connecting: bool,
    diagnostics: Arc<Diagnostics>,
}

impl Server {
    fn status(&self, id: &str, package: Option<&Package>) -> Value {
        let state = self.state.lock().unwrap();
        let connected = package.is_some_and(Package::available);
        json!({
            "id":id,"name":self.name,"library":id.replace('-', "_"),"enabled":state.enabled,
            "status":if state.connecting { "connecting" } else if connected { "connected" } else { "disconnected" },
            "transport":self.config.as_ref().map_or("invalid", Config::transport),
            "tool_count":package.filter(|_| connected).map_or(0, |p| p.manifest.tools.len()),
            "last_error":state.diagnostics.error(),"stderr_bytes":state.diagnostics.stderr_bytes(),
        })
    }
}

impl Registry {
    pub(crate) fn is_mcp(&self, id: &str) -> bool {
        self.mcp_servers.read().unwrap().contains_key(id)
    }

    pub(crate) fn mcp_status(&self) -> Value {
        let entries = self.entries.read().unwrap();
        json!(
            self.mcp_servers
                .read()
                .unwrap()
                .iter()
                .map(|(id, server)| server.status(id, entries.get(id)))
                .collect::<Vec<_>>()
        )
    }

    pub(crate) fn mcp_tool(&self, name: &str) -> Result<NativeTool> {
        self.entries
            .read()
            .unwrap()
            .values()
            .filter(|p| p.mcp.is_some() && p.available())
            .find_map(|p| p.native.as_ref()?.tools.get(name).cloned())
            .context("MCP tool unavailable; check the connection in plugin settings")
    }

    #[cfg(test)]
    pub(crate) fn mcp_stderr(&self, id: &str) -> String {
        self.mcp_servers.read().unwrap()[id]
            .state
            .lock()
            .unwrap()
            .diagnostics
            .stderr()
    }
}

pub(crate) async fn restore(runtime: Arc<PluginRuntime>, registry: Arc<Registry>) -> Result<()> {
    let Some(path) = mcp_client::config_path() else {
        return Ok(());
    };
    let servers = mcp_client::read_config(&path).await?;
    if servers.is_empty() {
        return Ok(());
    }
    let enabled = super::settings()?.enabled;
    register(&runtime, &registry, servers, &enabled);
    Ok(())
}

fn stable_suffix(name: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(name.as_bytes()))[..12].to_owned()
}

fn server_ids(names: impl Iterator<Item = String>) -> Vec<(String, String)> {
    let mut used = BTreeSet::new();
    names
        .map(|name| {
            let slug: String = name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .take(40)
                .collect();
            let slug = slug
                .split('-')
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("-");
            let slug = if slug.is_empty() { "server" } else { &slug };
            let mut id = format!("mcp-{slug}");
            if !used.insert(id.clone()) {
                id = format!("{id}-{}", stable_suffix(&name));
                while !used.insert(id.clone()) {
                    id = format!("mcp-{slug}-{}", stable_suffix(&id));
                }
            }
            (id, name)
        })
        .collect()
}

fn register(
    runtime: &Arc<PluginRuntime>,
    registry: &Arc<Registry>,
    raw: BTreeMap<String, Value>,
    overrides: &BTreeMap<String, bool>,
) {
    let ids = server_ids(raw.keys().cloned());
    let mut roster = registry.mcp_servers.write().unwrap();
    for (id, name) in ids {
        let config = serde_json::from_value::<Config>(raw[&name].clone()).ok();
        let enabled = overrides
            .get(&id)
            .copied()
            .or_else(|| config.as_ref().and_then(|c| c.enabled))
            .or_else(|| raw[&name]["enabled"].as_bool())
            .unwrap_or(true);
        let server = Arc::new(Server {
            name,
            config,
            operation: Default::default(),
            state: Mutex::new(State {
                enabled,
                connecting: enabled,
                diagnostics: Arc::new(Diagnostics::default()),
            }),
        });
        roster.insert(id.clone(), server.clone());
        if enabled {
            let runtime = Arc::downgrade(runtime);
            let registry = Arc::downgrade(registry);
            tokio::spawn(async move {
                let _operation = server.operation.lock().await;
                if let (Some(runtime), Some(registry)) = (runtime.upgrade(), registry.upgrade())
                    && server.state.lock().unwrap().connecting
                {
                    let _ = connect(&runtime, &registry, &server, &id).await;
                }
            });
        }
    }
}

pub(crate) async fn set_enabled(
    runtime: &PluginRuntime,
    registry: &Registry,
    id: &str,
    enabled: bool,
    persist: bool,
) -> Result<Value> {
    let server = server(registry, id)?;
    let _operation = server.operation.lock().await;
    if persist {
        let _transition = registry.transition.lock().unwrap();
        let mut config = super::settings()?;
        config.enabled.insert(id.to_owned(), enabled);
        super::save_settings(&config)?;
    }
    {
        let mut state = server.state.lock().unwrap();
        state.enabled = enabled;
        state.connecting = false;
    }
    if enabled {
        let live = registry
            .entries
            .read()
            .unwrap()
            .get(id)
            .is_some_and(Package::available);
        if !live {
            connect(runtime, registry, &server, id).await?;
        }
    } else {
        unmount(runtime, registry, id)?;
    }
    Ok(registry.catalog())
}

pub(crate) async fn reconnect(
    runtime: &PluginRuntime,
    registry: &Registry,
    id: &str,
) -> Result<Value> {
    let server = server(registry, id)?;
    let _operation = server.operation.lock().await;
    ensure!(
        server.state.lock().unwrap().enabled,
        "MCP server is disabled"
    );
    connect(runtime, registry, &server, id).await?;
    Ok(registry.catalog())
}

fn server(registry: &Registry, id: &str) -> Result<Arc<Server>> {
    registry
        .mcp_servers
        .read()
        .unwrap()
        .get(id)
        .cloned()
        .context("MCP server not found")
}

fn unmount(runtime: &PluginRuntime, registry: &Registry, id: &str) -> Result<()> {
    let _transition = registry.transition.lock().unwrap();
    let scope = registry
        .entries
        .read()
        .unwrap()
        .get(id)
        .filter(|p| p.mcp.is_some())
        .and_then(|p| p.scope);
    if let Some(scope) = scope {
        runtime.unmount(scope)?;
    }
    let mut entries = registry.entries.write().unwrap();
    if entries.get(id).is_some_and(|p| p.mcp.is_some()) {
        entries.remove(id);
    }
    Ok(())
}

async fn connect(
    runtime: &PluginRuntime,
    registry: &Registry,
    server: &Server,
    id: &str,
) -> Result<()> {
    let diagnostics = Arc::new(Diagnostics::default());
    {
        let mut state = server.state.lock().unwrap();
        state.connecting = true;
        state.diagnostics = diagnostics.clone();
    }
    // An aborted management HTTP request must not leave the status stuck at connecting.
    struct Attempt<'a>(&'a Server, &'a Diagnostics);
    impl Drop for Attempt<'_> {
        fn drop(&mut self) {
            let mut state = self.0.state.lock().unwrap();
            if state.connecting {
                self.1
                    .disconnect(&anyhow::anyhow!("MCP connection attempt cancelled"));
                state.connecting = false;
            }
        }
    }
    let attempt = Attempt(server, &diagnostics);
    let result = async {
        unmount(runtime, registry, id)?;
        let config = server
            .config
            .as_ref()
            .context("invalid MCP server configuration")?;
        let (connection, tools) = Connection::connect(config, diagnostics.clone()).await?;
        ensure!(connection.connected(), "MCP server closed during discovery");
        let package = package(id, &server.name, Arc::new(connection), tools)?;
        let _transition = registry.transition.lock().unwrap();
        ensure!(
            !registry.entries.read().unwrap().contains_key(id),
            "MCP package ID already in use"
        );
        let parent = runtime
            .snapshot()
            .into_iter()
            .find(|p| p.id == "skill-packages")
            .context("package service unavailable")?
            .scope;
        runtime.mount(parent, PackagePlugin(package))?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = &result {
        diagnostics.disconnect(error);
        tracing::warn!(server = id, error = ?diagnostics.error(), "MCP server disconnected");
    }
    server.state.lock().unwrap().connecting = false;
    drop(attempt);
    result
}

fn permission(tool: &Value) -> ToolPermission {
    if tool["annotations"]["readOnlyHint"] == true && tool["annotations"]["destructiveHint"] != true
    {
        ToolPermission::ReadOnly
    } else {
        ToolPermission::Execution
    }
}

fn method_base(raw: &str) -> String {
    let mut method: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    if !libraries::identifier(&method)
        || libraries::reserved(&method)
        || matches!(
            method.as_str(),
            "promises" | "then" | "__proto__" | "constructor" | "prototype"
        )
    {
        method.insert(0, '_');
    }
    method
}

fn package(
    id: &str,
    title: &str,
    connection: Arc<Connection>,
    tools: Vec<Value>,
) -> Result<Package> {
    let library = id.replace('-', "_");
    let mut native = NativeContributions::default();
    let mut by_name = BTreeMap::new();
    for definition in tools {
        let name = definition["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .context("MCP tool name required")?
            .to_owned();
        ensure!(
            by_name.insert(name, definition).is_none(),
            "duplicate MCP tool name"
        );
    }
    // Preserve already-valid names when a normalized name would otherwise shadow them.
    let mut used = by_name
        .keys()
        .filter(|raw| method_base(raw) == **raw)
        .cloned()
        .collect::<BTreeSet<_>>();
    for (raw, tool) in by_name {
        ensure!(
            tool["inputSchema"]["type"] == "object",
            "MCP tool requires an object schema"
        );
        let base = method_base(&raw);
        let mut method = base.clone();
        if method != raw && !used.insert(method.clone()) {
            method = format!("{base}_{}", stable_suffix(&raw));
            while !used.insert(method.clone()) {
                method = format!("{base}_{}", stable_suffix(&method));
            }
        }
        let name = format!("{library}__{method}");
        let description = tool["description"].as_str().unwrap_or_default();
        let mut definition =
            json!({"name":name,"description":description,"inputSchema":tool["inputSchema"]});
        // MCP outputSchema describes structuredContent, not the full tools/call result.
        let mut properties = json!({"content":{"type":"array"},"isError":{"type":"boolean"}});
        if tool["outputSchema"].is_object() {
            properties["structuredContent"] = json!({"type":"object"});
            let original = &tool["outputSchema"];
            // A schema with refs is still available verbatim from host.contracts.
            definition["mcpOutputSchema"] = original.clone();
        }
        definition["outputSchema"] =
            json!({"type":"object","properties":properties,"required":["content"]});
        let connection = connection.clone();
        native.tools.insert(
            name,
            NativeTool {
                definition,
                permission: permission(&tool),
                handler: Arc::new(move |_, _, args| {
                    let connection = connection.clone();
                    let raw = raw.clone();
                    Box::pin(async move { connection.call(&raw, args).await })
                }),
            },
        );
    }
    Ok(Package {
        root: PathBuf::new(),
        manifest: Manifest {
            version: 1,
            enabled_by_default: true,
            id: id.to_owned(),
            library: Some(library),
            title: title.to_owned(),
            skill: String::new(),
            inject: vec![],
            tools: native.tools.keys().cloned().collect(),
            command_tools: vec![],
            pages: vec![],
            attachment_extensions: None,
            local_service: None,
            prompt_profile: None,
        },
        description: "Standard MCP server".into(),
        scope: None,
        origin: Origin::External,
        native: Some(Arc::new(native)),
        remote: None,
        mcp: Some(connection),
        local_service_port: None,
        local_service_child: None,
    })
}

#[cfg(test)]
pub(crate) async fn mount_test_server(
    runtime: &Arc<PluginRuntime>,
    registry: &Arc<Registry>,
    name: &str,
    config: Value,
) {
    let id = server_ids(std::iter::once(name.to_owned())).remove(0).0;
    register(
        runtime,
        registry,
        BTreeMap::from([(name.into(), config)]),
        &BTreeMap::new(),
    );
    tests::status(registry, &id, "connected").await;
}

#[cfg(test)]
#[path = "mcp_client_tests.rs"]
mod tests;
