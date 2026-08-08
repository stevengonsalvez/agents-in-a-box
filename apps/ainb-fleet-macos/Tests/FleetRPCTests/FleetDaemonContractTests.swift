import Foundation
import XCTest
@testable import AINBFleet

final class FleetDaemonContractTests: XCTestCase {
    func testRealDaemonRejectsUnauthenticatedClient() async throws {
        let fixture = try FixtureDaemon()
        defer { fixture.stop() }
        let connection = try await fixture.connection()
        defer { Task { await connection.close() } }

        do {
            try await connection.authenticate(token: "mdt_invalid")
            XCTFail("expected daemon token rejection")
        } catch let FleetConnectionError.rpc(error) {
            XCTAssertEqual(error.code, -32_000)
        }
    }

    func testRealDaemonAuthenticatesTokenFile() async throws {
        let fixture = try FixtureDaemon()
        defer { fixture.stop() }
        let connection = try await fixture.authenticatedAndNegotiatedConnection()
        defer { Task { await connection.close() } }

        let snapshot = try await connection.snapshot()
        XCTAssertEqual(snapshot.headRevision, 0)
    }

    func testRealDaemonNegotiatesProtocol() async throws {
        let fixture = try FixtureDaemon()
        defer { fixture.stop() }
        let connection = try await fixture.authenticatedConnection()
        defer { Task { await connection.close() } }

        let result = try await connection.negotiate()
        XCTAssertEqual(result.protocolVersion, 2)
        XCTAssertTrue(result.readCompatible)
        XCTAssertTrue(result.writeCompatible)
        XCTAssertTrue(result.capabilityIDs.contains("fleet.subscription.live"))
    }

    /// I9 bump-and-refuse: a client whose declared range excludes v2 is refused
    /// by the real daemon on BOTH legs, and the connection surfaces the read
    /// refusal as a typed error.
    func testRealDaemonRefusesV1OnlyClientOnBothLegs() async throws {
        let fixture = try FixtureDaemon()
        defer { fixture.stop() }
        let connection = try await fixture.authenticatedConnection()
        defer { Task { await connection.close() } }

        do {
            _ = try await connection.negotiate(
                readVersions: FleetProtocolRange(min: 1, max: 1),
                writeVersions: FleetProtocolRange(min: 1, max: 1)
            )
            XCTFail("expected read-incompatible refusal")
        } catch let FleetConnectionError.protocolReadIncompatible(result) {
            XCTAssertEqual(result.protocolVersion, 2)
            XCTAssertFalse(result.readCompatible)
            XCTAssertFalse(result.writeCompatible)
        }
    }

    /// The provider after `acp` must be capability-only: a token this build has
    /// never heard of decodes to `.unknown` instead of failing the snapshot.
    func testProviderDecodeIsTolerantAndKnowsAcp() throws {
        func decode(_ raw: String) throws -> FleetProvider {
            try JSONDecoder().decode(FleetProvider.self, from: Data("\"\(raw)\"".utf8))
        }
        XCTAssertEqual(try decode("acp"), .acp)
        XCTAssertEqual(try decode("some-future-provider"), .unknown)
    }

    /// The write leg is the trap: reading v2 while writing v1 connects fine and
    /// then fails every action at `requireWriteCapability`. Assert the ranges
    /// the SHIPPED store declares, not the `FleetConnection` defaults alone.
    @MainActor
    func testShippedStoreDeclaresV2OnReadAndWriteLegs() {
        let store = FleetStore()
        XCTAssertEqual(store.readVersions, FleetProtocolRange(min: 1, max: 2))
        XCTAssertEqual(store.writeVersions, FleetProtocolRange(min: 1, max: 2))
    }

    func testRealDaemonReturnsTypedSnapshot() async throws {
        let fixture = try FixtureDaemon()
        defer { fixture.stop() }
        _ = try fixture.seed("snapshot")
        let connection = try await fixture.authenticatedAndNegotiatedConnection()
        defer { Task { await connection.close() } }

        let snapshot = try await connection.snapshot()
        XCTAssertEqual(snapshot.headRevision, 1)
        XCTAssertEqual(snapshot.sessions.map(\.sessionKey), ["claude:fixture-session"])
    }

