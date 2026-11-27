//! Standalone task API for the local Rust agent. Events are append-only so a
//! client can recover a task's trajectory after reconnecting.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

mod db;
mod office_files;
mod routes;
mod streaming;
mod tools;

pub use db::acquire_service_lock_and_recover;
#[cfg(test)]
pub(crate) use db::{recover_orphaned_tasks, try_acquire_service_lock};
pub(crate) use db::{
    append_event_tx, emit, finish, next_event_seq, now, open_db, permission_mode_for,
    save_permission_mode,
};
pub use office_files::{office_local_file, office_recent_docs, office_reveal_folder};
pub use routes::{
    cancel_task, continue_task, create_draft_task, create_task, get_event, get_events, get_task, list_tasks, workspace_file,
    slash_commands, stream_events, update_task, workspace_files,
};
pub(crate) use streaming::consume_model_stream_observed;
pub(crate) use tools::{execute_tool, local_tools};

pub(crate) fn model_request_slots() -> Arc<tokio::sync::Semaphore> {
    let limit = std::env::var("CODEX_AGENT_MODEL_CONCURRENCY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&value| value > 0 && value <= tokio::sync::Semaphore::MAX_PERMITS)
        .unwrap_or(3);
    Arc::new(tokio::sync::Semaphore::new(limit))
}
// Acceptance may include history, code and image reads before its return.
// Use the same production window as an Organizer decision, not the former
// short summary-only window. A deadline never implies a business defect.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    ReadOnly,
    WorkspaceWrite,
    /// Edits proceed; execution waits for the user's approval (tool_approvals).
    #[default]
    Ask,
    /// As Ask, but an AI reviewer decides first and hands only unclear
    /// operations to the user (approval_review).
    AutoReview,
    FullAccess,
}

impl PermissionMode {
    /// The value stored with a chat and sent on the wire.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::WorkspaceWrite => "workspace_write",
            Self::Ask => "ask",
            Self::AutoReview => "auto_review",
            Self::FullAccess => "full_access",
        }
    }

    /// A value this build does not know reads as no choice at all, so callers
    /// fall back to asking first instead of granting more access.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        [
            Self::ReadOnly,
            Self::WorkspaceWrite,
            Self::Ask,
            Self::AutoReview,
            Self::FullAccess,
        ]
        .into_iter()
        .find(|mode| mode.as_str() == value)
    }

    pub(crate) fn allows(self, name: &str) -> bool {
        crate::tools::tool_registry::ToolRegistry::allows_in_mode(name, self).unwrap_or(true)
    }
}

#[derive(Clone, Debug)]
pub struct AgentProviderRoute {
    /// The provider speaks the Responses API (`api: openai-responses`), as
    /// Codex does; requests then replay the model's reasoning items.
    pub responses: bool,
    /// The provider speaks the Messages API (`api: anthropic-messages`);
    /// requests are projected onto that wire and its cache extensions.
    pub messages: bool,
    /// The Messages extensions each upstream model declares in its catalog
    /// entry (`api_capabilities.anthropic_messages`).
    pub messages_capabilities: HashMap<String, crate::model::anthropic_messages::Capabilities>,
    pub url: String,
    pub api_key: String,
    pub models: HashMap<String, String>,
    /// Output limits declared per upstream model id (`maxTokens`).
    pub max_output_tokens: HashMap<String, usize>,
    /// Total input/output limits declared by the model catalog (`contextWindow`).
    pub context_windows: HashMap<String, usize>,
    /// Configured economic line before a model enters its long-context price tier.
    pub long_context_thresholds: HashMap<String, usize>,
    /// Optional per-million token rates declared by each model.
    pub model_costs: HashMap<String, crate::model::model_settings::ModelCost>,
}

impl AgentProviderRoute {
    /// The extensions the requested model declares; an undeclared model keeps
    /// the plain protocol, so no extension field is ever sent for it.
    pub fn messages_capabilities(&self, model: &str) -> crate::model::anthropic_messages::Capabilities {
        let upstream = self.models.get(model).map(String::as_str).unwrap_or(model);
        self.messages_capabilities
            .get(upstream)
            .or_else(|| self.messages_capabilities.get(model))
            .copied()
            .unwrap_or_default()
    }
}

