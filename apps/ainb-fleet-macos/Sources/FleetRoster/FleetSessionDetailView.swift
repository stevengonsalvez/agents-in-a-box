import SwiftUI

struct FleetSessionDetailView: View {
    @ObservedObject var store: FleetStore
    let session: FleetSession
    let connection: FleetConnectionState
    @State private var prompt = ""
    @State private var pendingDestructiveAction: FleetOperatorAction?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                header
                state
                promptComposer
                controls
                metadata
                connectionStatus
            }
            .padding(24)
            .frame(maxWidth: 860, alignment: .leading)
        }
        .background(FleetDetailPalette.canvas)
        .accessibilityIdentifier("fleet.detail.\(session.sessionKey)")
        .confirmationDialog(
            "Confirm \(pendingDestructiveAction?.title ?? "action") for \(session.displayName ?? session.sessionKey)?",
            isPresented: Binding(
                get: { pendingDestructiveAction != nil },
                set: { if !$0 { pendingDestructiveAction = nil } }
            ),
            titleVisibility: .visible
        ) {
            if let action = pendingDestructiveAction {
                Button(action.title, role: .destructive) {
                    pendingDestructiveAction = nil
                    store.perform(action, on: session)
                }
            }
            Button("Cancel", role: .cancel) { pendingDestructiveAction = nil }
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 16) {
            Circle()
                .fill(statusColor)
                .frame(width: 12, height: 12)
                .padding(.top, 8)
            VStack(alignment: .leading, spacing: 5) {
                Text(session.displayName ?? session.sessionKey)
                    .font(.title2.weight(.bold))
                    .textSelection(.enabled)
                Text(session.cwd)
                    .font(.subheadline)
                    .foregroundStyle(FleetDetailPalette.muted)
                    .textSelection(.enabled)
            }
            Spacer(minLength: 12)
            VStack(alignment: .trailing, spacing: 6) {
                stateBadge(session.lifecycle.rawValue.replacingOccurrences(of: "_", with: " "), color: statusColor)
                Text(session.provider.rawValue.uppercased())
                    .font(.caption.weight(.bold))
                    .foregroundStyle(FleetDetailPalette.muted)
            }
        }
    }

    private var state: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Current state")
                .font(.headline)
            Grid(alignment: .leading, horizontalSpacing: 28, verticalSpacing: 10) {
                GridRow {
                    detail("Attention", session.attention.rawValue)
                    detail("Management", session.management.rawValue)
                }
                GridRow {
                    detail("Transport", session.transportHealth.rawValue)
                    detail("Last observed", FleetRosterPresentation.freshnessLabel(for: session))
                }
            }
        }
        .padding(16)
        .background(FleetDetailPalette.inset, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }

    private var promptComposer: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Send prompt")
                    .font(.headline)
                Spacer()
                if !store.canWrite {
                    Text("Read-only")
                        .font(.caption.weight(.medium))
                        .foregroundStyle(FleetDetailPalette.amber)
                }
            }
            TextField("Write an exact instruction", text: $prompt, axis: .vertical)
                .lineLimit(3...6)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("fleet.control.prompt")
            HStack {
                Text("Delivered through the daemon with the selected session version.")
                    .font(.caption)
                    .foregroundStyle(FleetDetailPalette.muted)
                Spacer()
                Button("Send prompt") {
                    store.perform(.sendPrompt, on: session, prompt: prompt)
                    prompt = ""
                }
                .buttonStyle(.borderedProminent)
                .disabled(!store.canSendPrompt(prompt, on: session))
                .accessibilityIdentifier("fleet.control.send-prompt")
            }
        }
    }

    private var controls: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Session controls")
                .font(.headline)
            HStack(spacing: 8) {
                ForEach(FleetOperatorAction.allCases.filter { $0 != .sendPrompt }) { action in
                    Button(action.title) {
                        if action.isDestructive {
                            pendingDestructiveAction = action
                        } else {
                            store.perform(action, on: session)
                        }
                    }
                    .buttonStyle(.bordered)
                    .tint(action.isDestructive ? FleetDetailPalette.coral : nil)
                    .disabled(!store.canPerform(action, on: session))
                    .accessibilityIdentifier("fleet.control.\(action.id)")
                }
            }
            if !store.canWrite {
                Text("This daemon connection is read-only. Controls activate when write compatibility is available.")
                    .font(.caption)
                    .foregroundStyle(FleetDetailPalette.muted)
            }
        }
    }

    private var metadata: some View {
        DisclosureGroup("Session metadata") {
            Grid(alignment: .leading, horizontalSpacing: 28, verticalSpacing: 8) {
                GridRow {
                    detail("Session key", session.sessionKey)
                    detail("Confidence", session.confidence.rawValue)
                }
                GridRow {
                    detail("Provenance", session.provenance.rawValue)
                    detail("Version", String(session.version))
                }
                GridRow {
                    detail("Revision", String(session.updatedRevision))
                    detail("Structured answer", session.capabilities.structuredAnswer ? "Available" : "Unavailable")
                }
            }
            .padding(.top, 10)
        }
        .font(.subheadline)
        .foregroundStyle(FleetDetailPalette.muted)
    }

    @ViewBuilder private var connectionStatus: some View {
        if let pendingIntentID = store.pendingIntentID {
            HStack(spacing: 8) {
                ProgressView()
                Text("Sending control intent \(pendingIntentID)")
            }
            .font(.caption)
            .foregroundStyle(FleetDetailPalette.muted)
        }
        if let notice = store.controlNotice {
            Text(notice)
                .font(.caption)
                .foregroundStyle(FleetDetailPalette.amber)
        }
        Text(connection.message)
            .font(.caption)
            .foregroundStyle(FleetDetailPalette.muted)
    }

    private func detail(_ label: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(label)
                .font(.caption)
                .foregroundStyle(FleetDetailPalette.muted)
            Text(value)
                .font(.subheadline.weight(.medium))
                .lineLimit(2)
                .textSelection(.enabled)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func stateBadge(_ label: String, color: Color) -> some View {
        Text(label)
            .font(.caption.weight(.bold))
            .foregroundStyle(color)
            .padding(.horizontal, 9)
            .padding(.vertical, 5)
            .background(color.opacity(0.12), in: Capsule())
    }

    private var statusColor: Color {
        if session.attention != .none { return FleetDetailPalette.amber }
        if session.management == .degraded || session.transportHealth != .healthy { return FleetDetailPalette.coral }
        return FleetDetailPalette.mint
    }
}

private enum FleetDetailPalette {
    static let canvas = Color(red: 0.055, green: 0.071, blue: 0.086)
    static let inset = Color(red: 0.071, green: 0.091, blue: 0.109)
    static let muted = Color(red: 0.57, green: 0.64, blue: 0.71)
    static let mint = Color(red: 0.37, green: 0.88, blue: 0.76)
    static let amber = Color(red: 0.96, green: 0.72, blue: 0.23)
    static let coral = Color(red: 0.95, green: 0.43, blue: 0.49)
}
