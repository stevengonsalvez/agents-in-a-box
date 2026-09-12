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

/// `auth/hello` params in their final D17 shape.
///
/// `protocol` and `capabilities` are declared, never assumed: a daemon that
/// predates the negotiation ignores both members and answers the bare `{}` it
/// always did, which is exactly what `AuthHelloResult`'s defaults read as.
struct AuthHelloParams: Codable, Equatable {
    let token: String
    let surface: AuthHelloSurface?
    let protocolRange: HangarProtocolRange
    let capabilities: [String]

    private enum CodingKeys: String, CodingKey {
        case token
        case surface
        case protocolRange = "protocol"
        case capabilities
    }

    init(
        token: String,
        surface: AuthHelloSurface? = nil,
        protocolRange: HangarProtocolRange = .supported,
        capabilities: [String] = HangarCapability.clientCatalogue
    ) {
        self.token = token
        self.surface = surface
        self.protocolRange = protocolRange
        self.capabilities = capabilities
    }

    /// Tolerant decode: a frame from a pre-W0-wire client carries only `token`,
    /// and reading it has to yield "speaks version 1, declares nothing" rather
    /// than a decoding error. That is the N-1-client leg of the skew matrix.
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        token = try container.decode(String.self, forKey: .token)
        surface = try container.decodeIfPresent(AuthHelloSurface.self, forKey: .surface)
        protocolRange =
            try container.decodeIfPresent(HangarProtocolRange.self, forKey: .protocolRange)
            ?? .legacy
        capabilities = try container.decodeIfPresent([String].self, forKey: .capabilities) ?? []
    }
}

/// The surface metadata the live connection registry records.
struct AuthHelloSurface: Codable, Equatable {
    let kind: String
    let host: String?
    let pid: Int32?
}

/// An inclusive Hangar protocol-version range.
struct HangarProtocolRange: Codable, Equatable {
    let min: UInt32
    let max: UInt32

    /// What this build speaks. Mirrors `ainb_hangar_proto::protocol`.
    static let supported = HangarProtocolRange(min: 1, max: 1)
    /// What a peer that declared nothing is: version 1 and only 1.
    static let legacy = HangarProtocolRange(min: 1, max: 1)
}

/// The `auth/hello` reply.
///
/// EVERY member is optional with a legacy default, because a pre-W0-wire daemon
/// answers `{}` and that has to decode, it is the N-1-daemon leg of the skew
/// matrix, not an error.
struct AuthHelloResult: Decodable, Equatable {
    let protocolRange: HangarProtocolRange
    let selected: UInt32?
    let capabilities: [String]
    let daemonVersion: String?

    private enum CodingKeys: String, CodingKey {
        case protocolRange = "protocol"
        case selected
        case capabilities
        case daemonVersion = "daemon_version"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        protocolRange =
            try container.decodeIfPresent(HangarProtocolRange.self, forKey: .protocolRange)
            ?? .legacy
        selected = try container.decodeIfPresent(UInt32.self, forKey: .selected)
        capabilities = try container.decodeIfPresent([String].self, forKey: .capabilities) ?? []
        daemonVersion = try container.decodeIfPresent(String.self, forKey: .daemonVersion)
    }

    init(
        protocolRange: HangarProtocolRange = .legacy,
        selected: UInt32? = nil,
        capabilities: [String] = [],
        daemonVersion: String? = nil
    ) {
        self.protocolRange = protocolRange
        self.selected = selected
        self.capabilities = capabilities
        self.daemonVersion = daemonVersion
    }

    /// The negotiated version, treating a silent daemon as protocol 1.
    var selectedOrLegacy: UInt32 { selected ?? 1 }

    /// Whether the daemon advertised `capability`.
    func advertises(_ capability: String) -> Bool { capabilities.contains(capability) }
}

/// The capability strings this client understands.
///
/// Deliberately short: a client declares what it can READ, not the daemon's
/// whole catalogue. Adding one here is a statement that this app handles it.
enum HangarCapability {
    static let authHelloNegotiated = "hangar.auth.hello.negotiated"
    static let mutationOpID = "hangar.mutation.op_id"
    static let mutationReceipt = "hangar.mutation.receipt"
    static let socketVersioned = "hangar.socket.versioned"

    static let clientCatalogue: [String] = [
        authHelloNegotiated,
        mutationOpID,
        mutationReceipt,
        socketVersioned,
    ]
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

extension JSONValue {
    var objectValue: [String: JSONValue]? {
        guard case let .object(value) = self else { return nil }
        return value
    }

    var arrayValue: [JSONValue]? {
        guard case let .array(value) = self else { return nil }
        return value
    }

    var stringValue: String? {
        guard case let .string(value) = self else { return nil }
        return value
    }

    var boolValue: Bool? {
        guard case let .bool(value) = self else { return nil }
        return value
    }

