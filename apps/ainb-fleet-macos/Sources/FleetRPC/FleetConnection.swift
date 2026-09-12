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

/// Everything the daemon pushes at us on a socket we already own.
///
/// The three chat cases arrive only after `fleet/message_subscribe` is
/// acknowledged on THIS connection, and the transcript case only after
/// `fleet/transcript_subscribe`. They all ride the same socket as
/// `fleet/event`: the daemon runs them as independent forwarders over one
/// writer, so a client needs one connection, not four.
enum FleetIncoming: Equatable, Sendable {
    case event(FleetEvent)
    case resyncRequired(FleetResyncRequired)
    case messageEvent(FleetMessageEventParams)
    /// The card RAW, so a row this build cannot fully read still reaches the
    /// pane as an unanswerable one instead of being dropped.
    case confirmEvent(FleetConfirmEventRawParams)
    case activityEvent(FleetActivityEventParams)
    /// One ACP transcript chunk, after `fleet/transcript_subscribe` is
    /// acknowledged on THIS connection. Unlike the three above it is addressed
    /// by SESSION, so the daemon runs exactly one transcript forwarder per
    /// socket and a second subscribe REPLACES the first.
    case transcriptEvent(FleetTranscriptEventParams)
    /// A notification this connection did not turn into one of the above:
    /// either a method this build has never heard of, or a chat frame whose
    /// params did not decode. Both are named and dropped, never fatal.
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
        do {
            try Self.preventSIGPIPE(on: connectedDescriptor)
        } catch {
            Darwin.close(connectedDescriptor)
            throw error
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
        let hello = try await request(
            "auth/hello",
            params: AuthHelloParams(token: token),
            result: AuthHelloResult.self
        )
        // A daemon whose range does not overlap ours answers an error frame,
        // which `request` already throws on, so reaching here means a version
        // both ends can speak. What is recorded is which one.
        negotiatedProtocol = hello.selectedOrLegacy
        daemonCapabilities = hello.capabilities
        authenticated = true
    }

    /// The Hangar protocol version this connection settled on (D17).
    ///
    /// `1` against a daemon that does not negotiate, which is the honest
    /// reading of a bare `{}` ack.
    private(set) var negotiatedProtocol: UInt32 = 1

    /// The daemon's capability catalogue, as declared in the hello reply.
    /// Empty against a daemon that predates the negotiation.
    private(set) var daemonCapabilities: [String] = []

    /// Whether the connected daemon advertised `capability`.
    func daemonAdvertises(_ capability: String) -> Bool {
        daemonCapabilities.contains(capability)
    }

