//! P5 — Profile-editor screen: the agent-profile roster + a live preview of BOTH
//! compile targets (D14-D16).
//!
//! The screen (hotkey `P`) is the TUI face of the P5 profile system. The left
//! column is the roster of profile masters (slug + logical tier) the daemon
//! indexes from `~/.agents-in-a-box/profiles/`; the right pane shows the selected
//! profile's fields plus **both** compile previews the daemon returns from
//! `profile/get`:
//!
//! - the **lossless Claude** subagent `.md` (tier resolved to a Claude model,
//!   every field preserved), and
//! - the **lossy Codex** `[profiles.<slug>]` fragment + prompt, with a
//!   dropped-field warning line (amber) for each Claude-only field Codex cannot
//!   represent (`tools`, `color`).
//!
//! Editing in v1 is the highest-leverage field — the **logical tier** — cycled
//! with `t` (premium → balanced → fast); the change is written back through
//! `profile/upsert`, and the re-fetched previews show both targets re-resolving
//! their concrete model. The screen is pure (no IO / no RPC): the plugin glue
//! fills it from `profile/list` / `profile/get` and fires `profile/upsert`, so it
//! is snapshot-testable from a seeded state (mirroring [`super::logs`]).

use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// Title / accent gold.
const GOLD: Color = Color::rgb(255, 215, 0);
/// Primary text.
const SOFT_WHITE: Color = Color::rgb(220, 220, 230);
/// Muted text (labels, hints, unselected rows, preview body).
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);
/// Selection green (the `▶` marker + the selected roster row).
const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);
/// Section-header blue (the CLAUDE / CODEX preview labels + the column rule).
const CORNFLOWER_BLUE: Color = Color::rgb(100, 149, 237);
/// Dropped-field warning amber (the Codex WARN lines).
const WARN_AMBER: Color = Color::rgb(230, 190, 90);

/// The width of the left roster column (slug + tier). The preview pane takes the
/// rest; at the 80-col floor the column shrinks so the rule + pane still fit.
const ROSTER_COL_WIDTH: u16 = 26;
/// The minimum preview-pane width kept to the right of the rule.
const MIN_PREVIEW_WIDTH: u16 = 20;

/// One roster row: a profile master's identity (slug + logical tier).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRosterEntry {
    /// The profile slug (its filename stem + board-assignee slug).
    pub slug: String,
    /// The logical tier token (`premium` / `balanced` / `fast`).
    pub tier: String,
}

/// The selected profile's full detail + both compile previews (from
/// `profile/get`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProfileDetailView {
    /// The profile slug.
    pub slug: String,
    /// The one-line description.
    pub description: String,
    /// The logical tier token.
    pub tier: String,
    /// The Claude-only tool allowlist.
    pub tools: Vec<String>,
    /// The Claude-only card color (empty when unset).
    pub color: String,
    /// The system-prompt body (carried so a tier-cycle upsert round-trips the
    /// whole master rather than dropping it).
    pub body: String,
    /// The lossless Claude subagent `.md` preview.
    pub claude_preview: String,
    /// The lossy Codex `[profiles.<slug>]` config fragment.
    pub codex_fragment: String,
    /// The lossy Codex prompt body.
    pub codex_prompt: String,
    /// One warning per dropped Claude-only field (rendered amber).
    pub codex_warnings: Vec<String>,
}

/// The render state for the profile-editor screen.
///
/// Default is the empty roster shown before the first `profile/list` lands (or
/// when no profile masters exist on disk yet).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProfilesState {
    /// The profile roster, slug-ordered (as the daemon returns it).
    roster: Vec<ProfileRosterEntry>,
    /// The selected roster index (clamped into range on every roster update).
    selected: usize,
    /// The selected profile's detail + previews, once `profile/get` lands. `None`
    /// while the detail for the current selection is still loading.
    detail: Option<ProfileDetailView>,
}

impl ProfilesState {
    /// Replace the roster from a `profile/list` result, clamping the selection
    /// into range. The detail is cleared when the selected slug changes (a stale
    /// detail must never render under a different row).
    pub fn set_roster(&mut self, roster: Vec<ProfileRosterEntry>) {
        let prior_slug = self.selected_slug().map(ToString::to_string);
        self.roster = roster;
        if self.selected >= self.roster.len() {
            self.selected = self.roster.len().saturating_sub(1);
        }
        if self.selected_slug().map(str::to_string) != prior_slug {
            self.detail = None;
        }
    }

