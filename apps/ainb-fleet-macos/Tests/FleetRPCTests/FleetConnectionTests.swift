import Darwin
import Foundation
import XCTest
@testable import AINBFleet

final class FleetConnectionTests: XCTestCase {
    func testLocationHonorsNonEmptyAINBHangarHome() {
        let location = HangarLocation(
            environment: ["AINB_HANGAR_HOME": "/tmp/ainb-hangar"],
            homeDirectory: URL(fileURLWithPath: "/unused")
        )

        XCTAssertEqual(location.socketURL.path, "/tmp/ainb-hangar/hangar.sock")
        XCTAssertEqual(location.tokenURL.path, "/tmp/ainb-hangar/hangar/daemon.token")
    }

    func testLocationFallsBackToAgentsInABoxHome() {
        let location = HangarLocation(
            environment: ["AINB_HANGAR_HOME": ""],
            homeDirectory: URL(fileURLWithPath: "/tmp/test-home")
        )

        XCTAssertEqual(location.home.path, "/tmp/test-home/.agents-in-a-box")
    }

    func testTokenWhitespaceIsTrimmedOnce() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        let tokenDirectory = directory.appendingPathComponent("hangar", isDirectory: true)
        try FileManager.default.createDirectory(at: tokenDirectory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        try Data("  mdt_test\n".utf8).write(to: tokenDirectory.appendingPathComponent("daemon.token"))

        XCTAssertEqual(try HangarLocation(environment: ["AINB_HANGAR_HOME": directory.path]).readToken(), "mdt_test")
    }

    func testAuthMustCompleteBeforeNegotiate() async {
        let connection = FleetConnection(location: HangarLocation(environment: ["AINB_HANGAR_HOME": "/unused"]))

        do {
            _ = try await connection.negotiate()
            XCTFail("expected authentication guard")
        } catch let error as FleetConnectionError {
            XCTAssertEqual(error, .notAuthenticated)
        } catch {
            XCTFail("unexpected error: \(error)")
        }
    }

    func testConnectPreventsSIGPIPEOnOwnedSocket() async throws {
        var descriptors = [Int32](repeating: 0, count: 2)
        XCTAssertEqual(Darwin.socketpair(AF_UNIX, SOCK_STREAM, 0, &descriptors), 0)
        defer { Darwin.close(descriptors[1]) }
        let connection = FleetConnection(
            location: HangarLocation(environment: ["AINB_HANGAR_HOME": "/unused"]),
            injectedDescriptor: descriptors[0]
        )

        try await connection.connect()
        var enabled: Int32 = 0
        var length = socklen_t(MemoryLayout.size(ofValue: enabled))
        XCTAssertEqual(Darwin.getsockopt(descriptors[0], SOL_SOCKET, SO_NOSIGPIPE, &enabled, &length), 0)
        XCTAssertEqual(enabled, 1)
        await connection.close()
    }

    func testWriteMismatchIsBlocked() {
        XCTAssertThrowsError(try FleetConnection.validateWriteCompatibility(
            FleetNegotiateResult(
                daemonVersion: "test",
                protocolVersion: 1,
                readCompatible: true,
                writeCompatible: false,
                capabilityIDs: []
            )
        )) { XCTAssertEqual($0 as? FleetConnectionError, .protocolWriteIncompatible) }
    }

    func testReadMismatchCarriesDaemonAndProtocolVersions() {
        let result = FleetNegotiateResult(
            daemonVersion: "fixture-daemon",
            protocolVersion: 7,
            readCompatible: false,
            writeCompatible: false,
            capabilityIDs: []
        )
        XCTAssertEqual(
            FleetConnectionError.protocolReadIncompatible(result),
            .protocolReadIncompatible(result)
        )
    }

    func testOldCatalogueRefusesReceiptReadsAndStart() {
        let oldCatalogue = FleetNegotiateResult(
            daemonVersion: "fixture-daemon-0.9.0",
            protocolVersion: 1,
            readCompatible: true,
            writeCompatible: true,
            capabilityIDs: ["fleet.snapshot.read", "fleet.action.execute"]
        )

        XCTAssertThrowsError(try FleetConnection.validateCapability("fleet.receipt.read", in: oldCatalogue)) {
            XCTAssertEqual($0 as? FleetConnectionError, .missingNegotiatedCapability("fleet.receipt.read"))
        }
        XCTAssertThrowsError(try FleetConnection.validateCapability("fleet.start.execute", in: oldCatalogue)) {
            XCTAssertEqual($0 as? FleetConnectionError, .missingNegotiatedCapability("fleet.start.execute"))
        }
    }

