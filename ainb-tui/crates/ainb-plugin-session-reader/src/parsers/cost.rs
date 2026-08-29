//! Cost estimation — re-export of the shared rate table.
//!
//! The table itself lives in `ainb-model-rates` so the parser, the host's
//! legacy usage path, and burndown's CLI all price identically. It used to be
//! copied into all three, which is how every Opus stayed at the retired
//! $15/$75 rate for three model generations.

pub use ainb_model_rates::estimate_cost_usd;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_model_returns_none() {
        assert!(estimate_cost_usd("totally-made-up", 100, 100, 0, 0, 0).is_none());
    }

    #[test]
    fn claude_sonnet_priced_at_3_15_per_million() {
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
        let cost = estimate_cost_usd("claude-3-5-sonnet-20241022", 1_000_000, 0, 0, 0, 0);
        assert!((cost.unwrap() - 3.0).abs() < 1e-6);
    }

    /// The parser is the ingest path for every provider, so it is the right
    /// place to assert the models we actually see in transcripts today are
    /// priced at all — an unpriced model silently becomes `cost_usd: None`.
    #[test]
    fn every_model_seen_in_live_transcripts_is_priced() {
        for model in [
            "claude-fable-5",
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-sonnet-5",
            "claude-sonnet-4-6",
            "claude-sonnet-4-5-20250929",
            "claude-haiku-4-5-20251001",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
            "gpt-5",
            "gpt-5-mini",
            "gemini-3.7-flash",
            "gemini-2.5-pro",
            "gemini-2.5-flash",
        ] {
            assert!(
                estimate_cost_usd(model, 1_000, 1_000, 0, 0, 0).is_some(),
                "{model} has no published rate — burndown will render it as `cost n/a`"
            );
        }
    }
}
