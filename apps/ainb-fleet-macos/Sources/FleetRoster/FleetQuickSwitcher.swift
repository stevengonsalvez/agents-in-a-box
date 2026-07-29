import SwiftUI

struct FleetQuickSwitcher: View {
    @ObservedObject var store: FleetStore
    let sort: FleetRosterSort
    @Binding var isPresented: Bool
    @State private var search = ""

    var body: some View {
        NavigationStack {
            List(FleetRosterPresentation.visibleSessions(store.sessions, search: search, filters: .all, sort: sort), id: \.sessionKey) { session in
                Button(session.displayName ?? session.sessionKey) {
                    store.selectedSessionKey = session.sessionKey
                    isPresented = false
                }
                .accessibilityIdentifier("fleet.switch.\(session.sessionKey)")
            }
            .searchable(text: $search)
            .navigationTitle("Switch Fleet session")
        }
        .frame(minWidth: 420, minHeight: 360)
    }
}
