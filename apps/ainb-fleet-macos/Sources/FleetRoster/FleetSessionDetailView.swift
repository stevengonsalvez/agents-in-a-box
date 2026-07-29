import SwiftUI

struct FleetSessionDetailView: View {
    @ObservedObject var store: FleetStore
    let session: FleetSession
    let connection: FleetConnectionState
    @State private var prompt = ""
    @State private var pendingDestructiveAction: FleetOperatorAction?

    var body: some View {
        Form {
            Section("Identity") {
                LabeledContent("Session key", value: session.sessionKey)
                LabeledContent("Provider", value: session.provider.rawValue)
                LabeledContent("Display name", value: session.displayName ?? "Unavailable")
                LabeledContent("Workspace", value: session.cwd)
            }
            Section("State") {
                LabeledContent("Lifecycle", value: session.lifecycle.rawValue)
                LabeledContent("Attention", value: session.attention.rawValue)
                LabeledContent("Management", value: session.management.rawValue)
                LabeledContent("Transport", value: session.transportHealth.rawValue)
                LabeledContent("Freshness", value: FleetRosterPresentation.freshnessLabel(for: session))
            }
            Section("Source") {
                LabeledContent("Provenance", value: session.provenance.rawValue)
                LabeledContent("Confidence", value: session.confidence.rawValue)
                LabeledContent("Session version", value: String(session.version))
                LabeledContent("Updated revision", value: String(session.updatedRevision))
            }
            Section("Capabilities") {
                capability("Structured answer", session.capabilities.structuredAnswer)
                capability("Approvals", session.capabilities.approvals)
                capability("Send prompt", session.capabilities.sendPrompt)
                capability("Continue", session.capabilities.continueTurn)
                capability("Retry", session.capabilities.retry)
                capability("Interrupt", session.capabilities.interrupt)
            }
            Section("Controls") {
                TextField("Prompt", text: $prompt, axis: .vertical)
                    .accessibilityIdentifier("fleet.control.prompt")
                Button("Send prompt") {
                    store.perform(.sendPrompt, on: session, prompt: prompt)
                    prompt = ""
                }
                .disabled(!store.canSendPrompt(prompt, on: session))
                .accessibilityIdentifier("fleet.control.send-prompt")
                ForEach(FleetOperatorAction.allCases.filter { $0 != .sendPrompt }) { action in
                    Button(action.title) {
                        if action.isDestructive {
                            pendingDestructiveAction = action
                        } else {
                            store.perform(action, on: session)
                        }
                    }
                    .disabled(!store.canPerform(action, on: session))
                    .accessibilityIdentifier("fleet.control.\(action.id)")
                }
                Text("Approve and deny are unavailable until Fleet projects durable typed request context.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            if let pendingIntentID = store.pendingIntentID {
                Section("Control status") {
                    LabeledContent("Pending intent", value: pendingIntentID)
                    ProgressView()
                }
            }
            if let notice = store.controlNotice {
                Section("Control status") { Text(notice).foregroundStyle(.secondary) }
            }
            Section("Connection") { Text(connection.message) }
        }
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

    @ViewBuilder private func capability(_ name: String, _ available: Bool) -> some View {
        LabeledContent(name, value: available ? "Available" : "Unavailable")
    }
}