    func testRealDaemonReplaysEventsAfterCursor() async throws {
        let fixture = try FixtureDaemon()
        defer { fixture.stop() }
        let first = try fixture.seed("replay-1")
        _ = try fixture.seed("replay-2", eventType: "Stop")
        let connection = try await fixture.authenticatedAndNegotiatedConnection()
        defer { Task { await connection.close() } }

        let subscription = try await connection.subscribe(afterRevision: first)
        XCTAssertEqual(subscription.replayState, .complete)
        XCTAssertEqual(subscription.replay.map(\.eventID), ["replay-2"])
    }

    func testRealDaemonStreamsLiveEventsOnOpenSocket() async throws {
        let fixture = try FixtureDaemon()
        defer { fixture.stop() }
        let first = try fixture.seed("live-1")
        let connection = try await fixture.authenticatedAndNegotiatedConnection()
        defer { Task { await connection.close() } }
        let stream = await connection.incoming()
        let subscription = try await connection.subscribe(afterRevision: first)
        XCTAssertEqual(subscription.replayState, .complete)

        async let incoming = Self.nextFleetEvent(from: stream)
        _ = try fixture.seed("live-2", eventType: "Stop")
        let event = try await incoming
        XCTAssertEqual(event.eventID, "live-2")
        XCTAssertGreaterThan(event.revision, first)
    }

    func testRealDaemonGapForcesSnapshotReset() async throws {
        let fixture = try FixtureDaemon()
        defer { fixture.stop() }
        _ = try fixture.seed("reset")
        let connection = try await fixture.authenticatedAndNegotiatedConnection()
        defer { Task { await connection.close() } }

        let subscription = try await connection.subscribe(afterRevision: 99)
        XCTAssertEqual(subscription.replay, [])
        XCTAssertEqual(subscription.replayState, .snapshotReset(reason: .cursorAhead))
    }

    func testRealDaemonMalformedFrameClosesConnection() async throws {
        let fixture = try FixtureDaemon()
        defer { fixture.stop() }
        let connection = try await fixture.connection()
        defer { Task { await connection.close() } }

        try await connection.sendRawFrameForTesting(Data("X-Unsupported: 1\r\n\r\n".utf8))
        try await Task.sleep(for: .milliseconds(100))
        do {
            try await connection.authenticate(token: try fixture.location.readToken())
            XCTFail("expected malformed frame to close real daemon connection")
        } catch let error as FleetConnectionError {
            XCTAssertTrue(error == .notConnected || error == .closed || error == .disconnected)
        }
    }

    func testRealDaemonCancellationStopsSubscription() async throws {
        let fixture = try FixtureDaemon()
        defer { fixture.stop() }
        _ = try fixture.seed("cancel")
        let connection = try await fixture.authenticatedAndNegotiatedConnection()
        let stream = await connection.incoming()
        _ = try await connection.subscribe(afterRevision: 1)

        let waiter = Task { () -> FleetIncoming? in
            var iterator = stream.makeAsyncIterator()
            return await iterator.next()
        }
        waiter.cancel()
        _ = await waiter.value
        await connection.close()

        let replacement = try await fixture.authenticatedAndNegotiatedConnection()
        defer { Task { await replacement.close() } }
        let snapshot = try await replacement.snapshot()
        XCTAssertEqual(snapshot.headRevision, 1)
    }

    // MARK: - Shared chat fixtures (buzz-port part 2)
    //
    // The SAME files the Rust `chat_fixtures` test round-trips
    // (ainb-tui/crates/ainb-hangar-proto/fixtures/chat). One fixture set, two
    // suites: a field that drifts on one side goes red on the other instead of
    // being found by an operator. Read from the repo, never copied here — a
    // copy is how two suites agree with each other and disagree with the wire.

    /// Decode one shared fixture and assert it re-encodes to the same JSON.
    private func assertFixtureRoundTrips<T: Codable>(_ name: String, as type: T.Type, file: StaticString = #filePath, line: UInt = #line) throws -> T {
        let original = try Self.chatFixture(named: name)
        let decoded = try FleetWire.decoder().decode(T.self, from: original)
        let encoded = try FleetWire.encoder().encode(decoded)
        XCTAssertEqual(
            try JSONSerialization.data(withJSONObject: JSONSerialization.jsonObject(with: original), options: [.sortedKeys]),
            try JSONSerialization.data(withJSONObject: JSONSerialization.jsonObject(with: encoded), options: [.sortedKeys]),
            "\(name) does not round-trip",
            file: file,
            line: line
        )
        return decoded
    }

