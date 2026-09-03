// ABOUTME: Single source of truth for per-model token pricing.
//
// This table used to exist as three byte-identical copies (session-reader's
// parser, the host's legacy `models/usage.rs`, and burndown's CLI path). They
// drifted the only way copies can: nobody updated them, and every Opus stayed
// at the retired $15/$75 rate across three model generations, overstating Opus
// spend ~3x. One table, three consumers, one place to edit.

//! Per-model token rates and cost estimation.
//!
//! Rates are US dollars per 1M tokens, list price, Anthropic/OpenAI first-party.
//! A model with no published price returns `None` — callers must treat that as
//! "price unknown", never as zero (see [`estimate_cost_usd`]).

/// Per-*token* rates (not per-million — [`model_rates`] does that division).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelRates {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
}

/// Cost in USD for one call, or `None` when the model has no published price.
///
/// `None` is load-bearing: it flows into `TokenBucket::cost_usd` and every
/// ranking site, which must sink unpriced rows rather than treat them as free
/// or (worse) rank them by raw token count.
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

/// Resolve a model id to its rates.
///
/// **Ordering is load-bearing.** Every arm is a prefix test, so the more
/// specific id must be tested first: `"claude-opus-5"` starts with
/// `"claude-opus"`, and `"gpt-5.6-sol"` starts with `"gpt-5"`. Appending a new
/// model to the bottom of this function is the one way to silently break it —
/// the catch-all above it swallows the match and the new rate never fires.
pub fn model_rates(model: &str) -> Option<ModelRates> {
    let canonical = canonical_model_name(model);
    let m = canonical.as_str();

    let (input_per_million, output_per_million) =
        anthropic_rates(m).or_else(|| openai_rates(m)).or_else(|| google_rates(m))?;

    let input = input_per_million / 1_000_000.0;
    let output = output_per_million / 1_000_000.0;
    Some(ModelRates {
        input,
        output,
        // Anthropic bills cache writes at 1.25x input and cache reads at 0.1x.
        // OpenAI's cached input is also 0.1x list, and it does not bill a
        // separate cache write at all — the 1.25x is harmless there because
        // OpenAI transcripts carry no cache-creation tokens to multiply.
        cache_write: input * 1.25,
        cache_read: input * 0.1,
    })
}

/// Anthropic list prices, most-specific prefix first.
fn anthropic_rates(m: &str) -> Option<(f64, f64)> {
    // Fable / Mythos — the top capability tier, priced above Opus.
    if m.starts_with("claude-fable") || m.starts_with("claude-mythos") {
        return Some((10.0, 50.0));
    }
    // Current Opus generations dropped to $5/$25. Opus 4.5 and older stay at
    // the old $15/$75, so this cannot be one blanket `claude-opus` arm.
    if m.starts_with("claude-opus-5")
        || m.starts_with("claude-opus-4-8")
        || m.starts_with("claude-opus-4-7")
        || m.starts_with("claude-opus-4-6")
    {
        return Some((5.0, 25.0));
    }
    // `claude-3-opus` is the retired Opus 3 id, which does not share the
    // `claude-opus-*` shape.
    if m.starts_with("claude-opus") || m.starts_with("claude-3-opus") {
        return Some((15.0, 75.0));
    }
    if m.starts_with("claude-sonnet") || m.starts_with("claude-3-5-sonnet") {
        // Sonnet 5 carries a $2/$10 introductory rate through 2026-08-31. We
        // bill list deliberately: a date branch here would need a hardcoded
        // cliff that breaks the moment the promo is extended or pulled early,
        // and list self-corrects on that date with no code change.
        return Some((3.0, 15.0));
    }
    if m.starts_with("claude-3-5-haiku") {
        return Some((0.8, 4.0));
    }
    if m.starts_with("claude-haiku") {
        return Some((1.0, 5.0));
    }
    None
}

