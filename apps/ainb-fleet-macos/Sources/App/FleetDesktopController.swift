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

/// Interviews are answered inline in the session detail, so there is no separate
/// Interviews destination -- a tab that only re-showed the roster was a dead end
/// in a nav that is meant to be the single one.
enum FleetNotchRoute: String, CaseIterable, Identifiable {
    case sessions, needsYou, usage, settings

    var id: Self { self }

    var title: String {
        switch self {
        case .sessions: "Sessions"
        case .needsYou: "Needs you"
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
                panel = FleetNotchPanel(
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
            (panel as? NSPanel)?.becomesKeyOnlyIfNeeded = true
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
                    Button(action: quit) {
                        Label("Quit", systemImage: "xmark")
                    }
                    .accessibilityIdentifier("fleet.notch.quit")
                    .accessibilityLabel("Quit Fleet")
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
                        FleetNotchDetail(store: store, session: selectedSession)
                            // Identity must change with the session AND with the
                            // question set. Without this SwiftUI reuses the view
                            // and its @State, so a draft cursor from a 5-question
                            // interview survives into a 2-question one and lands
                            // out of range, leaving the new interview unanswerable.
                            .id("\(selectedSession.sessionKey):\(selectedSession.currentRequestFingerprint ?? "")")
                            .padding(.top, 4)
                    } else {
                        FleetRosterEmptyState(
                            connection: store.connectionState,
                            total: store.sessions.count,
                            filters: presentation.preferences.filters,
                            retry: { store.retry() },
                            clearFilters: { presentation.preferences.filters = .all }
                        )
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

    private func quit() {
        NSApp.terminate(nil)
    }

    private func selectFirstVisibleSession() {
        guard !routedSessions.contains(where: { $0.sessionKey == store.selectedSessionKey }) else { return }
        store.selectedSessionKey = routedSessions.first?.sessionKey
    }

}

private final class FleetNotchPanel: NSPanel {
    override var canBecomeKey: Bool { true }
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

    @State private var questionIndex = 0
    @State private var selections: [String: Set<String>] = [:]
    @State private var textAnswers: [String: String] = [:]
    @State private var rejectConfirmation = false

    private var deck: FleetInterviewDeck? { FleetInterviewDeck(session: session) }

    /// A deck is never empty (its initialiser fails on zero questions), so the
    /// clamp always yields a valid row even if a stale cursor survives.
    private func clampedIndex(_ deck: FleetInterviewDeck) -> Int {
        min(max(questionIndex, 0), deck.questions.count - 1)
    }

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
            } else if let deck, let question = deck.questions[safe: clampedIndex(deck)] {
                inlineInterview(deck: deck, question: question)
            } else if session.attention == .ask {
                Text("Interview ready")
                    .font(.headline)
                Text("Structured answer capability not available for this session.")
                    .font(.caption)
                    .foregroundStyle(FleetNotchPalette.muted)
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
        .confirmationDialog("Reject this interview?", isPresented: $rejectConfirmation) {
            Button("Reject", role: .destructive) {
                store.selectedSessionKey = session.sessionKey
                store.dismissStructuredInterview(on: session)
            }
        }
    }

    // MARK: - Inline interview

    @ViewBuilder private func inlineInterview(deck: FleetInterviewDeck, question: FleetInterviewQuestion) -> some View {
        // Question tabs
        if deck.questions.count > 1 {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 6) {
                    ForEach(Array(deck.questions.enumerated()), id: \.element.id) { index, q in
                        Button {
                            questionIndex = index
                        } label: {
                            Text("\(index + 1). \(q.header)")
                                .font(.caption2.weight(.semibold))
                                .padding(.horizontal, 8)
                                .padding(.vertical, 4)
                                .background(
                                    index == questionIndex ? FleetNotchPalette.selected : FleetNotchPalette.canvas,
                                    in: RoundedRectangle(cornerRadius: 6, style: .continuous)
                                )
                                .foregroundStyle(answered(deck: deck, question: q) ? .mint : .primary)
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
        }

        Text(question.header).font(.subheadline.weight(.semibold))
        Text(question.text).font(.caption).fixedSize(horizontal: false, vertical: true)

        if question.options.isEmpty {
            TextField("Type answer", text: textBinding(deck: deck, question: question), axis: .vertical)
                .textFieldStyle(.roundedBorder)
                .lineLimit(2...5)
                .font(.caption)
        } else {
            ForEach(question.options, id: \.self) { option in
                Button {
                    toggle(option, deck: deck, question: question)
                } label: {
                    HStack(spacing: 6) {
                        Image(systemName: isSelected(option, deck: deck, question: question)
                              ? (question.multiSelect ? "checkmark.square.fill" : "largecircle.fill.circle")
                              : (question.multiSelect ? "square" : "circle"))
                            .font(.caption)
                        Text(option).font(.caption)
                        Spacer()
                    }
                    .padding(6)
                    .background(FleetNotchPalette.canvas, in: RoundedRectangle(cornerRadius: 6, style: .continuous))
                }
                .buttonStyle(.plain)
            }
            if isSelected("Other", deck: deck, question: question) {
                TextField("Describe Other", text: textBinding(deck: deck, question: question))
                    .textFieldStyle(.roundedBorder)
                    .font(.caption)
            }
        }

        HStack(spacing: 8) {
            if deck.questions.count > 1 {
                Button("\u{2190}") { questionIndex = max(questionIndex - 1, 0) }
                    .disabled(questionIndex == 0)
                Button("\u{2192}") { questionIndex = min(questionIndex + 1, deck.questions.count - 1) }
                    .disabled(questionIndex + 1 >= deck.questions.count)
            }
            Spacer()
            if session.capabilities.structuredDismiss {
                Button("Reject", role: .destructive) { rejectConfirmation = true }
                    .font(.caption)
            }
            Button("Submit") { submit(deck) }
                .buttonStyle(.borderedProminent)
                .font(.caption)
                .disabled(!complete(deck) || store.pendingIntentID != nil)
        }
    }

    // MARK: - Interview state helpers

    private func answerKey(_ deck: FleetInterviewDeck, _ question: FleetInterviewQuestion) -> String {
        "\(deck.session.sessionKey):\(deck.session.currentRequestFingerprint ?? ""):\(question.id)"
    }

    private func isSelected(_ option: String, deck: FleetInterviewDeck, question: FleetInterviewQuestion) -> Bool {
        selections[answerKey(deck, question), default: []].contains(option)
    }

    private func toggle(_ option: String, deck: FleetInterviewDeck, question: FleetInterviewQuestion) {
        let key = answerKey(deck, question)
        if question.multiSelect {
            if selections[key, default: []].contains(option) {
                selections[key, default: []].remove(option)
            } else {
                selections[key, default: []].insert(option)
            }
        } else {
            selections[key] = [option]
        }
    }

    private func textBinding(deck: FleetInterviewDeck, question: FleetInterviewQuestion) -> Binding<String> {
        let key = answerKey(deck, question)
        return Binding(get: { textAnswers[key, default: ""] }, set: { textAnswers[key] = $0 })
    }

    private func answered(deck: FleetInterviewDeck, question: FleetInterviewQuestion) -> Bool {
        let key = answerKey(deck, question)
        if question.options.isEmpty { return !textAnswers[key, default: ""].trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
        let selected = selections[key, default: []]
        return !selected.isEmpty && (!selected.contains("Other") || !textAnswers[key, default: ""].trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
    }

    private func complete(_ deck: FleetInterviewDeck) -> Bool {
        deck.questions.allSatisfy { answered(deck: deck, question: $0) }
    }

    private func submit(_ deck: FleetInterviewDeck) {
        store.selectedSessionKey = session.sessionKey
        let answers = deck.questions.map { question in
            let key = answerKey(deck, question)
            let selected = Array(selections[key, default: []]).sorted()
            let text = textAnswers[key]?.trimmingCharacters(in: .whitespacesAndNewlines)
            return FleetQuestionAnswer(questionID: question.id, selectedOptions: selected, text: text?.isEmpty == false ? text : nil)
        }
        store.submitStructuredAnswers(answers, on: session)
    }
}

/// One usage view, segmented into tabs, rather than a single long scroll.
///
/// Modelled on CodeBurn's `InsightMode`: a scrolling pill row over one shared
/// dataset, so a tab is a lens on the same numbers rather than a separate
/// fetch. The previous Quick/Dashboard split asked the operator to learn a
/// distinction that bought them nothing, since both showed cost over different
/// windows.
private enum UsageTab: String, CaseIterable, Identifiable {
    case overview, trend, calendar, forecast, projects, sessions, commands

    var id: Self { self }

    var title: String {
        switch self {
        case .overview: "Overview"
        case .trend: "Trend"
        case .calendar: "Calendar"
        case .forecast: "Forecast"
        case .projects: "Projects"
        case .sessions: "Sessions"
        case .commands: "Commands"
        }
    }
}

/// The window a client-derived tab is looking at.
///
/// Only the daily series can honour this without a wire change: the dashboard
/// ships 53 weeks of daily cells, so slicing them is free. The breakdowns
/// arrive pre-aggregated over the whole window and cannot be re-sliced here,
/// which is why they carry an explicit "53 weeks" caption instead of silently
/// ignoring the selection.
private enum UsageLens: String, CaseIterable, Identifiable {
    case week, month, all

    var id: Self { self }

    var title: String {
        switch self {
        case .week: "7d"
        case .month: "30d"
        case .all: "53w"
        }
    }

    var days: Int? {
        switch self {
        case .week: 7
        case .month: 30
        case .all: nil
        }
    }
}

private struct FleetUsageView: View {
    @ObservedObject var store: FleetStore
    @Binding var period: FleetUsagePeriod
    @State private var tab: UsageTab = .overview
    // 30d by default: across 53 weeks a single recent spike flattens every
    // other bar into the axis, so the long window is what Calendar is for.
    @State private var lens: UsageLens = .month
    @State private var selectedDay: FleetHeatmapCell?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header

            if !store.canReadDashboard {
                unavailable(
                    "Usage unavailable",
                    detail: "This daemon does not advertise fleet.dashboard.read."
                )
            } else if let dash = store.usageDashboard {
                if dash.totals == nil {
                    unavailable(
                        "Usage \(dash.state.rawValue)",
                        detail: dash.detail ?? "The daemon has no usable projection yet."
                    )
                } else {
                    ScrollView {
                        VStack(alignment: .leading, spacing: 12) {
                            content(dash)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.bottom, 8)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            } else {
                ProgressView("Building 53-week dashboard\u{2026}")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .onAppear {
            store.refreshDashboard()
            store.refreshQuota()
        }
        .accessibilityIdentifier("fleet.notch.usage")
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                FleetPillSwitcher(selection: $tab, options: UsageTab.allCases) { $0.title }
                Spacer(minLength: 8)
                Button("Refresh") {
                    store.refreshDashboard()
                    store.refreshQuota()
                }
                .controlSize(.small)
            }
            if tab.showsLens {
                FleetPillSwitcher(selection: $lens, options: UsageLens.allCases) { $0.title }
            }
        }
    }

    @ViewBuilder private func content(_ dash: FleetUsageDashboardResult) -> some View {
        switch tab {
        case .overview: overviewTab(dash)
        case .trend: trendTab(dash)
        case .calendar: calendarTab(dash)
        case .forecast: forecastTab(dash)
        case .projects: projectsTab(dash)
        case .sessions: sessionsTab(dash)
        case .commands: commandsTab(dash)
        }
    }

    // MARK: - Tabs

    @ViewBuilder private func overviewTab(_ dash: FleetUsageDashboardResult) -> some View {
        if let total = dash.totals { heroCard(total, state: dash) }
        if let quota = store.quotaSummary { quotaCard(quota) }

        let stats = FleetUsageStats(heatmap: dash.heatmap)
        FleetStatStrip(tiles: [
            .init(label: "Peak day", value: stats.peakCostText, accent: true),
            .init(label: "Avg active day", value: stats.averageActiveText, accent: false),
            .init(label: "Streak", value: stats.streakText, accent: false),
        ])

        // Cache hit and cost per session need no wire change: the bucket
        // already breaks out cache-read against fresh input tokens.
        if let total = dash.totals {
            FleetStatStrip(tiles: [
                .init(label: "Cache hit", value: FleetUsageStats.cacheHitText(total), accent: false),
                .init(label: "Cost / session", value: FleetUsageStats.costPerSessionText(total), accent: false),
                .init(label: "Calls / session", value: FleetUsageStats.callsPerSessionText(total), accent: false),
            ])
        }

        if !dash.providers.isEmpty {
            FleetShareCard(
                title: "Providers",
                slices: dash.providers.map {
                    .init(label: $0.provider.capitalized, value: $0.bucket.costUSD ?? Double($0.bucket.totalTokens))
                },
                priced: dash.providers.first?.bucket.costUSD != nil
            )
        }
    }

    @ViewBuilder private func trendTab(_ dash: FleetUsageDashboardResult) -> some View {
        let cells = slice(dash.heatmap)
        card("Daily spend", caption: lensCaption(cells.count)) {
            FleetBarChart(
                cells: cells,
                selected: $selectedDay,
                valueOf: { $0.costUSD ?? Double($0.callCount) },
                priced: cells.contains { $0.costUSD != nil }
            )
        }
        if let day = selectedDay {
            FleetDayDetail(day: day)
        }
        if !dash.weekly.isEmpty {
            card("Weekly", caption: "\(dash.weekly.count) weeks") {
                FleetMetricList(rows: dash.weekly.suffix(12).map {
                    .init(label: $0.weekStart, value: metric($0.bucket), amount: sortValue($0.bucket))
                })
            }
        }
    }

    @ViewBuilder private func calendarTab(_ dash: FleetUsageDashboardResult) -> some View {
        card("Activity", caption: "\(dash.heatmap.count) active days") {
            VStack(alignment: .leading, spacing: 8) {
                FleetHeatmapGrid(cells: dash.heatmap, selected: $selectedDay)
                FleetHeatmapLegend().padding(.leading, 30)
            }
        }
        if let day = selectedDay {
            FleetDayDetail(day: day)
        }
        let stats = FleetUsageStats(heatmap: dash.heatmap)
        FleetStatStrip(tiles: [
            .init(label: "Active days", value: "\(dash.heatmap.count)", accent: false),
            .init(label: "Busiest weekday", value: stats.busiestWeekday, accent: false),
            .init(label: "Longest streak", value: stats.longestStreakText, accent: false),
        ])
    }

    @ViewBuilder private func forecastTab(_ dash: FleetUsageDashboardResult) -> some View {
        if let f = dash.forecast {
            card("30-day forecast", caption: "from \(f.sampleDays)d sample") {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(alignment: .firstTextBaseline, spacing: 18) {
                        FleetBigStat(
                            value: f.projected30dCostUSD.map(FleetFormat.currency) ?? FleetFormat.tokens(f.projected30dTokens),
                            caption: "projected"
                        )
                        FleetBigStat(
                            value: f.avgDailyCostUSD.map(FleetFormat.currency) ?? FleetFormat.tokens(f.avgDailyTokens),
                            caption: "per day",
                            muted: true
                        )
                    }
                    let stats = FleetUsageStats(heatmap: dash.heatmap)
                    FleetStatStrip(tiles: [
                        .init(label: "Yesterday", value: stats.yesterdayText, accent: false),
                        .init(label: "Last 7d", value: stats.lastSevenText, accent: false),
                        .init(label: "Peak day", value: stats.peakCostText, accent: false),
                    ])
                }
            }
        } else {
            emptyCard("No forecast yet", detail: "It needs at least one day of recent activity.")
        }
        if let total = dash.totals { heroCard(total, state: dash) }
    }

    @ViewBuilder private func projectsTab(_ dash: FleetUsageDashboardResult) -> some View {
        breakdown("Projects", rows: dash.projects.map {
            .init(
                label: FleetProjectLabel.display($0.project, repo: $0.repo),
                value: metric($0.bucket),
                amount: sortValue($0.bucket),
                secondary: FleetFormat.count($0.bucket.callCount, unit: "call")
            )
        })
        breakdown("Branches", rows: dash.branches.map {
            .init(
                label: $0.branch,
                value: metric($0.bucket),
                amount: sortValue($0.bucket),
                secondary: FleetFormat.count($0.bucket.callCount, unit: "call")
            )
        })
    }

    @ViewBuilder private func sessionsTab(_ dash: FleetUsageDashboardResult) -> some View {
        breakdown("Sessions", rows: dash.sessions.map {
            .init(
                label: FleetProjectLabel.display($0.project, repo: nil),
                value: metric($0.bucket),
                amount: sortValue($0.bucket),
                secondary: $0.provider
            )
        })
        breakdown("Models", rows: dash.models.map {
            .init(
                label: $0.model,
                value: metric($0.bucket),
                amount: sortValue($0.bucket),
                secondary: FleetFormat.count($0.bucket.callCount, unit: "call")
            )
        })
    }

    @ViewBuilder private func commandsTab(_ dash: FleetUsageDashboardResult) -> some View {
        breakdown("Shell commands", rows: dash.shellCommands.map {
            .init(label: $0.name, value: FleetFormat.count($0.callCount, unit: "call"), amount: Double($0.callCount))
        })
        breakdown("Tools", rows: dash.tools.map {
            .init(label: $0.name, value: FleetFormat.count($0.callCount, unit: "call"), amount: Double($0.callCount))
        })
        if dash.mcpServers.isEmpty {
            emptyCard("No MCP servers", detail: "No mcp__ prefixed tool calls in this window.")
        } else {
            breakdown("MCP servers", rows: dash.mcpServers.map {
                .init(label: $0.name, value: FleetFormat.count($0.callCount, unit: "call"), amount: Double($0.callCount))
            })
        }
    }

    // MARK: - Building blocks

    private func heroCard(_ total: FleetUsageBucket, state dash: FleetUsageDashboardResult) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline) {
                Text(total.costUSD.map(FleetFormat.currency) ?? FleetFormat.tokens(total.totalTokens))
                    .font(.system(size: 34, weight: .bold, design: .rounded))
                    .monospacedDigit()
                    .foregroundStyle(dash.costComplete ? FleetNotchPalette.mint : .orange)
                Spacer()
                VStack(alignment: .trailing, spacing: 2) {
                    Text(FleetFormat.count(total.callCount, unit: "call"))
                    Text(FleetFormat.count(total.sessionCount, unit: "session"))
                    Text(FleetFormat.count(total.projectCount, unit: "project"))
                }
                .font(.caption)
                .monospacedDigit()
                .foregroundStyle(FleetNotchPalette.muted)
            }
            Text(dash.costComplete ? "Canonical provider cost, 53 weeks" : "Partial: some calls have no canonical price")
                .font(.caption2)
                .foregroundStyle(dash.costComplete ? FleetNotchPalette.muted : .orange)
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(FleetNotchPalette.detail, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
    }

    @ViewBuilder private func quotaCard(_ summary: FleetQuotaSummaryResult) -> some View {
        if !summary.providers.isEmpty {
            card("Live quota", caption: summary.state == .ready ? "live" : summary.state.rawValue) {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(summary.providers, id: \.provider) { p in
                        HStack(spacing: 10) {
                            Text(p.provider.capitalized)
                                .font(.caption.weight(.semibold))
                                .frame(width: 54, alignment: .leading)
                            quotaBar("5h", p.fiveHour)
                            quotaBar("wk", p.sevenDay)
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder private func quotaBar(_ label: String, _ window: FleetQuotaWindow?) -> some View {
        if let window {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 4) {
                    Text(label).font(.caption2).foregroundStyle(FleetNotchPalette.muted)
                    Text("\(window.remainingPercent)%")
                        .font(.caption2.weight(.semibold))
                        .monospacedDigit()
                    if window.estimated {
                        // Inferred from local transcripts, not reported by the
                        // provider. Presenting it identically to a measured
                        // figure would overstate what we know.
                        Image(systemName: "questionmark.circle")
                            .font(.system(size: 8))
                            .foregroundStyle(FleetNotchPalette.muted)
                            .help("Estimated from local transcripts, not reported by the provider")
                    }
                }
                FleetProgressBar(fraction: Double(window.remainingPercent) / 100.0)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        } else {
            Spacer().frame(maxWidth: .infinity)
        }
    }

    @ViewBuilder private func breakdown(_ title: String, rows: [FleetMetricList.Row]) -> some View {
        if !rows.isEmpty {
            card(title, caption: lens == .all ? "53 weeks" : "53 weeks (window fixed)") {
                FleetMetricList(rows: rows)
            }
        }
    }

    private func card<Content: View>(
        _ title: String,
        caption: String? = nil,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline) {
                Text(title).font(.subheadline.weight(.semibold))
                Spacer()
                if let caption {
                    Text(caption).font(.caption2).foregroundStyle(FleetNotchPalette.muted)
                }
            }
            content()
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(FleetNotchPalette.detail, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }

    private func emptyCard(_ title: String, detail: String) -> some View {
        card(title) {
            Text(detail).font(.caption).foregroundStyle(FleetNotchPalette.muted)
        }
    }

    private func unavailable(_ title: String, detail: String) -> some View {
        VStack(spacing: 8) {
            Image(systemName: "chart.bar.xaxis").font(.title2)
            Text(title).font(.headline)
            Text(detail).font(.caption).foregroundStyle(FleetNotchPalette.muted)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    // MARK: - Helpers

    private func slice(_ cells: [FleetHeatmapCell]) -> [FleetHeatmapCell] {
        guard let days = lens.days else { return cells }
        return Array(cells.suffix(days))
    }

    private func lensCaption(_ count: Int) -> String {
        lens.days == nil ? "\(count) active days" : "last \(lens.title)"
    }

    private func metric(_ bucket: FleetUsageBucket) -> String {
        bucket.costUSD.map(FleetFormat.currency) ?? FleetFormat.tokens(bucket.totalTokens)
    }

    private func sortValue(_ bucket: FleetUsageBucket) -> Double {
        bucket.costUSD ?? Double(bucket.totalTokens)
    }
}

private extension UsageTab {
    /// Only the tabs driven by the daily series can honour a window client-side.
    var showsLens: Bool { self == .trend }
}

// MARK: - Usage widgets
//
// Hand-rolled rather than Swift Charts, so there is no macOS version gate and
// every widget fits the notch's tight type scale. The shapes are deliberately
// simple: a rounded rect whose length is a fraction of the section maximum
// carries almost all the meaning in a panel this narrow.

/// Shared number formatting.
///
/// The hero previously rendered `$36147.86` because `String(format:)` applies
/// no grouping separator. On the headline figure that is the difference between
/// a number you can read at a glance and one you have to count digits in.
enum FleetFormat {
    /// Always a bare "$", never the locale's "US$".
    ///
    /// `.currency(code: "USD")` renders "US$" outside a US locale, which is
    /// both wrong for this product and inconsistent with the compact form
    /// beside it, so the symbol is fixed and only the digits are localised.
    static func currency(_ value: Double) -> String {
        "$" + value.formatted(.number.precision(.fractionLength(2)))
    }

    static func compactCurrency(_ value: Double) -> String {
        value < 1000
            ? currency(value)
            : "$" + value.formatted(.number.notation(.compactName).precision(.fractionLength(1)))
    }

    static func tokens(_ value: UInt64) -> String {
        value.formatted(.number.notation(.compactName)) + " tokens"
    }

    static func count(_ value: UInt64, unit: String) -> String {
        "\(value.formatted(.number)) \(value == 1 ? unit : unit + "s")"
    }
}

/// A scrolling pill row, in place of a segmented Picker.
///
/// A `.segmented` Picker distributes width evenly and squeezes labels until they
/// wrap, which is how "Mode" became "Mod / e". Pills size to their content and
/// scroll, so the tab list can grow without the labels degrading.
private struct FleetPillSwitcher<Option: Hashable & Identifiable>: View {
    @Binding var selection: Option
    let options: [Option]
    let title: (Option) -> String

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 4) {
                ForEach(options) { option in
                    let isSelected = option == selection
                    Button { selection = option } label: {
                        Text(title(option))
                            .font(.caption.weight(isSelected ? .semibold : .regular))
                            .foregroundStyle(isSelected ? Color.black : Color.primary)
                            .padding(.horizontal, 10)
                            .padding(.vertical, 5)
                            .background(
                                isSelected ? FleetNotchPalette.mint : FleetNotchPalette.control,
                                in: Capsule()
                            )
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("fleet.usage.tab.\(title(option))")
                }
            }
            .padding(.vertical, 1)
        }
    }
}

/// A labelled row with an inline bar scaled to the section maximum.
///
/// This replaces the old `[String]` rows. Passing a formatted string meant the
/// view had nothing left to draw with, which is why every breakdown rendered as
/// flat text; keeping the magnitude alongside the label is what turns nine
/// lists into nine charts.
private struct FleetMetricList: View {
    struct Row: Identifiable {
        let id = UUID()
        let label: String
        let value: String
        let amount: Double
        var secondary: String?
    }

    let rows: [Row]

    var body: some View {
        let maximum = rows.map(\.amount).max() ?? 0
        VStack(spacing: 5) {
            ForEach(rows) { row in
                HStack(spacing: 8) {
                    Text(row.label)
                        .font(.caption)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .help(row.label)

                    FleetBar(fraction: maximum > 0 ? row.amount / maximum : 0)
                        .frame(width: 84)

                    VStack(alignment: .trailing, spacing: 0) {
                        Text(row.value)
                            .font(.caption.weight(.medium))
                            .monospacedDigit()
                        if let secondary = row.secondary {
                            Text(secondary)
                                .font(.system(size: 9))
                                .foregroundStyle(FleetNotchPalette.muted)
                        }
                    }
                    .frame(width: 96, alignment: .trailing)
                }
            }
        }
    }
}

/// The bar itself: a track with a fill whose opacity also rises with the value,
/// so ranking survives even where the lengths are close.
private struct FleetBar: View {
    let fraction: Double

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(Color.white.opacity(0.06))
                Capsule()
                    .fill(FleetNotchPalette.mint.opacity(0.42 + min(max(fraction, 0), 1) * 0.48))
                    .frame(width: max(2, geo.size.width * min(max(fraction, 0), 1)))
            }
        }
        .frame(height: 6)
    }
}

private struct FleetProgressBar: View {
    let fraction: Double

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(Color.white.opacity(0.08))
                Capsule()
                    .fill(colour)
                    .frame(width: max(2, geo.size.width * min(max(fraction, 0), 1)))
            }
        }
        .frame(height: 5)
    }

    /// Headroom, not usage: green when there is plenty left, orange when the
    /// window is nearly spent.
    private var colour: Color {
        switch fraction {
        case ..<0.15: .red
        case ..<0.35: .orange
        default: FleetNotchPalette.mint
        }
    }
}

private struct FleetStatStrip: View {
    struct Tile: Identifiable {
        let id = UUID()
        let label: String
        let value: String
        let accent: Bool
    }

    let tiles: [Tile]

    var body: some View {
        HStack(spacing: 8) {
            ForEach(tiles) { tile in
                VStack(alignment: .leading, spacing: 2) {
                    Text(tile.label)
                        .font(.system(size: 9.5))
                        .foregroundStyle(FleetNotchPalette.muted)
                    Text(tile.value)
                        .font(.system(size: 15, weight: .semibold, design: .rounded))
                        .monospacedDigit()
                        .foregroundStyle(tile.accent ? FleetNotchPalette.mint : Color.primary)
                        .lineLimit(1)
                        .minimumScaleFactor(0.7)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(10)
                .background(FleetNotchPalette.detail, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            }
        }
    }
}

private struct FleetBigStat: View {
    let value: String
    let caption: String
    var muted: Bool = false

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(value)
                .font(.system(size: muted ? 17 : 24, weight: .bold, design: .rounded))
                .monospacedDigit()
                .foregroundStyle(muted ? Color.primary : FleetNotchPalette.mint)
            Text(caption)
                .font(.system(size: 9.5))
                .foregroundStyle(FleetNotchPalette.muted)
        }
    }
}

/// Vertical bars for the daily series, with click-to-inspect.
private struct FleetBarChart: View {
    let cells: [FleetHeatmapCell]
    @Binding var selected: FleetHeatmapCell?
    let valueOf: (FleetHeatmapCell) -> Double
    let priced: Bool

    private let height: CGFloat = 68

    var body: some View {
        let maximum = cells.map(valueOf).max() ?? 0
        HStack(alignment: .bottom, spacing: 1.5) {
            ForEach(cells, id: \.date) { cell in
                let fraction = maximum > 0 ? valueOf(cell) / maximum : 0
                RoundedRectangle(cornerRadius: 1.5)
                    .fill(colour(for: cell, fraction: fraction))
                    .frame(height: max(2, height * fraction))
                    .frame(maxWidth: .infinity)
                    .contentShape(Rectangle())
                    .onTapGesture { selected = (selected?.date == cell.date) ? nil : cell }
                    .help("\(cell.date): \(priced ? FleetFormat.compactCurrency(valueOf(cell)) : "\(cell.callCount) calls")")
            }
        }
        .frame(height: height, alignment: .bottom)
    }

    private func colour(for cell: FleetHeatmapCell, fraction: Double) -> Color {
        if selected?.date == cell.date { return .white }
        return FleetNotchPalette.mint.opacity(0.42 + min(max(fraction, 0), 1) * 0.48)
    }
}

/// The detail panel for a picked day, so the grids are inspectable rather than
/// decorative.
private struct FleetDayDetail: View {
    let day: FleetHeatmapCell

    var body: some View {
        HStack(spacing: 16) {
            VStack(alignment: .leading, spacing: 1) {
                Text(day.date).font(.caption.weight(.semibold)).monospacedDigit()
                Text("selected day").font(.system(size: 9)).foregroundStyle(FleetNotchPalette.muted)
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 1) {
                Text(day.costUSD.map(FleetFormat.currency) ?? "no price")
                    .font(.caption.weight(.medium))
                    .monospacedDigit()
                Text(FleetFormat.count(day.callCount, unit: "call"))
                    .font(.system(size: 9))
                    .foregroundStyle(FleetNotchPalette.muted)
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(FleetNotchPalette.selected, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
    }
}

/// Contribution grid: week per column, weekday per row, with labels so the
/// axis is legible.
private struct FleetHeatmapGrid: View {
    let cells: [FleetHeatmapCell]
    @Binding var selected: FleetHeatmapCell?

    private let side: CGFloat = 9
    private let gap: CGFloat = 2

    var body: some View {
        let weeks = FleetHeatmapLayout.weekColumns(cells)
        let maximum = cells.map(\.callCount).max() ?? 0
        HStack(alignment: .top, spacing: 4) {
            VStack(alignment: .trailing, spacing: gap) {
                ForEach(0..<7, id: \.self) { row in
                    Text(FleetHeatmapLayout.weekdayLabel(row))
                        .font(.system(size: 7))
                        .foregroundStyle(FleetNotchPalette.muted)
                        .frame(height: side, alignment: .trailing)
                }
            }
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(alignment: .top, spacing: gap) {
                    ForEach(weeks, id: \.weekStart) { week in
                        VStack(spacing: gap) {
                            ForEach(0..<7, id: \.self) { row in
                                cellView(week.days[row], maximum: maximum)
                            }
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder private func cellView(_ cell: FleetHeatmapCell?, maximum: UInt64) -> some View {
        let fraction = (cell.map { maximum > 0 ? Double($0.callCount) / Double(maximum) : 0 }) ?? 0
        RoundedRectangle(cornerRadius: 2)
            .fill(fill(cell, fraction: fraction))
            .frame(width: side, height: side)
            .contentShape(Rectangle())
            .onTapGesture {
                guard let cell else { return }
                selected = (selected?.date == cell.date) ? nil : cell
            }
            .help(cell.map { "\($0.date): \($0.callCount) calls" } ?? "")
    }

    private func fill(_ cell: FleetHeatmapCell?, fraction: Double) -> Color {
        guard let cell else { return Color.clear }
        if selected?.date == cell.date { return .white }
        if cell.callCount == 0 { return FleetNotchPalette.canvas }
        return FleetNotchPalette.mint.opacity(0.25 + fraction * 0.7)
    }
}

private struct FleetHeatmapLegend: View {
    var body: some View {
        HStack(spacing: 4) {
            Text("less").font(.system(size: 8)).foregroundStyle(FleetNotchPalette.muted)
            ForEach([0.0, 0.25, 0.5, 0.75, 1.0], id: \.self) { step in
                RoundedRectangle(cornerRadius: 2)
                    .fill(step == 0 ? FleetNotchPalette.canvas : FleetNotchPalette.mint.opacity(0.25 + step * 0.7))
                    .frame(width: 9, height: 9)
            }
            Text("more").font(.system(size: 8)).foregroundStyle(FleetNotchPalette.muted)
        }
    }
}

/// Share of spend as a donut, for a small number of slices.
///
/// A pie is unreadable past a handful of wedges, so this is only used where the
/// dimension is naturally short, which in practice means providers.
private struct FleetShareCard: View {
    struct Slice: Identifiable {
        let id = UUID()
        let label: String
        let value: Double
    }

    let title: String
    let slices: [Slice]
    let priced: Bool

    var body: some View {
        let total = slices.map(\.value).reduce(0, +)
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.subheadline.weight(.semibold))
            HStack(spacing: 14) {
                FleetDonut(slices: slices, total: total)
                    .frame(width: 82, height: 82)
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(slices.enumerated()), id: \.element.id) { index, slice in
                        HStack(spacing: 6) {
                            Circle()
                                .fill(FleetDonut.colour(index))
                                .frame(width: 7, height: 7)
                            Text(slice.label).font(.caption)
                            Spacer(minLength: 6)
                            Text(total > 0 ? "\(Int((slice.value / total * 100).rounded()))%" : "0%")
                                .font(.caption.weight(.medium))
                                .monospacedDigit()
                                .foregroundStyle(FleetNotchPalette.muted)
                            Text(priced ? FleetFormat.compactCurrency(slice.value) : "")
                                .font(.caption.weight(.medium))
                                .monospacedDigit()
                                .frame(width: 62, alignment: .trailing)
                        }
                    }
                }
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(FleetNotchPalette.detail, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }
}

private struct FleetDonut: View {
    let slices: [FleetShareCard.Slice]
    let total: Double

    static func colour(_ index: Int) -> Color {
        let palette: [Color] = [FleetNotchPalette.mint, .cyan, .purple, .orange, .pink, .yellow]
        return palette[index % palette.count]
    }

    var body: some View {
        Canvas { context, size in
            guard total > 0 else { return }
            let rect = CGRect(origin: .zero, size: size).insetBy(dx: 2, dy: 2)
            let centre = CGPoint(x: rect.midX, y: rect.midY)
            let radius = min(rect.width, rect.height) / 2
            var start = Angle.degrees(-90)
            for (index, slice) in slices.enumerated() {
                let sweep = Angle.degrees(slice.value / total * 360)
                var path = Path()
                path.move(to: centre)
                path.addArc(center: centre, radius: radius, startAngle: start, endAngle: start + sweep, clockwise: false)
                path.closeSubpath()
                context.fill(path, with: .color(Self.colour(index)))
                start = start + sweep
            }
            // Punch the middle out so it reads as a donut rather than a pie.
            var hole = Path()
            hole.addEllipse(in: CGRect(
                x: centre.x - radius * 0.58,
                y: centre.y - radius * 0.58,
                width: radius * 1.16,
                height: radius * 1.16
            ))
            context.blendMode = .destinationOut
            context.fill(hole, with: .color(.black))
        }
    }
}

/// Statistics derived entirely from the daily cells already on the wire, so
/// none of these tiles cost a round trip or a protocol change.
private struct FleetUsageStats {
    let heatmap: [FleetHeatmapCell]

    private var priced: Bool { heatmap.contains { $0.costUSD != nil } }

    var peakCostText: String {
        guard let peak = heatmap.max(by: { value($0) < value($1) }) else { return "\u{2014}" }
        return priced ? FleetFormat.compactCurrency(value(peak)) : "\(peak.callCount)"
    }

    var averageActiveText: String {
        guard !heatmap.isEmpty else { return "\u{2014}" }
        let mean = heatmap.map(value).reduce(0, +) / Double(heatmap.count)
        return priced ? FleetFormat.compactCurrency(mean) : String(Int(mean.rounded()))
    }

    var yesterdayText: String { dayText(offset: 1) }

    var lastSevenText: String {
        let recent = heatmap.suffix(7).map(value).reduce(0, +)
        return priced ? FleetFormat.compactCurrency(recent) : String(Int(recent.rounded()))
    }

    var streakText: String { "\(currentStreak)d" }

    var longestStreakText: String { "\(longestStreak)d" }

    var busiestWeekday: String {
        var totals = [Double](repeating: 0, count: 7)
        for cell in heatmap {
            guard let date = FleetHeatmapLayout.formatter.date(from: cell.date) else { continue }
            let weekday = FleetHeatmapLayout.calendar.component(.weekday, from: date)
            totals[(weekday + 5) % 7] += value(cell)
        }
        guard let best = totals.enumerated().max(by: { $0.element < $1.element }), best.element > 0 else {
            return "\u{2014}"
        }
        return FleetHeatmapLayout.weekdayLabel(best.offset)
    }

    /// Share of input that came from cache rather than fresh tokens.
    static func cacheHitText(_ bucket: FleetUsageBucket) -> String {
        let considered = bucket.inputTokens + bucket.cacheCreationTokens + bucket.cacheReadTokens
        guard considered > 0 else { return "\u{2014}" }
        let ratio = Double(bucket.cacheReadTokens) / Double(considered)
        return "\(Int((ratio * 100).rounded()))%"
    }

    static func costPerSessionText(_ bucket: FleetUsageBucket) -> String {
        guard bucket.sessionCount > 0, let cost = bucket.costUSD else { return "\u{2014}" }
        return FleetFormat.compactCurrency(cost / Double(bucket.sessionCount))
    }

    static func callsPerSessionText(_ bucket: FleetUsageBucket) -> String {
        guard bucket.sessionCount > 0 else { return "\u{2014}" }
        return (bucket.callCount / bucket.sessionCount).formatted(.number)
    }

    private func value(_ cell: FleetHeatmapCell) -> Double { cell.costUSD ?? Double(cell.callCount) }

    private func dayText(offset: Int) -> String {
        let index = heatmap.count - 1 - offset
        guard heatmap.indices.contains(index) else { return "\u{2014}" }
        let cell = heatmap[index]
        return priced ? FleetFormat.compactCurrency(value(cell)) : "\(cell.callCount)"
    }

    /// Streaks walk actual calendar dates, since the series is sparse: counting
    /// consecutive ENTRIES would treat a three-week gap as an unbroken run.
    private var dates: [Date] {
        heatmap.compactMap { FleetHeatmapLayout.formatter.date(from: $0.date) }.sorted()
    }

    private var currentStreak: Int {
        guard let last = dates.last else { return 0 }
        var streak = 1
        var cursor = last
        for date in dates.dropLast().reversed() {
            guard let previous = FleetHeatmapLayout.calendar.date(byAdding: .day, value: -1, to: cursor),
                  FleetHeatmapLayout.calendar.isDate(date, inSameDayAs: previous) else { break }
            streak += 1
            cursor = date
        }
        return streak
    }

    private var longestStreak: Int {
        var best = 0
        var run = 0
        var previous: Date?
        for date in dates {
            if let previous,
               let next = FleetHeatmapLayout.calendar.date(byAdding: .day, value: 1, to: previous),
               FleetHeatmapLayout.calendar.isDate(date, inSameDayAs: next) {
                run += 1
            } else {
                run = 1
            }
            best = max(best, run)
            previous = date
        }
        return best
    }
}
/// Says WHY the roster is empty, and offers the action that fixes it.
///
/// "No matching sessions" was true but useless: it did not distinguish a
/// daemon that is not answering from a filter that excludes everything, and it
/// offered no way out. A persisted `attentionOnly` is the common case, because
/// the toggle lives in the separate window while the filter it sets also
/// governs this one, so the notch could sit permanently empty with no visible
/// control to clear it.
struct FleetRosterEmptyState: View {
    let connection: FleetConnectionState
    let total: Int
    let filters: FleetRosterFilters
    let retry: () -> Void
    let clearFilters: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(title, systemImage: symbol)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(connection.isLive ? Color.primary : .orange)

            Text(detail)
                .font(.caption)
                .foregroundStyle(FleetNotchPalette.muted)
                .fixedSize(horizontal: false, vertical: true)

            if !connection.isLive {
                Button("Retry connection", action: retry)
                    .controlSize(.small)
            } else if hiddenByFilters {
                Button("Show all sessions", action: clearFilters)
                    .controlSize(.small)
                    .accessibilityIdentifier("fleet.roster.clear-filters")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(FleetNotchPalette.detail, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .padding(.vertical, 8)
    }

    /// Sessions exist, but every one of them is filtered out.
    private var hiddenByFilters: Bool { total > 0 }

    private var symbol: String {
        if !connection.isLive { return "exclamationmark.triangle" }
        return hiddenByFilters ? "line.3.horizontal.decrease.circle" : "moon.zzz"
    }

    private var title: String {
        if !connection.isLive { return "Not connected to the Hangar daemon" }
        if hiddenByFilters { return "\(total) \(total == 1 ? "session" : "sessions") hidden by filters" }
        return "No sessions yet"
    }

    private var detail: String {
        if !connection.isLive {
            return "\(connection.message)\n\nStart it with `ainb hangar daemon run`, or retry once it is up."
        }
        if hiddenByFilters {
            return "Nothing matches the active filter\(activeFilters.count == 1 ? "" : "s"): \(activeFilters.joined(separator: ", "))."
        }
        return "The daemon is connected and reports no sessions. Start one with `ainb run`, and it will appear here."
    }

    private var activeFilters: [String] { FleetRosterEmptyState.activeFilterNames(filters) }

    /// Named explicitly, because a filter the operator cannot see is the whole
    /// reason this state was confusing. Static so it can be tested without
    /// standing up a view.
    static func activeFilterNames(_ filters: FleetRosterFilters) -> [String] {
        var active: [String] = []
        if filters.attentionOnly { active.append("needs you") }
        if filters.focus != .all { active.append("focus \(filters.focus.label.lowercased())") }
        if let lifecycle = filters.lifecycle { active.append("lifecycle \(lifecycle.rawValue.lowercased())") }
        if let provider = filters.provider { active.append("provider \(provider.rawValue)") }
        if let management = filters.management { active.append("management \(management.rawValue.lowercased())") }
        if let transport = filters.transportHealth { active.append("transport \(transport.rawValue.lowercased())") }
        // No structural filter is set, so a search term is the only thing left
        // that can be hiding rows.
        return active.isEmpty ? ["a search term"] : active
    }
}

/// Turns the scanner's project key into something readable in a narrow panel.
///
/// That key is the project's absolute path with the separators mangled to
/// dashes, so it arrives looking like
/// `Users-someone-.agents-in-a-box-worktrees-by-name-proj--f-thing--96da95da`.
/// Rendered raw it is unreadable at panel width, every row shares a long
/// identical prefix, and it puts the operator's username and whole directory
/// layout on screen.
///
/// The client knows its OWN home directory, so it can mangle that the same way
/// and strip it. What remains is the part that actually distinguishes one
/// project from another. This is presentation only: the wire still carries the
/// full key, which is tracked separately.
enum FleetProjectLabel {
    static func display(_ project: String, repo: String?, home: URL = FileManager.default.homeDirectoryForCurrentUser) -> String {
        if let repo, !repo.isEmpty { return repo }

        var text = project
        // The mangled home, with and without the leading separator, longest first
        // so the more specific prefix wins.
        let mangledHome = home.path.replacingOccurrences(of: "/", with: "-")
        for prefix in [mangledHome, String(mangledHome.dropFirst())].sorted(by: { $0.count > $1.count }) {
            guard !prefix.isEmpty else { continue }
            if text.hasPrefix(prefix) {
                text.removeFirst(prefix.count)
                break
            }
        }
        text = text.trimmingCharacters(in: CharacterSet(charactersIn: "-."))
        return text.isEmpty ? project : text
    }
}

/// Arranges flat daily cells into contribution-graph columns.
///
/// Pure and non-private so the weekday alignment can be pinned by a test: an
/// off-by-one here silently shifts every day into the wrong row, which looks
/// plausible on screen and is invisible to a compiler.
enum FleetHeatmapLayout {
    struct Week: Equatable {
        let weekStart: String
        /// Monday-first, 7 slots; nil where the window has no cell for that day.
        let days: [FleetHeatmapCell?]
    }

    /// The daemon emits `yyyy-MM-dd` in UTC, so parse in UTC with a fixed
    /// locale -- a device calendar could otherwise shift a day across a boundary.
    static let calendar: Calendar = {
        var calendar = Calendar(identifier: .iso8601)
        calendar.timeZone = TimeZone(secondsFromGMT: 0) ?? .gmt
        return calendar
    }()

    static let formatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.calendar = Calendar(identifier: .iso8601)
        formatter.timeZone = TimeZone(secondsFromGMT: 0) ?? .gmt
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd"
        return formatter
    }()

    /// Columns for a CONTINUOUS run of weeks, not just the weeks that have data.
    ///
    /// The daemon ships a sparse heatmap: only days with calls appear. Emitting
    /// one column per populated week silently deletes idle time, so a real
    /// two-month gap in this corpus (twelve absent weeks across November and
    /// December) rendered as adjacent columns and read like unbroken activity.
    /// The x-axis has to be a calendar for the grid to mean anything, so every
    /// week between the first and last observed one gets a column, populated or
    /// not.
    static func weekColumns(_ cells: [FleetHeatmapCell]) -> [Week] {
        var buckets: [Date: [Int: FleetHeatmapCell]] = [:]
        for cell in cells {
            guard let date = formatter.date(from: cell.date) else { continue }
            // ISO weekday: 1 = Monday ... 7 = Sunday. Map to a 0-based row.
            let weekday = calendar.component(.weekday, from: date)
            let row = (weekday + 5) % 7
            guard let monday = calendar.date(byAdding: .day, value: -row, to: date) else { continue }
            buckets[calendar.startOfDay(for: monday), default: [:]][row] = cell
        }
        guard let first = buckets.keys.min(), let last = buckets.keys.max() else { return [] }

        var weeks: [Week] = []
        var monday = first
        // Bounded by the wire cap so a malformed payload cannot spin here.
        while monday <= last, weeks.count < FleetHeatmapLayout.maxWeeks {
            let byRow = buckets[monday] ?? [:]
            weeks.append(
                Week(weekStart: formatter.string(from: monday), days: (0..<7).map { byRow[$0] })
            )
            guard let next = calendar.date(byAdding: .day, value: 7, to: monday) else { break }
            monday = next
        }
        return weeks
    }

    /// Matches the daemon's 53-week window.
    static let maxWeeks = 53

    /// Row 0 is Monday, matching `weekColumns`. Only alternate rows are
    /// labelled, since seven labels do not fit beside a 9pt cell.
    static func weekdayLabel(_ row: Int) -> String {
        switch row {
        case 0: "Mon"
        case 2: "Wed"
        case 4: "Fri"
        case 6: "Sun"
        default: ""
        }
    }
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