    private static func chatFixturesDirectory() -> URL {
        var repository = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { repository.deleteLastPathComponent() }
        return repository.appendingPathComponent("ainb-tui/crates/ainb-hangar-proto/fixtures/chat")
    }

    private static func chatFixture(named name: String) throws -> Data {
        try Data(contentsOf: chatFixturesDirectory().appendingPathComponent(name))
    }

    /// Every fixture the tests below decode. Rust's
    /// `every_fixture_is_claimed_by_a_type` reads the same directory against its
    /// own bound-type list, so it only ever catches drift on the RUST side; this
    /// is the mirror that makes a fixture added (or deleted) without a Swift
    /// decoder go red here too.
    private static let decodedChatFixtures: Set<String> = [
        "channel_create_params.json",
        "channel_create_result.json",
        "channel_list_result.json",
        "copilot_configure_params.json",
        "copilot_configure_result.json",
        "confirm_list_result.json",
        "confirm_answer_approve_params.json",
        "confirm_answer_edit_params.json",
        "confirm_answer_result.json",
        "confirm_event.json",
        "activity_list_params.json",
        "activity_list_result.json",
        "activity_event.json",
    ]

    func testEveryChatFixtureIsDecodedBySwift() throws {
        let present = try FileManager.default
            .contentsOfDirectory(at: Self.chatFixturesDirectory(), includingPropertiesForKeys: nil)
            .map(\.lastPathComponent)
            .filter { $0.hasSuffix(".json") }

        XCTAssertEqual(
            Set(present),
            Self.decodedChatFixtures,
            "fixtures/chat and the Swift-decoded set disagree"
        )
        XCTAssertEqual(present.count, Self.decodedChatFixtures.count, "duplicate fixture name")

        // And the set is not aspirational: each name really loads.
        for name in Self.decodedChatFixtures {
            XCTAssertNoThrow(try Self.chatFixture(named: name), "\(name) is not readable")
        }
    }

    func testChatChannelFixturesDecodeAndMintAChannelScope() throws {
        let params = try assertFixtureRoundTrips("channel_create_params.json", as: FleetChannelCreateParams.self)
        XCTAssertEqual(params.kind, .broadcast)
        XCTAssertEqual(params.recipients?.count, 3)

        let created = try assertFixtureRoundTrips("channel_create_result.json", as: FleetChannelCreateResult.self)
        XCTAssertEqual(created.channel.scopeKey, "channel:\(created.channel.id)")

        let listed = try assertFixtureRoundTrips("channel_list_result.json", as: FleetChannelListResult.self)
        XCTAssertEqual(listed.channels.first?.kind, .copilot)
        XCTAssertEqual(listed.channels.first?.recipients, [])
        XCTAssertTrue(listed.channels.allSatisfy { $0.scopeKey.hasPrefix("channel:") })
    }

    func testCopilotConfigureFixturesCarryNoPermissionMode() throws {
        let params = try assertFixtureRoundTrips("copilot_configure_params.json", as: FleetCopilotConfigureParams.self)
        XCTAssertEqual(params.provider, .claude)
        XCTAssertEqual(params.reasoningEffort, "medium")

        // The absence is the contract: a settable permission mode would be a
        // remote off-switch for the guardrails.
        let raw = try JSONSerialization.jsonObject(with: try Self.chatFixture(named: "copilot_configure_params.json")) as? [String: Any]
        for forbidden in ["permission_mode", "permissionMode", "mode"] {
            XCTAssertNil(raw?[forbidden], "\(forbidden) must never appear on copilot_configure")
        }

        let result = try assertFixtureRoundTrips("copilot_configure_result.json", as: FleetCopilotConfigureResult.self)
        XCTAssertTrue(result.personaSet)
    }

