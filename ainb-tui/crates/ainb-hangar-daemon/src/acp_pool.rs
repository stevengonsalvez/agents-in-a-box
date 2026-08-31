//! The daemon-owned ACP agent pool: MULTIPLEXED, one adapter process per
//! PROVIDER hosting many sessions (graft 6, decided 2026-08-04).
//!
//! ```text
//!  fleet/message_send ─┐
//!  fleet/action        │   ┌──────────── AcpPool ─────────────┐
//!                      └──▶│ sessions: session_key ─▶ actor   │
//!                          │ providers: token ─▶ process      │
//!                          └───┬──────────────────────┬───────┘
//!             per-session task │                      │ one per provider
//!         ┌────────────────────▼──────┐      ┌────────▼─────────┐
//!         │ reducer ─▶ StoreWriter    │◀─────│ demux by         │
//!         │ ─▶ transcript_tx wakeup   │      │ acp sessionId    │
//!         │ bounded FIFO, 1 in flight │      │ + SlotCircuit    │
//!         └───────────────────────────┘      └──────────────────┘
//! ```
//!
//! Five properties are load-bearing and each has a test:
//!
//! * **Demux is exact.** Every `session/update` is routed by its own
//!   `sessionId`; a chunk whose id no session claims is logged and DROPPED, never
//!   attributed to a neighbour. On a shared process, cross-attribution would put
//!   one tenant's output in another tenant's transcript.
//! * **At most once (I6).** A prompt is requeued ONLY when it provably never
//!   reached the adapter, which here means the failure happened before
//!   `session/prompt` was issued at all (spawn refused, session/new failed, or
//!   the transport was already closed at [`AdapterProcess::is_alive`]). After the
//!   request is issued the outcome is turn end or UNKNOWN, never a blind resend.
//! * **Convergence is not boot-only (I16).** An adapter that exits, or a turn
//!   that outlives its deadline, runs [`converge_dirty_session`] — the SAME
//!   function the boot scan runs. Under the multiplex it fans out to every
//!   session the dead process hosted. Convergence can only reach turns the
//!   STORE knows about, so a turn whose `open_turn_id` will not persist is
//!   never issued ([`DELIVERY_TURN_UNRECORDED`]).
//! * **A turn ends in ONE transaction.** The receipt, the agent's reply and the
//!   released session commit together, and the receipt gates the reply
//!   ([`FleetAcpSessionRepo::commit_turn_end`]), so no crash publishes an
//!   answer the receipt says never landed. The transcript's completion marker
//!   is committed first, on purpose: the only crash window it leaves is a turn
//!   the boot scan still lists as dirty.
//! * **Work is bounded; notifications are not.** The per-scope prompt queue is
//!   a bounded channel (a full queue is a REJECTED delivery carrying
//!   [`DELIVERY_QUEUE_FULL`], never silent growth), the per-process in-flight
//!   count is a semaphore, and the session count per provider is capped with LRU
//!   idle eviction. The ONE deliberate exception is the `session/update` and
//!   `session/request_permission` demux channels, which are unbounded: dropping
//!   a transcript notification is data loss and blocking the shared connection
//!   task would stall every OTHER tenant on the same process. The ceiling there
//!   is therefore observability, not backpressure: `transcript_bytes` on
//!   `hangar/daemon_health` reports what each session has committed so a chatty
//!   adapter against a contended writer is visible before it is a memory
//!   incident.
//! * **A queued prompt always has an outcome.** Convergence DRAINS the queue and
//!   resolves every drained job terminal (I16), and `start_turn` re-reads the
//!   leg before issuing, so a prompt whose delivery is already terminal is never
//!   sent.
//! * **A blocked permission is answerable.** `session/request_permission` parks
//!   its responder here and raises an attention row; the answer arrives through
//!   `fleet/action` and reaches the adapter's pending JSON-RPC id. A permission
//!   whose adapter dies is retired the moment its turn ends, and by convergence
//!   when no turn was open to end, never left as a ghost row for an operator to
//!   click at a delivery they can already see resolved.
//!   EVERY parked ask is answerable, not just the newest: an adapter running
//!   parallel tool calls blocks on several at once, and `parked` (not
//!   `fleet_session.current_request_fingerprint`, which has room for one) is
//!   what says which are live.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use agent_client_protocol::schema::v1::{PromptResponse, SessionNotification, StopReason};
use ainb_acp::circuit::{CircuitConfig, SlotCircuit};
use ainb_acp::client::{AcpError, AdapterProcess, PermissionRequest};
use ainb_acp::config::AdapterConfig;
use ainb_acp::reducer::TranscriptReducer;
use ainb_acp::store_writer::{HighWater, Lifecycle, StoreWriter, WriterConfig};
use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_proto::settings::{AcpPoolHealth, AcpProcessHealth, AcpSessionHealth};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::attention::AttentionRepo;
use ainb_hangar_store::repo::fleet::{
    FleetRepo, FleetSessionPatch, NewFleetEvent, ObservationAuthority,
};
use ainb_hangar_store::repo::fleet_acp_session::{
    FleetAcpSessionRepo, FleetAcpSessionRow, TurnEnd, TurnEndOutcome,
};
use ainb_hangar_store::repo::fleet_message::{FleetMessageRepo, NewFleetMessage};
use ainb_hangar_store::repo::fleet_provider_event::{
    FleetProviderEventRepo, NewFleetProviderEvent,
};
use sqlx::SqlitePool;
use tokio::sync::{Semaphore, mpsc, oneshot};
use tracing::Instrument as _;

// ------------------------------------------------------------ detail taxonomy

/// The per-scope FIFO was full; the prompt was never accepted.
pub const DELIVERY_QUEUE_FULL: &str = "queue_full";
/// The provider's breaker is open; every scope routed there fails fast.
pub const DELIVERY_BREAKER_OPEN: &str = "breaker_open";
/// The adapter process went away (crash or kill).
pub const DELIVERY_ADAPTER_EXIT: &str = "adapter_exit";
/// An operator stopped the session while the adapter was alive.
///
/// DISTINCT from [`DELIVERY_ADAPTER_EXIT`] on purpose: the runbook's first
/// question is "did the adapter exit", and answering it `yes` for a warm
/// process would inflate every crash count by every operator interrupt.
pub const DELIVERY_OPERATOR_STOP: &str = "operator_stop";
/// The turn outlived its wall-clock deadline and was cancelled.
pub const DELIVERY_TURN_DEADLINE: &str = "turn_deadline";
/// Convergence ran at boot: the daemon that owned this turn is gone.
pub const DELIVERY_DAEMON_RESTART: &str = "daemon_restart";
/// The adapter could not be started at all.
pub const DELIVERY_SPAWN_FAILED: &str = "spawn_failed";
/// The turn ended with an adapter-reported failure (refusal, cancel).
pub const DELIVERY_TURN_FAILED: &str = "turn_failed";
/// The turn could not be RECORDED, so it was never issued (I16).
///
/// Both convergence paths key off the persisted `open_turn_id`: the deadline
/// sweep queries it, and `converge_dirty_session` writes `acp.turn_interrupted`
/// only for a turn the store knows about. A prompt issued after that write
/// failed would be invisible to both, so a hung adapter would never be swept
/// and a dying one would resolve the leg with no marker. Nothing reached the
/// adapter, so FAILED is honest and an operator can resend.
pub const DELIVERY_TURN_UNRECORDED: &str = "turn_unrecorded";
/// The recipient exists but its session row is gone or dead.
pub const DELIVERY_SESSION_GONE: &str = "session_gone";
/// The provider's process is at its session cap and every tenant is busy.
///
/// Terminal, never requeued: the cap is a standing ceiling, not a transient
/// fault, and a retry that ignored it would put the process one tenant over the
/// maximum an operator configured. An operator resends once a turn ends.
pub const DELIVERY_PROVIDER_AT_CAPACITY: &str = "provider_at_capacity";
/// The pinned permission mode could not be proven for the session (I13).
///
/// Terminal, never requeued: retrying an adapter that will not hold the mode
/// just drives the same session in the wrong permission regime a second time.
pub const DELIVERY_MODE_UNPROVEN: &str = "mode_unproven";

/// The resume path fingerprint carried on the next delivery's receipt detail
/// and in the `acp.context_rebuilt` marker: the adapter still had the session.
pub const RESUME_LOADED: &str = "loaded";
/// See [`RESUME_LOADED`]: the context was rebuilt from persisted history.
pub const RESUME_REPRIMED: &str = "reprimed";
/// Neither of the above: there was nothing to resume.
///
/// A session that never had an adapter id and had no history to re-prime did
/// not LOSE anything, so it writes no `acp.context_rebuilt` marker and leaves
/// the receipt detail NULL. Fingerprinting it as `reprimed` would raise the
/// same flag on every healthy first turn in the fleet as on a genuine context
/// loss, which is exactly the signal the marker exists to carry.
///
/// Internal: it names the absence of a resume, so it never reaches the wire.
const RESUME_FRESH: &str = "fresh";

/// The `fleet_session.provider` token every ACP session carries.
pub const ACP_PROVIDER_TOKEN: &str = "acp";

/// How long a provider supervisor waits for silence on its update channel after
/// the transport closes, before it declares the adapter done talking.
const EXIT_QUIESCE: Duration = Duration::from_millis(50);

// --------------------------------------------------------------------- config

/// The permission mode an adapter gets when neither the built-in registry nor
/// config names one.
const DEFAULT_PERMISSION_MODE: &str = "default";

/// One `[acp.adapters.<name>]` table, as written.
///
/// Both fields are `Option` so an absent key means "leave the built-in alone"
/// rather than "reset it to a default": a table that only repoints `command`
/// must not silently unpin the permission mode, which is the setting that stops
/// an adapter inheriting `bypassPermissions` from ambient state.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct AcpAdapterToml {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    permission_mode: Option<String>,
}

/// Read `[acp.adapters]` from `~/.agents-in-a-box/config/config.toml`.
///
/// Empty on any failure (no file, no `$HOME`, bad TOML, malformed table), with
/// a warning: the built-in adapters are always the floor.
fn acp_adapters_from_config() -> std::collections::HashMap<String, AcpAdapterToml> {
    let Some(home) = std::env::var_os("HOME") else {
        return HashMap::new();
    };
    let path = std::path::PathBuf::from(home)
        .join(".agents-in-a-box")
        .join("config")
        .join("config.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    let root: toml::Value = match toml::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "acp: config.toml does not parse; using the built-in adapters");
            return HashMap::new();
        }
    };
    let Some(table) = root.get("acp").and_then(|acp| acp.get("adapters")).cloned() else {
        return HashMap::new();
    };
    table.try_into().unwrap_or_else(|error| {
        tracing::warn!(%error, "acp: [acp.adapters] is malformed; using the built-in adapters");
        HashMap::new()
    })
}

/// Pool tuning. Every knob the plan names, with its documented default.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// The adapter registry: token to spawn recipe. A provider absent here
    /// cannot be created by `fleet/acp_session_create`.
    pub adapters: HashMap<String, AdapterConfig>,
    /// Sessions multiplexed on ONE provider process before the LRU evicts.
    ///
    /// Eviction is ARRIVAL-triggered and has no idle threshold: a new tenant
    /// that would exceed this cap closes the least recently used session with
    /// no open turn ([`AcpPool::evict_if_at_cap`]). Nothing sweeps idle
    /// sessions on a process below its cap; the process idle window
    /// ([`PoolConfig::process_idle_window`]) is what reclaims a whole cold
    /// adapter.
    pub max_sessions_per_provider: usize,
    /// Turns in flight on one process at once.
    pub max_in_flight_per_process: usize,
    /// Prompts queued behind a scope's in-flight turn.
    pub queue_depth: usize,
    /// A provider process with zero live sessions stops after this long.
    pub process_idle_window: Duration,
    /// Wall-clock ceiling on ONE turn before `session/cancel` converges it.
    pub turn_deadline: Duration,
    /// How often the deadline/idle sweep runs.
    pub sweep_interval: Duration,
    /// Transcript commit cadence.
    pub writer: WriterConfig,
    /// Per-provider-process breaker tuning.
    pub circuit: CircuitConfig,
}

impl Default for PoolConfig {
    fn default() -> Self {
        let mode = DEFAULT_PERMISSION_MODE.to_string();
        let adapters = [
            ainb_acp::config::CLAUDE_ADAPTER,
            ainb_acp::config::CODEX_ADAPTER,
        ]
        .into_iter()
        .map(|name| (name.to_string(), AdapterConfig::new(name, mode.clone())))
        .collect();
        Self {
            adapters,
            max_sessions_per_provider: 16,
            max_in_flight_per_process: 4,
            queue_depth: 32,
            process_idle_window: Duration::from_mins(10),
            turn_deadline: Duration::from_mins(30),
            sweep_interval: Duration::from_secs(15),
            writer: WriterConfig::default(),
            circuit: CircuitConfig::default(),
        }
    }
}

impl PoolConfig {
    /// [`PoolConfig::default`] with the turn deadline overridden by
    /// `AINB_ACP_TURN_DEADLINE_MS` when it names a positive number.
    ///
    /// The 30-minute default is right for a human waiting on a real adapter and
    /// useless to a smoke run that has to PROVE the deadline converges a wedged
    /// turn (`scripts/chat-bus-smoke.sh`, journey `j5b`). The sweep interval
    /// follows the deadline down, because a 15 s sweep cannot observe a 2 s
    /// deadline promptly; it is never lengthened, so the production cadence is
    /// untouched when the variable is unset or junk.
    #[must_use]
    pub fn from_env() -> Self {
        let mut config = Self::from_config();
        if let Some(ms) = std::env::var("AINB_ACP_TURN_DEADLINE_MS")
            .ok()
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .filter(|ms| *ms > 0)
        {
            config.turn_deadline = Duration::from_millis(ms);
            config.sweep_interval = config
                .sweep_interval
                .min(Duration::from_millis(ms / 2).max(Duration::from_millis(100)));
        }
        config
    }

    /// [`PoolConfig::default`] with `[acp.adapters.*]` from the host config
    /// applied.
    ///
    /// The adapter registry was a hardcoded two-entry map with no user surface
    /// at all: a provider absent from it simply could not be created, and an
    /// adapter installed anywhere but `PATH` could not be reached. A named
    /// adapter here overrides the built-in entry; a new name adds one.
    ///
    /// Read directly off config.toml rather than through `ainb`, which this
    /// crate does not depend on, mirroring how the session-reader plugin reads
    /// `[session_reader]`. Every failure degrades to the built-ins: a malformed
    /// table must not leave the daemon with no adapters at all.
    #[must_use]
    pub fn from_config() -> Self {
        let mut config = Self::default();
        for (name, adapter) in acp_adapters_from_config() {
            let entry = config
                .adapters
                .entry(name.clone())
                .or_insert_with(|| AdapterConfig::new(name, DEFAULT_PERMISSION_MODE));
            // `filter(|c| !c.trim().is_empty())`: the registry seeds this row with
            // `""` and its help says blank resolves the adapter's name on PATH. A
            // hand-edited empty string would otherwise become an empty program path
            // that cannot spawn.
            if let Some(command) = adapter.command.filter(|c| !c.trim().is_empty()) {
                entry.command = std::path::PathBuf::from(command);
            }
            if let Some(mode) = adapter.permission_mode {
                // Validated here, not just in the settings screen: the row's
                // Choice list gates the UI and `ainb config set`, but a hand-edited
                // typo would otherwise reach `session/new` unchecked — and an
                // unpinned adapter has been observed inheriting
                // `bypassPermissions`, so a silent fall-through is not safe.
                const MODES: &[&str] = &["default", "acceptEdits", "bypassPermissions", "plan"];
                if MODES.contains(&mode.as_str()) {
                    entry.permission_mode = mode;
                } else {
                    tracing::warn!(
                        %mode,
                        "unknown acp permission_mode in config; using \"default\""
                    );
                }
            }
        }
        config
    }

