import XCTest

final class OfflineAndReconnectJourneyTests: FleetUITestCase {
    @MainActor
    func testRealFixtureDisconnectShowsRetryWithoutLaunchingDaemon() throws {
        try fixture.seed(eventID: "alpha-running", sessionID: "alpha", eventType: "UserPromptSubmit", observedAt: 1_700_000_000_001)
        launchFleetWindow()
        waitFor(fleetRow("claude:alpha"))

        fixture.stop(removeHome: false)
        let stale = textValue(beginningWith: "Stale since")
        waitFor(stale)
        XCTAssertTrue(fleetRow("claude:alpha").exists, "stale mode must retain authoritative roster")
    }
}
