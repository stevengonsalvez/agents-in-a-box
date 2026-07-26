//! Claim-time squad-leader briefing (multica `squad_briefing.go` parity, gap #7).
//!
//! When the daemon claims a task carrying a `squad_id` AND the claiming agent is
//! that squad's LEADER, it appends this briefing to the run's materialised
//! `CLAUDE.md` so the leader agent runs with squad context (the coordinator role
//! + the roster of members it can delegate to). Member tasks and non-squad tasks
//! get no briefing.
//!
//! # Faithful where it matters, honest where hangar differs
//!
//! Ported from multica's `buildSquadLeaderBriefing`, with two deliberate
//! divergences the current hangar schema forces:
//!
//! - **No @mention dispatch.** Multica's roster carries literal
//!   `[@Name](mention://<type>/<uuid>)` strings that round-trip through its
//!   mention-parse pipeline. Hangar has no such trigger, so the protocol
//!   describes coordination without promising a mention link it cannot honour,
//!   and roster rows carry `name — <agent|human> — <id>` (no mention markdown).
//! - **Roster SKILLS mean what hangar will actually MATERIALISE for that
//!   member, not every attached link.** Multica's `agentSkillsRosterSegment`
//!   reads one live tool registry; hangar has dispatch-time materialisation
//!   governed by TWO levers, and the roster applies BOTH — `agent_skill.enabled`
//!   (migration 0051) and the by-name `agent.disabled_runtime_skills` list — so
//!   the leader never routes work to a capability the member will not have on
//!   disk. Faithful to multica's intent (advertise real capability) even though
//!   multica has only one lever.
//!
//! Per-member `role`, per-member `skills` and `squad.instructions` ARE rendered:
//! a roled member's row carries a `— role: <label>` suffix, a skilled AGENT
//! member's row carries a trailing `— skills: <a>, <b>`, and a non-blank
//! `squad.instructions` becomes the third section `## Squad Instructions`,
//! appended VERBATIM. Each fragment is independently blank-omitted — exactly as
//! multica omits its own when the underlying value is empty:
//!
//! ```text
//! - <name> — agent — <id>[ — role: <role>][ — skills: <a>, <b>]
//! - <email|id> — human — <id>[ — role: <role>]
//! ```

use ainb_hangar_core::actor::ActorKind;
use ainb_hangar_core::ids::{AgentId, WorkspaceId};
use ainb_hangar_store::repo::agent::{Agent, AgentRepo};
use ainb_hangar_store::repo::skill::SkillRepo;
use ainb_hangar_store::repo::squad::{Squad, SquadRepo};
use sqlx::SqlitePool;

/// The hard-coded, not-user-editable operating protocol prepended to every
/// squad-leader briefing.
///
/// Adapted from multica's `squadOperatingProtocolHeader` + `…HardRules`: hangar
/// has no @mention trigger and no `multica squad activity` CLI, so the dispatch
/// mechanism is described as coordination over the roster rather than a mention
/// link, and the two `ownsIssueStatus` responsibility-6 variants are dropped
/// (hangar has no comment/status-ask pipeline to hang them on).
const SQUAD_OPERATING_PROTOCOL: &str = "\
## Squad Operating Protocol

**You have been activated as a squad LEADER for this task.** Your job is to \
**coordinate** the squad, not to silently do all the work yourself. Even if the \
task reads like a direct request to \"do X\", prefer to delegate X to the squad \
member best suited to it — doing everything yourself defeats the purpose of the \
squad.

Your responsibilities, in order:

1. **Read the issue** (title, description, latest comments, acceptance criteria) \
and decide which squad member is best suited to do the work. Pick from the \
members listed in the Squad Roster below, matching the task to each member's \
stated role and skills.
2. **Delegate and sequence the work** across the members in the Squad Roster. \
Split the work so each member owns the part that fits them.
3. **Coordinate, don't re-do.** Track what each member is doing and how the \
pieces fit together; only do the work yourself when no member is suitable.
4. **Stop after coordinating.** Once you have decided the plan and handed work \
to the members, end your turn — you will be re-triggered when a member reports \
back or the issue moves forward.
5. **Re-evaluate on each trigger.** When you wake again, read the new activity \
and decide the next step: delegate the next piece, escalate to the reporter, or \
close the loop.

