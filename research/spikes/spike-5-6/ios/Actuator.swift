// Spike 6 iOS simulator actuator. simctl has no lock or banner query, and the
// Simulator menu needs Accessibility rights this box does not grant, so the
// device transitions come from XCUITest instead. Each test is one action; the
// host scripts run them with `xcodebuild test-without-building -only-testing`.
// Every action stamps the host wall clock (the simulator shares it) into
// $SPIKE_EVENTS so the scripts line transitions up with the peer log.
import XCTest

final class Actuator: XCTestCase {
    private let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
    private let app = XCUIApplication(bundleIdentifier: "dev.ainb.spike.wire")

    private func stamp(_ event: String) {
        let path = ProcessInfo.processInfo.environment["SPIKE_EVENTS"] ?? "/tmp/spike56-events.log"
        let line = "\(Int(Date().timeIntervalSince1970 * 1000)) \(event)\n"
        if let h = FileHandle(forWritingAtPath: path) {
            h.seekToEndOfFile()
            h.write(line.data(using: .utf8)!)
            h.closeFile()
        } else {
            FileManager.default.createFile(atPath: path, contents: line.data(using: .utf8))
        }
    }

    func testLock() {
        stamp("lock_pressed")
        // ponytail: private XCUIDevice selector, the only lock press XCUITest has on a simulator.
        XCUIDevice.shared.perform(NSSelectorFromString("pressLockButton"))
    }

    func testHome() {
        stamp("home_pressed")
        XCUIDevice.shared.press(.home)
    }

    func testUnlockAndForeground() {
        XCUIDevice.shared.press(.home)
        sleep(1)
        // No passcode on the simulator: a swipe up on the lock screen dismisses it.
        springboard.swipeUp()
        sleep(1)
        app.activate()
        stamp("foregrounded")
    }

    func testAllowNotifications() {
        let allow = springboard.alerts.buttons["Allow"]
        if allow.waitForExistence(timeout: 10) {
            allow.tap()
            stamp("notifications_allowed")
        } else {
            stamp("notifications_prompt_absent")
        }
    }

    /// Taps the app's 30, 5 and 1 min banner buttons (longest first, so the
    /// 1 min banner cannot fire before the transition), answers the permission
    /// alert, then locks (SPIKE_LOCK=1) or presses Home, all in one runner
    /// launch so the runner's start-up cost stays outside the 1 min window. The
    /// simulator asks before opening a deep link, so taps replace the deep link.
    func testScheduleBannersAndLeave() {
        let cancel = springboard.alerts.buttons["Cancel"]
        if cancel.exists { cancel.tap() }
        app.activate()
        for label in ["30 min", "5 min", "1 min"] {
            let button = app.descendants(matching: .any).matching(NSPredicate(format: "label == %@", label)).firstMatch
            XCTAssertTrue(button.waitForExistence(timeout: 10))
            button.tap()
            stamp("tapped \(label)")
            let allow = springboard.alerts.buttons["Allow"]
            if allow.waitForExistence(timeout: 2) {
                allow.tap()
                stamp("notifications_allowed")
            }
        }
        sleep(1)
        if ProcessInfo.processInfo.environment["SPIKE_LOCK"] == "0" {
            stamp("home_pressed")
            XCUIDevice.shared.press(.home)
        } else {
            stamp("lock_pressed")
            XCUIDevice.shared.perform(NSSelectorFromString("pressLockButton"))
        }
    }

    /// Taps the app's biometric probe button, then stays alive long enough for
    /// the host to answer the Face ID sheet with a simulated match or non-match.
    func testTapBiometricProbe() {
        app.activate()
        // A React Native Pressable exposes its label as static text, not a button.
        let probe = app.descendants(matching: .any).matching(NSPredicate(format: "label == %@", "biometric gate")).firstMatch
        XCTAssertTrue(probe.waitForExistence(timeout: 10))
        probe.tap()
        stamp("biometric_probe_tapped")
        sleep(20)
    }

    /// Waits for a banner or lock-screen notification whose text contains
    /// SPIKE_BANNER (for example "scheduled 60s ahead"), up to SPIKE_WAIT_S.
    func testWaitBanner() {
        let env = ProcessInfo.processInfo.environment
        let needle = env["SPIKE_BANNER"] ?? "scheduled"
        let wait = TimeInterval(env["SPIKE_WAIT_S"] ?? "120") ?? 120
        let match = springboard.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS %@", needle)).firstMatch
        if match.waitForExistence(timeout: wait) {
            stamp("banner_seen \(needle)")
        } else {
            stamp("banner_absent \(needle)")
        }
    }
}
