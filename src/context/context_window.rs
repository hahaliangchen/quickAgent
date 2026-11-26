use serde_json::Value;
#[cfg(test)]
use serde_json::json;

#[derive(Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct TokenCalibration {
    route: String,
    ratio: Option<f64>,
}

impl TokenCalibration {
    pub fn estimate(&self, value: &Value, route: &str) -> usize {
        let ratio = if self.route == route {
            self.ratio.unwrap_or(1.0)
        } else {
            1.0
        };
        (estimate(value) as f64 * ratio).ceil() as usize
    }

    pub fn observe(&mut self, route: &str, estimated: usize, actual: usize) {
        if estimated == 0 || actual == 0 {
            return;
        }
        self.route = route.to_owned();
        // Include modest headroom for tokenizer/framing differences. Missing or
        // implausible usage must not make the host's estimate unbounded.
        self.ratio = Some((actual as f64 / estimated as f64 * 1.05).clamp(0.1, 10.0));
    }
}

/// Maximum field of view, not a target request size. Providers do not share a
/// tokenizer; use a conservative text estimate and reserve room for output.
pub const MAX_TOKENS: usize = 256_000;

/// Known two-times pricing/context cliffs. Unknown routes use the budget-only
/// economic line in the caller.
pub fn pricing_cliff(model: &str, _input_budget: usize) -> Option<usize> {
    if let Some(threshold) =
        crate::model::model_presets::lookup_preset(model).and_then(|p| p.long_context_threshold)
    {
        return Some(threshold);
    }
    let lower = model.to_ascii_lowercase();
    if (lower.contains("gpt-5.4") && !lower.contains("mini") && !lower.contains("nano"))
        || lower.contains("gpt-5.6")
        || lower.contains("gpt-6")
        || lower.contains("gpt-6.1")
        || lower.contains("gpt-6.2")
        || lower.contains("terra")
        || lower.contains("272k")
    {
        Some(272_000)
    } else if lower.contains("claude") || lower.contains("anthropic") {
        Some(200_000)
    } else {
        None
    }
}

pub fn configured_pricing_cliff(model: &str, configured: Option<usize>) -> Option<usize> {
    configured.or_else(|| pricing_cliff(model, 0))
}

#[cfg(test)]
pub fn soft_limit(model: &str, input_budget: usize) -> usize {
    let budget_line = ((input_budget as f64) * 0.75) as usize;
    pricing_cliff(model, input_budget)
        .map(|cliff| (((cliff as f64) * 0.85) as usize).min(budget_line))
        .unwrap_or(budget_line)
}

pub fn hard_guard(input_budget: usize) -> usize {
    ((input_budget as f64) * 0.80) as usize
}

pub fn hard_limit(model: &str, input_budget: usize, configured: Option<usize>) -> usize {
    let budget_line = hard_guard(input_budget);
    configured_pricing_cliff(model, configured)
        .map(|cliff| (((cliff as f64) * 0.85) as usize).min(budget_line))
        .unwrap_or(budget_line)
}
pub fn estimate(value: &Value) -> usize {
    match value {
        Value::String(text) => {
            let ascii = text.bytes().filter(u8::is_ascii).count();
            let unicode = text.chars().filter(|ch| !ch.is_ascii()).count();
            ascii.div_ceil(3) + unicode * 2
        }
        Value::Array(items) => items.iter().map(estimate).sum::<usize>() + items.len() * 4,
        // Image transport bytes are not text tokens. This is an allowance for
        // the supported task image input, not a count of its base64 encoding.
        Value::Object(fields)
            if matches!(
                fields.get("type").and_then(Value::as_str),
                Some("image_url" | "image_ref")
            ) =>
        {
            4096
        }
        // Encrypted Responses API reasoning blobs (`encrypted_content`) and
        // Anthropic thinking `signature` blobs are opaque ciphertexts, not text
        // prompt tokens — exclude them from the pre-request text estimate so
        // ContextRing doesn't spike before `provider_usage` arrives.
        Value::Object(fields) => fields
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "encrypted_content" | "signature"))
            .map(|(key, value)| key.len().div_ceil(3) + estimate(value) + 4)
            .sum(),
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calibration_is_scoped_to_provider_model_and_ignores_missing_usage() {
        let mut calibration = TokenCalibration::default();
        let input = json!({"messages":[{"role":"user","content":"中文材料".repeat(500)}]});
        let raw = estimate(&input);
        assert_eq!(calibration.estimate(&input, "a/model"), raw);
        calibration.observe("a/model", raw, raw / 3);
        let adjusted = calibration.estimate(&input, "a/model");
        assert!(adjusted < raw / 2 && adjusted >= raw / 3);
        assert_eq!(calibration.estimate(&input, "b/model"), raw);
        calibration.observe("a/model", raw, 0);
        assert_eq!(calibration.estimate(&input, "a/model"), adjusted);
        let restored: TokenCalibration =
            serde_json::from_value(serde_json::to_value(&calibration).unwrap()).unwrap();
        assert_eq!(restored.estimate(&input, "a/model"), adjusted);
    }
    #[test]
    fn image_references_and_wire_images_have_same_budget() {
        assert_eq!(
            estimate(&json!({"type":"image_ref","artifact_id":"v"})),
            estimate(&json!({"type":"image_url","image_url":{"url":"data:image/png;base64,abc"}}))
        );
    }

    #[test]
    fn encrypted_reasoning_and_thinking_signatures_do_not_inflate_estimate() {
        let base = json!({"input":[{"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"ok"}]}]});
        let with_enc = json!({"input":[{"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"ok"}],"encrypted_content":"A".repeat(60_000)}]});
        assert_eq!(estimate(&base), estimate(&with_enc));
    }

    #[test]
    fn pricing_lines_use_known_cliffs_and_budget_fallback() {
        assert_eq!(pricing_cliff("gpt-6.1-terra", 300_000), Some(272_000));
        assert_eq!(pricing_cliff("gpt-6.2", 300_000), Some(272_000));
        assert_eq!(soft_limit("gpt-6.1-terra", 300_000), 225_000);
        assert_eq!(soft_limit("local-model", 100_000), 75_000);
        assert_eq!(hard_guard(100_000), 80_000);
        assert_eq!(hard_limit("gpt-6.2", 300_000, None), 231_200);
        assert_eq!(hard_limit("local-model", 100_000, Some(90_000)), 76_500);
        assert_eq!(hard_limit("local-model", 100_000, Some(120_000)), 80_000);
    }
}
