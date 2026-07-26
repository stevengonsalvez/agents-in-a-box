//! Structured acceptance criteria (multica parity gap #11-rest).
//!
//! An issue's definition-of-done is a list of **individually addressable,
//! individually completable** criteria — not a flat list of strings. Each
//! element carries a stable id minted once at write time, the criterion text,
//! and whether it has been ticked off (plus when, and by whom).
//!
//! The list is stored as one JSON document in `issue.acceptance_criteria`
//! (mirroring multica's `JSONB NOT NULL DEFAULT '[]'` column), and travels the
//! wire as `IssueRow.acceptance`.
//!
//! [`criteria_from_json`] is the compatibility seam: it accepts BOTH the legacy
//! flat `["a","b"]` array written by migration 0048 and the structured object
//! array written from 0054 onwards.

use serde::{Deserialize, Serialize};

use crate::idgen::IdGen;

/// One acceptance criterion: a stable id, the criterion text, and whether it
/// has been ticked off.
///
/// Serialised as one element of the issue's `acceptance_criteria` JSON-array
/// column AND as one element of the wire's `acceptance` list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceCriterion {
    /// Stable per-criterion id, minted once at write time (`ac-<token>`).
    ///
    /// NEVER re-derived from position — an agent addresses a criterion by this,
    /// so it must survive any future edit/reorder path.
    pub id: String,
    /// The criterion text (trimmed, non-empty; blank criteria are dropped at
    /// authoring time by [`AcceptanceCriterion::new`]).
    pub text: String,
    /// Whether the criterion is ticked off. `#[serde(default)]` so a legacy
    /// object without the key decodes as unchecked.
    #[serde(default)]
    pub checked: bool,
    /// When it was ticked (epoch millis); `None` while unchecked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<i64>,
    /// Who ticked it, as `"<kind>:<id>"` ([`crate::actor::ActorRef`]'s wire
    /// form); `None` when unchecked or when the caller supplied no actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_by: Option<String>,
}

/// The `ac-` prefix every minted criterion id carries.
pub const CRITERION_ID_PREFIX: &str = "ac-";

impl AcceptanceCriterion {
    /// Mint a fresh, unchecked criterion from caller-supplied text.
    ///
    /// Returns `None` when `text` is blank after trimming — blank criteria are
    /// dropped at authoring time rather than persisted as empty rows.
    pub fn new(id_gen: &dyn IdGen, text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        Some(Self {
            id: format!(
                "{CRITERION_ID_PREFIX}{}",
                id_gen.new_ulid().to_ascii_lowercase()
            ),
            text: text.to_string(),
            checked: false,
            checked_at: None,
            checked_by: None,
        })
    }

    /// Build a criterion with a caller-chosen id (migration backfill / tests).
    ///
    /// Returns `None` for a blank id or blank text.
    pub fn with_id(id: &str, text: &str) -> Option<Self> {
        let (id, text) = (id.trim(), text.trim());
        if id.is_empty() || text.is_empty() {
            return None;
        }
        Some(Self {
            id: id.to_string(),
            text: text.to_string(),
            checked: false,
            checked_at: None,
            checked_by: None,
        })
    }

    /// Tick the criterion off.
    ///
    /// **Idempotent**: an already-checked criterion keeps its original
    /// `checked_at` / `checked_by` so a retrying agent does not rewrite
    /// provenance. Returns `true` when this call changed the state.
    pub fn tick(&mut self, at: i64, by: Option<&str>) -> bool {
        if self.checked {
            return false;
        }
        self.checked = true;
        self.checked_at = Some(at);
        self.checked_by = by.map(str::to_string);
        true
    }

    /// Un-tick the criterion, clearing BOTH `checked_at` and `checked_by` so no
    /// stale attribution survives. Returns `true` when this call changed state.
    pub fn untick(&mut self) -> bool {
        if !self.checked {
            return false;
        }
        self.checked = false;
        self.checked_at = None;
        self.checked_by = None;
        true
    }

    /// The `☑` / `☐` glyph for this criterion's state.
    #[must_use]
    pub const fn glyph(&self) -> &'static str {
        if self.checked { "☑" } else { "☐" }
    }
}

/// Wire/column representation of one element, tolerant of the legacy shape.
///
/// `Full` is declared FIRST because serde's untagged enum tries variants in
/// declaration order — an object must never fall through to the string arm.
#[derive(Deserialize)]
#[serde(untagged)]
enum CriterionRepr {
    Full(AcceptanceCriterion),
    Text(String),
}

