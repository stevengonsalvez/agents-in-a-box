//! Cost estimation. Lifted verbatim from `crates/ainb-core/src/models/usage.rs`
//! so both the in-tree CLI (during the 6c-cli transition) and the
//! plugin produce the same dollar figures for the same input.

/// Per-million-token rates from `usage.rs::model_rates`. The plugin
/// keeps the same model coverage (Claude opus / sonnet / haiku, GPT
/// 5 / 4.1 / 4o) and the same fallback (None for unknown models).
struct ModelRates {
    input: f64,
    output: f64,
    cache_write: f64,
    cache_read: f64,
}

#[allow(clippy::too_many_arguments)]
pub fn estimate_cost_usd(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    reasoning_tokens: u64,
) -> Option<f64> {
    let rates = model_rates(model)?;
    Some(
        input_tokens as f64 * rates.input
            + (output_tokens + reasoning_tokens) as f64 * rates.output
            + cache_creation_tokens as f64 * rates.cache_write
            + cache_read_tokens as f64 * rates.cache_read,
    )
}

fn model_rates(model: &str) -> Option<ModelRates> {
    let canonical = canonical_model_name(model);
    let (input_per_million, output_per_million) = if canonical.starts_with("claude-opus") {
        (15.0, 75.0)
    } else if canonical.starts_with("claude-sonnet") || canonical.starts_with("claude-3-5-sonnet") {
        (3.0, 15.0)
    } else if canonical.starts_with("claude-haiku") || canonical.starts_with("claude-3-5-haiku") {
        (0.8, 4.0)
    } else if canonical.starts_with("gpt-5")
        || canonical.starts_with("gpt-4.1")
        || canonical.starts_with("gpt-4o")
    {
        (1.25, 10.0)
    } else {
        return None;
    };

    let input = input_per_million / 1_000_000.0;
    let output = output_per_million / 1_000_000.0;
    Some(ModelRates {
        input,
        output,
        cache_write: input * 1.25,
        cache_read: input * 0.1,
    })
}

fn canonical_model_name(model: &str) -> String {
    let without_prefix = model
        .split('@')
        .next()
        .unwrap_or(model)
        .trim_start_matches("anthropic/")
        .trim_start_matches("openai/")
        .to_string();

    // Strip a trailing 8-digit date stamp (Claude versioning).
    if without_prefix
        .rsplit('-')
        .next()
        .is_some_and(|suffix| suffix.len() == 8 && suffix.chars().all(|ch| ch.is_ascii_digit()))
    {
        without_prefix
            .rsplit_once('-')
            .map_or(without_prefix.clone(), |(name, _)| name.to_string())
    } else {
        without_prefix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_model_returns_none() {
        assert!(estimate_cost_usd("totally-made-up", 100, 100, 0, 0, 0).is_none());
    }

    #[test]
    fn claude_sonnet_priced_at_3_15_per_million() {
        // 1M input + 0M output + 0 cache → $3.00.
        let cost = estimate_cost_usd("claude-3-5-sonnet", 1_000_000, 0, 0, 0, 0);
        assert!((cost.unwrap() - 3.0).abs() < 1e-6);
    }

    #[test]
    fn gpt_5_priced_at_1_25_10_per_million() {
        let cost = estimate_cost_usd("gpt-5", 1_000_000, 0, 0, 0, 0);
        assert!((cost.unwrap() - 1.25).abs() < 1e-6);
    }

    #[test]
    fn date_stamped_claude_strips_to_canonical() {
        // claude-3-5-sonnet-20241022 → claude-3-5-sonnet → 3.0/$M input.
        let cost = estimate_cost_usd("claude-3-5-sonnet-20241022", 1_000_000, 0, 0, 0, 0);
        assert!((cost.unwrap() - 3.0).abs() < 1e-6);
    }
}