    func value(_ names: String...) -> JSONValue? {
        guard let object = objectValue else { return nil }
        return names.lazy.compactMap { object[$0] }.first
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

enum FleetProvider: String, Encodable, Equatable { case claude, codex, copilot, acp, antigravity, unknown }
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
    let approvalSession: Bool
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
    let structuredDismiss: Bool
    private enum CodingKeys: String, CodingKey { case structuredAnswer = "structured_answer", approvals, approvalSession = "approval_session", sendPrompt = "send_prompt", continueTurn = "continue_turn", retry, interrupt, start, stop, restart, kill, archive, tmuxAttach = "tmux_attach", tmuxText = "tmux_text", verifiedPicker = "verified_picker", structuredDismiss = "structured_dismiss" }

    init(
        structuredAnswer: Bool,
        approvals: Bool,
        sendPrompt: Bool,
        continueTurn: Bool,
        retry: Bool,
        interrupt: Bool,
        start: Bool,
        stop: Bool,
        restart: Bool,
        kill: Bool,
        archive: Bool,
        tmuxAttach: Bool,
        tmuxText: Bool,
        verifiedPicker: Bool,
        structuredDismiss: Bool = false,
        approvalSession: Bool = false
    ) {
        self.structuredAnswer = structuredAnswer
        self.approvals = approvals
        self.approvalSession = approvalSession
        self.sendPrompt = sendPrompt
        self.continueTurn = continueTurn
        self.retry = retry
        self.interrupt = interrupt
        self.start = start
        self.stop = stop
        self.restart = restart
        self.kill = kill
        self.archive = archive
        self.tmuxAttach = tmuxAttach
        self.tmuxText = tmuxText
        self.verifiedPicker = verifiedPicker
        self.structuredDismiss = structuredDismiss
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        self.init(
            structuredAnswer: try c.decodeIfPresent(Bool.self, forKey: .structuredAnswer) ?? false,
            approvals: try c.decodeIfPresent(Bool.self, forKey: .approvals) ?? false,
            sendPrompt: try c.decodeIfPresent(Bool.self, forKey: .sendPrompt) ?? false,
            continueTurn: try c.decodeIfPresent(Bool.self, forKey: .continueTurn) ?? false,
            retry: try c.decodeIfPresent(Bool.self, forKey: .retry) ?? false,
            interrupt: try c.decodeIfPresent(Bool.self, forKey: .interrupt) ?? false,
            start: try c.decodeIfPresent(Bool.self, forKey: .start) ?? false,
            stop: try c.decodeIfPresent(Bool.self, forKey: .stop) ?? false,
            restart: try c.decodeIfPresent(Bool.self, forKey: .restart) ?? false,
            kill: try c.decodeIfPresent(Bool.self, forKey: .kill) ?? false,
            archive: try c.decodeIfPresent(Bool.self, forKey: .archive) ?? false,
            tmuxAttach: try c.decodeIfPresent(Bool.self, forKey: .tmuxAttach) ?? false,
            tmuxText: try c.decodeIfPresent(Bool.self, forKey: .tmuxText) ?? false,
            verifiedPicker: try c.decodeIfPresent(Bool.self, forKey: .verifiedPicker) ?? false,
            structuredDismiss: try c.decodeIfPresent(Bool.self, forKey: .structuredDismiss) ?? false,
            approvalSession: try c.decodeIfPresent(Bool.self, forKey: .approvalSession) ?? false
        )
    }
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
    let activeWorkCount: Int64?
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
    /// Provider-reported model id, verbatim. Absent until the daemon has
    /// actually observed one: nil means "never observed", never "default model".
    /// Declared WITHOUT an initialiser on purpose -- a `let` carrying one is
    /// skipped by the synthesized decoder and would be permanently nil.
    let model: String?
    let reasoningEffort: String?
    let modelUpdatedAt: Int64?
    private enum CodingKeys: String, CodingKey { case sessionKey = "session_key", provider, providerSessionID = "provider_session_id", tmuxTarget = "tmux_target", processStartFingerprint = "process_start_fingerprint", cwd, displayName = "display_name", lifecycle, activeWorkCount = "active_work_count", attention, currentRequestFingerprint = "current_request_fingerprint", currentRequest = "current_request", management, transportHealth = "transport_health", capabilities, provenance, confidence, discoveredAt = "discovered_at", lastObservedAt = "last_observed_at", lifecycleUpdatedAt = "lifecycle_updated_at", attentionUpdatedAt = "attention_updated_at", version, updatedRevision = "updated_revision", model, reasoningEffort = "reasoning_effort", modelUpdatedAt = "model_updated_at" }
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

extension FleetRequestIdentity {
    static func from(request: JSONValue?) -> FleetRequestIdentity? {
        guard let request else { return nil }
        let payload = request.value("payload") ?? request
        let identity = payload.value("identity") ?? payload
        guard let requestID = identity.value("requestId", "request_id", "tool_use_id", "id") else {
            return nil
        }
        return FleetRequestIdentity(
            requestID: requestID,
            threadID: identity.value("threadId", "thread_id")?.stringValue ?? "",
            turnID: identity.value("turnId", "turn_id")?.stringValue ?? "",
            itemID: identity.value("itemId", "item_id")?.stringValue ?? ""
        )
    }
}

enum ControlAction: Codable, Equatable {
    case structuredAnswer(requestFingerprint: String, requestIdentity: FleetRequestIdentity?, answers: [FleetQuestionAnswer])
    case dismissStructured(requestFingerprint: String, requestIdentity: FleetRequestIdentity?)
    case releaseStructured(requestFingerprint: String)
    /// Ask the daemon whether this Claude interview is still live.
    ///
    /// A VERIFICATION, not a decision: it answers nothing and rejects nothing.
    /// The daemon asks the notifyd broker whether the request is still waiting
    /// and, for a native or mirrored picker only, whether that picker has
    /// closed. A card it cannot prove either way is KEPT, which is why an
    /// `UNKNOWN` receipt from this action is a normal outcome rather than a
    /// failure. See `FleetStore.reconcileNotice`.
    ///
    /// Gated by `structured_answer`, the SAME capability the answer and the
    /// release use, not one of its own: `action_capability` in the daemon's
    /// `rpc/mod.rs` groups the three together.
    case reconcileStructured(requestFingerprint: String)
    case approve(requestFingerprint: String, requestIdentity: FleetRequestIdentity?)
    case approveForSession(requestFingerprint: String, requestIdentity: FleetRequestIdentity?)
    case deny(requestFingerprint: String, requestIdentity: FleetRequestIdentity?)
    case verifiedPicker(requestFingerprint: String, key: String)
    case sendPrompt(text: String)
    case `continue`, retry, interrupt
    case start(provider: FleetProvider, cwd: String, prompt: String?)
    case restart, stop, kill, archive

    private enum CodingKeys: String, CodingKey { case action, requestFingerprint = "request_fingerprint", requestIdentity = "request_identity", answers, key, text, provider, cwd, prompt }
    private enum Tag: String, Codable { case structuredAnswer = "structured_answer", dismissStructured = "dismiss_structured", releaseStructured = "release_structured", reconcileStructured = "reconcile_structured", approve, approveForSession = "approve_for_session", deny, verifiedPicker = "verified_picker", sendPrompt = "send_prompt", `continue`, retry, interrupt, start, restart, stop, kill, archive }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(Tag.self, forKey: .action) {
        case .structuredAnswer: self = .structuredAnswer(requestFingerprint: try c.decode(String.self, forKey: .requestFingerprint), requestIdentity: try c.decodeIfPresent(FleetRequestIdentity.self, forKey: .requestIdentity), answers: try c.decode([FleetQuestionAnswer].self, forKey: .answers))
        case .dismissStructured: self = .dismissStructured(requestFingerprint: try c.decode(String.self, forKey: .requestFingerprint), requestIdentity: try c.decodeIfPresent(FleetRequestIdentity.self, forKey: .requestIdentity))
        case .releaseStructured: self = .releaseStructured(requestFingerprint: try c.decode(String.self, forKey: .requestFingerprint))
        case .reconcileStructured: self = .reconcileStructured(requestFingerprint: try c.decode(String.self, forKey: .requestFingerprint))
        case .approve: self = .approve(requestFingerprint: try c.decode(String.self, forKey: .requestFingerprint), requestIdentity: try c.decodeIfPresent(FleetRequestIdentity.self, forKey: .requestIdentity))
        case .approveForSession: self = .approveForSession(requestFingerprint: try c.decode(String.self, forKey: .requestFingerprint), requestIdentity: try c.decodeIfPresent(FleetRequestIdentity.self, forKey: .requestIdentity))
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
        case let .dismissStructured(fingerprint, identity):
            try c.encode(Tag.dismissStructured, forKey: .action); try c.encode(fingerprint, forKey: .requestFingerprint); try c.encodeIfPresent(identity, forKey: .requestIdentity)
        case let .releaseStructured(fingerprint):
            try c.encode(Tag.releaseStructured, forKey: .action); try c.encode(fingerprint, forKey: .requestFingerprint)
        case let .reconcileStructured(fingerprint):
            try c.encode(Tag.reconcileStructured, forKey: .action); try c.encode(fingerprint, forKey: .requestFingerprint)
        case let .approve(fingerprint, identity):
            try c.encode(Tag.approve, forKey: .action); try c.encode(fingerprint, forKey: .requestFingerprint); try c.encodeIfPresent(identity, forKey: .requestIdentity)
        case let .approveForSession(fingerprint, identity):
            try c.encode(Tag.approveForSession, forKey: .action); try c.encode(fingerprint, forKey: .requestFingerprint); try c.encodeIfPresent(identity, forKey: .requestIdentity)
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

enum ActionReceiptStatus: String, Codable, Equatable, CaseIterable { case pending = "PENDING", delivered = "DELIVERED", failed = "FAILED", unknown = "UNKNOWN", rejected = "REJECTED" }

extension ActionReceiptStatus {
    /// The ONE operator-facing word for a status on this surface.
    ///
    /// DELIBERATELY PROSE, not the wire token. `receipt_status_token` on the
    /// daemon side returns the uppercase wire words (`FAILED`, `REJECTED`, …)
    /// for logs and CLI output; this reads inside an English sentence, so it is
    /// lowercase and renders `.unknown` as a phrase. The divergence is intended
    /// — do not "fix" it by aligning to the Rust strings.
    ///
    /// Exhaustive on purpose: a new variant must be a compile error here rather
    /// than silently rendering as whichever arm was written last.
    var operatorToken: String {
        switch self {
        case .pending: return "pending"
        case .delivered: return "delivered"
        case .failed: return "failed"
        case .rejected: return "rejected"
        case .unknown: return "could not be confirmed"
        }
    }
}

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

enum FleetUsagePeriod: String, Codable, CaseIterable, Identifiable {
    case today
    case trailing7Days = "trailing_7_days"
    case trailing30Days = "trailing_30_days"

    var id: Self { self }

    var label: String {
        switch self {
        case .today: "Today"
        case .trailing7Days: "7 days"
        case .trailing30Days: "30 days"
        }
    }
}

struct FleetUsageSummaryParams: Codable, Equatable {
    let period: FleetUsagePeriod
}

enum FleetUsageSummaryState: String, Encodable, Equatable {
    case scanning, ready, partial, unavailable
}

// Usage state gates a whole panel, and both the summary and the dashboard embed
// it. Left as a plain synthesized `Codable` it threw on any value this build did
// not know, so a daemon adding one state blanked the entire panel rather than
// degrading it. Unknown degrades to `.unavailable`: the panel then renders its
// explanatory empty state instead of over-promising `.ready` with no data.
extension FleetUsageSummaryState: TolerantWireEnum { static var wireFallback: Self { .unavailable } }

struct FleetUsageBucket: Codable, Equatable {
    let inputTokens: UInt64
    let cacheCreationTokens: UInt64
    let cacheReadTokens: UInt64
    let outputTokens: UInt64
    let reasoningTokens: UInt64
    let callCount: UInt64
    let sessionCount: UInt64
    let projectCount: UInt64
    let costUSD: Double?

    var totalTokens: UInt64 {
        inputTokens + cacheCreationTokens + cacheReadTokens + outputTokens + reasoningTokens
    }

    private enum CodingKeys: String, CodingKey {
        case inputTokens = "input_tokens"
        case cacheCreationTokens = "cache_creation_tokens"
        case cacheReadTokens = "cache_read_tokens"
        case outputTokens = "output_tokens"
        case reasoningTokens = "reasoning_tokens"
        case callCount = "call_count"
        case sessionCount = "session_count"
        case projectCount = "project_count"
        case costUSD = "cost_usd"
    }
}

struct FleetUsageDailyBucket: Codable, Equatable {
    let date: String
    let bucket: FleetUsageBucket
}

struct FleetUsageProviderBucket: Codable, Equatable {
    let provider: String
    let bucket: FleetUsageBucket
}

struct FleetUsageModelBucket: Codable, Equatable {
    let model: String
    let bucket: FleetUsageBucket
}

struct FleetUsageProjectBucket: Codable, Equatable {
    let project: String
    let repo: String?
    let bucket: FleetUsageBucket
}

struct FleetUsageSummaryResult: Codable, Equatable {
    let state: FleetUsageSummaryState
    let generatedAt: Int64?
    let startAt: Int64?
    let endAt: Int64?
    let totals: FleetUsageBucket?
    let daily: [FleetUsageDailyBucket]
    let providers: [FleetUsageProviderBucket]
    let models: [FleetUsageModelBucket]
    let projects: [FleetUsageProjectBucket]
    let detail: String?

    private enum CodingKeys: String, CodingKey {
        case state
        case generatedAt = "generated_at"
        case startAt = "start_at"
        case endAt = "end_at"
        case totals, daily, providers, models, projects, detail
    }
}

// MARK: - fleet/usage_dashboard

struct FleetUsageDashboardParams: Codable, Equatable { init() {} }

struct FleetHeatmapCell: Codable, Equatable {
    let date: String
    let callCount: UInt64
    let costUSD: Double?

    private enum CodingKeys: String, CodingKey {
        case date
        case callCount = "call_count"
        case costUSD = "cost_usd"
    }
}

struct FleetUsageWeeklyBucket: Codable, Equatable {
    let weekStart: String
    let bucket: FleetUsageBucket

    private enum CodingKeys: String, CodingKey {
        case weekStart = "week_start"
        case bucket
    }
}

struct FleetUsageSessionBucket: Codable, Equatable {
    let sessionID: String
    let provider: String
    let project: String
    let bucket: FleetUsageBucket

    private enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case provider, project, bucket
    }
}

struct FleetUsageBranchBucket: Codable, Equatable {
    let branch: String
    let bucket: FleetUsageBucket
}

struct FleetUsageNamedBucket: Codable, Equatable {
    let name: String
    let callCount: UInt64

    private enum CodingKeys: String, CodingKey {
        case name
        case callCount = "call_count"
    }
}

struct FleetUsageForecast: Codable, Equatable {
    let projected30dCostUSD: Double?
    let projected30dTokens: UInt64
    let avgDailyCostUSD: Double?
    let avgDailyTokens: UInt64
    let sampleDays: UInt32

    private enum CodingKeys: String, CodingKey {
        case projected30dCostUSD = "projected_30d_cost_usd"
        case projected30dTokens = "projected_30d_tokens"
        case avgDailyCostUSD = "avg_daily_cost_usd"
        case avgDailyTokens = "avg_daily_tokens"
        case sampleDays = "sample_days"
    }
}

struct FleetUsageDashboardResult: Codable, Equatable {
    let state: FleetUsageSummaryState
    let generatedAt: Int64?
    let startAt: Int64?
    let endAt: Int64?
    let costComplete: Bool
    let totals: FleetUsageBucket?
    let weekly: [FleetUsageWeeklyBucket]
    let heatmap: [FleetHeatmapCell]
    let forecast: FleetUsageForecast?
    let providers: [FleetUsageProviderBucket]
    let models: [FleetUsageModelBucket]
    let projects: [FleetUsageProjectBucket]
    let sessions: [FleetUsageSessionBucket]
    let branches: [FleetUsageBranchBucket]
    let tools: [FleetUsageNamedBucket]
    let mcpServers: [FleetUsageNamedBucket]
    let shellCommands: [FleetUsageNamedBucket]
    let detail: String?

    private enum CodingKeys: String, CodingKey {
        case state
        case generatedAt = "generated_at"
        case startAt = "start_at"
        case endAt = "end_at"
        case costComplete = "cost_complete"
        case totals, weekly, heatmap, forecast
        case providers, models, projects, sessions, branches
        case tools
        case mcpServers = "mcp_servers"
        case shellCommands = "shell_commands"
        case detail
    }
}

struct FleetQuotaSummaryParams: Codable, Equatable { init() {} }

struct FleetQuotaWindow: Codable, Equatable {
    let usedPercent: UInt8
    let resetsAt: Int64?
    let estimated: Bool

    var remainingPercent: UInt8 { 100 - min(usedPercent, 100) }

    private enum CodingKeys: String, CodingKey {
        case usedPercent = "used_percent"
        case resetsAt = "resets_at"
        case estimated
    }
}

struct FleetQuotaProvider: Codable, Equatable {
    let provider: String
    let fiveHour: FleetQuotaWindow?
    let sevenDay: FleetQuotaWindow?
    let planType: String?
    let updatedAt: Int64?

    private enum CodingKeys: String, CodingKey {
        case provider
        case fiveHour = "five_hour"
        case sevenDay = "seven_day"
        case planType = "plan_type"
        case updatedAt = "updated_at"
    }
}

struct FleetQuotaSummaryResult: Codable, Equatable {
    let state: FleetUsageSummaryState
    let generatedAt: Int64?
    let providers: [FleetQuotaProvider]
    let detail: String?

    private enum CodingKeys: String, CodingKey {
        case state
        case generatedAt = "generated_at"
        case providers, detail
    }
}

struct FleetRuntimeStatusParams: Codable, Equatable { init() {} }

struct FleetRuntimeHookStatus: Codable, Equatable {
    let provider: String
    let installed: Bool
    let hookReady: Bool
    let deliveryReady: Bool
    let lastEvent: String?

    private enum CodingKeys: String, CodingKey {
        case provider, installed
        case hookReady = "hook_ready"
        case deliveryReady = "delivery_ready"
        case lastEvent = "last_event"
    }
}

struct FleetRuntimeStatusResult: Codable, Equatable {
    let daemonVersion: String
    let protocolVersion: UInt32
    let hooks: [FleetRuntimeHookStatus]

    private enum CodingKeys: String, CodingKey {
        case daemonVersion = "daemon_version"
        case protocolVersion = "protocol_version"
        case hooks
    }
}

// MARK: - Fleet chat, Pal and guardrails (buzz-port part 2)
//
// These frames are CAPABILITY-ONLY until the daemon advertises
// fleet.chat.read / .write / fleet.copilot.configure / fleet.confirm.answer:
// part 2's methods answer -32601 in a daemon built between phases, so nothing
// here may be reached without checking the negotiated catalogue first.
//
// Every enum below is a TolerantWireEnum for the same reason the provider and
// lifecycle enums are: these are ONE value inside a decoded array, so a token
// this build has never heard of must degrade that one value, never fail the
// whole page and blank the pane. The fallbacks are chosen fail-SAFE, which for
// a confirm card means "not answerable" and for an activity class means "warn
// louder", never the reverse.

// `pal` keeps the raw value `copilot`: the surface was renamed, the wire was
// not, so the daemon's stored channel kind still reads `copilot`.
enum FleetChannelKind: String, Encodable, Equatable, CaseIterable {
    case pal = "copilot"
    case broadcast
    case unknown
}
// A channel whose kind this build cannot name is still a channel with a
// timeline, so it stays listed rather than vanishing from the sidebar.
extension FleetChannelKind: TolerantWireEnum { static var wireFallback: Self { .unknown } }

/// The Pal channel's guardrail dial.
///
/// NOT the adapter's permission mode, and no relation to it. This one moves the
/// daemon-side fleet-tool classifier: which of Pal's own tools fire,
/// which take a confirm card, and which are not offered at all. The adapter's
/// permission mode stays pinned at `session/new` under every value here.
enum FleetPalMode: String, Encodable, Equatable, CaseIterable {
    case help, guarded, yolo, unknown
}
extension FleetPalMode: TolerantWireEnum { static var wireFallback: Self { .unknown } }

enum FleetConfirmState: String, Encodable, Equatable, CaseIterable { case open, approved, denied, expired, unknown }
// NOT `.open`: a card in a state this build cannot name must never render as
// answerable, or the UI offers an approve button for a lifecycle it does not
// understand. Unknown is terminal-looking on purpose.
extension FleetConfirmState: TolerantWireEnum { static var wireFallback: Self { .unknown } }

enum FleetActivityClass: String, Encodable, Equatable, CaseIterable { case read, write, destructive, unknown }
// An unknown guardrail class over-warns rather than under-warns: rendering a
// future class as `unknown` next to the destructive styling is recoverable,
// silently rendering it as a harmless read is not.
extension FleetActivityClass: TolerantWireEnum { static var wireFallback: Self { .unknown } }

enum FleetActivityOutcome: String, Encodable, Equatable, CaseIterable { case ok, denied, expired, error, unknown }
extension FleetActivityOutcome: TolerantWireEnum { static var wireFallback: Self { .unknown } }

/// Chat message kind on the wire.
///
/// The Rust enum has no `unknown` arm: it does not need one, because a Rust
/// client decodes a message row into a `Result` it can drop. Swift decodes the
/// page as an ARRAY, so one future kind would fail every row on it. Same
/// reasoning as `FleetProvider`, same fallback shape.
enum FleetMessageKind: String, Encodable, Equatable, CaseIterable { case user, agent, marker, unknown }
extension FleetMessageKind: TolerantWireEnum { static var wireFallback: Self { .unknown } }

/// One persisted chat message.
///
/// `sender` is the daemon's record of WHO WROTE IT, taken from the send's
/// `actor` and never from the body. It is the whole reason a Pal write
/// cannot masquerade as a human's, so nothing in this client may synthesise or
/// default it: see `FleetChatActor`, which maps it for display and refuses to
/// read a blank one as the operator.
struct FleetMessage: Codable, Equatable {
    let id: String
    let scopeKey: String
    let originMessageID: String?
    let sender: String
    let kind: FleetMessageKind
    let body: String
    let createdAt: Int64

    private enum CodingKeys: String, CodingKey {
        case id, sender, kind, body
        case scopeKey = "scope_key"
        case originMessageID = "origin_message_id"
        case createdAt = "created_at"
    }
}

/// Params for `fleet/message_send`.
///
/// There is deliberately NO `actor` field. The wire key exists so a Pal
/// write is distinguishable from a human one, and this client is a human
/// surface: omitting the key is exactly what the daemon defaults to
/// (`operator`). Modelling it as a settable property would hand every caller
/// here the ability to file a message under somebody else's name, which is the
/// single guarantee `sender` exists to provide.
struct FleetMessageSendParams: Encodable, Equatable {
    let scopeKey: String?
    let targets: [String]
    let originMessageID: String?
    let text: String
    let requestID: String

    private enum CodingKeys: String, CodingKey {
        case targets, text
        case scopeKey = "scope_key"
        case originMessageID = "origin_message_id"
        case requestID = "request_id"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encodeIfPresent(scopeKey, forKey: .scopeKey)
        try container.encode(targets, forKey: .targets)
        try container.encodeIfPresent(originMessageID, forKey: .originMessageID)
        try container.encode(text, forKey: .text)
        try container.encode(requestID, forKey: .requestID)
    }
}

/// One recipient's leg of a send, with the daemon's REASON when it has one.
///
/// `detail` is not decoration: `REJECTED` alone does not tell an operator
/// whether to retry or to go and look at the session, while `REJECTED
/// claude:gone · target_not_running` does. The TUI's `receipt_line` and the
/// CLI's `render_delivery` both print it, so a client that dropped the key
/// would be the one surface unable to say why. The daemon omits the key when
/// it has no reason, which a plain optional decodes.
struct FleetMessageDelivery: Codable, Equatable {
    let sessionKey: String
    let state: ActionReceiptStatus
    let detail: String?
    private enum CodingKeys: String, CodingKey { case sessionKey = "session_key", state, detail }
}

struct FleetMessageSendResult: Codable, Equatable {
    let messageID: String
    let deliveries: [FleetMessageDelivery]
    private enum CodingKeys: String, CodingKey { case messageID = "message_id", deliveries }
}

struct FleetMessageListParams: Encodable, Equatable {
    let scopeKey: String?
    let originID: String?
    let afterID: String?
    let limit: UInt32

    private enum CodingKeys: String, CodingKey {
        case limit
        case scopeKey = "scope_key"
        case originID = "origin_id"
        case afterID = "after_id"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encodeIfPresent(scopeKey, forKey: .scopeKey)
        try container.encodeIfPresent(originID, forKey: .originID)
        try container.encodeIfPresent(afterID, forKey: .afterID)
        try container.encode(limit, forKey: .limit)
    }
}

/// Maximum rows one `fleet/message_list` page may return
/// (`FLEET_MESSAGE_LIST_MAX`). The daemon clamps, so asking for more is not an
/// error, but asking for the wrong number silently pages differently from the
/// TUI reading the same conversation.
let fleetMessageListMax: UInt32 = 100
/// Maximum rows one `fleet/activity_list` page may return
/// (`FLEET_ACTIVITY_LIST_MAX`).
let fleetActivityListMax: UInt32 = 200

struct FleetMessageListResult: Codable, Equatable {
    let messages: [FleetMessage]
    let nextAfterID: String?
    private enum CodingKeys: String, CodingKey { case messages, nextAfterID = "next_after_id" }
}

/// Params for `fleet/message_subscribe`.
///
/// `after_id` is OMITTED when nil rather than sent as `null`, matching the
/// daemon's `skip_serializing_if`. Absent means "start at the head the ack
/// publishes", which is what a client that has not read the log yet wants: a
/// `null` cursor and an absent one mean the same thing here, but a client that
/// sends the field it does not mean is one wire change away from meaning it.
struct FleetMessageSubscribeParams: Encodable, Equatable {
    let afterID: String?

    private enum CodingKeys: String, CodingKey { case afterID = "after_id" }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encodeIfPresent(afterID, forKey: .afterID)
    }
}

/// Result for `fleet/message_subscribe`: the newest committed message id, or
/// nil on an empty log.
///
/// The ack is not the point of the call; the `fleet/message_event`,
/// `fleet/confirm_event` and `fleet/activity_event` notifications that follow
/// it on the SAME socket are. The daemon starts the forwarder at this head, so
/// a client that pages after subscribing cannot miss a message between the two.
struct FleetMessageSubscribeResult: Codable, Equatable {
    let headID: String?

    private enum CodingKeys: String, CodingKey { case headID = "head_id" }
}

/// Payload of the `fleet/message_event` notification.
struct FleetMessageEventParams: Codable, Equatable {
    let message: FleetMessage
}

/// Adapter token named ONLY on the legacy rung of the mint ladder.
///
/// The same string the TUI keeps for the same reason (`LEGACY_DAEMON_ADAPTER`):
/// a daemon built before `provider` became optional refuses an absent one, and
/// this is what this client sent before that. It is NOT what a normal attach
/// names. The daemon binds a scope to the adapter the FIRST
/// `fleet/acp_session_create` names, so naming a guess reverts an engine the
/// operator swapped and is refused outright once the scope is held by another.
let palDefaultProvider = "claude-agent-acp"

/// Parameters for `fleet/acp_session_create`.
///
/// Both `provider` and `cwd` are OMITTED from the frame when nil, never sent as
/// `null`: absent is what the daemon reads as "whatever this scope already
/// runs, wherever it already runs it", and that is the only shape that attaches
/// to a session opened from a directory this client cannot know.
struct FleetAcpSessionCreateParams: Encodable, Equatable {
    let provider: String?
    let cwd: String?
    let scopeKey: String?

    private enum CodingKeys: String, CodingKey { case provider, cwd, scopeKey = "scope_key" }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encodeIfPresent(provider, forKey: .provider)
        try container.encodeIfPresent(cwd, forKey: .cwd)
        try container.encodeIfPresent(scopeKey, forKey: .scopeKey)
    }
}

struct FleetAcpSessionCreateResult: Codable, Equatable {
    let sessionKey: String
    let scopeKey: String
    /// The pool's wall-clock ceiling on ONE turn, in milliseconds.
    ///
    /// Daemon-private otherwise: it lives on the daemon's pool config, so a
    /// client reading the environment variable that moves it would be reading
    /// its own process. Absent from a daemon built before it, and absent is not
    /// 30 minutes: a pane that gets nil says nothing about a deadline rather
    /// than inventing one. Nothing displays it yet; decoding it is what pins
    /// the contract before the pane that bounds a PENDING leg needs it.
    let turnDeadlineMs: Int64?

