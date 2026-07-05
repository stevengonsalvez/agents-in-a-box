//! VAPID web-push: keypair, subscription store, attention-driven delivery.
//!
//! The dashboard can notify you when a session needs attention even while the
//! tab is backgrounded or the phone is locked, via the W3C Push API. This
//! module owns:
//!
//! * **VAPID keypair** — a per-install P-256 keypair persisted under
//!   `$AINB_HOME/.agents-in-a-box/web/vapid.json` (generated once, `0600`). The
//!   public key is handed to the browser as the `applicationServerKey`.
//! * **Subscription store** — the browser `PushSubscription`s, persisted
//!   atomically next to the keypair. Presence (tab focused?) is tracked per
//!   endpoint so we don't buzz a phone for a tab you're already looking at.
//! * **Delivery** — a background task watching the cached snapshot's `needs`
//!   surface; when a session transitions *into* an attention state
//!   (ASK / ERR / WAIT — the same `AlertKind` classification notifyd uses) a
//!   push is sent to every non-focused subscriber. Dead endpoints (404/410)
//!   are pruned automatically.
//!
//! ## Security
//!
//! All `/api/push/*` routes sit behind the same bearer middleware as the rest
//! of `/api/*`. The VAPID private key never leaves the server. Subscriptions
//! are stored locally only.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use ainb_hangar_proto::{Channel, ChannelSet};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::routes::AppState;
use crate::sender::{PushSender, SendOutcome};

/// How often the delivery task re-reads the cached snapshot to look for new
/// attention transitions. Cheap: it only reads the in-memory cache the poller
/// already maintains.
const DELIVERY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

/// Hard cap on stored push subscriptions. The store is a single-user local
/// dashboard, so a handful of devices is the realistic ceiling; capping it
/// stops a misbehaving (or hostile) client from growing the persisted file
/// without bound. On overflow we evict the oldest subscription (FIFO).
const MAX_SUBSCRIPTIONS: usize = 64;

/// Max accepted body size for `POST /api/push/subscribe`. A `PushSubscription`
/// is a small JSON object (endpoint URL + two short base64 keys); 16 KiB is
/// generous headroom while bounding the per-request allocation.
const SUBSCRIBE_BODY_LIMIT: usize = 16 * 1024;

// ── On-disk models ──────────────────────────────────────────────────────────

/// A persisted VAPID keypair. The private key is a base64url (no-pad) PKCS#8
/// DER; the public key is the base64url uncompressed P-256 point the browser
/// uses as its `applicationServerKey`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VapidKeys {
    /// base64url(no-pad) of the uncompressed SEC1 public point (65 bytes).
    pub public_key: String,
    /// base64url(no-pad) of the PKCS#8 DER private key.
    pub private_key: String,
    /// The VAPID `sub` claim (a `mailto:` or origin URL).
    pub subject: String,
}

/// A single browser push subscription plus our presence bookkeeping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    /// Push service endpoint URL.
    pub endpoint: String,
    /// Client public key (`keys.p256dh`), base64url.
    pub p256dh: String,
    /// Client auth secret (`keys.auth`), base64url.
    pub auth: String,
    /// Whether the owning tab currently reports itself focused. `None` until
    /// the first presence ping — treated as "not focused" so a freshly
    /// subscribed client still receives notifications (a more useful default
    /// than agent-deck's suppress-until-presence).
    #[serde(default)]
    pub focused: Option<bool>,
}

impl Subscription {
    /// Should this subscriber receive a notification right now? Suppressed only
    /// when the tab is explicitly focused.
    fn wants_notification(&self) -> bool {
        !self.focused.unwrap_or(false)
    }
}

/// The persisted subscription set.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SubscriptionStore {
    subscriptions: Vec<Subscription>,
}

// ── Push state ──────────────────────────────────────────────────────────────

/// Shared push state: the VAPID keys, the subscription store (behind a mutex
/// for atomic file writes), and the sender used to deliver messages.
#[derive(Clone)]
pub struct PushState {
    keys: Arc<VapidKeys>,
    store: Arc<Mutex<SubscriptionStore>>,
    store_path: Arc<PathBuf>,
    sender: Arc<dyn PushSender>,
}