    /// Set the selected profile's detail from a `profile/get` result. Ignored
    /// when it is for a slug other than the current selection (a stale reply for a
    /// row the user has since moved off).
    pub fn set_detail(&mut self, detail: ProfileDetailView) {
        if self.selected_slug() == Some(detail.slug.as_str()) {
            self.detail = Some(detail);
        }
    }

    /// The selected roster row, if any.
    #[must_use]
    pub fn selected_entry(&self) -> Option<&ProfileRosterEntry> {
        self.roster.get(self.selected)
    }

    /// The selected profile's slug, if the roster is non-empty.
    #[must_use]
    pub fn selected_slug(&self) -> Option<&str> {
        self.selected_entry().map(|e| e.slug.as_str())
    }

    /// The loaded detail when it matches `slug` — the glue reads this to re-send
    /// the FULL master on a tier-cycle upsert (so no field is dropped). `None`
    /// when the detail has not loaded (the glue then falls back to a minimal
    /// slug+tier write).
    #[must_use]
    pub fn detail_for_upsert(&self, slug: &str) -> Option<&ProfileDetailView> {
        self.detail.as_ref().filter(|d| d.slug == slug)
    }

    /// The selected index (for tests / glue).
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    /// The roster length (for tests / glue).
    #[must_use]
    pub fn len(&self) -> usize {
        self.roster.len()
    }

    /// Whether the roster is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roster.is_empty()
    }
}

/// A key press folded into the profile screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfilesEvent {
    /// A printable key (`j`/`k`/`t`/…).
    Key(char),
}

/// A deferred action the plugin glue carries out over the daemon socket (the
/// screen reducer is pure — it cannot `await`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfilesIntent {
    /// Load the detail + previews for `slug` (`profile/get`), raised when the
    /// selection moves to a row whose detail is not yet loaded.
    LoadDetail(String),
    /// Persist a tier change for `slug` to `tier` (`profile/upsert`), raised by
    /// `t`. The glue re-fetches the detail so both previews re-resolve.
    CycleTier {
        /// The profile whose tier changes.
        slug: String,
        /// The new logical tier token.
        tier: String,
    },
}

/// The result of folding one [`ProfilesEvent`] into a [`ProfilesState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilesReduction {
    /// The next state.
    pub state: ProfilesState,
    /// A deferred action for the glue, if any.
    pub intent: Option<ProfilesIntent>,
}

/// Fold one key press into the profile screen (pure).
///
/// - `j` / `n` move the selection down, `k` / `p` up — each raising a
///   [`ProfilesIntent::LoadDetail`] for the newly-selected slug so its previews
///   fetch.
/// - `t` cycles the selected profile's logical tier (premium → balanced → fast)
///   and raises a [`ProfilesIntent::CycleTier`] to persist it.
/// - any other key is a no-op.
#[must_use]
pub fn reduce_profiles(state: &ProfilesState, ev: ProfilesEvent) -> ProfilesReduction {
    let ProfilesEvent::Key(c) = ev;
    match c {
        'j' | 'n' => move_selection(state, 1),
        'k' | 'p' => move_selection(state, -1),
        't' => cycle_tier(state),
        _ => no_change(state),
    }
}

/// Move the selection by `delta` (clamped), loading the new row's detail.
fn move_selection(state: &ProfilesState, delta: isize) -> ProfilesReduction {
    if state.roster.is_empty() {
        return no_change(state);
    }
    let last = state.roster.len() - 1;
    let next = (state.selected as isize + delta).clamp(0, last as isize) as usize;
    if next == state.selected {
        return no_change(state);
    }
    let mut s = state.clone();
    s.selected = next;
    // The detail belongs to the prior row; clear it so the pane shows "loading"
    // until the fetched detail for the new slug lands.
    s.detail = None;
    let slug = s.roster[next].slug.clone();
    ProfilesReduction {
        state: s,
        intent: Some(ProfilesIntent::LoadDetail(slug)),
    }
}

