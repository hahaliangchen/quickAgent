//! Built-in plugins bridging the existing workspace capabilities into the
//! scoped runtime. The MCP implementations remain the single tool backend.

use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
};

use anyhow::{Result, ensure};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    server::agent_service::AgentServiceState,
    server::ai_proxy::AiProxyConfig,
    server::plugin_runtime::{Plugin, PluginContext},
    tools::tool_registry::{CatalogKind, ToolRegistry},
    tools::Workspace,
};

#[derive(Default)]
pub struct LifecycleLog {
    entries: Mutex<VecDeque<Value>>,
}

impl LifecycleLog {
    fn push(&self, event: &str, data: &Value) {
        let mut entries = self.entries.lock().unwrap();
        entries.push_back(json!({"event":event,"data":data}));
        if entries.len() > 100 {
            entries.pop_front();
        }
    }
    pub fn snapshot(&self) -> Vec<Value> {
        self.entries.lock().unwrap().iter().cloned().collect()
    }
}

pub struct DiagnosticsPlugin;
impl Plugin for DiagnosticsPlugin {
    fn id(&self) -> &'static str {
        "diagnostics"
    }
    fn start(&self, context: &PluginContext<'_>) -> Result<()> {
        let log = Arc::new(LifecycleLog::default());
        context.provide("plugin-events", log.clone())?;
        let started = log.clone();
        context.on("plugin/started", move |data| {
            started.push("plugin/started", data)
        });
        context.on("plugin/stopped", move |data| {
            log.push("plugin/stopped", data)
        });
        Ok(())
    }
}

pub struct WorkspacePlugin(pub Arc<Workspace>);

impl Plugin for WorkspacePlugin {
    fn id(&self) -> &'static str {
        "workspace"
    }
    fn start(&self, context: &PluginContext<'_>) -> Result<()> {
        context.provide("workspace", self.0.clone())
    }
}

pub struct ModelSettings {
    pub path: PathBuf,
    pub config: Arc<tokio::sync::RwLock<AiProxyConfig>>,
}

pub struct ModelSettingsPlugin(pub Arc<ModelSettings>);

impl Plugin for ModelSettingsPlugin {
    fn id(&self) -> &'static str {
        "model-settings"
    }
    fn start(&self, context: &PluginContext<'_>) -> Result<()> {
        context.provide("model-settings", self.0.clone())
    }
}

pub type ToolHandler =
    Arc<dyn Fn(Arc<Workspace>, String, Value) -> BoxFuture<'static, Result<Value>> + Send + Sync>;

#[derive(Default)]
pub struct ToolCatalog {
    handlers: RwLock<BTreeMap<String, ToolHandler>>,
    pub packages: Arc<crate::server::skill_packages::Registry>,
}

impl ToolCatalog {
    pub fn register(&self, names: &[&str], handler: ToolHandler) -> Result<()> {
        let mut installed = self.handlers.write().unwrap();
        for name in names {
            ensure!(
                !installed.contains_key(*name),
                "tool {name} is already registered"
            );
        }
        installed.extend(
            names
                .iter()
                .map(|name| ((*name).to_owned(), handler.clone())),
        );
        Ok(())
    }

    pub fn unregister(&self, names: &[&str]) {
        let mut installed = self.handlers.write().unwrap();
        for name in names {
            installed.remove(*name);
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.handlers.read().unwrap().contains_key(name)
    }

    pub async fn execute(
        &self,
        workspace: Arc<Workspace>,
        name: &str,
        args: Value,
    ) -> Result<Value> {
        let handler = self
            .handlers
            .read()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("tool {name} is not registered"))?;
        handler(workspace, name.to_owned(), args).await
    }
}

fn mcp_handler() -> ToolHandler {
    Arc::new(|workspace, name, mut args| {
        Box::pin(async move {
            if let Some(object) = args.as_object_mut() {
                object.insert(
                    "workspace_root".into(),
                    json!(workspace.root().display().to_string()),
                );
            }
            let response =
                crate::server::mcp::call_tool(&workspace, json!({"name":name,"arguments":args})).await?;
            Ok(response
                .get("structuredContent")
                .cloned()
                .unwrap_or(response))
        })
    })
}

fn program_handler() -> ToolHandler {
    Arc::new(|workspace, _, args| {
        Box::pin(async move { crate::tools::program_execution::execute(&workspace, &args).await })
    })
}

pub struct ToolCatalogPlugin;