    func testATCReadProjectionDecodesOwnershipAndScheduleFacts() throws {
        let result = try FleetWire.decoder().decode(AtcListResult.self, from: Data(#"""
        {"instances":[{"name":"main","cwd":"/tmp","tmux_session":"atc-main","heartbeat_cron":"*/2 * * * *","err_retry_cap":3,"idle_pause_min":60,"next_tick_at":2000,"enabled":true,"last_heartbeat_at":1000,"config_generation":4}],"scheduler_ownership":"legacy_timer_reconciliation_required"}
        """#.utf8))

        XCTAssertEqual(result.instances.map(\.name), ["main"])
        XCTAssertEqual(result.instances.first?.configGeneration, 4)
        XCTAssertEqual(result.schedulerOwnership, .legacyTimerReconciliationRequired)
    }

    func testOldCatalogueRefusesReceiptAndStartBeforeWireIO() async throws {
        var descriptors = [Int32](repeating: 0, count: 2)
        XCTAssertEqual(Darwin.socketpair(AF_UNIX, SOCK_STREAM, 0, &descriptors), 0)
        let clientDescriptor = descriptors[0]
        let serverDescriptor = descriptors[1]
        defer { Darwin.close(serverDescriptor) }
        let serverDone = expectation(description: "old catalogue server finished")
        let serverResult = SocketServerResult()

        DispatchQueue.global().async {
            defer { serverDone.fulfill() }
            do {
                let authentication = try Self.readRequest(from: serverDescriptor)
                try Self.writeResponse(to: serverDescriptor, request: authentication, result: [:])
                let negotiation = try Self.readRequest(from: serverDescriptor)
                try Self.writeResponse(to: serverDescriptor, request: negotiation, result: [
                    "daemon_version": "old-daemon",
                    "protocol_version": 1,
                    "read_compatible": true,
                    "write_compatible": true,
                    "capability_ids": ["fleet.snapshot.read"],
                ])
                var pollDescriptor = pollfd(fd: serverDescriptor, events: Int16(POLLIN), revents: 0)
                if Darwin.poll(&pollDescriptor, 1, 200) > 0 {
                    throw StoreServerError.closed
                }
            } catch {
                serverResult.record(error)
            }
        }

        let connection = FleetConnection(
            location: HangarLocation(environment: ["AINB_HANGAR_HOME": "/unused"]),
            injectedDescriptor: clientDescriptor
        )
        try await connection.connect()
        defer { Task { await connection.close() } }
        try await connection.authenticate(token: "mdt_test")
        _ = try await connection.negotiate()

        do {
            _ = try await connection.receiptList(FleetReceiptListParams(limit: 1))
            XCTFail("old catalogue must refuse receipt reads")
        } catch let error as FleetConnectionError {
            XCTAssertEqual(error, .missingNegotiatedCapability("fleet.receipt.read"))
        }
        do {
            _ = try await connection.start(FleetStartParams(requestID: "request", provider: .codex, cwd: "/tmp", prompt: nil))
            XCTFail("old catalogue must refuse fleet/start")
        } catch let error as FleetConnectionError {
            XCTAssertEqual(error, .missingNegotiatedCapability("fleet.start.execute"))
        }
        await fulfillment(of: [serverDone], timeout: 1)
        try serverResult.throwIfRecorded()
    }

    func testTmuxTextCapabilityAllowsPromptAndBroadcastTarget() {
        let capabilities = FleetCapabilities(
            structuredAnswer: false, approvals: false, sendPrompt: false, continueTurn: false,
            retry: false, interrupt: false, start: false, stop: false, restart: false,
            kill: false, archive: false, tmuxAttach: false, tmuxText: true, verifiedPicker: false
        )

        XCTAssertTrue(FleetOperatorAction.sendPrompt.isAvailable(in: capabilities))
    }

    @MainActor
    func testBroadcastReceiptMergePreservesDaemonInputOrder() {
        let existing = [receipt("old")]
        let daemonOrder = [receipt("first"), receipt("second")]

        XCTAssertEqual(FleetStore.mergedReceipts(daemonOrder, existing: existing).map(\.requestID), ["first", "second", "old"])
    }

    func testStartCWDPreflightRequiresExistingDirectory() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        let file = root.appendingPathComponent("file")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        try Data().write(to: file)

        XCTAssertTrue(FleetStartPreflight.isExistingDirectory(root.path))
        XCTAssertFalse(FleetStartPreflight.isExistingDirectory(file.path))
        XCTAssertFalse(FleetStartPreflight.isExistingDirectory(root.appendingPathComponent("missing").path))
    }

    func testStartPreflightSupportsOnlyCodex() {
        XCTAssertTrue(FleetStartPreflight.supports(.codex))
        XCTAssertFalse(FleetStartPreflight.supports(.claude))
        XCTAssertFalse(FleetStartPreflight.supports(.unknown))
    }

    func testUnavailablePromptNeverWritesActionWire() async throws {
        var descriptors = [Int32](repeating: 0, count: 2)
        XCTAssertEqual(Darwin.socketpair(AF_UNIX, SOCK_STREAM, 0, &descriptors), 0)
        let serverDescriptor = descriptors[1]
        defer { Darwin.close(serverDescriptor) }
        let location = try Self.testLocation()
        defer { try? FileManager.default.removeItem(at: location.home) }
        let releasePoll = DispatchSemaphore(value: 0)
        let serverDone = expectation(description: "prompt refusal server finished")
        let serverResult = SocketServerResult()

        DispatchQueue.global().async {
            defer { serverDone.fulfill() }
            do {
                let authentication = try Self.readRequest(from: serverDescriptor)
                try Self.writeResponse(to: serverDescriptor, request: authentication, result: [:])
                let negotiation = try Self.readRequest(from: serverDescriptor)
                try Self.writeResponse(to: serverDescriptor, request: negotiation, result: [
                    "daemon_version": "fixture-daemon",
                    "protocol_version": 1,
                    "read_compatible": true,
                    "write_compatible": true,
                    "capability_ids": ["fleet.action.execute"],
                ])
                let subscription = try Self.readRequest(from: serverDescriptor)
                try Self.writeResponse(to: serverDescriptor, request: subscription, result: [
                    "snapshot": try Self.snapshotObject(head: 1, sessions: [try Self.sampleSessionObject(head: 1)]),
                    "replay": [],
                    "replay_state": ["state": "complete"],
                ])
                _ = releasePoll.wait(timeout: .now() + 1)
                var pollDescriptor = pollfd(fd: serverDescriptor, events: Int16(POLLIN), revents: 0)
                if Darwin.poll(&pollDescriptor, 1, 200) > 0 {
                    throw StoreServerError.closed
                }
            } catch {
                serverResult.record(error)
            }
        }

        let store = await MainActor.run {
            FleetStore(
                location: location,
                makeConnection: { _ in FleetConnection(location: location, injectedDescriptor: descriptors[0]) },
                reconnectDelayNanoseconds: { _ in 10_000_000_000 }
            )
        }
        await MainActor.run { store.start() }
        let live = await Self.waitUntil {
            await MainActor.run {
                guard case .live = store.connectionState else { return false }
                return store.sessions.count == 1
            }
        }
        XCTAssertTrue(live)
        await MainActor.run {
            let session = store.sessions[0]
            store.selectedSessionKey = session.sessionKey
            XCTAssertFalse(store.canSendPrompt("hello", on: session))
            store.perform(.sendPrompt, on: session, prompt: "hello")
        }
        releasePoll.signal()
        await fulfillment(of: [serverDone], timeout: 1)
        try serverResult.throwIfRecorded()
        await MainActor.run { store.stop() }
    }

    func testReconnectReloadsReceiptsBeforeReturningLive() async throws {
        var first = [Int32](repeating: 0, count: 2)
        var second = [Int32](repeating: 0, count: 2)
        XCTAssertEqual(Darwin.socketpair(AF_UNIX, SOCK_STREAM, 0, &first), 0)
        XCTAssertEqual(Darwin.socketpair(AF_UNIX, SOCK_STREAM, 0, &second), 0)
        let firstServer = first[1]
        let secondServer = second[1]
        defer {
            Darwin.close(firstServer)
            Darwin.close(secondServer)
        }
        let location = try Self.testLocation()
        defer { try? FileManager.default.removeItem(at: location.home) }
        let factory = TestConnectionFactory(descriptors: [first[0], second[0]], location: location)
        let store = await MainActor.run {
            FleetStore(location: location, makeConnection: factory.make, reconnectDelayNanoseconds: { _ in 0 })
        }
        let firstDone = expectation(description: "first receipt server finished")
        let secondReady = expectation(description: "second receipt server ready")
        let releaseSecond = DispatchSemaphore(value: 0)
        let serverResult = SocketServerResult()

        DispatchQueue.global().async {
            defer { firstDone.fulfill() }
            do {
                try Self.serveStoreBootstrap(
                    descriptor: firstServer,
                    subscriptionSnapshot: try Self.snapshotObject(head: 1, sessions: [try Self.sampleSessionObject(head: 1)]),
                    eventBeforeSubscriptionResponse: false,
                    snapshotAfterEvent: try Self.snapshotObject(head: 1, sessions: [try Self.sampleSessionObject(head: 1)]),
                    capabilityIDs: ["fleet.receipt.read"],
                    receiptList: [try Self.receiptObject(requestID: "first")]
                )
                Darwin.shutdown(firstServer, SHUT_RDWR)
            } catch {
                serverResult.record(error)
            }
        }
        DispatchQueue.global().async {
            defer { Darwin.shutdown(secondServer, SHUT_RDWR) }
            do {
                try Self.serveStoreBootstrap(
                    descriptor: secondServer,
                    subscriptionSnapshot: try Self.snapshotObject(head: 1, sessions: [try Self.sampleSessionObject(head: 1)]),
                    eventBeforeSubscriptionResponse: false,
                    snapshotAfterEvent: try Self.snapshotObject(head: 1, sessions: [try Self.sampleSessionObject(head: 1)]),
                    capabilityIDs: ["fleet.receipt.read"],
                    receiptList: [try Self.receiptObject(requestID: "second")]
                )
                secondReady.fulfill()
                _ = releaseSecond.wait(timeout: .now() + 2)
            } catch {
                serverResult.record(error)
                secondReady.fulfill()
            }
        }

        await MainActor.run { store.start() }
        await fulfillment(of: [firstDone, secondReady], timeout: 2)
        let reloaded = await Self.waitUntil {
            await MainActor.run {
                guard case .live = store.connectionState else { return false }
                return factory.count == 2 && store.receipts.map(\.requestID) == ["second"]
            }
        }
        await MainActor.run { store.stop() }
        releaseSecond.signal()
        try serverResult.throwIfRecorded()
        XCTAssertTrue(reloaded, "reconnect must reload durable receipts before Fleet becomes live")
    }

    func testOptionalPreloadFailuresPreserveCachedProjections() async throws {
        var first = [Int32](repeating: 0, count: 2)
        var second = [Int32](repeating: 0, count: 2)
        XCTAssertEqual(Darwin.socketpair(AF_UNIX, SOCK_STREAM, 0, &first), 0)
        XCTAssertEqual(Darwin.socketpair(AF_UNIX, SOCK_STREAM, 0, &second), 0)
        let firstServer = first[1]
        let secondServer = second[1]
        defer {
            Darwin.close(firstServer)
            Darwin.close(secondServer)
        }
        let location = try Self.testLocation()
        defer { try? FileManager.default.removeItem(at: location.home) }
        let factory = TestConnectionFactory(descriptors: [first[0], second[0]], location: location)
        let store = await MainActor.run {
            FleetStore(location: location, makeConnection: factory.make, reconnectDelayNanoseconds: { _ in 0 })
        }
        let firstDone = expectation(description: "cached optional projections loaded")
        let secondReady = expectation(description: "optional preload failures returned")
        let releaseSecond = DispatchSemaphore(value: 0)
        let serverResult = SocketServerResult()

        DispatchQueue.global().async {
            defer {
                Darwin.shutdown(firstServer, SHUT_RDWR)
                firstDone.fulfill()
            }
            do {
                try Self.serveOptionalPreloads(descriptor: firstServer, succeed: true)
            } catch {
                serverResult.record(error)
            }
        }
        DispatchQueue.global().async {
            defer { Darwin.shutdown(secondServer, SHUT_RDWR) }
            do {
                try Self.serveOptionalPreloads(descriptor: secondServer, succeed: false)
                secondReady.fulfill()
                _ = releaseSecond.wait(timeout: .now() + 2)
            } catch {
                serverResult.record(error)
                secondReady.fulfill()
            }
        }

        await MainActor.run { store.start() }
        await fulfillment(of: [firstDone, secondReady], timeout: 2)
        try serverResult.throwIfRecorded()
        let preserved = await Self.waitUntil {
            await MainActor.run {
                guard case .live = store.connectionState else { return false }
                return factory.count == 2
                    && store.receipts.map(\.requestID) == ["cached"]
                    && store.atcInstances.map(\.name) == ["main"]
                    && store.atcSchedulerOwnership == .legacyTimerReconciliationRequired
                    && store.timeline.map(\.revision) == [7]
            }
        }
        await MainActor.run { store.stop() }
        releaseSecond.signal()

        XCTAssertTrue(preserved, "transient optional preload failures must keep last known good projections")
    }

    func testFastSuccessfulReconnectsStillExhaustBoundedRetryBudget() async throws {
        var pairs = [[Int32]]()
        for _ in 0...4 {
            var pair = [Int32](repeating: 0, count: 2)
            XCTAssertEqual(Darwin.socketpair(AF_UNIX, SOCK_STREAM, 0, &pair), 0)
            pairs.append(pair)
        }
        defer { Darwin.close(pairs[4][1]) }
        let location = try Self.testLocation()
        defer { try? FileManager.default.removeItem(at: location.home) }
        let factory = TestConnectionFactory(descriptors: pairs.map { $0[0] }, location: location)
        let store = await MainActor.run {
            FleetStore(location: location, makeConnection: factory.make, reconnectDelayNanoseconds: { _ in 0 })
        }
        let serversDone = expectation(description: "fast-closing servers finished")
        serversDone.expectedFulfillmentCount = 4

        for serverDescriptor in pairs.prefix(4).map({ $0[1] }) {
            DispatchQueue.global().async {
                defer {
                    Darwin.shutdown(serverDescriptor, SHUT_RDWR)
                    Darwin.close(serverDescriptor)
                    serversDone.fulfill()
                }
                try? Self.serveStoreBootstrap(
                    descriptor: serverDescriptor,
                    subscriptionSnapshot: Self.snapshotObject(head: 0, sessions: []),
                    eventBeforeSubscriptionResponse: false,
                    snapshotAfterEvent: Self.snapshotObject(head: 0, sessions: [])
                )
            }
        }

        await MainActor.run { store.start() }
        await fulfillment(of: [serversDone], timeout: 3)
        let stoppedReconnecting = await Self.waitUntil {
            await MainActor.run { factory.count == 4 && store.debugConnectionTaskCount == 0 }
        }
        if factory.count == 4 {
            Darwin.close(pairs[4][0])
        }
        await MainActor.run { store.stop() }

        XCTAssertTrue(stoppedReconnecting, "successful handshakes that close immediately must not reset the retry budget")
    }

    func testResyncNotificationStreamsThroughOwnedSocket() async throws {
        var descriptors = [Int32](repeating: 0, count: 2)
        XCTAssertEqual(Darwin.socketpair(AF_UNIX, SOCK_STREAM, 0, &descriptors), 0)
        let clientDescriptor = descriptors[0]
        let serverDescriptor = descriptors[1]
        defer { Darwin.close(serverDescriptor) }

        let connection = FleetConnection(
            location: HangarLocation(environment: ["AINB_HANGAR_HOME": "/unused"]),
            injectedDescriptor: clientDescriptor
        )
        try await connection.connect()
        defer { Task { await connection.close() } }
        let stream = await connection.incoming()
        let waiter = Task { () -> FleetResyncRequired? in
            var iterator = stream.makeAsyncIterator()
            while let incoming = await iterator.next() {
                if case let .resyncRequired(resync) = incoming {
                    return resync
                }
            }
            return nil
        }

        let body = Data(#"{"jsonrpc":"2.0","method":"fleet/resync_required","params":{"after_revision":4,"missed":2}}"#.utf8)
        let frame = try ContentLengthEncoder.encode(body)
        let written = frame.withUnsafeBytes { Darwin.write(serverDescriptor, $0.baseAddress, frame.count) }
        XCTAssertEqual(written, frame.count)

        let resync = await waiter.value
        XCTAssertEqual(resync, FleetResyncRequired(afterRevision: 4, missed: 2))
    }

    func testCancelledRequestClosesOwnedDescriptor() async throws {
        var descriptors = [Int32](repeating: 0, count: 2)
        XCTAssertEqual(Darwin.socketpair(AF_UNIX, SOCK_STREAM, 0, &descriptors), 0)
        let clientDescriptor = descriptors[0]
        let serverDescriptor = descriptors[1]
        let requestReceived = expectation(description: "client request received")
        let peerClosed = expectation(description: "client descriptor closed")

        DispatchQueue.global().async {
            _ = Self.readFrame(from: serverDescriptor)
            requestReceived.fulfill()
            var byte: UInt8 = 0
            if Darwin.read(serverDescriptor, &byte, 1) == 0 {
                peerClosed.fulfill()
            }
            Darwin.close(serverDescriptor)
        }

        let connection = FleetConnection(
            location: HangarLocation(environment: ["AINB_HANGAR_HOME": "/unused"]),
            injectedDescriptor: clientDescriptor
        )
        try await connection.connect()
        let authentication = Task {
            try await connection.authenticate(token: "mdt_test")
        }

        await fulfillment(of: [requestReceived], timeout: 1)
        authentication.cancel()
        do {
            try await authentication.value
            XCTFail("expected cancellation")
        } catch is CancellationError {
        } catch {
            XCTFail("unexpected error: \(error)")
        }
        await fulfillment(of: [peerClosed], timeout: 1)
    }

    func testStoreBuffersEventArrivingBeforeSubscribeResponse() async throws {
        var descriptors = [Int32](repeating: 0, count: 2)
        XCTAssertEqual(Darwin.socketpair(AF_UNIX, SOCK_STREAM, 0, &descriptors), 0)
        let clientDescriptor = descriptors[0]
        let serverDescriptor = descriptors[1]
        defer { Darwin.close(serverDescriptor) }

        let location = try Self.testLocation()
        defer { try? FileManager.default.removeItem(at: location.home) }
        let connection = FleetConnection(location: location, injectedDescriptor: clientDescriptor)
        let store = await MainActor.run {
            FleetStore(
                location: location,
                makeConnection: { _ in connection },
                reconnectDelayNanoseconds: { _ in 0 }
            )
        }
        let serverDone = expectation(description: "store bootstrap server finished")
        let serverResult = SocketServerResult()
        DispatchQueue.global().async {
            defer { serverDone.fulfill() }
            do {
                try Self.serveStoreBootstrap(
                    descriptor: serverDescriptor,
                    subscriptionSnapshot: try Self.snapshotObject(head: 0, sessions: []),
                    eventBeforeSubscriptionResponse: true,
                    snapshotAfterEvent: try Self.snapshotObject(head: 1, sessions: [try Self.sampleSessionObject(head: 1)])
                )
            } catch {
                serverResult.record(error)
            }
        }

        await MainActor.run { store.start() }
        await fulfillment(of: [serverDone], timeout: 2)
        try serverResult.throwIfRecorded()
        let receivedSnapshot = await Self.waitUntil {
            await MainActor.run { store.sessions.map(\.sessionKey) == ["s1"] }
        }
        await MainActor.run { store.stop() }

        XCTAssertTrue(receivedSnapshot, "event delivered before subscribe response must trigger an authoritative snapshot")
    }

    func testStoreReconnectsAfterLiveSnapshotFailure() async throws {
        var first = [Int32](repeating: 0, count: 2)
        var second = [Int32](repeating: 0, count: 2)
        XCTAssertEqual(Darwin.socketpair(AF_UNIX, SOCK_STREAM, 0, &first), 0)
        XCTAssertEqual(Darwin.socketpair(AF_UNIX, SOCK_STREAM, 0, &second), 0)
        let firstServer = first[1]
        let secondServer = second[1]
        defer {
            Darwin.close(firstServer)
            Darwin.close(secondServer)
        }

        let location = try Self.testLocation()
        defer { try? FileManager.default.removeItem(at: location.home) }
        let factory = TestConnectionFactory(
            descriptors: [first[0], second[0]],
            location: location
        )
        let store = await MainActor.run {
            FleetStore(
                location: location,
                makeConnection: factory.make,
                reconnectDelayNanoseconds: { _ in 0 }
            )
        }
        let firstServerDone = expectation(description: "first store server finished")
        let firstServerResult = SocketServerResult()
        DispatchQueue.global().async {
            defer { firstServerDone.fulfill() }
            do {
                try Self.serveStoreBootstrap(
                    descriptor: firstServer,
                    subscriptionSnapshot: try Self.snapshotObject(head: 0, sessions: [try Self.sampleSessionObject(head: 0)]),
                    eventBeforeSubscriptionResponse: false,
                    snapshotAfterEvent: nil
                )
                Darwin.shutdown(firstServer, SHUT_RDWR)
            } catch {
                firstServerResult.record(error)
            }
        }
        await MainActor.run { store.start() }
        await fulfillment(of: [firstServerDone], timeout: 2)
        try firstServerResult.throwIfRecorded()
        let reconnected = await Self.waitUntil {
            factory.count == 2
        }
        await MainActor.run { store.stop() }

        XCTAssertTrue(reconnected, "snapshot failure after a live subscription must start exactly one bounded reconnect attempt")
    }

    func testStoreReconnectsWhenBootstrapRequiresResubscribeBeforeLive() async throws {
        var first = [Int32](repeating: 0, count: 2)
        var second = [Int32](repeating: 0, count: 2)
        XCTAssertEqual(Darwin.socketpair(AF_UNIX, SOCK_STREAM, 0, &first), 0)
        XCTAssertEqual(Darwin.socketpair(AF_UNIX, SOCK_STREAM, 0, &second), 0)
        let firstServer = first[1]
        let secondServer = second[1]
        defer {
            Darwin.close(firstServer)
            Darwin.close(secondServer)
        }

        let location = try Self.testLocation()
        defer { try? FileManager.default.removeItem(at: location.home) }
        let factory = TestConnectionFactory(descriptors: [first[0], second[0]], location: location)
        let store = await MainActor.run {
            FleetStore(location: location, makeConnection: factory.make, reconnectDelayNanoseconds: { _ in 0 })
        }
        let firstServerDone = expectation(description: "invalid bootstrap server finished")
        let secondServerReady = expectation(description: "reconnected bootstrap server ready")
        let releaseSecondServer = DispatchSemaphore(value: 0)

        DispatchQueue.global().async {
            defer { firstServerDone.fulfill() }
            do {
                try Self.serveStoreBootstrap(
                    descriptor: firstServer,
                    subscriptionSnapshot: try Self.snapshotObject(head: 0, sessions: []),
                    eventBeforeSubscriptionResponse: false,
                    snapshotAfterEvent: try Self.snapshotObject(head: 0, sessions: []),
                    subscriptionReplay: [try Self.eventObject(revision: 1)],
                    replayState: ["state": "snapshot_reset", "reason": "bootstrap"]
                )
                Darwin.shutdown(firstServer, SHUT_RDWR)
            } catch {}
        }
        DispatchQueue.global().async {
            defer { Darwin.shutdown(secondServer, SHUT_RDWR) }
            do {
                try Self.serveStoreBootstrap(
                    descriptor: secondServer,
                    subscriptionSnapshot: try Self.snapshotObject(head: 0, sessions: [try Self.sampleSessionObject(head: 0)]),
                    eventBeforeSubscriptionResponse: false,
                    snapshotAfterEvent: try Self.snapshotObject(head: 0, sessions: [try Self.sampleSessionObject(head: 0)])
                )
                secondServerReady.fulfill()
                _ = releaseSecondServer.wait(timeout: .now() + 2)
            } catch {
                secondServerReady.fulfill()
            }
        }

        await MainActor.run { store.start() }
        await fulfillment(of: [firstServerDone, secondServerReady], timeout: 2)
        let reconnectedBeforeLive = await Self.waitUntil {
            await MainActor.run {
                guard case .live = store.connectionState else { return false }
                return factory.count == 2
            }
        }
        await MainActor.run { store.stop() }
        releaseSecondServer.signal()

        XCTAssertTrue(reconnectedBeforeLive, "invalid bootstrap replay must reconnect before Fleet becomes live")
    }

    func testStoreMarksProjectionForResubscribeBeforeResyncReconnect() async throws {
        var descriptors = [Int32](repeating: 0, count: 2)
        XCTAssertEqual(Darwin.socketpair(AF_UNIX, SOCK_STREAM, 0, &descriptors), 0)
        let serverDescriptor = descriptors[1]
        defer { Darwin.close(serverDescriptor) }

        let location = try Self.testLocation()
        defer { try? FileManager.default.removeItem(at: location.home) }
        let store = await MainActor.run {
            FleetStore(
                location: location,
                makeConnection: { _ in FleetConnection(location: location, injectedDescriptor: descriptors[0]) },
                reconnectDelayNanoseconds: { _ in 10_000_000_000 }
            )
        }
        let serverDone = expectation(description: "resync notification sent")
        let serverResult = SocketServerResult()
        DispatchQueue.global().async {
            defer { serverDone.fulfill() }
            do {
                try Self.serveStoreBootstrap(
                    descriptor: serverDescriptor,
                    subscriptionSnapshot: try Self.snapshotObject(head: 0, sessions: [try Self.sampleSessionObject(head: 0)]),
                    eventBeforeSubscriptionResponse: false,
                    snapshotAfterEvent: try Self.snapshotObject(head: 0, sessions: []),
                    resyncAfterSubscription: true
                )
            } catch {
                serverResult.record(error)
            }
        }

        await MainActor.run { store.start() }
        await fulfillment(of: [serverDone], timeout: 2)
        try serverResult.throwIfRecorded()
        let reducerApplied = await Self.waitUntil {
            await MainActor.run { store.needsResubscribe }
        }
        await MainActor.run { store.stop() }

        XCTAssertTrue(reducerApplied, "resync notification must mark projection before reconnect scheduling")
    }

    private static func readFrame(from descriptor: Int32) -> Data? {
        var decoder = ContentLengthDecoder()
        var bytes = [UInt8](repeating: 0, count: 1024)
        while true {
            let count = Darwin.read(descriptor, &bytes, bytes.count)
            guard count > 0 else { return nil }
            guard let frames = try? decoder.append(Data(bytes.prefix(Int(count)))) else { return nil }
            if let frame = frames.first { return frame }
        }
    }

    private func receipt(_ requestID: String) -> FleetActionReceipt {
        FleetActionReceipt(
            requestID: requestID,
            sessionKey: "session-\(requestID)",
            actionKind: "send_prompt",
            actionFingerprint: "fingerprint-\(requestID)",
            expectedVersion: 1,
            idempotencyKey: nil,
            status: .pending,
            detail: nil,
            sessionVersion: nil,
            createdAt: 1,
            updatedAt: 1
        )
    }

    private static func testLocation() throws -> HangarLocation {
        let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        let tokenDirectory = home.appendingPathComponent("hangar", isDirectory: true)
        try FileManager.default.createDirectory(at: tokenDirectory, withIntermediateDirectories: true)
        try Data("mdt_test".utf8).write(to: tokenDirectory.appendingPathComponent("daemon.token"))
        return HangarLocation(environment: ["AINB_HANGAR_HOME": home.path])
    }

    private static func serveStoreBootstrap(
        descriptor: Int32,
        subscriptionSnapshot: Any,
        eventBeforeSubscriptionResponse: Bool,
        snapshotAfterEvent: Any?,
        subscriptionReplay: [Any] = [],
        replayState: [String: Any] = ["state": "complete"],
        resyncAfterSubscription: Bool = false,
        capabilityIDs: [String] = [],
        receiptList: [Any] = []
    ) throws {
        let authentication = try readRequest(from: descriptor)
        try writeResponse(to: descriptor, request: authentication, result: [:])

        let negotiation = try readRequest(from: descriptor)
        try writeResponse(to: descriptor, request: negotiation, result: [
            "daemon_version": "fixture-daemon",
            "protocol_version": 1,
            "read_compatible": true,
            "write_compatible": true,
            "capability_ids": capabilityIDs,
        ])

        let subscription = try readRequest(from: descriptor)
        let event = try eventObject(revision: 1)
        if eventBeforeSubscriptionResponse {
            try writeNotification(to: descriptor, method: "fleet/event", params: event)
        }
        try writeResponse(to: descriptor, request: subscription, result: [
            "snapshot": subscriptionSnapshot,
            "replay": subscriptionReplay,
            "replay_state": replayState,
        ])
        if capabilityIDs.contains("fleet.receipt.read") {
            let receiptRequest = try readRequest(from: descriptor)
            XCTAssertEqual(receiptRequest["method"] as? String, "fleet/receipt_list")
            try writeResponse(to: descriptor, request: receiptRequest, result: ["receipts": receiptList])
        }
        if resyncAfterSubscription {
            try writeNotification(
                to: descriptor,
                method: "fleet/resync_required",
                params: ["after_revision": 0, "missed": 1]
            )
        }
        if eventBeforeSubscriptionResponse {
            let snapshot = try readRequest(from: descriptor)
            XCTAssertEqual(snapshot["method"] as? String, "fleet/snapshot")
            try writeResponse(to: descriptor, request: snapshot, result: snapshotAfterEvent ?? subscriptionSnapshot)
        } else if snapshotAfterEvent == nil {
            try writeNotification(to: descriptor, method: "fleet/event", params: event)
            _ = try readRequest(from: descriptor)
        }
    }

    private static func serveOptionalPreloads(descriptor: Int32, succeed: Bool) throws {
        let authentication = try readRequest(from: descriptor)
        try writeResponse(to: descriptor, request: authentication, result: [:])
        let negotiation = try readRequest(from: descriptor)
        try writeResponse(to: descriptor, request: negotiation, result: [
            "daemon_version": "fixture-daemon",
            "protocol_version": 1,
            "read_compatible": true,
            "write_compatible": true,
            "capability_ids": ["fleet.receipt.read", "fleet.atc.read", "fleet.timeline.read"],
        ])
        let subscription = try readRequest(from: descriptor)
        try writeResponse(to: descriptor, request: subscription, result: [
            "snapshot": try snapshotObject(head: 0, sessions: []),
            "replay": [],
            "replay_state": ["state": "complete"],
        ])
        let results: [String: Any] = [
            "fleet/receipt_list": ["receipts": [try receiptObject(requestID: "cached")]],
            "atc/list": [
                "instances": [[
                    "name": "main", "cwd": "/tmp", "tmux_session": "atc-main",
                    "heartbeat_cron": "*/2 * * * *", "err_retry_cap": 3,
                    "idle_pause_min": 60, "next_tick_at": 2_000, "enabled": true,
                    "last_heartbeat_at": 1_000, "config_generation": 4,
                ]],
                "scheduler_ownership": "legacy_timer_reconciliation_required",
            ],
            "fleet/timeline": [
                "entries": [[
                    "revision": 7, "session_key": "s1", "observed_at": 1_000,
                    "provenance": "authoritative", "kind": "turn_completed",
                    "applied": true, "session_version": 3,
                ]],
                "next_after_revision": 7,
            ],
        ]
        for method in ["fleet/receipt_list", "atc/list", "fleet/timeline"] {
            let request = try readRequest(from: descriptor)
            XCTAssertEqual(request["method"] as? String, method)
            if succeed {
                try writeResponse(to: descriptor, request: request, result: results[method]!)
            } else {
                try writeError(to: descriptor, request: request)
            }
        }
    }

    private static func readRequest(from descriptor: Int32) throws -> [String: Any] {
        guard let frame = readFrame(from: descriptor),
              let request = try JSONSerialization.jsonObject(with: frame) as? [String: Any] else {
            throw StoreServerError.closed
        }
        return request
    }

    private static func writeResponse(to descriptor: Int32, request: [String: Any], result: Any) throws {
        try writeJSONObject(["jsonrpc": "2.0", "id": request["id"] as Any, "result": result], to: descriptor)
    }

    private static func writeError(to descriptor: Int32, request: [String: Any]) throws {
        try writeJSONObject([
            "jsonrpc": "2.0",
            "id": request["id"] as Any,
            "error": ["code": -32603, "message": "fixture failure"],
        ], to: descriptor)
    }

    private static func writeNotification(to descriptor: Int32, method: String, params: Any) throws {
        try writeJSONObject(["jsonrpc": "2.0", "method": method, "params": params], to: descriptor)
    }

    private static func writeJSONObject(_ object: [String: Any], to descriptor: Int32) throws {
        let frame = try ContentLengthEncoder.encode(JSONSerialization.data(withJSONObject: object))
        let written = frame.withUnsafeBytes { Darwin.write(descriptor, $0.baseAddress, frame.count) }
        guard written == frame.count else { throw StoreServerError.closed }
    }

    private static func snapshotObject(head: Int64, sessions: [Any]) throws -> Any {
        ["head_revision": head, "sessions": sessions]
    }

    private static func sampleSessionObject(head: Int64) throws -> Any {
        let object = try JSONSerialization.jsonObject(with: FleetWire.encoder().encode(sampleSnapshot(head: head)))
        guard let snapshot = object as? [String: Any], let session = (snapshot["sessions"] as? [Any])?.first else {
            throw StoreServerError.closed
        }
        return session
    }

    private static func receiptObject(requestID: String) throws -> Any {
        let receipt = FleetActionReceipt(
            requestID: requestID,
            sessionKey: "s1",
            actionKind: "send_prompt",
            actionFingerprint: "fingerprint-\(requestID)",
            expectedVersion: 3,
            idempotencyKey: nil,
            status: .pending,
            detail: nil,
            sessionVersion: nil,
            createdAt: 1,
            updatedAt: 1
        )
        return try JSONSerialization.jsonObject(with: FleetWire.encoder().encode(receipt))
    }

    private static func eventObject(revision: Int64) throws -> Any {
        try JSONSerialization.jsonObject(with: FleetWire.encoder().encode(sampleEvent(revision: revision, eventID: "event-\(revision)")))
    }

    private static func waitUntil(_ condition: @escaping () async -> Bool) async -> Bool {
        let deadline = Date().addingTimeInterval(2)
        while Date() < deadline {
            if await condition() { return true }
            try? await Task.sleep(for: .milliseconds(20))
        }
        return false
    }
}

private final class TestConnectionFactory: @unchecked Sendable {
    private let lock = NSLock()
    private var descriptors: [Int32]
    private let initialCount: Int
    private let location: HangarLocation

    init(descriptors: [Int32], location: HangarLocation) {
        self.descriptors = descriptors
        initialCount = descriptors.count
        self.location = location
    }

    var count: Int {
        lock.lock()
        defer { lock.unlock() }
        return initialCount - descriptors.count
    }

    func make(_: HangarLocation) -> FleetConnection {
        lock.lock()
        defer { lock.unlock() }
        return FleetConnection(location: location, injectedDescriptor: descriptors.removeFirst())
    }
}

private enum StoreServerError: Error {
    case closed
}

private final class SocketServerResult: @unchecked Sendable {
    private let lock = NSLock()
    private var error: Error?

    func record(_ error: Error) {
        lock.lock()
        self.error = error
        lock.unlock()
    }

    func throwIfRecorded() throws {
        lock.lock()
        defer { lock.unlock() }
        if let error { throw error }
    }
}
