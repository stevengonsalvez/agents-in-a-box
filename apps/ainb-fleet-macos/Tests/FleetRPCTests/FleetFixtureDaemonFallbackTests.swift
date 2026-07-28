import XCTest

final class FleetFixtureDaemonFallbackTests: XCTestCase {
    func testLocalFallbackResolvesFixtureWithoutEnvironmentOverride() throws {
        let executable = FleetFixtureDaemon.fixtureExecutableURL(sourceFilePath: #filePath, environment: [:])

        XCTAssertTrue(executable.path.hasSuffix("ainb-tui/target/debug/examples/fleet_fixture_daemon"))
        XCTAssertTrue(FileManager.default.fileExists(atPath: executable.deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().path))
    }
}