#[derive(Clone)]
pub struct AgentServiceState {
    pub workspace: Arc<crate::tools::Workspace>,
    pub model_client: Client,
    /// Shared by all task sessions created by this host.
    pub model_request_slots: Arc<tokio::sync::Semaphore>,
    pub provider_url: String,
    pub api_key: String,
    pub provider_name: String,
    pub provider_routes: HashMap<String, AgentProviderRoute>,
    pub default_model: String,
    pub model_map: HashMap<String, String>,
    pub reasoning_effort: Option<String>,
    pub fast_mode: bool,
    pub permission_mode: PermissionMode,

    pub visual: crate::media::visual_artifacts::VisualSettings,
    pub visual_tool_settings: crate::model::model_settings::ModelSettings,
    pub image_generation_settings: crate::model::model_settings::ModelSettings,
    /// Model for task delegates; all-empty means "same as the conversation".
    pub delegate_agent_settings: crate::model::model_settings::ModelSettings,
    pub config: Option<Arc<tokio::sync::RwLock<crate::server::ai_proxy::AiProxyConfig>>>,
    pub config_path: Option<std::path::PathBuf>,
    pub tool_catalog: Option<Arc<crate::server::builtin_plugins::ToolCatalog>>,
    pub plugin_cancel: CancellationToken,
    pub cancellations: Arc<Mutex<HashMap<String, ActiveTask>>>,
}

impl AgentServiceState {
    pub async fn with_latest_config(mut self) -> Self {
        if let Some(shared) = self.config.clone() {
            if let Some(path) = self.config_path.clone() {
                if let Ok(Ok(latest)) =
                    tokio::task::spawn_blocking(move || crate::server::ai_proxy::load_config(&path)).await
                {
                    *shared.write().await = latest;
                }
            }
            let config = shared.read().await;
            crate::server::ai_proxy::apply_agent_config(&mut self, &config);
        }
        self
    }

    pub(crate) fn use_provider(&mut self, name: &str) -> bool {
        if self.provider_routes.is_empty() {
            return name == self.provider_name;
        }
        let Some(route) = self.provider_routes.get(name).cloned() else {
            return false;
        };
        self.provider_name = name.to_owned();
        self.provider_url = route.url.clone();
        self.api_key = route.api_key.clone();
        self.model_map = route.models;

        true
    }
}

#[derive(Clone)]
pub struct ActiveTask {
    run_id: String,
    token: CancellationToken,
}

#[derive(Debug, Deserialize)]
pub struct CreateTaskRequest {
    pub prompt: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub permission_mode: PermissionMode,
}

/// A browser session may exist before its first prompt. Its ID is also the
/// durable Rust task ID, so reconnects do not need a client-side ID map.
#[derive(Default, Deserialize)]
pub struct DraftSessionInput {
    pub workspace: Option<String>,
    pub kind: Option<String>,
    pub watched_task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ContinueTaskRequest {
    #[serde(default)]
    pub program_resource: Option<String>,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub content: Vec<Value>,
    #[serde(default, rename = "requestId")]
    pub request_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub fast_mode: Option<bool>,
    #[serde(default)]
    pub permission_mode: Option<PermissionMode>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTaskRequest {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<Option<String>>,
    #[serde(default)]
    pub fast_mode: Option<bool>,
    /// This chat's permission level; choosing it saves it at once, before any prompt.
    #[serde(default)]
    pub permission_mode: Option<PermissionMode>,
}

#[derive(Debug, Deserialize)]
pub struct ListTasksQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct StreamEventsQuery {
    #[serde(default)]
    pub since: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct FileQuery {
    q: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AgentEvent {
    pub seq: i64,
    pub time: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub data: Value,
    #[serde(rename = "surfaceOp", skip_serializing_if = "Option::is_none")]
    pub surface_op: Option<String>,
}

pub(crate) fn uuid_like() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_ok() {
        return bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    }
    static FALLBACK: AtomicU64 = AtomicU64::new(0);
    format!(
        "{:08x}{:016x}{:08x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64,
        FALLBACK.fetch_add(1, Ordering::Relaxed) as u32
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::db::{append_event, cleanup_terminal_task_data};
    use rusqlite::params;
    use serde_json::json;
    use std::time::Duration;

    include!("parts/agent_service_tests.inc.rs");
}
#[cfg(test)]
mod output_contract_tests {
    use serde_json::{Value, json};

    include!("parts/agent_service_output_contract_tests.inc.rs");
}
