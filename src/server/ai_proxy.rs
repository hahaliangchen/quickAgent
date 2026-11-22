use anyhow::Context as _;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::hash::{Hash, Hasher};
use tokio::{
    net::TcpListener,
    sync::{Mutex, RwLock},
};
use tower_http::cors::CorsLayer;
use tracing::info;

use crate::{server::builtin_plugins, server::plugin_runtime};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderConfig {
    url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    api_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key_ref: Option<String>,
    #[serde(default)]
    model_map: HashMap<String, String>,
    /// "openai" (default) or "anthropic".  Anthropic-type providers receive
    /// raw pass-through — no request/response format conversion.
    #[serde(default = "default_api_type")]
    api_type: String,
    /// Upstream model id -> declared `maxTokens` in the DSH model entry.
    #[serde(skip)]
    max_output_tokens: HashMap<String, usize>,
    #[serde(skip)]
    model_metadata: HashMap<String, Value>,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ModelCapabilities {
    #[serde(default)]
    tool_calling: Option<bool>,
    #[serde(default)]
    reasoning_efforts: Vec<String>,
    #[serde(default)]
    default_effort: Option<String>,
    #[serde(default)]
    fast_mode: bool,
    #[serde(default)]
    image_input: crate::media::visual_artifacts::ImageCapability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_probe: Option<Value>,
}

fn built_in_capabilities(model: &str) -> ModelCapabilities {
    if model == "gpt-6-luna" {
        ModelCapabilities {
            tool_calling: None,
            reasoning_efforts: ["none", "low", "medium", "high", "xhigh", "max"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            default_effort: Some("max".into()),
            fast_mode: true,
            image_input: Default::default(),
            image_probe: None,
        }
    } else {
        ModelCapabilities::default()
    }
}

fn default_api_type() -> String {
    "openai-completions".to_owned()
}

/// Reasoning levels a DSH model entry declares through `reasoningEfforts`.
/// Keys are folded onto the levels the host accepts (`false`/`off` mean "no
/// reasoning", `minimal` is the same wire level as `low`), unknown keys are
/// dropped, and the canonical order keeps the picker stable whatever the file
/// order. A declared level is therefore always one settings can save.
fn declared_reasoning_efforts(metadata: &Value) -> Vec<String> {
    let Some(levels) = metadata.get("reasoningEfforts").and_then(Value::as_object) else {
        return Vec::new();
    };
    crate::model::model_settings::canonical_efforts(&levels.keys().cloned().collect::<Vec<_>>())
}
/// The provider APIs the agent speaks: Chat Completions, Responses as Codex
/// uses it, and the native Anthropic Messages protocol.
fn agent_api(api_type: &str) -> bool {
    matches!(
        api_type,
        "openai-completions" | "openai-responses" | "anthropic-messages"
    )
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct AiProxyConfig {
    #[serde(default)]
    bash_path: Option<std::path::PathBuf>,
    #[serde(default)]
    pwsh_path: Option<std::path::PathBuf>,
    #[serde(default)]
    default_provider: Option<String>,
    #[serde(default)]
    orchestrator_provider: Option<String>,
    #[serde(default)]
    orchestrator_model: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    fast_mode: bool,
    #[serde(default)]
    model_capabilities: HashMap<String, HashMap<String, ModelCapabilities>>,
    #[serde(default)]
    visual_tool: crate::model::model_settings::ModelSettings,
    #[serde(default)]
    image_generation: crate::model::model_settings::ModelSettings,
    /// Task delegates; empty fields inherit the conversation's model.
    #[serde(default)]
    delegate_agent: crate::model::model_settings::ModelSettings,
    #[serde(default)]
    plugin_models: HashMap<String, Vec<crate::model::model_catalog::ModelRef>>,
    #[serde(default)]
    plugin_generation: HashMap<String, crate::model::model_settings::PluginGeneration>,
    #[serde(default)]
    providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    web_search_provider: Option<String>,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

pub(crate) fn apply_agent_config(
    state: &mut crate::server::agent_service::AgentServiceState,
    config: &AiProxyConfig,
) {
    state.visual_tool_settings = config.visual_tool.clone();
    state.image_generation_settings = config.image_generation.clone();
    state.delegate_agent_settings = config.delegate_agent.clone();
    let main_name = config
        .orchestrator_provider
        .as_deref()
        .or(config.default_provider.as_deref())
        .unwrap_or("");
    let main = config
        .providers
        .get(main_name)
        .filter(|provider| agent_api(&provider.api_type));
    state.provider_name = main.map(|_| main_name.to_owned()).unwrap_or_default();
    state.provider_routes = config
        .providers
        .iter()
        .filter(|(_, provider)| agent_api(&provider.api_type))
        .map(|(name, provider)| {
            (
                name.clone(),
                crate::server::agent_service::AgentProviderRoute {
                    responses: provider.api_type == "openai-responses",
                    messages: provider.api_type == "anthropic-messages",
                    messages_capabilities: provider
                        .model_metadata
                        .iter()
                        .filter_map(|(id, metadata)| {
                            let capabilities =
                                crate::model::anthropic_messages::Capabilities::from_metadata(metadata);
                            capabilities.declared().then(|| (id.clone(), capabilities))
                        })
                        .collect(),
                    url: provider.url.clone(),
                    api_key: provider.api_key.clone(),
                    models: provider.model_map.clone(),
                    max_output_tokens: provider.max_output_tokens.clone(),
                    context_windows: provider
                        .model_metadata
                        .iter()
                        .filter_map(|(id, metadata)| {
                            let tokens = metadata["contextWindow"]
                                .as_u64()
                                .and_then(|n| usize::try_from(n).ok())?;
                            (tokens > 0).then(|| {
                                (
                                    provider
                                        .model_map
                                        .get(id)
                                        .cloned()
                                        .unwrap_or_else(|| id.clone()),
                                    tokens,
                                )
                            })
                        })
                        .collect(),
                    model_costs: provider
                        .model_metadata
                        .iter()
                        .filter_map(|(id, metadata)| {
                            let cost = serde_json::from_value(metadata.get("cost")?.clone()).ok()?;
                            Some((id.clone(), cost))
                        })
                        .collect(),
                    long_context_thresholds: provider
                        .model_metadata
                        .iter()
                        .filter_map(|(id, metadata)| {
                            let threshold = metadata
                                .pointer("/cost/longContextThreshold")
                                .and_then(Value::as_u64)
                                .and_then(|n| usize::try_from(n).ok())?;
                            (threshold > 0).then(|| {
                                (
                                    provider
                                        .model_map
                                        .get(id)
                                        .cloned()
                                        .unwrap_or_else(|| id.clone()),
                                    threshold,
                                )
                            })
                        })
                        .collect(),
                },
            )
        })
        .collect();
    state.provider_url = main.map(|p| p.url.clone()).unwrap_or_default();
    state.api_key = main.map(|p| p.api_key.clone()).unwrap_or_default();
    state.model_map = main.map(|p| p.model_map.clone()).unwrap_or_default();
    state.reasoning_effort = config.reasoning_effort.clone();
    state.fast_mode = config.fast_mode;
    state.default_model = config
        .orchestrator_model
        .clone()
        .or_else(|| main.and_then(|p| p.model_map.values().next().cloned()))
        .unwrap_or_default();
    state.visual.capabilities = config
        .model_capabilities
        .iter()
        .map(|(provider, models)| {
            (
                provider.clone(),
                models
                    .iter()
                    .flat_map(|(model, cap)| {
                        let mapped = config
                            .providers
                            .get(provider)
                            .and_then(|p| p.model_map.get(model))
                            .cloned()
                            .unwrap_or_else(|| model.clone());
                        [(model.clone(), cap.image_input), (mapped, cap.image_input)]
                    })
                    .collect(),
            )
        })
        .collect();
}

/// The app's own configuration directory: `GROW_CONFIG_DIR`, else `grow`
/// under the platform's configuration directory (`%APPDATA%` on Windows,
/// `$XDG_CONFIG_HOME` or `~/.config` elsewhere).
pub(crate) fn config_dir() -> std::path::PathBuf {
    config_dir_from(|name| std::env::var_os(name), cfg!(windows))
}

fn config_dir_from(var: impl Fn(&str) -> Option<std::ffi::OsString>, windows: bool) -> std::path::PathBuf {
    let set = |name: &str| var(name).filter(|value| !value.is_empty()).map(std::path::PathBuf::from);
    if let Some(dir) = set("GROW_CONFIG_DIR") {
        return dir;
    }
    let base = if windows {
        set("APPDATA").or_else(|| set("USERPROFILE").map(|home| home.join("AppData").join("Roaming")))
    } else {
        set("XDG_CONFIG_HOME").or_else(|| set("HOME").map(|home| home.join(".config")))
    };
    base.unwrap_or_else(|| std::path::PathBuf::from(".config")).join("grow")
}

/// Model providers: `providers.yml` in the configuration directory. Its
/// generation settings, skill packages and credentials sit beside it.
pub(crate) fn config_path() -> std::path::PathBuf {
    config_dir().join("providers.yml")
}

fn credentials_path(config_path: &Path) -> std::path::PathBuf {
    config_path.with_file_name("credentials.yaml")
}

/// Copy a configuration made by DSH (`~/.dsh`, or `DSH_HOME`) into the app's
/// own directory, once, when the app has none yet. The DSH files are left
/// untouched and are never read again afterwards.
pub(crate) fn import_dsh_config(target: &Path) -> anyhow::Result<Option<std::path::PathBuf>> {
    match dsh_home() {
        Some(home) => import_dsh_config_from(target, &home),
        None => Ok(None),
    }
}

fn dsh_home() -> Option<std::path::PathBuf> {
    std::env::var_os("DSH_HOME")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|home| std::path::PathBuf::from(home).join(".dsh")))
}

/// Files that hold API keys and other secrets: the app's own and the DSH ones
/// it imports from. Source-file tools never open them; the host reads them
/// itself, so a model never needs their contents.
pub(crate) fn secret_files() -> Vec<std::path::PathBuf> {
    let config = config_dir();
    let mut files = vec![
        credentials_path(&config_path()),
        config.join(".env"),
        config.join("local_api_token"),
    ];
    if let Some(home) = dsh_home() {
        files.extend([home.join(".credentials.yaml"), home.join(".env")]);
    }
    files
}

fn import_dsh_config_from(target: &Path, home: &Path) -> anyhow::Result<Option<std::path::PathBuf>> {
    let providers = target.join("providers.yml");
    let profile = home.join("profiles").join("web");
    let source = profile.join("cordis.patch.yml");
    if providers.exists() || !source.is_file() {
        return Ok(None);
    }
    std::fs::create_dir_all(target)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o700))?;
    }
    let files = [
        (profile.join("agent-generation.yml"), "agent-generation.yml"),
        (profile.join("skill-packages.json"), "skill-packages.json"),
        (home.join(".credentials.yaml"), "credentials.yaml"),
        (home.join(".env"), ".env"),
        // Copied last: its presence marks a complete import.
        (source, "providers.yml"),
    ];
    for (from, name) in files {
        let to = target.join(name);
        if !from.is_file() || to.exists() {
            continue;
        }
        let temporary = target.join(format!(".{name}.import.tmp"));
        std::fs::copy(&from, &temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&temporary, &to)?;
    }
    Ok(Some(home.to_path_buf()))
}

