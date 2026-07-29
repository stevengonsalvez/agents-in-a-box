import SwiftUI

struct FleetWindowView: View {
    @ObservedObject var store: FleetStore
    @Binding var presentation: FleetPresentationPreferences
    @State private var search = ""
    @State private var switcherPresented = false
    @State private var startPresented = false
    @State private var receiptsPresented = false
    @State private var broadcastPresented = false
    @State private var atcPresented = false
    @State private var timelinePresented = false

    private var visibleSessions: [FleetSession] {
        FleetRosterPresentation.visibleSessions(
            store.sessions,
            search: search,
            filters: presentation.filters,
            sort: presentation.sort
        )
    }

    var body: some View {
        HStack(spacing: 0) {
            roster
                .frame(minWidth: 290, idealWidth: 330, maxWidth: 380)
                .background(FleetPalette.sidebar)

            VStack(spacing: 0) {
                commandBar
                Divider().overlay(FleetPalette.separator)
                detail
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(FleetPalette.canvas)
        }
        .sheet(isPresented: $switcherPresented) { FleetQuickSwitcher(store: store, sort: presentation.sort, isPresented: $switcherPresented) }
        .sheet(isPresented: $startPresented) { FleetStartForm(store: store, isPresented: $startPresented) }
        .sheet(isPresented: $receiptsPresented) { FleetReceiptList(store: store) }
        .sheet(isPresented: $atcPresented) { FleetATCList(store: store) }
        .sheet(isPresented: $timelinePresented) { FleetTimelineList(store: store) }
        .sheet(isPresented: $broadcastPresented) { FleetBroadcastForm(store: store, isPresented: $broadcastPresented) }
        .onAppear(perform: selectFirstVisibleSession)
        .onReceive(store.$sessions) { selectFirstVisibleSession(in: $0) }
        .onChange(of: presentation.filters) { _, _ in selectFirstVisibleSession() }
        .onChange(of: presentation.sort) { _, _ in selectFirstVisibleSession() }
        .frame(minWidth: 760, minHeight: 560)
    }

    private var roster: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 12) {
                HStack(alignment: .firstTextBaseline) {
                    Text("Fleet")
                        .font(.title3.weight(.bold))
                    Spacer()
                    Text("\(visibleSessions.count) shown")
                        .font(.caption.weight(.medium))
                        .foregroundStyle(FleetPalette.muted)
                }
                TextField("Search sessions", text: $search)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier("fleet.search")
            }
            .padding(16)