impl PushState {
    /// Initialise push: load-or-generate the VAPID keys and load the
    /// subscription store from `$AINB_HOME/.agents-in-a-box/web/`. The
    /// `sender` is injected so tests can assert delivery without real network
    /// I/O.
    pub fn init(subject: &str, sender: Arc<dyn PushSender>) -> anyhow::Result<Self> {
        Self::init_in(&web_state_dir(), subject, sender)
    }

    /// Initialise push state rooted at an explicit `dir`. Splitting this out
    /// from [`PushState::init`] lets tests point each instance at its own temp
    /// directory without racing on the process-wide `AINB_HOME` env var.
    pub fn init_in(dir: &Path, subject: &str, sender: Arc<dyn PushSender>) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let keys = Arc::new(load_or_generate_keys(dir, subject)?);
        let store_path = dir.join("push_subscriptions.json");
        let store = load_store(&store_path).unwrap_or_default();
        Ok(Self {
            keys,
            store: Arc::new(Mutex::new(store)),
            store_path: Arc::new(store_path),
            sender,
        })
    }

    /// The public application-server key the frontend subscribes with.
    pub fn public_key(&self) -> &str {
        &self.keys.public_key
    }

    /// Persist the current store to disk atomically (write-tmp-then-rename).
    async fn persist(&self, store: &SubscriptionStore) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(store)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = self.store_path.with_extension("tmp");
        tokio::fs::write(&tmp, &json).await?;
        tokio::fs::rename(&tmp, &*self.store_path).await
    }

    /// Add (or replace by endpoint) a subscription, capping the store at
    /// [`MAX_SUBSCRIPTIONS`] by evicting the oldest entries (FIFO) so it can
    /// never grow unbounded.
    async fn upsert(&self, sub: Subscription) -> std::io::Result<()> {
        let mut store = self.store.lock().await;
        store.subscriptions.retain(|s| s.endpoint != sub.endpoint);
        store.subscriptions.push(sub);
        // Evict oldest until within cap (a single push can only ever overflow
        // by one, but `drain` is robust to any prior over-cap on-disk state).
        if store.subscriptions.len() > MAX_SUBSCRIPTIONS {
            let overflow = store.subscriptions.len() - MAX_SUBSCRIPTIONS;
            store.subscriptions.drain(0..overflow);
        }
        self.persist(&store).await
    }

    /// Remove a subscription by endpoint. Returns whether one was removed.
    async fn remove(&self, endpoint: &str) -> std::io::Result<bool> {
        let mut store = self.store.lock().await;
        let before = store.subscriptions.len();
        store.subscriptions.retain(|s| s.endpoint != endpoint);
        let removed = store.subscriptions.len() != before;
        if removed {
            self.persist(&store).await?;
        }
        Ok(removed)
    }

    /// Update the focus flag for a subscription endpoint.
    async fn set_focus(&self, endpoint: &str, focused: bool) -> std::io::Result<bool> {
        let mut store = self.store.lock().await;
        let mut found = false;
        for s in &mut store.subscriptions {
            if s.endpoint == endpoint {
                s.focused = Some(focused);
                found = true;
            }
        }
        if found {
            self.persist(&store).await?;
        }
        Ok(found)
    }

    /// Number of stored subscriptions.
    async fn count(&self) -> usize {
        self.store.lock().await.subscriptions.len()
    }

    /// Send `payload` to every non-focused subscriber, pruning any endpoint the
    /// push service reports as gone (404/410). Returns the number of successful
    /// deliveries.
    async fn broadcast(&self, payload: &Value) -> usize {
        let targets: Vec<Subscription> = {
            let store = self.store.lock().await;
            store.subscriptions.iter().filter(|s| s.wants_notification()).cloned().collect()
        };

        let body = payload.to_string();
        let mut delivered = 0usize;
        let mut dead: Vec<String> = Vec::new();
        for sub in targets {
            match self.sender.send(&self.keys, &sub, body.as_bytes()).await {
                SendOutcome::Ok => delivered += 1,
                SendOutcome::Gone => dead.push(sub.endpoint.clone()),
                SendOutcome::TransientError(e) => {
                    tracing::warn!(endpoint = %sub.endpoint, error = %e, "push delivery failed");
                }
            }
        }

        if !dead.is_empty() {
            let mut store = self.store.lock().await;
            store.subscriptions.retain(|s| !dead.contains(&s.endpoint));
            if let Err(e) = self.persist(&store).await {
                tracing::warn!(error = %e, "failed to persist subscription pruning");
            }
        }
        delivered
    }
}