Hard rules:
- Only involve members that appear in the Squad Roster below — nobody else is \
part of this squad.
- Do NOT restate the issue body or prior discussion when you delegate — every \
member already has the full issue context.
- Do NOT do the implementation work yourself unless the squad has no suitable \
member for it.
";

/// Build the leader briefing for `squad_id` IF `claiming_agent_id` is the
/// squad's leader AGENT; otherwise `None`.
///
/// Sections, in multica's order: the Operating Protocol constant, then the Squad
/// Roster (a leader self-row plus one row per non-archived member, each carrying
/// its free-text role when set), then `## Squad Instructions` — which is omitted
/// ENTIRELY when `squad.instructions` is blank (multica blank-omit parity).
///
/// Returns `None` on:
/// - squad not found for `(workspace, squad_id)` — the dangling-`squad_id` guard
///   (mirrors multica's defensive re-check: a squad row gone → skip silently);
/// - the squad's leader is a human `member` (no agent to brief a runtime with);
/// - `claiming_agent_id` is not the leader (a member task — multica's
///   `daemon.go` leader-task discriminant, and the defensive
///   `squad.LeaderID == resp.Agent.ID` re-check).
///
/// The briefing never crosses the JSON-RPC boundary — the caller appends it to
/// the run CWD's `CLAUDE.md`.
pub async fn build_squad_leader_briefing(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    squad_id: &str,
    claiming_agent_id: &str,
) -> Option<String> {
    let squad = SquadRepo::get(pool, workspace, squad_id).await.ok().flatten()?;
    // Leader-task discriminant + defensive re-check (multica daemon.go:1755):
    // only an AGENT leader that equals the claimer gets briefed.
    if squad.leader.kind() != ActorKind::Agent || squad.leader.id() != claiming_agent_id {
        return None;
    }
    let mut out = String::from(SQUAD_OPERATING_PROTOCOL);
    out.push('\n');
    out.push_str(&render_roster(pool, workspace, &squad).await);
    // Section 3: the user-authored routing guidance, VERBATIM. Blank ⇒ the
    // heading is not emitted at all (multica blank-omit parity, migration 0053).
    let instructions = squad.instructions.trim();
    if !instructions.is_empty() {
        out.push_str("\n## Squad Instructions\n\n");
        out.push_str(instructions);
        out.push('\n');
    }
    Some(out)
}

/// Render the `## Squad Roster` section: the leader self-row followed by one row
/// per non-archived, resolvable member.
///
/// - The leader self-row uses [`AgentRepo::get`] for the name, falling back to
///   `"Leader"` on a lookup miss (multica `buildSquadRoster` parity).
/// - The leader is skipped if it also appears in the member list (no
///   self-delegation row).
/// - An `agent` member that cannot be loaded, or that is archived, is skipped
///   silently (multica `renderMemberRow` parity).
/// - A human `member` is listed (inert in fan-out, but the leader should see it)
///   with its email as the label, falling back to the id — and NEVER carries a
///   skills segment (multica appends skills "for agents"; a human has no skill
///   set to materialise).
/// - The leader SELF-row is deliberately skill-less: the leader is reading its
///   own briefing, so listing its own capabilities is noise, and multica's
///   self-row is the identity line only.
/// - When no member rows survive: `"Members: (none — you are the only member of
///   this squad)"`.
///
/// `workspace` scopes the per-member skill read (the roster must never leak
/// another tenant's skill names).
async fn render_roster(pool: &SqlitePool, workspace: &WorkspaceId, squad: &Squad) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("## Squad Roster\n\n");

    let leader_id = squad.leader.id();
    let leader_name = AgentRepo::get(pool, leader_id)
        .await
        .ok()
        .flatten()
        .map_or_else(|| "Leader".to_string(), |a| a.name);
    out.push_str("Leader (you):\n");
    let _ = writeln!(out, "- {leader_name} — agent — {leader_id}");

    let mut rows: Vec<String> = Vec::with_capacity(squad.members.len());
    for m in &squad.members {
        let actor = &m.actor;
        match actor.kind() {
            ActorKind::Agent => {
                // Skip the leader-as-member dupe (already shown above).
                if actor.id() == leader_id {
                    continue;
                }
                match AgentRepo::get(pool, actor.id()).await {
                    Ok(Some(agent)) if !agent.archived => {
                        let skills = skills_segment(pool, workspace, &agent).await;
                        rows.push(format!(
                            "- {} — agent — {}{}{}\n",
                            agent.name,
                            actor.id(),
                            role_suffix(&m.role),
                            skills
                        ));
                    }
                    // Unresolvable or archived agent → skip silently.
                    _ => {}
                }
            }
            ActorKind::Member => {
                let label = human_label(pool, actor.id()).await;
                rows.push(format!(
                    "- {label} — human — {}{}\n",
                    actor.id(),
                    role_suffix(&m.role)
                ));
            }
        }
    }

    if rows.is_empty() {
        out.push_str("\nMembers: (none — you are the only member of this squad)\n");
        return out;
    }
    out.push_str("\nMembers:\n");
    for r in &rows {
        out.push_str(r);
    }
    out
}

