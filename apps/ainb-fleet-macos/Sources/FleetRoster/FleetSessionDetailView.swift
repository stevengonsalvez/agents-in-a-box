import SwiftUI

struct FleetSessionDetailView: View {
    let session: FleetSession
    let connection: FleetConnectionState

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
            Section("Connection") { Text(connection.message) }
        }
        .accessibilityIdentifier("fleet.detail.\(session.sessionKey)")
    }

    @ViewBuilder private func capability(_ name: String, _ available: Bool) -> some View {
        LabeledContent(name, value: available ? "Available" : "Unavailable")
    }
}