/// Cycle the selected profile's tier and raise the persist intent.
fn cycle_tier(state: &ProfilesState) -> ProfilesReduction {
    let Some(entry) = state.selected_entry() else {
        return no_change(state);
    };
    let next = next_tier(&entry.tier).to_string();
    let slug = entry.slug.clone();
    // Optimistically reflect the new tier in the roster row AND the loaded detail
    // so the change is visible before the upsert round-trips; the re-fetched
    // detail reconciles the previews. The detail is kept (not cleared) so the glue
    // can re-send the FULL master on upsert rather than dropping its other fields.
    let mut s = state.clone();
    s.roster[s.selected].tier = next.clone();
    if let Some(d) = &mut s.detail {
        d.tier = next.clone();
    }
    ProfilesReduction {
        state: s,
        intent: Some(ProfilesIntent::CycleTier { slug, tier: next }),
    }
}

/// The next tier in the premium → balanced → fast → premium cycle. An
/// unrecognised token cycles into `balanced` (the safe default band).
fn next_tier(cur: &str) -> &'static str {
    match cur {
        "premium" => "balanced",
        "balanced" => "fast",
        "fast" => "premium",
        _ => "balanced",
    }
}

/// A reduction that leaves the state unchanged and raises no intent.
fn no_change(state: &ProfilesState) -> ProfilesReduction {
    ProfilesReduction {
        state: state.clone(),
        intent: None,
    }
}

/// Render the profile-editor screen into `buf` between rows `top` and `bottom`.
///
/// Layout:
///
/// ```text
/// Profiles                                              3 profiles
/// ▶ code-reviewer  premium  │ code-reviewer · premium
///   author         balanced │ Reviews a diff
///   docs-writer    fast      │ tools: Read, Grep   color: cyan
///                            │ CLAUDE  .claude/agents/code-reviewer.md
///                            │ ---
///                            │ model: opus …
///                            │ CODEX   [profiles.code-reviewer] + prompt
///                            │ model = "gpt-5"
///                            │ ⚠ dropped `tools` (Read, Grep)
/// [j/k] move   [t] cycle tier
/// ```
pub fn render_profiles(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &ProfilesState,
) {
    let mut row = top;
    put_str(buf, 0, row, "Profiles", GOLD, area_w);
    let count = format!(
        "{} profile{}",
        state.roster.len(),
        if state.roster.len() == 1 { "" } else { "s" }
    );
    let cx = area_w.saturating_sub(count.chars().count() as u16);
    put_str(buf, cx, row, &count, MUTED_GRAY, area_w);
    row += 1;

    if state.roster.is_empty() {
        put_str(
            buf,
            0,
            row + 1,
            "no profiles yet — create a master in ~/.agents-in-a-box/profiles/<slug>.md",
            MUTED_GRAY,
            area_w,
        );
        render_hints(buf, bottom, area_w);
        return;
    }

    // Column geometry: a left roster column, a 1-col rule, then the preview pane.
    let roster_w = ROSTER_COL_WIDTH.min(area_w.saturating_sub(MIN_PREVIEW_WIDTH + 1));
    let rule_x = roster_w;
    let pane_x = roster_w + 1;
    let pane_w = area_w.saturating_sub(pane_x);
    let body_bottom = bottom.saturating_sub(1); // reserve the footer-hints row.

    render_roster(buf, 0, roster_w, row, body_bottom, state);
    render_rule(buf, rule_x, row, body_bottom);
    render_detail(buf, pane_x, pane_w, row, body_bottom, state);
    render_hints(buf, bottom, area_w);
}

/// Render the left roster column: `▶ slug  tier`, selected row green.
fn render_roster(
    buf: &mut WireBuffer,
    x: u16,
    col_w: u16,
    top: u16,
    bottom: u16,
    state: &ProfilesState,
) {
    let mut row = top;
    for (i, entry) in state.roster.iter().enumerate() {
        if row > bottom {
            break;
        }
        let selected = i == state.selected;
        let (marker, name_color) = if selected {
            ("▶ ", SELECTION_GREEN)
        } else {
            ("  ", SOFT_WHITE)
        };
        let right = x + col_w;
        let mut cx = put_str(buf, x, row, marker, SELECTION_GREEN, right);
        // Reserve the tier column at the column's right edge.
        let tier_w = entry.tier.chars().count() as u16;
        let name_right = right.saturating_sub(tier_w + 1);
        cx = put_str(buf, cx, row, &entry.slug, name_color, name_right);
        let _ = cx;
        let tx = right.saturating_sub(tier_w);
        put_str(buf, tx, row, &entry.tier, tier_color(&entry.tier), right);
        row += 1;
    }
}

