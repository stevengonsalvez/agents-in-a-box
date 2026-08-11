import Combine
import Foundation

enum FleetOperatorAction: CaseIterable, Identifiable, Equatable {
    case sendPrompt, continueTurn, retry, interrupt, restart, stop, kill, archive

    var id: String { title }

    var title: String {
        switch self {
        case .sendPrompt: "Send prompt"
        case .continueTurn: "Continue"
        case .retry: "Retry"
        case .interrupt: "Interrupt"
        case .restart: "Restart"
        case .stop: "Stop"
        case .kill: "Kill"
        case .archive: "Archive"
        }
    }

    var isDestructive: Bool {
        switch self {
        case .interrupt, .stop, .kill, .archive: true
        default: false
        }
    }

    func isAvailable(in capabilities: FleetCapabilities) -> Bool {
        switch self {
        case .sendPrompt: capabilities.sendPrompt || capabilities.tmuxText
        case .continueTurn: capabilities.continueTurn
        case .retry: capabilities.retry
        case .interrupt: capabilities.interrupt
        case .restart: capabilities.restart
        case .stop: capabilities.stop
        case .kill: capabilities.kill
        case .archive: capabilities.archive
        }
    }

    func wireAction(prompt: String = "") -> ControlAction {
        switch self {
        case .sendPrompt: .sendPrompt(text: prompt)
        case .continueTurn: .continue
        case .retry: .retry
        case .interrupt: .interrupt
        case .restart: .restart
        case .stop: .stop
        case .kill: .kill
        case .archive: .archive
        }
    }
}

enum FleetStartPreflight {
    static func supports(_ provider: FleetProvider) -> Bool {
        provider == .codex
    }

    static func isExistingDirectory(_ path: String) -> Bool {
        var isDirectory: ObjCBool = false
        return FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory) && isDirectory.boolValue
    }
}

enum FleetApprovalDecision: Equatable {
    case allowOnce, deny, bypassSession

    var title: String {
        switch self {
        case .allowOnce: "Allow once"
        case .deny: "Deny"
        case .bypassSession: "Always allow this session"
        }
    }

    func action(for session: FleetSession) -> ControlAction {
        let fingerprint = session.currentRequestFingerprint ?? ""
        let identity = FleetRequestIdentity.from(request: session.currentRequest)
        return switch self {
        case .allowOnce: .approve(requestFingerprint: fingerprint, requestIdentity: identity)
        case .deny: .deny(requestFingerprint: fingerprint, requestIdentity: identity)
        case .bypassSession: .approveForSession(requestFingerprint: fingerprint, requestIdentity: identity)
        }
    }
}

@MainActor
final class FleetStore: ObservableObject {
    @Published private(set) var sessions: [FleetSession] = []
    @Published private(set) var connectionState: FleetConnectionState = .connecting
    @Published var selectedSessionKey: String?
    @Published private(set) var receipts: [FleetActionReceipt] = []
    @Published private(set) var atcInstances: [AtcInstance] = []
    @Published private(set) var atcSchedulerOwnership: AtcSchedulerOwnership?
    @Published private(set) var timeline: [FleetTimelineEntry] = []
    @Published private(set) var usageSummary: FleetUsageSummaryResult?
    @Published private(set) var usageDashboard: FleetUsageDashboardResult?
    @Published private(set) var quotaSummary: FleetQuotaSummaryResult?
    @Published private(set) var runtimeStatus: FleetRuntimeStatusResult?
    @Published private(set) var chat = FleetChatSurface()
    @Published private(set) var pendingIntentID: String?
    @Published private(set) var controlNotice: String?
    @Published private(set) var lastStart: FleetStartResult?

    private let location: HangarLocation
    // Internal (not private) so the contract tests can assert the ranges the
    // SHIPPED store actually declares, not the FleetConnection defaults alone.
    let readVersions: FleetProtocolRange
    let writeVersions: FleetProtocolRange
    private let makeConnection: (HangarLocation) -> FleetConnection
    private let reconnectDelayNanoseconds: (Int) -> UInt64
    private let notificationCenter = FleetNotificationCenter()
    private var connection: FleetConnection?
    private var connectionTask: Task<Void, Never>?
    private var reconnectTask: Task<Void, Never>?
    private var authoritativeRepairTask: Task<Void, Never>?
    private var connectionGeneration: UInt = 0
    private var projection = FleetProjection.empty
    private var negotiation: FleetNegotiateResult?
    private var lastAuthoritativeRefresh: Date?
    private var reconnectAttempts = 0
    private var hasEstablishedLiveConnection = false
    private var liveConnectionStartedAt: Date?
    private var hasAppliedAuthoritativeSnapshot = false
    private let maximumReconnectAttempts = 3
    private let reconnectResetInterval: TimeInterval = 30

