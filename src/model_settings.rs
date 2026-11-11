//! Optional per-use overrides; resolving them never mutates the main state.
use crate::agent_service::AgentServiceState;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

pub const OUTPUT_RESERVE: usize = 24_576;
/// Used when the DSH model entry does not declare `maxTokens`.
pub const DEFAULT_MAX_OUTPUT: usize = 32_768;
/// Sanity ceiling for a declared context window. Real vendor values stay far
/// below it; it only rejects a nonsensical declaration.
pub const MAX_CONTEXT_WINDOW: usize = 2_000_000;
/// Reasoning levels the host accepts, in picker order. A model entry may name
/// other keys; they are folded into these by [`canonical_effort`] so a declared
/// level is always one the settings screen can save and a request can send.
pub const REASONING_EFFORTS: [&str; 6] = ["none", "low", "medium", "high", "xhigh", "max"];

/// The accepted level a declared one stands for, or `None` when the host has no
/// equivalent. `minimal` maps to `low` because both reach the wire as `low`:
/// thinking is never off, so the lowest level is the floor.
pub fn canonical_effort(level: &str) -> Option<&'static str> {
    match level.trim().to_ascii_lowercase().as_str() {
        "false" | "off" | "none" => Some("none"),
        "minimal" | "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some("xhigh"),
        "max" => Some("max"),
        _ => None,
    }
}

/// Folds a declared level list onto the accepted ones, in picker order and
/// without duplicates; keys the host cannot send are dropped.
pub fn canonical_efforts(levels: &[String]) -> Vec<String> {
    let mut folded: Vec<&str> = levels.iter().filter_map(|level| canonical_effort(level)).collect();
    folded.sort_by_key(|level| REASONING_EFFORTS.iter().position(|known| known == level));
    folded.dedup();
    folded.into_iter().map(str::to_owned).collect()
}

/// A model's own declared context window. An entry that declares nothing falls
/// back to the conservative default instead of silently capping a real window.
pub fn context_window(state: &AgentServiceState, model: &str) -> usize {
    declared_context_window(state, model)
        .unwrap_or(crate::context_window::MAX_TOKENS)
        .min(MAX_CONTEXT_WINDOW)
}

/// A vendor keys its declarations by its own model id while a request may name
/// either side of an alias mapping, so look the alias up in both directions.
fn declared<'a, V>(
    table: &'a std::collections::HashMap<String, V>,
    aliases: &'a std::collections::HashMap<String, String>,
    model: &str,
) -> Option<&'a V> {
    let upstream = aliases.get(model).map(String::as_str).unwrap_or(model);
    table
        .get(upstream)
        .or_else(|| table.get(model))
        .or_else(|| {
            aliases
                .iter()
                .find(|(_, mapped)| mapped.as_str() == model)
                .and_then(|(local, _)| table.get(local))
        })
}

fn declared_context_window(state: &AgentServiceState, model: &str) -> Option<usize> {
    let route = state.provider_routes.get(&state.provider_name)?;
    declared(&route.context_windows, &route.models, model).copied()
}

fn declared_max_output(state: &AgentServiceState, model: &str) -> Option<usize> {
    let route = state.provider_routes.get(&state.provider_name)?;
    declared(&route.max_output_tokens, &route.models, model).copied()
}

pub fn input_budget(state: &AgentServiceState, model: &str) -> usize {
    let capacity = context_window(state, model);
    capacity
        .saturating_sub(OUTPUT_RESERVE.min(capacity / 4))
        .max(1)
}

