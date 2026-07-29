import Foundation
import XCTest
@testable import AINBFleet

final class CanonicalFixtureTests: XCTestCase {
    func testLocalWireSamplesDecodeAndRoundTripSemantically() throws {
        try assertJSON(
            #"{"id":1,"jsonrpc":"2.0","method":"auth/hello","params":{"token":"fixture-token"}}"#,
            as: RPCRequest<AuthHelloParams>.self
        )
        try assertJSON(
            #"{"capability_ids":["fleet.snapshot.read"],"daemon_version":"fixture-daemon","protocol_version":1,"read_compatible":true,"write_compatible":true}"#,
            as: FleetNegotiateResult.self
        )
        try assertJSON(
            #"{"head_revision":42,"sessions":[]}"#,
            as: FleetSnapshot.self
        )
        try assertJSON(
            #"{"action_fingerprint":"sha256:action","action_kind":"structured_answer","created_at":1700000000200,"detail":"fixture delivery","expected_version":7,"idempotency_key":"broadcast-001","request_id":"action-request-001","session_key":"fleet-session-001","session_version":8,"status":"DELIVERED","updated_at":1700000000300}"#,
            as: FleetActionReceipt.self
        )
    }

    func testReplayStateRejectsInconsistentForms() {
        XCTAssertThrowsError(try FleetWire.decoder().decode(FleetReplayState.self, from: Data(#"{"state":"complete","reason":"bootstrap"}"#.utf8)))
        XCTAssertThrowsError(try FleetWire.decoder().decode(FleetReplayState.self, from: Data(#"{"state":"snapshot_reset"}"#.utf8)))
        XCTAssertThrowsError(try FleetWire.decoder().decode(FleetReplayState.self, from: Data(#"{"state":"future"}"#.utf8)))
        XCTAssertThrowsError(try FleetWire.decoder().decode(FleetReplayState.self, from: Data(#"{"state":"snapshot_reset","reason":"bootstrap","extra":true}"#.utf8)))
    }

    private func assertJSON<T: Codable>(_ json: String, as type: T.Type, file: StaticString = #filePath, line: UInt = #line) throws {
        let original = Data(json.utf8)
        let decoded = try FleetWire.decoder().decode(T.self, from: original)
        let encoded = try FleetWire.encoder().encode(decoded)
        XCTAssertEqual(try canonicalJSON(original), try canonicalJSON(encoded), "wire sample drift", file: file, line: line)
    }

    private func canonicalJSON(_ data: Data) throws -> Data {
        try JSONSerialization.data(withJSONObject: JSONSerialization.jsonObject(with: data), options: [.sortedKeys])
    }
}