    private enum CodingKeys: String, CodingKey {
        case sessionKey = "session_key"
        case scopeKey = "scope_key"
        case turnDeadlineMs = "turn_deadline_ms"
    }
}

struct FleetChannel: Codable, Equatable {
    let id: String
    let kind: FleetChannelKind
    let name: String
    let scopeKey: String
    let recipients: [String]
    let createdAt: Int64

    private enum CodingKeys: String, CodingKey {
        case id, kind, name, recipients
        case scopeKey = "scope_key"
        case createdAt = "created_at"
    }
}

struct FleetChannelCreateParams: Codable, Equatable {
    let kind: FleetChannelKind
    let name: String
    let recipients: [String]?
}

struct FleetChannelCreateResult: Codable, Equatable {
    let channel: FleetChannel
}

struct FleetChannelListParams: Codable, Equatable { init() {} }

struct FleetChannelListResult: Codable, Equatable {
    let channels: [FleetChannel]
}

/// One ACP adapter the daemon's registry can spawn.
///
/// The registry is `[acp.adapters.*]` in the host config plus the built-in
/// floor, so it grows by editing config rather than by shipping a build. That
/// is why `name` is a STRING everywhere it appears on the Pal wire and why
/// the engine picker reads this list instead of a set compiled in here: an
/// adapter an operator installed is selectable without a new client.
struct FleetAdapter: Codable, Equatable, Identifiable {
    /// The registry key, and the token `provider` carries on a configure.
    let name: String
    /// The program the daemon spawns, as configured.
    let command: String
    /// The mode pinned at `session/new`. Reported so an operator can SEE it; it
    /// is not settable over this wire, deliberately.
    let permissionMode: String
    /// Whether this came from the built-in floor rather than config.
    let builtIn: Bool
    /// The model ids an operator declared, in picker order.
    ///
    /// EMPTY IS THE ANSWER, not a missing one: ACP has no model-discovery call,
    /// so an empty list means the adapter runs its own default and the picker
    /// must say that rather than offering a guess.
    let models: [String]

