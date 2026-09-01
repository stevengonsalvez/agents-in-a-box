// ABOUTME: ATC (Air Traffic Control) — the persistent orchestrating brain of the
// fleet. A plain `ainb run` Claude session driving a generated `CLAUDE.md` policy,
// woken on an OS-timer heartbeat that is built from the LLM-free `fleet needs`
// read. Poll-mode: no hook/inbox plumbing dependency.
//
// Layers (all pure logic except `timer` which shells out to launchctl/systemctl):
// - `meta`      — `AtcMeta` config record (name + heartbeat knobs), JSON round-trip
// - `paths`     — `~/.agents-in-a-box/atc/<name>/` layout resolution
// - `render`    — pure `CLAUDE.md` policy template renderer
// - `heartbeat` — pure heartbeat-message builder from `fleet needs` JSON,
//                 idle-pause decision, and the ERR auto-continue retry cap
// - `supervisor`— the ONE-owner rule: which mode a fleet is in, which controller
//                 that mode permits to send actions, and which providers can
//                 actually drive full mode
// - `timer`     — launchd plist / systemd timer generation + idempotent install

pub mod heartbeat;
pub mod meta;
pub mod paths;
pub mod render;
pub mod supervisor;
pub mod timer;

pub use heartbeat::{
    DEFAULT_ERR_RETRY_CAP, HeartbeatState, RetryLedger, build_heartbeat,
    build_heartbeat_enforcing_cap, build_heartbeat_with_ledger, should_pause_for_idle,
};
pub use meta::AtcMeta;
pub use paths::{AtcPaths, instance_name_for_cwd_in, sanitize_instance_name};
pub use render::render_claude_md;
pub use supervisor::{
    Controller, DEFAULT_PROVIDER, SupervisorMode, may_act, mode_help, provider_control,
    resolve_full_provider, stand_down_reason, supported_full_providers,
};
