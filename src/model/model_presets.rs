//! Unified vendor/model specifications compiled directly into the binary from
//! `config/model-presets.json`.
//!
//! Keeping all vendor specifications in one repository JSON file and embedding
//! it via `include_str!` guarantees identical context windows, max output
//! limits, reasoning effort tiers, fast-mode flags, and long-context pricing
//! cliffs across Windows, macOS, and Linux builds without per-machine setup.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

const EMBEDDED_PRESETS_JSON: &str = include_str!("../../config/model-presets.json");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModelPreset {
    pub context_window: usize,
    pub max_output_tokens: usize,
    #[serde(default)]
    pub reasoning_efforts: Vec<String>,
    #[serde(default)]
    pub default_effort: Option<String>,
    #[serde(default)]
    pub fast_mode: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_context_threshold: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct VendorPreset {
    pub id: String,
    pub name: String,
    pub default_base_url: String,
    pub default_api_type: String,
    pub models: BTreeMap<String, ModelPreset>,
}

#[derive(Debug, Clone, Deserialize)]
struct PresetCatalogFile {
    #[allow(dead_code)]
    version: u32,
    vendors: Vec<VendorPreset>,
}

struct CompiledPresets {
    vendors: Vec<VendorPreset>,
    models: HashMap<String, ModelPreset>,
}

fn compiled() -> &'static CompiledPresets {
    static REGISTRY: OnceLock<CompiledPresets> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let parsed: PresetCatalogFile = serde_json::from_str(EMBEDDED_PRESETS_JSON)
            .expect("config/model-presets.json must be valid JSON matching PresetCatalogFile");
        let mut models = HashMap::new();
        for vendor in &parsed.vendors {
            for (id, preset) in &vendor.models {
                models.insert(id.to_ascii_lowercase(), preset.clone());
            }
        }
        CompiledPresets {
            vendors: parsed.vendors,
            models,
        }
    })
}

/// Normalize a model ID by lowercasing, stripping an optional vendor prefix
/// (e.g. `openai/gpt-6.2-sol`), and stripping trailing `-YYYY-MM-DD` date
/// snapshots or `-latest` aliases.
pub(crate) fn normalize_model_id(raw: &str) -> String {
    let trimmed = raw.trim().to_ascii_lowercase();
    let without_vendor = trimmed
        .rsplit_once('/')
        .map(|(_, name)| name)
        .unwrap_or(trimmed.as_str());
    let without_latest = without_vendor
        .strip_suffix("-latest")
        .unwrap_or(without_vendor);
    strip_date_suffix(without_latest).to_owned()
}

fn strip_date_suffix(id: &str) -> &str {
    // Match trailing `-YYYY-MM-DD` (11 ASCII bytes).
    let bytes = id.as_bytes();
    if bytes.len() > 11 {
        let split = bytes.len() - 11;
        let tail = &bytes[split..];
        if tail[0] == b'-'
            && tail[1..5].iter().all(u8::is_ascii_digit)
            && tail[5] == b'-'
            && tail[6..8].iter().all(u8::is_ascii_digit)
            && tail[8] == b'-'
            && tail[9..11].iter().all(u8::is_ascii_digit)
        {
            return &id[..split];
        }
    }
    id
}

/// Look up compiled-in official vendor specifications for a known model ID.
pub(crate) fn lookup_preset(model: &str) -> Option<&'static ModelPreset> {
    let normalized = normalize_model_id(model);
    compiled().models.get(&normalized)
}

