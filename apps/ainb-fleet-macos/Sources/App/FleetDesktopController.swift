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

@MainActor
final class FleetDesktopController: NSObject, NSWindowDelegate {
    static var shared: FleetDesktopController?

    private let store: FleetStore
    private let presentation: FleetPresentationStore
    private var notchPanel: NSPanel?
    private var fleetWindow: NSWindow?

    init(store: FleetStore, presentation: FleetPresentationStore) {
        self.store = store
        self.presentation = presentation
        super.init()
    }

    func launch() {
        if notchPanel == nil {
            let size = NSSize(width: 320, height: 38)
            let panel = NSPanel(
                contentRect: NSRect(origin: .zero, size: size),
                styleMask: [.borderless, .nonactivatingPanel],
                backing: .buffered,
                defer: false
            )
            panel.isOpaque = false
            panel.backgroundColor = .clear
            panel.hasShadow = false
            panel.level = .statusBar
            panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
            panel.contentView = NSHostingView(rootView: FleetNotchView(
                store: store,
                presentation: presentation,
                setExpanded: { [weak self] in self?.setNotchExpanded($0) },
                openFleet: { [weak self] in
                    self?.setNotchExpanded(false)
                    self?.showFleet()
                }
            ))
            notchPanel = panel
        }
        positionNotch()
        notchPanel?.orderFrontRegardless()
    }

    func showFleet() {
        NSApp.setActivationPolicy(.regular)
        if fleetWindow == nil {
            let window = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 980, height: 680),
                styleMask: [.titled, .closable, .miniaturizable, .resizable],
                backing: .buffered,
                defer: false
            )
            window.title = "Fleet"
            window.minSize = NSSize(width: 620, height: 520)
            window.level = .floating
            window.delegate = self
            window.contentView = NSHostingView(rootView: FleetWindowView(store: store, presentation: presentation.binding))
            window.center()
            window.setFrameAutosaveName("AINBFleetWindow")
            fleetWindow = window
        }
        fleetWindow?.makeKeyAndOrderFront(nil)
        fleetWindow?.orderFrontRegardless()
        NSApp.activate(ignoringOtherApps: true)
    }

    private func setNotchExpanded(_ expanded: Bool) {
        guard let panel = notchPanel else { return }
        panel.setContentSize(NSSize(width: expanded ? 640 : 320, height: expanded ? 620 : 38))
        positionNotch()
    }

    func open(_ url: URL) {
        guard url.scheme == "ainbfleet",
              url.host == "session",
              let encodedPath = URLComponents(url: url, resolvingAgainstBaseURL: false)?.percentEncodedPath,
              let key = String(encodedPath.dropFirst()).removingPercentEncoding,
              !key.isEmpty else { return }
        store.refresh()
        store.selectedSessionKey = key
        showFleet()
    }

    func windowWillClose(_ notification: Notification) {
        guard let closingWindow = notification.object as? NSWindow,
              closingWindow === fleetWindow else { return }
        fleetWindow = nil
        NSApp.setActivationPolicy(.accessory)
    }

    private func positionNotch() {
        guard let panel = notchPanel,
              let screen = NSScreen.screens.first(where: { $0.frame.contains(NSEvent.mouseLocation) }) ?? NSScreen.main
        else { return }
        let frame = screen.frame
        panel.setFrameOrigin(NSPoint(x: frame.midX - panel.frame.width / 2, y: frame.maxY - panel.frame.height))
    }
}

final class FleetAppDelegate: NSObject, NSApplicationDelegate {
    func application(_ application: NSApplication, open urls: [URL]) {
        Task { @MainActor in
            urls.forEach { FleetDesktopController.shared?.open($0) }
        }
    }
}

private struct FleetNotchView: View {
    @ObservedObject var store: FleetStore
    @ObservedObject var presentation: FleetPresentationStore
    let setExpanded: (Bool) -> Void
    let openFleet: () -> Void
    @State private var isExpanded = false
    @State private var search = ""
    @State private var interviewPresented = false

    private var visibleSessions: [FleetSession] {
        FleetRosterPresentation.visibleSessions(
            store.sessions,
            search: search,
            filters: presentation.preferences.filters,
            sort: presentation.preferences.sort
        )
    }

    private var selectedSession: FleetSession? {
        guard let key = store.selectedSessionKey else { return visibleSessions.first }
        return visibleSessions.first(where: { $0.sessionKey == key }) ?? visibleSessions.first
    }