    /// The pinned permission mode for `provider`, or `default`.
    #[must_use]
    pub fn permission_mode(&self, provider: &str) -> String {
        self.adapters.get(provider).map_or_else(
            || "default".to_string(),
            |config| config.permission_mode.clone(),
        )
    }

    /// Whether the registry knows how to spawn `provider`.
    #[must_use]
    pub fn knows(&self, provider: &str) -> bool {
        self.adapters.contains_key(provider)
    }
}

// -------------------------------------------------------------- public shapes

/// What `message_send` learned by handing a prompt to the pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// Accepted; the delivery stays PENDING and resolves at TURN END.
    Queued,
    /// Refused outright with an enumerated delivery detail.
    Rejected(&'static str),
}

/// How an operator answered a parked permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    /// Take the first `allow`-flavoured option the adapter offered.
    Approve,
    /// Take the first `reject`-flavoured option, else answer `Cancelled`.
    Deny,
    /// Take exactly this option id (the structured-answer path).
    Option(String),
}

/// The outcome of routing an answer back to the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionAnswer {
    /// HANDED OFF to the adapter's pending JSON-RPC id, carrying the option id
    /// that was selected.
    ///
    /// Deliberately not "applied": `Responder::respond` enqueues the response
    /// on this process's outgoing side and returns, and ACP has no
    /// acknowledgement for a permission answer, so no local state can prove the
    /// adapter received it, let alone acted on it. A daemon that dies in that
    /// window loses the decision; the adapter re-asks on its next turn and
    /// convergence has already closed the attention row, so the operator is
    /// asked again rather than left staring at a row nobody will answer. The
    /// receipt detail says hand-off for the same reason (`rpc::acp_permission_receipt`).
    Delivered(String),
    /// No permission with that fingerprint is parked (already answered, or the
    /// adapter died and convergence cleared it).
    NotWaiting,
    /// The answer named an option the adapter never offered.
    UnknownOption,
    /// The session has no live actor at all.
    NoSession,
}

/// Why a session is being converged. The token lands in the delivery detail, so
/// "why did this message not deliver" is answerable from the receipt alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvergeCause {
    /// The daemon restarted while the turn was open (the boot scan).
    DaemonRestart,
    /// The adapter process exited.
    AdapterExit,
    /// The turn outlived its deadline.
    TurnDeadline,
    /// An operator asked for the session to stop.
    OperatorStop,
}

impl ConvergeCause {
    /// The enumerated delivery-detail token.
    #[must_use]
    pub const fn detail(self) -> &'static str {
        match self {
            Self::DaemonRestart => DELIVERY_DAEMON_RESTART,
            Self::AdapterExit => DELIVERY_ADAPTER_EXIT,
            Self::TurnDeadline => DELIVERY_TURN_DEADLINE,
            Self::OperatorStop => DELIVERY_OPERATOR_STOP,
        }
    }
}

// ------------------------------------------------------------------ the pool

/// Everything one hosted session's actor is told from the outside.
#[derive(Debug)]
enum Control {
    /// Cancel the in-flight turn (`session/cancel`) and converge with `cause`.
    ///
    /// `turn_id` makes the cancel TURN-SCOPED (I16): the deadline sweep reads an
    /// overdue `open_turn_id` from the store and only then sends this message,
    /// so by the time the actor handles it the overdue turn may already have
    /// ended and the NEXT queued prompt may be running. A `Some(turn_id)` that
    /// no longer matches the open turn is a no-op, which is also what makes an
    /// operator Interrupt idempotent against a turn that ended in flight.
    /// `None` means "whatever is open right now" (operator Stop/Kill).
    Cancel {
        cause: ConvergeCause,
        turn_id: Option<String>,
    },
    /// Answer a parked permission.
    Answer {
        fingerprint: String,
        decision: PermissionDecision,
        reply: oneshot::Sender<PermissionAnswer>,
    },
    /// Close the adapter-side session; the process stays warm (LRU eviction).
    Evict,
    /// Stop the actor entirely.
    Shutdown,
    /// The named process died: drop the handle and converge.
    ///
    /// PROCESS-SCOPED for the same reason [`Control::Cancel`] is turn-scoped.
    /// The exit watcher reads the routes it hosted and only then sends this,
    /// and a legal I6 requeue in between moves the session onto a NEW process.
    /// Applied unconditionally, the late event from the dead process would
    /// detach a live route, write `turn_interrupted`, resolve a running turn's
    /// leg UNKNOWN and drain the queue, all while the prompt is still going.
    /// A `Weak` that no longer upgrades cannot be the process this actor holds,
    /// because holding it would keep it alive.
    ProcessExited(Weak<ProviderProcess>),
}

/// One queued prompt.
#[derive(Debug, Clone)]
struct PromptJob {
    message_id: String,
    text: String,
}

/// The live facts the health pane reads without touching the actor.
#[derive(Debug, Default)]
struct SessionStats {
    turn_started_at: StdMutex<Option<Instant>>,
    pending_permissions: AtomicU32,
    state: StdMutex<String>,
    /// Transcript payload bytes this session has COMMITTED. The demux channels
    /// are unbounded by design, so this is the growth signal that replaces the
    /// backpressure we deliberately do not apply.
    transcript_bytes: AtomicU64,
}

struct SessionHandle {
    scope_key: String,
    provider: String,
    prompts: mpsc::Sender<PromptJob>,
    control: mpsc::UnboundedSender<Control>,
    stats: Arc<SessionStats>,
    /// Which actor incarnation owns this entry. An exiting actor removes itself
    /// ONLY when the map still holds its own generation, so a later actor for
    /// the same key is never evicted by its predecessor's teardown.
    generation: u64,
}

/// One live adapter process and the routing table for the sessions on it.
struct ProviderProcess {
    provider: String,
    process: Arc<AdapterProcess>,
    routes: Arc<StdMutex<HashMap<String, SessionRoute>>>,
    in_flight: Arc<Semaphore>,
    in_flight_used: Arc<AtomicU32>,
    /// When this process last had ZERO routes, or `None` while it has tenants.
    /// The idle window is measured from here, so a warm process is not killed
    /// the instant a sweep catches it between tenants.
    empty_since: StdMutex<Option<Instant>>,
    /// Sessions between `session/new` and route registration. A brand-new
    /// adapter has no routes yet and must not read as idle: `session/new` can
    /// take up to the spawn timeout against a real adapter, which is longer than
    /// a sweep tick.
    ///
    /// It is also the session cap's RESERVATION: it is incremented before the
    /// cap is consulted, so two arrivals that race count each other instead of
    /// both reading a table with one free slot in it.
    attaching: AtomicU32,
    /// One capacity decision at a time. Eviction is asynchronous (the victim's
    /// own actor closes its adapter session), so two arrivals evaluating the
    /// cap concurrently would read the same route table, choose the SAME idle
    /// victim, and both attach: one tenant over the cap with nothing left to
    /// evict.
    evicting: tokio::sync::Mutex<()>,
    /// [`PoolConfig::process_idle_window`], copied so the sweep predicate needs
    /// only the process.
    idle_window: Duration,
    /// Set before the idle sweep kills this process. An INTENTIONAL stop is not
    /// a crash: counting it would push a provider that is merely unused toward
    /// its breaker, and would leave a phantom `exited` row on the health pane
    /// for a process nobody wanted.
    stopping: std::sync::atomic::AtomicBool,
}

/// Hold a process's "a session is attaching" count for the duration of an
/// `ensure_session`, whatever way it returns.
struct AttachGuard(Arc<ProviderProcess>);

impl Drop for AttachGuard {
    fn drop(&mut self) {
        self.0.attaching.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Clone)]
struct SessionRoute {
    session_key: String,
    updates: mpsc::UnboundedSender<SessionNotification>,
    permissions: mpsc::UnboundedSender<PermissionRequest>,
}

/// The daemon's ACP agent pool.
pub struct AcpPool {
    store: Store,
    events: crate::events::EventSink,
    config: PoolConfig,
    providers: tokio::sync::Mutex<HashMap<String, Arc<ProviderProcess>>>,
    /// One spawn at a time per provider, held INSTEAD of the `providers` map
    /// lock: `AdapterProcess::spawn` runs initialize plus the mode assertion and
    /// is bounded only by the spawn timeout, and `health()` (the
    /// `hangar/daemon_health` pane that answers "why is the copilot stuck")
    /// takes the map lock.
    spawn_locks: StdMutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Providers with a spawn in flight, so health reports `spawning` rather
    /// than an absent process.
    spawning: StdMutex<HashSet<String>>,
    sessions: tokio::sync::Mutex<HashMap<String, SessionHandle>>,
    circuits: StdMutex<HashMap<String, SlotCircuit>>,
    evicted_total: AtomicU32,
    next_generation: AtomicU64,
}

impl AcpPool {
    /// Build a pool. Nothing is spawned until the first prompt.
    #[must_use]
    pub fn new(store: Store, events: crate::events::EventSink, config: PoolConfig) -> Arc<Self> {
        Arc::new(Self {
            store,
            events,
            config,
            providers: tokio::sync::Mutex::new(HashMap::new()),
            spawn_locks: StdMutex::new(HashMap::new()),
            spawning: StdMutex::new(HashSet::new()),
            sessions: tokio::sync::Mutex::new(HashMap::new()),
            circuits: StdMutex::new(HashMap::new()),
            evicted_total: AtomicU32::new(0),
            next_generation: AtomicU64::new(1),
        })
    }

    /// The adapter registry this pool validates `fleet/acp_session_create`
    /// against.
    #[must_use]
    pub const fn config(&self) -> &PoolConfig {
        &self.config
    }