fn generation_settings_path(config_path: &Path) -> std::path::PathBuf {
    config_path.with_file_name("agent-generation.yml")
}

fn read_yaml(path: &Path, fallback: Value) -> anyhow::Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(serde_yaml::from_str(&raw)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(fallback),
        Err(error) => Err(error.into()),
    }
}

fn write_yaml_atomic(path: &Path, value: &Value) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("configuration path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let suffix = format!(
        "{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        suffix
    ));
    let result = (|| -> anyhow::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(serde_yaml::to_string(value)?.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn plugin_config<'a>(patch: &'a Value, id: &str) -> Option<&'a Value> {
    patch
        .as_array()?
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(id))?
        .get("config")
}

fn set_plugin_config(patch: &mut Value, id: &str, config: Value) -> anyhow::Result<()> {
    let entries = patch
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("DSH profile patch must be a YAML sequence"))?;
    if let Some(entry) = entries
        .iter_mut()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(id))
    {
        entry
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("DSH plugin entry is not a mapping"))?
            .insert("config".into(), config);
    } else {
        entries.push(json!({"id":id,"config":config}));
    }
    Ok(())
}

fn dotenv_value(path: &Path, reference: &str) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    for line in raw.lines() {
        let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
        if line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() != reference {
            continue;
        }
        let value = value.trim();
        let value = if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            &value[1..value.len() - 1]
        } else {
            value
        };
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
}