    func negotiate(
        clientName: String = "ainb-fleet-macos",
        clientVersion: String = "0.1.0",
        readVersions: FleetProtocolRange = FleetProtocolRange(min: 1, max: 2),
        writeVersions: FleetProtocolRange = FleetProtocolRange(min: 1, max: 2)
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

    func usageSummary(_ params: FleetUsageSummaryParams) async throws -> FleetUsageSummaryResult {
        try requireReadCapability("fleet.usage.read")
        return try await request("fleet/usage_summary", params: params, result: FleetUsageSummaryResult.self)
    }

    func usageDashboard() async throws -> FleetUsageDashboardResult {
        try requireReadCapability("fleet.dashboard.read")
        return try await request("fleet/usage_dashboard", params: FleetUsageDashboardParams(), result: FleetUsageDashboardResult.self)
    }

    func quotaSummary() async throws -> FleetQuotaSummaryResult {
        try requireReadCapability("fleet.quota.read")
        return try await request("fleet/quota_summary", params: FleetQuotaSummaryParams(), result: FleetQuotaSummaryResult.self)
    }

    func runtimeStatus() async throws -> FleetRuntimeStatusResult {
        try requireReadCapability("fleet.runtime.read")
        return try await request("fleet/runtime_status", params: FleetRuntimeStatusParams(), result: FleetRuntimeStatusResult.self)
    }

    // MARK: - Fleet chat, Pal and guardrails (buzz-port part 2)
    //
    // Every capability id below is the one its DAEMON ARM checks, not a
    // plausible-looking neighbour: `fleet/message_*` is gated by
    // `fleet.message.*` while the channel, confirm and activity reads share
    // `fleet.chat.read`, and answering a card needs its own
    // `fleet.confirm.answer`. Gating on the wrong id is how a surface either
    // stays dark against a daemon that would have served it, or offers a
    // control that answers -32601 on the click.

    func channelList() async throws -> FleetChannelListResult {
        try requireReadCapability("fleet.chat.read")
        return try await request("fleet/channel_list", params: FleetChannelListParams(), result: FleetChannelListResult.self)
    }

    func channelCreate(_ params: FleetChannelCreateParams) async throws -> FleetChannelCreateResult {
        try requireWriteCapability("fleet.chat.write")
        return try await request("fleet/channel_create", params: params, result: FleetChannelCreateResult.self)
    }

    func acpSessionCreate(_ params: FleetAcpSessionCreateParams) async throws -> FleetAcpSessionCreateResult {
        try requireWriteCapability("fleet.acp.spawn")
        return try await request("fleet/acp_session_create", params: params, result: FleetAcpSessionCreateResult.self)
    }

    func messageList(_ params: FleetMessageListParams) async throws -> FleetMessageListResult {
        try requireReadCapability("fleet.message.read")
        return try await request("fleet/message_list", params: params, result: FleetMessageListResult.self)
    }

    /// Open the live chat stream on this connection.
    ///
    /// Gated by `fleet.message.read`, which is what
    /// `handle_fleet_message_subscribe` checks, NOT the `fleet.chat.read` its
    /// confirm and activity neighbours check: the ack is a read of the message
    /// log's head and the daemon gates it as one. Gating this on `chat.read`
    /// would open the stream against a daemon that refuses it, and leave it
    /// shut against one that would have served it.
    ///
    /// The daemon answers with the log head and THEN registers two forwarders
    /// on this socket: the message forwarder (`fleet/message_event`, replayed
    /// from `afterID` or from the head just acked) and the chat notification
    /// forwarder (`fleet/confirm_event`, `fleet/activity_event`). Both arrive
    /// on `incoming()` alongside `fleet/event`.
    func messageSubscribe(afterID: String? = nil) async throws -> FleetMessageSubscribeResult {
        try requireReadCapability("fleet.message.read")
        return try await request(
            "fleet/message_subscribe",
            params: FleetMessageSubscribeParams(afterID: afterID),
            result: FleetMessageSubscribeResult.self
        )
    }

    func messageSend(_ params: FleetMessageSendParams) async throws -> FleetMessageSendResult {
        try requireWriteCapability("fleet.message.send")
        return try await request("fleet/message_send", params: params, result: FleetMessageSendResult.self)
    }

    /// Confirm cards as raw rows, so one undecodable card cannot blank the pane.
    func confirmList(_ params: FleetConfirmListParams) async throws -> FleetConfirmListRawResult {
        try requireReadCapability("fleet.chat.read")
        return try await request("fleet/confirm_list", params: params, result: FleetConfirmListRawResult.self)
    }

    /// Every adapter this daemon's registry can spawn.
    ///
    /// Gated by `fleet.chat.read`, which is what `handle_fleet_adapter_list`
    /// checks. Its NEIGHBOUR on this surface, `palConfigure`, checks a
    /// different and stronger id, so the two are gated SEPARATELY: a daemon can
    /// serve the registry to a caller it will not let reconfigure Pal,
    /// and one combined gate would either hide the engine list or offer a
    /// picker whose every choice answers -32601.
    func adapterList() async throws -> FleetAdapterListResult {
        try requireReadCapability("fleet.chat.read")
        return try await request("fleet/adapter_list", params: FleetAdapterListParams(), result: FleetAdapterListResult.self)
    }

    /// Set the Pal session's adapter, guardrail dial and model.
    ///
    /// Gated by `fleet.copilot.configure`, its OWN id and not the
    /// `fleet.chat.write` its channel neighbour uses: the daemon holds this
    /// behind a stronger capability because the persona it can carry is a
    /// system prompt for an agent holding destructive tools.
    ///
    /// A result whose `sessionReplaced` is true means the caller's previous
    /// Pal session key is DEAD: a provider swap retires the old session and
    /// mints a new one on the same channel scope.
    func palConfigure(_ params: FleetPalConfigureParams) async throws -> FleetPalConfigureResult {
        try requireWriteCapability("fleet.copilot.configure")
        return try await request("fleet/copilot_configure", params: params, result: FleetPalConfigureResult.self)
    }

    func confirmAnswer(_ params: FleetConfirmAnswerParams) async throws -> FleetConfirmAnswerResult {
        try requireWriteCapability("fleet.confirm.answer")
        return try await request("fleet/confirm_answer", params: params, result: FleetConfirmAnswerResult.self)
    }

    func activityList(_ params: FleetActivityListParams) async throws -> FleetActivityListResult {
        try requireReadCapability("fleet.chat.read")
        return try await request("fleet/activity_list", params: params, result: FleetActivityListResult.self)
    }

    /// Page one ACP session's transcript by `ingest_order`.
    ///
    /// Gated by `fleet.transcript.read`, which is the id
    /// `handle_fleet_transcript_list` itself checks, NOT the `fleet.chat.read`
    /// its confirm and activity neighbours use: the transcript is a different
    /// log with a different capability, and the daemon can advertise one
    /// without the other. `fleet.transcript.prune` is a SEPARATE id naming the
    /// destructive verb, and nothing on this surface asks for it.
    func transcriptList(_ params: FleetTranscriptListParams) async throws -> FleetTranscriptListResult {
        try requireReadCapability("fleet.transcript.read")
        return try await request("fleet/transcript_list", params: params, result: FleetTranscriptListResult.self)
    }

    /// Open the live transcript stream for ONE session on this connection.
    ///
    /// Gated by the same `fleet.transcript.read`, because
    /// `handle_fleet_transcript_subscribe` checks the same id: the ack is a
    /// read of that session's transcript head and the daemon gates it as one.
    ///
    /// The daemon answers with the head order and THEN registers a per-session
    /// forwarder on this socket, aborting whichever one it was already running.
    /// So a second call for a different session MOVES the stream rather than
    /// adding one, and `fleet/transcript_event` arrives on `incoming()`
    /// alongside `fleet/event` and the chat frames.
    func transcriptSubscribe(_ params: FleetTranscriptSubscribeParams) async throws -> FleetTranscriptSubscribeResult {
        try requireReadCapability("fleet.transcript.read")
        return try await request(
            "fleet/transcript_subscribe",
            params: params,
            result: FleetTranscriptSubscribeResult.self
        )
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

    #if DEBUG
    /// One request with a caller-supplied params type, for the contract suite's
    /// NEGATIVE tests.
    ///
    /// The proofs it exists for are ones this client's own types cannot express
    /// by design: `FleetPalConfigureParams` deliberately has no
    /// permission-mode field, because a settable one would be a remote
    /// off-switch for the guardrails, and the test that the daemon's door is
    /// shut has to knock on it. Debug-only and unused by the app, so the shape
    /// the shipped client can build is still exactly the shape its types allow.
    func requestForTesting<Params: Encodable, Result: Decodable>(
        _ method: String,
        params: Params,
        result: Result.Type
    ) async throws -> Result {
        try await request(method, params: params, result: result)
    }
    #endif

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
            // The three chat frames degrade to a named drop rather than
            // throwing, and that difference from the two arms above is the
            // whole point. `fleet/event` is this client's OWN roster stream,
            // where a frame that does not decode means the two ends disagree
            // about the wire and continuing to render is worse than
            // reconnecting. The chat frames are fleet-wide BROADCASTS: they
            // carry every conversation on the daemon, so one row from a scope
            // this pane never renders would otherwise take the roster, the
            // menu bar and the operator's live session list down with it.
            // The safety-net page repairs whatever a dropped frame missed.
            case "fleet/message_event":
                yield(decoded(FleetMessageEventParams.self, from: envelope, as: FleetIncoming.messageEvent, method: method))
            case "fleet/confirm_event":
                yield(decoded(FleetConfirmEventRawParams.self, from: envelope, as: FleetIncoming.confirmEvent, method: method))
            case "fleet/activity_event":
                yield(decoded(FleetActivityEventParams.self, from: envelope, as: FleetIncoming.activityEvent, method: method))
            // The transcript frame takes the SAME degrade-and-name answer as
            // the three chat frames, for the same reason and one more: its
            // payload is arbitrary adapter-authored JSON, so it is the frame
            // most likely to carry a shape this build has never seen. Throwing
            // here would let an adapter's output close the operator's roster.
            case "fleet/transcript_event":
                yield(decoded(FleetTranscriptEventParams.self, from: envelope, as: FleetIncoming.transcriptEvent, method: method))
            // KEPT, and it is not the `default` this codebase bans: that rule is
            // about switches over a WIRE ENUM this build owns, where a new
            // variant must fail to compile. This switches over an open string
            // the daemon chooses, so an unknown method is a daemon that grew a
            // notification, and the only safe answer is to name it and carry
            // on. Throwing here would tear down a live connection over a frame
            // that concerns some other client's subscription.
            default:
                yield(.unknownNotification(method))
            }
        } catch {
            disconnect(FleetConnectionError.malformedEnvelope, descriptor: descriptor)
        }
    }

    /// One chat frame, decoded into its case, or NAMED as undecodable.
    ///
    /// Generic over the payload so the three arms cannot drift apart in how
    /// they handle a frame this build cannot read.
    private func decoded<Params: Decodable>(
        _ type: Params.Type,
        from envelope: Envelope,
        as make: (Params) -> FleetIncoming,
        method: String
    ) -> FleetIncoming {
        guard let params = try? FleetWire.decoder().decode(type, from: envelope.paramsData) else {
            return .unknownNotification(method)
        }
        return make(params)
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

    private static func preventSIGPIPE(on descriptor: Int32) throws {
        var enabled: Int32 = 1
        guard Darwin.setsockopt(
            descriptor,
            SOL_SOCKET,
            SO_NOSIGPIPE,
            &enabled,
            socklen_t(MemoryLayout.size(ofValue: enabled))
        ) == 0 else {
            throw FleetConnectionError.socket(errno)
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