    /// Hand one prompt to the recipient's OWN session (never a broadcast
    /// scope's, which owns no session). The delivery stays PENDING; the actor
    /// resolves it at turn end.
    pub async fn submit_prompt(
        self: &Arc<Self>,
        session_key: &str,
        message_id: &str,
        text: &str,
    ) -> SubmitOutcome {
        let row = match FleetAcpSessionRepo::get(self.store.pool(), session_key).await {
            Ok(Some(row)) if row.state != "DEAD" => row,
            Ok(_) => return SubmitOutcome::Rejected(DELIVERY_SESSION_GONE),
            Err(error) => {
                tracing::error!(%session_key, %error, "acp pool could not read its session row");
                return SubmitOutcome::Rejected(DELIVERY_SESSION_GONE);
            }
        };
        // The breaker is consulted BEFORE the queue: a provider that is
        // crash-looping must fail every scope routed to it fast, not fill 32
        // queue slots per scope with prompts that will fail anyway.
        if self.breaker_open(&row.provider) {
            return SubmitOutcome::Rejected(DELIVERY_BREAKER_OPEN);
        }
        let sender = {
            let mut sessions = self.sessions.lock().await;
            if !sessions.contains_key(session_key) {
                let handle = self.spawn_actor(&row);
                sessions.insert(session_key.to_string(), handle);
            }
            sessions.get(session_key).map(|handle| handle.prompts.clone())
        };
        let Some(sender) = sender else {
            return SubmitOutcome::Rejected(DELIVERY_SESSION_GONE);
        };
        match sender.try_send(PromptJob {
            message_id: message_id.to_string(),
            text: text.to_string(),
        }) {
            Ok(()) => SubmitOutcome::Queued,
            // BOUNDED by construction: a full queue is an answered delivery,
            // not an unbounded buffer and not a silent drop.
            Err(mpsc::error::TrySendError::Full(_)) => SubmitOutcome::Rejected(DELIVERY_QUEUE_FULL),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                SubmitOutcome::Rejected(DELIVERY_SESSION_GONE)
            }
        }
    }

    /// Route an operator's answer back to the adapter's pending JSON-RPC id.
    pub async fn answer_permission(
        &self,
        session_key: &str,
        fingerprint: &str,
        decision: PermissionDecision,
    ) -> PermissionAnswer {
        let control = {
            let sessions = self.sessions.lock().await;
            sessions.get(session_key).map(|handle| handle.control.clone())
        };
        let Some(control) = control else {
            return PermissionAnswer::NoSession;
        };
        let (reply, wait) = oneshot::channel();
        if control
            .send(Control::Answer {
                fingerprint: fingerprint.to_string(),
                decision,
                reply,
            })
            .is_err()
        {
            return PermissionAnswer::NoSession;
        }
        wait.await.unwrap_or(PermissionAnswer::NoSession)
    }

    /// `session/cancel` the scope's in-flight turn and converge it. The shared
    /// process and its OTHER sessions are untouched.
    pub async fn cancel(&self, session_key: &str, cause: ConvergeCause) -> bool {
        self.cancel_turn(session_key, cause, None).await
    }

    /// The turn-scoped cancel. `turn_id` is `Some` only when the caller knows
    /// WHICH turn it means (the deadline sweep, which read the id from the store
    /// some time before the actor gets this message); the actor drops the
    /// message when that turn is no longer the open one, so a cancel can never
    /// land on the turn that legitimately succeeded it.
    pub async fn cancel_turn(
        &self,
        session_key: &str,
        cause: ConvergeCause,
        turn_id: Option<String>,
    ) -> bool {
        let sessions = self.sessions.lock().await;
        sessions
            .get(session_key)
            .is_some_and(|handle| handle.control.send(Control::Cancel { cause, turn_id }).is_ok())
    }

    /// Stop hosting this session: cancel, close the adapter-side session, and
    /// drop the actor. The provider process stays warm for its other tenants.
    ///
    /// The handle stays in the map until the ACTOR removes it on exit. Removing
    /// it here would let a concurrent `submit_prompt` for the same key spawn a
    /// SECOND actor while the first is still closing its adapter session and
    /// converging: two `session/new` calls, two writers on one transcript, two
    /// final messages for one turn. A prompt that arrives during the shutdown
    /// window is answered `session_gone` (the actor closes its queue before it
    /// goes), and the next one after that spawns a fresh actor.
    pub async fn teardown(&self, session_key: &str, cause: ConvergeCause) -> bool {
        let control = {
            let sessions = self.sessions.lock().await;
            sessions.get(session_key).map(|handle| handle.control.clone())
        };
        let Some(control) = control else {
            return false;
        };
        let _ = control.send(Control::Cancel {
            cause,
            turn_id: None,
        });
        control.send(Control::Shutdown).is_ok()
    }

    /// Kill a provider process outright (the `Kill` action, and the fault
    /// injection every I16 test needs). Convergence runs for every session it
    /// hosted, exactly as if it had crashed.
    pub async fn kill_provider(&self, provider: &str) -> bool {
        let process = {
            let providers = self.providers.lock().await;
            providers.get(provider).map(Arc::clone)
        };
        process.is_some_and(|process| {
            process.process.kill();
            true
        })
    }

    /// Health rows for providers with NO live process: one mid-spawn, and one
    /// whose process has died with the breaker still counting.
    ///
    /// A 30 s spawn is exactly when someone is staring at this pane, and a dead
    /// process is dropped from the map by its supervisor, so without these the
    /// breaker that is refusing every prompt would be invisible in precisely
    /// the incident it explains ("why is the copilot stuck" answers
    /// `breaker_open`, not "there is no such provider").
    fn processless_rows(&self, live: &[AcpProcessHealth]) -> Vec<AcpProcessHealth> {
        let spawning: Vec<String> = self
            .spawning
            .lock()
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default();
        let faulted: Vec<String> = self.circuits.lock().map_or_else(
            |_| Vec::new(),
            |circuits| {
                let now = Instant::now();
                circuits
                    .iter()
                    .filter(|(_, circuit)| {
                        circuit.is_open(now) || circuit.consecutive_failures() > 0
                    })
                    .map(|(provider, _)| provider.clone())
                    .collect()
            },
        );
        let mut rows: Vec<AcpProcessHealth> = Vec::new();
        for provider in spawning.iter().chain(faulted.iter()) {
            if live.iter().chain(rows.iter()).any(|row| &row.provider == provider) {
                continue;
            }
            let (breaker_open, breaker_failures) = self.breaker_state(provider);
            rows.push(AcpProcessHealth {
                provider: provider.clone(),
                state: if spawning.contains(provider) {
                    "spawning"
                } else {
                    "exited"
                }
                .to_string(),
                sessions: 0,
                session_cap: u32::try_from(self.config.max_sessions_per_provider)
                    .unwrap_or(u32::MAX),
                in_flight: 0,
                in_flight_cap: u32::try_from(self.config.max_in_flight_per_process)
                    .unwrap_or(u32::MAX),
                breaker_open,
                breaker_failures,
                provider_version: None,
            });
        }
        rows
    }

    /// The pool's live shape for `hangar/daemon_health`.
    pub async fn health(&self) -> AcpPoolHealth {
        let now = Instant::now();
        let providers = self.providers.lock().await;
        let mut processes: Vec<AcpProcessHealth> = providers
            .values()
            .map(|process| {
                let (open, failures) = self.breaker_state(&process.provider);
                AcpProcessHealth {
                    provider: process.provider.clone(),
                    state: if process.process.is_alive() {
                        "running".to_string()
                    } else {
                        "exited".to_string()
                    },
                    sessions: process
                        .routes
                        .lock()
                        .map_or(0, |routes| u32::try_from(routes.len()).unwrap_or(u32::MAX)),
                    session_cap: u32::try_from(self.config.max_sessions_per_provider)
                        .unwrap_or(u32::MAX),
                    in_flight: process.in_flight_used.load(Ordering::Relaxed),
                    in_flight_cap: u32::try_from(self.config.max_in_flight_per_process)
                        .unwrap_or(u32::MAX),
                    breaker_open: open,
                    breaker_failures: failures,
                    provider_version: process.process.info().version.clone(),
                }
            })
            .collect();
        drop(providers);
        let extra = self.processless_rows(&processes);
        processes.extend(extra);

        let sessions = self.sessions.lock().await;
        let session_rows: Vec<AcpSessionHealth> = sessions
            .iter()
            .map(|(session_key, handle)| {
                let turn_started = handle.stats.turn_started_at.lock().ok().and_then(|slot| *slot);
                AcpSessionHealth {
                    session_key: session_key.clone(),
                    scope_key: handle.scope_key.clone(),
                    provider: handle.provider.clone(),
                    state: handle
                        .stats
                        .state
                        .lock()
                        .map_or_else(|_| "IDLE".to_string(), |state| state.clone()),
                    queue_depth: u32::try_from(
                        handle.prompts.max_capacity() - handle.prompts.capacity(),
                    )
                    .unwrap_or(u32::MAX),
                    queue_capacity: u32::try_from(handle.prompts.max_capacity())
                        .unwrap_or(u32::MAX),
                    turn_open: turn_started.is_some(),
                    turn_age_ms: turn_started.map(|start| {
                        i64::try_from(now.saturating_duration_since(start).as_millis())
                            .unwrap_or(i64::MAX)
                    }),
                    pending_permissions: handle.stats.pending_permissions.load(Ordering::Relaxed),
                    transcript_bytes: handle.stats.transcript_bytes.load(Ordering::Relaxed),
                }
            })
            .collect();
        drop(sessions);
        AcpPoolHealth {
            processes,
            sessions: session_rows,
            evicted_total: self.evicted_total.load(Ordering::Relaxed),
        }
    }

    /// The turn-deadline sweep: `session/cancel` any turn that outlived
    /// [`PoolConfig::turn_deadline`], one SESSION at a time.
    pub fn spawn_sweeper(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let pool = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(pool.config.sweep_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                pool.sweep_once().await;
            }
        })
    }

    /// One sweep pass: expire overdue turns, then stop idle processes.
    pub async fn sweep_once(&self) {
        let deadline_ms = i64::try_from(self.config.turn_deadline.as_millis()).unwrap_or(i64::MAX);
        let cutoff = SystemClock.now_ms().saturating_sub(deadline_ms);
        let overdue = FleetAcpSessionRepo::list_open_turns_older_than(self.store.pool(), cutoff)
            .await
            .unwrap_or_default();
        for row in overdue {
            tracing::warn!(
                session_key = %row.session_key,
                turn_id = ?row.open_turn_id,
                "acp turn outlived its deadline; cancelling this session only"
            );
            // TURN-SCOPED (I16): between this read and the actor handling the
            // message the overdue turn can end and the next queued prompt can
            // start. Carrying the id makes the cancel a no-op in that case
            // instead of killing a turn that is seconds old.
            self.cancel_turn(
                &row.session_key,
                ConvergeCause::TurnDeadline,
                row.open_turn_id.clone(),
            )
            .await;
        }
        self.stop_idle_processes().await;
    }

    /// A provider process hosting zero sessions is stopped after
    /// [`PoolConfig::process_idle_window`] has ELAPSED; a warm process, or one
    /// whose first session is still attaching, is left alone.
    async fn stop_idle_processes(&self) {
        let now = Instant::now();
        let expired: Vec<String> = {
            let providers = self.providers.lock().await;
            providers
                .iter()
                .filter(|(_, process)| Self::idle_window_expired(process, now))
                .map(|(token, _)| token.clone())
                .collect()
        };
        for provider in expired {
            let process = {
                let mut providers = self.providers.lock().await;
                providers.remove(&provider)
            };
            if let Some(process) = process {
                tracing::info!(
                    %provider,
                    idle_window_secs = self.config.process_idle_window.as_secs(),
                    "stopping an acp adapter process that has been idle for its whole window"
                );
                process.stopping.store(true, Ordering::Relaxed);
                process.process.kill();
            }
        }
    }

    /// Has this process been tenant-free for longer than the idle window?
    ///
    /// Also STAMPS the transition, so the clock starts at the first sweep that
    /// observes an empty route table rather than at the kill. A process with a
    /// session still between `session/new` and route registration is NOT idle:
    /// `session/new` can take up to the spawn timeout against a real adapter,
    /// which is longer than a sweep tick, and killing there would SIGKILL a
    /// healthy adapter, resolve its prompt UNKNOWN, and count as a crash on the
    /// breaker.
    fn idle_window_expired(process: &Arc<ProviderProcess>, now: Instant) -> bool {
        let busy = process.attaching.load(Ordering::Relaxed) > 0
            || process.routes.lock().is_ok_and(|routes| !routes.is_empty());
        let Ok(mut empty_since) = process.empty_since.lock() else {
            return false;
        };
        if busy {
            *empty_since = None;
            return false;
        }
        // `get_or_insert` IS the stamp: the first sweep to see an empty route
        // table starts the clock at `now` (and therefore measures zero elapsed),
        // every later one measures from that same instant.
        let since = *empty_since.get_or_insert(now);
        now.saturating_duration_since(since) >= process.idle_window
    }

    fn breaker_open(&self, provider: &str) -> bool {
        let now = Instant::now();
        self.circuits
            .lock()
            .is_ok_and(|circuits| circuits.get(provider).is_some_and(|c| c.is_open(now)))
    }

    fn breaker_state(&self, provider: &str) -> (bool, u32) {
        let now = Instant::now();
        self.circuits.lock().map_or((false, 0), |circuits| {
            circuits
                .get(provider)
                .map_or((false, 0), |c| (c.is_open(now), c.consecutive_failures()))
        })
    }

    fn record_provider_crash(&self, provider: &str) {
        if let Ok(mut circuits) = self.circuits.lock() {
            let circuit = circuits
                .entry(provider.to_string())
                .or_insert_with(|| SlotCircuit::new(self.config.circuit));
            circuit.record_crash(Instant::now());
        }
    }

    fn record_provider_success(&self, provider: &str) {
        if let Ok(mut circuits) = self.circuits.lock() {
            circuits
                .entry(provider.to_string())
                .or_insert_with(|| SlotCircuit::new(self.config.circuit))
                .record_success();
        }
    }

    /// The live process for `provider`, or `None` when there is none.
    async fn live_process(&self, provider: &str) -> Option<Arc<ProviderProcess>> {
        let mut providers = self.providers.lock().await;
        match providers.get(provider) {
            Some(existing) if existing.process.is_alive() => Some(Arc::clone(existing)),
            Some(_) => {
                providers.remove(provider);
                None
            }
            None => None,
        }
    }

    /// Get the provider's live process, spawning it on first use.
    ///
    /// The `providers` map lock is NEVER held across the spawn. `spawn` runs
    /// initialize plus the mode assertion and is bounded only by the adapter's
    /// spawn timeout; `health()` takes the same lock and is the pane that
    /// answers "why is the copilot stuck", so holding it here would blind the
    /// operator for exactly as long as the interesting failure lasts.
    /// Concurrent callers serialise on a PER-PROVIDER spawn lock instead, so
    /// they await the spawn rather than duplicating it.
    ///
    /// The `acp.spawn` span records which path ran, so "what is the pool doing"
    /// is answerable from the log alone.
    async fn provider_process(
        self: &Arc<Self>,
        provider: &str,
    ) -> Result<Arc<ProviderProcess>, AcpError> {
        if let Some(existing) = self.live_process(provider).await {
            return Ok(existing);
        }
        let gate = {
            let mut locks = self.spawn_locks.lock().expect("spawn lock map");
            Arc::clone(
                locks
                    .entry(provider.to_string())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        let _spawning = gate.lock().await;
        // Another caller may have spawned it while we waited on the gate.
        if let Some(existing) = self.live_process(provider).await {
            return Ok(existing);
        }
        let config =
            self.config.adapters.get(provider).cloned().ok_or_else(|| AcpError::Spawn {
                adapter: provider.to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "provider is not in the adapter registry",
                ),
            })?;
        let span = tracing::info_span!(
            "acp.spawn",
            provider = %provider,
            path = "session_new",
            mode = %config.permission_mode,
            provider_version = tracing::field::Empty,
        );
        self.spawn_provider_process(provider, config, span.clone())
            .instrument(span)
            .await
    }

    /// The spawn itself, running INSIDE the caller's `acp.spawn` span.
    ///
    /// It is a separate function so the span can be attached with `.instrument`
    /// rather than `span.enter()`. `Entered` is `Send` in tracing 0.1 (unlike
    /// `EnteredSpan`), so holding one across an `.await` compiles, and then the
    /// guard is dropped on whichever worker resumed the task. The worker that
    /// ENTERED keeps the span id on its thread-local stack forever, and the next
    /// contextual span opened on that worker clones an already-closed span. That
    /// leaves a `DataInner` slot back in the registry's pool with a non-zero ref
    /// count, and the next `new_span` anywhere in the process trips
    /// `tracing-subscriber`'s `sharded.rs` refcount assertion, killing whatever
    /// task happened to open that span, which in the daemon is usually an RPC
    /// connection handler.
    async fn spawn_provider_process(
        self: &Arc<Self>,
        provider: &str,
        config: AdapterConfig,
        span: tracing::Span,
    ) -> Result<Arc<ProviderProcess>, AcpError> {
        let (update_tx, update_rx) = mpsc::unbounded_channel();
        let (permission_tx, permission_rx) = mpsc::unbounded_channel();
        if let Ok(mut spawning) = self.spawning.lock() {
            spawning.insert(provider.to_string());
        }
        let spawned = AdapterProcess::spawn(&config, update_tx, permission_tx).await;
        if let Ok(mut spawning) = self.spawning.lock() {
            spawning.remove(provider);
        }
        let process = match spawned {
            Ok(process) => Arc::new(process),
            Err(error) => {
                self.record_provider_crash(provider);
                return Err(error);
            }
        };
        span.record(
            "provider_version",
            tracing::field::display(process.info().version.clone().unwrap_or_default()),
        );
        self.record_provider_success(provider);

        let entry = Arc::new(ProviderProcess {
            provider: provider.to_string(),
            process: Arc::clone(&process),
            routes: Arc::new(StdMutex::new(HashMap::new())),
            in_flight: Arc::new(Semaphore::new(self.config.max_in_flight_per_process)),
            in_flight_used: Arc::new(AtomicU32::new(0)),
            // Its first tenant is on the way in: the idle clock starts only once
            // a sweep sees it genuinely empty.
            empty_since: StdMutex::new(None),
            attaching: AtomicU32::new(0),
            evicting: tokio::sync::Mutex::new(()),
            idle_window: self.config.process_idle_window,
            stopping: std::sync::atomic::AtomicBool::new(false),
        });
        self.providers.lock().await.insert(provider.to_string(), Arc::clone(&entry));

        self.spawn_supervisor(Arc::clone(&entry), update_rx, permission_rx);
        Ok(entry)
    }

    /// The per-PROCESS task: demultiplex by `sessionId`, and converge every
    /// hosted session when the process goes away.
    fn spawn_supervisor(
        self: &Arc<Self>,
        entry: Arc<ProviderProcess>,
        mut updates: mpsc::UnboundedReceiver<SessionNotification>,
        mut permissions: mpsc::UnboundedReceiver<PermissionRequest>,
    ) {
        let pool = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    // BIASED, notifications first: an adapter that writes its
                    // last chunks and exits closes the transport in the same
                    // breath, and an unbiased select picks a ready arm at
                    // RANDOM. Losing that race drops committed-by-the-adapter
                    // output on the floor. A recv that returns `None` disables
                    // its own arm, so the exit arm is still reached.
                    biased;
                    Some(notification) = updates.recv() => forward_update(&entry, notification),
                    Some(permission) = permissions.recv() => forward_permission(&entry, permission),
                    () = entry.process.wait_closed() => break,
                    else => break,
                }
            }
            // The closed signal and the notification handlers RACE: the handler
            // that hands us a `session/update` runs as its own future, so a
            // chunk the adapter wrote before it died can be dispatched after
            // `wait_closed()` has already resolved. Quiesce on a short silence
            // rather than trusting that ordering. It is the same shape
            // `SessionActor::quiesce` uses for the turn-reply race, and it is
            // there for the same reason: the transcript is the only place this
            // output exists.
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match tokio::time::timeout(EXIT_QUIESCE, updates.recv()).await {
                    Ok(Some(notification)) => forward_update(&entry, notification),
                    // Silence, or a closed channel: the adapter is done talking.
                    Ok(None) | Err(_) => break,
                }
                if Instant::now() >= deadline {
                    break;
                }
            }
            let hosted: Vec<String> = entry
                .routes
                .lock()
                .map(|routes| routes.values().map(|r| r.session_key.clone()).collect())
                .unwrap_or_default();
            tracing::warn!(
                provider = %entry.provider,
                sessions = hosted.len(),
                "acp adapter process exited; converging every session it hosted"
            );
            if !entry.stopping.load(Ordering::Relaxed) {
                pool.record_provider_crash(&entry.provider);
            }
            {
                let mut providers = pool.providers.lock().await;
                if providers
                    .get(&entry.provider)
                    .is_some_and(|current| Arc::ptr_eq(current, &entry))
                {
                    providers.remove(&entry.provider);
                }
            }
            // Convergence runs IN the actor so exactly one writer per session
            // touches the open turn; the actor falls back to the shared
            // function, which is the same one the boot scan calls.
            let sessions = pool.sessions.lock().await;
            for session_key in hosted {
                if let Some(handle) = sessions.get(&session_key) {
                    // Named, so a session that has already moved on ignores it.
                    let _ = handle.control.send(Control::ProcessExited(Arc::downgrade(&entry)));
                }
            }
        });
    }

    fn spawn_actor(self: &Arc<Self>, row: &FleetAcpSessionRow) -> SessionHandle {
        let (prompt_tx, prompt_rx) = mpsc::channel(self.config.queue_depth);
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let stats = Arc::new(SessionStats {
            turn_started_at: StdMutex::new(None),
            pending_permissions: AtomicU32::new(0),
            state: StdMutex::new(row.state.clone()),
            transcript_bytes: AtomicU64::new(0),
        });
        let actor = SessionActor {
            generation,
            pool: Arc::clone(self),
            session_key: row.session_key.clone(),
            scope_key: row.scope_key.clone(),
            provider: row.provider.clone(),
            cwd: row.cwd.clone(),
            writer: StoreWriter::new(
                self.store.clone(),
                row.provider.clone(),
                row.session_key.clone(),
                Box::new(SystemIdGen),
                self.pool_writer_config(),
            ),
            reducer: TranscriptReducer::new(String::new()),
            acp_session_id: None,
            process: None,
            updates: None,
            permissions: None,
            parked: HashMap::new(),
            turn: None,
            stats: Arc::clone(&stats),
            prompts: prompt_rx,
            control: control_rx,
            pending_prelude: None,
            resume_path: None,
        };
        tokio::spawn(actor.run());
        SessionHandle {
            scope_key: row.scope_key.clone(),
            provider: row.provider.clone(),
            prompts: prompt_tx,
            control: control_tx,
            stats,
            generation,
        }
    }

    /// Drop an exited actor's map entry, but ONLY while the map still holds
    /// that actor's own incarnation.
    async fn retire_actor(&self, session_key: &str, generation: u64) {
        let mut sessions = self.sessions.lock().await;
        retire_if_current(&mut sessions, session_key, generation, |handle| {
            handle.generation
        });
    }

    const fn pool_writer_config(&self) -> WriterConfig {
        self.config.writer
    }
}

