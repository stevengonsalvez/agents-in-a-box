import SwiftUI

@main
struct AINBFleetApp: App {
    @StateObject private var store: FleetStore
    @State private var presentation: FleetPresentationPreferences
    private let presentationDefaults: UserDefaults

    init() {
        let defaults = Self.presentationDefaults
        let fleetStore = FleetStore(readVersions: Self.testReadVersions)
        _store = StateObject(wrappedValue: fleetStore)
        _presentation = State(initialValue: FleetPresentationPreferences.load(defaults: defaults))
        presentationDefaults = defaults
        Task { @MainActor in fleetStore.start() }
    }

    var body: some Scene {
        standardScenes
    }

    @SceneBuilder
    private var standardScenes: some Scene {
        MenuBarExtra {
            FleetMenuBarView(store: store, openFleet: openFleet)
        } label: {
            Label("Fleet", systemImage: FleetStatusPresentation.symbol(for: store.connectionState, needsYou: store.needsYouCount, sessions: store.sessions))
                .accessibilityLabel(FleetStatusPresentation.label(active: store.activeCount, needsYou: store.needsYouCount, state: store.connectionState, sessions: store.sessions))
                .accessibilityIdentifier("fleet.status-item")
                .task {
                    #if DEBUG
                    guard Self.testLaunchesFleetWindow else { return }
                    openWindow(id: "fleet")
                    #endif
                }
        }
        .menuBarExtraStyle(.window)

        Window("Fleet", id: "fleet") {
            FleetWindowView(store: store, presentation: presentationBinding)
                .onOpenURL(perform: openDeepLink)
        }
        Settings {
            FleetSettingsView(presentation: presentationBinding)
        }
    }

    @Environment(\.openWindow) private var openWindow

    private var presentationBinding: Binding<FleetPresentationPreferences> {
        Binding(
            get: { presentation },
            set: { newValue in
                presentation = newValue
                newValue.save(defaults: presentationDefaults)
            }
        )
    }

    private func openFleet(attentionOnly: Bool = false) {
        if attentionOnly {
            var next = presentation
            next.filters.attentionOnly = true
            presentationBinding.wrappedValue = next
        }
        openWindow(id: "fleet")
    }

    private func openDeepLink(_ url: URL) {
        guard url.scheme == "ainbfleet", url.host == "session", let key = url.pathComponents.last else { return }
        store.selectedSessionKey = key
        openWindow(id: "fleet")
    }

    private static var testReadVersions: FleetProtocolRange {
        #if DEBUG
        if CommandLine.arguments.contains("--fleet-test-read-range=2...2")
            || ProcessInfo.processInfo.environment["AINB_FLEET_TEST_READ_RANGE"] == "2...2" {
            return FleetProtocolRange(min: 2, max: 2)
        }
        #endif
        return FleetProtocolRange(min: 1, max: 1)
    }

    private static var presentationDefaults: UserDefaults {
        #if DEBUG
        guard testLaunchesFleetWindow else { return .standard }
        let suite = "dev.ainb.fleet.xcui"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        return defaults
        #else
        return .standard
        #endif
    }

    #if DEBUG
    private static var testLaunchesFleetWindow: Bool {
        CommandLine.arguments.contains("--fleet-test-open-window")
            || ProcessInfo.processInfo.environment["AINB_FLEET_TEST_OPEN_WINDOW"] == "1"
    }
    #endif
}
