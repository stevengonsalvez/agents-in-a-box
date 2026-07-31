import Foundation

enum RPCID: Codable, Equatable, Hashable {
    case number(Int64)
    case text(String)

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let number = try? container.decode(Int64.self) {
            self = .number(number)
        } else {
            self = .text(try container.decode(String.self))
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .number(let value): try container.encode(value)
        case .text(let value): try container.encode(value)
        }
    }
}

struct RPCRequest<Params: Encodable>: Encodable {
    let jsonrpc: String
    let id: RPCID
    let method: String
    let params: Params

    init(id: RPCID, method: String, params: Params) {
        jsonrpc = "2.0"
        self.id = id
        self.method = method
        self.params = params
    }
}

extension RPCRequest: Decodable where Params: Decodable {}

struct RPCResponse<Result: Decodable>: Decodable {
    let jsonrpc: String
    let id: RPCID
    let result: Result?
    let error: RPCError?

    private enum CodingKeys: String, CodingKey { case jsonrpc, id, result, error }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        jsonrpc = try container.decode(String.self, forKey: .jsonrpc)
        id = try container.decode(RPCID.self, forKey: .id)
        result = try container.decodeIfPresent(Result.self, forKey: .result)
        error = try container.decodeIfPresent(RPCError.self, forKey: .error)
        guard (result == nil) != (error == nil) else {
            throw DecodingError.dataCorruptedError(forKey: .result, in: container, debugDescription: "response requires exactly one result or error")
        }
    }
}

struct RPCError: Codable, Equatable {
    let code: Int
    let message: String
    let data: JSONValue?
}

struct AuthHelloParams: Codable, Equatable {
    let token: String
}

indirect enum JSONValue: Codable, Equatable {
    case null
    case bool(Bool)
    case number(Decimal)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() { self = .null }
        else if let value = try? container.decode(Bool.self) { self = .bool(value) }
        else if let value = try? container.decode(Decimal.self) { self = .number(value) }
        else if let value = try? container.decode(String.self) { self = .string(value) }
        else if let value = try? container.decode([JSONValue].self) { self = .array(value) }
        else { self = .object(try container.decode([String: JSONValue].self)) }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .null: try container.encodeNil()
        case .bool(let value): try container.encode(value)
        case .number(let value): try container.encode(value)
        case .string(let value): try container.encode(value)
        case .array(let value): try container.encode(value)
        case .object(let value): try container.encode(value)
        }
    }
}

struct FleetProtocolRange: Codable, Equatable {
    let min: UInt32
    let max: UInt32
}

struct FleetNegotiateParams: Codable, Equatable {
    let clientName: String
    let clientVersion: String
    let readVersions: FleetProtocolRange
    let writeVersions: FleetProtocolRange
    private enum CodingKeys: String, CodingKey { case clientName = "client_name", clientVersion = "client_version", readVersions = "read_versions", writeVersions = "write_versions" }
}

struct FleetNegotiateResult: Codable, Equatable {
    let daemonVersion: String
    let protocolVersion: UInt32
    let readCompatible: Bool
    let writeCompatible: Bool
    let capabilityIDs: [String]
    private enum CodingKeys: String, CodingKey { case daemonVersion = "daemon_version", protocolVersion = "protocol_version", readCompatible = "read_compatible", writeCompatible = "write_compatible", capabilityIDs = "capability_ids" }
}

/// A wire enum that survives a value this client has never heard of.
///
/// The daemon may add enum values — a new provider, a new lifecycle — and that
/// is NOT a protocol version bump today, so version negotiation does not catch
/// it. Swift's synthesized `Codable` throws `DecodingError.dataCorrupted` on an
/// unrecognised raw value, and because `FleetSnapshot` decodes `sessions` as an
/// ARRAY, one unknown value fails the ENTIRE snapshot: the client goes blank
/// rather than degrading. Falling back is strictly better than failing.
///
/// Each `wireFallback` is chosen for FAIL-SAFETY, not convenience: where the
/// choice affects whether an operator sees a session, it errs toward showing it.
/// These types declare `Encodable` only, so the synthesized `init(from:)` does
/// not shadow the tolerant one below.
protocol TolerantWireEnum: RawRepresentable, Decodable where RawValue == String {
    /// Used when the daemon sends a value this build does not know.
    static var wireFallback: Self { get }
}

extension TolerantWireEnum {
    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = Self(rawValue: raw) ?? Self.wireFallback
    }
}

