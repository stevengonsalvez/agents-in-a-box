import Foundation
import SwiftUI

enum FleetAppConfiguration {
    static var isUITest: Bool {
        #if DEBUG
        ProcessInfo.processInfo.environment["AINB_FLEET_UI_TEST_MODE"] == "1"
        #else
        false
        #endif
    }

    static var readVersions: FleetProtocolRange {
        #if DEBUG
        // 3...3 excludes the daemon's v2, simulating a read-incompatible client
        // for the protocol-compatibility UI journey.
        if CommandLine.arguments.contains("--fleet-test-read-range=3...3")
            || ProcessInfo.processInfo.environment["AINB_FLEET_TEST_READ_RANGE"] == "3...3" {
            return FleetProtocolRange(min: 3, max: 3)
        }
        #endif
        return FleetProtocolRange(min: 1, max: 2)
    }

    static var presentationDefaults: UserDefaults {
        #if DEBUG
        guard ProcessInfo.processInfo.environment["AINB_FLEET_TEST_ISOLATE_DEFAULTS"] == "1" else { return .standard }
        let suite = "dev.ainb.fleet.xcui"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        return defaults
        #else
        return .standard
        #endif
    }

}

@main
struct AINBFleetApp: App {
    @StateObject private var store: FleetStore
    @StateObject private var presentation: FleetPresentationStore
    @NSApplicationDelegateAdaptor(FleetAppDelegate.self) private var appDelegate
    private let desktop: FleetDesktopController

    init() {
        let store = FleetStore(readVersions: FleetAppConfiguration.readVersions)
        let presentation = FleetPresentationStore(defaults: FleetAppConfiguration.presentationDefaults)
        let desktop = FleetDesktopController(store: store, presentation: presentation)
        _store = StateObject(wrappedValue: store)
        _presentation = StateObject(wrappedValue: presentation)
        self.desktop = desktop
        FleetDesktopController.shared = desktop
        Task { @MainActor in
            store.start()
            desktop.launch()
            if FleetAppConfiguration.isUITest {
                NSApp.activate(ignoringOtherApps: true)
            }
        }
    }

    var body: some Scene {
        Settings { EmptyView() }
    }
}
