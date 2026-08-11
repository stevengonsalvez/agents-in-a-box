import Foundation

// The macOS half of the Fleet chat surface, in pure display terms so the tests
// can hold it without a window. The TUI solved the same problems first
// (`ainb-plugin-hangar/src/screen/fleet_chat.rs`) and this MATCHES ITS
// SEMANTICS even though the idiom differs: same attribution vocabulary, same
// answerability gate, same "a card this build cannot name is never offered an
// approve button".
//
// Every `switch` below is exhaustive with NO `default`. That is deliberate and
// it is the whole point of the file: a new wire variant must fail to COMPILE
// here rather than fall through to a label that reads like something else. The
// bug that rule exists for is real -- an ACP session rendered as UNKNOWN in the
// Fleet panel because the provider was mapped twice and only one half knew the
// token. `default` is how that happens quietly.

/// Who wrote one chat row.
///
/// Derived from `FleetMessage.sender`, which the daemon records from the send's
/// `actor` and never from the body. The wire carries it precisely so a copilot
/// write cannot masquerade as a human's, and that guarantee dies at the last
/// inch if the pane renders both the same, so the label is part of the
/// contract, not decoration.
enum FleetChatActor: Equatable {
    /// A human at a client: `sender == "operator"`.
    case operatorHuman
    /// The fleet copilot writing through its MCP tools: `sender == "copilot"`.
    case copilot
    /// An agent session replying in its own name: `sender` is a session key.
    case session(String)
    /// A row whose sender is blank.
    ///
    /// The daemon refuses a blank actor, so this should be unreachable over a
    /// current wire. It exists because the alternative default is "operator",
    /// and a row that cannot say who wrote it must NEVER claim a human did.
    case unattributed

    /// Map a wire `sender` token onto the actor the pane paints.
    static func from(wire sender: String) -> Self {
        switch sender.trimmingCharacters(in: .whitespacesAndNewlines) {
        case "": .unattributed
        case "operator": .operatorHuman
        case "copilot": .copilot
        case let other: .session(other)
        }
    }

    /// The on-screen attribution label. Matches the TUI's vocabulary exactly:
    /// an operator reading both surfaces must not have to learn two words for
    /// the same fact.
    var label: String {
        switch self {
        case .operatorHuman: "YOU"
        case .copilot: "COPILOT"
        case let .session(key): key
        case .unattributed: "UNATTRIBUTED"
        }
    }

    /// Spoken attribution. VoiceOver users get the SAME distinction sighted
    /// ones do: a pane that only separates the two by colour or alignment has
    /// not actually attributed anything.
    var accessibilityLabel: String {
        switch self {
        case .operatorHuman: "From you"
        case .copilot: "From the fleet copilot"
        case let .session(key): "From session \(key)"
        case .unattributed: "Sender unknown"
        }
    }

    /// Whether the row is drawn on the operator's side of the timeline.
    ///
    /// Only a human's own writing is; a copilot row is never mine. Side alone
    /// is not the attribution (the label is), it is the second, glanceable
    /// signal that carries when a row scrolls past at speed.
    var isOperator: Bool {
        switch self {
        case .operatorHuman: true
        case .copilot, .session, .unattributed: false
        }
    }

    /// A stable identifier for the accessibility tree and the UI tests, so a
    /// journey can assert "this row is attributed to the copilot" rather than
    /// matching a substring that could appear anywhere in the pane.
    var identifier: String {
        switch self {
        case .operatorHuman: "fleet.chat.actor.operator"
        case .copilot: "fleet.chat.actor.copilot"
        case .session: "fleet.chat.actor.session"
        case .unattributed: "fleet.chat.actor.unattributed"
        }
    }
}

/// One timeline row, already attributed.
struct FleetChatMessageRow: Equatable, Identifiable {
    let id: String
    let actor: FleetChatActor
    let kind: FleetMessageKind
    let body: String
    /// Whether this row answers another one. `origin_message_id` is the only
    /// thing on the wire that says so.
    let isReply: Bool
    let createdAt: Int64

    init(message: FleetMessage) {
        id = message.id
        actor = .from(wire: message.sender)
        kind = message.kind
        body = message.body
        isReply = message.originMessageID != nil
        createdAt = message.createdAt
    }
}

