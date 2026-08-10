import Foundation
import XCTest
@testable import AINBFleet

/// The macOS chat pane's display contract.
///
/// Reads the SAME fixtures the Rust round-trips and the Swift daemon-contract
/// suite read (`ainb-tui/crates/ainb-hangar-proto/fixtures/chat`), never copies
/// of them: a copy is how two suites agree with each other and disagree with
/// the wire.
///
/// The label tests are the Swift mirror of
/// `every_wire_provider_renders_a_label_operators_can_read`: each one iterates
/// `allCases` and classifies every variant through a wildcard-free `switch`, so
/// adding a variant fails to COMPILE both in `FleetChatLabels` and here rather
/// than silently rendering as something an operator misreads.
final class FleetChatPresentationTests: XCTestCase {
    // MARK: - Attribution

    /// The guarantee the wire's `actor` exists to provide, at the last inch: a
    /// copilot row and an operator row must be TELLABLE APART on screen.
    func testCopilotAndOperatorRowsAreDistinguishable() throws {
        let human = FleetChatMessageRow(message: try Self.message(sender: "operator", body: "restart sess-a"))
        let copilot = FleetChatMessageRow(message: try Self.message(sender: "copilot", body: "restart sess-a"))

        XCTAssertNotEqual(human.actor, copilot.actor)
        XCTAssertNotEqual(human.actor.label, copilot.actor.label)
        XCTAssertNotEqual(human.actor.accessibilityLabel, copilot.actor.accessibilityLabel)
        XCTAssertNotEqual(human.actor.identifier, copilot.actor.identifier)
        // Side is the glanceable half. Only a human's own writing is mine.
        XCTAssertTrue(human.actor.isOperator)
        XCTAssertFalse(copilot.actor.isOperator)
        // Identical BODIES: attribution cannot come from the text, because the
        // text is the one thing a prompt-injected copilot fully controls.
        XCTAssertEqual(human.body, copilot.body)
    }

    /// A blank sender must never read as the operator. The daemon refuses one,
    /// so this is the belt for a wire that changes its mind.
    func testBlankSenderIsUnattributedRatherThanTheOperator() {
        for blank in ["", "   ", "\n"] {
            XCTAssertEqual(FleetChatActor.from(wire: blank), .unattributed, "\(blank.debugDescription) read as somebody")
        }
        XCTAssertFalse(FleetChatActor.unattributed.isOperator)
        XCTAssertEqual(FleetChatActor.from(wire: "  operator "), .operatorHuman)
        XCTAssertEqual(FleetChatActor.from(wire: "tmux:sess-a"), .session("tmux:sess-a"))
    }

    /// Every actor renders a distinct, non-empty label. Exhaustive on purpose.
    func testEveryActorRendersADistinctLabel() {
        let actors: [FleetChatActor] = [.operatorHuman, .copilot, .session("tmux:sess-a"), .unattributed]
        // Wildcard-free: a new actor kind is a compile error here.
        func isNamedHuman(_ actor: FleetChatActor) -> Bool {
            switch actor {
            case .operatorHuman: true
            case .copilot, .session, .unattributed: false
            }
        }
        XCTAssertEqual(actors.filter(isNamedHuman).count, 1, "exactly one actor may read as the human")
        XCTAssertEqual(Set(actors.map(\.label)).count, actors.count, "two actors share a label")
        XCTAssertEqual(Set(actors.map(\.accessibilityLabel)).count, actors.count, "two actors sound alike")
        XCTAssertTrue(actors.allSatisfy { !$0.label.isEmpty })
    }

    // MARK: - Display mappings

    func testEveryMessageKindRendersALabel() {
        assertLabelsAreDistinctAndNamed(FleetMessageKind.allCases, FleetChatLabels.messageKind) { kind in
            switch kind {
            case .user, .agent, .marker: true
            case .unknown: false
            }
        }
    }