/// The roster row's trailing `— role: <label>` fragment, or `""` when the member
/// has no stated role (migration 0053).
///
/// A blank role renders NOTHING — not an empty `role:` — so a pre-0053 squad's
/// roster is byte-identical to what it was before the column existed.
fn role_suffix(role: &str) -> String {
    let role = role.trim();
    if role.is_empty() {
        String::new()
    } else {
        format!(" — role: {role}")
    }
}

/// The roster row's trailing `— skills: a, b, c` fragment, or `""` when the
/// member will materialise no skills (multica `agentSkillsRosterSegment`
/// parity).
///
/// Applies BOTH suppression levers so the roster advertises only what the member
/// will actually have on disk at dispatch: `agent_skill.enabled = 0` (filtered
/// in the query, migration 0051) and `agent.disabled_runtime_skills` (filtered
/// here, exactly as `materialise::materialise_for_agent` does). Advertising a
/// capability the member will not have is worse than advertising none — the
/// leader would route work that the member then cannot do.
///
/// A read fault degrades to `""`: a briefing must never fail to build over a
/// skill read.
///
/// The names are ADVISORY — they inform the leader's routing, they are never a
/// dispatch filter (`SquadRepo::member_agent_ids` / `assign_fanout` stay
/// skill-blind; selective routing is parity #16).
async fn skills_segment(pool: &SqlitePool, workspace: &WorkspaceId, agent: &Agent) -> String {
    let Ok(agent_id) = AgentId::from_str(agent.id.clone()) else {
        return String::new();
    };
    let names = SkillRepo::enabled_skill_names_for_agent(pool, workspace, &agent_id)
        .await
        .unwrap_or_default();
    let live: Vec<&str> = names
        .iter()
        .map(ainb_hangar_core::skill::SkillName::as_str)
        .filter(|n| !agent.disabled_runtime_skills.iter().any(|d| d == n))
        .collect();
    if live.is_empty() {
        String::new()
    } else {
        format!(" — skills: {}", live.join(", "))
    }
}