/// The replacement for a Codex model id that has been retired, or `None` for
/// an id that is still live.
///
/// Lives beside the rate table because both are provider model metadata that
/// more than one crate needs: the TUI/CLI launcher in `ainb-core` and the
/// hangar daemon's dispatch each have to keep a retired id off the wire, and
/// they share no crate other than this one.
///
/// At those launch sites this is the LAST resort, not the first. `ainb-core`
/// prefers the `upgrade` block Codex publishes in its own `models_cache.json`,
/// because the provider owns that data and keeps it current. This table exists
/// for the moments the cache cannot answer: absent on a fresh machine,
/// unreadable, or pruned of the entry once the model is fully gone - exactly
/// when a stale pin most needs rescuing.
///
/// Matching is case-insensitive. The Configure picker lowercases before it
/// asks and the CLI launch path does not, so `--model GPT-5.4` has to resolve
/// the same as `--model gpt-5.4`.
///
/// Keep it small and dated. An entry earns its place only while real on-disk
/// presets and session records still carry the retired id.
pub fn retired_codex_replacement(model: &str) -> Option<&'static str> {
    // Retired 2026-08-31.
    match model.trim().to_lowercase().as_str() {
        "gpt-5.4" => Some("gpt-5.6-terra"),
        // Note this is a price INCREASE, $0.75/$4.50 -> $1.00/$6.00. luna is
        // the closest surviving cheap tier, not a like-for-like swap, so the
        // launch-site warning is what tells a cost-sensitive user it happened.
        "gpt-5.4-mini" => Some("gpt-5.6-luna"),
        _ => None,
    }
}

/// OpenAI list prices, most-specific prefix first.
fn openai_rates(m: &str) -> Option<(f64, f64)> {
    // GPT-5.6 family: three tiers at three price points. Bare `gpt-5.6`
    // aliases to Sol, so it shares Sol's rate.
    if m.starts_with("gpt-5.6-terra") {
        return Some((2.5, 15.0));
    }
    if m.starts_with("gpt-5.6-luna") {
        return Some((1.0, 6.0));
    }
    if m.starts_with("gpt-5.6") {
        return Some((5.0, 30.0));
    }
    if m.starts_with("gpt-5.5-pro") {
        return Some((30.0, 180.0));
    }
    if m.starts_with("gpt-5.5") {
        return Some((5.0, 30.0));
    }
    // The gpt-5.4 family retired 2026-08-31. Its rates STAY: this table prices
    // historical transcripts, and deleting a retired model's rate does not tidy
    // anything up: it turns every past session that used it into `cost n/a`.
    if m.starts_with("gpt-5.4-nano") {
        return Some((0.2, 1.25));
    }
    if m.starts_with("gpt-5.4-mini") {
        return Some((0.75, 4.5));
    }
    if m.starts_with("gpt-5.4-pro") {
        return Some((30.0, 180.0));
    }
    if m.starts_with("gpt-5.4") {
        return Some((2.5, 15.0));
    }
    if m.starts_with("gpt-5-nano") {
        return Some((0.05, 0.4));
    }
    if m.starts_with("gpt-5-mini") {
        return Some((0.25, 2.0));
    }
    if m.starts_with("gpt-5") || m.starts_with("gpt-4.1") || m.starts_with("gpt-4o") {
        return Some((1.25, 10.0));
    }
    None
}

/// Google Gemini list prices, most-specific prefix first.
fn google_rates(m: &str) -> Option<(f64, f64)> {
    if m.starts_with("gemini-3.7-flash") || m.starts_with("3.7-flash") {
        return Some((0.10, 0.40));
    }
    if m.starts_with("gemini-2.5-pro") || m.starts_with("2.5-pro") {
        return Some((1.25, 5.00));
    }
    if m.starts_with("gemini-2.5-flash") || m.starts_with("2.5-flash") {
        return Some((0.075, 0.30));
    }
    if m.starts_with("gemini-") || m.starts_with("gemini") {
        return Some((0.10, 0.40));
    }
    None
}

