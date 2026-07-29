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
                ToolbarItem {
                    Toggle("Needs you", isOn: $presentation.filters.attentionOnly)
                        .accessibilityIdentifier("fleet.filter.attention")
                }
                ToolbarItem { Picker("Sort", selection: $presentation.sort) { ForEach(FleetRosterSort.allCases) { Text($0.label).tag($0) } } }
                ToolbarItem { Picker("Lifecycle", selection: $presentation.filters.lifecycle) { Text("Any lifecycle").tag(LifecycleState?.none); ForEach([LifecycleState.starting, .running, .turnComplete, .idle, .exited, .unknown], id: \.self) { Text($0.rawValue).tag(Optional($0)) } } }
                ToolbarItem { Picker("Provider", selection: $presentation.filters.provider) { Text("Any provider").tag(FleetProvider?.none); ForEach([FleetProvider.claude, .codex, .unknown], id: \.self) { Text($0.rawValue).tag(Optional($0)) } } }
                ToolbarItem { Picker("Management", selection: $presentation.filters.management) { Text("Any management").tag(ManagementState?.none); ForEach([ManagementState.managed, .degraded], id: \.self) { Text($0.rawValue).tag(Optional($0)) } } }
                ToolbarItem { Picker("Transport", selection: $presentation.filters.transportHealth) { Text("Any transport").tag(TransportHealth?.none); ForEach([TransportHealth.healthy, .degraded, .unavailable, .unknown], id: \.self) { Text($0.rawValue).tag(Optional($0)) } } }
                ToolbarItem {
                    Button("Quick switch") { switcherPresented = true }
                        .accessibilityIdentifier("fleet.quick-switch.open")
                }
                ToolbarItem { Button("Start") { startPresented = true }.disabled(!store.canStart).accessibilityIdentifier("fleet.start.open") }
                ToolbarItem { Button("Receipts") { receiptsPresented = true }.disabled(!store.canReadReceipts).accessibilityIdentifier("fleet.receipts.open") }
                ToolbarItemGroup {
                    Button("ATC") { atcPresented = true }.disabled(!store.canReadATC).accessibilityIdentifier("fleet.atc.open")
                    Button("Timeline") { timelinePresented = true }.disabled(!store.canReadTimeline).accessibilityIdentifier("fleet.timeline.open")
                    Button("Broadcast") { broadcastPresented = true }.disabled(!store.canBroadcast).accessibilityIdentifier("fleet.broadcast.open")
                }
            }
        } detail: {
            if let key = store.selectedSessionKey, let session = store.sessions.first(where: { $0.sessionKey == key }) {
                FleetSessionDetailView(store: store, session: session, connection: store.connectionState)
            } else {
                VStack(spacing: 8) {
                    Image(systemName: "bolt.circle")
                        .font(.largeTitle)
                        .accessibilityHidden(true)
                    Text("Select a Fleet session")
                        .font(.title3.weight(.semibold))
                    Text("Choose a session from the sidebar to inspect its current Fleet state.")
                }
                .foregroundStyle(.primary)
                .accessibilityElement(children: .combine)
                .accessibilityLabel("Select a Fleet session")
                .accessibilityHint("Choose a session from the sidebar to inspect its current Fleet state.")
            }
        }
        .overlay(alignment: .top) {
            if !store.connectionState.isLive { Text(store.connectionState.message).padding(8).background(.yellow.opacity(0.2)) }
        }
        .sheet(isPresented: $switcherPresented) { FleetQuickSwitcher(store: store, sort: presentation.sort, isPresented: $switcherPresented) }
        .sheet(isPresented: $startPresented) { FleetStartForm(store: store, isPresented: $startPresented) }
        .sheet(isPresented: $receiptsPresented) { FleetReceiptList(store: store) }
        .sheet(isPresented: $atcPresented) { FleetATCList(store: store) }
        .sheet(isPresented: $timelinePresented) { FleetTimelineList(store: store) }
        .sheet(isPresented: $broadcastPresented) { FleetBroadcastForm(store: store, isPresented: $broadcastPresented) }
        .frame(minWidth: 620, minHeight: 520)
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
    @State private var provider: FleetProvider = .codex
    @State private var cwd = FileManager.default.currentDirectoryPath
    @State private var prompt = ""

    var body: some View {
        Form {
            Picker("Provider", selection: $provider) {
                Text("Codex").tag(FleetProvider.codex)
                Text("Claude").tag(FleetProvider.claude)
            }
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
                    store.start(provider: provider, cwd: cwd, prompt: prompt)
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
