import XCTest

final class MenuBarRosterJourneyTests: FleetUITestCase {
    @MainActor
    func testRealStatusItemReflectsFleetTotals() throws {
        try fixture.seed(eventID: "alpha-start", sessionID: "alpha", eventType: "SessionStart", observedAt: 1_700_000_000_001)
        try fixture.seed(eventID: "beta-ask", provider: "codex", sessionID: "beta", eventType: "AskUserQuestion", observedAt: 1_700_000_000_002)
        launchApp()

        let statusItem = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", "fleet.status-item"))
            .firstMatch
        waitFor(statusItem)
        let totals = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "title CONTAINS %@", "1 active, 1 need you"),
            object: statusItem
        )
        XCTAssertEqual(XCTWaiter().wait(for: [totals], timeout: 8), .completed, app.debugDescription)
    }

    @MainActor
    func testRealFixtureRendersAttentionRosterAndFiltersInFleetWindow() throws {
        try fixture.seed(eventID: "alpha", sessionID: "alpha", eventType: "AskUserQuestion", observedAt: 1_700_000_000_001)
        try fixture.seed(eventID: "beta", provider: "codex", sessionID: "beta", eventType: "AskUserQuestion", observedAt: 1_700_000_000_002)
        try fixture.seed(eventID: "gamma", provider: "unknown", sessionID: "gamma", eventType: "AskUserQuestion", observedAt: 1_700_000_000_003)
        try fixture.seed(eventID: "running", sessionID: "running", eventType: "UserPromptSubmit", observedAt: 1_700_000_000_004)
        launchFleetWindow()

        waitFor(fleetRow("claude:alpha"))
        waitFor(fleetRow("codex:beta"))
        waitFor(fleetRow("unknown:gamma"))
        waitFor(fleetRow("claude:running"))

    }

    @MainActor
    func testFleetWindowPassesAccessibilityAudit() throws {
        try fixture.seed(eventID: "alpha", sessionID: "alpha", eventType: "AskUserQuestion", observedAt: 1_700_000_000_001)
        launchFleetWindow()
        waitFor(fleetRow("claude:alpha"))

        try app.performAccessibilityAudit()
    }
}
