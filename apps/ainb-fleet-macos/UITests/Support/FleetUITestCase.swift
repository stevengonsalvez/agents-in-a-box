import XCTest

class FleetUITestCase: XCTestCase {
    var fixture: FleetFixtureDaemon!
    var app: XCUIApplication!

    override func setUpWithError() throws {
        continueAfterFailure = false
        fixture = try FleetFixtureDaemon()
        app = XCUIApplication()
    }

    override func tearDownWithError() throws {
        app?.terminate()
        fixture?.stop(removeHome: true)
    }

    @MainActor
    func launchApp(arguments: [String] = []) {
        if app.state != .notRunning {
            app.terminate()
        }
        app.launchEnvironment["AINB_HANGAR_HOME"] = fixture.home.path
        app.launchEnvironment["AINB_FLEET_TEST_ISOLATE_DEFAULTS"] = "1"
        app.launchEnvironment["AINB_FLEET_UI_TEST_MODE"] = "1"
        if arguments.contains("--fleet-test-read-range=3...3") {
            app.launchEnvironment["AINB_FLEET_TEST_READ_RANGE"] = "3...3"
        }
        app.launchArguments = arguments
        app.launch()
    }

    @MainActor
    func launchExpandedNotch(
        arguments: [String] = [],
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        launchApp(arguments: arguments)
        let notch = app.buttons["fleet.notch"]
        XCTAssertTrue(notch.waitForExistence(timeout: 8), "Fleet notch missing. \(app.debugDescription)", file: file, line: line)
        notch.click()
        XCTAssertTrue(app.textFields["fleet.notch.search"].waitForExistence(timeout: 8), "Fleet notch did not expand. \(app.debugDescription)", file: file, line: line)
    }

    @MainActor
    func waitFor(_ element: XCUIElement, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertTrue(
            element.waitForExistence(timeout: 8),
            "Missing \(element). \(app.debugDescription)",
            file: file,
            line: line
        )
    }

    @MainActor
    func fleetRow(_ sessionKey: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "fleet.notch.row.\(sessionKey)"))
            .firstMatch
    }

    @MainActor
    func fleetDetail(_ sessionKey: String) -> XCUIElement {
        app.staticTexts["fleet.notch.detail.\(sessionKey)"]
    }

    @MainActor
    func textValue(beginningWith value: String) -> XCUIElement {
        app.staticTexts
            .matching(NSPredicate(format: "value BEGINSWITH %@", value))
            .firstMatch
    }

}
