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

@MainActor
final class FleetStore: ObservableObject {
    @Published private(set) var sessions: [FleetSession] = []
    @Published private(set) var connectionState: FleetConnectionState = .connecting
    @Published var selectedSessionKey: String?
    @Published private(set) var receipts: [FleetActionReceipt] = []
    @Published private(set) var atcInstances: [AtcInstance] = []
    @Published private(set) var atcSchedulerOwnership: AtcSchedulerOwnership?
    @Published private(set) var timeline: [FleetTimelineEntry] = []
    @Published private(set) var pendingIntentID: String?
    @Published private(set) var controlNotice: String?
    @Published private(set) var lastStart: FleetStartResult?

    private let location: HangarLocation
    private let readVersions: FleetProtocolRange
    private let makeConnection: (HangarLocation) -> FleetConnection
    private let reconnectDelayNanoseconds: (Int) -> UInt64
    private let notificationCenter = FleetNotificationCenter()
    private var connection: FleetConnection?
    private var connectionTask: Task<Void, Never>?
    private var reconnectTask: Task<Void, Never>?
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
        readVersions: FleetProtocolRange = FleetProtocolRange(min: 1, max: 1),
        makeConnection: @escaping (HangarLocation) -> FleetConnection = { FleetConnection(location: $0) },
        reconnectDelayNanoseconds: @escaping (Int) -> UInt64 = { UInt64($0) * 500_000_000 }
    ) {
        self.location = location
        self.readVersions = readVersions
        self.makeConnection = makeConnection
        self.reconnectDelayNanoseconds = reconnectDelayNanoseconds
    }

    deinit {
        connectionTask?.cancel()
        reconnectTask?.cancel()
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
        let currentConnection = connection
        connection = nil
        negotiation = nil
        Task { await currentConnection?.close() }
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
            let result = try await newConnection.negotiate(readVersions: readVersions)
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