impl Plugin for ToolCatalogPlugin {
    fn id(&self) -> &'static str {
        "tools"
    }
    fn requires(&self) -> Vec<String> {
        ["workspace"].into_iter().map(str::to_owned).collect()
    }
    fn start(&self, context: &PluginContext<'_>) -> Result<()> {
        context.provide("tools", Arc::new(ToolCatalog::default()))?;
        context.mount(FileToolsPlugin)?;
        context.mount(MemoryPlugin)?;
        context.mount(crate::server::skill_packages::PackagesPlugin)?;
        context.mount(AgentTasksPlugin)?;
        Ok(())
    }
}

pub(crate) const FILE_TOOLS: &[&str] = &ToolRegistry::catalog_names::<
    { ToolRegistry::catalog_len(CatalogKind::File) },
>(CatalogKind::File);
const MEMORY_TOOLS: &[&str] = &ToolRegistry::catalog_names::<
    { ToolRegistry::catalog_len(CatalogKind::Memory) },
>(CatalogKind::Memory);

fn register_tools(
    context: &PluginContext<'_>,
    names: &'static [&'static str],
    handler: ToolHandler,
) -> Result<()> {
    let catalog = context.require::<ToolCatalog>("tools")?;
    catalog.register(names, handler)?;
    context.effect(move || catalog.unregister(names));
    Ok(())
}

pub struct FileToolsPlugin;
impl Plugin for FileToolsPlugin {
    fn id(&self) -> &'static str {
        "file-tools"
    }
    fn requires(&self) -> Vec<String> {
        ["tools", "workspace"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
    fn start(&self, context: &PluginContext<'_>) -> Result<()> {
        register_tools(context, FILE_TOOLS, mcp_handler())?;
        register_tools(context, crate::tools::project_process::TOOLS, mcp_handler())?;
        register_tools(context, crate::tools::http_probe::TOOLS, mcp_handler())?;
        register_tools(context, crate::tools::program_execution::TOOLS, program_handler())?;
        register_tools(context, crate::tools::verify_tools::TOOLS, mcp_handler())?;
        register_tools(context, crate::tools::plan_tools::TOOLS, mcp_handler())?;
        register_tools(context, crate::tools::git_tools::TOOLS, mcp_handler())?;
        register_tools(context, &[crate::tools::batch_edits::NAME], mcp_handler())?;
        register_tools(context, crate::tools::browser_control::TOOLS, mcp_handler())?;
        context.provide("file-tools", Arc::new(()))
    }
}

pub struct MemoryPlugin;
impl Plugin for MemoryPlugin {
    fn id(&self) -> &'static str {
        "memory"
    }
    fn requires(&self) -> Vec<String> {
        ["tools", "workspace"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
    fn start(&self, context: &PluginContext<'_>) -> Result<()> {
        register_tools(context, MEMORY_TOOLS, mcp_handler())?;
        context.provide("memory", Arc::new(()))
    }
}

pub struct AgentTasksPlugin;

impl Plugin for AgentTasksPlugin {
    fn id(&self) -> &'static str {
        "agent-tasks"
    }
    fn requires(&self) -> Vec<String> {
        [
            "workspace",
            "model-settings",
            "tools",
            "file-tools",
            "memory",
            "skill-packages",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
    fn start(&self, context: &PluginContext<'_>) -> Result<()> {
        let workspace = context.require::<Workspace>("workspace")?;
        let settings = context.require::<ModelSettings>("model-settings")?;
        let tools = context.require::<ToolCatalog>("tools")?;
        let plugin_cancel = CancellationToken::new();
        let dispose_cancel = plugin_cancel.clone();
        context.effect(move || dispose_cancel.cancel());
        let mut state = AgentServiceState {
            workspace,
            model_client: crate::model::model_transport::client()?,
            model_request_slots: crate::server::agent_service::model_request_slots(),
            provider_url: String::new(),
            api_key: String::new(),
            provider_name: String::new(),
            provider_routes: Default::default(),
            default_model: String::new(),
            model_map: Default::default(),
            reasoning_effort: None,
            fast_mode: false,
            permission_mode: crate::server::agent_service::PermissionMode::default(),
            visual: Default::default(),
            visual_tool_settings: Default::default(),
            image_generation_settings: Default::default(),
            delegate_agent_settings: Default::default(),
            config: Some(settings.config.clone()),
            config_path: Some(settings.path.clone()),
            tool_catalog: Some(tools),
            plugin_cancel,
            cancellations: Arc::default(),
        };
        // Mounting captures a known-good model snapshot; each new task reloads it.
        if let Ok(config) = settings.config.try_read() {
            crate::server::ai_proxy::apply_agent_config(&mut state, &config);
        }
        context.provide("agent-tasks", Arc::new(state))
    }
}