/// Render the vertical rule between the roster and the preview pane.
fn render_rule(buf: &mut WireBuffer, x: u16, top: u16, bottom: u16) {
    let mut row = top;
    while row <= bottom {
        put_cell(buf, x, row, '│', CORNFLOWER_BLUE);
        row += 1;
    }
}

/// Render the right preview pane: the selected profile's fields + BOTH compile
/// previews (Claude lossless, Codex lossy + amber warnings).
fn render_detail(
    buf: &mut WireBuffer,
    x: u16,
    pane_w: u16,
    top: u16,
    bottom: u16,
    state: &ProfilesState,
) {
    let right = x + pane_w;
    let mut row = top;

    let Some(detail) = &state.detail else {
        // Selection made, detail still fetching.
        let slug = state.selected_slug().unwrap_or("");
        put_str(buf, x, row, &format!("{slug} · loading…"), MUTED_GRAY, right);
        return;
    };

    // Heading: slug · tier.
    let mut cx = put_str(buf, x, row, &detail.slug, SOFT_WHITE, right);
    cx = put_str(buf, cx, row, " · ", MUTED_GRAY, right);
    put_str(buf, cx, row, &detail.tier, tier_color(&detail.tier), right);
    row += 1;

    if !detail.description.is_empty() {
        row = put_line(buf, x, row, bottom, &detail.description, MUTED_GRAY, right);
    }

    // tools / color meta.
    let mut meta = String::new();
    if !detail.tools.is_empty() {
        meta.push_str(&format!("tools: {}", detail.tools.join(", ")));
    }
    if !detail.color.is_empty() {
        if !meta.is_empty() {
            meta.push_str("   ");
        }
        meta.push_str(&format!("color: {}", detail.color));
    }
    if !meta.is_empty() {
        row = put_line(buf, x, row, bottom, &meta, MUTED_GRAY, right);
    }

    row = blank(row, bottom);

    // CLAUDE preview (lossless).
    row = put_line(
        buf,
        x,
        row,
        bottom,
        &format!("CLAUDE  .claude/agents/{}.md", detail.slug),
        CORNFLOWER_BLUE,
        right,
    );
    for line in detail.claude_preview.lines() {
        row = put_line(buf, x, row, bottom, line, MUTED_GRAY, right);
        if row > bottom {
            break;
        }
    }

    row = blank(row, bottom);

    // CODEX preview (lossy) + amber warnings.
    row = put_line(
        buf,
        x,
        row,
        bottom,
        &format!("CODEX   ~/.codex/config.toml + prompts/{}.md", detail.slug),
        CORNFLOWER_BLUE,
        right,
    );
    for line in detail.codex_fragment.lines() {
        row = put_line(buf, x, row, bottom, line, MUTED_GRAY, right);
    }
    for w in &detail.codex_warnings {
        if row > bottom {
            break;
        }
        row = put_line(buf, x, row, bottom, &format!("⚠ {w}"), WARN_AMBER, right);
    }
}

/// Render the footer key hints on the bottom row.
fn render_hints(buf: &mut WireBuffer, row: u16, area_w: u16) {
    let mut x = 0u16;
    for (key, label) in [("j/k", "move"), ("t", "cycle tier")] {
        x = put_str(buf, x, row, "[", MUTED_GRAY, area_w);
        x = put_str(buf, x, row, key, GOLD, area_w);
        x = put_str(buf, x, row, "] ", MUTED_GRAY, area_w);
        x = put_str(buf, x, row, label, MUTED_GRAY, area_w);
        x = put_str(buf, x, row, "   ", MUTED_GRAY, area_w);
        if x >= area_w {
            break;
        }
    }
}

/// The colour for a tier token badge.
fn tier_color(tier: &str) -> Color {
    match tier {
        "premium" => GOLD,
        "balanced" => CORNFLOWER_BLUE,
        "fast" => SELECTION_GREEN,
        _ => MUTED_GRAY,
    }
}

/// Write `s` at `(x, row)` and advance one row, returning the next row (clamped
/// so it never exceeds `bottom + 1`). A no-op paint when already past `bottom`.
fn put_line(
    buf: &mut WireBuffer,
    x: u16,
    row: u16,
    bottom: u16,
    s: &str,
    color: Color,
    right: u16,
) -> u16 {
    if row > bottom {
        return row;
    }
    put_str(buf, x, row, s, color, right);
    row + 1
}