            if visibleSessions.isEmpty {
                FleetEmptyRoster(query: search, hasFilters: presentation.filters != .all) {
                    search = ""
                    presentation.filters = .all
                }
            } else {
                ScrollView {
                    LazyVStack(spacing: 5) {
                        ForEach(visibleSessions, id: \.sessionKey) { session in
                            FleetRosterRow(
                                session: session,
                                isSelected: store.selectedSessionKey == session.sessionKey,
                                connection: store.connectionState
                            ) {
                                store.selectedSessionKey = session.sessionKey
                            }
                        }
                    }
                    .padding(.horizontal, 10)
                    .padding(.bottom, 12)
                }
            }
        }
    }

    private var commandBar: some View {
        HStack(spacing: 10) {
            connectionBadge

            Toggle("Needs you", isOn: $presentation.filters.attentionOnly)
                .toggleStyle(.button)
                .buttonStyle(.bordered)
                .accessibilityIdentifier("fleet.filter.attention")

            Menu {
                Button("All providers") { presentation.filters.provider = nil }
                Divider()
                Button("Claude") { presentation.filters.provider = .claude }
                Button("Codex") { presentation.filters.provider = .codex }
                Button("Unknown") { presentation.filters.provider = .unknown }
            } label: {
                Label(providerFilterLabel, systemImage: "slider.horizontal.3")
            }
            .accessibilityIdentifier("fleet.filter.provider")

            Menu {
                Button("Priority") { presentation.sort = .priority }
                Button("Recent") { presentation.sort = .recent }
                Divider()
                Button("Any lifecycle") { presentation.filters.lifecycle = nil }
                ForEach([LifecycleState.starting, .running, .turnComplete, .idle, .exited, .unknown], id: \.self) { lifecycle in
                    Button(lifecycle.rawValue.replacingOccurrences(of: "_", with: " ").capitalized) {
                        presentation.filters.lifecycle = lifecycle
                    }
                }
            } label: {
                Label(presentation.sort == .priority ? "Priority" : "Recent", systemImage: "arrow.up.arrow.down")
            }

            Spacer(minLength: 0)

            Button { switcherPresented = true } label: { Image(systemName: "magnifyingglass") }
                .accessibilityLabel("Quick switch")
                .accessibilityIdentifier("fleet.quick-switch.open")
            Button("Start") { startPresented = true }
                .disabled(!store.canStart)
                .accessibilityIdentifier("fleet.start.open")
            Menu {
                Button("Receipts") { receiptsPresented = true }.disabled(!store.canReadReceipts)
                Button("ATC") { atcPresented = true }.disabled(!store.canReadATC)
                Button("Timeline") { timelinePresented = true }.disabled(!store.canReadTimeline)
                Button("Broadcast") { broadcastPresented = true }.disabled(!store.canBroadcast)
            } label: {
                Image(systemName: "ellipsis.circle")
            }
        }
        .controlSize(.regular)
        .padding(.horizontal, 18)
        .padding(.vertical, 12)
    }

    @ViewBuilder private var detail: some View {
        if let key = store.selectedSessionKey,
           let session = store.sessions.first(where: { $0.sessionKey == key }) {
            FleetSessionDetailView(store: store, session: session, connection: store.connectionState)
        } else {
            FleetEmptyDetail()
        }
    }

    private var connectionBadge: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(store.connectionState.isLive ? FleetPalette.mint : FleetPalette.amber)
                .frame(width: 7, height: 7)
            Text(store.connectionState.isLive ? "Live" : "Offline")
                .font(.caption.weight(.semibold))
        }
        .foregroundStyle(FleetPalette.ink)
        .padding(.horizontal, 9)
        .padding(.vertical, 6)
        .background(FleetPalette.control, in: Capsule())
        .accessibilityLabel(store.connectionState.message)
    }

    private var providerFilterLabel: String {
        switch presentation.filters.provider {
        case .claude: "Claude"
        case .codex: "Codex"
        case .unknown: "Unknown"
        case nil: "All providers"
        }
    }

    private func selectFirstVisibleSession() {
        selectFirstVisibleSession(in: visibleSessions)
    }

    private func selectFirstVisibleSession(in sessions: [FleetSession]) {
        let matching = FleetRosterPresentation.visibleSessions(
            sessions,
            search: search,
            filters: presentation.filters,
            sort: presentation.sort
        )
        guard !matching.isEmpty,
              !matching.contains(where: { $0.sessionKey == store.selectedSessionKey }) else { return }
        store.selectedSessionKey = matching.first?.sessionKey
    }
}

private enum FleetPalette {
    static let canvas = Color(red: 0.055, green: 0.071, blue: 0.086)
    static let sidebar = Color(red: 0.043, green: 0.057, blue: 0.071)
    static let control = Color(red: 0.091, green: 0.118, blue: 0.141)
    static let selected = Color(red: 0.075, green: 0.172, blue: 0.255)
    static let separator = Color.white.opacity(0.08)
    static let ink = Color(red: 0.89, green: 0.92, blue: 0.95)
    static let muted = Color(red: 0.57, green: 0.64, blue: 0.71)
    static let mint = Color(red: 0.37, green: 0.88, blue: 0.76)
    static let amber = Color(red: 0.96, green: 0.72, blue: 0.23)
    static let coral = Color(red: 0.95, green: 0.43, blue: 0.49)
}

private struct FleetRosterRow: View {
    let session: FleetSession
    let isSelected: Bool
    let connection: FleetConnectionState
    let select: () -> Void