/// Decode the `acceptance_criteria` column / wire list, accepting BOTH shapes:
/// a legacy JSON array of bare strings (migration 0048) and the structured
/// object array (migration 0054 onwards).
///
/// A bare string decodes to an unchecked criterion whose id is a **read-time
/// placeholder** derived from its position; the first write through
/// [`criteria_to_json`] after a normalisation pass replaces it with a minted
/// stable id. Blank strings are dropped, matching the authoring rule.
///
/// # Errors
///
/// Returns the underlying `serde_json` error when `raw` is not a JSON array of
/// strings-or-criterion-objects.
pub fn criteria_from_json(raw: &str) -> Result<Vec<AcceptanceCriterion>, serde_json::Error> {
    let reprs: Vec<CriterionRepr> = serde_json::from_str(raw)?;
    Ok(reprs
        .into_iter()
        .enumerate()
        .filter_map(|(idx, repr)| match repr {
            CriterionRepr::Full(c) => Some(c),
            CriterionRepr::Text(text) => {
                AcceptanceCriterion::with_id(&legacy_placeholder_id(idx), &text)
            }
        })
        .collect())
}

/// The read-time placeholder id a legacy bare-string element decodes to.
///
/// Deterministic per position so two reads of an un-normalised row agree; it is
/// replaced by a minted id on the first write.
#[must_use]
pub fn legacy_placeholder_id(index: usize) -> String {
    format!("{CRITERION_ID_PREFIX}legacy-{}", index + 1)
}

/// Whether `id` is a read-time placeholder rather than a minted stable id.
#[must_use]
pub fn is_legacy_placeholder_id(id: &str) -> bool {
    id.starts_with("ac-legacy-")
}

/// Encode criteria for the `acceptance_criteria` column.
///
/// Always a JSON array (`[]` when empty) so the column's `NOT NULL DEFAULT '[]'`
/// invariant holds.
#[must_use]
pub fn criteria_to_json(items: &[AcceptanceCriterion]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".to_string())
}

/// Replace any read-time placeholder ids with freshly minted stable ids.
///
/// The write-time half of the 0054 backfill: a legacy row that is written for
/// any reason converges to the structured shape. Returns `true` when at least
/// one id changed.
pub fn normalise_ids(items: &mut [AcceptanceCriterion], id_gen: &dyn IdGen) -> bool {
    let mut changed = false;
    for item in items.iter_mut() {
        if is_legacy_placeholder_id(&item.id) {
            item.id = format!(
                "{CRITERION_ID_PREFIX}{}",
                id_gen.new_ulid().to_ascii_lowercase()
            );
            changed = true;
        }
    }
    changed
}

/// Resolve a caller-supplied criterion selector to an index in `items`.
///
/// Accepts either the exact criterion `id`, or a **1-based ordinal** (`"2"`).
/// Ordinals exist because an agent reading the detail card sees positions, not
/// ids. Id matching is tried first so an id that happens to look numeric wins.
#[must_use]
pub fn resolve_criterion_index(items: &[AcceptanceCriterion], sel: &str) -> Option<usize> {
    let sel = sel.trim();
    if sel.is_empty() {
        return None;
    }
    if let Some(idx) = items.iter().position(|c| c.id == sel) {
        return Some(idx);
    }
    let ordinal: usize = sel.parse().ok()?;
    if ordinal == 0 || ordinal > items.len() {
        return None;
    }
    Some(ordinal - 1)
}

/// Resolve a caller-supplied selector (id or 1-based ordinal) to a criterion.
#[must_use]
pub fn resolve_criterion<'a>(
    items: &'a [AcceptanceCriterion],
    sel: &str,
) -> Option<&'a AcceptanceCriterion> {
    resolve_criterion_index(items, sel).map(|idx| &items[idx])
}

