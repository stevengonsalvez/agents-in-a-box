import Foundation
import XCTest
@testable import AINBFleet

final class CanonicalFixtureTests: XCTestCase {
    func testGeneratedFleetFixturesDecodeAndRoundTripSemantically() throws {
        try assertFixture("auth-hello-request", as: RPCRequest<AuthHelloParams>.self)
        try assertFixture("negotiate-request", as: RPCRequest<FleetNegotiateParams>.self)
        try assertFixture("negotiate-result-compatible", as: FleetNegotiateResult.self)
        try assertFixture("negotiate-result-read-only", as: FleetNegotiateResult.self)
        try assertFixture("snapshot", as: FleetSnapshot.self)
        try assertFixture("subscribe-bootstrap", as: FleetSubscribeResult.self)
        try assertFixture("subscribe-replay", as: FleetSubscribeResult.self)
        try assertFixture("fleet-event", as: FleetEvent.self)
        try assertFixture("action-request", as: FleetActionParams.self)
        try assertFixture("action-receipt", as: FleetActionReceipt.self)
        try assertFixture("broadcast-request", as: FleetBroadcastParams.self)
        try assertFixture("broadcast-result", as: FleetBroadcastResult.self)
        try assertFixture("rpc-error", as: RPCError.self)
    }

    func testReplayStateRejectsInconsistentForms() {
        XCTAssertThrowsError(try FleetWire.decoder().decode(FleetReplayState.self, from: Data(#"{"state":"complete","reason":"bootstrap"}"#.utf8)))
        XCTAssertThrowsError(try FleetWire.decoder().decode(FleetReplayState.self, from: Data(#"{"state":"snapshot_reset"}"#.utf8)))
        XCTAssertThrowsError(try FleetWire.decoder().decode(FleetReplayState.self, from: Data(#"{"state":"future"}"#.utf8)))
        XCTAssertThrowsError(try FleetWire.decoder().decode(FleetReplayState.self, from: Data(#"{"state":"snapshot_reset","reason":"bootstrap","extra":true}"#.utf8)))
    }

    private func assertFixture<T: Codable>(_ name: String, as type: T.Type, file: StaticString = #filePath, line: UInt = #line) throws {
        let original = try Data(contentsOf: fixtureURL(named: name))
        let decoded = try FleetWire.decoder().decode(T.self, from: original)
        let encoded = try FleetWire.encoder().encode(decoded)
        XCTAssertEqual(try canonicalJSON(original), try canonicalJSON(encoded), "fixture drift: \(name)", file: file, line: line)
    }

    private func fixtureURL(named name: String) -> URL {
        var repository = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { repository.deleteLastPathComponent() }
        return repository.appendingPathComponent("ainb-tui/fleet-parity/fixtures/v1/\(name).json")
    }

    private func canonicalJSON(_ data: Data) throws -> Data {
        try JSONSerialization.data(withJSONObject: JSONSerialization.jsonObject(with: data), options: [.sortedKeys])
    }
}