/// The model's declared output limit, bounded by what the window has left
/// after `input` tokens. Small windows reserve one quarter for generation.
pub fn output_tokens(
    state: &AgentServiceState,
    model: &str,
    input_budget: usize,
    input: usize,
) -> usize {
    let declared = declared_max_output(state, model).unwrap_or(DEFAULT_MAX_OUTPUT);
    let capacity = input_budget.saturating_add(OUTPUT_RESERVE.min(input_budget / 3));
    declared
        .min(
            declared_context_window(state, model)
                .unwrap_or(capacity)
                .min(capacity)
                .saturating_sub(input),
        )
        .max(1)
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelSettings {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub context_window: Option<usize>,
}

/// Generation defaults belong to the plugin, independent of its model allowlist.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginGeneration {
    pub reasoning_effort: Option<String>,
    pub visual_reasoning_effort: Option<String>,
}
impl PluginGeneration {
    pub fn normalize(&mut self) -> Result<()> {
        for effort in [
            &mut self.reasoning_effort,
            &mut self.visual_reasoning_effort,
        ] {
            let mut settings = ModelSettings {
                reasoning_effort: effort.take(),
                ..Default::default()
            };
            settings.normalize()?;
            *effort = settings.reasoning_effort;
        }
        Ok(())
    }
}
impl ModelSettings {
    pub fn normalize(&mut self) -> Result<()> {
        for value in [
            &mut self.provider,
            &mut self.model,
            &mut self.reasoning_effort,
        ] {
            *value = value
                .take()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty());
        }
        // An alias written by hand (`false`, `minimal`) is folded onto the level
        // it stands for; a level the host cannot send is still refused.
        if let Some(level) = self.reasoning_effort.as_deref().and_then(canonical_effort) {
            self.reasoning_effort = Some(level.to_owned());
        }
        ensure!(
            self.reasoning_effort
                .as_deref()
                .is_none_or(|s| REASONING_EFFORTS.contains(&s)),
            "unsupported reasoning effort"
        );
        ensure!(
            self.context_window
                .is_none_or(|n| (32_768..=MAX_CONTEXT_WINDOW).contains(&n)),
            "context window must be between 32768 and {MAX_CONTEXT_WINDOW} tokens"
        );
        Ok(())
    }
    pub fn input_budget(&self, inherited: usize) -> usize {
        self.context_window
            .map(|n| n.saturating_sub(OUTPUT_RESERVE.min(n / 4)))
            .unwrap_or(inherited)
    }
    pub fn resolved_input_budget(&self, state: &AgentServiceState, model: &str) -> usize {
        let catalog = declared_context_window(state, model);
        let capacity = match (self.context_window, catalog) {
            (Some(configured), Some(declared)) => configured.min(declared),
            (configured, declared) => configured
                .or(declared)
                .unwrap_or(crate::context_window::MAX_TOKENS),
        };
        capacity
            .saturating_sub(OUTPUT_RESERVE.min(capacity / 4))
            .max(1)
    }
    pub fn resolve(&self, main: &AgentServiceState, main_model: &str) -> Result<AgentServiceState> {
        let mut state = main.clone();
        if let Some(provider) = &self.provider {
            let route = main
                .provider_routes
                .get(provider)
                .context("configured model provider is unavailable")?;
            state.provider_name = provider.clone();
            state.provider_url = route.url.clone();
            state.api_key = route.api_key.clone();
            state.model_map = route.models.clone();
        }
        let model = self.model.as_deref().unwrap_or(main_model);
        // Dedicated uses resolve registered references, never arbitrary upstream IDs.
        if !main.provider_routes.is_empty() {
            ensure!(
                state.model_map.contains_key(model)
                    || state.model_map.values().any(|id| id == model),
                "dedicated task model is not in the selected provider's catalog"
            );
        }
        state.default_model = state
            .model_map
            .get(model)
            .cloned()
            .unwrap_or_else(|| model.to_owned());
        if let Some(effort) = &self.reasoning_effort {
            state.reasoning_effort = Some(effort.clone());
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_overrides_preserve_main_and_empty_fields_inherit() {
        let root = std::env::temp_dir().join(crate::visual_artifacts::new_id());
        std::fs::create_dir_all(&root).unwrap();
        let mut main =
            crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse().unwrap());
        main.reasoning_effort = Some("high".into());
        main.model_map = ["turn-model", "gpt-6.1-sol"]
            .into_iter()
            .map(|m| (m.into(), m.into()))
            .collect();
        main.provider_routes.insert(
            "alternate".into(),
            crate::agent_service::AgentProviderRoute {
                prefix_system_messages: false,
                responses: false,
                url: "http://alternate.example/v1".into(),
                api_key: "fixture-key".into(),
                models: [("alias".into(), "mapped-model".into())]
                    .into_iter()
                    .collect(),
                max_output_tokens: [("mapped-model".into(), 64_000)].into_iter().collect(),
                context_windows: Default::default(),
            },
        );
        let routed = ModelSettings {
            provider: Some("alternate".into()),
            model: Some("alias".into()),
            ..Default::default()
        }
        .resolve(&main, "turn-model")
        .unwrap();
        assert_eq!(routed.provider_url, "http://alternate.example/v1");
        assert_eq!(routed.default_model, "mapped-model");
        assert_eq!(routed.api_key, "fixture-key");
        assert_ne!(main.provider_name, routed.provider_name);
        assert_eq!(
            output_tokens(&routed, "mapped-model", 231_424, 1_000),
            64_000
        );
        // The alias a request names resolves to the same declared limit.
        assert_eq!(
            output_tokens(&routed, "alias", 231_424, 1_000),
            64_000
        );
        assert_eq!(
            output_tokens(&routed, "mapped-model", 231_424, 231_424),
            OUTPUT_RESERVE
        );
        assert_eq!(
            output_tokens(&routed, "undeclared", 231_424, 1_000),
            DEFAULT_MAX_OUTPUT
        );
        let mut bounded = routed.clone();
        bounded
            .provider_routes
            .get_mut("alternate")
            .unwrap()
            .context_windows
            .insert("mapped-model".into(), 65_536);
        assert_eq!(context_window(&bounded, "alias"), 65_536);
        assert_eq!(input_budget(&bounded, "mapped-model"), 49_152);
        assert_eq!(
            output_tokens(&bounded, "mapped-model", 49_152, 49_152),
            16_384
        );
        assert_eq!(
            context_window(&bounded, "missing"),
            crate::context_window::MAX_TOKENS
        );

        let inherited = ModelSettings::default()
            .resolve(&main, "turn-model")
            .unwrap();
        assert_eq!(inherited.default_model, "turn-model");
        assert_eq!(inherited.reasoning_effort, main.reasoning_effort);
        assert_eq!(ModelSettings::default().input_budget(231424), 231424);
        let config = ModelSettings {
            model: Some("gpt-6.1-sol".into()),
            reasoning_effort: Some("low".into()),
            context_window: Some(500000),
            ..Default::default()
        };
        let child = config.resolve(&main, "turn-model").unwrap();
        assert_eq!(child.default_model, "gpt-6.1-sol");
        assert_eq!(child.reasoning_effort.as_deref(), Some("low"));
        assert!(
            ModelSettings {
                model: Some("unlisted".into()),
                ..Default::default()
            }
            .resolve(&main, "turn-model")
            .is_err()
        );
        assert_eq!(config.input_budget(231424), 475424);
        assert_eq!(config.resolved_input_budget(&main, "turn-model"), 475424);
        assert_eq!(
            config.resolved_input_budget(&bounded, "mapped-model"),
            49152
        );
        assert_eq!(main.reasoning_effort.as_deref(), Some("high"));
        assert_ne!(main.default_model, child.default_model);
        let mut invalid = config.clone();
        invalid.context_window = Some(1000);
        assert!(invalid.normalize().is_err());
        let mut empty = ModelSettings {
            model: Some("  ".into()),
            ..Default::default()
        };
        empty.normalize().unwrap();
        assert_eq!(empty, ModelSettings::default());
        let _ = std::fs::remove_dir_all(root);
    }
    #[test]
    fn a_declared_vendor_window_is_used_instead_of_the_default_cap() {
        let root = std::env::temp_dir().join(crate::visual_artifacts::new_id());
        std::fs::create_dir_all(&root).unwrap();
        let mut state =
            crate::agent_service::tests::test_state(&root, "127.0.0.1:1".parse().unwrap());
        state.provider_name = "deepseek".into();
        state.provider_routes.insert(
            "deepseek".into(),
            crate::agent_service::AgentProviderRoute {
                prefix_system_messages: false,
                responses: false,
                url: "https://api.deepseek.com/v1".into(),
                api_key: "fixture-key".into(),
                models: [("deepseek-flash".into(), "deepseek-flash".into())]
                    .into_iter()
                    .collect(),
                max_output_tokens: [("deepseek-flash".into(), 393_216)].into_iter().collect(),
                context_windows: [("deepseek-flash".into(), 1_048_576)].into_iter().collect(),
            },
        );
        // The declared 1M window is what a request may use, not the fallback.
        assert_eq!(context_window(&state, "deepseek-flash"), 1_048_576);
        assert_eq!(input_budget(&state, "deepseek-flash"), 1_024_000);
        assert_eq!(
            output_tokens(&state, "deepseek-flash", 1_024_000, 1_000),
            393_216
        );
        let mut declared = ModelSettings {
            context_window: Some(1_048_576),
            ..Default::default()
        };
        declared.normalize().unwrap();
        let mut beyond_ceiling = ModelSettings {
            context_window: Some(MAX_CONTEXT_WINDOW + 1),
            ..Default::default()
        };
        assert!(beyond_ceiling.normalize().is_err());
        let _ = std::fs::remove_dir_all(root);
    }
    #[test]
    fn a_declared_level_is_folded_onto_one_the_host_accepts() {
        assert_eq!(canonical_effort(" minimal "), Some("low"));
        assert_eq!(canonical_effort("LOW"), Some("low"));
        assert_eq!(canonical_effort("false"), Some("none"));
        assert_eq!(canonical_effort("off"), Some("none"));
        assert_eq!(canonical_effort("max"), Some("max"));
        assert_eq!(canonical_effort("nano"), None);
        assert_eq!(canonical_effort(""), None);
        // Every folded level is storable, so a model entry can never leave the
        // settings screen with a value it refuses to save.
        for level in ["minimal", "false", "off", "none", "low", "max"] {
            let mut settings = ModelSettings {
                reasoning_effort: canonical_effort(level).map(str::to_owned),
                ..Default::default()
            };
            settings.normalize().unwrap();
            assert_eq!(settings.reasoning_effort.as_deref(), canonical_effort(level));
        }
        // A hand-written level list folds in picker order, unknown keys dropped.
        assert_eq!(
            canonical_efforts(&[
                "max".to_owned(),
                "minimal".to_owned(),
                "false".to_owned(),
                "nano".to_owned(),
                "high".to_owned(),
            ]),
            vec!["none", "low", "high", "max"]
        );
        assert!(canonical_efforts(&[]).is_empty());
        // A stored alias is folded instead of failing the whole settings load.
        let mut aliased = ModelSettings {
            reasoning_effort: Some(" minimal ".to_owned()),
            ..Default::default()
        };
        aliased.normalize().unwrap();
        assert_eq!(aliased.reasoning_effort.as_deref(), Some("low"));
        let mut unknown = ModelSettings {
            reasoning_effort: Some("nano".to_owned()),
            ..Default::default()
        };
        assert!(unknown.normalize().is_err());
    }
}