fn credential_value(credentials: &Value, config_path: &Path, reference: &str) -> String {
    std::env::var(reference)
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            credentials
                .pointer(&format!("/refs/{reference}"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|dir| dotenv_value(&dir.join(".env"), reference))
        })
        .or_else(|| {
            dotenv_value(
                &credentials_path(config_path).with_file_name(".env"),
                reference,
            )
        })
        .unwrap_or_default()
}

pub(crate) fn load_config(config_path: &Path) -> anyhow::Result<AiProxyConfig> {
    let patch = read_yaml(config_path, json!([]))?;
    anyhow::ensure!(
        patch.is_array(),
        "DSH profile patch must be a YAML sequence"
    );
    let credentials = read_yaml(
        &credentials_path(config_path),
        json!({"version":1,"refs":{},"records":{}}),
    )?;
    let mut config = AiProxyConfig::default();
    if let Some(providers) = plugin_config(&patch, "llm-pi-ai")
        .and_then(|v| v.get("providers"))
        .and_then(Value::as_object)
    {
        for (id, entry) in providers {
            let Some(url) = entry.get("baseURL").and_then(Value::as_str) else {
                continue;
            };
            let Some(models) = entry.get("models").and_then(Value::as_array) else {
                continue;
            };
            let model_map = models
                .iter()
                .filter_map(|v| {
                    v.get("id")
                        .and_then(Value::as_str)
                        .map(|id| (id.to_owned(), id.to_owned()))
                })
                .collect();
            let max_output_tokens = models
                .iter()
                .filter_map(|v| {
                    Some((
                        v.get("id")?.as_str()?.to_owned(),
                        usize::try_from(v.get("maxTokens")?.as_u64()?).ok()?,
                    ))
                })
                .filter(|(_, n)| *n > 0)
                .collect();
            let reference = entry
                .get("apiKeyEnv")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let api_key = reference
                .as_deref()
                .map(|r| credential_value(&credentials, config_path, r))
                .unwrap_or_default();
            config.providers.insert(
                id.clone(),
                ProviderConfig {
                    url: url.to_owned(),
                    api_key,
                    api_key_ref: reference,
                    model_map,
                    api_type: entry
                        .get("api")
                        .and_then(Value::as_str)
                        .unwrap_or("openai-completions")
                        .to_owned(),
                    max_output_tokens,
                    model_metadata: models
                        .iter()
                        .filter_map(|m| Some((m["id"].as_str()?.to_owned(), m.clone())))
                        .collect(),
                    extra: [
                        "displayName",
                        "prefixSystemMessages",
                        "contextRequestOptions",
                    ]
                    .into_iter()
                    .filter_map(|key| Some((key.to_owned(), entry.get(key)?.clone())))
                    .collect(),
                },
            );
        }
    }
    if let Some(main) = plugin_config(&patch, "agent-default-model") {
        config.orchestrator_provider = main
            .get("provider")
            .and_then(Value::as_str)
            .map(str::to_owned);
        config.orchestrator_model = main.get("model").and_then(Value::as_str).map(str::to_owned);
        config.default_provider = config.orchestrator_provider.clone();
    }
    let generation = read_yaml(&generation_settings_path(config_path), json!({}))?;
    config.web_search_provider = generation["web_search_provider"].as_str()
        .filter(|id| !id.trim().is_empty()).map(str::to_owned);
    let shell = crate::tools::shell_environment::read_settings(config_path)?;
    config.bash_path = shell.bash_path;
    config.pwsh_path = shell.pwsh_path;
    for (id, provider) in &mut config.providers {
        if let Some(prefix) =
            generation["provider_compatibility"][id]["prefix_system_messages"].as_bool()
        {
            provider
                .extra
                .insert("prefixSystemMessages".into(), json!(prefix));
        }
    }
    config.reasoning_effort = generation
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .map(str::to_owned);
    config.fast_mode = generation
        .get("fast_mode")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    config.visual_tool =
        serde_json::from_value(generation.get("visual_tool").cloned().unwrap_or(json!({})))?;
    config.image_generation = serde_json::from_value(
        generation.get("image_generation").cloned().unwrap_or(json!({})),
    )?;
    config.delegate_agent = serde_json::from_value(
        generation
            .get("delegate_agent")
            .cloned()
            .unwrap_or(json!({})),
    )?;
    config.visual_tool.normalize()?;
    config.image_generation.normalize()?;
    config.delegate_agent.normalize()?;
    config.plugin_models = serde_json::from_value(
        generation
            .get("plugin_models")
            .cloned()
            .unwrap_or(json!({})),
    )?;
    config.plugin_generation = serde_json::from_value(
        generation
            .get("plugin_generation")
            .cloned()
            .unwrap_or(json!({})),
    )?;
    for policy in config.plugin_generation.values_mut() {
        policy.normalize()?;
    }
    config.model_capabilities = generation
        .get("model_capabilities")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    for (provider_id, provider) in &config.providers {
        let models = config
            .model_capabilities
            .entry(provider_id.clone())
            .or_default();
        for model_id in provider.model_map.keys() {
            models
                .entry(model_id.clone())
                .or_insert_with(|| built_in_capabilities(model_id));
        }
        for (model, capability) in models {
            // A model entry that names its own levels is honoured even when the
            // generation settings do not repeat them.
            if capability.reasoning_efforts.is_empty() {
                if let Some(metadata) = provider.model_metadata.get(model) {
                    capability.reasoning_efforts = declared_reasoning_efforts(metadata);
                }
            }
            if capability.default_effort.is_none() {
                if let Some(metadata) = provider.model_metadata.get(model) {
                    capability.default_effort = metadata
                        .get("defaultReasoningEffort")
                        .and_then(Value::as_str)
                        .and_then(crate::model::model_settings::canonical_effort)
                        .filter(|level| capability.reasoning_efforts.iter().any(|own| own == level))
                        .map(str::to_owned);
                }
            }
            // A level written by hand in the generation settings is folded onto
            // one the host can save, so the settings screen never offers a level
            // its own save would reject.
            capability.reasoning_efforts =
                crate::model::model_settings::canonical_efforts(&capability.reasoning_efforts);
            // A default that folding removed falls back to "model default".
            if capability.default_effort.as_deref().is_some_and(|default| {
                !capability
                    .reasoning_efforts
                    .iter()
                    .any(|own| own == default)
            }) {
                capability.default_effort = None;
            }
            if capability.image_probe.as_ref().is_some_and(|probe| {
                provider.api_type != "openai-completions"
                    || probe["route_fingerprint"]
                        != crate::media::visual_probe::route_fingerprint(&provider.url, model)
            }) {
                capability.image_input = crate::media::visual_artifacts::ImageCapability::Unknown;
            }
            if capability.image_input == crate::media::visual_artifacts::ImageCapability::Supported
                && (provider.api_type != "openai-completions"
                    || !verified_image_support(capability, &provider.url, model))
            {
                capability.image_input = crate::media::visual_artifacts::ImageCapability::Unknown;
            }
        }
    }
    Ok(config)
}

