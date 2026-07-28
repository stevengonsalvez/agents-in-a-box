import XCTest

final class WindowAndSwitcherJourneyTests: FleetUITestCase {
    @MainActor
    func testRealFixtureWindowAndSwitcherSelectStableSessionKey() throws {
        try fixture.seed(eventID: "alpha-start", sessionID: "alpha", eventType: "SessionStart", observedAt: 1_700_000_000_001)
        try fixture.seed(eventID: "beta-ask", provider: "codex", sessionID: "beta", eventType: "AskUserQuestion", observedAt: 1_700_000_000_002)
        launchFleetWindow()
        let betaRow = fleetRow("codex:beta")
        waitFor(betaRow)
        selectToolbarItem("Quick switch")
        let beta = app.buttons["fleet.switch.codex:beta"]
        waitFor(beta)
        beta.click()
        waitFor(fleetDetail("codex:beta"))
    }

    @MainActor
    func testRealFixtureRetainsSelectedSessionAcrossReorderedLiveSnapshot() throws {
        try fixture.seed(eventID: "alpha-start", sessionID: "alpha", eventType: "SessionStart", observedAt: 1_700_000_000_001)
        try fixture.seed(eventID: "beta-start", provider: "codex", sessionID: "beta", eventType: "SessionStart", observedAt: 1_700_000_000_002)
        launchFleetWindow()
        let betaRow = fleetRow("codex:beta")
        waitFor(betaRow)
        betaRow.click()
        waitFor(fleetDetail("codex:beta"))

        try fixture.seed(eventID: "alpha-attention", sessionID: "alpha", eventType: "AskUserQuestion", observedAt: 1_700_000_000_003)
        waitFor(fleetRow("claude:alpha"))
        XCTAssertTrue(fleetDetail("codex:beta").exists, "live reorder must retain selection by session key")
    }
}
