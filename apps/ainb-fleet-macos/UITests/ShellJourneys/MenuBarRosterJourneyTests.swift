import XCTest

final class MenuBarRosterJourneyTests: FleetUITestCase {
    @MainActor
    func testNotchReflectsFleetTotals() throws {
        try fixture.seed(eventID: "alpha-start", sessionID: "alpha", eventType: "SessionStart", observedAt: 1_700_000_000_001)
        try fixture.seed(eventID: "beta-ask", provider: "codex", sessionID: "beta", eventType: "AskUserQuestion", observedAt: 1_700_000_000_002)
        launchApp()

        let notch = app.buttons["fleet.notch"]
        waitFor(notch)
        let totals = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "label CONTAINS %@", "1 active, 1 need you"),
            object: notch
        )
        XCTAssertEqual(XCTWaiter().wait(for: [totals], timeout: 8), .completed, app.debugDescription)
    }

    @MainActor
    func testNotchExpandsControlsAndExplicitCTAOpensFleetWindow() throws {
        launchApp()

        let notch = app.buttons["fleet.notch"]
        waitFor(notch)
        notch.click()

        waitFor(app.textFields["fleet.notch.search"])
        XCTAssertFalse(app.windows["Fleet"].exists, "notch click must not open full Fleet")
        app.buttons["fleet.notch.show-all"].click()
        XCTAssertTrue(app.windows["Fleet"].waitForExistence(timeout: 8), app.debugDescription)
    }

    @MainActor
    func testNotchFilterSelectsOnlyVisibleSessions() throws {
        try fixture.seed(eventID: "active", sessionID: "active", eventType: "UserPromptSubmit", observedAt: 1_700_000_000_001)
        try fixture.seed(eventID: "ask", provider: "codex", sessionID: "ask", eventType: "AskUserQuestion", observedAt: 1_700_000_000_002)
        launchApp()

        let notch = app.buttons["fleet.notch"]
        waitFor(notch)
        notch.click()
        app.buttons["fleet.notch.filter.all"].click()
        waitFor(app.buttons["fleet.notch.row.claude:active"])
        waitFor(app.buttons["fleet.notch.row.codex:ask"])

        app.buttons["fleet.notch.filter.ask"].click()
        waitFor(app.buttons["fleet.notch.row.codex:ask"])
        XCTAssertFalse(app.buttons["fleet.notch.row.claude:active"].exists)
        waitFor(app.staticTexts["fleet.notch.detail.codex:ask"])
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
    func testFleetWindowExposesVoiceOverLabels() throws {
        try fixture.seed(eventID: "alpha", sessionID: "alpha", eventType: "AskUserQuestion", observedAt: 1_700_000_000_001)
        launchFleetWindow()
        let row = fleetRow("claude:alpha")
        waitFor(row)

        XCTAssertEqual(row.label, "claude:alpha")
        XCTAssertEqual(row.value as? String, "IDLE · ASK")
    }
}