    var body: some View {
        Button(action: select) {
            HStack(alignment: .top, spacing: 10) {
                Circle()
                    .fill(statusColor)
                    .frame(width: 8, height: 8)
                    .padding(.top, 6)
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 6) {
                        Text(session.displayName ?? session.sessionKey)
                            .font(.subheadline.weight(.semibold))
                            .lineLimit(1)
                        Spacer(minLength: 0)
                        Text(session.provider.rawValue.uppercased())
                            .font(.caption2.weight(.bold))
                            .foregroundStyle(FleetPalette.muted)
                    }
                    Text(FleetRosterPresentation.statusLabel(for: session).replacingOccurrences(of: "_", with: " "))
                        .font(.caption)
                        .foregroundStyle(isSelected ? FleetPalette.ink : FleetPalette.muted)
                        .lineLimit(1)
                    Text(session.cwd)
                        .font(.caption2)
                        .foregroundStyle(FleetPalette.muted)
                        .lineLimit(1)
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 9)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(isSelected ? FleetPalette.selected : .clear, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("fleet.row.\(session.sessionKey)")
        .accessibilityLabel(session.displayName ?? session.sessionKey)
        .accessibilityValue(FleetRosterPresentation.semanticStatus(for: session, connection: connection))
    }

    private var statusColor: Color {
        if session.attention != .none { return FleetPalette.amber }
        if session.management == .degraded || session.transportHealth != .healthy { return FleetPalette.coral }
        return FleetPalette.mint
    }
}

private struct FleetEmptyRoster: View {
    let query: String
    let hasFilters: Bool
    let reset: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Image(systemName: "line.3.horizontal.decrease.circle")
                .font(.title2)
                .foregroundStyle(FleetPalette.muted)
            Text(query.isEmpty ? "No matching sessions" : "No match for \(query)")
                .font(.headline)
            Text(hasFilters ? "Clear filters to inspect the rest of Fleet." : "The daemon has not reported sessions yet.")
                .font(.caption)
                .foregroundStyle(FleetPalette.muted)
            if hasFilters || !query.isEmpty {
                Button("Clear filters", action: reset)
                    .buttonStyle(.bordered)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .padding(20)
    }
}

private struct FleetEmptyDetail: View {
    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: "scope")
                .font(.system(size: 28, weight: .medium))
                .foregroundStyle(FleetPalette.mint)
            Text("Choose a Fleet session")
                .font(.title3.weight(.semibold))
            Text("Select a session to inspect current state and send exact controls.")
                .foregroundStyle(FleetPalette.muted)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Choose a Fleet session")
    }
}

private struct FleetTimelineList: View {
    @ObservedObject var store: FleetStore

    var body: some View {
        List(store.timeline, id: \.revision) { entry in
            VStack(alignment: .leading) {
                Text(entry.kind.rawValue.replacingOccurrences(of: "_", with: " ").capitalized)
                Text(entry.sessionKey).font(.caption).textSelection(.enabled)
                Text(Date(timeIntervalSince1970: TimeInterval(entry.observedAt) / 1_000).formatted(date: .abbreviated, time: .shortened)).font(.caption2).foregroundStyle(.secondary)
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel(entry.kind.rawValue)
            .accessibilityValue(entry.sessionKey)
        }
        .navigationTitle("Fleet timeline")
        .frame(minWidth: 520, minHeight: 360)
        .onAppear { store.refreshTimeline() }
        .accessibilityIdentifier("fleet.timeline.list")
    }
}

private struct FleetATCList: View {
    @ObservedObject var store: FleetStore