    var id: String { name }

    private enum CodingKeys: String, CodingKey {
        case name, command, models
        case permissionMode = "permission_mode"
        case builtIn = "built_in"
    }

    init(name: String, command: String, permissionMode: String, builtIn: Bool, models: [String]) {
        self.name = name
        self.command = command
        self.permissionMode = permissionMode
        self.builtIn = builtIn
        self.models = models
    }

    /// Hand-written because `models` is `#[serde(default)]` on the Rust side.
    /// The synthesized decoder would reject a daemon that omitted the key, and
    /// an adapter with no declared models is the ordinary case rather than an
    /// error.
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        name = try container.decode(String.self, forKey: .name)
        command = try container.decode(String.self, forKey: .command)
        permissionMode = try container.decode(String.self, forKey: .permissionMode)
        builtIn = try container.decode(Bool.self, forKey: .builtIn)
        models = try container.decodeIfPresent([String].self, forKey: .models) ?? []
    }
}

struct FleetAdapterListParams: Codable, Equatable { init() {} }

struct FleetAdapterListResult: Codable, Equatable {
    let adapters: [FleetAdapter]
}

/// Params for `fleet/copilot_configure`.
///
/// There is deliberately no permission-mode field: the mode is daemon config
/// and a settable one would be a remote off-switch for the guardrails.
struct FleetPalConfigureParams: Codable, Equatable {
    /// An adapter name from `fleet/adapter_list`.
    ///
    /// A STRING, not an enum: the registry is `[acp.adapters.*]` plus the
    /// built-in floor, so a closed enum here could not name an adapter the
    /// daemon can already spawn. The daemon validates it.
    let provider: String
    /// The channel's guardrail dial; `nil` leaves it where it is.
    ///
    /// Spelled `copilot_mode`, never `mode`: `mode` is one of the keys this
    /// method refuses outright, because the setting an operator would most
    /// plausibly send under that name is the adapter permission mode.
    let palMode: FleetPalMode?
    let model: String?
    let reasoningEffort: String?
    let persona: String?