fn disk_revision(config_path: &Path) -> anyhow::Result<String> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for path in [
        config_path.to_path_buf(),
        credentials_path(config_path),
        generation_settings_path(config_path),
    ] {
        match std::fs::read(path) {
            Ok(bytes) => bytes.hash(&mut hasher),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0u8.hash(&mut hasher),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(format!("{:016x}", hasher.finish()))
}

fn web_provider_route(config: &AiProxyConfig) -> anyhow::Result<(&str, &ProviderConfig)> {
    let id = config.web_search_provider.as_deref()
        .context("WEB_NOT_CONFIGURED: choose a DeepSeek credential in Settings (设置) > 专用模型 > 联网搜索 > 搜索凭据")?;
    let provider = config.providers.get(id)
        .context("WEB_NOT_CONFIGURED: the selected web search provider no longer exists")?;
    let url = reqwest::Url::parse(&provider.url)?;
    anyhow::ensure!(url.scheme() == "https" && url.host_str() == Some("api.deepseek.com")
        && url.port_or_known_default() == Some(443) && url.username().is_empty() && url.password().is_none(),
        "WEB_PROVIDER_UNSUPPORTED: web search currently requires an official DeepSeek provider");
    Ok((id, provider))
}

fn web_provider(config: &AiProxyConfig) -> anyhow::Result<(&str, &ProviderConfig)> {
    let (id, provider) = web_provider_route(config)?;
    anyhow::ensure!(!provider.api_key.is_empty(), "WEB_NOT_CONFIGURED: add an API key to the selected DeepSeek provider");
    Ok((id, provider))
}

/// Resolve credentials inside the host. No key is accepted in tool arguments or results.
pub(crate) fn web_search_credentials(config_path: &Path) -> anyhow::Result<(String, String)> {
    let config = load_config(config_path)
        .map_err(|_| anyhow::anyhow!("WEB_CONFIGURATION_ERROR: cannot load the saved provider settings"))?;
    let (id, provider) = web_provider(&config)?;
    Ok((id.to_owned(), provider.api_key.clone()))
}

fn settings_view(config: &AiProxyConfig, revision: String) -> Value {
    let mut providers: Vec<_> = config
        .providers
        .iter()
        .map(|(id, provider)| {
            json!({
                "id": id, "url": provider.url, "api_type": provider.api_type,
                "prefix_system_messages":provider.extra.get("prefixSystemMessages").and_then(Value::as_bool).unwrap_or(false),
                "display_name": provider.extra.get("displayName").and_then(Value::as_str),
                "api_key_env": provider.api_key_ref,
                "models": provider.model_map.keys().collect::<Vec<_>>(), "has_api_key": !provider.api_key.is_empty(),
                "model_capabilities": config.model_capabilities.get(id).cloned().unwrap_or_default(),
            })
        })
        .collect();
    providers.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    json!({
        "revision": revision,
        "bash_path": config.bash_path,
        "pwsh_path": config.pwsh_path,
        "shell_environment": crate::tools::shell_environment::cached_description(if cfg!(windows) { config.pwsh_path.as_deref() } else { config.bash_path.as_deref() }),
        "providers": providers,
        "orchestrator_provider": config.orchestrator_provider,
        "orchestrator_model": config.orchestrator_model,
        "reasoning_effort": config.reasoning_effort,
        "fast_mode": config.fast_mode,
                "visual_tool":config.visual_tool,"image_generation":config.image_generation,"delegate_agent":config.delegate_agent,
        "plugin_models":config.plugin_models,"plugin_generation":config.plugin_generation,
        "web_search_provider":config.web_search_provider,
    })
}

/// Explicit projection: never expose URLs, credentials or raw probe responses.
pub(crate) fn catalog_view(config: &AiProxyConfig) -> Value {
    let mut models = Vec::new();
    for (provider, route) in &config.providers {
        for id in route.model_map.keys() {
            let cap = config
                .model_capabilities
                .get(provider)
                .and_then(|m| m.get(id))
                .cloned()
                .unwrap_or_else(|| built_in_capabilities(id));
            let metadata = route.model_metadata.get(id);
            let probe = cap.image_probe.as_ref().filter(|p| {
                route.api_type == "openai-completions"
                    && p["route_fingerprint"]
                        == crate::media::visual_probe::route_fingerprint(&route.url, id)
            });
            models.push(json!({
                "provider":provider, "id":id,
                "name":metadata.and_then(|m|m["name"].as_str()).unwrap_or(id),
                "provider_name":route.extra.get("displayName").and_then(Value::as_str).unwrap_or(provider),
                "enabled":true, "available_for_agent":agent_api(&route.api_type),
                "api_type":route.api_type,
                "context_window":metadata.and_then(|m|m["contextWindow"].as_u64()),
                "max_output_tokens":route.max_output_tokens.get(id),
                "capabilities":{
                    "image_input":cap.image_input, "tool_calling":cap.tool_calling,
                    "reasoning_efforts":cap.reasoning_efforts, "default_effort":cap.default_effort,
                    "fast_mode":cap.fast_mode,
                },
                "verification":{
                    "image_input":probe.and_then(|p|p["status"].as_str()).unwrap_or("unverified"),
                    "checked_at":probe.and_then(|p|p["checked_at"].as_u64()),
                },
            }));
        }
    }
    models.sort_by(|a, b| {
        (a["provider"].as_str(), a["id"].as_str()).cmp(&(b["provider"].as_str(), b["id"].as_str()))
    });
    json!({"version":1,"models":models,"defaults":{
        "agent":{"provider":config.orchestrator_provider,"model":config.orchestrator_model},
        "visual_tool":config.visual_tool,"image_generation":config.image_generation,"delegate_agent":config.delegate_agent,"plugin_generation":config.plugin_generation,
    }})
}

pub(crate) fn restricted_config(
    config: &AiProxyConfig,
    plugin: &str,
) -> anyhow::Result<AiProxyConfig> {
    let mut config = config.clone();
    if let Some(policy) = config.plugin_generation.get(plugin) {
        if let Some(effort) = &policy.reasoning_effort {
            config.reasoning_effort = Some(effort.clone());
        }
        if let Some(effort) = &policy.visual_reasoning_effort {
            config.visual_tool.reasoning_effort = Some(effort.clone());
        }
    }
    let config = &config;
    let Some(allowed) = config.plugin_models.get(plugin) else {
        return Ok(config.clone());
    };
    let permits = |provider: &str, model: &str| {
        allowed
            .iter()
            .any(|r| r.provider == provider && r.model == model)
    };
    let mut scoped = config.clone();
    scoped.providers.retain(|id, p| {
        p.model_map.retain(|model, _| permits(id, model));
        !p.model_map.is_empty() && p.api_type == "openai-completions"
    });
    let selected = config
        .orchestrator_provider
        .as_deref()
        .zip(config.orchestrator_model.as_deref())
        .filter(|(p, m)| {
            scoped
                .providers
                .get(*p)
                .is_some_and(|r| r.model_map.contains_key(*m))
        })
        .map(|(p, m)| crate::model::model_catalog::ModelRef {
            provider: p.into(),
            model: m.into(),
        })
        .or_else(|| {
            allowed
                .iter()
                .find(|r| {
                    scoped
                        .providers
                        .get(&r.provider)
                        .is_some_and(|p| p.model_map.contains_key(&r.model))
                })
                .cloned()
        })
        .context("插件没有可用的授权模型，请在宿主设置中配置")?;
    scoped.orchestrator_provider = Some(selected.provider.clone());
    scoped.orchestrator_model = Some(selected.model.clone());
    scoped.default_provider = Some(selected.provider.clone());
    for role in [&mut scoped.visual_tool, &mut scoped.delegate_agent] {
        if role.provider.is_some() || role.model.is_some() {
            let p = role
                .provider
                .as_ref()
                .or(config.orchestrator_provider.as_ref());
            let m = role.model.as_ref().or(config.orchestrator_model.as_ref());
            if p.zip(m).is_none_or(|(p, m)| {
                !scoped
                    .providers
                    .get(p)
                    .is_some_and(|r| r.model_map.contains_key(m))
            }) {
                *role = Default::default();
            } else {
                role.provider = p.cloned();
                role.model = m.cloned();
            }
        }
    }
    let cap = scoped
        .model_capabilities
        .get(&selected.provider)
        .and_then(|m| m.get(&selected.model));
    scoped.reasoning_effort = config
        .reasoning_effort
        .clone()
        .filter(|e| cap.is_some_and(|c| c.reasoning_efforts.contains(e)));
    if let Some(effort) = config
        .plugin_generation
        .get(plugin)
        .and_then(|p| p.visual_reasoning_effort.clone())
    {
        scoped.visual_tool.reasoning_effort = Some(effort);
    }
    scoped.fast_mode = config.fast_mode && cap.is_some_and(|c| c.fast_mode);
    Ok(scoped)
}

pub(crate) fn plugin_catalog_view(config: &AiProxyConfig, plugin: &str) -> Value {
    match restricted_config(config, plugin) {
        Ok(scoped) => catalog_view(&scoped),
        Err(_) => json!({"version":1,"models":[],"defaults":{}}),
    }
}

include!("parts/ai_proxy_routes.inc.rs");

#[derive(Clone)]
struct AiProxyState {
    config: Arc<RwLock<AiProxyConfig>>,
    config_path: std::path::PathBuf,
    settings_write: Arc<Mutex<()>>,
    client: Client,
    runtime: Arc<plugin_runtime::PluginRuntime>,
}

async fn plugin_tree(State(state): State<AiProxyState>) -> Json<Value> {
    let events = state
        .runtime
        .service::<builtin_plugins::LifecycleLog>(plugin_runtime::ROOT, "plugin-events")
        .map(|log| log.snapshot())
        .unwrap_or_default();
    let mcp_servers = state.runtime.service::<builtin_plugins::ToolCatalog>(plugin_runtime::ROOT, "tools")
        .map(|catalog| catalog.packages.mcp_status()).unwrap_or_else(|_| json!([]));
    Json(json!({"plugins":state.runtime.snapshot(),"events":events,"mcp_servers":mcp_servers}))
}

async fn skill_packages(State(state): State<AiProxyState>) -> Json<Value> {
    let registry = state
        .runtime
        .service::<builtin_plugins::ToolCatalog>(plugin_runtime::ROOT, "tools")
        .unwrap();
    Json(json!({"packages":registry.packages.catalog()}))
}
#[derive(Deserialize)]
struct PackageEnable {
    enabled: bool,
}
async fn enable_skill_package(
    State(state): State<AiProxyState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(input): Json<PackageEnable>,
) -> Response {
    let registry = state
        .runtime
        .service::<builtin_plugins::ToolCatalog>(plugin_runtime::ROOT, "tools")
        .unwrap();
    if registry.packages.is_mcp(&id) {
        return package_response(crate::server::skill_packages::mcp_packages::set_enabled(
            &state.runtime, &registry.packages, &id, input.enabled, true,
        ).await);
    }
    if input.enabled && registry.packages.is_remote(&id) {
        if let Err(error) =
            crate::server::skill_packages::remote::reconnect(&state.runtime, &registry.packages, &id).await
        {
            return package_response(Err(error));
        }
    }
    let result = crate::server::skill_packages::set_enabled(
        &state.runtime,
        &registry.packages,
        &id,
        input.enabled,
        true,
    );
    match result {
        Ok(packages) => Json(json!({"packages":packages})).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":format!("{error:#}")})),
        )
            .into_response(),
    }
}
#[derive(Deserialize)]
struct PackageInstall {
    path: std::path::PathBuf,
}
async fn install_skill_package(
    State(state): State<AiProxyState>,
    Json(input): Json<PackageInstall>,
) -> Response {
    let catalog = state
        .runtime
        .service::<builtin_plugins::ToolCatalog>(plugin_runtime::ROOT, "tools")
        .unwrap();
    package_response(crate::server::skill_packages::install(
        &state.runtime,
        &catalog.packages,
        &input.path,
    ))
}
async fn connect_remote_package(
    State(state): State<AiProxyState>,
    Json(input): Json<crate::server::skill_packages::remote::Config>,
) -> Response {
    let catalog = state
        .runtime
        .service::<builtin_plugins::ToolCatalog>(plugin_runtime::ROOT, "tools")
        .unwrap();
    package_response(
        crate::server::skill_packages::remote::install(&state.runtime, &catalog.packages, input, true)
            .await,
    )
}
async fn remote_page_request(
    State(state): State<AiProxyState>,
    axum::extract::Path((id, method)): axum::extract::Path<(String, String)>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> Response {
    let scope = state
        .runtime
        .snapshot()
        .into_iter()
        .find(|p| p.id == "agent-tasks")
        .map(|p| p.scope)
        .unwrap_or(plugin_runtime::ROOT);
    let agent = match state
        .runtime
        .service::<crate::server::agent_service::AgentServiceState>(scope, "agent-tasks")
    {
        Ok(agent) => agent,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"Agent service unavailable"})),
            )
                .into_response();
        }
    };
    crate::server::remote_sessions::request(
        &state.runtime,
        (*agent).clone(),
        &id,
        &method,
        headers,
        args,
    )
    .await
}
async fn reload_skill_package(
    State(state): State<AiProxyState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let catalog = state
        .runtime
        .service::<builtin_plugins::ToolCatalog>(plugin_runtime::ROOT, "tools")
        .unwrap();
    if catalog.packages.is_mcp(&id) {
        return package_response(crate::server::skill_packages::mcp_packages::reconnect(
            &state.runtime, &catalog.packages, &id,
        ).await);
    }
    if catalog.packages.is_remote(&id) {
        return package_response(
            crate::server::skill_packages::remote::reconnect(&state.runtime, &catalog.packages, &id).await,
        );
    }
    package_response(crate::server::skill_packages::reload(
        &state.runtime,
        &catalog.packages,
        &id,
    ))
}
async fn remove_skill_package(
    State(state): State<AiProxyState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let catalog = state
        .runtime
        .service::<builtin_plugins::ToolCatalog>(plugin_runtime::ROOT, "tools")
        .unwrap();
    if catalog.packages.is_mcp(&id) {
        return package_response(Err(anyhow::anyhow!("Remove the server from mcp-servers.json instead")));
    }
    package_response(crate::server::skill_packages::remove(
        &state.runtime,
        &catalog.packages,
        &id,
    ))
}
fn package_response(result: anyhow::Result<Value>) -> Response {
    match result {
        Ok(packages) => Json(json!({"packages":packages})).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":format!("{error:#}")})),
        )
            .into_response(),
    }
}
async fn skill_page_asset(
    State(state): State<AiProxyState>,
    axum::extract::Path((id, page, resource)): axum::extract::Path<(String, String, String)>,
) -> Response {
    let registry = state
        .runtime
        .service::<builtin_plugins::ToolCatalog>(plugin_runtime::ROOT, "tools")
        .unwrap();
    if let Ok(Some((connection, entry))) = registry.packages.remote_page(&id, &page) {
        if resource != "index" {
            return StatusCode::NOT_FOUND.into_response();
        }
        return match connection.asset(&entry, &[]).await {
            Ok((mime, bytes)) => {
                let mut response = (
                    [
                        (axum::http::header::CONTENT_TYPE, mime),
                        (axum::http::header::CACHE_CONTROL, "no-store".to_owned()),
                        (
                            axum::http::header::X_CONTENT_TYPE_OPTIONS,
                            "nosniff".to_owned(),
                        ),
                    ],
                    bytes,
                )
                    .into_response();
                if entry == "page" {
                    response.headers_mut().insert(axum::http::header::CONTENT_SECURITY_POLICY, axum::http::HeaderValue::from_static("default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; connect-src 'none'; form-action 'none'; base-uri 'none'"));
                }
                response
            }
            Err(_) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error":"remote page unavailable; check plugin connection"})),
            )
                .into_response(),
        };
    }
    let result = registry.packages.page(
        &id,
        &page,
        if resource == "index" {
            None
        } else {
            Some(&resource)
        },
    );
    match result {
        Ok(path) => match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let content_type = match path.extension().and_then(|extension| extension.to_str()) {
                    Some("html") => "text/html; charset=utf-8",
                    Some("js" | "mjs") => "text/javascript",
                    Some("css") => "text/css",
                    Some("png") => "image/png",
                    Some("svg") => "image/svg+xml",
                    _ => "application/octet-stream",
                };
                ([(axum::http::header::CONTENT_TYPE, content_type)], bytes).into_response()
            }
            Err(error) => (
                StatusCode::NOT_FOUND,
                Json(json!({"error":error.to_string()})),
            )
                .into_response(),
        },
        Err(error) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

