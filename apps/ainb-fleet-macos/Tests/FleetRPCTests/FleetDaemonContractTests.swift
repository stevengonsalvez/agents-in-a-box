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
        XCTAssertEqual(result.protocolVersion, 1)
        XCTAssertTrue(result.readCompatible)
        XCTAssertTrue(result.writeCompatible)
        XCTAssertTrue(result.capabilityIDs.contains("fleet.subscription.live"))
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