// -------------------------------------------------------------- shared handle

static ACTIVE_POOL: OnceLock<tokio::sync::RwLock<Option<Arc<AcpPool>>>> = OnceLock::new();

fn active_slot() -> &'static tokio::sync::RwLock<Option<Arc<AcpPool>>> {
    ACTIVE_POOL.get_or_init(|| tokio::sync::RwLock::new(None))
}

/// Publish the process-wide pool the RPC handlers route through.
pub async fn install(pool: Arc<AcpPool>) {
    *active_slot().write().await = Some(pool);
}

/// Drop the process-wide pool (shutdown, and test isolation).
pub async fn uninstall() {
    *active_slot().write().await = None;
}

/// Drop the process-wide pool WITHOUT awaiting.
///
/// For a test drop-guard, which runs while a failing assertion unwinds and
/// therefore cannot await: leaving a pool installed there would route the NEXT
/// test's prompts into a dead one. Answers `false` only if someone holds the
/// slot, which nothing does outside [`install`] and [`uninstall`].
#[must_use]
pub fn try_uninstall() -> bool {
    active_slot().try_write().is_ok_and(|mut slot| {
        *slot = None;
        true
    })
}

/// The process-wide pool, when one is running.
pub async fn active_handle() -> Option<Arc<AcpPool>> {
    active_slot().read().await.clone()
}

// -------------------------------------------------------------- convergence

/// Convergence failures. Every arm is a store fault; there is no "converged
/// wrongly" case by construction.
#[derive(Debug, thiserror::Error)]
pub enum ConvergeError {
    /// `SQLite` failed.
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
}

/// The BOOT scan (I16): converge every session a previous daemon left dirty.
///
/// A daemon killed with SIGKILL leaves `open_turn_id` set, its legs PENDING and
/// any parked permission's attention row open, and nothing in the running
/// daemon would ever revisit them: the process-exit path and the deadline sweep
/// only see sessions THIS process is hosting. Runs before the pool is
/// installed, so a scope that died mid-turn is usable again on the first prompt
/// rather than after an operator restarts something.
///
/// Idempotent, like the routine it fans out to: a boot that finds nothing dirty
/// writes nothing.
pub async fn converge_dirty_sessions_at_boot(pool: &SqlitePool, events: &crate::events::EventSink) {
    let dirty = match FleetAcpSessionRepo::list_dirty(pool).await {
        Ok(dirty) => dirty,
        Err(error) => {
            tracing::error!(%error, "acp boot scan could not list dirty sessions");
            return;
        }
    };
    if dirty.is_empty() {
        return;
    }
    tracing::info!(
        sessions = dirty.len(),
        "converging acp sessions left dirty by a previous daemon"
    );
    for row in dirty {
        if let Err(error) =
            converge_dirty_session(pool, events, &row.session_key, ConvergeCause::DaemonRestart)
                .await
        {
            tracing::error!(
                session_key = %row.session_key,
                %error,
                "acp boot convergence failed"
            );
        }
    }
}

/// Bring one ACP session back to a defined state, whatever left it dirty.
///
/// THE shared routine (plan I16): [`converge_dirty_sessions_at_boot`] calls it,
/// the pool's process-exit path calls it, and the turn-deadline sweep calls it.
/// Two copies would drift, and the drift would only show up as a wedged scope in
/// production.
///
/// Idempotent by construction: every write is conditioned on the dirty state it
/// repairs, so running it twice (or at boot after a runtime run) changes
/// nothing.
pub async fn converge_dirty_session(
    pool: &SqlitePool,
    events: &crate::events::EventSink,
    session_key: &str,
    cause: ConvergeCause,
) -> Result<(), ConvergeError> {
    let Some(row) = FleetAcpSessionRepo::get(pool, session_key).await? else {
        return Ok(());
    };
    let now = SystemClock.now_ms();

    // 1. An open turn becomes an INTERRUPTED turn, in the transcript, so a
    //    reader can tell a cut-short turn from a finished one without consulting
    //    live process state.
    if let Some(turn_id) = row.open_turn_id.clone() {
        let marker = NewFleetProviderEvent {
            event_id: format!("acp-interrupt:{session_key}:{turn_id}"),
            provider: row.provider.clone(),
            source: ainb_acp::store_writer::ACP_SOURCE.to_string(),
            session_key: Some(session_key.to_string()),
            provider_session_id: row.acp_session_id.clone(),
            observed_at: now,
            received_at: now,
            event_type: Lifecycle::TurnInterrupted.event_type().to_string(),
            raw_payload: serde_json::json!({
                "turnId": turn_id,
                "cause": cause.detail(),
            })
            .to_string(),
        };
        // A deterministic event_id makes the SECOND convergence of the same
        // turn a no-op insert rather than a duplicate marker.
        match FleetProviderEventRepo::append(pool, &marker).await {
            Ok(stored) => events.emit_transcript_order(session_key, stored.ingest_order),
            Err(error) => tracing::error!(
                %session_key,
                %error,
                "could not write the turn_interrupted marker"
            ),
        }
        let _ = FleetAcpSessionRepo::clear_open_turn(pool, session_key, now).await;
    }

    // 2. Every stuck leg gets a terminal state with an enumerated reason. The
    //    claim is the single-winner guard, so a concurrent resolver never
    //    double-writes.
    for leg in FleetMessageRepo::pending_deliveries_for_session(pool, session_key).await? {
        let mint = format!("converge:{}:{}", cause.detail(), leg.message_id);
        let fingerprint =
            if FleetMessageRepo::claim_delivery(pool, &leg.message_id, session_key, &mint).await? {
                mint
            } else if let Some(stale) = leg.fingerprint.clone() {
                // A leg that is CLAIMED but still PENDING is a resolver that
                // died (or errored) between the two writes. `claim_delivery`
                // will never hand it out again, so without this takeover the row
                // stays PENDING forever and every convergence pass skips it in
                // silence. Convergence is the only routine allowed to do this,
                // and it only ever runs when the claim's owner is provably gone
                // (boot, process exit, deadline, operator stop) or is this very
                // actor, which is single-threaded with respect to its own legs.
                tracing::warn!(
                    %session_key,
                    message_id = %leg.message_id,
                    "taking over a stale delivery claim during convergence"
                );
                stale
            } else {
                continue;
            };
        FleetMessageRepo::resolve_delivery(
            pool,
            &leg.message_id,
            session_key,
            &fingerprint,
            "UNKNOWN",
            Some(cause.detail()),
            now,
        )
        .await?;
    }

    // 3. A permission whose responder is gone is answered here, not left as a
    //    ghost row an operator can click forever.
    for id in AttentionRepo::open_ask_ids_for_session(pool, session_key).await? {
        let _ =
            AttentionRepo::mark_answered_if_open(pool, &id, "hangar-converge", cause.detail(), now)
                .await;
    }
    for id in AttentionRepo::open_approval_ids_for_session(pool, session_key).await? {
        let _ =
            AttentionRepo::mark_answered_if_open(pool, &id, "hangar-converge", cause.detail(), now)
                .await;
    }

    // 4. The scope is reusable WITHOUT a daemon restart: state back to IDLE and
    //    the stale request fingerprint cleared.
    if row.state == "ACTIVE" {
        let _ = FleetAcpSessionRepo::set_state(pool, session_key, "IDLE", now).await;
    }
    let event = NewFleetEvent {
        event_id: format!("acp-converge:{session_key}:{}:{now}", cause.detail()),
        session_key: session_key.to_string(),
        observed_at: now,
        authority: ObservationAuthority::Authoritative,
        event_type: "acp_converged".to_string(),
        payload: serde_json::json!({ "cause": cause.detail() }).to_string(),
        patch: FleetSessionPatch {
            attention_state: Some("NONE".to_string()),
            current_request_fingerprint: Some(None),
            lifecycle_state: Some("IDLE".to_string()),
            ..FleetSessionPatch::default()
        },
    };
    match FleetRepo::apply_event(pool, &event).await {
        Ok(result) if !result.duplicate => events.emit_fleet_revision(result.revision),
        Ok(_) => {}
        Err(error) => tracing::error!(%session_key, %error, "convergence fleet event failed"),
    }
    Ok(())
}

// ------------------------------------------------------------- session actor

/// The in-flight turn's bookkeeping.
struct OpenTurn {
    message_id: String,
    started: Instant,
    /// Which resume path built the context this turn runs on
    /// ([`RESUME_LOADED`] / [`RESUME_REPRIMED`]), or `None` when the session was
    /// already attached. Carried onto the delivery receipt, so "did this reply
    /// come from a session that still had its history, or from one we rebuilt"
    /// is answerable from the receipt alone (B retained).
    resume: Option<&'static str>,
    /// The turn's OWN `acp.turn` span, carried so `finish_turn` can record the
    /// outcome on it. `Span::current()` is useless there: the turn's reply
    /// arrives on a later pass of the actor's select loop, long after
    /// `start_turn`'s entered guard was dropped, so recording against whatever
    /// is current would populate nothing.
    span: tracing::Span,
}

/// A permission waiting on an operator: the adapter's blocked responder AND the
/// attention row raised for it.
///
/// They are parked TOGETHER because they must be retired together. An answer
/// that unblocks the adapter but leaves the row open reads, in the attention
/// list and in the Fleet snapshot, as a session still awaiting approval forever
/// (R8/I7's ghost row).
struct ParkedPermission {
    attention_id: String,
    request: PermissionRequest,
}

/// One hosted session: its queue, its reducer, its writer, its parked
/// permissions, and the ONE turn it may have in flight.
struct SessionActor {
    pool: Arc<AcpPool>,
    /// This actor's incarnation, so it only ever retires its OWN map entry.
    generation: u64,
    session_key: String,
    scope_key: String,
    provider: String,
    cwd: String,
    writer: StoreWriter,
    reducer: TranscriptReducer,
    acp_session_id: Option<String>,
    process: Option<Arc<ProviderProcess>>,
    updates: Option<mpsc::UnboundedReceiver<SessionNotification>>,
    permissions: Option<mpsc::UnboundedReceiver<PermissionRequest>>,
    parked: HashMap<String, ParkedPermission>,
    turn: Option<OpenTurn>,
    stats: Arc<SessionStats>,
    prompts: mpsc::Receiver<PromptJob>,
    control: mpsc::UnboundedReceiver<Control>,
    /// The re-prime prelude the NEXT prompt must carry, set by a rebuild.
    ///
    /// Prepended to the prompt text rather than sent as a prompt of its own: a
    /// standalone prelude would be a turn, and a turn is a delivery, a
    /// transcript span and a timeline reply for a message no operator sent.
    pending_prelude: Option<String>,
    /// The resume path the next turn's receipt reports.
    resume_path: Option<&'static str>,
}

/// What one turn's `session/prompt` came back with.
type TurnResult = Result<PromptResponse, AcpError>;

impl SessionActor {
    async fn run(mut self) {
        let (turn_tx, mut turn_rx) = mpsc::channel::<TurnResult>(1);
        let mut ticker = tokio::time::interval(self.pool.config.writer.flush_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            // The receivers only exist once a session is attached; an empty
            // channel stands in until then so the select arms stay uniform.
            let mut idle_updates = mpsc::unbounded_channel::<SessionNotification>().1;
            let mut idle_permissions = mpsc::unbounded_channel::<PermissionRequest>().1;
            let updates = self.updates.as_mut().unwrap_or(&mut idle_updates);
            let permissions = self.permissions.as_mut().unwrap_or(&mut idle_permissions);

            tokio::select! {
                biased;
                Some(control) = self.control.recv() => {
                    if self.handle_control(control).await {
                        break;
                    }
                }
                Some(notification) = updates.recv() => {
                    self.ingest(&notification).await;
                }
                Some(permission) = permissions.recv() => {
                    self.raise_permission(permission).await;
                }
                Some(result) = turn_rx.recv() => {
                    self.finish_turn(result).await;
                }
                // ONE prompt in flight per scope: the queue is only read while
                // no turn is open, so the bounded channel IS the FIFO.
                Some(job) = self.prompts.recv(), if self.turn.is_none() => {
                    self.start_turn(job, turn_tx.clone()).await;
                }
                _ = ticker.tick() => {
                    self.pump().await;
                }
                else => break,
            }
        }
        self.pump().await;
        self.cancel_parked("hangar-converge").await;
        // Close the queue BEFORE the last drain: after this a `submit_prompt`
        // gets `Closed` (answered `session_gone`) rather than landing a job in a
        // buffer nobody will ever read.
        self.prompts.close();
        self.drain_queue(DELIVERY_SESSION_GONE).await;
        self.pool.retire_actor(&self.session_key, self.generation).await;
    }