    func testEveryConfirmStateRendersALabel() {
        assertLabelsAreDistinctAndNamed(FleetConfirmState.allCases, FleetChatLabels.confirmState) { state in
            switch state {
            case .open, .approved, .denied, .expired: true
            case .unknown: false
            }
        }
    }

    func testEveryActivityClassRendersALabel() {
        assertLabelsAreDistinctAndNamed(FleetActivityClass.allCases, FleetChatLabels.activityClass) { activityClass in
            switch activityClass {
            case .read, .write, .destructive: true
            case .unknown: false
            }
        }
    }

    /// A class this build cannot name is styled LOUD, never quiet. Over-warning
    /// about a future class is recoverable; painting it as a harmless read is
    /// not.
    func testUnknownActivityClassWarnsRatherThanReadsAsHarmless() {
        XCTAssertTrue(FleetChatLabels.activityClassIsLoud(.unknown))
        XCTAssertTrue(FleetChatLabels.activityClassIsLoud(.destructive))
        XCTAssertFalse(FleetChatLabels.activityClassIsLoud(.read))
    }

    func testEveryActivityOutcomeRendersALabel() {
        assertLabelsAreDistinctAndNamed(FleetActivityOutcome.allCases, FleetChatLabels.activityOutcome) { outcome in
            switch outcome {
            case .ok, .denied, .expired, .error: true
            case .unknown: false
            }
        }
    }

    func testEveryChannelKindRendersALabel() {
        assertLabelsAreDistinctAndNamed(FleetChannelKind.allCases, FleetChatLabels.channelKind) { kind in
            switch kind {
            case .copilot, .broadcast: true
            case .unknown: false
            }
        }
    }

    func testEveryCopilotProviderRendersALabel() {
        assertLabelsAreDistinctAndNamed(FleetCopilotProvider.allCases, FleetChatLabels.copilotProvider) { provider in
            switch provider {
            case .claude, .codex: true
            case .unknown: false
            }
        }
    }

    // MARK: - Confirm cards