// ── Attention-transition delivery loop ──────────────────────────────────────

/// Spawn the background task that turns `needs` transitions into pushes.
///
/// It reads the **cached** snapshot every [`DELIVERY_INTERVAL`] (never shelling
/// out itself), classifies each session's current attention state from the
/// `needs` rows, and sends a push the moment a session enters an attention
/// state it wasn't in last tick. The first tick is a baseline (no flood on
/// startup).
pub fn spawn_delivery(state: AppState, push: PushState) {
    tokio::spawn(async move {
        // Per-cwd last-seen attention kind, so we notify only on transitions.
        let mut last: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut baseline_done = false;
        let mut ticker = tokio::time::interval(DELIVERY_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            if state.cache.is_closed() {
                break;
            }
            let Ok(snap) = state.resolve_snapshot().await else {
                continue;
            };
            // The first tick only records a baseline (no flood on startup); every
            // later tick delivers the transitions.
            last = deliver_web_transitions(&push, &snap, &last, baseline_done).await;
            baseline_done = true;
        }
    });
}

/// Deliver one tick's web pushes and return the next `last`-seen kind map.
///
/// For each session key, buzz a device iff (a) `deliver` is true (never on the
/// startup baseline tick), (b) the attention transitioned INTO a new kind since
/// `last`, (c) it is an actionable kind, and (d) the notify rules routed it to
/// the `web` channel (tcp T5) — a decision resolved once at raise time and
/// carried on the card. Extracted from the loop so the routing gate is testable
/// against a stub [`PushSender`].
async fn deliver_web_transitions(
    push: &PushState,
    snap: &crate::data::FleetSnapshot,
    last: &std::collections::HashMap<String, String>,
    deliver: bool,
) -> std::collections::HashMap<String, String> {
    let current = attention_by_key(&snap.needs);
    if deliver {
        for (key, (kind, channels)) in &current {
            let changed = last.get(key).map(String::as_str) != Some(kind.as_str());
            if changed && is_attention(kind) && channels.contains(Channel::Web) {
                let payload = build_payload(key, kind, snap);
                let n = push.broadcast(&payload).await;
                tracing::debug!(key, kind, delivered = n, "attention push sent");
            }
        }
    }
    current.into_iter().map(|(k, (kind, _))| (k, kind)).collect()
}

/// True for the actionable attention kinds (the ones worth considering for a
/// push). `IDLE` and `FINISHED` are informational and never buzz.
fn is_attention(kind: &str) -> bool {
    matches!(kind, "ASK" | "ERR" | "WAIT")
}

/// Reduce the `needs` JSON array to a map of session key → the highest-priority
/// attention's `(kind, channels)`. `channels` is the push routing resolved at
/// raise time (tcp T5), carried per card — the delivery loop filters on the `web`
/// channel. A card that omits `channels` (a legacy row) reads as board-only and
/// never buzzes.
///
/// The key is the raising session's cwd (stable across renders). The daemon's
/// `attention_to_needs` cards carry `cwd` / `sessionId` at the TOP LEVEL, so those
/// are read first; a nested `session.{cwd,workspace_name,tmux_session}` is the
/// fallback for the legacy `ainb fleet needs` shape. Reading only the nested form
/// (the pre-fix bug) collapsed every daemon-backed card onto one `"session"` key,
/// which would fan a single push out per host instead of per session.
fn attention_by_key(needs: &Value) -> std::collections::HashMap<String, (String, ChannelSet)> {
    let mut out: std::collections::HashMap<String, (String, ChannelSet)> =
        std::collections::HashMap::new();
    let Some(rows) = needs.as_array() else {
        return out;
    };
    let rank = |k: &str| match k {
        "ASK" => 0,
        "ERR" => 1,
        "WAIT" => 2,
        _ => 3,
    };
    // A nested `fn` (not a closure) so the borrow lifetime ties input to output.
    fn non_empty(v: Option<&str>) -> Option<&str> {
        v.filter(|s| !s.is_empty())
    }
    for row in rows {
        let kind = row.get("kind").and_then(Value::as_str).unwrap_or("IDLE").to_uppercase();
        let channels = row
            .get("channels")
            .and_then(|c| serde_json::from_value::<ChannelSet>(c.clone()).ok())
            .unwrap_or(ChannelSet::NONE);
        let session = row.get("session").cloned().unwrap_or(Value::Null);
        // Top-level card fields first (the real daemon `attention/list` shape),
        // then the legacy nested `session` fallback.
        let key = non_empty(row.get("cwd").and_then(Value::as_str))
            .or_else(|| non_empty(row.get("sessionId").and_then(Value::as_str)))
            .or_else(|| non_empty(session.get("cwd").and_then(Value::as_str)))
            .or_else(|| non_empty(session.get("workspace_name").and_then(Value::as_str)))
            .or_else(|| non_empty(session.get("tmux_session").and_then(Value::as_str)))
            .unwrap_or("session")
            .to_string();
        match out.get(&key) {
            Some((existing, _)) if rank(existing) <= rank(&kind) => {}
            _ => {
                out.insert(key, (kind, channels));
            }
        }
    }
    out
}

