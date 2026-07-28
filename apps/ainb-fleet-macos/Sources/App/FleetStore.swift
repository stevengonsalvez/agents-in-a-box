import Combine
import Foundation

@MainActor
final class FleetStore: ObservableObject {
    @Published private(set) var sessions: [FleetSession] = []
    @Published private(set) var connectionState: FleetConnectionState = .connecting
    @Published var selectedSessionKey: String?

    private let location: HangarLocation
    private let readVersions: FleetProtocolRange
    private let makeConnection: (HangarLocation) -> FleetConnection
    private let reconnectDelayNanoseconds: (Int) -> UInt64
    private var connection: FleetConnection?
    private var connectionTask: Task<Void, Never>?
    private var reconnectTask: Task<Void, Never>?
    private var connectionGeneration: UInt = 0
    private var projection = FleetProjection.empty
    private var negotiation: FleetNegotiateResult?
    private var lastAuthoritativeRefresh: Date?
    private var reconnectAttempts = 0
    private var hasEstablishedLiveConnection = false
    private let maximumReconnectAttempts = 3

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
        reconnectTask?.cancel()
        reconnectTask = nil
        connectionState = .connecting
        beginConnection()
    }

    func stop() {
        connectionGeneration &+= 1
        hasEstablishedLiveConnection = false
        connectionTask?.cancel()
        connectionTask = nil
        reconnectTask?.cancel()
        reconnectTask = nil
        let currentConnection = connection
        connection = nil
        Task { await currentConnection?.close() }
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
            connectionState = .live(daemonVersion: result.daemonVersion, writeCompatible: result.writeCompatible)
            established = true
            if reconnectAttempts > 0 {
                reconnectAttempts = 0
            }
            hasEstablishedLiveConnection = true

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
        projection = next
        sessions = next.snapshot?.sessions ?? sessions
        if next.snapshot != nil { lastAuthoritativeRefresh = Date() }
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
}