    var body: some View {
        List(store.atcInstances, id: \.name) { instance in
            VStack(alignment: .leading) {
                Text(instance.name)
                Text(instance.enabled ? "Enabled" : "Disabled")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text("Cron \(instance.heartbeatCron) · retry cap \(instance.errRetryCap)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text("Next \(timestamp(instance.nextTickAt)) · last \(timestamp(instance.lastHeartbeatAt))")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel(instance.name)
            .accessibilityValue(instance.enabled ? "Enabled" : "Disabled")
        }
        .navigationTitle("ATC")
        .safeAreaInset(edge: .bottom) {
            VStack(alignment: .leading, spacing: 4) {
                if let ownership = store.atcSchedulerOwnership {
                    Text(ownershipLabel(ownership)).font(.caption).foregroundStyle(.secondary)
                }
                Text("Schedule edits remain disabled until daemon scheduler ownership is proven.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding()
        }
        .frame(minWidth: 520, minHeight: 360)
        .onAppear { store.refreshATC() }
        .accessibilityIdentifier("fleet.atc.list")
    }

    private func timestamp(_ value: Int64?) -> String {
        guard let value else { return "not scheduled" }
        return Date(timeIntervalSince1970: TimeInterval(value) / 1_000).formatted(date: .abbreviated, time: .shortened)
    }

    private func ownershipLabel(_ ownership: AtcSchedulerOwnership) -> String {
        switch ownership {
        case .legacyTimerReconciliationRequired: "Legacy timer reconciliation required"
        }
    }
}

private struct FleetStartForm: View {
    @ObservedObject var store: FleetStore
    @Binding var isPresented: Bool
    @State private var cwd = FileManager.default.currentDirectoryPath
    @State private var prompt = ""

    var body: some View {
        Form {
            LabeledContent("Provider", value: "Codex")
            TextField("Working directory", text: $cwd)
            TextField("Initial prompt", text: $prompt, axis: .vertical)
            if let start = store.lastStart {
                LabeledContent("Prospective session", value: start.prospectiveSessionKey)
                LabeledContent("Receipt", value: start.receipt.status.rawValue)
            }
            if let notice = store.controlNotice { Text(notice).foregroundStyle(.secondary) }
            HStack {
                Button("Cancel") { isPresented = false }
                Spacer()
                Button("Start") {
                    store.start(provider: .codex, cwd: cwd, prompt: prompt)
                }
                .disabled(!store.canStart || !FleetStartPreflight.isExistingDirectory(cwd.trimmingCharacters(in: .whitespacesAndNewlines)))
                .accessibilityIdentifier("fleet.start.submit")
            }
        }
        .padding()
        .frame(minWidth: 440)
        .accessibilityIdentifier("fleet.start.form")
    }
}

private struct FleetReceiptList: View {
    @ObservedObject var store: FleetStore

    var body: some View {
        List(store.receipts, id: \.requestID) { receipt in
            VStack(alignment: .leading) {
                Text(receipt.actionKind)
                Text(receipt.status.rawValue).font(.caption).foregroundStyle(.secondary)
                Text(receipt.detail ?? "No daemon detail").font(.caption).foregroundStyle(.secondary)
                Text(receipt.requestID).font(.caption2).textSelection(.enabled)
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel("\(receipt.actionKind), \(receipt.status.rawValue)")
            .accessibilityValue(receipt.detail ?? "No daemon detail")
        }
        .navigationTitle("Receipts")
        .frame(minWidth: 520, minHeight: 360)
        .onAppear { store.refreshReceipts() }
        .accessibilityIdentifier("fleet.receipts.list")
    }
}

private struct FleetBroadcastForm: View {
    @ObservedObject var store: FleetStore
    @Binding var isPresented: Bool
    @State private var text = ""
    @State private var selected = Set<String>()
    @State private var confirming = false

    private var targets: [FleetSession] {
        store.sessions.filter { $0.version > 0 && ($0.capabilities.sendPrompt || $0.capabilities.tmuxText) }
    }

    private var orderedTargets: [String] {
        targets.map(\.sessionKey).filter(selected.contains)
    }

    var body: some View {
        Form {
            TextField("Message", text: $text, axis: .vertical)
            Section("Recipients") {
                ForEach(targets, id: \.sessionKey) { session in
                    Toggle(session.displayName ?? session.sessionKey, isOn: Binding(
                        get: { selected.contains(session.sessionKey) },
                        set: { enabled in
                            if enabled { selected.insert(session.sessionKey) }
                            else { selected.remove(session.sessionKey) }
                        }
                    ))
                }
            }
            Text("Targets: \(orderedTargets.joined(separator: ", "))")
                .font(.caption)
                .foregroundStyle(.secondary)
            if let notice = store.controlNotice { Text(notice).foregroundStyle(.secondary) }
            HStack {
                Button("Cancel") { isPresented = false }
                Spacer()
                Button("Review broadcast") { confirming = true }
                    .disabled(!store.canBroadcast || orderedTargets.isEmpty || text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding()
        .frame(minWidth: 480)
        .confirmationDialog("Send to \(orderedTargets.count) explicit recipients?", isPresented: $confirming, titleVisibility: .visible) {
            Button("Send") {
                store.broadcast(targetKeys: orderedTargets, text: text)
                isPresented = false
            }
            Button("Cancel", role: .cancel) {}
        }
        .accessibilityIdentifier("fleet.broadcast.form")
    }
}