    /// Returns `true` when the actor should stop.
    async fn handle_control(&mut self, control: Control) -> bool {
        match control {
            Control::Cancel { cause, turn_id } => {
                // TURN-SCOPED: the deadline sweep names the turn it read as
                // overdue. If that turn has already ended, the open turn now is
                // a DIFFERENT, healthy one and cancelling it would resolve a
                // fresh delivery UNKNOWN for a deadline it never came near.
                if let Some(wanted) = turn_id {
                    let matches = self.turn.as_ref().is_some_and(|turn| turn.message_id == wanted);
                    if !matches {
                        tracing::debug!(
                            session_key = %self.session_key,
                            turn_id = %wanted,
                            "dropping a cancel for a turn that is no longer open"
                        );
                        return false;
                    }
                }
                if let (Some(process), Some(id)) =
                    (self.process.as_ref(), self.acp_session_id.as_ref())
                {
                    // ONE session's id: a shared process's other tenants keep
                    // running, which is the whole point of the multiplex.
                    let _ = process.process.cancel(id);
                }
                self.converge(cause).await;
                false
            }
            Control::Answer {
                fingerprint,
                decision,
                reply,
            } => {
                let answer = self.answer(&fingerprint, decision).await;
                let _ = reply.send(answer);
                false
            }
            Control::Evict => {
                self.close_adapter_session().await;
                // The DB row is set EVICTED by `evict_if_at_cap`; without this
                // the health pane keeps rendering the victim as IDLE and the
                // two disagree about the same session.
                self.set_state("EVICTED");
                false
            }
            Control::ProcessExited(dead) => {
                // Not ours: this actor already requeued onto a live process
                // (I6) and the event is a straggler from the corpse. Applying
                // it would kill a turn that is genuinely running.
                //
                // A DETACHED actor skips it too, and cannot thereby strand an
                // open turn: the prompt arm is guarded by `self.turn.is_none()`,
                // so nothing detaches this actor while a turn is open, and an
                // open turn therefore always still holds its process.
                if !holds_process(&dead, self.process.as_ref()) {
                    tracing::debug!(
                        session_key = %self.session_key,
                        "dropping an exit event for a process this session no longer holds"
                    );
                    return false;
                }
                self.drain_updates().await;
                self.detach();
                self.converge(ConvergeCause::AdapterExit).await;
                false
            }
            Control::Shutdown => {
                self.close_adapter_session().await;
                true
            }
        }
    }

    /// Feed one `session/update` into the reducer and commit on the cadence.
    async fn ingest(&mut self, notification: &SessionNotification) {
        let chunks = self.reducer.push(&notification.update);
        for chunk in &chunks {
            match self.writer.push(chunk).await {
                Ok(Some(high_water)) => self.wake(&high_water),
                Ok(None) => {}
                Err(error) => {
                    tracing::error!(
                        session_key = %self.session_key,
                        %error,
                        "acp transcript commit failed"
                    );
                }
            }
        }
    }

    /// The cadence leg: commit whatever is buffered so a slow turn still
    /// streams (I12), then wake subscribers with the committed high-water mark.
    async fn pump(&mut self) {
        match self.writer.tick().await {
            Ok(Some(high_water)) => self.wake(&high_water),
            Ok(None) => {}
            Err(error) => tracing::error!(
                session_key = %self.session_key,
                %error,
                "acp transcript cadence commit failed"
            ),
        }
    }

    fn wake(&self, high_water: &HighWater) {
        // The demux channels are unbounded on purpose, so committed bytes are
        // the growth signal the health pane carries in their place.
        self.stats
            .transcript_bytes
            .store(self.writer.bytes_written(), Ordering::Relaxed);
        self.pool
            .events
            .emit_transcript_order(&high_water.session_key, high_water.ingest_order);
    }

    async fn start_turn(&mut self, job: PromptJob, turn_tx: mpsc::Sender<TurnResult>) {
        // Belt and braces to the convergence drain: a job whose delivery is no
        // longer PENDING has already been answered (by convergence, by a boot
        // scan, or by an operator path), and its receipt cannot be corrected
        // because the claim is taken. Sending it would put a reply on the
        // timeline threaded to a message whose receipt says otherwise.
        if !leg_is_pending(self.pool.store.pool(), &self.session_key, &job.message_id).await {
            tracing::warn!(
                session_key = %self.session_key,
                message_id = %job.message_id,
                "skipping a queued acp prompt whose delivery is already terminal"
            );
            return;
        }
        let span = tracing::info_span!(
            "acp.turn",
            session_key = %self.session_key,
            provider = %self.provider,
            message_id = %job.message_id,
            outcome = tracing::field::Empty,
        );
        self.start_turn_inner(job, turn_tx, span.clone()).instrument(span).await;
    }

    /// The turn itself, running INSIDE the caller's `acp.turn` span.
    ///
    /// Split out for the same reason as [`AcpPool::spawn_provider_process`]: the
    /// span is attached with `.instrument`, never with `span.enter()`, because an
    /// `Entered` guard held across an `.await` is dropped on whichever worker
    /// resumed the task and corrupts the registry's span-refcount pool.
    async fn start_turn_inner(
        &mut self,
        job: PromptJob,
        turn_tx: mpsc::Sender<TurnResult>,
        span: tracing::Span,
    ) {
        let process = match self.attach_with_one_requeue(&job.message_id).await {
            Ok(process) => process,
            Err(refusal) => {
                self.resolve(&job.message_id, "FAILED", refusal).await;
                return;
            }
        };
        let Some(acp_session_id) = self.acp_session_id.clone() else {
            self.resolve(&job.message_id, "FAILED", DELIVERY_SPAWN_FAILED).await;
            return;
        };
        // I13 as a STANDING guarantee, not a spawn-time snapshot. `ensure_session`
        // early-returns for a session that is already attached and alive, so
        // without this check an adapter that flips a live session to
        // `bypassPermissions` mid conversation would keep receiving prompts in
        // that regime forever and nothing would report it.
        if process.process.mode_violated(&acp_session_id) {
            tracing::error!(
                session_key = %self.session_key,
                observed = ?process.process.observed_mode(&acp_session_id),
                "refusing to prompt a live session that changed permission regime"
            );
            self.resolve(&job.message_id, "FAILED", DELIVERY_MODE_UNPROVEN).await;
            return;
        }

        // The per-PROCESS in-flight ceiling bounds how many of this provider's
        // sessions interleave; different scopes still run concurrently.
        let Ok(permit) = Arc::clone(&process.in_flight).acquire_owned().await else {
            self.resolve(&job.message_id, "FAILED", DELIVERY_ADAPTER_EXIT).await;
            return;
        };

        let now = SystemClock.now_ms();
        if !self.record_turn(&job.message_id, now).await {
            self.resolve(&job.message_id, "FAILED", DELIVERY_TURN_UNRECORDED).await;
            return;
        }

        // A rebuilt session gets its context back on the SAME prompt: one turn,
        // one delivery, one reply.
        //
        // Taken AFTER the permit and after the turn is recorded: a leg that
        // fails either of those never prompts, and consuming the prelude first
        // would burn the rebuilt context on a turn that never happened.
        let text = match self.pending_prelude.take() {
            Some(prelude) => format!("{prelude}\n\n{}", job.text),
            None => job.text.clone(),
        };
        process.in_flight_used.fetch_add(1, Ordering::Relaxed);

        // The replay seam closes as LATE as it can, and only after everything
        // already forwarded has been swallowed by it. `session/load` replays
        // history as notifications a DIFFERENT task (the process supervisor)
        // forwards, so a replay tail can land in this session's channel while
        // the turn above was being recorded. Draining it here, with the seam
        // still on, is what stops it from being read as this turn's output on
        // the next pass of the select loop (R5), and doing it BEFORE
        // `begin_turn` is what stops an ordinary post-turn straggler from being
        // merged into the next turn's final message (I4).
        self.drain_updates().await;
        self.reducer.begin_turn();

        self.turn = Some(OpenTurn {
            message_id: job.message_id.clone(),
            started: Instant::now(),
            span: span.clone(),
            resume: self.resume_path.take(),
        });
        if let Ok(mut slot) = self.stats.turn_started_at.lock() {
            *slot = Some(Instant::now());
        }

        let adapter = Arc::clone(&process.process);
        let used = Arc::clone(&process.in_flight_used);
        tokio::spawn(async move {
            let result = adapter.prompt(&acp_session_id, &text).await;
            used.fetch_sub(1, Ordering::Relaxed);
            drop(permit);
            let _ = turn_tx.send(result).await;
        });
    }

    /// Record a turn BEFORE it exists at the adapter, answering whether it may
    /// be issued at all (I16).
    ///
    /// Both convergence paths read the persisted `open_turn_id` and neither can
    /// see a turn this write missed, so a prompt issued anyway would be a turn
    /// no sweep could expire and no exit could mark interrupted: a hung adapter
    /// would hold the scope with a PENDING leg nothing revisits. Nothing has
    /// reached the adapter when this answers `false` (the caller still holds
    /// the in-flight permit and has not spent the re-prime prelude), so the leg
    /// fails terminal rather than being requeued: a store that cannot take this
    /// write will not take it on an immediate retry either.
    ///
    /// The ACTIVE state and the `acp.turn_started` marker follow it, in that
    /// order, so the transcript never opens a turn the store has not accepted.
    async fn record_turn(&mut self, message_id: &str, now: i64) -> bool {
        if let Err(error) = FleetAcpSessionRepo::set_open_turn(
            self.pool.store.pool(),
            &self.session_key,
            message_id,
            now,
        )
        .await
        {
            tracing::error!(
                session_key = %self.session_key,
                %message_id,
                %error,
                "could not record the turn; refusing to prompt an adapter no sweep could reach"
            );
            return false;
        }
        let _ = FleetAcpSessionRepo::set_state(
            self.pool.store.pool(),
            &self.session_key,
            "ACTIVE",
            now,
        )
        .await;
        self.set_state("ACTIVE");
        if let Ok(Some(high_water)) = self
            .writer
            .lifecycle(
                Lifecycle::TurnStarted,
                serde_json::json!({ "turnId": message_id }),
            )
            .await
        {
            self.wake(&high_water);
        }
        true
    }

