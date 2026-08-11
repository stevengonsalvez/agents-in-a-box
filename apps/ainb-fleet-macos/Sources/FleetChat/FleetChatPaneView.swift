import SwiftUI

/// The Fleet copilot conversation: timeline, composer, confirm cards, activity.
///
/// Attribution is the load-bearing part of this view. A copilot row and an
/// operator row are separated FOUR ways -- a named label, a colour, a side, and
/// an accessibility label -- because the wire carries `sender` precisely so a
/// copilot write cannot masquerade as a human's, and that guarantee dies at the
/// last inch if the pane renders both the same. Colour alone would not survive
/// VoiceOver; side alone would not survive a screenshot; the LABEL is the one
/// that always reads, and the rest make it glanceable.
struct FleetChatPaneView: View {
    @ObservedObject var store: FleetStore
    @Environment(\.dismiss) private var dismiss
    @State private var composer = ""
    @State private var editingCard: FleetChatConfirmCard?
    @State private var editedArguments = ""

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().overlay(FleetChatPalette.separator)
            if store.canReadChat {
                HSplitView {
                    conversation.frame(minWidth: 380)
                    sidebar.frame(minWidth: 280, idealWidth: 320)
                }
            } else {
                unavailable
            }
        }
        .frame(minWidth: 760, minHeight: 520)
        .background(FleetChatPalette.canvas)
        // Bound to the pane's lifetime: the loop is cancelled when the sheet
        // closes, so a closed chat costs the daemon nothing.
        .task {
            while !Task.isCancelled {
                await store.refreshChatOnce()
                try? await Task.sleep(for: fleetChatPollInterval)
            }
        }
        .sheet(item: $editingCard) { card in
            confirmEditor(card)
        }
    }

    private var header: some View {
        HStack(spacing: 10) {
            Text("Copilot chat")
                .font(.title3.weight(.bold))
            if let scope = store.chat.scopeKey {
                Text(scope)
                    .font(.caption.monospaced())
                    .foregroundStyle(FleetChatPalette.muted)
                    .accessibilityIdentifier("fleet.chat.scope")
            }
            Spacer(minLength: 0)
            Button("Refresh") { store.refreshChat() }
                .disabled(!store.canReadChat)
                .accessibilityIdentifier("fleet.chat.refresh")
            // Explicit, not Esc-only: the composer holds focus, and a sheet
            // whose only exit is a key the focused control might eat is a trap.
            Button("Close") { dismiss() }
                .accessibilityIdentifier("fleet.chat.close")
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 12)
    }

    private var unavailable: some View {
        VStack(spacing: 8) {
            Text("This daemon does not serve Fleet chat.")
                .font(.headline)
            Text("The copilot conversation needs fleet.chat.read and fleet.message.read.")
                .font(.callout)
                .foregroundStyle(FleetChatPalette.muted)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityIdentifier("fleet.chat.unavailable")
    }

    private var conversation: some View {
        VStack(spacing: 0) {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 8) {
                        if store.chat.messages.isEmpty {
                            Text("No messages yet. Ask the copilot below.")
                                .font(.callout)
                                .foregroundStyle(FleetChatPalette.muted)
                                .padding(.top, 24)
                                .accessibilityIdentifier("fleet.chat.timeline.empty")
                        }
                        ForEach(store.chat.messages) { row in
                            FleetChatMessageView(row: row).id(row.id)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(16)
                }
                .onChange(of: store.chat.messages.last?.id) { _, newest in
                    guard let newest else { return }
                    proxy.scrollTo(newest, anchor: .bottom)
                }
            }
            Divider().overlay(FleetChatPalette.separator)
            composerBar
        }
    }

    private var composerBar: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let detail = store.chat.sessionDetail {
                // The daemon's own refusal wording, verbatim. Paraphrasing it
                // costs the operator the only sentence that names the fix.
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(FleetChatPalette.amber)
                    .accessibilityIdentifier("fleet.chat.session-detail")
            }
            if let notice = store.controlNotice {
                Text(notice)
                    .font(.caption)
                    .foregroundStyle(FleetChatPalette.muted)
                    .accessibilityIdentifier("fleet.chat.notice")
            }
            HStack(spacing: 8) {
                TextField("Message the copilot", text: $composer, axis: .vertical)
                    .textFieldStyle(.roundedBorder)
                    .lineLimit(1...4)
                    .accessibilityIdentifier("fleet.chat.composer")
                    .onSubmit(send)
                Button("Send", action: send)
                    .keyboardShortcut(.return, modifiers: .command)
                    .disabled(!canSend)
                    .accessibilityIdentifier("fleet.chat.send")
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
    }

    private var sidebar: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                confirmSection
                activitySection
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(16)
        }
        .background(FleetChatPalette.sidebar)
    }

    private var confirmSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Confirm cards")
                .font(.caption.weight(.bold))
                .foregroundStyle(FleetChatPalette.muted)
            // An empty feed and an UNREADABLE feed are different facts. Saying
            // "none open" when the daemon refused the call tells the operator
            // there is nothing to approve while cards pile up unseen.
            if let detail = store.chat.confirmsDetail {
                Text("Cards unavailable: \(detail)")
                    .font(.caption)
                    .foregroundStyle(FleetChatPalette.amber)
                    .accessibilityIdentifier("fleet.chat.confirms.detail")
            } else if store.chat.confirms.isEmpty {
                Text("None open")
                    .font(.callout)
                    .foregroundStyle(FleetChatPalette.muted)
                    .accessibilityIdentifier("fleet.chat.confirms.empty")
            }
            ForEach(store.chat.confirms) { card in
                confirmRow(card)
            }
        }
    }

    private func confirmRow(_ card: FleetChatConfirmCard) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Text(card.stateLabel)
                    .font(.caption2.weight(.bold))
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(card.isAnswerable ? FleetChatPalette.amber.opacity(0.22) : FleetChatPalette.control, in: Capsule())
                Text(card.tool)
                    .font(.callout.weight(.semibold))
                Spacer(minLength: 0)
            }
            if !card.argumentsLine.isEmpty {
                Text(card.argumentsLine)
                    .font(.caption.monospaced())
                    .foregroundStyle(FleetChatPalette.muted)
                    .lineLimit(3)
            }
            if card.isAnswerable {
                HStack(spacing: 6) {
                    Button("Approve") { store.answerConfirm(card, answer: .approve) }
                        .accessibilityIdentifier("fleet.chat.confirm.approve")
                    Button("Deny") { store.answerConfirm(card, answer: .deny) }
                        .accessibilityIdentifier("fleet.chat.confirm.deny")
                    Button("Edit") {
                        editedArguments = card.argumentsLine
                        editingCard = card
                    }
                    .accessibilityIdentifier("fleet.chat.confirm.edit")
                }
                .controlSize(.small)
                .disabled(!store.canAnswerConfirms)
            } else {
                // No control at all, not a disabled one: a greyed-out Approve
                // next to a state this build cannot name still reads as "there
                // is an approve for this", and `isAnswerable` said there is not.
                Text(card.refusal)
                    .font(.caption)
                    .foregroundStyle(FleetChatPalette.muted)
                    .accessibilityIdentifier("fleet.chat.confirm.not-answerable")
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(FleetChatPalette.control, in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("fleet.chat.confirm.\(card.id)")
    }

    @ViewBuilder private var activitySection: some View {
        if !store.chat.activity.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                Text("Copilot activity")
                    .font(.caption.weight(.bold))
                    .foregroundStyle(FleetChatPalette.muted)
                ForEach(store.chat.activity, id: \.seq) { row in
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text(FleetChatLabels.activityClass(row.activityClass))
                            .font(.caption2.weight(.bold))
                            .foregroundStyle(
                                FleetChatLabels.activityClassIsLoud(row.activityClass)
                                    ? FleetChatPalette.coral
                                    : FleetChatPalette.muted
                            )
                        Text(row.tool).font(.caption)
                        Text(FleetChatLabels.activityOutcome(row.outcome))
                            .font(.caption)
                            .foregroundStyle(FleetChatPalette.muted)
                        Spacer(minLength: 0)
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityIdentifier("fleet.chat.activity.\(row.seq)")
                }
            }
        }
    }

    private func confirmEditor(_ card: FleetChatConfirmCard) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Edit arguments for \(card.tool)")
                .font(.headline)
            TextEditor(text: $editedArguments)
                .font(.callout.monospaced())
                .frame(minWidth: 420, minHeight: 160)
                .accessibilityIdentifier("fleet.chat.confirm.edit.arguments")
            HStack {
                Spacer()
                Button("Cancel") { editingCard = nil }
                Button("Answer") {
                    submitEdit(card)
                }
                .keyboardShortcut(.defaultAction)
                .disabled(editedJSON == nil)
                .accessibilityIdentifier("fleet.chat.confirm.edit.submit")
            }
            if editedJSON == nil {
                // Refused HERE rather than on the wire: the daemon would answer
                // with a parse error the operator has to translate back into
                // "your JSON was wrong", and the edited text would be gone.
                Text("Arguments must be valid JSON.")
                    .font(.caption)
                    .foregroundStyle(FleetChatPalette.coral)
            }
        }
        .padding(18)
    }

    private var editedJSON: JSONValue? {
        try? FleetWire.decoder().decode(JSONValue.self, from: Data(editedArguments.utf8))
    }

    private func submitEdit(_ card: FleetChatConfirmCard) {
        guard let arguments = editedJSON else { return }
        store.answerConfirm(card, answer: .edit(arguments: arguments))
        editingCard = nil
    }

    private var canSend: Bool {
        store.canSendChat
            && store.chat.targetSessionKey != nil
            && !composer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func send() {
        guard canSend else { return }
        store.sendChatMessage(composer)
        composer = ""
    }
}