enum FleetProvider: String, Encodable, Equatable { case claude, codex, unknown }
enum LifecycleState: String, Encodable, Equatable { case starting = "STARTING", running = "RUNNING", turnComplete = "TURN_COMPLETE", idle = "IDLE", exited = "EXITED", unknown = "UNKNOWN" }
enum AttentionState: String, Encodable, Equatable { case none = "NONE", ask = "ASK", approval = "APPROVAL", waiting = "WAITING", error = "ERROR" }
enum ManagementState: String, Encodable, Equatable { case managed = "MANAGED", degraded = "DEGRADED" }
enum TransportHealth: String, Encodable, Equatable { case healthy = "HEALTHY", degraded = "DEGRADED", unavailable = "UNAVAILABLE", unknown = "UNKNOWN" }
enum FleetProvenance: String, Encodable, Equatable { case authoritative, inferred }
enum FleetConfidence: String, Encodable, Equatable { case high = "HIGH", medium = "MEDIUM", low = "LOW" }

// An unrecognised PROVIDER or LIFECYCLE is simply unknown — both already model it.
extension FleetProvider: TolerantWireEnum { static var wireFallback: Self { .unknown } }
extension LifecycleState: TolerantWireEnum { static var wireFallback: Self { .unknown } }
// An unrecognised ATTENTION means the daemon is signalling something we cannot
// name. Falling back to `.none` would DROP it out of the Needs-input tab and the
// operator would never learn a session wanted them; `.waiting` keeps it visible.
extension AttentionState: TolerantWireEnum { static var wireFallback: Self { .waiting } }
// Unrecognised capability/quality signals degrade rather than over-promise.
extension ManagementState: TolerantWireEnum { static var wireFallback: Self { .degraded } }
extension TransportHealth: TolerantWireEnum { static var wireFallback: Self { .unknown } }
extension FleetProvenance: TolerantWireEnum { static var wireFallback: Self { .inferred } }
extension FleetConfidence: TolerantWireEnum { static var wireFallback: Self { .low } }

struct FleetCapabilities: Codable, Equatable {
    let structuredAnswer: Bool
    let approvals: Bool
    let sendPrompt: Bool
    let continueTurn: Bool
    let retry: Bool
    let interrupt: Bool
    let start: Bool
    let stop: Bool
    let restart: Bool
    let kill: Bool
    let archive: Bool
    let tmuxAttach: Bool
    let tmuxText: Bool
    let verifiedPicker: Bool
    private enum CodingKeys: String, CodingKey { case structuredAnswer = "structured_answer", approvals, sendPrompt = "send_prompt", continueTurn = "continue_turn", retry, interrupt, start, stop, restart, kill, archive, tmuxAttach = "tmux_attach", tmuxText = "tmux_text", verifiedPicker = "verified_picker" }
}

struct FleetSession: Codable, Equatable {
    let sessionKey: String
    let provider: FleetProvider
    let providerSessionID: String?
    let tmuxTarget: String?
    let processStartFingerprint: String?
    let cwd: String
    let displayName: String?
    let lifecycle: LifecycleState
    let attention: AttentionState
    let currentRequestFingerprint: String?
    let currentRequest: JSONValue?
    let management: ManagementState
    let transportHealth: TransportHealth
    let capabilities: FleetCapabilities
    let provenance: FleetProvenance
    let confidence: FleetConfidence
    let discoveredAt: Int64
    let lastObservedAt: Int64
    let lifecycleUpdatedAt: Int64
    let attentionUpdatedAt: Int64
    let version: Int64
    let updatedRevision: Int64
    private enum CodingKeys: String, CodingKey { case sessionKey = "session_key", provider, providerSessionID = "provider_session_id", tmuxTarget = "tmux_target", processStartFingerprint = "process_start_fingerprint", cwd, displayName = "display_name", lifecycle, attention, currentRequestFingerprint = "current_request_fingerprint", currentRequest = "current_request", management, transportHealth = "transport_health", capabilities, provenance, confidence, discoveredAt = "discovered_at", lastObservedAt = "last_observed_at", lifecycleUpdatedAt = "lifecycle_updated_at", attentionUpdatedAt = "attention_updated_at", version, updatedRevision = "updated_revision" }
}

struct FleetSnapshot: Codable, Equatable {
    let headRevision: Int64
    let sessions: [FleetSession]
    private enum CodingKeys: String, CodingKey { case headRevision = "head_revision", sessions }
}

struct FleetSnapshotParams: Codable, Equatable { init() {} }