    /// Drain the chunks the adapter wrote just BEFORE its prompt reply.
    ///
    /// The reply and the updates ride the same pipe in order, but the upstream
    /// connection hands notifications to a handler task, so a turn that
    /// finished the instant its reply arrived would commit a transcript missing
    /// its own tail (and a final message missing its own last words). Quiesce
    /// on a short silence rather than a fixed sleep: a chatty turn drains fully,
    /// a silent one costs one grace window.
    async fn quiesce(&mut self) {
        let grace = Duration::from_millis(50);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let Some(updates) = self.updates.as_mut() else {
                return;
            };
            match tokio::time::timeout(grace, updates.recv()).await {
                Ok(Some(notification)) => self.ingest(&notification).await,
                // Silence, closed channel, or a pathological talker: either way
                // this turn is done accumulating.
                Ok(None) | Err(_) => return,
            }
            if Instant::now() >= deadline {
                return;
            }
        }
    }

    async fn finish_turn(&mut self, result: TurnResult) {
        let Some(turn) = self.turn.take() else {
            return;
        };
        self.quiesce().await;
        if let Ok(mut slot) = self.stats.turn_started_at.lock() {
            *slot = None;
        }
        // Everything the reducer still holds belongs to THIS turn.
        if let Some(chunk) = self.reducer.flush() {
            match self.writer.push(&chunk).await {
                Ok(Some(high_water)) => self.wake(&high_water),
                Ok(None) => {}
                Err(error) => tracing::error!(%error, "final transcript chunk failed to commit"),
            }
        }
        self.pump().await;

        let (marker, state, detail) = match &result {
            Ok(response) if turn_succeeded(response) => {
                (Lifecycle::TurnCompleted, "DELIVERED", None)
            }
            Ok(response) => (
                Lifecycle::TurnFailed,
                "FAILED",
                Some(format!(
                    "{DELIVERY_TURN_FAILED}; {:?}",
                    response.stop_reason
                )),
            ),
            // The request WAS issued, so the honest answer is UNKNOWN. A resend
            // here is exactly the double delivery I6 forbids.
            Err(error) => (
                Lifecycle::TurnInterrupted,
                "UNKNOWN",
                Some(format!("{DELIVERY_ADAPTER_EXIT}; {error}")),
            ),
        };
        // The receipt carries WHICH resume path built this turn's context (B
        // retained): "why did the agent not remember" is otherwise unanswerable
        // from the delivery row alone.
        let detail = match (turn.resume, detail) {
            (Some(path), Some(detail)) => Some(format!("{detail}; resume={path}")),
            (Some(path), None) => Some(format!("resume={path}")),
            (None, detail) => detail,
        };
        turn.span.record("outcome", state);
        if let Ok(Some(high_water)) = self
            .writer
            .lifecycle(
                marker,
                serde_json::json!({
                    "turnId": turn.message_id,
                    "durationMs": turn.started.elapsed().as_millis(),
                }),
            )
            .await
        {
            self.wake(&high_water);
        }

        let now = SystemClock.now_ms();
        // I4/I11: the TIMELINE gets exactly the final agent message, in the
        // RECIPIENT'S OWN scope, threaded to the prompt that caused it. The
        // whole chunk stream already went to the transcript.
        let reply = (state == "DELIVERED")
            .then(|| self.reducer.final_message().trim().to_string())
            .filter(|body| !body.is_empty())
            .map(|body| NewFleetMessage {
                id: SystemIdGen.new_ulid(),
                request_id: None,
                request_fingerprint: None,
                scope_key: self.scope_key.clone(),
                origin_message_id: Some(turn.message_id.clone()),
                sender: self.session_key.clone(),
                kind: "agent".to_string(),
                body,
                created_at: now,
            });
        // A permission still parked when the turn ENDS has no answerable
        // responder left: the adapter either died holding it (the `Err` leg) or
        // finished the turn without waiting for it. It is retired HERE, BEFORE
        // the receipt lands, because the receipt is what every reader treats as
        // "this turn is over": left to convergence, the attention list keeps
        // advertising an approval to click for a delivery the operator can
        // already see resolved (R8/I7's ghost row). Convergence still does this
        // and must, for a session with no open turn and for a daemon that died
        // mid-turn; this is that same `cancel_parked` pulled forward to the
        // first moment the turn is known to be over, not a second copy of the
        // repair. The process-exit route gets here an `EXIT_QUIESCE` later at
        // best, and only once this actor finishes the write set below.
        self.cancel_parked("hangar-turn-end").await;
        // ONE transaction (I4): the reply, its receipt and the released session
        // land together or not at all. Four separate commits left a daemon
        // death between them showing an answer with no receipt, or a receipt
        // for a session still marked mid-turn, and nothing repaired either.
        self.commit_turn_end(
            &turn.message_id,
            state,
            detail.as_deref(),
            reply.as_ref(),
            now,
        )
        .await;
        self.set_state("IDLE");
    }

    /// Commit the turn-end write set, retrying a contended writer.
    ///
    /// The retry is the same reasoning [`resolve_leg`] carries: the usual
    /// failure is a busy `SQLite` writer, and giving up on the first one leaves
    /// a turn that only convergence can finish. Unlike `resolve_leg` there is
    /// no half-written state to rescue if every attempt fails, because the
    /// whole set is one transaction.
    // `&mut self` is LOAD-BEARING for the same reason it is on
    // [`SessionActor::resolve`]: this future is spawned and must be `Send`.
    #[allow(
        clippy::needless_pass_by_ref_mut,
        reason = "an exclusive borrow is what keeps this spawned future Send"
    )]
    async fn commit_turn_end(
        &mut self,
        message_id: &str,
        state: &str,
        detail: Option<&str>,
        reply: Option<&NewFleetMessage>,
        now: i64,
    ) {
        let fingerprint = format!("acp:{}:{message_id}", self.session_key);
        let turn = TurnEnd {
            session_key: &self.session_key,
            message_id,
            fingerprint: &fingerprint,
            state,
            detail: detail.filter(|detail| !detail.is_empty()),
            session_state: "IDLE",
            reply,
            now,
        };
        for attempt in 0..3 {
            match FleetAcpSessionRepo::commit_turn_end(self.pool.store.pool(), &turn).await {
                Ok(TurnEndOutcome::Committed { reply_seq }) => {
                    if let Some(seq) = reply_seq {
                        self.pool.events.emit_message_seq(seq);
                    }
                    return;
                }
                Ok(TurnEndOutcome::AlreadyResolved) => {
                    tracing::debug!(
                        session_key = %self.session_key,
                        %message_id,
                        "acp turn ended on a leg another resolver already owns"
                    );
                    return;
                }
                Err(error) if attempt == 2 => tracing::error!(
                    session_key = %self.session_key,
                    %message_id,
                    %error,
                    "could not commit the acp turn end; convergence must finish this turn"
                ),
                Err(error) => {
                    tracing::warn!(%error, attempt = attempt + 1, "retrying an acp turn-end commit");
                    tokio::time::sleep(Duration::from_millis(50 * (attempt + 1))).await;
                }
            }
        }
    }

    /// I6: attach with EXACTLY one legal requeue.
    ///
    /// A retry is legal only while the prompt provably never reached the
    /// adapter, and [`SessionActor::ensure_session`] fails BEFORE
    /// `session/prompt` is issued by construction, so a retry here cannot
    /// double-deliver. An open breaker is terminal: retrying it is the
    /// crash-loop the breaker exists to stop.
    ///
    /// The SECOND attempt never tries `session/load`. A load failure that is
    /// not provably "unknown session" leaves `acp_session_id` in place on
    /// purpose (a rebuild would throw away adapter-side history), and nothing
    /// else ever clears it, so an adapter whose replay is slower than the spawn
    /// timeout would retry the same load on every attempt of every prompt
    /// forever. Attempt two rebuilds instead: losing adapter-side history to a
    /// re-primed context is recoverable, a permanently wedged scope is not.
    async fn attach_with_one_requeue(
        &mut self,
        message_id: &str,
    ) -> Result<Arc<ProviderProcess>, &'static str> {
        for attempt in 0..2 {
            match self.ensure_session(message_id, attempt == 0).await {
                Ok(process) => return Ok(process),
                Err(EnsureFailure::BreakerOpen) => return Err(DELIVERY_BREAKER_OPEN),
                Err(EnsureFailure::AtCapacity) => return Err(DELIVERY_PROVIDER_AT_CAPACITY),
                // I13 is terminal: an adapter that will not hold the pinned mode
                // holds it no better on a retry, and retrying would drive the
                // session a second time in a permission regime nobody chose.
                Err(EnsureFailure::ModeUnproven(error)) => {
                    tracing::error!(
                        session_key = %self.session_key,
                        %error,
                        "refusing to prompt a session whose permission mode is unproven"
                    );
                    return Err(DELIVERY_MODE_UNPROVEN);
                }
                Err(EnsureFailure::NeverSent(error)) => {
                    tracing::warn!(
                        session_key = %self.session_key,
                        attempt = attempt + 1,
                        %error,
                        "acp prompt never reached the adapter; requeueing under the I6 rule"
                    );
                    self.detach();
                }
            }
        }
        Err(DELIVERY_ADAPTER_EXIT)
    }

    /// Attach to (or build) the adapter-side session, WITHOUT ever issuing the
    /// prompt. Every failure here is provably pre-write.
    ///
    /// This is the plan's Phase 6 RESUME routine (R5), and it deliberately does
    /// not DEPEND on `session/load`:
    ///
    /// ```text
    ///   stored acp_session_id? ──no──▶ session/new ─▶ re-prime prelude
    ///        │yes                            (reprimed, or fresh when there
    ///        │                                was no id and no history)
    ///   allow_load AND adapter advertises loadSession? ──no──▶ ────┘
    ///        │yes
    ///   route + replay seam live, THEN session/load
    ///        ├─ ok ────────────────────▶ path = loaded
    ///        ├─ unknown session ───────▶ rebuild ──────────┘
    ///        ├─ mode unproven ─────────▶ SPAWN FAILS (I13)
    ///        └─ anything else ─────────▶ spawn failure, one legal requeue
    /// ```
    ///
    /// `allow_load` is false on the retry (see
    /// [`SessionActor::attach_with_one_requeue`]): it is the only thing that
    /// stops an unclassifiable load failure from being retried forever, because
    /// no path clears `acp_session_id` and neither convergence nor teardown
    /// touches it.
    ///
    /// `message_id` is the prompt this attach is for: the re-prime corpus stops
    /// BELOW it, because it is about to be sent as the prompt itself and a body
    /// quoted inside the fenced context AND asked as the question reads as the
    /// operator saying it twice.
    async fn ensure_session(
        &mut self,
        message_id: &str,
        allow_load: bool,
    ) -> Result<Arc<ProviderProcess>, EnsureFailure> {
        if let Some(process) = self.process.clone() {
            if process.process.is_alive() && self.acp_session_id.is_some() {
                return Ok(process);
            }
            self.detach();
        }
        if self.pool.breaker_open(&self.provider) {
            return Err(EnsureFailure::BreakerOpen);
        }
        let process = self
            .pool
            .provider_process(&self.provider)
            .await
            .map_err(|error| EnsureFailure::NeverSent(error.to_string()))?;

        // Held until this function returns, so the idle sweep cannot mistake a
        // process whose first tenant is mid-`session/new` for an idle one and
        // SIGKILL a healthy adapter.
        process.attaching.fetch_add(1, Ordering::Relaxed);
        let _attaching = AttachGuard(Arc::clone(&process));

        // The session cap is enforced HERE, where a new tenant arrives: evict
        // the least recently used idle session (`session/close`) and keep the
        // process warm for everyone else, or refuse when nothing can be freed.
        if !self.pool.make_room(&process, &self.session_key).await {
            return Err(EnsureFailure::AtCapacity);
        }

        // PROBED per spawn, never persisted (B-defect 5): `can_load` on disk
        // would outlive the adapter version that justified it.
        let stored = stored_acp_session_id(self.pool.store.pool(), &self.session_key).await;
        // Kept, because "the adapter forgot our session" and "we never had one"
        // are the same code path and must NOT be the same receipt.
        let had_stored = stored.is_some();
        let mut path = None;
        if let Some(stored) = stored.filter(|_| allow_load && process.process.supports_load()) {
            if self.try_load(&process, &stored).await? {
                path = Some(RESUME_LOADED);
            }
        }
        if path.is_none() {
            self.rebuild(&process, message_id).await?;
            // A rebuild is only a RESUME when something was actually resumed:
            // an id that was lost, or history the prelude carries back in.
            path = Some(if had_stored || self.pending_prelude.is_some() {
                RESUME_REPRIMED
            } else {
                RESUME_FRESH
            });
        }
        let Some(acp_session_id) = self.acp_session_id.clone() else {
            return Err(EnsureFailure::NeverSent(
                "the adapter session vanished during attach".to_string(),
            ));
        };
        if !process.process.is_alive() {
            return Err(EnsureFailure::NeverSent(
                "adapter transport closed before the prompt was issued".to_string(),
            ));
        }

        // I5: the STABLE `session_key` keeps its receipts, its scope and its
        // transcript; only the adapter's mutable id is written here.
        let _ = FleetAcpSessionRepo::set_acp_session_id(
            self.pool.store.pool(),
            &self.session_key,
            Some(&acp_session_id),
        )
        .await;
        if let Some(version) = process.process.info().version.clone() {
            let _ = FleetAcpSessionRepo::set_provider_version(
                self.pool.store.pool(),
                &self.session_key,
                &version,
            )
            .await;
        }
        // A fresh session rebuilt nothing, so it fingerprints nothing: no
        // marker, and a receipt whose detail stays NULL.
        if let Some(path) = path.filter(|path| *path != RESUME_FRESH) {
            self.record_context_rebuilt(path, &acp_session_id).await;
            self.resume_path = Some(path);
        }
        Ok(process)
    }

    /// Attempt `session/load`. `Ok(true)` means the session was resumed;
    /// `Ok(false)` means the adapter has never heard of it and the caller must
    /// rebuild. An error is a spawn failure.
    async fn try_load(
        &mut self,
        process: &Arc<ProviderProcess>,
        acp_session_id: &str,
    ) -> Result<bool, EnsureFailure> {
        // HANDLER LIVE FIRST. `session/load` replays the whole conversation as
        // `session/update` notifications AHEAD of its own reply, so the route
        // exists before the request is issued. The plan calls a handler
        // registered after the call the port's single most likely bug.
        self.attach_channels(process, acp_session_id);
        // ...and the replay must write NOTHING. Those rows are already in the
        // transcript from the turns that produced them, and rebuilding
        // `final_message` from an old turn's text would put a stale reply on the
        // chat timeline as if it had just arrived ("no client-side transcript
        // replay for session/load resume").
        self.reducer.set_replaying(true);
        // Re-declared on load exactly like the static config options: adapter
        // state does not survive a load, so a resumed copilot would otherwise
        // come back with no fleet tools and no error saying so.
        let mcp_servers =
            crate::copilot::session_mcp_servers(self.pool.store.pool(), &self.scope_key).await;
        let loaded = process
            .process
            .load_session_with_mcp(
                acp_session_id,
                std::path::Path::new(&self.cwd),
                &mcp_servers,
            )
            .await;
        // Drained with the seam STILL ON, and LEFT on: the notifications sit in
        // the actor's channel until something reads them, and a replay tail
        // that outran this window would otherwise be classified as live output.
        //
        // This quiesce is therefore an optimisation, NOT the guarantee. The
        // guarantee is in `start_turn`, which drains again with the seam still
        // on immediately before it closes it and prompts: the process
        // supervisor forwards on its own task, so how much of the replay has
        // reached this channel by now is not something a timer can answer.
        self.quiesce().await;

        match loaded {
            Ok(()) => Ok(true),
            // I13, and the correction the gate re-run of 2026-08-06 forced: a
            // loaded session whose mode we cannot prove fails the SPAWN. It is
            // not a rebuild case, because the adapter answered and the session
            // exists; it is the permission regime that is wrong.
            Err(error @ AcpError::ModeMismatch { .. }) => {
                self.detach();
                Err(EnsureFailure::ModeUnproven(error.to_string()))
            }
            Err(error) if error.load_means_rebuild() => {
                tracing::info!(
                    session_key = %self.session_key,
                    provider = %self.provider,
                    %acp_session_id,
                    %error,
                    "the adapter no longer knows this session; rebuilding its context"
                );
                self.detach();
                Ok(false)
            }
            Err(error) => {
                self.detach();
                Err(EnsureFailure::NeverSent(error.to_string()))
            }
        }
    }

    /// `session/new` under the SAME `session_key` (I5), plus the re-prime
    /// prelude the next prompt carries.
    async fn rebuild(
        &mut self,
        process: &Arc<ProviderProcess>,
        message_id: &str,
    ) -> Result<(), EnsureFailure> {
        // The copilot's session is the only one that gets fleet tools; every
        // other scope resolves to an empty list. See `copilot::session_mcp_servers`.
        let mcp_servers =
            crate::copilot::session_mcp_servers(self.pool.store.pool(), &self.scope_key).await;
        let acp_session_id = process
            .process
            .new_session_with_mcp(std::path::Path::new(&self.cwd), &mcp_servers)
            .await
            .map_err(|error| match error {
                error @ AcpError::ModeMismatch { .. } => {
                    EnsureFailure::ModeUnproven(error.to_string())
                }
                other => EnsureFailure::NeverSent(other.to_string()),
            })?;
        self.attach_channels(process, &acp_session_id);
        let pool = self.pool.store.pool().clone();
        self.pending_prelude = render_resume_prelude(&pool, &self.session_key, message_id).await;
        Ok(())
    }

    /// Register this session's demux route and rebind everything keyed on the
    /// adapter's id. Called BEFORE `session/load` on purpose (see
    /// [`SessionActor::try_load`]).
    fn attach_channels(&mut self, process: &Arc<ProviderProcess>, acp_session_id: &str) {
        let (update_tx, update_rx) = mpsc::unbounded_channel();
        let (permission_tx, permission_rx) = mpsc::unbounded_channel();
        if let Ok(mut routes) = process.routes.lock() {
            routes.insert(
                acp_session_id.to_string(),
                SessionRoute {
                    session_key: self.session_key.clone(),
                    updates: update_tx,
                    permissions: permission_tx,
                },
            );
        }
        self.updates = Some(update_rx);
        self.permissions = Some(permission_rx);
        self.reducer = TranscriptReducer::new(acp_session_id.to_string());
        self.writer.set_acp_session_id(Some(acp_session_id.to_string()));
        // Set NOW, not at the end of the attach, so `detach` can unwind a
        // half-built attach (a load that failed) instead of leaking the route.
        self.acp_session_id = Some(acp_session_id.to_string());
        self.process = Some(Arc::clone(process));
    }

    /// The `acp.context_rebuilt {mode}` marker both paths write.
    ///
    /// A transcript reader can then tell a session that kept its own history
    /// from one the daemon reconstructed, which is the difference between "the
    /// agent forgot" and "the agent was never told".
    async fn record_context_rebuilt(&mut self, path: &'static str, acp_session_id: &str) {
        match self
            .writer
            .lifecycle(
                Lifecycle::ContextRebuilt,
                serde_json::json!({
                    "mode": path,
                    "acpSessionId": acp_session_id,
                }),
            )
            .await
        {
            Ok(Some(high_water)) => self.wake(&high_water),
            Ok(None) => {}
            Err(error) => tracing::error!(
                session_key = %self.session_key,
                %error,
                "could not write the context_rebuilt marker"
            ),
        }
    }

    /// Forget the adapter-side session without touching the store: the stable
    /// `session_key` survives (I5), only the adapter's id is transient.
    fn detach(&mut self) {
        if let (Some(process), Some(id)) = (self.process.as_ref(), self.acp_session_id.as_ref()) {
            if let Ok(mut routes) = process.routes.lock() {
                routes.remove(id);
            }
        }
        self.process = None;
        self.acp_session_id = None;
        self.updates = None;
        self.permissions = None;
        // The prelude belongs to the adapter session that was just torn down.
        // Left behind, a rebuild that then failed its liveness check would leak
        // it onto a later successfully LOADED session, duplicating context on a
        // session that lost none and contradicting its own `resume=loaded`.
        self.pending_prelude = None;
    }

    async fn close_adapter_session(&mut self) {
        if let (Some(process), Some(id)) = (self.process.clone(), self.acp_session_id.clone()) {
            let _ = process.process.close_session(&id).await;
        }
        self.drain_updates().await;
        self.detach();
    }

    /// Commit everything the adapter wrote before it went away.
    ///
    /// The demux channel is unbounded and the actor's select is CONTROL-biased,
    /// so an eviction or a process exit is handled with the adapter's last
    /// chunks still queued on it. [`SessionActor::detach`] drops that receiver,
    /// so whatever is not drained here is output the adapter genuinely produced
    /// and the transcript would never show. The reducer is flushed for the same
    /// reason: a re-attach builds a fresh one, and only `finish_turn` (which a
    /// dead turn never reaches) would otherwise commit the pending text.
    async fn drain_updates(&mut self) {
        while let Some(notification) =
            self.updates.as_mut().and_then(|updates| updates.try_recv().ok())
        {
            self.ingest(&notification).await;
        }
        if let Some(chunk) = self.reducer.flush() {
            match self.writer.push(&chunk).await {
                Ok(Some(high_water)) => self.wake(&high_water),
                Ok(None) => {}
                Err(error) => tracing::error!(
                    session_key = %self.session_key,
                    %error,
                    "the adapter's last transcript chunk failed to commit"
                ),
            }
        }
        self.pump().await;
    }

    /// Answer every prompt still sitting in the FIFO, terminal, with `detail`.
    ///
    /// A queued prompt provably never reached the adapter, so FAILED is the
    /// honest state and an operator can resend under a fresh `request_id`.
    /// Draining is what makes I16's "queued prompts have defined outcomes"
    /// true: leaving them in the channel would let the actor pick one up AFTER
    /// convergence had already resolved its delivery, opening a real turn for a
    /// leg whose receipt is terminal and can never be corrected.
    async fn drain_queue(&mut self, detail: &str) {
        loop {
            match self.prompts.try_recv() {
                Ok(job) => {
                    tracing::warn!(
                        session_key = %self.session_key,
                        message_id = %job.message_id,
                        %detail,
                        "resolving a queued acp prompt that will never be sent"
                    );
                    self.resolve(&job.message_id, "FAILED", detail).await;
                }
                Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                    return;
                }
            }
        }
    }

    async fn converge(&mut self, cause: ConvergeCause) {
        self.turn = None;
        if let Ok(mut slot) = self.stats.turn_started_at.lock() {
            *slot = None;
        }
        self.cancel_parked("hangar-converge").await;
        // BEFORE the shared routine, so each drained prompt carries its own
        // enumerated cause instead of being swept up as an anonymous stuck leg.
        self.drain_queue(cause.detail()).await;
        if let Err(error) = converge_dirty_session(
            self.pool.store.pool(),
            &self.pool.events,
            &self.session_key,
            cause,
        )
        .await
        {
            tracing::error!(session_key = %self.session_key, %error, "convergence failed");
        }
        self.set_state("IDLE");
    }

    /// Raise the attention row R8 exists for, and PARK the responder.
    async fn raise_permission(&mut self, permission: PermissionRequest) {
        // No open turn means nothing is left to answer this. The request was
        // already in the actor's channel when `finish_turn` retired the parked
        // set, so raising it now would insert an attention row AFTER the
        // receipt committed and the session went IDLE: the same ghost row
        // `finish_turn` closes, through a narrower window. Refuse it at the
        // door instead of parking a responder nobody will ever reach.
        if self.turn.is_none() {
            let _ = permission.answer_cancelled();
            return;
        }
        let fingerprint = permission_fingerprint(&self.session_key, &permission);
        let payload = serde_json::json!({
            "kind": "acp_permission",
            "sessionKey": self.session_key,
            "acpSessionId": permission.session_id(),
            "requestFingerprint": fingerprint,
            "rpcId": permission.rpc_id(),
            "options": permission.options_wire(),
            "toolCall": permission.request.tool_call,
        });
        let chunk = self.reducer.permission_chunk(payload.clone());
        match self.writer.push(&chunk).await {
            Ok(Some(high_water)) => self.wake(&high_water),
            Ok(None) => {}
            Err(error) => tracing::error!(%error, "permission transcript row failed to commit"),
        }
        self.pump().await;

        let now = SystemClock.now_ms();
        let attention_id = SystemIdGen.new_ulid();
        let attention = ainb_hangar_store::repo::attention::NewAttention {
            id: attention_id.clone(),
            session_id: self.session_key.clone(),
            cwd: self.cwd.clone(),
            workspace_id: None,
            kind: ainb_hangar_store::repo::attention::AttentionKind::Approval,
            payload: payload.to_string(),
            degraded: false,
            created_at: now,
            raise_transcript: None,
            channels: ainb_hangar_core::channel::ChannelSet::default(),
        };
        if let Err(error) = AttentionRepo::insert(self.pool.store.pool(), &attention).await {
            tracing::error!(%error, "could not raise the acp permission attention row");
        }
        // The fingerprint on the SESSION row is what `fleet/action`'s staleness
        // machinery validates the answer against, so a stale UI cannot answer a
        // permission that has already moved on.
        let event = NewFleetEvent {
            event_id: format!("acp-permission:{}:{fingerprint}", self.session_key),
            session_key: self.session_key.clone(),
            observed_at: now,
            authority: ObservationAuthority::Authoritative,
            event_type: "acp_permission_requested".to_string(),
            payload: payload.to_string(),
            patch: FleetSessionPatch {
                attention_state: Some("APPROVAL".to_string()),
                current_request_fingerprint: Some(Some(fingerprint.clone())),
                ..FleetSessionPatch::default()
            },
        };
        match FleetRepo::apply_event(self.pool.store.pool(), &event).await {
            Ok(result) if !result.duplicate => {
                self.pool.events.emit_fleet_revision(result.revision);
            }
            Ok(_) => {}
            Err(error) => tracing::error!(%error, "acp permission fleet event failed"),
        }
        self.parked.insert(
            fingerprint,
            ParkedPermission {
                attention_id,
                request: permission,
            },
        );
        self.stats.pending_permissions.store(
            u32::try_from(self.parked.len()).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );
    }

    async fn answer(
        &mut self,
        fingerprint: &str,
        decision: PermissionDecision,
    ) -> PermissionAnswer {
        let chosen = match self.parked.get(fingerprint) {
            Some(parked) => choose_option(&parked.request.request.options, &decision),
            None => return PermissionAnswer::NotWaiting,
        };
        // Refused with the responder STILL PARKED. Taking it out of the map
        // would spend the adapter's one reply slot on an answer we are not
        // sending: the adapter would stay blocked and its attention row would
        // outlive every path that could close it but convergence.
        if chosen.is_none() && decision != PermissionDecision::Deny {
            return PermissionAnswer::UnknownOption;
        }
        let Some(parked) = self.parked.remove(fingerprint) else {
            return PermissionAnswer::NotWaiting;
        };
        let attention_id = parked.attention_id;
        let permission = parked.request;
        self.stats.pending_permissions.store(
            u32::try_from(self.parked.len()).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );
        let result = match chosen {
            Some(option_id) => permission.answer_selected(&option_id).map(|()| option_id),
            // Deny against an adapter that offered no reject option:
            // `Cancelled` IS the refusal ACP defines for that case.
            None => permission.answer_cancelled().map(|()| "cancelled".to_string()),
        };
        match result {
            Ok(option) => {
                self.retire_attention(&attention_id, "operator", &option).await;
                PermissionAnswer::Delivered(option)
            }
            Err(AcpError::InvalidParams { .. }) => PermissionAnswer::UnknownOption,
            Err(error) => {
                tracing::warn!(session_key = %self.session_key, %error, "permission answer failed");
                // The responder is spent either way, so the row must not
                // outlive it: a failed hand-off still closes the ask.
                self.retire_attention(&attention_id, "operator", "failed").await;
                PermissionAnswer::NotWaiting
            }
        }
    }

    /// Close one answered ask and re-point the Fleet session at whatever is
    /// still waiting, or back to NONE when nothing is.
    ///
    /// Without this an ANSWERED permission stays `open` in the attention list
    /// and the snapshot keeps rendering `attention_state = APPROVAL` with a
    /// stale `current_request_fingerprint` forever: only a crash, a deadline or
    /// an Interrupt (all of which run convergence) would ever clear it.
    ///
    /// A session can be waiting on SEVERAL asks at once (an adapter that runs
    /// parallel tool calls raises one `session/request_permission` each) and
    /// the row carries exactly ONE fingerprint. Leaving it on the ask just
    /// answered would advertise a decision the operator has already made; the
    /// oldest ask still parked is the honest value. Which permissions are LIVE
    /// is answered by [`SessionActor::parked`], never by this single slot.
    // `&mut self` is LOAD-BEARING, not accidental: this future is spawned, so it
    // must be `Send`, and a shared `&SessionActor` held across an await would
    // additionally require `SessionActor: Sync`. It never can be — it parks
    // adapter `Responder`s, whose boxed callback is `Send` but not `Sync`.
    #[allow(
        clippy::needless_pass_by_ref_mut,
        reason = "an exclusive borrow is what keeps this spawned future Send"
    )]
    async fn retire_attention(&mut self, attention_id: &str, answered_by: &str, answer: &str) {
        let now = SystemClock.now_ms();
        if let Err(error) = AttentionRepo::mark_answered_if_open(
            self.pool.store.pool(),
            attention_id,
            answered_by,
            answer,
            now,
        )
        .await
        {
            tracing::error!(
                session_key = %self.session_key,
                %error,
                "could not close the answered acp permission attention row"
            );
        }
        // Raise order, so a reader sees the ask that has been waiting longest:
        // the attention id is a ULID, which sorts by mint time.
        let (attention_state, next_fingerprint) = self
            .parked
            .iter()
            .min_by(|left, right| left.1.attention_id.cmp(&right.1.attention_id))
            .map_or(("NONE", None), |(fingerprint, _)| {
                ("APPROVAL", Some(fingerprint.clone()))
            });
        let event = NewFleetEvent {
            event_id: format!(
                "acp-permission-answered:{}:{attention_id}",
                self.session_key
            ),
            session_key: self.session_key.clone(),
            observed_at: now,
            authority: ObservationAuthority::Authoritative,
            event_type: "acp_permission_answered".to_string(),
            payload: serde_json::json!({ "attentionId": attention_id, "answer": answer })
                .to_string(),
            patch: FleetSessionPatch {
                attention_state: Some(attention_state.to_string()),
                current_request_fingerprint: Some(next_fingerprint),
                ..FleetSessionPatch::default()
            },
        };
        match FleetRepo::apply_event(self.pool.store.pool(), &event).await {
            Ok(result) if !result.duplicate => {
                self.pool.events.emit_fleet_revision(result.revision);
            }
            Ok(_) => {}
            Err(error) => tracing::error!(
                session_key = %self.session_key,
                %error,
                "acp permission answered fleet event failed"
            ),
        }
    }

    /// Answer every parked permission `Cancelled` and close its row.
    /// A permission whose adapter is gone must never survive as a ghost row.
    ///
    /// `answered_by` is the caller's own name, not a constant: turn end and
    /// convergence both retire parked permissions, and a row stamped
    /// `hangar-converge` by the turn-end path would report a repair that never
    /// ran, over-counting adapter crashes in any audit over that column.
    async fn cancel_parked(&mut self, answered_by: &str) {
        let parked: Vec<ParkedPermission> = self.parked.drain().map(|(_, value)| value).collect();
        self.stats.pending_permissions.store(0, Ordering::Relaxed);
        for permission in parked {
            let attention_id = permission.attention_id;
            let _ = permission.request.answer_cancelled();
            self.retire_attention(&attention_id, answered_by, "cancelled").await;
        }
    }

    // `&mut self` is LOAD-BEARING, not accidental: this future is spawned, so it
    // must be `Send`, and a shared `&SessionActor` held across an await would
    // additionally require `SessionActor: Sync`. It never can be — it parks
    // adapter `Responder`s, whose boxed callback is `Send` but not `Sync`.
    #[allow(
        clippy::needless_pass_by_ref_mut,
        reason = "an exclusive borrow is what keeps this spawned future Send"
    )]
    async fn resolve(&mut self, message_id: &str, state: &str, detail: &str) {
        let pool = self.pool.store.pool().clone();
        resolve_leg(&pool, &self.session_key, message_id, state, detail).await;
    }

    fn set_state(&self, state: &str) {
        if let Ok(mut slot) = self.stats.state.lock() {
            *slot = state.to_string();
        }
    }
}