    private enum CodingKeys: String, CodingKey {
        case provider, model, persona
        case palMode = "copilot_mode"
        case reasoningEffort = "reasoning_effort"
    }
}

struct FleetPalConfigureResult: Codable, Equatable {
    let sessionKey: String
    let provider: String
    let palMode: FleetPalMode
    /// Whether this call retired the previous session to swap the adapter. A
    /// caller holding the old `sessionKey` is holding a dead session.
    let sessionReplaced: Bool
    let model: String?
    let reasoningEffort: String?
    let personaSet: Bool

    private enum CodingKeys: String, CodingKey {
        case provider, model
        case sessionKey = "session_key"
        case palMode = "copilot_mode"
        case sessionReplaced = "session_replaced"
        case reasoningEffort = "reasoning_effort"
        case personaSet = "persona_set"
    }
}

/// One guardrail confirm card: a Pal tool call held for an operator.
///
/// NOT an ACP permission request — those stay attention rows answered through
/// `fleet/action`, and the two must not be merged in the UI.
struct FleetConfirm: Codable, Equatable {
    let confirmID: String
    let scopeKey: String
    let tool: String
    let arguments: JSONValue
    let targetSessionKey: String?
    let state: FleetConfirmState
    let createdAt: Int64
    let expiresAt: Int64