/// One confirm card as this build understands it.
///
/// The list is decoded ROW BY ROW from raw JSON. A typed array would fail the
/// whole frame on one undecodable row and the operator would lose the cards
/// this build does understand along with the one it does not.
enum FleetChatConfirmCard: Equatable, Identifiable {
    /// A card this build decoded completely.
    case known(FleetConfirm)
    /// A card carrying something this build cannot decode. Rendered, never
    /// answerable.
    case unrecognised(confirmID: String, tool: String, detail: String)

    /// Decode one confirm frame, tolerantly.
    static func decode(_ value: JSONValue) -> Self {
        do {
            let data = try FleetWire.encoder().encode(value)
            return .known(try FleetWire.decoder().decode(FleetConfirm.self, from: data))
        } catch {
            return .unrecognised(
                confirmID: value.value("confirm_id")?.stringValue ?? "unknown",
                tool: value.value("tool")?.stringValue ?? "unknown",
                detail: String(describing: error)
            )
        }
    }

    var id: String {
        switch self {
        case let .known(confirm): confirm.confirmID
        case let .unrecognised(confirmID, _, _): confirmID
        }
    }

    /// The tool the copilot asked to run.
    var tool: String {
        switch self {
        case let .known(confirm): confirm.tool
        case let .unrecognised(_, tool, _): tool
        }
    }

    /// The state label the row shows.
    var stateLabel: String {
        switch self {
        case let .known(confirm): FleetChatLabels.confirmState(confirm.state)
        case .unrecognised: "UNRECOGNISED"
        }
    }

    /// Whether this row may be approved, denied, or edited.
    ///
    /// Delegates to `FleetConfirm.isAnswerable` rather than re-deriving the
    /// rule. That gate existing in ONE place is the reason an unknown state
    /// cannot be approved blind: a second copy here would be a second thing to
    /// remember to update, and the state that got missed would be the one this
    /// build could not name.
    var isAnswerable: Bool {
        switch self {
        case let .known(confirm): confirm.isAnswerable
        case .unrecognised: false
        }
    }

    /// Why the row is not answerable, for the operator and the bug report.
    var refusal: String {
        switch self {
        case let .known(confirm):
            "\(confirm.confirmID) is \(FleetChatLabels.confirmState(confirm.state)), not answerable"
        case let .unrecognised(_, _, detail):
            "card not understood by this build, not answerable: \(detail)"
        }
    }

    /// The tool arguments, already projected by the daemon.
    ///
    /// The daemon projects a card's arguments down to its tool's declared
    /// schema keys before persisting, so model-authored prose never reaches
    /// this line. The pane renders what it is given and adds nothing: it does
    /// not read the arguments, it re-encodes them.
    var argumentsLine: String {
        switch self {
        case let .known(confirm):
            (try? FleetWire.encoder().encode(confirm.arguments)).map { String(decoding: $0, as: UTF8.self) } ?? ""
        case .unrecognised:
            ""
        }
    }
}

/// Every display mapping for a part-2 wire enum, in one place.
///
/// One place on purpose: the ACP-provider bug happened because a provider was
/// mapped TWICE and only one half learned the new token. A single mapping
/// cannot disagree with itself.
enum FleetChatLabels {
    static func messageKind(_ kind: FleetMessageKind) -> String {
        switch kind {
        case .user: "prompt"
        case .agent: "reply"
        case .marker: "marker"
        case .unknown: "unrecognised kind"
        }
    }

    static func confirmState(_ state: FleetConfirmState) -> String {
        switch state {
        case .open: "OPEN"
        case .approved: "APPROVED"
        case .denied: "DENIED"
        case .expired: "EXPIRED"
        case .unknown: "UNRECOGNISED"
        }
    }

    static func activityClass(_ activityClass: FleetActivityClass) -> String {
        switch activityClass {
        case .read: "READ"
        case .write: "WRITE"
        case .destructive: "DESTRUCTIVE"
        case .unknown: "UNRECOGNISED"
        }
    }

    /// Whether a class is drawn with the loud styling.
    ///
    /// `.unknown` is loud on purpose: over-warning about a class from a newer
    /// daemon is recoverable, silently painting it as a harmless read is not.
    static func activityClassIsLoud(_ activityClass: FleetActivityClass) -> Bool {
        switch activityClass {
        case .destructive, .unknown: true
        case .read, .write: false
        }
    }

    static func activityOutcome(_ outcome: FleetActivityOutcome) -> String {
        switch outcome {
        case .ok: "ok"
        case .denied: "denied"
        case .expired: "expired"
        case .error: "error"
        case .unknown: "unrecognised"
        }
    }