/// One attributed timeline row.
private struct FleetChatMessageView: View {
    let row: FleetChatMessageRow

    var body: some View {
        HStack {
            if row.actor.isOperator { Spacer(minLength: 60) }
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(row.actor.label)
                        .font(.caption2.weight(.bold))
                        .foregroundStyle(attributionColor)
                        .accessibilityIdentifier(row.actor.identifier)
                    Text(FleetChatLabels.messageKind(row.kind))
                        .font(.caption2)
                        .foregroundStyle(FleetChatPalette.muted)
                    if row.isReply {
                        Text("reply")
                            .font(.caption2)
                            .foregroundStyle(FleetChatPalette.muted)
                    }
                }
                Text(row.body)
                    .font(.callout)
                    .textSelection(.enabled)
            }
            .padding(10)
            .background(bubble, in: RoundedRectangle(cornerRadius: 10))
            .overlay(
                RoundedRectangle(cornerRadius: 10)
                    .strokeBorder(attributionColor.opacity(0.5), lineWidth: 1)
            )
            if !row.actor.isOperator { Spacer(minLength: 60) }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(row.actor.accessibilityLabel): \(row.body)")
    }

    private var attributionColor: Color {
        switch row.actor {
        case .operatorHuman: FleetChatPalette.amber
        case .copilot: FleetChatPalette.violet
        case .session: FleetChatPalette.mint
        case .unattributed: FleetChatPalette.coral
        }
    }

    private var bubble: Color {
        row.actor.isOperator ? FleetChatPalette.selected : FleetChatPalette.control
    }
}

/// Local palette. `FleetPalette` is file-private to `FleetWindowView.swift`;
/// these values match it rather than importing, and the chat-only accent
/// (`violet`, the copilot's colour in the TUI too) lives here.
private enum FleetChatPalette {
    static let canvas = Color(red: 0.055, green: 0.071, blue: 0.086)
    static let sidebar = Color(red: 0.043, green: 0.057, blue: 0.071)
    static let control = Color(red: 0.091, green: 0.118, blue: 0.141)
    static let selected = Color(red: 0.075, green: 0.172, blue: 0.255)
    static let separator = Color.white.opacity(0.08)
    static let muted = Color(red: 0.57, green: 0.64, blue: 0.71)
    static let mint = Color(red: 0.37, green: 0.88, blue: 0.76)
    static let amber = Color(red: 0.96, green: 0.72, blue: 0.23)
    static let coral = Color(red: 0.95, green: 0.43, blue: 0.49)
    static let violet = Color(red: 0.71, green: 0.60, blue: 0.98)
}