struct FleetSubscribeParams: Codable, Equatable {
    let afterRevision: Int64
    private enum CodingKeys: String, CodingKey { case afterRevision = "after_revision" }
}

enum FleetReplayResetReason: String, Codable, Equatable { case bootstrap, cursorAhead = "cursor_ahead", replayLimitExceeded = "replay_limit_exceeded" }

enum FleetReplayState: Codable, Equatable {
    case complete
    case snapshotReset(reason: FleetReplayResetReason)

    private enum CodingKeys: String, CodingKey { case state, reason }
    private enum Tag: String, Codable { case complete, snapshotReset = "snapshot_reset" }

    init(from decoder: Decoder) throws {
        let raw = try decoder.container(keyedBy: AnyCodingKey.self)
        guard raw.allKeys.allSatisfy({ $0.stringValue == "state" || $0.stringValue == "reason" }) else {
            throw DecodingError.dataCorruptedError(forKey: .state, in: try decoder.container(keyedBy: CodingKeys.self), debugDescription: "unknown replay state field")
        }
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(Tag.self, forKey: .state) {
        case .complete:
            guard !container.contains(.reason) else {
                throw DecodingError.dataCorruptedError(forKey: .reason, in: container, debugDescription: "complete replay cannot include reason")
            }
            self = .complete
        case .snapshotReset:
            self = .snapshotReset(reason: try container.decode(FleetReplayResetReason.self, forKey: .reason))
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .complete: try container.encode(Tag.complete, forKey: .state)
        case .snapshotReset(let reason):
            try container.encode(Tag.snapshotReset, forKey: .state)
            try container.encode(reason, forKey: .reason)
        }
    }
}

private struct AnyCodingKey: CodingKey {
    let stringValue: String
    let intValue: Int?

    init?(stringValue: String) { self.stringValue = stringValue; intValue = nil }
    init?(intValue: Int) { stringValue = String(intValue); self.intValue = intValue }
}

struct FleetEvent: Codable, Equatable {
    let revision: Int64
    let eventID: String
    let sessionKey: String
    let observedAt: Int64
    let provenance: FleetProvenance
    let eventType: String
    let payload: JSONValue
    let sessionVersion: Int64
    let applied: Bool
    private enum CodingKeys: String, CodingKey { case revision, eventID = "event_id", sessionKey = "session_key", observedAt = "observed_at", provenance, eventType = "event_type", payload, sessionVersion = "session_version", applied }
}

struct FleetSubscribeResult: Codable, Equatable {
    let snapshot: FleetSnapshot
    let replay: [FleetEvent]
    let replayState: FleetReplayState
    private enum CodingKeys: String, CodingKey { case snapshot, replay, replayState = "replay_state" }
}

struct FleetQuestionAnswer: Codable, Equatable {
    let questionID: String
    let selectedOptions: [String]
    let text: String?
    private enum CodingKeys: String, CodingKey { case questionID = "question_id", selectedOptions = "selected_options", text }
}

struct FleetRequestIdentity: Codable, Equatable {
    let requestID: JSONValue
    let threadID: String
    let turnID: String
    let itemID: String
    private enum CodingKeys: String, CodingKey { case requestID = "request_id", threadID = "thread_id", turnID = "turn_id", itemID = "item_id" }
}

enum ControlAction: Codable, Equatable {
    case structuredAnswer(requestFingerprint: String, requestIdentity: FleetRequestIdentity?, answers: [FleetQuestionAnswer])
    case approve(requestFingerprint: String, requestIdentity: FleetRequestIdentity?)
    case deny(requestFingerprint: String, requestIdentity: FleetRequestIdentity?)
    case verifiedPicker(requestFingerprint: String, key: String)
    case sendPrompt(text: String)
    case `continue`, retry, interrupt
    case start(provider: FleetProvider, cwd: String, prompt: String?)
    case restart, stop, kill, archive

