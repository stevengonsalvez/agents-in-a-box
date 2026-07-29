import Foundation
import XCTest
@testable import AINBFleet

final class FleetProjectionReducerTests: XCTestCase {
    func testBootstrapUsesOneAuthoritativeSnapshot() throws {
        let snapshot = try sampleSnapshot(head: 4)
        let projection = FleetProjectionReducer.bootstrap(FleetSubscribeResult(snapshot: snapshot, replay: [], replayState: .snapshotReset(reason: .bootstrap)))
        XCTAssertEqual(projection.snapshot, snapshot)
        XCTAssertEqual(projection.committedRevision, 4)
        XCTAssertFalse(projection.needsSnapshot)
    }

    func testCompleteReplayAcceptsContiguousRevisions() throws {
        let projection = FleetProjectionReducer.bootstrap(FleetSubscribeResult(snapshot: try sampleSnapshot(head: 4), replay: [try sampleEvent(revision: 3, eventID: "e3"), try sampleEvent(revision: 4, eventID: "e4")], replayState: .complete))
        XCTAssertEqual(projection.committedRevision, 4)
        XCTAssertFalse(projection.needsResubscribe)
    }

    func testDuplicateRevisionIsIgnored() throws {
        let projection = baseline()
        XCTAssertEqual(FleetProjectionReducer.live(try sampleEvent(revision: 4, eventID: "old"), from: projection), projection)
    }

    func testDuplicateEventIDIsIgnored() throws {
        let projection = FleetProjectionReducer.bootstrap(FleetSubscribeResult(snapshot: try sampleSnapshot(head: 4), replay: [try sampleEvent(revision: 4, eventID: "same")], replayState: .complete))
        XCTAssertEqual(FleetProjectionReducer.live(try sampleEvent(revision: 5, eventID: "same"), from: projection), projection)
    }

    func testDuplicateEventIDIsIgnoredWithinSnapshotBoundary() throws {
        let afterLiveEvent = FleetProjectionReducer.live(try sampleEvent(revision: 5, eventID: "e5"), from: baseline())
        XCTAssertEqual(FleetProjectionReducer.live(try sampleEvent(revision: 6, eventID: "e5"), from: afterLiveEvent), afterLiveEvent)
    }

    func testAuthoritativeSnapshotResetsSeenEventIDs() throws {
        let afterLiveEvent = FleetProjectionReducer.live(try sampleEvent(revision: 5, eventID: "e5"), from: baseline())
        let refreshed = FleetProjectionReducer.snapshot(try sampleSnapshot(head: 5), from: afterLiveEvent)
        XCTAssertEqual(FleetProjectionReducer.live(try sampleEvent(revision: 6, eventID: "e5"), from: refreshed).committedRevision, 6)
    }

    func testGapRequiresResubscribe() throws {
        let projection = FleetProjectionReducer.live(try sampleEvent(revision: 6, eventID: "gap"), from: baseline())
        XCTAssertTrue(projection.needsResubscribe)
        XCTAssertTrue(projection.needsSnapshot)
    }

    func testResyncNotificationRequiresResubscribe() {
        let projection = FleetProjectionReducer.resyncRequired(from: baseline())
        XCTAssertTrue(projection.needsResubscribe)
        XCTAssertTrue(projection.needsSnapshot)
    }

    func testSnapshotCannotClearResubscribeRequirement() throws {
        let resync = FleetProjectionReducer.resyncRequired(from: baseline())
        XCTAssertEqual(FleetProjectionReducer.snapshot(try sampleSnapshot(head: 5), from: resync), resync)
    }

    func testOlderSnapshotCannotReplaceNewerProjection() throws {
        XCTAssertEqual(FleetProjectionReducer.snapshot(try sampleSnapshot(head: 3), from: baseline()), baseline())
    }

    func testLiveRevisionRequestsFocusedSnapshot() throws {
        let projection = FleetProjectionReducer.live(try sampleEvent(revision: 5, eventID: "next"), from: baseline())
        XCTAssertEqual(projection.committedRevision, 5)
        XCTAssertTrue(projection.needsSnapshot)
        XCTAssertFalse(projection.needsResubscribe)
    }

    private func baseline() -> FleetProjection {
        FleetProjectionReducer.bootstrap(FleetSubscribeResult(snapshot: try! sampleSnapshot(head: 4), replay: [], replayState: .snapshotReset(reason: .bootstrap)))
    }
}

func sampleSnapshot(head: Int64, currentRequest: String = "{\"opaque\":\"one\"}") throws -> FleetSnapshot {
    let json = #"{"head_revision":\#(head),"sessions":[{"session_key":"s1","provider":"codex","provider_session_id":null,"tmux_target":null,"process_start_fingerprint":null,"cwd":"/tmp","display_name":null,"lifecycle":"RUNNING","attention":"ASK","current_request_fingerprint":"f1","current_request":\#(currentRequest),"management":"MANAGED","transport_health":"HEALTHY","capabilities":{"structured_answer":true,"approvals":true,"send_prompt":false,"continue_turn":false,"retry":false,"interrupt":true,"start":false,"stop":true,"restart":true,"kill":false,"archive":false,"tmux_attach":true,"tmux_text":false,"verified_picker":false},"provenance":"authoritative","confidence":"HIGH","discovered_at":1,"last_observed_at":2,"lifecycle_updated_at":3,"attention_updated_at":4,"version":3,"updated_revision":\#(head)}]}"#
    return try FleetWire.decoder().decode(FleetSnapshot.self, from: Data(json.utf8))
}

func sampleEvent(revision: Int64, eventID: String, payload: String = "{\"opaque\":\"one\"}") throws -> FleetEvent {
    let json = #"{"revision":\#(revision),"event_id":"\#(eventID)","session_key":"s1","observed_at":5,"provenance":"authoritative","event_type":"provider_event","payload":\#(payload),"session_version":3,"applied":true}"#
    return try FleetWire.decoder().decode(FleetEvent.self, from: Data(json.utf8))
}
