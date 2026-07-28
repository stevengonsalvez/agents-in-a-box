import SwiftUI

struct FleetWindowView: View {
    @ObservedObject var store: FleetStore
    @Binding var presentation: FleetPresentationPreferences
    @State private var search = ""
    @State private var switcherPresented = false

    var body: some View {
        NavigationSplitView {
            List(selection: $store.selectedSessionKey) {
                ForEach(FleetRosterPresentation.visibleSessions(store.sessions, search: search, filters: presentation.filters, sort: presentation.sort), id: \.sessionKey) { session in
                    HStack {
                        Image(systemName: FleetRosterPresentation.statusSymbol(for: session, connection: store.connectionState))
                            .accessibilityHidden(true)
                        VStack(alignment: .leading) {
                            Text(session.displayName ?? session.sessionKey)
                            Text("\(session.provider.rawValue) · \(FleetRosterPresentation.statusLabel(for: session))").font(.caption)
                            Text(session.cwd).font(.caption2).foregroundStyle(.secondary)
                            Text("Updated \(FleetRosterPresentation.freshnessLabel(for: session))").font(.caption2).foregroundStyle(.secondary)
                        }
                    }
                    .tag(session.sessionKey)
                    .accessibilityIdentifier("fleet.row.\(session.sessionKey)")
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel(session.displayName ?? session.sessionKey)
                    .accessibilityValue(FleetRosterPresentation.semanticStatus(for: session, connection: store.connectionState))
                }
            }
            .searchable(text: $search)
            .toolbar {
                ToolbarItem { Toggle("Needs you", isOn: $presentation.filters.attentionOnly) }
                ToolbarItem { Picker("Sort", selection: $presentation.sort) { ForEach(FleetRosterSort.allCases) { Text($0.label).tag($0) } } }
                ToolbarItem { Picker("Lifecycle", selection: $presentation.filters.lifecycle) { Text("Any lifecycle").tag(LifecycleState?.none); ForEach([LifecycleState.starting, .running, .turnComplete, .idle, .exited, .unknown], id: \.self) { Text($0.rawValue).tag(Optional($0)) } } }
                ToolbarItem { Picker("Provider", selection: $presentation.filters.provider) { Text("Any provider").tag(FleetProvider?.none); ForEach([FleetProvider.claude, .codex, .unknown], id: \.self) { Text($0.rawValue).tag(Optional($0)) } } }
                ToolbarItem { Picker("Management", selection: $presentation.filters.management) { Text("Any management").tag(ManagementState?.none); ForEach([ManagementState.managed, .degraded], id: \.self) { Text($0.rawValue).tag(Optional($0)) } } }
                ToolbarItem { Picker("Transport", selection: $presentation.filters.transportHealth) { Text("Any transport").tag(TransportHealth?.none); ForEach([TransportHealth.healthy, .degraded, .unavailable, .unknown], id: \.self) { Text($0.rawValue).tag(Optional($0)) } } }
                ToolbarItem { Button("Quick switch") { switcherPresented = true } }
            }
        } detail: {
            if let key = store.selectedSessionKey, let session = store.sessions.first(where: { $0.sessionKey == key }) {
                FleetSessionDetailView(session: session, connection: store.connectionState)
            } else {
                ContentUnavailableView("Select a Fleet session", systemImage: "bolt.circle")
            }
        }
        .overlay(alignment: .top) {
            if !store.connectionState.isLive { Text(store.connectionState.message).padding(8).background(.yellow.opacity(0.2)) }
        }
        .sheet(isPresented: $switcherPresented) { FleetQuickSwitcher(store: store, sort: presentation.sort, isPresented: $switcherPresented) }
        .frame(minWidth: 820, minHeight: 520)
    }
}
