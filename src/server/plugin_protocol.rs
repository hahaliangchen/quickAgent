//! Fixed contribution protocol shared by project-owned and installed plugins.
use crate::{server::agent_service::PermissionMode, server::builtin_plugins::ToolHandler};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Builtin,
    External,
}

/// Capability / prompt footprint profile for plugin-owned conversations.
/// - `Full` (default): full host coding-agent prompt and all page-permitted host libraries.
/// - `Slim`: minimal `run_js` protocol + core `host`/`session` libraries + the plugin's own
///   library + inlined `SKILL.md` (ideal for focused Q&A / retrieval plugins like `rag-kg`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptProfile {
    #[default]
    Full,
    Slim,
}

impl PromptProfile {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "full" => Some(Self::Full),
            "slim" => Some(Self::Slim),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Slim => "slim",
        }
    }
}

fn enabled_by_default() -> bool { true }

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalService {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub id: String,
    pub title: String,
    #[serde(default = "enabled_by_default")]
    pub enabled_by_default: bool,
    /// Stable source namespace; absent on legacy manifests: derive from the ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<String>,
    /// Optional instructions for this plugin's own conversation.
    #[serde(default)]
    pub skill: String,
    #[serde(default)]
    pub inject: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub command_tools: Vec<crate::server::skill_packages::command::CommandTool>,
    #[serde(default)]
    pub pages: Vec<Page>,
    #[serde(default)]
    pub attachment_extensions: Option<Vec<String>>,
    /// Optional plugin-owned local HTTP helper process (e.g. font or format engine).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_service: Option<LocalService>,
    /// Optional default capability profile (`"full"` or `"slim"`) for this plugin's sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_profile: Option<PromptProfile>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub id: String,
    pub title: String,
    /// Registered frontend module key, or installed module/resource path.
    pub entry: String,
    pub renderer: String,
}

#[derive(Clone, Copy)]
pub enum ToolPermission {
    ReadOnly,
    WorkspaceWrite,
    Execution,
}
impl ToolPermission {
    pub fn allows(self, mode: PermissionMode, ban_execution: bool) -> bool {
        match self {
            Self::ReadOnly => true,
            Self::WorkspaceWrite => !matches!(mode, PermissionMode::ReadOnly),
            Self::Execution => {
                !ban_execution && matches!(mode, PermissionMode::FullAccess | PermissionMode::Ask | PermissionMode::AutoReview)
            }
        }
    }
}
#[derive(Clone)]
pub struct NativeTool {
    pub definition: Value,
    pub handler: ToolHandler,
    pub permission: ToolPermission,
}
/// A compiled implementation adapter. It contributes through exactly the same
/// package validation, mount and effect cleanup as installed implementations.
#[derive(Clone, Default)]
pub struct NativeContributions {
    pub tools: BTreeMap<String, NativeTool>,
    pub frontend_entries: Vec<String>,
    pub on_unmount: Option<Arc<dyn Fn() + Send + Sync>>,
}
pub type NativeBackend = Arc<NativeContributions>;