    private enum CodingKeys: String, CodingKey {
        case tool, arguments, state
        case confirmID = "confirm_id"
        case scopeKey = "scope_key"
        case targetSessionKey = "target_session_key"
        case createdAt = "created_at"
        case expiresAt = "expires_at"
    }

    /// Only an `open` card may be answered. An unknown state is not open, so
    /// this is also the guard for a token from a newer daemon.
    var isAnswerable: Bool { state == .open }
}

struct FleetConfirmListParams: Encodable, Equatable {
    let scopeKey: String?
    private enum CodingKeys: String, CodingKey { case scopeKey = "scope_key" }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encodeIfPresent(scopeKey, forKey: .scopeKey)
    }
}

struct FleetConfirmListResult: Codable, Equatable {
    let confirms: [FleetConfirm]
}

/// The same frame decoded ROW BY ROW.
///
/// `FleetConfirmListResult` decodes `confirms` as a typed array, so a single
/// row this build cannot decode (a renamed key, a retyped field: the failures a
/// tolerant ENUM does not cover) fails the whole page, and the operator loses
/// every card this build does understand along with the one it does not. The
/// UI reads this shape instead and classifies each row itself; the typed result
/// stays for the contract tests, which SHOULD fail on drift.
struct FleetConfirmListRawResult: Decodable, Equatable {
    let confirms: [JSONValue]
}