    static func channelKind(_ kind: FleetChannelKind) -> String {
        switch kind {
        case .copilot: "Copilot"
        case .broadcast: "Broadcast"
        case .unknown: "Unrecognised channel"
        }
    }

    static func copilotProvider(_ provider: FleetCopilotProvider) -> String {
        switch provider {
        case .claude: "Claude"
        case .codex: "Codex"
        case .unknown: "Unrecognised provider"
        }
    }

    /// The state word for one delivery leg.
    ///
    /// The daemon's own token, verbatim, because the TUI pane
    /// (`fleet_chat::delivery_state_label`) and the CLI (`msg::render_delivery`)
    /// print exactly these words: an operator watching two surfaces must read
    /// ONE vocabulary. Wildcard-free, so a new status is a compile error here.
    static func deliveryState(_ state: ActionReceiptStatus) -> String {
        switch state {
        case .pending: "PENDING"
        case .delivered: "DELIVERED"
        case .failed: "FAILED"
        case .unknown: "UNKNOWN"
        case .rejected: "REJECTED"
        }
    }

    /// One leg as one line: state, recipient, and the daemon's REASON when it
    /// has one. Mirrors the TUI's `receipt_line`.
    static func receiptLine(_ delivery: FleetMessageDelivery) -> String {
        let state = deliveryState(delivery.state)
        let detail = delivery.detail?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return detail.isEmpty
            ? "\(state) \(delivery.sessionKey)"
            : "\(state) \(delivery.sessionKey) · \(detail)"
    }

    /// What a send actually did, as one operator-facing line.
    ///
    /// A send whose only leg came back REJECTED still answers 200 on the RPC:
    /// the daemon reports per-leg honesty in the RESULT, not in the transport.
    /// A client that discarded the result would clear the composer and show the
    /// operator's own message in the timeline with no notice at all, i.e. a
    /// 1-of-1 fan-out where 0 delivered reading as complete success. The
    /// counts and the words are the TUI's (`ChatState::apply_receipts` plus
    /// `receipt_line`), so the two surfaces cannot disagree about what happened.
    static func deliverySummary(_ deliveries: [FleetMessageDelivery]) -> String {
        let total = deliveries.count
        let undelivered = deliveries.filter { $0.state != .delivered }
        let delivered = total - undelivered.count
        var line = "delivered to \(delivered)/\(total)"
        if !undelivered.isEmpty {
            line += " · \(undelivered.count) not delivered"
            // The reason, per refused leg: "REJECTED" alone does not say
            // whether to retry or to go and look at the session.
            line += " · " + undelivered.map(receiptLine).joined(separator: " · ")
        }
        return line
    }
}

/// Everything one page of the copilot conversation put on screen.
///
/// A value type so the store can replace it atomically: a half-applied refresh
/// that shows this poll's cards next to the last poll's timeline is a pane that
/// lies about what the copilot just did.
struct FleetChatSurface: Equatable {
    /// The channel scope being paged, once resolved.
    var scopeKey: String? = nil
    /// The copilot session messages are delivered to. Without it there is
    /// nobody to send to, and the composer says so rather than posting into a
    /// conversation no agent answers on.
    var targetSessionKey: String? = nil
    var messages: [FleetChatMessageRow] = []
    var confirms: [FleetChatConfirmCard] = []
    var activity: [FleetActivityRow] = []
    /// Why the confirm feed is empty, when it is empty for a REASON. A daemon
    /// built between phases answers -32601 here, and a pane that renders that
    /// as "no cards open" is telling the operator there is nothing to approve
    /// when it simply cannot see.
    var confirmsDetail: String? = nil
    /// The daemon's own refusal wording when the copilot session could not be
    /// resolved. Kept verbatim: it is the only actionable thing an operator
    /// gets, and it is usually "this scope is already held by a session whose
    /// cwd is X, not Y".
    var sessionDetail: String? = nil
}

/// Seconds between poll refreshes while the chat surface is open.
///
/// The same interval the TUI polls at (`CHAT_POLL_INTERVAL_MS`), for the same
/// reason: one timer beats a second long-lived socket in the render path, and
/// the daemon's `fleet/confirm_event` and `fleet/activity_event` stream is the
/// upgrade for when a poll's latency is actually felt. Matching the TUI matters
/// because an operator watching both surfaces must not see one lag the other.
///
/// ponytail: polling, not a subscription. Upgrade when a second is visible.
let fleetChatPollInterval = Duration.seconds(1)
