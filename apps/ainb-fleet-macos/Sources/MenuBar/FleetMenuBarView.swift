import SwiftUI

struct FleetMenuBarView: View {
    @ObservedObject var store: FleetStore
    let openFleet: (Bool) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(FleetStatusPresentation.label(active: store.activeCount, needsYou: store.needsYouCount, state: store.connectionState, sessions: store.sessions))
                .accessibilityIdentifier("fleet.status")
            Text("\(store.activeCount) active · \(store.needsYouCount) need you")
                .font(.caption)
            #if DEBUG
            Text("Diagnostics: canWrite \(store.canWrite), connection tasks \(store.debugConnectionTaskCount)")
                .accessibilityIdentifier("fleet.debug.connection")
            #endif
            Divider()
            ForEach(Array(FleetRosterPresentation.visibleSessions(store.sessions, search: "", filters: .attentionOnly).prefix(3)), id: \.sessionKey) { session in
                VStack(alignment: .leading) {
                    Text(session.displayName ?? session.sessionKey)
                    Text(FleetRosterPresentation.statusLabel(for: session)).font(.caption)
                }
                .accessibilityIdentifier("fleet.popover.\(session.sessionKey)")
            }
            if store.needsYouCount > 3 {
                Button("Overflow") { openFleet(true) }
                    .accessibilityIdentifier("fleet.overflow")
            }
            if !store.connectionState.isLive {
                Button("Retry") { store.retry() }.accessibilityIdentifier("fleet.retry")
            }
            Button("Open Fleet") { openFleet(false) }.accessibilityIdentifier("fleet.open")
        }
        .padding()
        .frame(width: 320)
        .task { store.start() }
    }
}