    private enum CodingKeys: String, CodingKey { case action, requestFingerprint = "request_fingerprint", requestIdentity = "request_identity", answers, key, text, provider, cwd, prompt }
    private enum Tag: String, Codable { case structuredAnswer = "structured_answer", approve, deny, verifiedPicker = "verified_picker", sendPrompt = "send_prompt", `continue`, retry, interrupt, start, restart, stop, kill, archive }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(Tag.self, forKey: .action) {
        case .structuredAnswer: self = .structuredAnswer(requestFingerprint: try c.decode(String.self, forKey: .requestFingerprint), requestIdentity: try c.decodeIfPresent(FleetRequestIdentity.self, forKey: .requestIdentity), answers: try c.decode([FleetQuestionAnswer].self, forKey: .answers))
        case .approve: self = .approve(requestFingerprint: try c.decode(String.self, forKey: .requestFingerprint), requestIdentity: try c.decodeIfPresent(FleetRequestIdentity.self, forKey: .requestIdentity))
        case .deny: self = .deny(requestFingerprint: try c.decode(String.self, forKey: .requestFingerprint), requestIdentity: try c.decodeIfPresent(FleetRequestIdentity.self, forKey: .requestIdentity))
        case .verifiedPicker: self = .verifiedPicker(requestFingerprint: try c.decode(String.self, forKey: .requestFingerprint), key: try c.decode(String.self, forKey: .key))
        case .sendPrompt: self = .sendPrompt(text: try c.decode(String.self, forKey: .text))
        case .continue: self = .continue
        case .retry: self = .retry
        case .interrupt: self = .interrupt
        case .start: self = .start(provider: try c.decode(FleetProvider.self, forKey: .provider), cwd: try c.decode(String.self, forKey: .cwd), prompt: try c.decodeIfPresent(String.self, forKey: .prompt))
        case .restart: self = .restart
        case .stop: self = .stop
        case .kill: self = .kill
        case .archive: self = .archive
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .structuredAnswer(fingerprint, identity, answers):
            try c.encode(Tag.structuredAnswer, forKey: .action); try c.encode(fingerprint, forKey: .requestFingerprint); try c.encodeIfPresent(identity, forKey: .requestIdentity); try c.encode(answers, forKey: .answers)
        case let .approve(fingerprint, identity):
            try c.encode(Tag.approve, forKey: .action); try c.encode(fingerprint, forKey: .requestFingerprint); try c.encodeIfPresent(identity, forKey: .requestIdentity)
        case let .deny(fingerprint, identity):
            try c.encode(Tag.deny, forKey: .action); try c.encode(fingerprint, forKey: .requestFingerprint); try c.encodeIfPresent(identity, forKey: .requestIdentity)
        case let .verifiedPicker(fingerprint, key):
            try c.encode(Tag.verifiedPicker, forKey: .action); try c.encode(fingerprint, forKey: .requestFingerprint); try c.encode(key, forKey: .key)
        case let .sendPrompt(text): try c.encode(Tag.sendPrompt, forKey: .action); try c.encode(text, forKey: .text)
        case .continue: try c.encode(Tag.continue, forKey: .action)
        case .retry: try c.encode(Tag.retry, forKey: .action)
        case .interrupt: try c.encode(Tag.interrupt, forKey: .action)
        case let .start(provider, cwd, prompt): try c.encode(Tag.start, forKey: .action); try c.encode(provider, forKey: .provider); try c.encode(cwd, forKey: .cwd); try c.encodeIfPresent(prompt, forKey: .prompt)
        case .restart: try c.encode(Tag.restart, forKey: .action)
        case .stop: try c.encode(Tag.stop, forKey: .action)
        case .kill: try c.encode(Tag.kill, forKey: .action)
        case .archive: try c.encode(Tag.archive, forKey: .action)
        }
    }
}

struct FleetActionParams: Codable, Equatable {
    let sessionKey: String
    let expectedVersion: Int64
    let requestID: String
    let action: ControlAction
    private enum CodingKeys: String, CodingKey { case sessionKey = "session_key", expectedVersion = "expected_version", requestID = "request_id", action }
}

enum ActionReceiptStatus: String, Codable, Equatable { case pending = "PENDING", delivered = "DELIVERED", failed = "FAILED", unknown = "UNKNOWN", rejected = "REJECTED" }

struct FleetActionReceipt: Codable, Equatable {
    let requestID: String
    let sessionKey: String
    let actionKind: String
    let actionFingerprint: String
    let expectedVersion: Int64
    let idempotencyKey: String?
    let status: ActionReceiptStatus
    let detail: String?
    let sessionVersion: Int64?
    let createdAt: Int64
    let updatedAt: Int64
    private enum CodingKeys: String, CodingKey { case requestID = "request_id", sessionKey = "session_key", actionKind = "action_kind", actionFingerprint = "action_fingerprint", expectedVersion = "expected_version", idempotencyKey = "idempotency_key", status, detail, sessionVersion = "session_version", createdAt = "created_at", updatedAt = "updated_at" }
}

