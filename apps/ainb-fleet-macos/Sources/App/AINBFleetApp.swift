import SwiftUI

@main
struct AINBFleetApp: App {
    @StateObject private var store: FleetStore
    @StateObject private var presentation: FleetPresentationStore
    @NSApplicationDelegateAdaptor(FleetAppDelegate.self) private var appDelegate
    private let desktop: FleetDesktopController

    init() {
        let defaults = Self.presentationDefaults
        let fleetStore = FleetStore(readVersions: Self.testReadVersions)
        let presentationStore = FleetPresentationStore(defaults: defaults)
        _store = StateObject(wrappedValue: fleetStore)
        _presentation = StateObject(wrappedValue: presentationStore)
        let desktopController = FleetDesktopController(store: fleetStore, presentation: presentationStore)
        desktop = desktopController
        FleetDesktopController.shared = desktopController
        Task { @MainActor in
            fleetStore.start()
            desktopController.launch()
            #if DEBUG
            if Self.testLaunchesFleetWindow {
                desktopController.showFleet()
            }
            #endif
        }
    }

    var body: some Scene {
        Settings {
            FleetSettingsView(presentation: presentation.binding)
        }
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