/// Serialize compiled-in vendor & model presets for the Settings UI.
pub(crate) fn presets_view() -> Value {
    let registry = compiled();
    let mut flat_models = BTreeMap::new();
    for vendor in &registry.vendors {
        for (id, preset) in &vendor.models {
            flat_models.insert(id.clone(), preset.clone());
        }
    }
    json!({
        "vendors": registry.vendors,
        "models": flat_models,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::model_settings::{MAX_CONTEXT_WINDOW, REASONING_EFFORTS};
    use std::collections::HashSet;

    #[test]
    fn embedded_presets_file_is_valid_unique_and_canonical() {
        let parsed: PresetCatalogFile =
            serde_json::from_str(EMBEDDED_PRESETS_JSON).expect("embedded JSON parses");
        assert_eq!(parsed.version, 1);
        assert!(!parsed.vendors.is_empty());

        let mut seen_models = HashSet::new();
        for vendor in &parsed.vendors {
            assert!(!vendor.id.trim().is_empty());
            assert!(!vendor.models.is_empty());
            for (id, preset) in &vendor.models {
                assert_eq!(id, &id.to_ascii_lowercase(), "preset id must be lowercase: {id}");
                assert!(
                    seen_models.insert(id.clone()),
                    "duplicate model preset across vendors: {id}"
                );
                assert!(
                    (1_024..=MAX_CONTEXT_WINDOW).contains(&preset.context_window),
                    "invalid context_window for {id}"
                );
                assert!(
                    (1..=preset.context_window).contains(&preset.max_output_tokens),
                    "invalid max_output_tokens for {id}"
                );
                let canonical = crate::model::model_settings::canonical_efforts(&preset.reasoning_efforts);
                assert_eq!(
                    preset.reasoning_efforts, canonical,
                    "reasoning_efforts for {id} must be canonical, deduplicated, and ordered"
                );
                for level in &preset.reasoning_efforts {
                    assert!(REASONING_EFFORTS.contains(&level.as_str()));
                }
                if let Some(default) = preset.default_effort.as_deref() {
                    assert!(
                        preset.reasoning_efforts.iter().any(|l| l == default),
                        "default_effort {default} for {id} must be in reasoning_efforts"
                    );
                }
            }
        }
    }

    #[test]
    fn gpt_6_series_matches_official_openai_specs() {
        let astra62 = lookup_preset("gpt-6.2-astra").unwrap();
        assert_eq!(astra62.context_window, 1_050_000);
        assert_eq!(astra62.max_output_tokens, 128_000);
        assert_eq!(
            astra62.reasoning_efforts,
            vec!["low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(astra62.default_effort.as_deref(), Some("medium"));
        assert!(astra62.fast_mode);
        assert_eq!(astra62.long_context_threshold, Some(272_000));

        let sol62 = lookup_preset("gpt-6.2-sol-2026-11-15").unwrap();
        assert_eq!(sol62, astra62);

        let terra61 = lookup_preset("openai/gpt-6.1-terra").unwrap();
        assert_eq!(terra61.context_window, 1_050_000);
        assert_eq!(terra61.max_output_tokens, 128_000);
        assert_eq!(
            terra61.reasoning_efforts,
            vec!["none", "low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(terra61.default_effort.as_deref(), Some("medium"));
        assert!(terra61.fast_mode);

        let luna6 = lookup_preset("gpt-6-luna").unwrap();
        assert_eq!(luna6, terra61);

        let sol61 = lookup_preset("gpt-6.1-sol").unwrap();
        assert!(!sol61.reasoning_efforts.iter().any(|l| l == "none"));
        assert_eq!(sol61.context_window, 1_050_000);
        assert_eq!(sol61.max_output_tokens, 128_000);
    }

    #[test]
    fn deepseek_and_moonshot_presets_match_vendor_specs() {
        let flash = lookup_preset("deepseek-flash").unwrap();
        assert_eq!(flash.context_window, 1_048_576);
        assert_eq!(flash.max_output_tokens, 393_216);
        assert_eq!(flash.reasoning_efforts, vec!["none", "low", "high", "max"]);
        assert_eq!(flash.default_effort.as_deref(), Some("high"));
        assert!(!flash.fast_mode);

        let pro = lookup_preset("deepseek-v4-pro").unwrap();
        assert_eq!(pro, flash);

        let k3 = lookup_preset("kimi-k3").unwrap();
        assert_eq!(k3.context_window, 1_048_576);
        assert_eq!(k3.max_output_tokens, 131_072);
        assert_eq!(k3.reasoning_efforts, vec!["low", "high", "max"]);
        assert_eq!(k3.default_effort.as_deref(), Some("max"));

        let k26 = lookup_preset("kimi-k2.6").unwrap();
        assert_eq!(k26.context_window, 262_144);
        assert_eq!(k26.max_output_tokens, 65_536);
        assert!(k26.reasoning_efforts.is_empty());
        assert_eq!(k26.default_effort, None);
    }

    #[test]
    fn dated_and_unknown_models_behave_predictably() {
        let gpt54 = lookup_preset("gpt-5.4-2026-03-05").unwrap();
        assert_eq!(gpt54.context_window, 1_050_000);
        assert_eq!(gpt54.max_output_tokens, 128_000);
        assert_eq!(gpt54.default_effort.as_deref(), Some("none"));
        assert_eq!(gpt54.long_context_threshold, Some(272_000));

        assert!(lookup_preset("custom-local-model").is_none());
    }
}
