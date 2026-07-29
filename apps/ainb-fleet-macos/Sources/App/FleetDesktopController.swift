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
            panel.contentView = NSHostingView(rootView: FleetNotchView(store: store, openFleet: { [weak self] in
                self?.showFleet()
            }))
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
    let openFleet: () -> Void

    var body: some View {
        Button(action: openFleet) {
            HStack(spacing: 8) {
                Image(systemName: FleetStatusPresentation.symbol(for: store.connectionState, needsYou: store.needsYouCount, sessions: store.sessions))
                    .font(.caption2.weight(.bold))
                    .foregroundStyle(store.needsYouCount > 0 ? .orange : .mint)
                Text("Fleet")
                    .fontWeight(.bold)
                Spacer()
                Text(store.connectionState.isLive ? "\(store.sessions.count) observed" : "Offline")
                if store.needsYouCount > 0 {
                    Text("\(store.needsYouCount) need you")
                        .fontWeight(.semibold)
                        .foregroundStyle(.orange)
                }
            }
            .font(.caption2)
            .padding(.horizontal, 20)
            .frame(width: 320, height: 38)
            .background(FleetNotchShape().fill(.black))
            .contentShape(FleetNotchShape())
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("fleet.notch")
        .accessibilityLabel(FleetStatusPresentation.label(active: store.activeCount, needsYou: store.needsYouCount, state: store.connectionState, sessions: store.sessions))
        .accessibilityHint("Open Fleet")
    }
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
