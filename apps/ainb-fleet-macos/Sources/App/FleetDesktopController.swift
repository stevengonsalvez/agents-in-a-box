import AppKit
import SwiftUI

@MainActor
final class FleetPresentationStore: ObservableObject {
    @Published var preferences: FleetPresentationPreferences {
        didSet { preferences.save(defaults: defaults) }
    }

    private let defaults: UserDefaults

    init(defaults: UserDefaults) {
        self.defaults = defaults
        preferences = FleetPresentationPreferences.load(defaults: defaults)
    }

    var binding: Binding<FleetPresentationPreferences> {
        Binding(get: { self.preferences }, set: { self.preferences = $0 })
    }
}

enum FleetNotchRoute: String, CaseIterable, Identifiable {
    case sessions, needsYou, interviews, usage, settings

    var id: Self { self }

    var title: String {
        switch self {
        case .sessions: "Sessions"
        case .needsYou: "Needs you"
        case .interviews: "Interviews"
        case .usage: "Usage"
        case .settings: "Settings"
        }
    }
}

@MainActor
final class FleetNotchNavigation: ObservableObject {
    @Published var isExpanded = false
    @Published var route: FleetNotchRoute = .sessions
}

@MainActor
final class FleetDesktopController: NSObject {
    static var shared: FleetDesktopController?

    private let store: FleetStore
    private let presentation: FleetPresentationStore
    private let navigation = FleetNotchNavigation()
    private var notchPanel: NSWindow?

    init(store: FleetStore, presentation: FleetPresentationStore) {
        self.store = store
        self.presentation = presentation
        super.init()
    }

    func launch() {
        if notchPanel == nil {
            let size = NSSize(width: 320, height: 38)
            let panel: NSWindow
            if FleetAppConfiguration.isUITest {
                panel = NSWindow(
                    contentRect: NSRect(origin: .zero, size: size),
                    styleMask: [.borderless],
                    backing: .buffered,
                    defer: false
                )
            } else {
                panel = NSPanel(
                    contentRect: NSRect(origin: .zero, size: size),
                    styleMask: [.borderless, .nonactivatingPanel],
                    backing: .buffered,
                    defer: false
                )
            }
            panel.isOpaque = false
            panel.backgroundColor = .clear
            panel.hasShadow = false
            panel.level = FleetAppConfiguration.isUITest ? .floating : .statusBar
            panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
            let contentView = NSHostingView(rootView: FleetNotchView(
                store: store,
                presentation: presentation,
                navigation: navigation,
                setExpanded: { [weak self] in self?.setNotchExpanded($0) }
            ))
            panel.contentView = contentView
            notchPanel = panel
        }
        positionNotch()
        notchPanel?.orderFrontRegardless()
        if FleetAppConfiguration.isUITest {
            notchPanel?.makeKey()
        }
    }

    private func setNotchExpanded(_ expanded: Bool) {
        guard let panel = notchPanel else { return }
        panel.setFrame(FleetNotchGeometry.frame(expanded: expanded, on: notchScreen(for: panel)), display: true)
    }

    func open(_ url: URL) {
        guard url.scheme == "ainbfleet",
              url.host == "session",
              let encodedPath = URLComponents(url: url, resolvingAgainstBaseURL: false)?.percentEncodedPath,
              let key = String(encodedPath.dropFirst()).removingPercentEncoding,
              !key.isEmpty else { return }
        store.refresh()
        store.selectedSessionKey = key
        navigation.route = .needsYou
        navigation.isExpanded = true
        setNotchExpanded(true)
    }

    private func positionNotch() {
        guard let panel = notchPanel else { return }
        panel.setFrame(FleetNotchGeometry.frame(expanded: navigation.isExpanded, on: notchScreen(for: panel)), display: true)
    }

    private func notchScreen(for panel: NSWindow) -> NSScreen? {
        panel.screen ?? NSScreen.main ?? NSScreen.screens.first
    }
}

@MainActor
final class FleetAppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(FleetAppConfiguration.isUITest ? .regular : .accessory)
    }

    func application(_ application: NSApplication, open urls: [URL]) {
        urls.forEach { FleetDesktopController.shared?.open($0) }
    }
}