    func testConfirmFixturesDecodeThroughTheFullLifecycle() throws {
        let list = try assertFixtureRoundTrips("confirm_list_result.json", as: FleetConfirmListResult.self)
        XCTAssertEqual(list.confirms.count, 2)
        XCTAssertTrue(list.confirms[0].isAnswerable)
        XCTAssertGreaterThan(list.confirms[0].expiresAt, list.confirms[0].createdAt)
        XCTAssertNil(list.confirms[1].targetSessionKey)

        let approve = try assertFixtureRoundTrips("confirm_answer_approve_params.json", as: FleetConfirmAnswerParams.self)
        XCTAssertEqual(approve.answer, .approve)

        let edit = try assertFixtureRoundTrips("confirm_answer_edit_params.json", as: FleetConfirmAnswerParams.self)
        guard case let .edit(arguments) = edit.answer else {
            return XCTFail("expected an edit answer")
        }
        XCTAssertEqual(arguments.value("provider")?.stringValue, "codex")

        let answered = try assertFixtureRoundTrips("confirm_answer_result.json", as: FleetConfirmAnswerResult.self)
        XCTAssertEqual(answered.state, .approved)

        let event = try assertFixtureRoundTrips("confirm_event.json", as: FleetConfirmEventParams.self)
        XCTAssertEqual(event.confirm.state, .expired)
        XCTAssertFalse(event.confirm.isAnswerable)
    }

    func testActivityFixturesPageByCommitSeq() throws {
        let params = try assertFixtureRoundTrips("activity_list_params.json", as: FleetActivityListParams.self)
        XCTAssertEqual(params.afterSeq, 41)

        let result = try assertFixtureRoundTrips("activity_list_result.json", as: FleetActivityListResult.self)
        XCTAssertEqual(result.activities.map(\.activityClass), [.write, .destructive])
        XCTAssertEqual(result.activities.last?.outcome, .expired)
        XCTAssertEqual(result.nextAfterSeq, result.activities.last?.seq)
        XCTAssertEqual(result.activities.map(\.seq), result.activities.map(\.seq).sorted())

        let event = try assertFixtureRoundTrips("activity_event.json", as: FleetActivityEventParams.self)
        XCTAssertEqual(event.activity.activityClass, .read)
        XCTAssertNil(event.activity.detail)
    }