pub async fn run(
    listener: TcpListener,
    config_path: &Path,
    workspace: Arc<crate::tools::Workspace>,
) -> anyhow::Result<()> {
    let config = load_config(config_path)?;

    crate::server::agent_service::recover_orphaned_tasks(workspace.root()).await?;

    let total_maps: usize = config.providers.values().map(|p| p.model_map.len()).sum();

    let config = Arc::new(RwLock::new(config));
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    let log_dir = workspace.root().join("logs");
    crate::model::proxy_log::init(log_dir)?;

    info!("========== Agent service started ==========");
    info!("config: {} total model mappings", total_maps);

    let runtime = Arc::new(plugin_runtime::PluginRuntime::new());
    runtime.mount(plugin_runtime::ROOT, builtin_plugins::DiagnosticsPlugin)?;
    runtime.mount(
        plugin_runtime::ROOT,
        builtin_plugins::WorkspacePlugin(workspace.clone()),
    )?;
    runtime.mount(
        plugin_runtime::ROOT,
        builtin_plugins::ModelSettingsPlugin(Arc::new(builtin_plugins::ModelSettings {
            path: config_path.to_path_buf(),
            config: config.clone(),
        })),
    )?;
    runtime.mount(
        plugin_runtime::ROOT,
        crate::model::model_catalog::ModelCatalogPlugin,
    )?;
    let tools_scope = runtime.mount(plugin_runtime::ROOT, builtin_plugins::ToolCatalogPlugin)?;
    let agent_state = (*runtime
        .service::<crate::server::agent_service::AgentServiceState>(tools_scope, "agent-tasks")?)
    .clone();
    crate::server::skill_packages::bind_root(agent_state.tool_catalog.as_ref().unwrap());
    let mcp_runtime = runtime.clone();
    let mcp_registry = agent_state.tool_catalog.as_ref().unwrap().packages.clone();
    tokio::spawn(async move {
        if let Err(error) = crate::server::skill_packages::mcp_packages::restore(mcp_runtime, mcp_registry).await {
            tracing::warn!(%error, "MCP configuration could not be restored");
        }
    });
    let remote_runtime = runtime.clone();
    let remote_registry = agent_state.tool_catalog.as_ref().unwrap().packages.clone();
    tokio::spawn(async move {
        if let Err(_) =
            crate::server::skill_packages::remote::restore(&remote_runtime, &remote_registry).await
        {
            tracing::warn!("remote plugin configuration could not be restored");
        }
    });
    let agent_routes = Router::new()
        .route("/agent", get(crate::server::agent_web::page))
        .route("/agent/", get(crate::server::agent_web::page))
        .nest_service(
            "/agent/assets",
            tower_http::services::ServeDir::new(crate::server::agent_web::web_dir().join("assets")),
        )
        .route("/agent/star.png", get(crate::server::agent_web::star_png))
        .route("/agent/logo.png", get(crate::server::agent_web::star_png))
        .route("/agent/favicon.png", get(crate::server::agent_web::favicon_png))
        .route("/agent/favicon.ico", get(crate::server::agent_web::favicon_png))
        .route("/agent/settings", get(crate::server::agent_web::settings_page))
        .route("/agent/info", get(crate::server::agent_web::info))
        .route(
            "/agent/clipboard-image",
            get(crate::server::agent_web::clipboard_image),
        )
        .route(
            "/agent/workspace/files",
            get(crate::server::agent_service::workspace_files),
        )
        .route("/agent/commands", get(crate::server::agent_service::slash_commands))
        .route(
            "/agent/notes",
            get(crate::context::project_notes::get)
                .put(crate::context::project_notes::put)
                .post(crate::context::project_notes::post),
        )
        .route(
            "/agent/skill-packages/{id}/tools/{name}",
            post(crate::server::skill_packages::call_tool),
        )
        .route(
            "/agent/skill-packages/{id}/session",
            post(crate::server::plugin_sessions::request)
                .layer(axum::extract::DefaultBodyLimit::max(30 * 1024 * 1024)),
        )
        .route(
            "/agent/skill-packages/{id}/service",
            post(crate::server::skill_packages::remote::service),
        )
        .route(
            "/agent/tasks",
            get(crate::server::agent_service::list_tasks).post(crate::server::agent_service::create_task),
        )
        .route(
            "/agent/sessions",
            post(crate::server::agent_service::create_draft_task),
        )
        .route(
            "/agent/workspaces",
            get(crate::context::session_browser::projects)
                .post(crate::context::session_browser::add_project)
                .patch(crate::context::session_browser::rename_project)
                .delete(crate::context::session_browser::remove_project),
        )
        .route(
            "/agent/sessions/search",
            get(crate::context::session_browser::search),
        )
        .route("/agent/memories", get(crate::context::memory_panel::get))
        .route(
            "/agent/memories/{kind}/{id}",
            axum::routing::patch(crate::context::memory_panel::patch).delete(crate::context::memory_panel::delete),
        )
        .route(
            "/agent/sessions/{task_id}",
            axum::routing::delete(crate::context::session_browser::delete_session)
                .patch(crate::context::session_browser::update_session),
        )
        .route(
            "/agent/tasks/{task_id}",
            get(crate::server::agent_service::get_task)
                .patch(crate::server::agent_service::update_task)
                .delete(crate::server::agent_service::cancel_task),
        )
        .route(
            "/agent/tasks/{task_id}/events",
            get(crate::server::agent_service::get_events),
        )
        .route(
            "/agent/tasks/{task_id}/changes/{turn}",
            get(crate::tools::workspace_changes::get_summary),
        )
        .route(
            "/agent/tasks/{task_id}/changes/{turn}/files/{index}",
            get(crate::tools::workspace_changes::get_diff),
        )
        .route(
            "/agent/tasks/{task_id}/changes/{turn}/undo",
            post(crate::tools::workspace_changes::undo_turn),
        )
        .route(
            "/agent/tasks/{task_id}/context-debug",
            get(crate::context::request_context::setting).put(crate::context::request_context::set_setting),
        )
        .route(
            "/agent/tasks/{task_id}/request-contexts",
            get(crate::context::request_context::list),
        )
        .route(
            "/agent/tasks/{task_id}/request-contexts/{id}",
            get(crate::context::request_context::get),
        )
        .route(
            "/agent/tasks/{task_id}/visual-artifacts/{id}/image",
            get(crate::media::visual_artifacts::image_route),
        )
        .route("/agent/tasks/{task_id}/rewind", post(crate::context::rewind::post))
        .route(
            "/agent/tasks/{task_id}/interjections",
            post(crate::context::interjections::post),
        )
        .route(
            "/agent/tasks/{task_id}/approvals",
            post(crate::tools::tool_approvals::decide),
        )
        .route(
            "/agent/tasks/{task_id}/skill-draft",
            post(crate::runtime::flow_skills::draft),
        )
        .route("/agent/tasks/{task_id}/programs", get(crate::server::workflow_drafts::programs))
        .route("/agent/tasks/{task_id}/workflow-drafts", post(crate::server::workflow_drafts::create))
        .route("/agent/workflow-drafts", get(crate::server::workflow_drafts::list))
        .route("/agent/workflow-drafts/{id}", get(crate::server::workflow_drafts::read))
        .route("/agent/workflow-drafts/{id}/chat", post(crate::server::workflow_drafts::chat))
        .route("/agent/workflow-drafts/{id}/publish", post(crate::server::workflow_drafts::publish))
        .route("/agent/tasks/{task_id}/flow-library", post(crate::server::workflow_drafts::publish_task))
        .route(
            "/agent/tasks/{task_id}/skills",
            post(crate::runtime::flow_skills::save_route),
        )
        .route(
            "/agent/tasks/{task_id}/frames",
            get(crate::runtime::frame_view::list),
        )
        .route(
            "/agent/tasks/{task_id}/frames/{node_id}",
            get(crate::runtime::frame_view::get),
        )
        .route(
            "/agent/tasks/{task_id}/messages",
            post(crate::server::agent_service::continue_task)
                .layer(axum::extract::DefaultBodyLimit::max(280 * 1024 * 1024)),
        )
        .route(
            "/agent/tasks/{task_id}/stream",
            get(crate::server::agent_service::stream_events),
        )
        .route(
            "/api/session/uploadFileBinary",
            post(crate::media::attachments::upload).layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .route(
            "/agent/tasks/{task_id}/attachments/{receipt}",
            get(crate::media::attachments::download),
        )
        .with_state(agent_state);

    let state = AiProxyState {
        config,
        config_path: config_path.to_path_buf(),
        settings_write: Arc::new(Mutex::new(())),
        client,
        runtime: runtime.clone(),
    };

    let app = Router::new()
        .route("/star.png", get(crate::server::agent_web::star_png))
        .route("/logo.png", get(crate::server::agent_web::star_png))
        .route("/favicon.png", get(crate::server::agent_web::favicon_png))
        .route("/favicon.ico", get(crate::server::agent_web::favicon_png))
        .route(
            "/agent/settings/data",
            get(get_agent_settings).put(put_agent_settings),
        )
        .route("/agent/models", get(get_model_catalog))
        .route("/agent/settings/shell", get(get_shell_environment))
        .route("/agent/settings/shell/detect", post(detect_shell_environment))
        .route("/agent/settings/shell/install", get(get_shell_install).post(start_shell_install).delete(cancel_shell_install))
        .route("/agent/settings/shell/store", post(open_shell_store))
        .route("/agent/settings/discover", post(discover_agent_models))
        .route("/agent/settings/probe-image", post(probe_agent_image_input))
        .route("/agent/plugins", get(plugin_tree))
        .route(
            "/agent/skill-page-sdk.js",
            get(|| async {
                (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "text/javascript; charset=utf-8",
                    )],
                    include_str!("../../web/skill-page-sdk.js"),
                )
            }),
        )
        .route(
            "/agent/skill-packages",
            get(skill_packages).post(install_skill_package),
        )
        .route("/agent/skill-packages/remote", post(connect_remote_package))
        .route(
            "/agent/plugins/{id}/{method}",
            post(remote_page_request).layer(axum::extract::DefaultBodyLimit::max(30 * 1024 * 1024)),
        )
        .route(
            "/agent/skill-packages/{id}",
            axum::routing::delete(remove_skill_package),
        )
        .route(
            "/agent/skill-packages/{id}/reload",
            post(reload_skill_package),
        )
        .route(
            "/agent/skill-packages/{id}/enabled",
            post(enable_skill_package),
        )
        .route(
            "/agent/skill-packages/{id}/pages/{page}/{*resource}",
            get(skill_page_asset),
        )
        .merge(agent_routes)
        .with_state(state)
        .layer(CorsLayer::permissive());

    let addr = listener.local_addr()?;
    crate::tools::process_sandbox::register_control_port(addr.port());
    let app = if std::env::args().any(|arg| arg == "--desktop-agent") {
        app.layer(axum::middleware::from_fn_with_state(
            addr.to_string(),
            desktop_origin_guard,
        ))
    } else {
        app
    };
    let app = app.layer(axum::middleware::from_fn_with_state(
        addr,
        crate::tools::process_sandbox::local_http_guard,
    ));
    info!(%addr, "Agent service starting");

    let served = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await;
    runtime.shutdown()?;
    served?;
    Ok(())
}

#[cfg(test)]
mod tests {
    include!("parts/ai_proxy_tests.inc.rs");
}
