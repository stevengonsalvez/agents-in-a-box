import Foundation
import XCTest
@testable import AINBFleet

/// The daemon may grow enum values without a protocol version bump, so this
/// client must degrade rather than fail when it meets one.
///
/// Before this, every wire enum used synthesized `Codable`, which throws
/// `DecodingError.dataCorrupted` on an unrecognised raw value. Because a
/// snapshot decodes `sessions` as an ARRAY, a single unknown value failed the
/// WHOLE snapshot and the app went blank. Adding `Provider::Copilot` daemon-side
/// would have done exactly that to every un-updated client.
final class FleetWireForwardCompatibilityTests: XCTestCase {
    private func decode<T: Decodable>(_ type: T.Type, _ raw: String) throws -> T {
        try JSONDecoder().decode(type, from: Data("\"\(raw)\"".utf8))
    }

    func testUnknownProviderDecodesAsUnknownRatherThanThrowing() throws {
        XCTAssertEqual(try decode(FleetProvider.self, "gemini"), .unknown)
        XCTAssertEqual(try decode(FleetProvider.self, "claude"), .claude)
        // copilot is a KNOWN provider now, so it must decode as itself.
        XCTAssertEqual(try decode(FleetProvider.self, "copilot"), .copilot)
    }

    func testUnknownLifecycleDecodesAsUnknown() throws {
        XCTAssertEqual(try decode(LifecycleState.self, "HIBERNATING"), .unknown)
        XCTAssertEqual(try decode(LifecycleState.self, "RUNNING"), .running)
    }

    /// Fail-safe direction: an attention state we cannot name still reaches the
    /// operator. Falling back to `.none` would silently drop the session out of
    /// the Needs-input tab, which is the one failure the tab exists to prevent.
    func testUnknownAttentionStaysActionable() throws {
        XCTAssertEqual(try decode(AttentionState.self, "ELICITATION"), .waiting)
        XCTAssertEqual(try decode(AttentionState.self, "NONE"), .none)
    }

    func testUnknownCapabilitySignalsDegradeRatherThanOverPromise() throws {
        XCTAssertEqual(try decode(ManagementState.self, "SUPERVISED"), .degraded)
        XCTAssertEqual(try decode(TransportHealth.self, "FLAKY"), .unknown)
        XCTAssertEqual(try decode(FleetProvenance.self, "measured"), .inferred)
        XCTAssertEqual(try decode(FleetConfidence.self, "CERTAIN"), .low)
    }

    func testKnownValuesStillRoundTripThroughEncoding() throws {
        let encoded = try JSONEncoder().encode(FleetProvider.codex)
        XCTAssertEqual(String(decoding: encoded, as: UTF8.self), "\"codex\"")
    }

    /// The regression that matters: one unknown value inside a session row must
    /// not take down the whole snapshot.
    func testAnUnknownProviderDoesNotFailTheWholeSnapshot() throws {
        let json = """
        {"head_revision":7,"sessions":[
          {"session_key":"legacy:gemini:pane","provider":"gemini","provider_session_id":null,
           "tmux_target":"demo:1.1","process_start_fingerprint":"pane=%1;pid=1;session_started=1",
           "cwd":"/repo","display_name":null,"lifecycle":"RUNNING","attention":"NONE",
           "current_request_fingerprint":null,"current_request":null,"management":"DEGRADED",
           "transport_health":"HEALTHY","capabilities":{"structured_answer":false,"approvals":false,"send_prompt":false,"continue_turn":false,"retry":false,"interrupt":false,"start":false,"stop":false,"restart":false,"kill":false,"archive":false,"tmux_attach":true,"tmux_text":true,"verified_picker":false},"provenance":"inferred","confidence":"LOW",
           "discovered_at":1,"last_observed_at":2,"lifecycle_updated_at":2,"attention_updated_at":2,
           "version":1,"updated_revision":7}
        ]}
        """
        let snapshot = try JSONDecoder().decode(FleetSnapshot.self, from: Data(json.utf8))
        XCTAssertEqual(snapshot.sessions.count, 1, "the row must survive an unknown provider")
        XCTAssertEqual(snapshot.sessions.first?.provider, .unknown)
        XCTAssertEqual(snapshot.sessions.first?.lifecycle, .running)
    }
}