/// Why `ensure_session` gave up, and therefore whether a requeue is legal.
#[derive(Debug)]
enum EnsureFailure {
    /// The provider's breaker is open: terminal, no retry.
    BreakerOpen,
    /// The provider process is full and nothing can be evicted: terminal, no
    /// retry. A second attempt would find the same busy tenants, and requeueing
    /// past the cap is the overshoot the cap exists to prevent.
    AtCapacity,
    /// The pinned permission mode could not be proven (I13): terminal, no
    /// retry. Distinct from [`EnsureFailure::NeverSent`] because a retry would
    /// re-attach a session whose permission regime is not the configured one.
    ModeUnproven(String),
    /// The prompt provably never reached the adapter: ONE requeue is legal.
    NeverSent(String),
}

impl AcpPool {
    /// Make room for one arriving session, or answer `false` when the cap
    /// cannot be honoured.
    ///
    /// The cap is a CEILING, not a hint: at the cap with every tenant mid-turn
    /// there is nothing to evict, and the honest answer is to refuse the
    /// arrival with [`DELIVERY_PROVIDER_AT_CAPACITY`]. Attaching anyway (what
    /// the warn-and-continue path did) puts the process over the maximum an
    /// operator configured, and does it silently and repeatedly, since every
    /// later arrival takes the same branch.
    ///
    /// Occupancy counts the tenants that still HOLD a slot plus everything
    /// still attaching, because the arrival's own reservation is already in
    /// `attaching` and so is every concurrent one. A session already marked
    /// `EVICTED` is on its way out and is counted as gone: eviction only frees
    /// the route asynchronously (the victim's actor closes its own adapter
    /// session), so counting it as a tenant would make the SECOND of two
    /// concurrent arrivals refuse a slot the first had already freed for it.
    async fn make_room(&self, process: &Arc<ProviderProcess>, incoming: &str) -> bool {
        let _one_at_a_time = process.evicting.lock().await;
        let cap = self.config.max_sessions_per_provider;
        let hosted: Vec<String> = process
            .routes
            .lock()
            .map(|routes| routes.values().map(|route| route.session_key.clone()).collect())
            .unwrap_or_default();

        // LRU by the store's own `last_active_at`, so the choice survives a
        // daemon that has only just adopted these sessions.
        let mut tenants = 0_usize;
        let mut candidates: Vec<FleetAcpSessionRow> = Vec::new();
        for session_key in hosted {
            if session_key == incoming {
                continue;
            }
            let Ok(Some(row)) = FleetAcpSessionRepo::get(self.store.pool(), &session_key).await
            else {
                // A row we cannot read is a tenant we cannot evict; counting it
                // is the direction that respects the cap.
                tenants += 1;
                continue;
            };
            if row.state == "EVICTED" {
                continue;
            }
            tenants += 1;
            if row.open_turn_id.is_none() {
                candidates.push(row);
            }
        }
        let attaching = process.attaching.load(Ordering::Relaxed) as usize;
        let Some(over) = (tenants + attaching).checked_sub(cap).filter(|over| *over > 0) else {
            return true;
        };
        if candidates.len() < over {
            tracing::warn!(
                provider = %process.provider,
                %incoming,
                cap,
                idle = candidates.len(),
                "acp provider is at its session cap with nothing idle to evict; refusing the arrival"
            );
            return false;
        }
        candidates.sort_by_key(|row| row.last_active_at);
        for victim in candidates.into_iter().take(over) {
            tracing::info!(
                provider = %process.provider,
                session_key = %victim.session_key,
                "evicting the least recently used idle acp session; the process stays warm"
            );
            let control = {
                let sessions = self.sessions.lock().await;
                sessions.get(&victim.session_key).map(|handle| handle.control.clone())
            };
            if let Some(control) = control {
                let _ = control.send(Control::Evict);
            }
            let _ = FleetAcpSessionRepo::set_state(
                self.store.pool(),
                &victim.session_key,
                "EVICTED",
                SystemClock.now_ms(),
            )
            .await;
            self.evicted_total.fetch_add(1, Ordering::Relaxed);
        }
        true
    }
}