/// Build the push payload the service worker renders into a notification.
fn build_payload(key: &str, kind: &str, snap: &crate::data::FleetSnapshot) -> Value {
    // Try to resolve a friendly title + a deep link from the session list.
    let mut title_name = key.to_string();
    let mut session_id = String::new();
    if let Some(sessions) = snap.sessions.as_array() {
        for s in sessions {
            // `attention_by_key` builds the key from the needs row's `session.cwd`.
            // For ainb-managed sessions cwd == workspace_path != worktree_path, so
            // match on the same field that built the key (cwd) and fall back to
            // worktree_path for sessions keyed that way.
            let cwd = s.get("cwd").and_then(Value::as_str).unwrap_or("");
            let worktree = s.get("worktree_path").and_then(Value::as_str).unwrap_or("");
            if cwd == key || worktree == key {
                if let Some(name) = s.get("workspace_name").and_then(Value::as_str) {
                    title_name = name.to_string();
                }
                if let Some(id) = s.get("session_id").and_then(Value::as_str) {
                    session_id = id.to_string();
                }
                break;
            }
        }
    }
    let label = match kind {
        "ASK" => "needs an answer",
        "ERR" => "hit an error",
        "WAIT" => "is waiting on you",
        _ => "needs attention",
    };
    json!({
        "title": format!("ainb · {title_name}"),
        "body": format!("{title_name} {label}."),
        "kind": kind,
        "sessionId": session_id,
        "requireInteraction": kind == "ERR",
        "tag": format!("ainb-{key}"),
    })
}

// ── HTTP handlers ────────────────────────────────────────────────────────────

/// Request body for `POST /api/push/subscribe`, matching the browser
/// `PushSubscription.toJSON()` shape.
#[derive(Debug, Deserialize)]
struct SubscribeBody {
    endpoint: String,
    keys: SubscribeKeys,
}

/// The `keys` object of a browser `PushSubscription`.
#[derive(Debug, Deserialize)]
struct SubscribeKeys {
    p256dh: String,
    auth: String,
}

/// Request body for `POST /api/push/{unsubscribe,presence}`.
#[derive(Debug, Deserialize)]
struct EndpointBody {
    endpoint: String,
    #[serde(default)]
    focused: Option<bool>,
}

/// `GET /api/push/config` — the VAPID public key + subscriber count, so the
/// frontend can decide whether to offer the "enable notifications" affordance.
async fn config(State(state): State<AppState>) -> Response {
    let Some(push) = state.push.as_ref() else {
        return push_unconfigured();
    };
    Json(json!({
        "enabled": true,
        "vapidPublicKey": push.public_key(),
        "subscriptionCount": push.count().await,
    }))
    .into_response()
}