    /// The part-2 mirror of `testProviderDecodeIsTolerantAndKnowsAcp`: an event
    /// kind this build has never heard of degrades that ONE value instead of
    /// failing the page, and it never degrades into an actionable state.
    func testPartTwoEventKindDecodeIsTolerantAndFailsSafe() throws {
        func decode<T: Decodable>(_ raw: String, as type: T.Type) throws -> T {
            try FleetWire.decoder().decode(T.self, from: Data("\"\(raw)\"".utf8))
        }
        XCTAssertEqual(try decode("copilot", as: FleetChannelKind.self), .copilot)
        XCTAssertEqual(try decode("some-future-kind", as: FleetChannelKind.self), .unknown)
        XCTAssertEqual(try decode("some-future-provider", as: FleetCopilotProvider.self), .unknown)
        XCTAssertEqual(try decode("some-future-state", as: FleetConfirmState.self), .unknown)
        XCTAssertEqual(try decode("some-future-class", as: FleetActivityClass.self), .unknown)
        XCTAssertEqual(try decode("some-future-outcome", as: FleetActivityOutcome.self), .unknown)

        // A whole frame carrying unknown tokens still decodes, and the card it
        // describes is NOT offered as answerable.
        let frame = Data(#"{"confirm":{"confirm_id":"01J0FUTURE","scope_key":"channel:copilot","tool":"future_tool","arguments":{},"state":"quarantined","created_at":1,"expires_at":2}}"#.utf8)
        let event = try FleetWire.decoder().decode(FleetConfirmEventParams.self, from: frame)
        XCTAssertEqual(event.confirm.state, .unknown)
        XCTAssertFalse(event.confirm.isAnswerable)

        let row = Data(#"{"activity":{"seq":1,"id":"01J0FUTURE","scope_key":"channel:copilot","tool":"future_tool","class":"quantum","outcome":"pending","created_at":1}}"#.utf8)
        let activity = try FleetWire.decoder().decode(FleetActivityEventParams.self, from: row)
        XCTAssertEqual(activity.activity.activityClass, .unknown)
        XCTAssertEqual(activity.activity.outcome, .unknown)
    }

    /// A part-2 capability is advertised exactly when its dispatch arm exists.
    /// This is the client-side half of the Rust advertisement test, and it is
    /// the assertion a UI depends on: gating a chat surface on the catalogue is
    /// only safe if the catalogue never names a method that answers -32601.
    ///
    /// It asserted the NEGATIVE while the arms were unlanded. Flipping it is
    /// the point rather than a chore: the day the arms land, this test is what
    /// says the client may now offer the surface.
    func testRealDaemonAdvertisesPartTwoCapabilitiesWithTheirArms() async throws {
        let fixture = try FixtureDaemon()
        defer { fixture.stop() }
        let connection = try await fixture.authenticatedConnection()
        defer { Task { await connection.close() } }

        let result = try await connection.negotiate()
        for capability in ["fleet.chat.write", "fleet.chat.read", "fleet.copilot.configure", "fleet.confirm.answer"] {
            XCTAssertTrue(
                result.capabilityIDs.contains(capability),
                "\(capability) has a dispatch arm but is not advertised, so a UI gating on the catalogue stays dark"
            )
        }
    }

    private static func nextFleetEvent(from stream: AsyncStream<FleetIncoming>) async throws -> FleetEvent {
        var iterator = stream.makeAsyncIterator()
        while let incoming = await iterator.next() {
            if case let .event(event) = incoming {
                return event
            }
        }
        throw FleetConnectionError.closed
    }
}

private final class FixtureDaemon {
    let home: URL
    let location: HangarLocation
    private let process: Process
    private let input = Pipe()
    private let output = Pipe()
    private let error = Pipe()
    private let responseState = FixtureResponseState()
    private let errorState = FixtureResponseState()
    private var pipesTornDown = false

    init() throws {
        let binary = try Self.fixtureBinary()
        home = URL(fileURLWithPath: "/tmp", isDirectory: true)
            .appendingPathComponent("ainb-fleet-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        location = HangarLocation(environment: ["AINB_HANGAR_HOME": home.path])
        process = Process()
        process.executableURL = URL(fileURLWithPath: binary)
        process.standardInput = input
        process.standardOutput = output
        process.standardError = error
        process.terminationHandler = { [responseState] terminatedProcess in
            responseState.recordExit(status: terminatedProcess.terminationStatus)
        }
        output.fileHandleForReading.readabilityHandler = { [responseState] handle in
            let chunk = handle.availableData
            if chunk.isEmpty {
                handle.readabilityHandler = nil
            } else {
                responseState.appendOutput(chunk)
            }
        }
        error.fileHandleForReading.readabilityHandler = { [errorState] handle in
            let chunk = handle.availableData
            if chunk.isEmpty {
                handle.readabilityHandler = nil
            } else {
                errorState.appendOutput(chunk)
            }
        }
        var environment = ProcessInfo.processInfo.environment
        environment["AINB_HANGAR_HOME"] = home.path
        process.environment = environment
        try process.run()
        try waitForSocket()
    }

    private static func fixtureBinary() throws -> String {
        if let configured = ProcessInfo.processInfo.environment["AINB_FLEET_FIXTURE_DAEMON"], !configured.isEmpty {
            return configured
        }
        var repository = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { repository.deleteLastPathComponent() }
        let built = repository.appendingPathComponent("ainb-tui/target/debug/examples/fleet_fixture_daemon")
        guard FileManager.default.isExecutableFile(atPath: built.path) else {
            throw XCTSkip("build fleet_fixture_daemon before real daemon contract tests")
        }
        return built.path
    }

    deinit {
        stop()
    }

    func stop() {
        guard process.isRunning else {
            tearDownPipes()
            removeHome()
            return
        }
        _ = try? send(["command": "shutdown"])
        if process.isRunning {
            process.terminate()
        }
        process.waitUntilExit()
        tearDownPipes()
        removeHome()
    }

    func connection() async throws -> FleetConnection {
        let connection = FleetConnection(location: location)
        try await connection.connect()
        return connection
    }

    func authenticatedConnection() async throws -> FleetConnection {
        let connection = try await connection()
        try await connection.authenticate(token: try location.readToken())
        return connection
    }

    func authenticatedAndNegotiatedConnection() async throws -> FleetConnection {
        let connection = try await authenticatedConnection()
        _ = try await connection.negotiate()
        return connection
    }

    func seed(_ eventID: String, eventType: String = "SessionStart") throws -> Int64 {
        let response = try send([
            "command": "seed",
            "event_id": eventID,
            "event_type": eventType,
        ])
        guard response["ok"] as? Bool == true, let revision = response["revision"] as? NSNumber else {
            throw FixtureError.invalidResponse
        }
        return revision.int64Value
    }

    private func send(_ command: [String: Any]) throws -> [String: Any] {
        let data = try JSONSerialization.data(withJSONObject: command)
        input.fileHandleForWriting.write(data)
        input.fileHandleForWriting.write(Data("\n".utf8))
        return try readResponse()
    }

    private func readResponse() throws -> [String: Any] {
        let timeout = DispatchTime.now() + .seconds(5)
        while true {
            if let line = responseState.takeOutputLine() {
                guard let object = try JSONSerialization.jsonObject(with: line) as? [String: Any] else {
                    throw FixtureError.invalidJSON
                }
                return object
            }
            if let exitStatus = responseState.recordedExitStatus() {
                throw FixtureError.exited(exitStatus, errorState.outputText())
            }
            guard responseState.waitForResponse(timeout: timeout) else {
                if let exitStatus = responseState.recordedExitStatus() {
                    throw FixtureError.exited(exitStatus, errorState.outputText())
                }
                throw FixtureError.responseTimeout
            }
        }
    }

    private func waitForSocket() throws {
        let deadline = Date().addingTimeInterval(5)
        while !FileManager.default.fileExists(atPath: location.socketURL.path) {
            guard process.isRunning else {
                throw FixtureError.exited(responseState.recordedExitStatus() ?? -1, errorState.outputText())
            }
            guard Date() < deadline else { throw FixtureError.socketTimeout }
            Thread.sleep(forTimeInterval: 0.02)
        }
    }

    private func tearDownPipes() {
        guard !pipesTornDown else { return }
        pipesTornDown = true
        output.fileHandleForReading.readabilityHandler = nil
        error.fileHandleForReading.readabilityHandler = nil
        input.fileHandleForWriting.closeFile()
        output.fileHandleForReading.closeFile()
        error.fileHandleForReading.closeFile()
    }

    private func removeHome() {
        guard FileManager.default.fileExists(atPath: home.path) else { return }
        do {
            try FileManager.default.removeItem(at: home)
        } catch let error as CocoaError where error.code == .fileNoSuchFile {
        } catch {
            XCTFail("failed to remove fixture home: \(error)")
        }
    }
}

private final class FixtureResponseState: @unchecked Sendable {
    private let lock = NSLock()
    private let signal = DispatchSemaphore(value: 0)
    private var outputBuffer = Data()
    private var exitStatus: Int32?

    func appendOutput(_ chunk: Data) {
        lock.lock()
        outputBuffer.append(chunk)
        lock.unlock()
        signal.signal()
    }

    func takeOutputLine() -> Data? {
        lock.lock()
        defer { lock.unlock() }
        guard let newline = outputBuffer.firstRange(of: Data("\n".utf8)) else { return nil }
        let line = outputBuffer.subdata(in: 0..<newline.lowerBound)
        outputBuffer.removeSubrange(0..<newline.upperBound)
        return line
    }

    func recordExit(status: Int32) {
        lock.lock()
        exitStatus = status
        lock.unlock()
        signal.signal()
    }

    func recordedExitStatus() -> Int32? {
        lock.lock()
        defer { lock.unlock() }
        return exitStatus
    }

    func outputText() -> String {
        lock.lock()
        defer { lock.unlock() }
        return String(decoding: outputBuffer, as: UTF8.self)
    }

    func waitForResponse(timeout: DispatchTime) -> Bool {
        signal.wait(timeout: timeout) != .timedOut
    }
}

private enum FixtureError: Error {
    case invalidJSON
    case invalidResponse
    case exited(Int32, String)
    case socketTimeout
    case responseTimeout
}