/// Is this message's leg to `session_key` still awaiting an outcome?
///
/// A free function for the same reason [`resolve_leg`] is: an `&SessionActor`
/// held across an await would make the actor's future require `Sync`, and it
/// owns parked `Responder`s that are `Send` only.
///
/// A store fault answers `true`: refusing to send on an unreadable store would
/// turn a transient `SQLite` error into a silently dropped prompt, whereas
/// sending it risks at worst a duplicate the claim then refuses.
async fn leg_is_pending(pool: &SqlitePool, session_key: &str, message_id: &str) -> bool {
    match FleetMessageRepo::deliveries_for_message(pool, message_id).await {
        Ok(legs) => legs
            .iter()
            .find(|leg| leg.session_key == session_key)
            .is_none_or(|leg| leg.state == "PENDING"),
        Err(error) => {
            tracing::error!(
                %session_key,
                %message_id,
                %error,
                "could not re-read the delivery leg before starting a turn"
            );
            true
        }
    }
}

/// The adapter id the store remembers for this stable `session_key`.
///
/// A free function for the same reason [`resolve_leg`] is: an `&SessionActor`
/// held across an await would make the actor's future require `Sync`, and it
/// owns parked `Responder`s that are `Send` only.
async fn stored_acp_session_id(pool: &SqlitePool, session_key: &str) -> Option<String> {
    match FleetAcpSessionRepo::get(pool, session_key).await {
        Ok(row) => row.and_then(|row| row.acp_session_id),
        Err(error) => {
            tracing::warn!(
                %session_key,
                %error,
                "could not read the stored adapter session id; rebuilding instead of loading"
            );
            None
        }
    }
}

/// Render the resume prelude from the DELIVERY JOIN corpus.
///
/// The corpus is `list_for_session`, not a raw scope filter, so a prompt that
/// reached this session as one recipient of a BROADCAST is in the rebuilt
/// context too. `None` when there is nothing to replay: a session that has
/// never spoken gets its prompt unadorned rather than an empty fence that only
/// tells the agent to distrust a context it does not have.
///
/// `message_id` is the prompt this rebuild is for, and it bounds the corpus by
/// SEQ rather than being filtered out of it by id. A delivery row exists from
/// the moment a message is queued, so an id filter alone still hands the agent
/// every message queued BEHIND this one as if it were earlier history, and a
/// burst deeper than [`ainb_acp::reprime::REPRIME_ROWS`] pushes the in-flight
/// prompt out of the window entirely. An id that resolves to no row leaves the
/// bound unknowable, so the rebuild carries no prelude rather than a wrong one.
///
/// Free for the same `Sync` reason as [`resolve_leg`], and PUBLIC so the
/// plan's "byte-identical prelude for a fixed corpus" is assertable against the
/// real delivery-join query rather than against a hand-built row list.
pub async fn render_resume_prelude(
    pool: &SqlitePool,
    session_key: &str,
    message_id: &str,
) -> Option<String> {
    let before_seq = match FleetMessageRepo::seq_for_id(pool, message_id).await {
        Ok(Some(seq)) => seq,
        Ok(None) => {
            tracing::error!(
                %session_key,
                %message_id,
                "the in-flight prompt has no message row; rebuilding with no prior context"
            );
            return None;
        }
        Err(error) => {
            tracing::error!(
                %session_key,
                %message_id,
                %error,
                "could not resolve the in-flight prompt's cursor; rebuilding with no prior context"
            );
            return None;
        }
    };
    let rows = FleetMessageRepo::list_for_session(
        pool,
        session_key,
        before_seq,
        i64::try_from(ainb_acp::reprime::REPRIME_ROWS).unwrap_or(i64::MAX),
    )
    .await
    .unwrap_or_else(|error| {
        tracing::error!(
            %session_key,
            %error,
            "could not read the re-prime corpus; rebuilding with no prior context"
        );
        Vec::new()
    });
    let corpus: Vec<ainb_acp::reprime::CorpusRow> = rows
        .into_iter()
        .map(|row| ainb_acp::reprime::CorpusRow {
            sender: row.sender,
            kind: row.kind,
            body: row.body,
        })
        .collect();
    (!corpus.is_empty()).then(|| ainb_acp::reprime::render_prelude(&corpus))
}

/// Claim and resolve ONE delivery leg terminal.
///
/// A free function, not a method: an `&SessionActor` held across an await would
/// require the actor to be `Sync`, and it deliberately is not (it owns parked
/// `Responder`s, which are `Send` only).
async fn resolve_leg(
    pool: &SqlitePool,
    session_key: &str,
    message_id: &str,
    state: &str,
    detail: &str,
) {
    let fingerprint = format!("acp:{session_key}:{message_id}");
    match FleetMessageRepo::claim_delivery(pool, message_id, session_key, &fingerprint).await {
        Ok(true) => {
            let detail = (!detail.is_empty()).then_some(detail);
            // The claim is already taken at this point, so a resolve that never
            // lands leaves a leg no other claimer can rescue until a convergence
            // pass takes the claim over. Retry before accepting that cost: the
            // usual failure is a contended SQLite writer exhausting its
            // busy_timeout, which the next attempt usually wins.
            for attempt in 0..3 {
                match FleetMessageRepo::resolve_delivery(
                    pool,
                    message_id,
                    session_key,
                    &fingerprint,
                    state,
                    detail,
                    SystemClock.now_ms(),
                )
                .await
                {
                    Ok(_) => break,
                    Err(error) if attempt == 2 => tracing::error!(
                        %session_key,
                        %message_id,
                        %error,
                        "could not resolve the acp delivery leg; convergence must take the claim over"
                    ),
                    Err(error) => {
                        tracing::warn!(%error, attempt = attempt + 1, "retrying an acp leg resolve");
                        tokio::time::sleep(Duration::from_millis(50 * (attempt + 1))).await;
                    }
                }
            }
        }
        Ok(false) => tracing::debug!(
            %session_key,
            %message_id,
            "acp delivery leg was already resolved"
        ),
        Err(error) => tracing::error!(%error, "could not claim the acp delivery leg"),
    }
}

/// A turn that ended on the agent's own terms, as opposed to a refusal or a
/// cancellation.
const fn turn_succeeded(response: &PromptResponse) -> bool {
    matches!(
        response.stop_reason,
        StopReason::EndTurn | StopReason::MaxTokens | StopReason::MaxTurnRequests
    )
}

/// Hand one `session/update` to the session that owns its ACP session id.
///
/// NEVER cross-attributed: a chunk we cannot place belongs to nobody, and
/// guessing would put one tenant's output in another tenant's transcript.
fn forward_update(entry: &ProviderProcess, notification: SessionNotification) {
    let id = notification.session_id.to_string();
    let route = entry.routes.lock().ok().and_then(|routes| routes.get(&id).cloned());
    let Some(route) = route else {
        tracing::warn!(
            provider = %entry.provider,
            acp_session_id = %id,
            "dropped a session/update for an unknown acp session id"
        );
        return;
    };
    let _ = route.updates.send(notification);
}

/// Hand one `session/request_permission` to the session that owns its ACP
/// session id, or answer it here so the adapter is not left blocked.
fn forward_permission(entry: &ProviderProcess, permission: PermissionRequest) {
    let id = permission.session_id();
    let route = entry.routes.lock().ok().and_then(|routes| routes.get(&id).cloned());
    let Some(route) = route else {
        tracing::warn!(
            provider = %entry.provider,
            acp_session_id = %id,
            "cancelling a permission for an unknown acp session id"
        );
        // Answer rather than drop on the floor: the adapter is BLOCKED on this
        // request.
        let _ = permission.answer_cancelled();
        return;
    };
    let _ = route.permissions.send(permission);
}

/// The option id an operator's decision names, or `None` when the adapter
/// offered nothing that answers it.
///
/// Matched on the KIND enum, never on a debug rendering: a substring test for
/// `"allow"` reads `AllowOnce` and a hypothetical `DisallowAlways` the same
/// way. There is deliberately NO positional fallback: `options.first()` made
/// `Approve` select whatever came first, which on an adapter offering only
/// reject-flavoured options is a REJECT answered as an approval, and on an
/// adapter offering a kind this build has never heard of is a coin toss on a
/// destructive tool call. An option this function cannot classify is not
/// selected, and the caller refuses instead.
fn choose_option(
    options: &[agent_client_protocol::schema::v1::PermissionOption],
    decision: &PermissionDecision,
) -> Option<String> {
    use agent_client_protocol::schema::v1::PermissionOptionKind;

    options
        .iter()
        .find(|option| match decision {
            PermissionDecision::Option(id) => option.option_id.to_string() == *id,
            PermissionDecision::Approve => matches!(
                option.kind,
                PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
            ),
            PermissionDecision::Deny => matches!(
                option.kind,
                PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways
            ),
        })
        .map(|option| option.option_id.to_string())
}

/// Is a [`Control::ProcessExited`] about the process the actor still holds?
///
/// A `Weak` that no longer upgrades cannot be it: holding the process would
/// keep it alive. Generic so the three cases are testable without spawning a
/// real adapter.
fn holds_process<T>(dead: &Weak<T>, current: Option<&Arc<T>>) -> bool {
    match (dead.upgrade(), current) {
        (Some(dead), Some(current)) => Arc::ptr_eq(&dead, current),
        _ => false,
    }
}

/// Drop a session's map entry ONLY while the retiring actor still owns it.
///
/// Teardown then respawn is the normal path through convergence and resume, so
/// a predecessor's retirement routinely races its successor's registration. An
/// unguarded remove there evicts the LIVE actor's entry and leaves a running
/// actor nothing can route to. Generic so the race is testable without an
/// adapter process.
fn retire_if_current<H>(
    sessions: &mut HashMap<String, H>,
    session_key: &str,
    generation: u64,
    generation_of: impl Fn(&H) -> u64,
) {
    if sessions
        .get(session_key)
        .is_some_and(|handle| generation_of(handle) == generation)
    {
        sessions.remove(session_key);
    }
}

/// The stable identity of one permission ask.
///
/// It keys the parked responder, the attention row, and
/// `fleet_session.current_request_fingerprint`, which is what makes a stale
/// answer detectable rather than silently applied to the next ask.
fn permission_fingerprint(session_key: &str, permission: &PermissionRequest) -> String {
    let body = serde_json::json!({
        "sessionKey": session_key,
        "acpSessionId": permission.session_id(),
        "rpcId": permission.rpc_id(),
        "options": permission.option_ids(),
    });
    let digest = blake3::hash(serde_json::to_string(&body).unwrap_or_default().as_bytes());
    digest.to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::{Arc, HashMap, holds_process, retire_if_current};

    /// The exit event is PROCESS-SCOPED. The interleaving it defends against
    /// (the watcher snapshots the routes a dying process hosted, the actor then
    /// requeues onto a new one before it reads the message) is a real race but
    /// not reachable on demand from a test, so the decision itself is pinned
    /// here instead.
    #[test]
    fn an_exit_event_is_only_applied_to_the_process_the_actor_holds() {
        let mine = Arc::new(1_u32);
        let other = Arc::new(1_u32);

        assert!(
            holds_process(&Arc::downgrade(&mine), Some(&mine)),
            "the process this actor is on converges"
        );
        assert!(
            !holds_process(&Arc::downgrade(&other), Some(&mine)),
            "an equal-VALUED but different process is a straggler, not ours"
        );
        assert!(
            !holds_process(&Arc::downgrade(&mine), None),
            "a detached actor has no process to converge"
        );

        let dead = Arc::downgrade(&other);
        drop(other);
        assert!(
            !holds_process(&dead, Some(&mine)),
            "a process nobody holds cannot be the one this actor holds"
        );
    }

    /// Retirement is GENERATION-scoped. Teardown then respawn is the normal
    /// path through convergence and resume, so a predecessor's retirement
    /// routinely lands after its successor registered; removing then would
    /// leave a live actor nothing can route to.
    #[test]
    fn a_retiring_actor_never_evicts_its_successor() {
        let mut sessions: HashMap<String, u64> = HashMap::new();
        sessions.insert("acp:1".to_string(), 7);

        // The successor registered first: generation 8 now owns the entry.
        sessions.insert("acp:1".to_string(), 8);
        retire_if_current(&mut sessions, "acp:1", 7, |generation| *generation);
        assert_eq!(
            sessions.get("acp:1"),
            Some(&8),
            "the predecessor's retirement must not evict the live successor"
        );

        // The owner retires: the entry goes.
        retire_if_current(&mut sessions, "acp:1", 8, |generation| *generation);
        assert!(
            sessions.get("acp:1").is_none(),
            "the owning actor clears its own entry"
        );

        // Retiring twice, or against a session nobody registered, is a no-op.
        retire_if_current(&mut sessions, "acp:1", 8, |generation| *generation);
        retire_if_current(&mut sessions, "acp:missing", 1, |generation| *generation);
        assert!(sessions.is_empty());
    }
}
