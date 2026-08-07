import XCTest

final class ProtocolCompatibilityJourneyTests: FleetUITestCase {
    @MainActor
    func testRealFixtureReportsReadIncompatibleDaemonProtocol() throws {
        try fixture.seed(eventID: "alpha", sessionID: "alpha", eventType: "SessionStart", observedAt: 1_700_000_000_001)
        launchExpandedNotch(arguments: ["--fleet-test-read-range=3...3"])

        let incompatibility = textValue(beginningWith: "Daemon")
        waitFor(incompatibility)
        XCTAssertTrue((incompatibility.value as? String)?.contains("Fleet protocol 2") == true)
    }
}
