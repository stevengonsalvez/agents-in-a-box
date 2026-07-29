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
        if arguments.contains("--fleet-test-open-window") {
            app.launchEnvironment["AINB_FLEET_TEST_OPEN_WINDOW"] = "1"
        }
        if arguments.contains("--fleet-test-read-range=2...2") {
            app.launchEnvironment["AINB_FLEET_TEST_READ_RANGE"] = "2...2"
        }
        app.launchArguments = arguments
        app.launch()
    }

    @MainActor
    func launchFleetWindow(
        arguments: [String] = [],
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        launchApp(arguments: ["--fleet-test-open-window"] + arguments)
        let fleetWindow = app.windows["Fleet"]
        XCTAssertTrue(
            fleetWindow.waitForExistence(timeout: 8),
            "Fleet window missing after normal app launch. \(app.debugDescription)",
            file: file,
            line: line
        )
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
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "fleet.row.\(sessionKey)"))
            .firstMatch
    }

    @MainActor
    func fleetDetail(_ sessionKey: String) -> XCUIElement {
        app.staticTexts["fleet.detail.\(sessionKey)"]
    }

    @MainActor
    func textValue(beginningWith value: String) -> XCUIElement {
        app.staticTexts
            .matching(NSPredicate(format: "value BEGINSWITH %@", value))
            .firstMatch
    }

}