/// The answer to a confirm card, internally tagged on the wire (`answer`),
/// mirroring `ControlAction`'s `action` tag.
///
/// This is a frame this client AUTHORS, so an unrecognised tag throws rather
/// than degrading: tolerance is for values the daemon sends us.
enum FleetConfirmAnswer: Equatable {
    case approve
    case deny
    case edit(arguments: JSONValue)
}

struct FleetConfirmAnswerParams: Codable, Equatable {
    let confirmID: String
    let answer: FleetConfirmAnswer

    private enum CodingKeys: String, CodingKey {
        case confirmID = "confirm_id"
        case answer, arguments
    }

    init(confirmID: String, answer: FleetConfirmAnswer) {
        self.confirmID = confirmID
        self.answer = answer
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        confirmID = try container.decode(String.self, forKey: .confirmID)
        switch try container.decode(String.self, forKey: .answer) {
        case "approve": answer = .approve
        case "deny": answer = .deny
        case "edit": answer = .edit(arguments: try container.decode(JSONValue.self, forKey: .arguments))
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .answer, in: container, debugDescription: "unknown confirm answer \(other)")
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(confirmID, forKey: .confirmID)
        switch answer {
        case .approve: try container.encode("approve", forKey: .answer)
        case .deny: try container.encode("deny", forKey: .answer)
        case let .edit(arguments):
            try container.encode("edit", forKey: .answer)
            try container.encode(arguments, forKey: .arguments)
        }
    }
}

struct FleetConfirmAnswerResult: Codable, Equatable {
    let confirmID: String
    let state: FleetConfirmState

    private enum CodingKeys: String, CodingKey {
        case state
        case confirmID = "confirm_id"
    }
}

/// Payload of the `fleet/confirm_event` notification.
struct FleetConfirmEventParams: Codable, Equatable {
    let confirm: FleetConfirm
}

/// The same frame with its card left RAW.
///
/// The same split, for the same reason, as `FleetConfirmListRawResult`: a card
/// carrying one key this build cannot read must still reach the operator as an
/// unanswerable row, and the typed shape above cannot express that. The live
/// path decodes this and hands the value to `FleetChatConfirmCard.decode`, the
/// identical function the paged list uses, so a card renders the same whether
/// it arrived on the stream or in a page. The typed shape stays for the
/// contract tests, which SHOULD fail on drift.
struct FleetConfirmEventRawParams: Decodable, Equatable {
    let confirm: JSONValue
}

/// One append-only Pal activity row.
///
/// `seq` is the commit-ordered cursor and the ONLY paging key; `id` is stable
/// identity and never an ordering key.
struct FleetActivityRow: Codable, Equatable {
    let seq: Int64
    let id: String
    let scopeKey: String
    let tool: String
    let activityClass: FleetActivityClass
    let targetSessionKey: String?
    let outcome: FleetActivityOutcome
    let detail: String?
    let createdAt: Int64

    private enum CodingKeys: String, CodingKey {
        case seq, id, tool, outcome, detail
        case activityClass = "class"
        case scopeKey = "scope_key"
        case targetSessionKey = "target_session_key"
        case createdAt = "created_at"
    }
}

struct FleetActivityListParams: Codable, Equatable {
    let scopeKey: String?
    let afterSeq: Int64?
    let limit: UInt32