    /// The shared fixture's two open cards are answerable, and the pane's gate
    /// agrees with the wire type's rather than re-deriving it.
    func testSharedConfirmFixtureRendersAnswerableCards() throws {
        let cards = try Self.confirmCards(fromFixture: "confirm_list_result.json")
        XCTAssertEqual(cards.count, 2)
        XCTAssertTrue(cards.allSatisfy(\.isAnswerable))
        XCTAssertEqual(cards.map(\.stateLabel), ["OPEN", "OPEN"])
        XCTAssertEqual(cards.first?.tool, "kill")
        XCTAssertEqual(cards.first?.id, "01J0CONFIRM")
        // The arguments line is re-encoded, never read: the daemon already
        // projected them onto the tool's declared schema keys.
        XCTAssertEqual(cards.first?.argumentsLine, #"{"session":"tmux:sess-c"}"#)
    }

    /// An expired card from the shared event fixture renders and is refused.
    func testExpiredCardRendersButIsNotAnswerable() throws {
        let event = try FleetWire.decoder().decode(
            FleetConfirmEventParams.self,
            from: try Self.chatFixture(named: "confirm_event.json")
        )
        let card = FleetChatConfirmCard.known(event.confirm)
        XCTAssertEqual(card.stateLabel, "EXPIRED")
        XCTAssertFalse(card.isAnswerable)
        XCTAssertTrue(card.refusal.contains("EXPIRED"), card.refusal)
    }

    /// A state token this build has never heard of decodes to `.unknown` and is
    /// NOT answerable, so the pane cannot offer an approve button for a
    /// lifecycle it does not understand.
    func testStateFromANewerDaemonIsNeverAnswerable() throws {
        let card = FleetChatConfirmCard.decode(try Self.json(#"""
        {"confirm_id":"01J0FUTURE","scope_key":"channel:copilot","tool":"future_tool",
         "arguments":{},"state":"quarantined","created_at":1,"expires_at":2}
        """#))
        guard case let .known(confirm) = card else { return XCTFail("expected a decoded card") }
        XCTAssertEqual(confirm.state, .unknown)
        XCTAssertFalse(card.isAnswerable)
        XCTAssertEqual(card.stateLabel, "UNRECOGNISED")
    }

    /// One undecodable row must not cost the operator the cards this build DOES
    /// understand. Row-by-row decoding is the whole reason `confirmList`
    /// returns raw rows.
    func testOneUndecodableRowDoesNotBlankThePage() throws {
        let raw = try FleetWire.decoder().decode(FleetConfirmListRawResult.self, from: Data(#"""
        {"confirms":[
          {"confirm_id":"01J0GOOD","scope_key":"channel:copilot","tool":"kill",
           "arguments":{},"state":"open","created_at":1,"expires_at":2},
          {"confirm_id":"01J0BAD","tool":"spawn_session","created_at":"not-a-number"}
        ]}
        """#.utf8))
        let cards = raw.confirms.map(FleetChatConfirmCard.decode)

        XCTAssertEqual(cards.count, 2)
        XCTAssertTrue(cards[0].isAnswerable, "a good card was lost to a bad neighbour")
        XCTAssertFalse(cards[1].isAnswerable)
        XCTAssertEqual(cards[1].id, "01J0BAD", "identity is salvaged so the row is reportable")
        XCTAssertEqual(cards[1].tool, "spawn_session")
        XCTAssertEqual(cards[1].stateLabel, "UNRECOGNISED")
        XCTAssertTrue(cards[1].refusal.contains("not answerable"), cards[1].refusal)
        XCTAssertEqual(cards[1].argumentsLine, "", "an undecoded card must not claim arguments it never read")
    }

    /// A row with no readable identity at all is still rendered rather than
    /// dropped: an absence is unreportable, an "unknown" row is not.
    func testARowWithNoReadableIdentityStillRenders() throws {
        let card = FleetChatConfirmCard.decode(try Self.json(#"{"nothing":"useful"}"#))
        XCTAssertEqual(card.id, "unknown")
        XCTAssertEqual(card.tool, "unknown")
        XCTAssertFalse(card.isAnswerable)
    }

    // MARK: - Activity

    func testSharedActivityFixtureRendersEveryRow() throws {
        let result = try FleetWire.decoder().decode(
            FleetActivityListResult.self,
            from: try Self.chatFixture(named: "activity_list_result.json")
        )
        XCTAssertEqual(result.activities.map { FleetChatLabels.activityClass($0.activityClass) }, ["WRITE", "DESTRUCTIVE"])
        XCTAssertEqual(result.activities.map { FleetChatLabels.activityOutcome($0.outcome) }, ["ok", "expired"])
        XCTAssertTrue(result.activities.map(\.activityClass).contains { FleetChatLabels.activityClassIsLoud($0) })
    }

    // MARK: - Send params

    /// The client cannot file a message under anybody else's name, because
    /// `FleetMessageSendParams` has no `actor` to set. Asserted on the ENCODED
    /// frame: a field that exists but is never populated is one refactor away
    /// from being populated.
    func testOperatorSendCarriesNoActorKey() throws {
        let encoded = try FleetWire.encoder().encode(FleetMessageSendParams(
            scopeKey: "channel:copilot",
            targets: ["acp:01J0COPILOT"],
            originMessageID: nil,
            text: "status?",
            requestID: "req-1"
        ))
        let object = try XCTUnwrap(try JSONSerialization.jsonObject(with: encoded) as? [String: Any])

        XCTAssertNil(object["actor"], "an operator surface must never name the sender")
        XCTAssertNil(object["origin_message_id"], "an absent optional is omitted, not sent as null")
        XCTAssertEqual(object["scope_key"] as? String, "channel:copilot")
        XCTAssertEqual(object["targets"] as? [String], ["acp:01J0COPILOT"])
        XCTAssertEqual(object["request_id"] as? String, "req-1")
    }

    // MARK: - Send receipts

    /// The daemon's REASON survives the wire onto this client.
    ///
    /// Decoded from the `message_send` result shape rather than built in
    /// Swift: a `detail` the type declares but the CodingKeys never read would
    /// pass an initialiser-based test and still print REJECTED with no reason,
    /// which is the one thing the operator needs to decide whether to retry.
    func testDeliveryDetailSurvivesTheWire() throws {
        let result = try FleetWire.decoder().decode(FleetMessageSendResult.self, from: Data("""
        {"message_id":"01J0MSG","deliveries":[
          {"session_key":"claude:gone","state":"REJECTED","detail":"target_not_running"}]}
        """.utf8))

        let leg = try XCTUnwrap(result.deliveries.first)
        XCTAssertEqual(leg.detail, "target_not_running", "the reason must cross the socket")
        XCTAssertTrue(
            FleetChatLabels.receiptLine(leg).contains("target_not_running"),
            "a receipt that cannot say WHY is one an operator can only stare at"
        )
    }

    /// A leg with no reason decodes too: the daemon omits the key rather than
    /// sending null, and an older daemon never sends it at all.
    func testADeliveryWithNoReasonStillDecodes() throws {
        let result = try FleetWire.decoder().decode(FleetMessageSendResult.self, from: Data("""
        {"message_id":"01J0MSG","deliveries":[{"session_key":"claude:one","state":"DELIVERED"}]}
        """.utf8))

        XCTAssertNil(try XCTUnwrap(result.deliveries.first).detail)
    }

    /// The hunt-2 failure in its worst form: a 1-of-1 fan-out where 0
    /// delivered must NOT read as success. Asserted on the decoded result, not
    /// on a view, so it fails whatever the pane happens to look like.
    func testALoneRejectedLegNeverReadsAsSent() throws {
        let result = try FleetWire.decoder().decode(FleetMessageSendResult.self, from: Data("""
        {"message_id":"01J0MSG","deliveries":[
          {"session_key":"claude:gone","state":"REJECTED","detail":"target_not_running"}]}
        """.utf8))

        let summary = FleetChatLabels.deliverySummary(result.deliveries)
        XCTAssertTrue(summary.contains("delivered to 0/1"), summary)
        XCTAssertTrue(summary.contains("1 not delivered"), summary)
        XCTAssertTrue(summary.contains("claude:gone"), summary)
        XCTAssertTrue(summary.contains("target_not_running"), summary)
    }

    /// The vocabulary is the TUI's, word for word
    /// (`ChatState::apply_receipts`), so an operator watching both surfaces
    /// reads one sentence rather than two dialects of it.
    func testAFullyDeliveredSendReadsLikeTheTUI() {
        XCTAssertEqual(
            FleetChatLabels.deliverySummary([
                FleetMessageDelivery(sessionKey: "claude:one", state: .delivered, detail: nil),
                FleetMessageDelivery(sessionKey: "codex:two", state: .delivered, detail: nil),
            ]),
            "delivered to 2/2"
        )
    }

    /// Every receipt state renders a distinct word, and only `DELIVERED` reads
    /// as success. `UNKNOWN` counts as NOT delivered on purpose: at-most-once
    /// delivery means an unknown leg may never have arrived, and counting it as
    /// sent is the single lie this summary exists to prevent. Adding a status
    /// fails to COMPILE in `deliveryState` and in the classifier below.
    func testEveryReceiptStateRendersALabelAndOnlyDeliveredIsSuccess() {
        let states = ActionReceiptStatus.allCases
        XCTAssertEqual(Set(states.map(FleetChatLabels.deliveryState)).count, states.count)
        for state in states {
            let word = FleetChatLabels.deliveryState(state)
            XCTAssertEqual(word, state.rawValue, "the label must be the daemon's own token")
            let readsAsSuccess: Bool = switch state {
            case .delivered: true
            case .pending, .failed, .unknown, .rejected: false
            }
            let summary = FleetChatLabels.deliverySummary([
                FleetMessageDelivery(sessionKey: "claude:one", state: state, detail: nil)
            ])
            XCTAssertEqual(
                summary == "delivered to 1/1",
                readsAsSuccess,
                "\(word) is counted wrongly: \(summary)"
            )
        }
    }

    /// The page size is the daemon's own maximum. A client that pages
    /// differently from the TUI shows a different conversation for the same
    /// scope, which reads as message loss.
    func testChatPagesAtTheDaemonMaximums() {
        XCTAssertEqual(fleetMessageListMax, 100, "FLEET_MESSAGE_LIST_MAX")
        XCTAssertEqual(fleetActivityListMax, 200, "FLEET_ACTIVITY_LIST_MAX")
    }

    /// The adapter token the copilot scope is bound to. The daemon binds a
    /// scope to whatever the FIRST `fleet/acp_session_create` names, so a
    /// mismatch with the TUI means whoever opened the chat first wins and the
    /// other client is refused forever.
    func testCopilotProviderMatchesTheTUI() {
        XCTAssertEqual(copilotDefaultProvider, "claude-agent-acp")
    }

    // MARK: - Helpers

    private func assertLabelsAreDistinctAndNamed<Value: Hashable>(
        _ values: [Value],
        _ label: (Value) -> String,
        _ operatorsShouldRecognise: (Value) -> Bool,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        XCTAssertFalse(values.isEmpty, "no variants to check", file: file, line: line)
        XCTAssertEqual(Set(values.map(label)).count, values.count, "two variants share a label", file: file, line: line)
        for value in values {
            XCTAssertFalse(label(value).trimmingCharacters(in: .whitespaces).isEmpty, "\(value) renders blank", file: file, line: line)
            if !operatorsShouldRecognise(value) {
                XCTAssertTrue(
                    label(value).lowercased().contains("unrecognised"),
                    "\(value) is a fallback but does not say so, so it reads like a real value",
                    file: file,
                    line: line
                )
            }
        }
        XCTAssertEqual(values.filter { !operatorsShouldRecognise($0) }.count, 1, "expected exactly one fallback variant", file: file, line: line)
    }

    /// A `FleetMessage` built from JSON in the shape the Rust struct defines
    /// (`ainb-hangar-proto::fleet::FleetMessage`). There is no shared fixture
    /// for a chat MESSAGE: part 2 ships fixtures for the channel, confirm and
    /// activity frames only, and this suite may not add files under
    /// `ainb-tui/`. Built from JSON rather than a Swift initialiser anyway, so
    /// a renamed wire key fails here instead of being renamed on both sides.
    private static func message(sender: String, body: String) throws -> FleetMessage {
        try FleetWire.decoder().decode(FleetMessage.self, from: Data("""
        {"id":"01J0MSG","scope_key":"channel:copilot","sender":"\(sender)",
         "kind":"user","body":"\(body)","created_at":1700000000000}
        """.utf8))
    }

    private static func json(_ raw: String) throws -> JSONValue {
        try FleetWire.decoder().decode(JSONValue.self, from: Data(raw.utf8))
    }

    private static func confirmCards(fromFixture name: String) throws -> [FleetChatConfirmCard] {
        try FleetWire.decoder()
            .decode(FleetConfirmListRawResult.self, from: try chatFixture(named: name))
            .confirms
            .map(FleetChatConfirmCard.decode)
    }

    private static func chatFixture(named name: String) throws -> Data {
        var repository = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { repository.deleteLastPathComponent() }
        return try Data(contentsOf: repository
            .appendingPathComponent("ainb-tui/crates/ainb-hangar-proto/fixtures/chat")
            .appendingPathComponent(name))
    }
}