private struct FleetNotchView: View {
    @ObservedObject var store: FleetStore
    @ObservedObject var presentation: FleetPresentationStore
    @ObservedObject var navigation: FleetNotchNavigation
    let setExpanded: (Bool) -> Void
    @State private var search = ""
    @State private var usagePeriod: FleetUsagePeriod = .trailing7Days

    private var visibleSessions: [FleetSession] {
        FleetRosterPresentation.visibleSessions(
            store.sessions,
            search: search,
            filters: presentation.preferences.filters,
            sort: presentation.preferences.sort
        )
    }

    private var selectedSession: FleetSession? {
        guard let key = store.selectedSessionKey else { return routedSessions.first }
        return routedSessions.first(where: { $0.sessionKey == key }) ?? routedSessions.first
    }

    var body: some View {
        VStack(spacing: 0) {
            Button(action: toggleExpanded) {
                header
            }
            .buttonStyle(.plain)
            .frame(maxWidth: .infinity)
            .accessibilityIdentifier("fleet.notch")
            .accessibilityLabel(FleetStatusPresentation.label(active: store.activeCount, needsYou: store.needsYouCount, state: store.connectionState, sessions: store.sessions))
            .accessibilityHint(navigation.isExpanded ? "Collapse Fleet controls" : "Expand Fleet controls")

            if navigation.isExpanded {
                expandedContent
                    .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .frame(width: notchSize.width, height: notchSize.height, alignment: .top)
        .background(FleetNotchPalette.canvas, in: FleetNotchShape())
        .onChange(of: navigation.isExpanded) { _, value in setExpanded(value) }
        .onChange(of: visibleSessions.map(\.sessionKey)) { _, _ in selectFirstVisibleSession() }
        .onChange(of: presentation.preferences.filters) { _, _ in selectFirstVisibleSession() }
        .onChange(of: navigation.route) { _, _ in selectFirstVisibleSession() }
        .onAppear(perform: selectFirstVisibleSession)
    }

    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: FleetStatusPresentation.symbol(for: store.connectionState, needsYou: store.needsYouCount, sessions: store.sessions))
                .font(.caption2.weight(.bold))
                .foregroundStyle(store.needsYouCount > 0 ? .orange : .mint)
            Text("Fleet")
                .fontWeight(.bold)
            Spacer()
            Text(store.connectionState.isLive ? "\(store.activeCount) active · \(store.needsYouCount) needs you" : "Offline")
                .foregroundStyle(FleetNotchPalette.muted)
            Image(systemName: navigation.isExpanded ? "chevron.up" : "chevron.down")
                .font(.caption2.weight(.bold))
                .foregroundStyle(FleetNotchPalette.muted)
        }
        .font(.caption)
        .padding(.horizontal, 20)
        .frame(height: 38)
        .contentShape(Rectangle())
    }

    private var notchSize: CGSize {
        FleetNotchGeometry.size(expanded: navigation.isExpanded, on: NSScreen.main)
    }

    private var expandedContent: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(spacing: 14) {
                HStack(spacing: 8) {
                    TextField("Search sessions", text: $search)
                        .textFieldStyle(.roundedBorder)
                        .accessibilityIdentifier("fleet.notch.search")
                    Menu {
                        Button("All providers") { presentation.preferences.filters.provider = nil }
                        Divider()
                        Button("Claude") { presentation.preferences.filters.provider = .claude }
                        Button("Codex") { presentation.preferences.filters.provider = .codex }
                        Button("Copilot") { presentation.preferences.filters.provider = .copilot }
                        Button("ACP") { presentation.preferences.filters.provider = .acp }
                        Button("Unknown") { presentation.preferences.filters.provider = .unknown }
                    } label: {
                        Label(providerLabel, systemImage: "slider.horizontal.3")
                    }
                    .accessibilityIdentifier("fleet.notch.provider-filter")
                    Menu {
                        ForEach(FleetRosterFocus.allCases) { focus in
                            Button(focus.label) { presentation.preferences.filters.focus = focus }
                        }
                    } label: {
                        Label(presentation.preferences.filters.focus.label, systemImage: "line.3.horizontal.decrease.circle")
                    }
                    .accessibilityIdentifier("fleet.notch.focus-filter")
                    Button(action: toggleExpanded) { Image(systemName: "xmark") }
                        .accessibilityLabel("Close Fleet controls")
                }

                Picker("Fleet view", selection: $navigation.route) {
                    ForEach(FleetNotchRoute.allCases) { route in
                        Text(route.title).tag(route)
                    }
                }
                .pickerStyle(.segmented)
                .accessibilityIdentifier("fleet.notch.route")
            }
            .fixedSize(horizontal: false, vertical: true)

            routeContent
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        }
        .padding(16)
        .frame(height: 682, alignment: .top)
    }

    @ViewBuilder private var routeContent: some View {
        if case .readIncompatible = store.connectionState {
            VStack(alignment: .leading, spacing: 8) {
                Text(store.connectionState.message)
                    .font(.headline)
                Text("Update Fleet or install a compatible daemon before using controls.")
                    .font(.caption)
                    .foregroundStyle(FleetNotchPalette.muted)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .padding(16)
            .background(FleetNotchPalette.detail, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        } else {
            switch navigation.route {
            case .sessions, .needsYou:
                rosterContent
            case .interviews:
                FleetAnswerQueue(store: store) { navigation.route = .needsYou }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            case .usage:
                FleetUsageView(store: store, period: $usagePeriod)
            case .settings:
                FleetRuntimeSettingsView(store: store, presentation: presentation.binding)
            }
        }
    }

    private var routedSessions: [FleetSession] {
        if navigation.route == .needsYou {
            return FleetRosterPresentation.visibleSessions(
                store.sessions,
                search: search,
                filters: .attentionOnly,
                sort: presentation.preferences.sort
            )
        }
        return visibleSessions
    }

    private var rosterContent: some View {
        VStack(alignment: .leading, spacing: 14) {

            HStack {
                Text("\(routedSessions.count) of \(store.sessions.count) sessions")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(FleetNotchPalette.muted)
                Spacer()
                if !store.connectionState.isLive {
                    Button("Retry") { store.retry() }
                }
            }

            ScrollView {
                LazyVStack(spacing: 6) {
                    ForEach(routedSessions, id: \.sessionKey) { session in
                        FleetNotchSessionRow(
                            session: session,
                            selected: selectedSession?.sessionKey == session.sessionKey,
                            connection: store.connectionState
                        ) {
                            store.selectedSessionKey = session.sessionKey
                        }
                    }
                    if let selectedSession {
                        FleetNotchDetail(store: store, session: selectedSession) {
                            navigation.route = .interviews
                        }
                            .padding(.top, 4)
                    } else {
                        Text("No matching sessions")
                            .font(.subheadline)
                            .foregroundStyle(FleetNotchPalette.muted)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.vertical, 20)
                    }
                }
            }
        }
    }

    private var providerLabel: String {
        switch presentation.preferences.filters.provider {
        case .claude: "Claude"
        case .codex: "Codex"
        case .copilot: "Copilot"
        case .acp: "ACP"
        case .unknown: "Unknown"
        case nil: "All providers"
        }
    }

    private func toggleExpanded() {
        withAnimation(.easeInOut(duration: 0.16)) { navigation.isExpanded.toggle() }
    }

    private func selectFirstVisibleSession() {
        guard !routedSessions.contains(where: { $0.sessionKey == store.selectedSessionKey }) else { return }
        store.selectedSessionKey = routedSessions.first?.sessionKey
    }

}