    init(
        location: HangarLocation = HangarLocation(),
        readVersions: FleetProtocolRange = FleetProtocolRange(min: 1, max: 2),
        writeVersions: FleetProtocolRange = FleetProtocolRange(min: 1, max: 2),
        makeConnection: @escaping (HangarLocation) -> FleetConnection = { FleetConnection(location: $0) },
        reconnectDelayNanoseconds: @escaping (Int) -> UInt64 = { UInt64($0) * 500_000_000 }
    ) {
        self.location = location
        self.readVersions = readVersions
        self.writeVersions = writeVersions
        self.makeConnection = makeConnection
        self.reconnectDelayNanoseconds = reconnectDelayNanoseconds
    }

    deinit {
        connectionTask?.cancel()
        reconnectTask?.cancel()
        authoritativeRepairTask?.cancel()
    }

    var activeCount: Int {
        sessions.filter { $0.lifecycle == .starting || $0.lifecycle == .running }.count
    }

    var needsYouCount: Int { sessions.filter { $0.attention != .none }.count }

    var canWrite: Bool {
        guard case let .live(_, writeCompatible) = connectionState else { return false }
        return writeCompatible && negotiation?.readCompatible == true
    }

    var canReadReceipts: Bool {
        connectionState.isLive && negotiation?.capabilityIDs.contains("fleet.receipt.read") == true
    }

    var canReadATC: Bool {
        connectionState.isLive && negotiation?.capabilityIDs.contains("fleet.atc.read") == true
    }

    var canReadTimeline: Bool {
        connectionState.isLive && negotiation?.capabilityIDs.contains("fleet.timeline.read") == true
    }

    var canReadUsage: Bool {
        connectionState.isLive && negotiation?.capabilityIDs.contains("fleet.usage.read") == true
    }

    var canReadDashboard: Bool {
        connectionState.isLive && negotiation?.capabilityIDs.contains("fleet.dashboard.read") == true
    }

    var canReadQuota: Bool {
        connectionState.isLive && negotiation?.capabilityIDs.contains("fleet.quota.read") == true
    }

    var canReadRuntime: Bool {
        connectionState.isLive && negotiation?.capabilityIDs.contains("fleet.runtime.read") == true
    }

    // MARK: - Fleet chat (buzz-port part 2, Phase A3)
    //
    // Each surface is gated on the capability its own daemon arm checks, and
    // gated SEPARATELY: a daemon built between phases serves the timeline while
    // answering -32601 for confirm cards, and a single combined gate would
    // either hide a working conversation or offer an approve button that
    // errors on the click.

    var canReadChat: Bool {
        guard connectionState.isLive, let capabilities = negotiation?.capabilityIDs else { return false }
        return capabilities.contains("fleet.chat.read") && capabilities.contains("fleet.message.read")
    }

    var canSendChat: Bool {
        canWrite && negotiation?.capabilityIDs.contains("fleet.message.send") == true && pendingIntentID == nil
    }

    var canAnswerConfirms: Bool {
        canWrite && negotiation?.capabilityIDs.contains("fleet.confirm.answer") == true && pendingIntentID == nil
    }

    var canStart: Bool {
        canWrite && negotiation?.capabilityIDs.contains("fleet.start.execute") == true && pendingIntentID == nil
    }

    var canBroadcast: Bool {
        canWrite && negotiation?.capabilityIDs.contains("fleet.broadcast.execute") == true && pendingIntentID == nil
    }

    #if DEBUG
    var debugConnectionTaskCount: Int {
        [connectionTask, reconnectTask].compactMap { $0 }.count
    }
    #endif

    var needsResubscribe: Bool { projection.needsResubscribe }

    func start() {
        guard connectionTask == nil, reconnectTask == nil else { return }
        connectionState = .connecting
        startAuthoritativeRepair()
        beginConnection()
    }

    func retry() {
        reconnectAttempts = 0
        hasEstablishedLiveConnection = false
        liveConnectionStartedAt = nil
        reconnectTask?.cancel()
        reconnectTask = nil
        connectionState = .connecting
        beginConnection()
    }

    func refresh() {
        guard let connection else { return }
        Task { [weak self] in
            guard let self else { return }
            await self.refreshAuthoritativeState(using: connection)
        }
    }

