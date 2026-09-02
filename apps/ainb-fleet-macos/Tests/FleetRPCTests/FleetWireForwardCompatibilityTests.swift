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

    /// Usage state gates the whole usage/dashboard panel. An unknown state must
    /// degrade to `.unavailable` (which renders an explanatory empty state), not
    /// throw -- and not decode as `.ready`, which would promise data that is not
    /// there.
    func testUnknownUsageStateDegradesInsteadOfBlankingThePanel() throws {
        XCTAssertEqual(try decode(FleetUsageSummaryState.self, "degraded"), .unavailable)
        XCTAssertEqual(try decode(FleetUsageSummaryState.self, "ready"), .ready)
        XCTAssertEqual(try decode(FleetUsageSummaryState.self, "scanning"), .scanning)
    }

    /// The regression that matters for the dashboard: one unknown state value
    /// must not fail the decode of the entire result.
    func testUnknownUsageStateDoesNotFailTheWholeDashboard() throws {
        let json = #"{"state":"recalibrating","cost_complete":false,"weekly":[],"heatmap":[],"providers":[],"models":[],"projects":[],"sessions":[],"branches":[],"tools":[],"mcp_servers":[],"shell_commands":[],"detail":"daemon is newer than this build"}"#
        let result = try JSONDecoder().decode(FleetUsageDashboardResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.state, .unavailable)
        XCTAssertEqual(result.detail, "daemon is newer than this build")
    }

    func testKnownValuesStillRoundTripThroughEncoding() throws {
        let encoded = try JSONEncoder().encode(FleetProvider.codex)
        XCTAssertEqual(String(decoding: encoded, as: UTF8.self), "\"codex\"")
    }

    func testAntigravityProviderDecodesAndRoundTrips() throws {
        XCTAssertEqual(try decode(FleetProvider.self, "antigravity"), .antigravity)
        let encoded = try JSONEncoder().encode(FleetProvider.antigravity)
        XCTAssertEqual(String(decoding: encoded, as: UTF8.self), "\"antigravity\"")
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

    /// An older daemon omits the model keys entirely. They must land as nil, not
    /// as "", so the roster can tell "never observed" from "observed as blank".
    func testSessionDecodesWithoutModelKeys() throws {
        let session = try JSONDecoder().decode(FleetSession.self, from: Data(Self.sessionJSON().utf8))
        XCTAssertNil(session.model)
        XCTAssertNil(session.reasoningEffort)
        XCTAssertNil(session.modelUpdatedAt)
    }

    /// The three keys are snake_case on the wire. This also pins that they are
    /// declared without an initialiser: a `let model: String? = nil` compiles,
    /// is skipped by the synthesized decoder, and would make this fail with nil.
    func testSessionDecodesModelKeys() throws {
        let json = Self.sessionJSON(
            extra: #""model":"claude-opus-5","reasoning_effort":"xhigh","model_updated_at":1700000000000,"#
        )
        let session = try JSONDecoder().decode(FleetSession.self, from: Data(json.utf8))
        XCTAssertEqual(session.model, "claude-opus-5")
        XCTAssertEqual(session.reasoningEffort, "xhigh")
        XCTAssertEqual(session.modelUpdatedAt, 1_700_000_000_000)
    }

    /// Absent optionals must not re-encode as explicit nulls, or a full-session
    /// sample added to `CanonicalFixtureTests` would fail its round-trip gate.
    func testSessionOmitsAbsentModelKeysWhenEncoded() throws {
        let session = try JSONDecoder().decode(FleetSession.self, from: Data(Self.sessionJSON().utf8))
        let encoded = String(decoding: try FleetWire.encoder().encode(session), as: UTF8.self)
        XCTAssertFalse(encoded.contains("model"), "an absent model must not be re-encoded at all")
        XCTAssertFalse(encoded.contains("reasoning_effort"))
    }

    /// End to end for the part the client owns: a daemon snapshot carrying the
    /// snake_case keys must come out the other side as the chip the operator
    /// reads. A key renamed on either side fails here rather than rendering a
    /// permanently empty chip that looks like a session with no model.
    func testSnapshotModelKeysReachTheRosterChip() throws {
        let json = """
        {"head_revision":9,"sessions":[\(Self.sessionJSON(
            extra: #""model":"claude-opus-5","reasoning_effort":"xhigh","model_updated_at":1700000000000,"#
        ))]}
        """
        let snapshot = try FleetWire.decoder().decode(FleetSnapshot.self, from: Data(json.utf8))
        let session = try XCTUnwrap(snapshot.sessions.first)
        XCTAssertEqual(FleetRosterPresentation.modelLabel(for: session), "opus-5 · xhigh")
        XCTAssertTrue(FleetRosterPresentation.matches(session, search: "opus", filters: .all))
    }

    private static func sessionJSON(extra: String = "") -> String {
        """
        {"session_key":"claude:s1","provider":"claude","provider_session_id":null,
         "tmux_target":null,"process_start_fingerprint":null,
         "cwd":"/repo","display_name":null,"lifecycle":"RUNNING","attention":"NONE",
         "current_request_fingerprint":null,"current_request":null,"management":"MANAGED",
         "transport_health":"HEALTHY","capabilities":{"structured_answer":false,"approvals":false,"send_prompt":false,"continue_turn":false,"retry":false,"interrupt":false,"start":false,"stop":false,"restart":false,"kill":false,"archive":false,"tmux_attach":true,"tmux_text":true,"verified_picker":false},"provenance":"authoritative","confidence":"HIGH",
         \(extra)"discovered_at":1,"last_observed_at":2,"lifecycle_updated_at":2,"attention_updated_at":2,
         "version":1,"updated_revision":7}
        """
    }
}
