import Darwin
import Foundation

enum FleetConnectionError: Error, Equatable {
    case alreadyConnected
    case notConnected
    case notAuthenticated
    case protocolReadIncompatible(FleetNegotiateResult)
    case protocolWriteIncompatible
    case missingNegotiatedCapability(String)
    case emptyToken
    case closed
    case disconnected
    case malformedEnvelope
    case unknownResponseID
    case rpc(RPCError)
    case socket(Int32)
}

struct FleetResyncRequired: Decodable, Equatable, Sendable {
    let afterRevision: Int64
    let missed: Int64

    private enum CodingKeys: String, CodingKey {
        case afterRevision = "after_revision"
        case missed
    }
}

enum FleetIncoming: Equatable, Sendable {
    case event(FleetEvent)
    case resyncRequired(FleetResyncRequired)
    case unknownNotification(String)
}

actor FleetConnection {
    private let location: HangarLocation
    private var injectedDescriptor: Int32?
    private var descriptor: Int32?
    private var readerTask: Task<Void, Never>?
    private var pending: [RPCID: CheckedContinuation<Data, Error>] = [:]
    private var notificationStreams: [UUID: AsyncStream<FleetIncoming>.Continuation] = [:]
    private var nextRequestID: Int64 = 1
    private var authenticated = false
    private var negotiation: FleetNegotiateResult?

    init(location: HangarLocation = HangarLocation(), injectedDescriptor: Int32? = nil) {
        self.location = location
        self.injectedDescriptor = injectedDescriptor
    }

    deinit {
        if let descriptor {
            Darwin.close(descriptor)
        }
        readerTask?.cancel()
    }

    func connect() throws {
        guard descriptor == nil else {
            throw FleetConnectionError.alreadyConnected
        }

        let connectedDescriptor: Int32
        if let injectedDescriptor {
            self.injectedDescriptor = nil
            connectedDescriptor = injectedDescriptor
        } else {
            connectedDescriptor = try Self.connectUnixSocket(at: location.socketURL)
        }
        descriptor = connectedDescriptor
        readerTask = Task.detached { [weak self] in
            guard let connection = self else { return }
            let error = await Self.readFrames(from: connectedDescriptor) { frame in
                await connection.receive(frame)
            }
            guard !Task.isCancelled else { return }
            await connection.disconnect(error, descriptor: connectedDescriptor)
        }
    }

    func authenticate(token: String) async throws {
        guard descriptor != nil else {
            throw FleetConnectionError.notConnected
        }
        guard !token.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw FleetConnectionError.emptyToken
        }
        struct EmptyResult: Decodable {}
        _ = try await request("auth/hello", params: AuthHelloParams(token: token), result: EmptyResult.self)
        authenticated = true
    }

    func negotiate(
        clientName: String = "ainb-fleet-macos",
        clientVersion: String = "0.1.0",
        readVersions: FleetProtocolRange = FleetProtocolRange(min: 1, max: 1),
        writeVersions: FleetProtocolRange = FleetProtocolRange(min: 1, max: 1)
    ) async throws -> FleetNegotiateResult {
        guard authenticated else {
            throw FleetConnectionError.notAuthenticated
        }
        let result = try await request(
            "fleet/negotiate",
            params: FleetNegotiateParams(
                clientName: clientName,
                clientVersion: clientVersion,
                readVersions: readVersions,
                writeVersions: writeVersions
            ),
            result: FleetNegotiateResult.self
        )
        guard result.readCompatible else {
            throw FleetConnectionError.protocolReadIncompatible(result)
        }
        negotiation = result
        return result
    }

    func snapshot() async throws -> FleetSnapshot {
        try requireReadCompatibility()
        return try await request("fleet/snapshot", params: FleetSnapshotParams(), result: FleetSnapshot.self)
    }

    func subscribe(afterRevision: Int64) async throws -> FleetSubscribeResult {
        try requireReadCompatibility()
        return try await request(
            "fleet/subscribe",
            params: FleetSubscribeParams(afterRevision: afterRevision),
            result: FleetSubscribeResult.self
        )
    }

    func action(_ params: FleetActionParams) async throws -> FleetActionResult {
        try requireWriteCapability("fleet.action.execute")
        return try await request("fleet/action", params: params, result: FleetActionResult.self)
    }

    func start(_ params: FleetStartParams) async throws -> FleetStartResult {
        try requireWriteCapability("fleet.start.execute")
        return try await request("fleet/start", params: params, result: FleetStartResult.self)
    }

    func broadcast(_ params: FleetBroadcastParams) async throws -> FleetBroadcastResult {
        try requireWriteCapability("fleet.broadcast.execute")
        return try await request("fleet/broadcast", params: params, result: FleetBroadcastResult.self)
    }

    func receiptList(_ params: FleetReceiptListParams) async throws -> FleetReceiptListResult {
        try requireReadCapability("fleet.receipt.read")
        return try await request("fleet/receipt_list", params: params, result: FleetReceiptListResult.self)
    }

    func receiptGet(_ params: FleetReceiptGetParams) async throws -> FleetReceiptGetResult {
        try requireReadCapability("fleet.receipt.read")
        return try await request("fleet/receipt_get", params: params, result: FleetReceiptGetResult.self)
    }

    func incoming() -> AsyncStream<FleetIncoming> {
        AsyncStream { continuation in
            let id = UUID()
            notificationStreams[id] = continuation
            continuation.onTermination = { [weak self] _ in
                Task { await self?.removeNotificationStream(id) }
            }
        }
    }

    func requireWriteCompatibility() throws {
        guard let negotiation else {
            throw FleetConnectionError.notAuthenticated
        }
        try Self.validateWriteCompatibility(negotiation)
    }

    func close() {
        let currentDescriptor = descriptor
        descriptor = nil
        authenticated = false
        negotiation = nil
        readerTask?.cancel()
        readerTask = nil
        if let currentDescriptor {
            Darwin.shutdown(currentDescriptor, SHUT_RDWR)
            Darwin.close(currentDescriptor)
        }
        resumePending(with: FleetConnectionError.closed)
        notificationStreams.values.forEach { $0.finish() }
        notificationStreams.removeAll()
    }

    /// Test-only malformed-wire proof still uses the owned, real Unix socket.
    func sendRawFrameForTesting(_ frame: Data) throws {
        guard let descriptor else {
            throw FleetConnectionError.notConnected
        }
        try Self.writeAll(frame, to: descriptor)
    }

    private func request<Params: Encodable, Result: Decodable>(
        _ method: String,
        params: Params,
        result: Result.Type
    ) async throws -> Result {
        guard let descriptor else {
            throw FleetConnectionError.notConnected
        }
        let id = RPCID.number(nextRequestID)
        nextRequestID += 1
        let body = try FleetWire.encoder().encode(RPCRequest(id: id, method: method, params: params))
        let frame = try ContentLengthEncoder.encode(body)
        let responseData: Data = try await withTaskCancellationHandler(operation: {
            try await withCheckedThrowingContinuation { continuation in
                beginRequest(id: id, frame: frame, descriptor: descriptor, continuation: continuation)
            }
        }, onCancel: {
            Task { await self.cancelRequest(id) }
        })
        let response = try FleetWire.decoder().decode(RPCResponse<Result>.self, from: responseData)
        if let error = response.error {
            throw FleetConnectionError.rpc(error)
        }
        guard let value = response.result else {
            throw FleetConnectionError.malformedEnvelope
        }
        return value
    }

    private func beginRequest(
        id: RPCID,
        frame: Data,
        descriptor: Int32,
        continuation: CheckedContinuation<Data, Error>
    ) {
        guard self.descriptor == descriptor else {
            continuation.resume(throwing: FleetConnectionError.closed)
            return
        }
        pending[id] = continuation
        do {
            try Self.writeAll(frame, to: descriptor)
        } catch {
            pending.removeValue(forKey: id)?.resume(throwing: error)
        }
    }

    private func cancelRequest(_ id: RPCID) {
        guard let continuation = pending.removeValue(forKey: id) else { return }
        continuation.resume(throwing: CancellationError())
        close()
    }

    private nonisolated static func readFrames(
        from descriptor: Int32,
        receive: @escaping @Sendable (Data) async -> Void
    ) async -> FleetConnectionError {
        var decoder = ContentLengthDecoder()
        var buffer = [UInt8](repeating: 0, count: 16 * 1024)

        while !Task.isCancelled {
            let readCount = Darwin.read(descriptor, &buffer, buffer.count)
            if readCount > 0 {
                do {
                    let frames = try decoder.append(Data(buffer.prefix(Int(readCount))))
                    for frame in frames {
                        await receive(frame)
                    }
                } catch {
                    return .malformedEnvelope
                }
                continue
            }
            if readCount == 0 || errno != EINTR {
                return .disconnected
            }
        }
        return .closed
    }

    private func receive(_ frame: Data) {
        do {
            let envelope = try FleetWire.decoder().decode(Envelope.self, from: frame)
            if let id = envelope.id {
                guard let continuation = pending.removeValue(forKey: id) else {
                    return
                }
                continuation.resume(returning: frame)
                return
            }
            guard let method = envelope.method else {
                throw FleetConnectionError.malformedEnvelope
            }
            switch method {
            case "fleet/event":
                let params = try FleetWire.decoder().decode(FleetEvent.self, from: envelope.paramsData)
                yield(.event(params))
            case "fleet/resync_required":
                let params = try FleetWire.decoder().decode(FleetResyncRequired.self, from: envelope.paramsData)
                yield(.resyncRequired(params))
            default:
                yield(.unknownNotification(method))
            }
        } catch {
            disconnect(FleetConnectionError.malformedEnvelope, descriptor: descriptor)
        }
    }

    private func disconnect(_ error: Error, descriptor: Int32?) {
        guard descriptor == nil || self.descriptor == descriptor else { return }
        let currentDescriptor = self.descriptor
        self.descriptor = nil
        authenticated = false
        negotiation = nil
        readerTask?.cancel()
        readerTask = nil
        if let currentDescriptor {
            Darwin.shutdown(currentDescriptor, SHUT_RDWR)
            Darwin.close(currentDescriptor)
        }
        resumePending(with: error)
        notificationStreams.values.forEach { $0.finish() }
        notificationStreams.removeAll()
    }

    private func requireReadCompatibility() throws {
        guard let negotiation else {
            throw FleetConnectionError.notAuthenticated
        }
        guard negotiation.readCompatible else {
            throw FleetConnectionError.protocolReadIncompatible(negotiation)
        }
    }

    private func removeNotificationStream(_ id: UUID) {
        notificationStreams.removeValue(forKey: id)
    }

    private func yield(_ incoming: FleetIncoming) {
        notificationStreams.values.forEach { $0.yield(incoming) }
    }

    private func resumePending(with error: Error) {
        let continuations = pending.values
        pending.removeAll()
        continuations.forEach { $0.resume(throwing: error) }
    }

    private static func connectUnixSocket(at url: URL) throws -> Int32 {
        let descriptor = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard descriptor >= 0 else {
            throw FleetConnectionError.socket(errno)
        }
        do {
            var address = sockaddr_un()
            address.sun_family = sa_family_t(AF_UNIX)
            let path = Array(url.path.utf8)
            let capacity = MemoryLayout.size(ofValue: address.sun_path)
            guard path.count + 1 <= capacity else {
                throw FleetConnectionError.socket(ENAMETOOLONG)
            }
            withUnsafeMutableBytes(of: &address.sun_path) { destination in
                destination.initializeMemory(as: UInt8.self, repeating: 0)
                destination.copyBytes(from: path)
            }
            let length = socklen_t(MemoryLayout<sa_family_t>.size + path.count + 1)
            let result = withUnsafePointer(to: &address) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    Darwin.connect(descriptor, $0, length)
                }
            }
            guard result == 0 else {
                throw FleetConnectionError.socket(errno)
            }
            return descriptor
        } catch {
            Darwin.close(descriptor)
            throw error
        }
    }

    static func validateWriteCompatibility(_ result: FleetNegotiateResult) throws {
        guard result.writeCompatible else {
            throw FleetConnectionError.protocolWriteIncompatible
        }
    }

    static func validateCapability(_ capabilityID: String, in result: FleetNegotiateResult) throws {
        guard result.capabilityIDs.contains(capabilityID) else {
            throw FleetConnectionError.missingNegotiatedCapability(capabilityID)
        }
    }

    private func requireWriteCapability(_ capabilityID: String) throws {
        try requireWriteCompatibility()
        guard let negotiation else {
            throw FleetConnectionError.notAuthenticated
        }
        try Self.validateCapability(capabilityID, in: negotiation)
    }

    private func requireReadCapability(_ capabilityID: String) throws {
        try requireReadCompatibility()
        guard let negotiation else {
            throw FleetConnectionError.notAuthenticated
        }
        try Self.validateCapability(capabilityID, in: negotiation)
    }

    private static func writeAll(_ data: Data, to descriptor: Int32) throws {
        try data.withUnsafeBytes { bytes in
            guard let baseAddress = bytes.baseAddress else { return }
            var written = 0
            while written < bytes.count {
                let result = Darwin.write(descriptor, baseAddress.advanced(by: written), bytes.count - written)
                if result > 0 {
                    written += result
                } else if result < 0, errno == EINTR {
                    continue
                } else {
                    throw FleetConnectionError.socket(errno)
                }
            }
        }
    }
}

private struct Envelope: Decodable {
    let jsonrpc: String
    let id: RPCID?
    let method: String?
    let paramsData: Data

    private enum CodingKeys: String, CodingKey {
        case jsonrpc
        case id
        case method
        case params
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        jsonrpc = try container.decode(String.self, forKey: .jsonrpc)
        id = try container.decodeIfPresent(RPCID.self, forKey: .id)
        method = try container.decodeIfPresent(String.self, forKey: .method)
        guard jsonrpc == "2.0", (id != nil) != (method != nil) else {
            throw FleetConnectionError.malformedEnvelope
        }
        let params = try container.decodeIfPresent(JSONValue.self, forKey: .params) ?? .null
        paramsData = try FleetWire.encoder().encode(params)
    }
}