/// `POST /api/push/subscribe` — register a browser push subscription.
async fn subscribe(State(state): State<AppState>, body: Json<SubscribeBody>) -> Response {
    let Some(push) = state.push.as_ref() else {
        return push_unconfigured();
    };
    let sub = Subscription {
        endpoint: body.0.endpoint,
        p256dh: body.0.keys.p256dh,
        auth: body.0.keys.auth,
        focused: Some(false),
    };
    match push.upsert(sub).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// `POST /api/push/unsubscribe` — drop a subscription by endpoint.
async fn unsubscribe(State(state): State<AppState>, body: Json<EndpointBody>) -> Response {
    let Some(push) = state.push.as_ref() else {
        return push_unconfigured();
    };
    match push.remove(&body.0.endpoint).await {
        Ok(removed) => Json(json!({ "ok": true, "removed": removed })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// `POST /api/push/presence` — update whether the owning tab is focused, so we
/// can suppress pushes for a session you're already watching.
async fn presence(State(state): State<AppState>, body: Json<EndpointBody>) -> Response {
    let Some(push) = state.push.as_ref() else {
        return push_unconfigured();
    };
    let focused = body.0.focused.unwrap_or(false);
    match push.set_focus(&body.0.endpoint, focused).await {
        Ok(found) => Json(json!({ "ok": true, "found": found })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// 503 when the server was built/started without push configured.
fn push_unconfigured() -> Response {
    let body = Json(json!({
        "error": { "code": "PUSH_NOT_CONFIGURED", "message": "web-push is not enabled" }
    }));
    (StatusCode::SERVICE_UNAVAILABLE, body).into_response()
}

/// 500 envelope for storage failures.
fn internal_error(detail: &str) -> Response {
    let body = Json(json!({
        "error": { "code": "INTERNAL_ERROR", "message": detail }
    }));
    (StatusCode::INTERNAL_SERVER_ERROR, body).into_response()
}

/// The `/api/push/*` sub-router (auth is applied by the caller alongside the
/// rest of `/api/*`).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/push/config", get(config))
        .route(
            "/api/push/subscribe",
            // Bound the request body: a subscription is small JSON, so reject
            // anything oversized before it is buffered/deserialized.
            post(subscribe).layer(DefaultBodyLimit::max(SUBSCRIBE_BODY_LIMIT)),
        )
        .route(
            "/api/push/unsubscribe",
            post(unsubscribe).layer(DefaultBodyLimit::max(SUBSCRIBE_BODY_LIMIT)),
        )
        .route(
            "/api/push/presence",
            post(presence).layer(DefaultBodyLimit::max(SUBSCRIBE_BODY_LIMIT)),
        )
}

// ── Key + store persistence ──────────────────────────────────────────────────

/// `$AINB_HOME/.agents-in-a-box/web` (honoring the `AINB_HOME` override the
/// rest of the codebase uses), where VAPID keys + subscriptions live.
fn web_state_dir() -> PathBuf {
    let base = std::env::var_os("AINB_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")));
    base.join(".agents-in-a-box").join("web")
}

/// Load the VAPID keypair from `dir`, generating + persisting one on first run.
fn load_or_generate_keys(dir: &Path, subject: &str) -> anyhow::Result<VapidKeys> {
    let path = dir.join("vapid.json");
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(keys) = serde_json::from_slice::<VapidKeys>(&bytes) {
            return Ok(keys);
        }
        tracing::warn!("vapid.json present but unreadable; regenerating");
    }
    let keys = crate::sender::generate_vapid_keys(subject)?;
    let json = serde_json::to_vec_pretty(&keys)?;
    write_private(&path, &json)?;
    Ok(keys)
}

/// Write `bytes` to `path` with `0600` perms on Unix (the private key must not
/// be world-readable).
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("vapid.json");
        let tmp = path.with_file_name(format!("{file_name}.tmp.{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }

    #[cfg(not(unix))]
    {
        let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("vapid.json");
        let tmp = path.with_file_name(format!("{file_name}.tmp.{}", std::process::id()));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path)?;
    }
    Ok(())
}

/// Load the subscription store, returning `None` on first run / parse error.
fn load_store(path: &Path) -> Option<SubscriptionStore> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sender::{PushSender, SendOutcome};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A sender that counts deliveries and can be told to report specific
    /// endpoints as gone (404/410) so pruning is testable.
    struct FakeSender {
        sent: AtomicUsize,
        gone_endpoint: Option<String>,
    }

    impl FakeSender {
        fn new() -> Self {
            Self {
                sent: AtomicUsize::new(0),
                gone_endpoint: None,
            }
        }
        fn with_gone(endpoint: &str) -> Self {
            Self {
                sent: AtomicUsize::new(0),
                gone_endpoint: Some(endpoint.to_string()),
            }
        }
    }

    impl PushSender for FakeSender {
        fn send<'a>(
            &'a self,
            _keys: &'a VapidKeys,
            sub: &'a Subscription,
            _payload: &'a [u8],
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = SendOutcome> + Send + 'a>> {
            Box::pin(async move {
                if self.gone_endpoint.as_deref() == Some(sub.endpoint.as_str()) {
                    return SendOutcome::Gone;
                }
                self.sent.fetch_add(1, Ordering::SeqCst);
                SendOutcome::Ok
            })
        }
    }

    fn sub(endpoint: &str, focused: Option<bool>) -> Subscription {
        Subscription {
            endpoint: endpoint.into(),
            p256dh: "p".into(),
            auth: "a".into(),
            focused,
        }
    }

    /// Init a PushState pointed at an isolated `AINB_HOME` temp dir. Serialized
    /// because it mutates the process-wide `AINB_HOME` env var.
    fn push_state(sender: Arc<dyn PushSender>) -> (PushState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let state = PushState::init_in(dir.path(), "mailto:test@localhost", sender).unwrap();
        (state, dir)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn broadcast_delivers_to_unfocused_and_skips_focused() {
        let sender = Arc::new(FakeSender::new());
        let (push, _dir) = push_state(sender.clone());
        push.upsert(sub("https://a", Some(false))).await.unwrap();
        push.upsert(sub("https://b", Some(true))).await.unwrap(); // focused → skip
        push.upsert(sub("https://c", None)).await.unwrap(); // unknown → deliver

        let delivered = push.broadcast(&json!({ "title": "x" })).await;
        assert_eq!(delivered, 2, "focused subscriber must be suppressed");
        assert_eq!(sender.sent.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn broadcast_prunes_dead_endpoints() {
        let sender = Arc::new(FakeSender::with_gone("https://dead"));
        let (push, _dir) = push_state(sender);
        push.upsert(sub("https://live", Some(false))).await.unwrap();
        push.upsert(sub("https://dead", Some(false))).await.unwrap();
        assert_eq!(push.count().await, 2);

        let delivered = push.broadcast(&json!({ "title": "x" })).await;
        assert_eq!(delivered, 1, "only the live endpoint delivers");
        assert_eq!(push.count().await, 1, "the 410-gone endpoint is pruned");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn presence_toggles_suppression() {
        let sender = Arc::new(FakeSender::new());
        let (push, _dir) = push_state(sender.clone());
        push.upsert(sub("https://a", Some(false))).await.unwrap();

        assert_eq!(push.broadcast(&json!({})).await, 1);
        push.set_focus("https://a", true).await.unwrap();
        assert_eq!(
            push.broadcast(&json!({})).await,
            0,
            "focused now suppressed"
        );
        push.set_focus("https://a", false).await.unwrap();
        assert_eq!(
            push.broadcast(&json!({})).await,
            1,
            "unfocused again delivers"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unsubscribe_removes_endpoint() {
        let sender = Arc::new(FakeSender::new());
        let (push, _dir) = push_state(sender);
        push.upsert(sub("https://a", Some(false))).await.unwrap();
        assert!(push.remove("https://a").await.unwrap());
        assert_eq!(push.count().await, 0);
        assert!(
            !push.remove("https://a").await.unwrap(),
            "second remove is a no-op"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_caps_store_and_evicts_oldest() {
        let sender = Arc::new(FakeSender::new());
        let (push, _dir) = push_state(sender);
        // Insert one more than the cap; the very first endpoint must be evicted.
        for i in 0..=MAX_SUBSCRIPTIONS {
            push.upsert(sub(&format!("https://e{i}"), Some(false))).await.unwrap();
        }
        assert_eq!(
            push.count().await,
            MAX_SUBSCRIPTIONS,
            "store must be capped at MAX_SUBSCRIPTIONS"
        );
        // The oldest (e0) was evicted; removing it is a no-op now.
        assert!(
            !push.remove("https://e0").await.unwrap(),
            "oldest subscription should have been evicted"
        );
        // The newest is still present.
        assert!(
            push.remove(&format!("https://e{MAX_SUBSCRIPTIONS}")).await.unwrap(),
            "newest subscription must be retained"
        );
    }

    #[test]
    fn attention_dedupes_to_highest_priority_per_key() {
        let needs = json!([
            { "kind": "WAIT", "session": { "cwd": "/a" } },
            { "kind": "ASK",  "session": { "cwd": "/a" } },
            { "kind": "ERR",  "session": { "cwd": "/b" } },
            { "kind": "IDLE", "session": { "cwd": "/c" } },
        ]);
        let map = attention_by_key(&needs);
        assert_eq!(map.get("/a").map(|(k, _)| k.as_str()), Some("ASK"));
        assert_eq!(map.get("/b").map(|(k, _)| k.as_str()), Some("ERR"));
        assert_eq!(map.get("/c").map(|(k, _)| k.as_str()), Some("IDLE"));
    }

    /// Regression: the daemon's `attention_to_needs` cards carry `cwd` at the TOP
    /// LEVEL (not nested under `session`). Each card must key on its OWN cwd so a
    /// per-session push routes per session — the pre-fix nested-only read collapsed
    /// every daemon card onto one `"session"` key.
    #[test]
    fn attention_by_key_reads_top_level_daemon_card_cwd() {
        // The real card shape from the daemon `attention/list` mapping.
        let needs = json!([
            { "kind": "ASK", "cwd": "/work/one", "sessionId": "s1", "channels": ["web"] },
            { "kind": "ERR", "cwd": "/work/two", "sessionId": "s2", "channels": ["os"] },
        ]);
        let map = attention_by_key(&needs);
        assert_eq!(map.len(), 2, "each card keys on its own cwd, not one shared key");
        assert_eq!(map.get("/work/one").map(|(k, _)| k.as_str()), Some("ASK"));
        assert_eq!(map.get("/work/two").map(|(k, _)| k.as_str()), Some("ERR"));
        assert!(!map.contains_key("session"), "cards must not collapse onto the fallback key");

        // A card with an empty top-level cwd falls back to sessionId, still distinct.
        let by_id = json!([{ "kind": "ASK", "cwd": "", "sessionId": "s9", "channels": ["web"] }]);
        assert!(attention_by_key(&by_id).contains_key("s9"));
    }

    /// The web-push channel filter (tcp T5): a card carries its raise-time
    /// `channels`, and only a card routed to the `web` channel is eligible to
    /// buzz a device. A board-only card (empty set) is suppressed even though it
    /// is an actionable kind.
    #[test]
    fn attention_by_key_carries_web_routing_channels() {
        let needs = json!([
            // ASK routed to web+os → web-eligible.
            { "kind": "ASK",  "session": { "cwd": "/ask" },  "channels": ["web", "os"] },
            // WAIT board-only (no channels) → not web-eligible.
            { "kind": "WAIT", "session": { "cwd": "/wait" }, "channels": [] },
            // ERR routed os-only → not web-eligible.
            { "kind": "ERR",  "session": { "cwd": "/err" },  "channels": ["os"] },
        ]);
        let map = attention_by_key(&needs);
        assert!(map.get("/ask").unwrap().1.contains(Channel::Web), "ASK is web-routed");
        assert!(!map.get("/wait").unwrap().1.contains(Channel::Web), "board-only WAIT is not");
        assert!(!map.get("/err").unwrap().1.contains(Channel::Web), "os-only ERR is not");
        // A card that omits `channels` reads as board-only (no web buzz).
        let legacy = json!([{ "kind": "ASK", "session": { "cwd": "/x" } }]);
        assert!(!attention_by_key(&legacy).get("/x").unwrap().1.contains(Channel::Web));
    }

    /// End-to-end web filter (tcp T5) through the stub [`PushSender`]: a tick with
    /// a web-routed ASK, a board-only WAIT, and an os-only ERR buzzes the device
    /// exactly once — the web-routed card. The suppressed channels never fire.
    #[tokio::test(flavor = "multi_thread")]
    async fn web_transition_delivers_only_web_routed_attentions() {
        let sender = Arc::new(FakeSender::new());
        let (push, _dir) = push_state(sender.clone());
        push.upsert(sub("https://dev", Some(false))).await.unwrap();

        let snap = crate::data::FleetSnapshot::from_parts(
            crate::data::CoreSnapshot {
                sessions: json!([]),
                needs: json!([
                    { "kind": "ASK",  "session": { "cwd": "/ask" },  "channels": ["web", "os"] },
                    { "kind": "WAIT", "session": { "cwd": "/wait" }, "channels": [] },
                    { "kind": "ERR",  "session": { "cwd": "/err" },  "channels": ["os"] },
                ]),
            },
            Value::Null,
        );

        // deliver = true, empty `last` → each card is a fresh transition.
        let last =
            deliver_web_transitions(&push, &snap, &std::collections::HashMap::new(), true).await;

        // Exactly ONE device buzz — the web-routed ASK. The board-only WAIT and
        // the os-only ERR are suppressed on web.
        assert_eq!(
            sender.sent.load(Ordering::SeqCst),
            1,
            "only the web-routed ASK buzzes the device"
        );
        // Every key's kind is tracked for the next tick's transition detection,
        // even the suppressed ones.
        assert_eq!(last.get("/ask").map(String::as_str), Some("ASK"));
        assert_eq!(last.get("/wait").map(String::as_str), Some("WAIT"));
    }

    /// The baseline tick (`deliver = false`) records state but never buzzes — no
    /// startup flood even for a web-routed card.
    #[tokio::test(flavor = "multi_thread")]
    async fn web_baseline_tick_records_without_delivering() {
        let sender = Arc::new(FakeSender::new());
        let (push, _dir) = push_state(sender.clone());
        push.upsert(sub("https://dev", Some(false))).await.unwrap();

        let snap = crate::data::FleetSnapshot::from_parts(
            crate::data::CoreSnapshot {
                sessions: json!([]),
                needs: json!([
                    { "kind": "ASK", "session": { "cwd": "/ask" }, "channels": ["web", "os"] },
                ]),
            },
            Value::Null,
        );
        let last =
            deliver_web_transitions(&push, &snap, &std::collections::HashMap::new(), false).await;
        assert_eq!(sender.sent.load(Ordering::SeqCst), 0, "baseline never buzzes");
        assert_eq!(last.get("/ask").map(String::as_str), Some("ASK"), "but records state");
    }

    #[test]
    fn only_actionable_kinds_are_attention() {
        assert!(is_attention("ASK"));
        assert!(is_attention("ERR"));
        assert!(is_attention("WAIT"));
        assert!(!is_attention("IDLE"));
        assert!(!is_attention("FINISHED"));
        // `ainb fleet needs` never emits NEEDS_PERMISSION (permission → ASK).
        assert!(!is_attention("NEEDS_PERMISSION"));
    }

    #[test]
    fn subscription_focus_suppresses_notification() {
        let mut s = Subscription {
            endpoint: "e".into(),
            p256dh: "p".into(),
            auth: "a".into(),
            focused: None,
        };
        assert!(s.wants_notification(), "unknown focus => notify");
        s.focused = Some(false);
        assert!(s.wants_notification(), "unfocused => notify");
        s.focused = Some(true);
        assert!(!s.wants_notification(), "focused => suppress");
    }

    #[test]
    fn payload_carries_kind_and_title() {
        let snap = crate::data::FleetSnapshot::from_parts(
            crate::data::CoreSnapshot {
                sessions: json!([
                    { "session_id": "id-1", "workspace_name": "demo", "worktree_path": "/a" }
                ]),
                needs: json!([]),
            },
            Value::Null,
        );
        let p = build_payload("/a", "ASK", &snap);
        assert_eq!(p["kind"], "ASK");
        assert_eq!(p["sessionId"], "id-1");
        assert!(p["title"].as_str().unwrap().contains("demo"));
        assert_eq!(p["requireInteraction"], false);
    }

    #[test]
    fn payload_resolves_ainb_managed_session_by_cwd() {
        // For ainb-managed sessions the needs key is the session `cwd`
        // (== workspace_path), which differs from `worktree_path`. Resolution
        // must match on cwd so the title/deep-link don't mis-resolve.
        let snap = crate::data::FleetSnapshot::from_parts(
            crate::data::CoreSnapshot {
                sessions: json!([
                    {
                        "session_id": "id-9",
                        "workspace_name": "managed",
                        "cwd": "/ws/managed",
                        "worktree_path": "/repo/.worktrees/managed"
                    }
                ]),
                needs: json!([]),
            },
            Value::Null,
        );
        let p = build_payload("/ws/managed", "ASK", &snap);
        assert_eq!(
            p["sessionId"], "id-9",
            "must resolve via cwd, not worktree_path"
        );
        assert!(p["title"].as_str().unwrap().contains("managed"));
    }

    #[cfg(unix)]
    #[test]
    fn write_private_creates_vapid_file_with_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vapid.json");
        write_private(&path, b"{\"private_key\":\"secret\"}").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "vapid private key must start private");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"{\"private_key\":\"secret\"}"
        );
    }
}