private enum FleetNotchGeometry {
    static func size(expanded: Bool, on screen: NSScreen?) -> CGSize {
        let requested = CGSize(width: expanded ? 920 : 320, height: expanded ? 720 : 38)
        let available = screen?.frame.size ?? requested
        return CGSize(width: min(requested.width, available.width), height: min(requested.height, available.height))
    }

    static func frame(expanded: Bool, on screen: NSScreen?) -> NSRect {
        guard let screen else { return NSRect(origin: .zero, size: size(expanded: expanded, on: nil)) }
        let size = size(expanded: expanded, on: screen)
        return NSRect(
            x: screen.frame.midX - size.width / 2,
            y: screen.frame.maxY - size.height,
            width: size.width,
            height: size.height
        )
    }
}

private struct FleetNotchSessionRow: View {
    let session: FleetSession
    let selected: Bool
    let connection: FleetConnectionState
    let select: () -> Void

    private var identity: FleetSessionIdentity {
        FleetRosterPresentation.sessionIdentity(for: session)
    }

    var body: some View {
        Button(action: select) {
            HStack(alignment: .top, spacing: 10) {
                FleetProviderIcon(provider: session.provider, size: 25)
                VStack(alignment: .leading, spacing: 3) {
                    Text(identity.repository)
                        .font(.subheadline.weight(.semibold))
                        .lineLimit(1)
                    Text(metadata)
                        .font(.caption)
                        .foregroundStyle(FleetNotchPalette.muted)
                        .lineLimit(1)
                    Text(activity)
                        .font(.caption2)
                        .foregroundStyle(FleetNotchPalette.muted)
                        .lineLimit(1)
                }
                Spacer(minLength: 8)
                VStack(alignment: .trailing, spacing: 5) {
                    Text(status)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(statusColor)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 4)
                        .background(statusColor.opacity(0.14), in: Capsule())
                    Text(freshness)
                        .font(.caption2)
                        .foregroundStyle(FleetNotchPalette.muted)
                }
            }
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(selected ? FleetNotchPalette.selected : FleetNotchPalette.control, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("fleet.notch.row.\(session.sessionKey)")
        .accessibilityLabel(identity.accessibilityLabel)
        .accessibilityValue(FleetRosterPresentation.semanticStatus(for: session, connection: connection))
    }

    private var metadata: String {
        return [identity.worktree, identity.branch].compactMap { $0 }.joined(separator: " · ")
    }

    private var activity: String {
        if session.attention == .ask { return "Waiting for your answer" }
        if session.attention == .approval { return "Waiting for approval" }
        if session.activeWorkCount ?? 0 > 0 { return "\(session.activeWorkCount ?? 0) active tasks" }
        return session.lifecycle.rawValue.replacingOccurrences(of: "_", with: " ").capitalized
    }

    private var status: String {
        if session.attention != .none { return session.attention.rawValue.replacingOccurrences(of: "_", with: " ").capitalized }
        return session.lifecycle.rawValue.replacingOccurrences(of: "_", with: " ").capitalized
    }

    private var freshness: String {
        let seconds = max(0, Int(Date().timeIntervalSince1970 - TimeInterval(session.lastObservedAt) / 1_000))
        return seconds < 60 ? "now" : "\(seconds / 60)m"
    }

    private var statusColor: Color {
        if session.attention != .none { return .orange }
        if session.management == .degraded || session.transportHealth != .healthy { return .red }
        return .mint
    }
}

private struct FleetNotchDetail: View {
    @ObservedObject var store: FleetStore
    let session: FleetSession
    let answerInterview: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if session.attention == .approval {
                Text("Approval required")
                    .font(.headline)
                Text("This action is routed through Hangar and checked against the current request.")
                    .font(.caption)
                    .foregroundStyle(FleetNotchPalette.muted)
                HStack {
                    Button("Deny", role: .destructive) { store.decideApproval(.deny, on: session) }
                        .disabled(!store.canDecideApproval(.deny, on: session))
                    Button("Allow once") { store.decideApproval(.allowOnce, on: session) }
                        .buttonStyle(.borderedProminent)
                        .disabled(!store.canDecideApproval(.allowOnce, on: session))
                    if session.provider == .codex && session.capabilities.approvalSession {
                        Button("Always allow this session") { store.decideApproval(.bypassSession, on: session) }
                            .disabled(!store.canDecideApproval(.bypassSession, on: session))
                    }
                }
            } else if session.attention == .ask {
                Text("Interview ready")
                    .font(.headline)
                Text("Answer this structured interview without leaving the notch.")
                    .font(.caption)
                    .foregroundStyle(FleetNotchPalette.muted)
                Button("Answer interview", action: answerInterview)
                    .buttonStyle(.borderedProminent)
                    .disabled(!session.capabilities.structuredAnswer)
            } else {
                Text(FleetRosterPresentation.semanticStatus(for: session, connection: store.connectionState))
                    .font(.caption)
                    .foregroundStyle(FleetNotchPalette.muted)
            }
            if let notice = store.controlNotice {
                Text(notice)
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(FleetNotchPalette.detail, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .accessibilityIdentifier("fleet.notch.detail.\(session.sessionKey)")
    }
}

private struct FleetUsageView: View {
    @ObservedObject var store: FleetStore
    @Binding var period: FleetUsagePeriod

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Picker("Usage period", selection: $period) {
                    ForEach(FleetUsagePeriod.allCases) { option in Text(option.label).tag(option) }
                }
                .pickerStyle(.segmented)
                Spacer()
                Button("Refresh") {
                    store.refreshUsage(period: period)
                    store.refreshQuota()
                }
                    .disabled(!store.canReadUsage)
            }

