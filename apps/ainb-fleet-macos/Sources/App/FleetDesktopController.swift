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
final class FleetDesktopController {
    static var shared: FleetDesktopController?

    private let store: FleetStore
    private let presentation: FleetPresentationStore
    private var notchPanel: NSPanel?
    private var fleetWindow: NSWindow?
    private var notificationObserver: NSObjectProtocol?

    init(store: FleetStore, presentation: FleetPresentationStore) {
        self.store = store
        self.presentation = presentation
        notificationObserver = NotificationCenter.default.addObserver(
            forName: .fleetNotificationOpen,
            object: nil,
            queue: .main
        ) { [weak self] notification in
            guard let url = notification.object as? URL else { return }
            Task { @MainActor in self?.open(url) }
        }
    }

    deinit {
        if let notificationObserver {
            NotificationCenter.default.removeObserver(notificationObserver)
        }
    }

    func launch() {
        if notchPanel == nil {
            let size = NSSize(width: 360, height: 58)
            let panel = NSPanel(
                contentRect: NSRect(origin: .zero, size: size),
                styleMask: [.borderless, .nonactivatingPanel],
                backing: .buffered,
                defer: false
            )
            panel.isOpaque = false
            panel.backgroundColor = .clear
            panel.hasShadow = true
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
        if fleetWindow == nil {
            let window = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 980, height: 680),
                styleMask: [.titled, .closable, .miniaturizable, .resizable],
                backing: .buffered,
                defer: false
            )
            window.title = "Fleet"
            window.minSize = NSSize(width: 620, height: 520)
            window.contentView = NSHostingView(rootView: FleetWindowView(store: store, presentation: presentation.binding))
            window.center()
            window.setFrameAutosaveName("AINBFleetWindow")
            fleetWindow = window
        }
        fleetWindow?.makeKeyAndOrderFront(nil)
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

    private func positionNotch() {
        guard let panel = notchPanel,
              let screen = NSScreen.screens.first(where: { $0.frame.contains(NSEvent.mouseLocation) }) ?? NSScreen.main
        else { return }
        let frame = screen.frame
        panel.setFrameOrigin(NSPoint(x: frame.midX - panel.frame.width / 2, y: frame.maxY - panel.frame.height - 2))
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
            HStack(spacing: 10) {
                Image(systemName: FleetStatusPresentation.symbol(for: store.connectionState, needsYou: store.needsYouCount, sessions: store.sessions))
                    .foregroundStyle(store.needsYouCount > 0 ? .orange : .primary)
                Text("Fleet")
                    .fontWeight(.semibold)
                Spacer()
                Text("\(store.activeCount) active")
                if store.needsYouCount > 0 {
                    Text("\(store.needsYouCount) need you")
                        .fontWeight(.semibold)
                        .foregroundStyle(.orange)
                }
            }
            .font(.caption)
            .padding(.horizontal, 18)
            .frame(width: 360, height: 58)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(FleetStatusPresentation.label(active: store.activeCount, needsYou: store.needsYouCount, state: store.connectionState, sessions: store.sessions))
        .accessibilityHint("Open Fleet")
    }
}