/// Strip vendor prefixes, `@`-suffixed platform versions, and an 8-digit date
/// snapshot suffix so `claude-3-5-sonnet-20241022` prices like
/// `claude-3-5-sonnet`.
pub fn canonical_model_name(model: &str) -> String {
    let without_prefix = model
        .split('@')
        .next()
        .unwrap_or(model)
        .trim_start_matches("anthropic/")
        .trim_start_matches("openai/")
        .trim_start_matches("google/")
        .to_string();

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

    /// Rate for 1M input tokens, i.e. the published per-million input price.
    fn input_per_million(model: &str) -> f64 {
        estimate_cost_usd(model, 1_000_000, 0, 0, 0, 0).expect("model should be priced")
    }

    fn output_per_million(model: &str) -> f64 {
        estimate_cost_usd(model, 0, 1_000_000, 0, 0, 0).expect("model should be priced")
    }

    #[test]
    fn unknown_model_has_no_price() {
        assert!(estimate_cost_usd("totally-made-up", 100, 100, 0, 0, 0).is_none());
        // Claude Code's placeholder for non-model turns must stay unpriced
        // rather than being guessed at.
        assert!(model_rates("<synthetic>").is_none());
    }

    #[test]
    fn current_opus_generations_are_5_25_not_the_retired_15_75() {
        // The bug this crate exists to stop: every one of these used to be
        // priced at 15/75, inflating reported Opus spend ~3x.
        for model in [
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
        ] {
            assert_eq!(input_per_million(model), 5.0, "{model} input");
            assert_eq!(output_per_million(model), 25.0, "{model} output");
        }
    }

    #[test]
    fn legacy_opus_keeps_the_old_15_75() {
        for model in ["claude-opus-4-5", "claude-opus-4-1", "claude-3-opus"] {
            assert_eq!(input_per_million(model), 15.0, "{model} input");
            assert_eq!(output_per_million(model), 75.0, "{model} output");
        }
    }

    #[test]
    fn fable_and_mythos_are_priced_above_opus() {
        for model in ["claude-fable-5", "claude-mythos-5"] {
            assert_eq!(input_per_million(model), 10.0, "{model} input");
            assert_eq!(output_per_million(model), 50.0, "{model} output");
        }
    }

    #[test]
    fn sonnet_is_3_15_and_haiku_4_5_is_1_5() {
        assert_eq!(input_per_million("claude-sonnet-5"), 3.0);
        assert_eq!(input_per_million("claude-sonnet-4-6"), 3.0);
        assert_eq!(input_per_million("claude-haiku-4-5"), 1.0);
        assert_eq!(output_per_million("claude-haiku-4-5"), 5.0);
        // Haiku 3.5 was cheaper than 4.5 — the legacy arm must survive.
        assert_eq!(input_per_million("claude-3-5-haiku"), 0.8);
    }

    #[test]
    fn gpt_5_6_tiers_are_distinct() {
        assert_eq!(input_per_million("gpt-5.6-sol"), 5.0);
        assert_eq!(output_per_million("gpt-5.6-sol"), 30.0);
        assert_eq!(input_per_million("gpt-5.6-terra"), 2.5);
        assert_eq!(output_per_million("gpt-5.6-terra"), 15.0);
        assert_eq!(input_per_million("gpt-5.6-luna"), 1.0);
        assert_eq!(output_per_million("gpt-5.6-luna"), 6.0);
        // Bare `gpt-5.6` aliases to Sol.
        assert_eq!(input_per_million("gpt-5.6"), 5.0);
    }

    #[test]
    fn specific_prefixes_win_over_general_ones() {
        // The ordering trap: every one of these also matches a shorter prefix
        // that appears later in the function. If someone appends a new arm to
        // the bottom instead of ordering it, this test fires.
        assert_ne!(
            input_per_million("gpt-5.6-terra"),
            input_per_million("gpt-5.6-sol")
        );
        assert_ne!(
            input_per_million("gpt-5.4-mini"),
            input_per_million("gpt-5.4")
        );
        assert_ne!(input_per_million("gpt-5-mini"), input_per_million("gpt-5"));
        assert_ne!(
            input_per_million("claude-opus-5"),
            input_per_million("claude-opus-4-5")
        );
    }

    #[test]
    fn dated_snapshot_suffix_is_stripped() {
        assert_eq!(input_per_million("claude-3-5-sonnet-20241022"), 3.0);
        assert_eq!(input_per_million("claude-haiku-4-5-20251001"), 1.0);
        assert_eq!(input_per_million("claude-sonnet-4-5-20250929"), 3.0);
    }

    #[test]
    fn vendor_prefix_and_platform_version_are_stripped() {
        assert_eq!(input_per_million("anthropic/claude-opus-5"), 5.0);
        assert_eq!(input_per_million("openai/gpt-5.6-sol"), 5.0);
        assert_eq!(input_per_million("google/gemini-3.7-flash"), 0.10);
        assert_eq!(input_per_million("claude-opus-4-5@20251101"), 15.0);
    }

    #[test]
    fn gemini_rates_are_priced_correctly() {
        assert_eq!(input_per_million("gemini-3.7-flash"), 0.10);
        assert_eq!(output_per_million("gemini-3.7-flash"), 0.40);
        assert_eq!(input_per_million("3.7-flash"), 0.10);
        assert_eq!(output_per_million("3.7-flash"), 0.40);

        assert_eq!(input_per_million("gemini-2.5-pro"), 1.25);
        assert_eq!(output_per_million("gemini-2.5-pro"), 5.00);
        assert_eq!(input_per_million("2.5-pro"), 1.25);
        assert_eq!(output_per_million("2.5-pro"), 5.00);

        assert_eq!(input_per_million("gemini-2.5-flash"), 0.075);
        assert_eq!(output_per_million("gemini-2.5-flash"), 0.30);
        assert_eq!(input_per_million("2.5-flash"), 0.075);
        assert_eq!(output_per_million("2.5-flash"), 0.30);

        assert_eq!(input_per_million("gemini-pro"), 0.10);
        assert_eq!(output_per_million("gemini-pro"), 0.40);
    }

    #[test]
    fn cache_write_is_1_25x_input_and_cache_read_is_0_1x() {
        let rates = model_rates("claude-opus-5").expect("priced");
        assert!((rates.cache_write - rates.input * 1.25).abs() < f64::EPSILON);
        assert!((rates.cache_read - rates.input * 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn reasoning_tokens_bill_at_the_output_rate() {
        let with_reasoning = estimate_cost_usd("gpt-5.6-sol", 0, 0, 0, 0, 1_000_000);
        assert_eq!(with_reasoning, Some(30.0));
    }

    /// The table the Configure row, the CLI launcher and the daemon dispatch
    /// all share. A live id must return `None` so the provider keeps owning
    /// its own catalogue.
    #[test]
    fn only_retired_ids_have_a_replacement() {
        assert_eq!(retired_codex_replacement("gpt-5.4"), Some("gpt-5.6-terra"));
        assert_eq!(
            retired_codex_replacement("gpt-5.4-mini"),
            Some("gpt-5.6-luna")
        );
        assert_eq!(retired_codex_replacement("gpt-5.6-terra"), None);
        assert_eq!(retired_codex_replacement("gpt-5.5"), None);
        assert_eq!(retired_codex_replacement("anything-else"), None);
    }

    /// `ainb run --model GPT-5.4` reaches the table with the case the user
    /// typed, while the Configure picker lowercases first. Both must resolve.
    #[test]
    fn retired_ids_match_regardless_of_case_and_padding() {
        assert_eq!(retired_codex_replacement("GPT-5.4"), Some("gpt-5.6-terra"));
        assert_eq!(
            retired_codex_replacement("  Gpt-5.4-Mini  "),
            Some("gpt-5.6-luna")
        );
    }

    /// The gpt-5.4 family retired 2026-08-31 but its RATES stay: this table
    /// prices historical transcripts, and dropping them turns every past
    /// session that used the model into `cost n/a`.
    #[test]
    fn retired_models_keep_their_historical_rates() {
        assert_eq!(input_per_million("gpt-5.4"), 2.5);
        assert_eq!(input_per_million("gpt-5.4-mini"), 0.75);
    }
}