            if !store.canReadUsage {
                VStack(spacing: 8) {
                    Image(systemName: "chart.bar.xaxis").font(.title2)
                    Text("Usage unavailable").font(.headline)
                    Text("This daemon does not advertise fleet.usage.read.").font(.caption)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let summary = store.usageSummary {
                usage(summary)
            } else {
                ProgressView("Reading canonical provider usage…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .onAppear {
            store.refreshUsage(period: period)
            store.refreshQuota()
        }
        .onChange(of: period) { _, value in store.refreshUsage(period: value) }
        .accessibilityIdentifier("fleet.notch.usage")
    }

    @ViewBuilder private func usage(_ summary: FleetUsageSummaryResult) -> some View {
        if let quotaSummary = store.quotaSummary {
            quotaCard(quotaSummary)
        }
        if let total = summary.totals {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 3) {
                    Text(total.costUSD.map(currency) ?? tokens(total.totalTokens))
                        .font(.system(size: 34, weight: .bold, design: .rounded))
                    Text(total.costUSD == nil ? "Tokens, price unavailable" : "Canonical provider cost")
                        .font(.caption)
                        .foregroundStyle(FleetNotchPalette.muted)
                }
                Spacer()
                VStack(alignment: .trailing, spacing: 3) {
                    Text("\(total.callCount) calls")
                    Text("\(total.sessionCount) sessions")
                    Text("\(total.projectCount) projects")
                }
                .font(.caption)
                .foregroundStyle(FleetNotchPalette.muted)
            }
            .padding(16)
            .background(FleetNotchPalette.detail, in: RoundedRectangle(cornerRadius: 14, style: .continuous))

            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    if !summary.daily.isEmpty { section("Daily", rows: summary.daily.map { "\($0.date)  \(value($0.bucket))" }) }
                    if !summary.providers.isEmpty { section("Providers", rows: summary.providers.map { "\($0.provider.capitalized)  \(value($0.bucket))" }) }
                    if !summary.models.isEmpty { section("Models", rows: summary.models.map { "\($0.model)  \(value($0.bucket))" }) }
                    if !summary.projects.isEmpty { section("Projects", rows: summary.projects.map { "\($0.repo ?? $0.project)  \(value($0.bucket))" }) }
                }
            }
        } else {
            VStack(spacing: 8) {
                Image(systemName: "chart.bar.xaxis").font(.title2)
                Text("Usage \(summary.state.rawValue)").font(.headline)
                Text(summary.detail ?? "Daemon returned no usable usage projection.").font(.caption)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    @ViewBuilder private func quotaCard(_ summary: FleetQuotaSummaryResult) -> some View {
        if !summary.providers.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Label("Live quota", systemImage: "gauge.with.dots.needle.67percent")
                        .font(.headline)
                    Spacer()
                    Text(summary.state == .ready ? "Live" : summary.state.rawValue.capitalized)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(summary.state == .ready ? .mint : .orange)
                }
                ForEach(summary.providers, id: \.provider) { provider in
                    HStack(spacing: 10) {
                        Text(provider.provider.capitalized)
                            .font(.subheadline.weight(.semibold))
                            .frame(width: 58, alignment: .leading)
                        quotaWindow("5h", window: provider.fiveHour)
                        quotaWindow("Week", window: provider.sevenDay)
                    }
                }
                if let detail = summary.detail {
                    Text(detail).font(.caption).foregroundStyle(FleetNotchPalette.muted)
                }
            }
            .padding(14)
            .background(FleetNotchPalette.detail, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        } else if let detail = summary.detail {
            Label(detail, systemImage: "gauge.with.dots.needle.67percent")
                .font(.caption)
                .foregroundStyle(FleetNotchPalette.muted)
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(FleetNotchPalette.detail, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        }
    }

    @ViewBuilder private func quotaWindow(_ label: String, window: FleetQuotaWindow?) -> some View {
        if let window {
            VStack(alignment: .leading, spacing: 1) {
                Text("\(label) · \(window.remainingPercent)% left")
                    .font(.caption.weight(.semibold))
                if let reset = window.resetsAt {
                    Text(Date(timeIntervalSince1970: TimeInterval(reset) / 1_000), style: .relative)
                        .font(.caption2)
                        .foregroundStyle(FleetNotchPalette.muted)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 7)
            .padding(.horizontal, 9)
            .background(FleetNotchPalette.canvas, in: RoundedRectangle(cornerRadius: 9, style: .continuous))
        }
    }

    private func section(_ title: String, rows: [String]) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.headline)
            ForEach(rows, id: \.self) { row in
                Text(row).font(.caption).foregroundStyle(FleetNotchPalette.muted)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(12)
        .background(FleetNotchPalette.detail, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }

    private func value(_ bucket: FleetUsageBucket) -> String {
        bucket.costUSD.map(currency) ?? tokens(bucket.totalTokens)
    }

    private func currency(_ value: Double) -> String { String(format: "$%.2f", value) }
    private func tokens(_ value: UInt64) -> String { value.formatted(.number.notation(.compactName)) + " tokens" }
}

private struct FleetRuntimeSettingsView: View {
    @ObservedObject var store: FleetStore
    @Binding var presentation: FleetPresentationPreferences

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                HStack {
                    Text("Runtime").font(.headline)
                    Spacer()
                    Button("Refresh") { store.refreshRuntime() }.disabled(!store.canReadRuntime)
                }
                VStack(alignment: .leading, spacing: 4) {
                    Text("Setup or repair")
                        .font(.subheadline.weight(.semibold))
                    Text("Install or repair Fleet runtime in Terminal, then refresh this view.")
                        .font(.caption)
                        .foregroundStyle(FleetNotchPalette.muted)
                        .textSelection(.enabled)
                }
                .padding(10)
                .background(FleetNotchPalette.detail, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                if let runtime = store.runtimeStatus {
                    Text("Daemon \(runtime.daemonVersion) · protocol \(runtime.protocolVersion)")
                        .font(.caption).foregroundStyle(FleetNotchPalette.muted)
                    ForEach(runtime.hooks, id: \.provider) { hook in
                        HStack {
                            Text(hook.provider.capitalized).fontWeight(.semibold)
                            Spacer()
                            Text(hook.installed && hook.hookReady ? "Installed" : "Setup required")
                                .foregroundStyle(hook.installed && hook.hookReady ? .mint : .orange)
                            Text(hook.deliveryReady ? "Live" : "Waiting")
                                .foregroundStyle(FleetNotchPalette.muted)
                        }
                        .font(.caption)
                        .padding(10)
                        .background(FleetNotchPalette.detail, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                    }
                } else {
                    Text(store.connectionState.isLive ? "Runtime status unavailable." : store.connectionState.message)
                        .font(.caption).foregroundStyle(FleetNotchPalette.muted)
                }
                Divider()
                FleetSettingsView(presentation: $presentation)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(.vertical, 2)
        }
        .onAppear(perform: store.refreshRuntime)
        .accessibilityIdentifier("fleet.notch.settings")
    }
}

private enum FleetNotchPalette {
    static let canvas = Color(red: 0.025, green: 0.035, blue: 0.05)
    static let control = Color.white.opacity(0.07)
    static let selected = Color(red: 0.05, green: 0.17, blue: 0.27)
    static let detail = Color.white.opacity(0.05)
    static let muted = Color(red: 0.62, green: 0.68, blue: 0.76)
    static let mint = Color(red: 0.31, green: 0.93, blue: 0.63)
}

private struct FleetNotchShape: Shape {
    func path(in rect: CGRect) -> Path {
        let radius = min(15, rect.height / 2)
        var path = Path()
        path.move(to: .zero)
        path.addLine(to: CGPoint(x: rect.maxX, y: 0))
        path.addLine(to: CGPoint(x: rect.maxX, y: rect.maxY - radius))
        path.addQuadCurve(
            to: CGPoint(x: rect.maxX - radius, y: rect.maxY),
            control: CGPoint(x: rect.maxX, y: rect.maxY)
        )
        path.addLine(to: CGPoint(x: radius, y: rect.maxY))
        path.addQuadCurve(
            to: CGPoint(x: 0, y: rect.maxY - radius),
            control: CGPoint(x: 0, y: rect.maxY)
        )
        path.closeSubpath()
        return path
    }
}
