//! Process-only model routes. They never enter the shared on-disk configuration.
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{OnceLock, RwLock},
};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalModelProvider {
    pub url: String,
    #[serde(default)]
    pub api_key: String,
    pub model: String,
    #[serde(default = "super::ai_proxy::default_api_type")]
    pub protocol: String,
    pub provider_id: Option<String>,
}

impl ExternalModelProvider {
    pub fn validate(mut self) -> Result<Self> {
        ensure!(
            !self.url.chars().any(char::is_control)
                && (self.url.starts_with("http://") || self.url.starts_with("https://")),
            "model provider URL must start with http:// or https:// and contain no control characters"
        );
        let url = reqwest::Url::parse(&self.url)
            .map_err(|_| anyhow::anyhow!("invalid model provider URL"))?;
        ensure!(
            url.has_host() && url.username().is_empty() && url.password().is_none()
                && url.query().is_none() && url.fragment().is_none(),
            "model provider URL must not include credentials, a query, or a fragment"
        );
        self.url = url.as_str().trim_end_matches('/').to_owned();
        ensure!(
            !self.model.trim().is_empty() && !self.model.chars().any(char::is_control),
            "model provider model must be nonempty and contain no control characters"
        );
        self.model = self.model.trim().to_owned();
        ensure!(super::ai_proxy::agent_api(&self.protocol), "unsupported model provider protocol");
        ensure!(
            self.provider_id.as_ref().is_none_or(|id| !id.is_empty() && id.bytes().all(|b|
                b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')
            )),
            "model provider ID must contain only ASCII letters, digits, hyphens, or underscores"
        );
        ensure!(
            reqwest::header::HeaderValue::from_str(&self.api_key).is_ok(),
            "invalid model provider API key"
        );
        Ok(self)
    }

    pub fn id(&self, plugin: &str) -> String {
        self.provider_id.clone().unwrap_or_else(|| format!("ext-{plugin}"))
    }

    pub fn summary(&self, plugin: &str) -> Value {
        json!({"plugin_id":plugin,"provider":self.id(plugin),"model":self.model,
            "protocol":self.protocol,"url":self.url})
    }
}

#[derive(Default)]
pub(super) struct ExternalProviderRegistry {
    environment: Option<(ExternalModelProvider, HashSet<String>)>,
    dynamic: RwLock<HashMap<String, ExternalModelProvider>>,
}

impl ExternalProviderRegistry {
    pub(super) fn from_environment(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let url = get("EXTERNAL_LLM_BASE_URL").unwrap_or_default();
        let model = get("EXTERNAL_LLM_MODEL").unwrap_or_default();
        if url.trim().is_empty() || model.trim().is_empty() {
            return Ok(Self::default());
        }
        let provider = ExternalModelProvider {
            url,
            model,
            api_key: get("EXTERNAL_LLM_API_KEY").unwrap_or_default(),
            protocol: get("EXTERNAL_LLM_PROTOCOL").unwrap_or_else(super::ai_proxy::default_api_type),
            provider_id: Some(get("EXTERNAL_LLM_PROVIDER_ID").unwrap_or_else(|| "external-upstream".into())),
        }.validate()?;
        let plugins = get("EXTERNAL_LLM_PLUGINS").unwrap_or_default().split(',')
            .map(str::trim).filter(|id| !id.is_empty()).map(str::to_owned).collect();
        Ok(Self { environment: Some((provider, plugins)), ..Self::default() })
    }

    pub(super) fn for_plugin(&self, plugin: &str) -> Option<ExternalModelProvider> {
        self.dynamic.read().unwrap().get(plugin).cloned().or_else(|| {
            self.environment.as_ref().filter(|(_, plugins)| plugins.contains(plugin))
                .map(|(provider, _)| provider.clone())
        })
    }

    pub(super) fn catalog_only(&self) -> Option<&ExternalModelProvider> {
        self.environment.as_ref().filter(|(_, plugins)| plugins.is_empty())
            .map(|(provider, _)| provider)
    }

    pub(super) fn bind(&self, plugin: &str, provider: ExternalModelProvider) {
        self.dynamic.write().unwrap().insert(plugin.to_owned(), provider);
    }

    pub(super) fn unbind(&self, plugin: &str) -> Option<ExternalModelProvider> {
        self.dynamic.write().unwrap().remove(plugin)
    }
}

pub(super) fn registry() -> &'static ExternalProviderRegistry {
    static REGISTRY: OnceLock<ExternalProviderRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        ExternalProviderRegistry::from_environment(|key| std::env::var(key).ok())
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "ignoring invalid external model environment configuration");
                ExternalProviderRegistry::default()
            })
    })
}