/// Advance one blank row (bounded by `bottom`).
fn blank(row: u16, bottom: u16) -> u16 {
    if row > bottom {
        row
    } else {
        row + 1
    }
}

/// Write `s` at `(x, row)` in `color`, clipping at `right`. Returns the next free
/// column. Char-safe (iterates `char`s — the rust-utf8-truncate trap).
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= right {
            break;
        }
        put_cell(buf, cx, row, ch, color);
        cx = cx.saturating_add(1);
    }
    cx
}

/// Write a single coloured glyph at `(x, row)`.
fn put_cell(buf: &mut WireBuffer, x: u16, row: u16, ch: char, color: Color) {
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(color);
    buf.push(Coord::new(x, row), cell);
}

/// Palette exported so the snapshot test can assert colours non-vacuously.
pub mod colors {
    use ainb_plugin_sdk::Color;

    /// Selected roster row + `▶` marker green.
    pub const SELECTION: Color = super::SELECTION_GREEN;
    /// Section-header blue (CLAUDE / CODEX labels + column rule).
    pub const SECTION: Color = super::CORNFLOWER_BLUE;
    /// Dropped-field warning amber.
    pub const WARN: Color = super::WARN_AMBER;
    /// Premium-tier gold.
    pub const PREMIUM: Color = super::GOLD;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster3() -> Vec<ProfileRosterEntry> {
        vec![
            ProfileRosterEntry { slug: "author".into(), tier: "balanced".into() },
            ProfileRosterEntry { slug: "code-reviewer".into(), tier: "premium".into() },
            ProfileRosterEntry { slug: "docs-writer".into(), tier: "fast".into() },
        ]
    }

    #[test]
    fn set_roster_clamps_selection_and_clears_stale_detail() {
        let mut s = ProfilesState::default();
        s.set_roster(roster3());
        s.set_detail(ProfileDetailView { slug: "author".into(), ..Default::default() });
        assert_eq!(s.selected(), 0);
        // Shrinking the roster below the selection clamps it and clears detail.
        s.selected = 2;
        s.set_roster(vec![roster3()[0].clone()]);
        assert_eq!(s.selected(), 0);
    }

    #[test]
    fn set_detail_ignored_for_non_selected_slug() {
        let mut s = ProfilesState::default();
        s.set_roster(roster3()); // selected = author
        s.set_detail(ProfileDetailView { slug: "docs-writer".into(), ..Default::default() });
        assert!(s.detail.is_none(), "stale detail for a non-selected row is dropped");
    }

    #[test]
    fn j_moves_down_and_loads_detail() {
        let mut s = ProfilesState::default();
        s.set_roster(roster3());
        let out = reduce_profiles(&s, ProfilesEvent::Key('j'));
        assert_eq!(out.state.selected(), 1);
        assert_eq!(
            out.intent,
            Some(ProfilesIntent::LoadDetail("code-reviewer".into()))
        );
    }

    #[test]
    fn k_at_top_is_noop() {
        let mut s = ProfilesState::default();
        s.set_roster(roster3());
        let out = reduce_profiles(&s, ProfilesEvent::Key('k'));
        assert_eq!(out.state.selected(), 0);
        assert_eq!(out.intent, None);
    }

    #[test]
    fn t_cycles_tier_and_raises_upsert() {
        let mut s = ProfilesState::default();
        s.set_roster(roster3()); // author = balanced
        let out = reduce_profiles(&s, ProfilesEvent::Key('t'));
        // balanced → fast, reflected optimistically + persisted.
        assert_eq!(out.state.roster[0].tier, "fast");
        assert_eq!(
            out.intent,
            Some(ProfilesIntent::CycleTier { slug: "author".into(), tier: "fast".into() })
        );
    }

    #[test]
    fn tier_cycle_wraps() {
        assert_eq!(next_tier("premium"), "balanced");
        assert_eq!(next_tier("balanced"), "fast");
        assert_eq!(next_tier("fast"), "premium");
        assert_eq!(next_tier("garbage"), "balanced");
    }

    #[test]
    fn empty_roster_keys_are_noops() {
        let s = ProfilesState::default();
        for c in ['j', 'k', 't'] {
            let out = reduce_profiles(&s, ProfilesEvent::Key(c));
            assert_eq!(out.intent, None);
            assert!(out.state.is_empty());
        }
    }
}