struct FleetActionResult: Codable, Equatable { let receipt: FleetActionReceipt }

struct FleetReceiptListParams: Codable, Equatable {
    let limit: UInt32
}

struct FleetReceiptListResult: Codable, Equatable {
    let receipts: [FleetActionReceipt]
}

struct FleetReceiptGetParams: Codable, Equatable {
    let requestID: String
    private enum CodingKeys: String, CodingKey { case requestID = "request_id" }
}

struct FleetReceiptGetResult: Codable, Equatable {
    let receipt: FleetActionReceipt?
}

enum FleetTimelineKind: String, Codable, Equatable {
    case sessionStarted = "session_started", turnRunning = "turn_running", questionRaised = "question_raised", approvalRequested = "approval_requested", attentionWaiting = "attention_waiting", turnCompleted = "turn_completed", turnFailed = "turn_failed", sessionEnded = "session_ended", managerUnavailable = "manager_unavailable", managerRecovered = "manager_recovered", managerStarted = "manager_started", transportUnavailable = "transport_unavailable", transportAvailable = "transport_available", sessionDiscovered = "session_discovered", sessionSuperseded = "session_superseded"
}

struct FleetTimelineParams: Codable, Equatable {
    let afterRevision: Int64?
    let sessionKey: String?
    let limit: UInt32
    private enum CodingKeys: String, CodingKey { case afterRevision = "after_revision", sessionKey = "session_key", limit }
}

struct FleetTimelineEntry: Codable, Equatable {
    let revision: Int64
    let sessionKey: String
    let observedAt: Int64
    let provenance: FleetProvenance
    let kind: FleetTimelineKind
    let applied: Bool
    let sessionVersion: Int64
    private enum CodingKeys: String, CodingKey { case revision, sessionKey = "session_key", observedAt = "observed_at", provenance, kind, applied, sessionVersion = "session_version" }
}

struct FleetTimelineResult: Codable, Equatable {
    let entries: [FleetTimelineEntry]
    let nextAfterRevision: Int64?
    private enum CodingKeys: String, CodingKey { case entries, nextAfterRevision = "next_after_revision" }
}

struct FleetStartParams: Codable, Equatable {
    let requestID: String
    let provider: FleetProvider
    let cwd: String
    let prompt: String?
    private enum CodingKeys: String, CodingKey { case requestID = "request_id", provider, cwd, prompt }
}

struct FleetStartResult: Codable, Equatable {
    let prospectiveSessionKey: String
    let receipt: FleetActionReceipt
    private enum CodingKeys: String, CodingKey { case prospectiveSessionKey = "prospective_session_key", receipt }
}

struct FleetBroadcastParams: Codable, Equatable {
    let targetKeys: [String]
    let text: String
    let idempotencyKey: String
    private enum CodingKeys: String, CodingKey { case targetKeys = "target_keys", text, idempotencyKey = "idempotency_key" }
}

struct FleetBroadcastResult: Codable, Equatable { let receipts: [FleetActionReceipt] }

enum AtcSchedulerOwnership: String, Codable, Equatable {
    case legacyTimerReconciliationRequired = "legacy_timer_reconciliation_required"
}

struct AtcListParams: Codable, Equatable { init() {} }

struct AtcInstance: Codable, Equatable {
    let name: String
    let cwd: String
    let tmuxSession: String?
    let heartbeatCron: String
    let errRetryCap: Int64
    let idlePauseMin: Int64
    let nextTickAt: Int64?
    let enabled: Bool
    let lastHeartbeatAt: Int64?
    let configGeneration: Int64

    private enum CodingKeys: String, CodingKey {
        case name, cwd, enabled
        case tmuxSession = "tmux_session"
        case heartbeatCron = "heartbeat_cron"
        case errRetryCap = "err_retry_cap"
        case idlePauseMin = "idle_pause_min"
        case nextTickAt = "next_tick_at"
        case lastHeartbeatAt = "last_heartbeat_at"
        case configGeneration = "config_generation"
    }
}

struct AtcListResult: Codable, Equatable {
    let instances: [AtcInstance]
    let schedulerOwnership: AtcSchedulerOwnership

    private enum CodingKeys: String, CodingKey {
        case instances
        case schedulerOwnership = "scheduler_ownership"
    }
}

enum FleetWire {
    static func decoder() -> JSONDecoder { JSONDecoder() }
    static func encoder() -> JSONEncoder {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return encoder
    }
}