/// Resolve a human member's display label (its `user.email`), falling back to the
/// raw id when the user row cannot be read — humans have no agent record, so this
/// is a direct scalar read rather than a repo model.
async fn human_label(pool: &SqlitePool, user_id: &str) -> String {
    sqlx::query_scalar::<_, String>("SELECT email FROM user WHERE id = ?")
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| user_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_core::actor::ActorRef;
    use ainb_hangar_store::{Store, bootstrap};

    fn ws(id: &str) -> WorkspaceId {
        WorkspaceId::from_str(id.to_string()).unwrap()
    }

    /// A leader claiming its own squad task gets a briefing with the protocol,
    /// the roster, its self-row and every non-archived member — and NOT the
    /// archived agent. A member claimer, an unknown squad, and a human-leader
    /// squad all return `None`.
    #[tokio::test]
    async fn builds_leader_briefing_with_roster_skipping_archived() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws_id = bootstrap::ensure_default_workspace(pool).await.unwrap();
        let w = ws(&ws_id);

        // Leader L + agent members A, B, an archived agent C, and a human H.
        let leader =
            bootstrap::create_agent(pool, &ws_id, "captain", "claude", None).await.unwrap();
        let a = bootstrap::create_agent(pool, &ws_id, "scout", "claude", None).await.unwrap();
        let b = bootstrap::create_agent(pool, &ws_id, "medic", "claude", None).await.unwrap();
        let c = bootstrap::create_agent(pool, &ws_id, "ghost", "claude", None).await.unwrap();
        AgentRepo::set_archived(pool, &ws_id, &c.id, true, None, 0).await.unwrap();

        SquadRepo::create(
            pool,
            &w,
            "sq-1",
            "alpha",
            &ActorRef::new(ActorKind::Agent, leader.id.clone()).unwrap(),
            1,
        )
        .await
        .unwrap();
        for m in [&a.id, &b.id, &c.id] {
            SquadRepo::add_member(
                pool,
                &w,
                "sq-1",
                &ActorRef::new(ActorKind::Agent, m.clone()).unwrap(),
            )
            .await
            .unwrap();
        }
        // A human member (email is the label).
        let human = ainb_hangar_store::repo::member::MemberRepo::add(
            pool,
            &w,
            "hank@example.com",
            ainb_hangar_store::repo::member::MemberRole::Member,
        )
        .await
        .unwrap();
        SquadRepo::add_member(
            pool,
            &w,
            "sq-1",
            &ActorRef::new(ActorKind::Member, human.user_id.clone()).unwrap(),
        )
        .await
        .unwrap();

        let briefing = build_squad_leader_briefing(pool, &w, "sq-1", &leader.id)
            .await
            .expect("leader claim gets a briefing");

        assert!(
            briefing.contains("## Squad Operating Protocol"),
            "protocol present"
        );
        assert!(briefing.contains("## Squad Roster"), "roster present");
        assert!(
            briefing.contains("Leader (you):"),
            "leader self-row present"
        );
        assert!(briefing.contains("captain"), "leader name present");
        assert!(briefing.contains("scout"), "member A present");
        assert!(briefing.contains("medic"), "member B present");
        assert!(
            briefing.contains("hank@example.com"),
            "human member present"
        );
        assert!(
            !briefing.contains("ghost"),
            "archived agent C must be skipped"
        );

        // A member claiming (not the leader) gets no briefing.
        assert_eq!(
            build_squad_leader_briefing(pool, &w, "sq-1", &a.id).await,
            None
        );
        // An unknown squad id → None (dangling-id guard).
        assert_eq!(
            build_squad_leader_briefing(pool, &w, "nope", &leader.id).await,
            None
        );
    }

    /// A human-leader squad briefs nobody (no agent runtime to brief).
    #[tokio::test]
    async fn human_leader_squad_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws_id = bootstrap::ensure_default_workspace(pool).await.unwrap();
        let w = ws(&ws_id);
        let human = ainb_hangar_store::repo::member::MemberRepo::add(
            pool,
            &w,
            "boss@example.com",
            ainb_hangar_store::repo::member::MemberRole::Owner,
        )
        .await
        .unwrap();
        SquadRepo::create(
            pool,
            &w,
            "sq-h",
            "humans",
            &ActorRef::new(ActorKind::Member, human.user_id.clone()).unwrap(),
            1,
        )
        .await
        .unwrap();

        assert_eq!(
            build_squad_leader_briefing(pool, &w, "sq-h", &human.user_id).await,
            None,
            "a human leader cannot brief an agent runtime"
        );
    }

    /// Parity #25: a squad with `instructions` and roled members renders the
    /// `## Squad Instructions` section VERBATIM and stamps each roled member's
    /// row with its `— role: <label>` suffix. Asserted against the FULL line, not
    /// a bare substring, so a half-rendered row cannot pass.
    #[tokio::test]
    async fn briefing_carries_instructions_and_member_roles() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws_id = bootstrap::ensure_default_workspace(pool).await.unwrap();
        let w = ws(&ws_id);

        let leader =
            bootstrap::create_agent(pool, &ws_id, "captain", "claude", None).await.unwrap();
        let a = bootstrap::create_agent(pool, &ws_id, "scout", "claude", None).await.unwrap();
        let b = bootstrap::create_agent(pool, &ws_id, "medic", "claude", None).await.unwrap();
        SquadRepo::create(
            pool,
            &w,
            "sq-r",
            "roled",
            &ActorRef::new(ActorKind::Agent, leader.id.clone()).unwrap(),
            1,
        )
        .await
        .unwrap();
        SquadRepo::add_member_with_role(
            pool,
            &w,
            "sq-r",
            &ActorRef::new(ActorKind::Agent, a.id.clone()).unwrap(),
            "owns the migrations",
        )
        .await
        .unwrap();
        // A roleless member on the same squad must render WITHOUT any suffix.
        SquadRepo::add_member(
            pool,
            &w,
            "sq-r",
            &ActorRef::new(ActorKind::Agent, b.id.clone()).unwrap(),
        )
        .await
        .unwrap();
        let instructions =
            "Route schema work to the DB owner.\nEscalate to the reporter on a red CI.";
        SquadRepo::set_instructions(pool, &w, "sq-r", instructions).await.unwrap();

        let briefing = build_squad_leader_briefing(pool, &w, "sq-r", &leader.id).await.unwrap();

        // The roled member's WHOLE line, and the roleless member's whole line.
        assert!(
            briefing.contains(&format!(
                "- scout — agent — {} — role: owns the migrations\n",
                a.id
            )),
            "roled member row must carry the role suffix:\n{briefing}"
        );
        assert!(
            briefing.contains(&format!("- medic — agent — {}\n", b.id)),
            "roleless member row must render unchanged:\n{briefing}"
        );
        // The instructions section, VERBATIM (embedded newline preserved).
        assert!(
            briefing.contains("## Squad Instructions"),
            "instructions section present:\n{briefing}"
        );
        assert!(
            briefing.contains(instructions),
            "instructions rendered verbatim:\n{briefing}"
        );

        // Section ORDER matches multica: Protocol → Roster → Instructions.
        let protocol = briefing.find("## Squad Operating Protocol").unwrap();
        let roster = briefing.find("## Squad Roster").unwrap();
        let instr = briefing.find("## Squad Instructions").unwrap();
        assert!(
            protocol < roster && roster < instr,
            "section order:\n{briefing}"
        );
    }

    /// Parity #25 blank-omit: a squad with blank `instructions` emits NO
    /// `## Squad Instructions` heading at all, and a roleless member's row
    /// carries no `— role:` fragment (byte-identical to the pre-0053 render).
    #[tokio::test]
    async fn blank_instructions_and_roles_render_nothing_extra() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws_id = bootstrap::ensure_default_workspace(pool).await.unwrap();
        let w = ws(&ws_id);

        let leader =
            bootstrap::create_agent(pool, &ws_id, "captain", "claude", None).await.unwrap();
        let a = bootstrap::create_agent(pool, &ws_id, "scout", "claude", None).await.unwrap();
        SquadRepo::create(
            pool,
            &w,
            "sq-b",
            "blank",
            &ActorRef::new(ActorKind::Agent, leader.id.clone()).unwrap(),
            1,
        )
        .await
        .unwrap();
        SquadRepo::add_member(
            pool,
            &w,
            "sq-b",
            &ActorRef::new(ActorKind::Agent, a.id.clone()).unwrap(),
        )
        .await
        .unwrap();

        let briefing = build_squad_leader_briefing(pool, &w, "sq-b", &leader.id).await.unwrap();
        assert!(
            !briefing.contains("## Squad Instructions"),
            "a blank instructions field must not emit the heading:\n{briefing}"
        );
        assert!(
            !briefing.contains("— role:"),
            "a roleless member must not emit a role fragment:\n{briefing}"
        );
        assert!(
            !briefing.contains("— skills:"),
            "a skill-less member must not emit a skills fragment (parity 7-rest \
             blank-omit — the briefing stays byte-identical to the pre-skills \
             render):\n{briefing}"
        );

        // Clearing a previously-set value re-omits the section.
        SquadRepo::set_instructions(pool, &w, "sq-b", "temporary").await.unwrap();
        assert!(
            build_squad_leader_briefing(pool, &w, "sq-b", &leader.id)
                .await
                .unwrap()
                .contains("## Squad Instructions")
        );
        SquadRepo::set_instructions(pool, &w, "sq-b", "").await.unwrap();
        assert!(
            !build_squad_leader_briefing(pool, &w, "sq-b", &leader.id)
                .await
                .unwrap()
                .contains("## Squad Instructions"),
            "cleared instructions re-omit the section"
        );
    }

    /// Parity `7-rest`: an agent member's roster row carries the skills it will
    /// actually MATERIALISE — enabled links only, minus the by-name
    /// `disabled_runtime_skills` suppression — and every fragment (role, skills)
    /// is independently omittable. Human rows and the leader self-row never grow
    /// a skills segment.
    #[tokio::test]
    async fn roster_rows_carry_enabled_skill_names() {
        use ainb_hangar_core::ids::{AgentId, SkillId};
        use ainb_hangar_store::repo::skill::SkillRepo;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws_id = bootstrap::ensure_default_workspace(pool).await.unwrap();
        let w = ws(&ws_id);

        let leader =
            bootstrap::create_agent(pool, &ws_id, "captain", "claude", None).await.unwrap();
        // scout: role AND skills. medic: skills, NO role. sapper: role, NO skills.
        let scout = bootstrap::create_agent(pool, &ws_id, "scout", "claude", None).await.unwrap();
        let medic = bootstrap::create_agent(pool, &ws_id, "medic", "claude", None).await.unwrap();
        let sapper = bootstrap::create_agent(pool, &ws_id, "sapper", "claude", None).await.unwrap();

        // Attach four skills to scout: alpha + gamma stay live, beta is
        // link-disabled (0051), delta is suppressed by name on the agent row.
        let mut ids: Vec<(String, SkillId)> = Vec::new();
        for name in ["alpha", "beta", "gamma", "delta"] {
            let id = SkillRepo::create(pool, &w, name, None, Some("# body"), vec![])
                .await
                .unwrap();
            ids.push((name.to_string(), id));
        }
        let scout_id = AgentId::from_str(scout.id.clone()).unwrap();
        for (_, id) in &ids {
            SkillRepo::attach_to_agent(pool, &w, &scout_id, id).await.unwrap();
        }
        let beta = &ids.iter().find(|(n, _)| n == "beta").unwrap().1;
        SkillRepo::set_enabled(pool, &w, &scout_id, beta, false).await.unwrap();
        AgentRepo::set_disabled_runtime_skills(pool, &scout.id, &["delta".to_string()])
            .await
            .unwrap();

        // medic gets one live skill and no role. The LEADER also gets one, to
        // prove the self-row stays skill-less.
        let medic_id = AgentId::from_str(medic.id.clone()).unwrap();
        let recon = SkillRepo::create(pool, &w, "recon", None, Some("# body"), vec![])
            .await
            .unwrap();
        SkillRepo::attach_to_agent(pool, &w, &medic_id, &recon).await.unwrap();
        let leader_id = AgentId::from_str(leader.id.clone()).unwrap();
        SkillRepo::attach_to_agent(pool, &w, &leader_id, &recon).await.unwrap();

        SquadRepo::create(
            pool,
            &w,
            "sq-s",
            "skilled",
            &ActorRef::new(ActorKind::Agent, leader.id.clone()).unwrap(),
            1,
        )
        .await
        .unwrap();
        SquadRepo::add_member_with_role(
            pool,
            &w,
            "sq-s",
            &ActorRef::new(ActorKind::Agent, scout.id.clone()).unwrap(),
            "owns the migrations",
        )
        .await
        .unwrap();
        SquadRepo::add_member(
            pool,
            &w,
            "sq-s",
            &ActorRef::new(ActorKind::Agent, medic.id.clone()).unwrap(),
        )
        .await
        .unwrap();
        SquadRepo::add_member_with_role(
            pool,
            &w,
            "sq-s",
            &ActorRef::new(ActorKind::Agent, sapper.id.clone()).unwrap(),
            "runs the drills",
        )
        .await
        .unwrap();

        let briefing = build_squad_leader_briefing(pool, &w, "sq-s", &leader.id).await.unwrap();

        // Whole lines, never bare substrings — a half-rendered row must fail.
        assert!(
            briefing.contains(&format!(
                "- scout — agent — {} — role: owns the migrations — skills: alpha, gamma\n",
                scout.id
            )),
            "role THEN skills, name-ordered, enabled only:\n{briefing}"
        );
        assert!(
            briefing.contains(&format!("- medic — agent — {} — skills: recon\n", medic.id)),
            "skills with no role must not emit an empty role fragment:\n{briefing}"
        );
        assert!(
            briefing.contains(&format!(
                "- sapper — agent — {} — role: runs the drills\n",
                sapper.id
            )),
            "a skill-less member emits no skills fragment at all:\n{briefing}"
        );
        assert!(
            !briefing.contains("beta"),
            "a link-disabled skill must never be advertised:\n{briefing}"
        );
        assert!(
            !briefing.contains("delta"),
            "a disabled_runtime_skills name must never be advertised:\n{briefing}"
        );
        // The leader self-row is skill-less even though the leader HAS `recon`.
        assert!(
            briefing.contains(&format!("- captain — agent — {}\n", leader.id)),
            "leader self-row carries identity only:\n{briefing}"
        );
    }

    /// A human `member` row never grows a skills segment — multica appends
    /// skills "for agents", and a human has no skill set to materialise.
    #[tokio::test]
    async fn human_member_row_has_no_skills_segment() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws_id = bootstrap::ensure_default_workspace(pool).await.unwrap();
        let w = ws(&ws_id);
        let leader =
            bootstrap::create_agent(pool, &ws_id, "captain", "claude", None).await.unwrap();
        SquadRepo::create(
            pool,
            &w,
            "sq-h2",
            "mixed",
            &ActorRef::new(ActorKind::Agent, leader.id.clone()).unwrap(),
            1,
        )
        .await
        .unwrap();
        let human = ainb_hangar_store::repo::member::MemberRepo::add(
            pool,
            &w,
            "hank@example.com",
            ainb_hangar_store::repo::member::MemberRole::Member,
        )
        .await
        .unwrap();
        SquadRepo::add_member_with_role(
            pool,
            &w,
            "sq-h2",
            &ActorRef::new(ActorKind::Member, human.user_id.clone()).unwrap(),
            "product owner",
        )
        .await
        .unwrap();

        let briefing = build_squad_leader_briefing(pool, &w, "sq-h2", &leader.id).await.unwrap();
        assert!(
            briefing.contains(&format!(
                "- hank@example.com — human — {} — role: product owner\n",
                human.user_id
            )),
            "human row: role yes, skills never:\n{briefing}"
        );
        assert!(
            !briefing.contains("— skills:"),
            "no skills fragment anywhere in a human-only roster:\n{briefing}"
        );
    }

    /// A solo leader (no other members) renders the lone-member roster line.
    #[tokio::test]
    async fn solo_leader_renders_lone_member_line() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws_id = bootstrap::ensure_default_workspace(pool).await.unwrap();
        let w = ws(&ws_id);
        let leader = bootstrap::create_agent(pool, &ws_id, "solo", "claude", None).await.unwrap();
        SquadRepo::create(
            pool,
            &w,
            "sq-solo",
            "lonely",
            &ActorRef::new(ActorKind::Agent, leader.id.clone()).unwrap(),
            1,
        )
        .await
        .unwrap();

        let briefing = build_squad_leader_briefing(pool, &w, "sq-solo", &leader.id).await.unwrap();
        assert!(
            briefing.contains("(none — you are the only member of this squad)"),
            "lone-member line present"
        );
    }
}