    private enum CodingKeys: String, CodingKey {
        case limit
        case scopeKey = "scope_key"
        case afterSeq = "after_seq"
    }
}

struct FleetActivityListResult: Codable, Equatable {
    let activities: [FleetActivityRow]
    let nextAfterSeq: Int64?

    private enum CodingKeys: String, CodingKey {
        case activities
        case nextAfterSeq = "next_after_seq"
    }
}

/// Payload of the `fleet/activity_event` notification.
struct FleetActivityEventParams: Codable, Equatable {
    let activity: FleetActivityRow
}

// MARK: - fleet/transcript_* (the ACP execution stream)
//
// Addressed by SESSION, not by scope, which is the one structural difference
// from the three chat frames above: a transcript belongs to the ACP session
// that produced it, and `fleet/transcript_subscribe` names that session. The
// gate on all of it is `fleet.transcript.read`, which is what BOTH daemon arms
// check (`handle_fleet_transcript_list`, `handle_fleet_transcript_subscribe`).

/// One ACP transcript chunk: a `fleet_provider_event` row with `source='acp'`.
///
/// `payload` is arbitrary adapter-authored JSON, decoded as `JSONValue` rather
/// than as a typed shape, because the taxonomy that reads it
/// (`AcpTranscriptClassifier`) is deliberately total: a row type this build
/// does not carry yields nothing rather than failing the page it arrived on.
struct FleetTranscriptChunk: Codable, Equatable, Sendable {
    /// Commit-ordered transcript cursor (`ingest_order`).
    let ingestOrder: Int64
    /// Replay-safe chunk identity.
    let eventID: String
    /// The owning Fleet session.
    let sessionKey: String
    /// Normalized discriminator, `acp.<kind>`.
    let eventType: String
    /// Normalized chunk body.
    let payload: JSONValue
    /// Observation time in epoch milliseconds.
    let observedAt: Int64

    private enum CodingKeys: String, CodingKey {
        case payload
        case ingestOrder = "ingest_order"
        case eventID = "event_id"
        case sessionKey = "session_key"
        case eventType = "event_type"
        case observedAt = "observed_at"
    }
}

/// Maximum chunks one `fleet/transcript_list` page may return
/// (`FLEET_TRANSCRIPT_LIST_MAX`). The daemon clamps, so asking for more is not
/// an error, but asking for the wrong number silently pages differently from
/// the CLI reading the same transcript.
let fleetTranscriptListMax: UInt32 = 100

/// Params for `fleet/transcript_list`.
///
/// `after_order` is OMITTED when nil rather than sent as `null`, matching the
/// daemon's `skip_serializing_if`, for the same reason `FleetMessageListParams`
/// omits its cursor.
struct FleetTranscriptListParams: Encodable, Equatable {
    let sessionKey: String
    let afterOrder: Int64?
    let limit: UInt32

    private enum CodingKeys: String, CodingKey {
        case limit
        case sessionKey = "session_key"
        case afterOrder = "after_order"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(sessionKey, forKey: .sessionKey)
        try container.encodeIfPresent(afterOrder, forKey: .afterOrder)
        try container.encode(limit, forKey: .limit)
    }
}

/// Result for `fleet/transcript_list`: chunks in ascending `ingest_order`.
///
/// The page walks FORWARD from `after_order`, so a page taken from the start of
/// a long transcript is its OLDEST hundred chunks. A tail view has to name a
/// window that ends at the head, which is why the store subscribes first and
/// pages from the head the ack publishes.
struct FleetTranscriptListResult: Codable, Equatable {
    let chunks: [FleetTranscriptChunk]
    let nextAfterOrder: Int64?
    /// Whether the daemon left older rows behind.
    ///
    /// NOT inferable from `chunks.count`, which is the whole reason it is on
    /// the wire. The uncursored read is bounded twice, by rows and by payload
    /// bytes, and which bound bit depends on the run: a short page can mean a
    /// short transcript or one 4 KiB chunk after another. A pane that read a
    /// short page as completeness would render a partial run as a whole one.
    ///
    /// Defaulted rather than required, so a daemon built before the flag reads
    /// as "nothing was left behind", which is what its unbounded page meant.
    let truncated: Bool

    private enum CodingKeys: String, CodingKey {
        case chunks, truncated
        case nextAfterOrder = "next_after_order"
    }

    init(chunks: [FleetTranscriptChunk], nextAfterOrder: Int64?, truncated: Bool = false) {
        self.chunks = chunks
        self.nextAfterOrder = nextAfterOrder
        self.truncated = truncated
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.init(
            chunks: try container.decode([FleetTranscriptChunk].self, forKey: .chunks),
            nextAfterOrder: try container.decodeIfPresent(Int64.self, forKey: .nextAfterOrder),
            truncated: try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        )
    }
}

/// Params for `fleet/transcript_subscribe`.
///
/// An absent `after_order` starts the forwarder at the head the ack publishes;
/// a named one asks the daemon to replay from that cursor. The daemon refuses a
/// NEGATIVE order outright, so nothing here may send one.
struct FleetTranscriptSubscribeParams: Encodable, Equatable {
    let sessionKey: String
    let afterOrder: Int64?

    private enum CodingKeys: String, CodingKey {
        case sessionKey = "session_key"
        case afterOrder = "after_order"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(sessionKey, forKey: .sessionKey)
        try container.encodeIfPresent(afterOrder, forKey: .afterOrder)
    }
}

/// Result for `fleet/transcript_subscribe`: the newest committed
/// `ingest_order` for the session, or nil on an empty transcript.
///
/// The ack is not the point of the call; the `fleet/transcript_event`
/// notifications that follow it on the SAME socket are. The daemon registers
/// the forwarder at this head AFTER the response is queued, and the forwarder
/// reads everything past its cursor before it waits, so a client that pages up
/// to this head cannot miss a chunk between the two.
struct FleetTranscriptSubscribeResult: Codable, Equatable {
    let headOrder: Int64?

    private enum CodingKeys: String, CodingKey { case headOrder = "head_order" }
}

/// Payload of the `fleet/transcript_event` notification.
struct FleetTranscriptEventParams: Codable, Equatable, Sendable {
    let chunk: FleetTranscriptChunk
}

enum FleetWire {
    static func decoder() -> JSONDecoder { JSONDecoder() }
    static func encoder() -> JSONEncoder {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return encoder
    }
}