    var body: some View {
        VStack(spacing: 0) {
            Button(action: toggleExpanded) {
                header
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("fleet.notch")
            .accessibilityLabel(FleetStatusPresentation.label(active: store.activeCount, needsYou: store.needsYouCount, state: store.connectionState, sessions: store.sessions))
            .accessibilityHint(isExpanded ? "Collapse Fleet controls" : "Expand Fleet controls")

            if isExpanded {
                expandedContent
                    .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .frame(width: isExpanded ? 640 : 320, alignment: .top)
        .background(FleetNotchPalette.canvas, in: FleetNotchShape())
        .onChange(of: isExpanded) { _, value in setExpanded(value) }
        .onChange(of: visibleSessions.map(\.sessionKey)) { _, _ in selectFirstVisibleSession() }
        .onChange(of: presentation.preferences.filters) { _, _ in selectFirstVisibleSession() }
        .onAppear(perform: selectFirstVisibleSession)
        .sheet(isPresented: $interviewPresented) { FleetAnswerQueue(store: store) }
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
            Image(systemName: isExpanded ? "chevron.up" : "chevron.down")
                .font(.caption2.weight(.bold))
                .foregroundStyle(FleetNotchPalette.muted)
        }
        .font(.caption)
        .padding(.horizontal, 20)
        .frame(height: 38)
        .contentShape(Rectangle())
    }

    private var expandedContent: some View {
        VStack(alignment: .leading, spacing: 14) {
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
                Button(action: toggleExpanded) { Image(systemName: "xmark") }
                    .accessibilityLabel("Close Fleet controls")
            }

            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 7) {
                    ForEach(FleetRosterFocus.allCases) { focus in
                        FleetNotchFocusChip(
                            focus: focus,
                            selected: presentation.preferences.filters.focus == focus
                        ) {
                            toggleFocus(focus)
                        }
                    }
                }
            }

            HStack {
                Text("\(visibleSessions.count) of \(store.sessions.count) sessions")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(FleetNotchPalette.muted)
                Spacer()
                if !store.connectionState.isLive {
                    Button("Retry") { store.retry() }
                }
            }

            ScrollView {
                LazyVStack(spacing: 6) {
                    ForEach(visibleSessions, id: \.sessionKey) { session in
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
                            interviewPresented = true
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

            HStack {
                Button("Show all \(store.sessions.count) sessions", action: openFleet)
                    .buttonStyle(.bordered)
                    .accessibilityIdentifier("fleet.notch.show-all")
                Spacer()
                Button("Expand to Fleet", action: openFleet)
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier("fleet.notch.expand-fleet")
            }
        }
        .padding(16)
        .frame(height: 582, alignment: .top)
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
        withAnimation(.easeInOut(duration: 0.16)) { isExpanded.toggle() }
    }

    private func selectFirstVisibleSession() {
        guard !visibleSessions.contains(where: { $0.sessionKey == store.selectedSessionKey }) else { return }
        store.selectedSessionKey = visibleSessions.first?.sessionKey
    }

    private func toggleFocus(_ focus: FleetRosterFocus) {
        if presentation.preferences.filters.focus == focus, focus != .all {
            presentation.preferences.filters.focus = .all
        } else {
            presentation.preferences.filters.focus = focus
        }
    }
}

private struct FleetNotchFocusChip: View {
    let focus: FleetRosterFocus
    let selected: Bool
    let toggle: () -> Void

    var body: some View {
        Button(action: toggle) {
            Text(focus.label)
        }
        .buttonStyle(.bordered)
        .tint(selected ? FleetNotchPalette.mint : .gray)
        .accessibilityIdentifier("fleet.notch.filter.\(focus.rawValue)")
    }
}

private struct FleetNotchSessionRow: View {
    let session: FleetSession
    let selected: Bool
    let connection: FleetConnectionState
    let select: () -> Void

    var body: some View {
        Button(action: select) {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: providerSymbol)
                    .font(.title3)
                    .foregroundStyle(providerColor)
                    .frame(width: 25)
                VStack(alignment: .leading, spacing: 3) {
                    Text(FleetRosterPresentation.sessionIdentity(for: session).repository)
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
        .accessibilityLabel(session.displayName ?? session.sessionKey)
        .accessibilityValue(FleetRosterPresentation.semanticStatus(for: session, connection: connection))
    }

    private var metadata: String {
        let identity = FleetRosterPresentation.sessionIdentity(for: session)
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

    private var providerSymbol: String {
        switch session.provider {
        case .claude: "sparkles"
        case .codex: "brain.head.profile"
        case .copilot: "cursorarrow.rays"
        case .acp: "point.3.connected.trianglepath.dotted"
        case .unknown: "terminal"
        }
    }

    private var providerColor: Color {
        switch session.provider {
        case .claude: .orange
        case .codex: .cyan
        case .copilot: .purple
        case .acp: .mint
        case .unknown: FleetNotchPalette.muted
        }
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
                        Button("Bypass session") { store.decideApproval(.bypassSession, on: session) }
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