    func stop() {
        connectionGeneration &+= 1
        hasEstablishedLiveConnection = false
        liveConnectionStartedAt = nil
        connectionTask?.cancel()
        connectionTask = nil
        reconnectTask?.cancel()
        reconnectTask = nil
        authoritativeRepairTask?.cancel()
        authoritativeRepairTask = nil
        let currentConnection = connection
        connection = nil
        negotiation = nil
        Task { await currentConnection?.close() }
    }

    private func startAuthoritativeRepair() {
        guard authoritativeRepairTask == nil else { return }
        authoritativeRepairTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(10))
                guard let self, let connection = self.connection else { continue }
                await self.refreshAuthoritativeState(using: connection)
            }
        }
    }

    func canPerform(_ action: FleetOperatorAction, on session: FleetSession) -> Bool {
        canWrite
            && pendingIntentID == nil
            && negotiation?.capabilityIDs.contains("fleet.action.execute") == true
            && selectedSessionKey == session.sessionKey
            && !session.sessionKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && session.version > 0
            && action.isAvailable(in: session.capabilities)
    }

    func canSendPrompt(_ prompt: String, on session: FleetSession) -> Bool {
        canPerform(.sendPrompt, on: session)
            && !prompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    func perform(_ action: FleetOperatorAction, on session: FleetSession, prompt: String = "") {
        let trimmedPrompt = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        let allowed = action == .sendPrompt
            ? canSendPrompt(trimmedPrompt, on: session)
            : canPerform(action, on: session)
        guard allowed else {
            controlNotice = "Action is unavailable or incomplete."
            return
        }
        guard selectedSessionKey == session.sessionKey,
              let current = sessions.first(where: { $0.sessionKey == session.sessionKey }),
              current.version == session.version else {
            controlNotice = "Fleet state changed. Review the current session before sending."
            return
        }
        guard let connection else {
            controlNotice = "Fleet connection is unavailable."
            return
        }
        let requestID = UUID().uuidString
        pendingIntentID = requestID
        controlNotice = nil
        Task { [weak self] in
            guard let self else { return }
            defer { self.pendingIntentID = nil }
            do {
                let result = try await connection.action(FleetActionParams(
                    sessionKey: current.sessionKey,
                    expectedVersion: current.version,
                    requestID: requestID,
                    action: action.wireAction(prompt: trimmedPrompt)
                ))
                self.record(result.receipt)
                await self.refreshAuthoritativeState(using: connection)
            } catch {
                self.controlNotice = "Action refused: \(String(describing: error))"
            }
        }
    }

    func submitStructuredAnswers(_ answers: [FleetQuestionAnswer], on session: FleetSession) {
        guard !answers.isEmpty else {
            controlNotice = "Complete every interview question before submit."
            return
        }
        performStructured(
            .structuredAnswer(
                requestFingerprint: session.currentRequestFingerprint ?? "",
                requestIdentity: FleetRequestIdentity.from(request: session.currentRequest),
                answers: answers
            ),
            on: session,
            allowed: session.capabilities.structuredAnswer,
            failurePrefix: "Interview"
        )
    }

    func dismissStructuredInterview(on session: FleetSession) {
        performStructured(
            .dismissStructured(
                requestFingerprint: session.currentRequestFingerprint ?? "",
                requestIdentity: FleetRequestIdentity.from(request: session.currentRequest)
            ),
            on: session,
            allowed: session.provider == .claude && session.capabilities.structuredDismiss,
            failurePrefix: "Interview rejection"
        )
    }

    func openStructuredInterviewInClaude(on session: FleetSession) {
        performStructured(
            .releaseStructured(requestFingerprint: session.currentRequestFingerprint ?? ""),
            on: session,
            allowed: session.provider == .claude && session.capabilities.structuredAnswer,
            failurePrefix: "Open Claude picker"
        )
    }

    func canDecideApproval(_ decision: FleetApprovalDecision, on session: FleetSession) -> Bool {
        guard session.attention == .approval,
              session.capabilities.approvals,
              !session.currentRequestFingerprint.orEmpty.isEmpty else { return false }
        if decision == .bypassSession {
            return session.provider == .codex && session.capabilities.approvalSession && canPerformRequest(on: session)
        }
        return canPerformRequest(on: session)
    }

    func decideApproval(_ decision: FleetApprovalDecision, on session: FleetSession) {
        guard canDecideApproval(decision, on: session) else {
            controlNotice = "Approval action is unavailable or stale."
            return
        }
        performStructured(
            decision.action(for: session),
            on: session,
            allowed: true,
            failurePrefix: "Approval"
        )
    }

    private func canPerformRequest(on session: FleetSession) -> Bool {
        canWrite
            && pendingIntentID == nil
            && negotiation?.capabilityIDs.contains("fleet.action.execute") == true
            && selectedSessionKey == session.sessionKey
            && !session.sessionKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && session.version > 0
    }

    private func performStructured(
        _ action: ControlAction,
        on session: FleetSession,
        allowed: Bool,
        failurePrefix: String
    ) {
        guard canPerformRequest(on: session),
              allowed,
              let fingerprint = session.currentRequestFingerprint,
              !fingerprint.isEmpty,
              let connection else {
            controlNotice = "Interview action is unavailable or stale."
            return
        }
        guard let current = sessions.first(where: { $0.sessionKey == session.sessionKey }),
              current.version == session.version,
              current.currentRequestFingerprint == fingerprint else {
            controlNotice = "Fleet interview changed. Review current question before sending."
            return
        }
        let requestID = UUID().uuidString
        pendingIntentID = requestID
        controlNotice = nil
        Task { [weak self] in
            guard let self else { return }
            defer { self.pendingIntentID = nil }
            do {
                let result = try await connection.action(FleetActionParams(
                    sessionKey: current.sessionKey,
                    expectedVersion: current.version,
                    requestID: requestID,
                    action: action
                ))
                self.record(result.receipt)
                self.controlNotice = "Delivered. Confirming Fleet state."
                await self.refreshAuthoritativeState(using: connection)
            } catch {
                self.controlNotice = "\(failurePrefix) refused: \(String(describing: error))"
            }
        }
    }

    func start(provider: FleetProvider, cwd: String, prompt: String?) {
        let trimmedCWD = cwd.trimmingCharacters(in: .whitespacesAndNewlines)
        guard canStart,
              FleetStartPreflight.supports(provider),
              !trimmedCWD.isEmpty,
              FleetStartPreflight.isExistingDirectory(trimmedCWD),
              let connection else {
            controlNotice = "Start is unavailable or incomplete."
            return
        }
        let requestID = UUID().uuidString
        pendingIntentID = requestID
        controlNotice = nil
        Task { [weak self] in
            guard let self else { return }
            defer { self.pendingIntentID = nil }
            do {
                let result = try await connection.start(FleetStartParams(
                    requestID: requestID,
                    provider: provider,
                    cwd: trimmedCWD,
                    prompt: prompt?.trimmingCharacters(in: .whitespacesAndNewlines)
                ))
                self.lastStart = result
                self.record(result.receipt)
                await self.refreshAuthoritativeState(using: connection)
            } catch {
                self.controlNotice = "Start refused: \(String(describing: error))"
            }
        }
    }

    func broadcast(targetKeys: [String], text: String) {
        var seen = Set<String>()
        let orderedTargets = targetKeys.filter { seen.insert($0).inserted }
        let trimmedText = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard canBroadcast,
              !orderedTargets.isEmpty,
              !trimmedText.isEmpty,
              let connection,
              orderedTargets.allSatisfy({ key in
                  sessions.contains { $0.sessionKey == key && $0.version > 0 && ($0.capabilities.sendPrompt || $0.capabilities.tmuxText) }
              }) else {
            controlNotice = "Broadcast is unavailable or incomplete."
            return
        }
        let intentID = UUID().uuidString
        pendingIntentID = intentID
        controlNotice = nil
        Task { [weak self] in
            guard let self else { return }
            defer { self.pendingIntentID = nil }
            do {
                let result = try await connection.broadcast(FleetBroadcastParams(
                    targetKeys: orderedTargets,
                    text: trimmedText,
                    idempotencyKey: intentID
                ))
                self.record(result.receipts)
                await self.refreshAuthoritativeState(using: connection)
            } catch {
                self.controlNotice = "Broadcast refused: \(String(describing: error))"
            }
        }
    }

    func refreshReceipts() {
        guard canReadReceipts, let connection else {
            controlNotice = "Receipt reads are unavailable for this daemon."
            return
        }
        Task { [weak self] in
            guard let self else { return }
            do {
                self.receipts = try await connection.receiptList(FleetReceiptListParams(limit: 50)).receipts
            } catch {
                self.controlNotice = "Receipt refresh refused: \(String(describing: error))"
            }
        }
    }

    func refreshATC() {
        guard canReadATC, let connection else {
            controlNotice = "ATC reads are unavailable for this daemon."
            return
        }
        Task { [weak self] in
            guard let self else { return }
            do {
                let result = try await connection.atcList()
                atcInstances = result.instances
                atcSchedulerOwnership = result.schedulerOwnership
            } catch {
                controlNotice = "ATC refresh refused: \(String(describing: error))"
            }
        }
    }

    func refreshTimeline() {
        guard canReadTimeline, let connection else {
            controlNotice = "Timeline reads are unavailable for this daemon."
            return
        }
        Task { [weak self] in
            guard let self else { return }
            do {
                timeline = try await connection.timeline(FleetTimelineParams(afterRevision: nil, sessionKey: nil, limit: 100)).entries
            } catch {
                controlNotice = "Timeline refresh refused: \(String(describing: error))"
            }
        }
    }

    /// Usage is requested only from the Usage route or its explicit refresh
    /// control. It can scan local provider histories, so connection bootstrap
    /// must remain fast and side-effect free.
    func refreshUsage(period: FleetUsagePeriod = .trailing7Days) {
        guard canReadUsage, let connection else {
            usageSummary = nil
            controlNotice = "Usage is unavailable for this daemon."
            return
        }
        Task { [weak self] in
            guard let self else { return }
            do {
                usageSummary = try await connection.usageSummary(FleetUsageSummaryParams(period: period))
            } catch {
                usageSummary = nil
                controlNotice = "Usage refresh refused: \(String(describing: error))"
            }
        }
    }

    func refreshDashboard() {
        guard canReadDashboard, let connection else {
            usageDashboard = nil
            return
        }
        Task { [weak self] in
            guard let self else { return }
            do {
                usageDashboard = try await connection.usageDashboard()
            } catch {
                usageDashboard = nil
                controlNotice = "Dashboard refresh refused: \(String(describing: error))"
            }
        }
    }

    func refreshQuota() {
        guard canReadQuota, let connection else {
            quotaSummary = nil
            return
        }
        Task { [weak self] in
            guard let self else { return }
            do {
                quotaSummary = try await connection.quotaSummary()
            } catch {
                quotaSummary = nil
                controlNotice = "Quota refresh refused: \(String(describing: error))"
            }
        }
    }

    func refreshRuntime() {
        guard canReadRuntime, let connection else {
            runtimeStatus = nil
            return
        }
        Task { [weak self] in
            guard let self else { return }
            do {
                runtimeStatus = try await connection.runtimeStatus()
            } catch {
                runtimeStatus = nil
                controlNotice = "Runtime status refused: \(String(describing: error))"
            }
        }
    }

    // MARK: - Fleet chat

    /// Page the copilot conversation: timeline, confirm cards, activity.
    ///
    /// Mirrors the TUI's resolution (`ainb-core/src/fleet/control.rs`) step for
    /// step so the two clients cannot disagree about which channel `#copilot`
    /// is. Anything else and an operator watching both surfaces sees two
    /// conversations and has no way to tell which one the copilot is in.
    func refreshChat() {
        Task { [weak self] in await self?.refreshChatOnce() }
    }

    /// One page, awaited.
    ///
    /// The poll loop awaits THIS rather than firing `refreshChat()` on a timer:
    /// a page that takes longer than the interval would otherwise stack, and
    /// five overlapping RPC sets racing each other means whichever finishes
    /// last wins and the pane can go backwards in time.
    func refreshChatOnce() async {
        guard canReadChat, let connection else {
            // Assigned only on a real change: this runs once a second and a
            // @Published write redraws the pane whether or not the value moved.
            let unavailable = FleetChatSurface(sessionDetail: "Chat is unavailable for this daemon.")
            if chat != unavailable { chat = unavailable }
            return
        }
        do {
            // Same guard as the unavailable branch above, for the same reason:
            // this runs once a second and an unconditional @Published write
            // re-evaluates the whole window subtree even when nothing moved.
            // `FleetChatSurface` is Equatable, so the comparison is free.
            let paged = try await Self.pageChat(using: connection, canWrite: canWrite)
            if chat != paged { chat = paged }
        } catch {
            controlNotice = "Chat refresh refused: \(String(describing: error))"
        }
    }

    /// Post one operator message into the copilot channel.
    ///
    /// No `actor` rides this and none can: `FleetMessageSendParams` has no such
    /// field, so this surface cannot file a row under anybody but the operator
    /// the daemon already authenticated.
    func sendChatMessage(_ text: String) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard canSendChat,
              !trimmed.isEmpty,
              let scopeKey = chat.scopeKey,
              let target = chat.targetSessionKey,
              let connection else {
            controlNotice = "Chat send is unavailable or incomplete."
            return
        }
        let requestID = UUID().uuidString
        pendingIntentID = requestID
        controlNotice = nil
        Task { [weak self] in
            guard let self else { return }
            defer { self.pendingIntentID = nil }
            do {
                // The RESULT is the honest half. A 200 says the daemon accepted
                // the send, not that anybody received it: a lone leg coming back
                // REJECTED (`target_not_running`) or UNKNOWN
                // (`tmux_identity_unknown`) answers 200 too, and discarding it
                // would clear the composer, re-page, and leave the operator
                // reading their own message in the timeline as proof it landed.
                let result = try await connection.messageSend(FleetMessageSendParams(
                    scopeKey: scopeKey,
                    targets: [target],
                    originMessageID: nil,
                    text: trimmed,
                    requestID: requestID
                ))
                self.controlNotice = FleetChatLabels.deliverySummary(result.deliveries)
            } catch {
                self.controlNotice = "Chat send refused: \(String(describing: error))"
                return
            }
            self.chat = (try? await Self.pageChat(using: connection, canWrite: self.canWrite)) ?? self.chat
        }
    }

    /// Answer one guardrail confirm card.
    ///
    /// The card's own `isAnswerable` is the gate, not a state comparison
    /// rewritten here. A card whose state this build cannot name reports false
    /// and is refused, which is the same answer the button already gave by
    /// being disabled: this is the second lock, for the paths a disabled
    /// control does not cover (a keyboard shortcut, a card that expired between
    /// the render and the click).
    func answerConfirm(_ card: FleetChatConfirmCard, answer: FleetConfirmAnswer) {
        guard canAnswerConfirms, card.isAnswerable, let connection else {
            controlNotice = card.isAnswerable
                ? "Confirm answers are unavailable for this daemon."
                : card.refusal
            return
        }
        let intentID = UUID().uuidString
        pendingIntentID = intentID
        controlNotice = nil
        Task { [weak self] in
            guard let self else { return }
            defer { self.pendingIntentID = nil }
            do {
                let result = try await connection.confirmAnswer(
                    FleetConfirmAnswerParams(confirmID: card.id, answer: answer)
                )
                self.controlNotice = "Card \(result.confirmID) is \(FleetChatLabels.confirmState(result.state))."
            } catch {
                self.controlNotice = "Confirm answer refused: \(String(describing: error))"
            }
            self.chat = (try? await Self.pageChat(using: connection, canWrite: self.canWrite)) ?? self.chat
        }
    }

    /// Resolve the copilot channel and page everything filed under its scope.
    ///
    /// Only the timeline is fatal. The confirm and activity feeds degrade to an
    /// explained absence: a daemon built between phases answers -32601 for
    /// them, and a chat that refuses to render its conversation over that is a
    /// worse surface than one that says which half is missing.
    private static func pageChat(using connection: FleetConnection, canWrite: Bool) async throws -> FleetChatSurface {
        var surface = FleetChatSurface()
        let channels = try await connection.channelList().channels
        // Newest-wins, matching the TUI: a race that created two copilot
        // channels must not leave the two clients reading different ones.
        let existing = channels.last { $0.kind == .copilot }
        let channel: FleetChannel
        if let existing {
            channel = existing
        } else if canWrite {
            // Create-if-absent on a read path, deliberately: the copilot
            // channel is a singleton an operator expects to simply exist and
            // there is no other door to it here.
            channel = try await connection.channelCreate(
                FleetChannelCreateParams(kind: .copilot, name: "copilot", recipients: nil)
            ).channel
        } else {
            surface.sessionDetail = "No copilot channel yet, and this connection may not create one."
            return surface
        }
        surface.scopeKey = channel.scopeKey

        // A COPILOT channel carries no recipient list: its membership is the
        // ACP session that ANSWERS on the scope, so the recipient is resolved
        // the way it is minted, by creating that session against this scope.
        // The call is idempotent per live scope. The daemon's refusal is KEPT
        // rather than swallowed: its wording is the only actionable thing an
        // operator gets when a client in another directory already claimed it.
        if canWrite {
            do {
                surface.targetSessionKey = try await connection.acpSessionCreate(FleetAcpSessionCreateParams(
                    provider: copilotDefaultProvider,
                    cwd: FileManager.default.homeDirectoryForCurrentUser.path,
                    scopeKey: channel.scopeKey
                )).sessionKey
            } catch {
                surface.targetSessionKey = channel.recipients.first
                surface.sessionDetail = String(describing: error)
            }
        } else {
            surface.targetSessionKey = channel.recipients.first
            surface.sessionDetail = "This connection may not open a copilot session."
        }

        surface.messages = try await connection.messageList(FleetMessageListParams(
            scopeKey: channel.scopeKey,
            originID: nil,
            afterID: nil,
            limit: fleetMessageListMax
        )).messages.map(FleetChatMessageRow.init(message:))

        do {
            surface.confirms = try await connection
                .confirmList(FleetConfirmListParams(scopeKey: channel.scopeKey))
                .confirms
                .map(FleetChatConfirmCard.decode)
        } catch {
            surface.confirms = []
            surface.confirmsDetail = String(describing: error)
        }
        surface.activity = (try? await connection.activityList(FleetActivityListParams(
            scopeKey: channel.scopeKey,
            afterSeq: nil,
            limit: fleetActivityListMax
        )).activities) ?? []
        return surface
    }

    private func beginConnection() {
        connectionGeneration &+= 1
        let generation = connectionGeneration
        let currentConnection = connection
        connection = nil
        connectionTask?.cancel()
        connectionTask = Task { [weak self] in
            await currentConnection?.close()
            guard !Task.isCancelled else { return }
            await self?.connectAndConsume(generation: generation)
        }
    }

    private func connectAndConsume(generation: UInt) async {
        defer {
            if connectionGeneration == generation {
                connectionTask = nil
            }
        }
        let newConnection = makeConnection(location)
        connection = newConnection
        var established = false
        do {
            try await newConnection.connect()
            try await newConnection.authenticate(token: location.readToken())
            // Declaring the write range is load-bearing: omitting it left the
            // connection's default in charge, and a stale default silently
            // fails every action at requireWriteCapability after a bump.
            let result = try await newConnection.negotiate(readVersions: readVersions, writeVersions: writeVersions)
            negotiation = result
            let stream = await newConnection.incoming()
            let subscription = try await newConnection.subscribe(afterRevision: projection.committedRevision)
            let bootstrapped = FleetProjectionReducer.bootstrap(subscription)
            guard !bootstrapped.needsResubscribe else {
                projection = bootstrapped
                await reconnectOrBecomeUnavailable(
                    reason: "Fleet subscription requires resubscription",
                    connection: newConnection,
                    generation: generation
                )
                return
            }
            apply(bootstrapped)
            if result.capabilityIDs.contains("fleet.receipt.read") {
                do {
                    receipts = try await newConnection.receiptList(FleetReceiptListParams(limit: 50)).receipts
                } catch {}
            } else {
                receipts = []
            }
            if result.capabilityIDs.contains("fleet.atc.read") {
                do {
                    let atc = try await newConnection.atcList()
                    atcInstances = atc.instances
                    atcSchedulerOwnership = atc.schedulerOwnership
                } catch {}
            } else {
                atcInstances = []
                atcSchedulerOwnership = nil
            }
            if result.capabilityIDs.contains("fleet.timeline.read") {
                do {
                    timeline = try await newConnection.timeline(FleetTimelineParams(afterRevision: nil, sessionKey: nil, limit: 100)).entries
                } catch {}
            } else {
                timeline = []
            }
            if result.capabilityIDs.contains("fleet.runtime.read") {
                runtimeStatus = try? await newConnection.runtimeStatus()
            } else {
                runtimeStatus = nil
            }
            connectionState = .live(daemonVersion: result.daemonVersion, writeCompatible: result.writeCompatible)
            established = true
            hasEstablishedLiveConnection = true
            liveConnectionStartedAt = Date()

            for await incoming in stream {
                if Task.isCancelled || connectionGeneration != generation { return }
                if try await handle(incoming, on: newConnection, generation: generation) == false { return }
            }
            if !Task.isCancelled, connectionGeneration == generation {
                await reconnectOrBecomeUnavailable(
                    reason: "Fleet daemon connection closed",
                    connection: newConnection,
                    generation: generation
                )
            }
        } catch let error as FleetConnectionError {
            if case .protocolReadIncompatible = error {
                handle(error)
                await close(newConnection, ifCurrentGeneration: generation)
            } else if established || hasEstablishedLiveConnection {
                await reconnectOrBecomeUnavailable(
                    reason: error.localizedDescription,
                    connection: newConnection,
                    generation: generation
                )
            } else {
                handle(error)
                await close(newConnection, ifCurrentGeneration: generation)
            }
        } catch is CancellationError {
        } catch {
            if established || hasEstablishedLiveConnection {
                await reconnectOrBecomeUnavailable(
                    reason: error.localizedDescription,
                    connection: newConnection,
                    generation: generation
                )
            } else {
                handle(error)
                await close(newConnection, ifCurrentGeneration: generation)
            }
        }
    }

    private func handle(_ incoming: FleetIncoming, on connection: FleetConnection, generation: UInt) async throws -> Bool {
        switch incoming {
        case let .event(event):
            let next = FleetProjectionReducer.live(event, from: projection)
            if next.needsResubscribe {
                await reconnectOrBecomeUnavailable(
                    reason: "Fleet stream needs resubscription",
                    connection: connection,
                    generation: generation
                )
                return false
            }
            apply(next)
            if next.needsSnapshot {
                apply(FleetProjectionReducer.snapshot(try await connection.snapshot(), from: next))
            }
        case .resyncRequired:
            projection = FleetProjectionReducer.resyncRequired(from: projection)
            await reconnectOrBecomeUnavailable(
                reason: "Fleet daemon requested resync",
                connection: connection,
                generation: generation
            )
            return false
        case .unknownNotification:
            break
        }
        return true
    }

    private func apply(_ next: FleetProjection) {
        let previousSessions = sessions
        projection = next
        sessions = next.snapshot?.sessions ?? sessions
        if next.snapshot != nil {
            lastAuthoritativeRefresh = Date()
            if hasAppliedAuthoritativeSnapshot {
                let events = FleetNotificationPolicy.events(previous: previousSessions, current: sessions)
                Task { await notificationCenter.deliver(events) }
            }
            hasAppliedAuthoritativeSnapshot = true
        }
        if let selectedSessionKey, !sessions.contains(where: { $0.sessionKey == selectedSessionKey }) {
            self.selectedSessionKey = nil
        }
    }

    private func becomeStale(reason: String) {
        if sessions.isEmpty {
            connectionState = .unavailable(message: reason)
        } else {
            connectionState = .stale(lastUpdated: lastAuthoritativeRefresh ?? Date(), reason: reason)
        }
        negotiation = nil
    }

    private func reconnectOrBecomeUnavailable(
        reason: String,
        connection: FleetConnection,
        generation: UInt
    ) async {
        guard connectionGeneration == generation else { return }
        if let liveConnectionStartedAt,
           Date().timeIntervalSince(liveConnectionStartedAt) >= reconnectResetInterval {
            reconnectAttempts = 0
        }
        liveConnectionStartedAt = nil
        becomeStale(reason: reason)
        await close(connection, ifCurrentGeneration: generation)
        guard connectionGeneration == generation,
              reconnectAttempts < maximumReconnectAttempts,
              !Task.isCancelled else { return }
        reconnectAttempts += 1
        let delay = reconnectDelayNanoseconds(reconnectAttempts)
        reconnectTask?.cancel()
        reconnectTask = Task { @MainActor [weak self] in
            do {
                try await Task.sleep(nanoseconds: delay)
            } catch {
                return
            }
            guard let self,
                  !Task.isCancelled,
                  self.connectionGeneration == generation else { return }
            self.reconnectTask = nil
            self.connectionState = .connecting
            self.beginConnection()
        }
    }

    private func close(_ connection: FleetConnection, ifCurrentGeneration generation: UInt) async {
        await connection.close()
        guard connectionGeneration == generation, self.connection === connection else { return }
        self.connection = nil
    }

    private func handle(_ error: FleetConnectionError) {
        switch error {
        case let .protocolReadIncompatible(result):
            negotiation = result
            connectionState = .readIncompatible(daemonVersion: result.daemonVersion, protocolVersion: result.protocolVersion)
        default:
            becomeStale(reason: error.localizedDescription)
        }
    }

    private func handle(_ error: Error) {
        becomeStale(reason: error.localizedDescription)
    }

    private func record(_ receipt: FleetActionReceipt) {
        record([receipt])
    }

    private func record(_ incoming: [FleetActionReceipt]) {
        receipts = Self.mergedReceipts(incoming, existing: receipts)
    }

    static func mergedReceipts(_ incoming: [FleetActionReceipt], existing: [FleetActionReceipt]) -> [FleetActionReceipt] {
        let incomingIDs = Set(incoming.map(\.requestID))
        return incoming + existing.filter { !incomingIDs.contains($0.requestID) }
    }

    private func refreshAuthoritativeState(using connection: FleetConnection) async {
        do {
            let snapshot = try await connection.snapshot()
            apply(FleetProjectionReducer.snapshot(snapshot, from: projection))
        } catch {
            controlNotice = "Fleet refresh refused: \(String(describing: error))"
        }
        guard canReadReceipts else { return }
        do {
            receipts = try await connection.receiptList(FleetReceiptListParams(limit: 50)).receipts
        } catch {
            controlNotice = "Receipt refresh refused: \(String(describing: error))"
        }
    }
}

private extension Optional where Wrapped == String {
    var orEmpty: String { self ?? "" }
}