/// How many of `items` are ticked off (the `checked/total` header numerator).
#[must_use]
pub fn checked_count(items: &[AcceptanceCriterion]) -> usize {
    items.iter().filter(|c| c.checked).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idgen::FixedIdGen;

    fn gen() -> FixedIdGen {
        FixedIdGen::new(vec!["AAA".into(), "BBB".into(), "CCC".into()])
    }

    #[test]
    fn new_trims_and_drops_blank() {
        let g = gen();
        let c = AcceptanceCriterion::new(&g, "  builds green  ").expect("non-blank");
        assert_eq!(c.text, "builds green");
        assert_eq!(c.id, "ac-aaa");
        assert!(!c.checked);
        assert!(AcceptanceCriterion::new(&g, "   ").is_none());
    }

    #[test]
    fn tick_is_idempotent_and_untick_clears_provenance() {
        let g = gen();
        let mut c = AcceptanceCriterion::new(&g, "x").expect("non-blank");
        assert!(c.tick(100, Some("agent:builder")));
        assert_eq!(c.checked_at, Some(100));
        assert!(!c.tick(999, Some("agent:other")), "second tick is a no-op");
        assert_eq!(c.checked_at, Some(100), "provenance not rewritten");
        assert_eq!(c.checked_by.as_deref(), Some("agent:builder"));
        assert!(c.untick());
        assert!(!c.checked);
        assert_eq!(c.checked_at, None);
        assert_eq!(c.checked_by, None);
        assert!(!c.untick(), "second untick is a no-op");
    }

    #[test]
    fn legacy_string_array_decodes_as_unchecked_criteria() {
        let items = criteria_from_json(r#"["a","b"]"#).expect("legacy array decodes");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "a");
        assert_eq!(items[0].id, "ac-legacy-1");
        assert!(!items[0].checked);
        assert_eq!(items[1].id, "ac-legacy-2");
        assert!(is_legacy_placeholder_id(&items[0].id));
    }

    #[test]
    fn structured_array_round_trips_and_object_never_hits_string_arm() {
        let json = r#"[{"id":"ac-1","text":"a","checked":true,"checked_at":7,"checked_by":"agent:x"}]"#;
        let items = criteria_from_json(json).expect("structured array decodes");
        assert_eq!(items[0].id, "ac-1");
        assert!(items[0].checked);
        assert_eq!(items[0].checked_at, Some(7));
        let re = criteria_from_json(&criteria_to_json(&items)).expect("round-trip");
        assert_eq!(re, items);
    }

    #[test]
    fn object_missing_checked_key_decodes_unchecked() {
        let items = criteria_from_json(r#"[{"id":"ac-1","text":"a"}]"#).expect("decodes");
        assert!(!items[0].checked);
    }

    #[test]
    fn empty_and_blank_handling() {
        assert!(criteria_from_json("[]").expect("empty").is_empty());
        assert_eq!(criteria_to_json(&[]), "[]");
        assert!(
            criteria_from_json(r#"["", "  "]"#)
                .expect("blank strings")
                .is_empty()
        );
    }

    #[test]
    fn normalise_ids_replaces_only_placeholders() {
        let mut items = criteria_from_json(r#"["a",{"id":"ac-keep","text":"b"}]"#).expect("mixed");
        let g = FixedIdGen::new(vec!["ZZZ".into()]);
        assert!(normalise_ids(&mut items, &g));
        assert_eq!(items[0].id, "ac-zzz");
        assert_eq!(items[1].id, "ac-keep");
        assert!(!normalise_ids(&mut items, &g), "second pass is a no-op");
    }

    #[test]
    fn resolve_accepts_id_and_one_based_ordinal() {
        let items = criteria_from_json(r#"[{"id":"ac-a","text":"a"},{"id":"ac-b","text":"b"}]"#)
            .expect("decodes");
        assert_eq!(resolve_criterion_index(&items, "ac-b"), Some(1));
        assert_eq!(resolve_criterion_index(&items, "2"), Some(1));
        assert_eq!(resolve_criterion_index(&items, "1"), Some(0));
        assert_eq!(resolve_criterion_index(&items, "0"), None);
        assert_eq!(resolve_criterion_index(&items, "3"), None);
        assert_eq!(resolve_criterion_index(&items, "nope"), None);
        assert_eq!(resolve_criterion_index(&items, ""), None);
        assert_eq!(
            resolve_criterion(&items, "ac-a").map(|c| c.text.as_str()),
            Some("a")
        );
    }

    #[test]
    fn glyph_and_checked_count() {
        let mut items =
            criteria_from_json(r#"[{"id":"ac-a","text":"a"},{"id":"ac-b","text":"b"}]"#)
                .expect("decodes");
        assert_eq!(checked_count(&items), 0);
        assert_eq!(items[0].glyph(), "☐");
        items[1].tick(1, None);
        assert_eq!(checked_count(&items), 1);
        assert_eq!(items[1].glyph(), "☑");
    }

    #[test]
    fn unchecked_criterion_omits_optional_keys() {
        let g = gen();
        let c = AcceptanceCriterion::new(&g, "a").expect("non-blank");
        let json = criteria_to_json(std::slice::from_ref(&c));
        assert!(!json.contains("checked_at"), "got {json}");
        assert!(!json.contains("checked_by"), "got {json}");
        assert!(json.contains("\"checked\":false"), "got {json}");
    }
}
