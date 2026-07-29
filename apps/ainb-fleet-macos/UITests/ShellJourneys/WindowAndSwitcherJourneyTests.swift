import Foundation
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

    @MainActor
    func testRealDaemonDeliversOnePromptThroughExactTmuxAndShowsReceipt() throws {
        let target = try FleetExactTmuxSession()
        defer { target.stop() }
        try fixture.seed(
            eventID: "live-session",
            sessionID: "live-session",
            eventType: "SessionStart",
            payload: target.hookPayload,
            observedAt: 1_700_000_000_001
        )
        launchFleetWindow()

        let row = fleetRow("claude:live-session")
        waitFor(row)
        row.click()
        waitFor(fleetDetail("claude:live-session"))

        let prompt = app.textFields["fleet.control.prompt"]
        waitFor(prompt)
        prompt.click()
        prompt.typeText("AFM-E03-ONE-DELIVERY")
        let send = app.buttons["fleet.control.Send prompt"]
        waitFor(send)
        send.click()
        send.click()

        XCTAssertTrue(target.waitForText("AFM-E03-ONE-DELIVERY", count: 1), target.capture())

        selectToolbarItem("Receipts")
        let receipts = app.staticTexts["fleet.receipts.list"]
        waitFor(receipts)
        let delivered = app.staticTexts.matching(NSPredicate(format: "value CONTAINS %@", "tmux (")).firstMatch
        waitFor(delivered)
        XCTAssertTrue((delivered.label as String).contains("send_prompt"), app.debugDescription)
    }

    @MainActor
    func testRealDaemonStartAndBroadcastPreserveDaemonReceiptTruth() throws {
        let delivered = try FleetExactTmuxSession()
        defer { delivered.stop() }
        try fixture.seed(
            eventID: "broadcast-delivered",
            sessionID: "broadcast-delivered",
            eventType: "SessionStart",
            payload: delivered.hookPayload,
            observedAt: 1_700_000_000_001
        )
        try fixture.seed(
            eventID: "broadcast-stale",
            sessionID: "broadcast-stale",
            eventType: "SessionStart",
            payload: [
                "tmux_target": "afm-e03-stale:0.0",
                "process_start_fingerprint": "pane=%999;pid=999;session_started=1",
            ],
            observedAt: 1_700_000_000_002
        )
        launchFleetWindow()
        waitFor(fleetRow("claude:broadcast-delivered"))
        waitFor(fleetRow("claude:broadcast-stale"))

        selectToolbarItem("Start")
        let start = app.otherElements["fleet.start.form"]
        waitFor(start)
        let cwd = app.textFields["Working directory"]
        waitFor(cwd)
        cwd.click()
        cwd.typeKey("a", modifierFlags: .command)
        cwd.typeText("/definitely/not/a/directory")
        XCTAssertFalse(app.buttons["fleet.start.submit"].isEnabled)
        cwd.click()
        cwd.typeKey("a", modifierFlags: .command)
        cwd.typeText(FileManager.default.temporaryDirectory.path)
        app.buttons["fleet.start.submit"].click()
        let prospective = app.staticTexts.matching(NSPredicate(format: "value BEGINSWITH %@", "start:codex:")).firstMatch
        waitFor(prospective)
        XCTAssertTrue(app.staticTexts["UNKNOWN"].exists, app.debugDescription)
        app.buttons["Cancel"].click()

        selectToolbarItem("Broadcast")
        let broadcast = app.otherElements["fleet.broadcast.form"]
        waitFor(broadcast)
        let message = app.textFields["Message"]
        waitFor(message)
        message.click()
        message.typeText("AFM-E03-BROADCAST")
        for key in ["claude:broadcast-delivered", "claude:broadcast-stale"] {
            let recipient = app.checkBoxes[key]
            waitFor(recipient)
            recipient.click()
        }
        app.buttons["Review broadcast"].click()
        let send = app.buttons["Send"]
        waitFor(send)
        send.click()
        XCTAssertTrue(delivered.waitForText("AFM-E03-BROADCAST", count: 1), delivered.capture())

        selectToolbarItem("Receipts")
        let receipts = app.staticTexts["fleet.receipts.list"]
        waitFor(receipts)
        let received = app.staticTexts.matching(NSPredicate(format: "value CONTAINS %@", "tmux (")).firstMatch
        waitFor(received)
        let failed = app.staticTexts.matching(NSPredicate(format: "value CONTAINS %@", "identity changed")).firstMatch
        waitFor(failed)
    }
}

private final class FleetExactTmuxSession {
    private let session = "afm-e03-claude-\(UUID().uuidString.prefix(8).lowercased())"

    init() throws {
        try run("new-session", "-d", "-s", session, "-c", FileManager.default.temporaryDirectory.path, "--", "/bin/sh", "-c", "printf 'AFM-E03-READY\\n'; while IFS= read -r line; do printf 'AFM-E03-RECEIVED:%s\\n' \"$line\"; done")
        _ = capture()
    }

    deinit { stop() }

    var hookPayload: [String: Any] {
        let fields = try! run("list-panes", "-t", session, "-F", "#{session_name}:#{window_index}.#{pane_index}\\t#{pane_id}\\t#{pane_pid}\\t#{session_created}")
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .split(separator: "\\t", omittingEmptySubsequences: false)
        precondition(fields.count == 4, "unexpected tmux identity")
        return [
            "tmux_target": String(fields[0]),
            "process_start_fingerprint": "pane=\(fields[1]);pid=\(fields[2]);session_started=\(fields[3])",
        ]
    }

    func capture() -> String {
        (try? run("capture-pane", "-p", "-t", "\(session):0.0")) ?? "unable to capture \(session)"
    }

    func waitForText(_ text: String, count: Int) -> Bool {
        let deadline = Date().addingTimeInterval(8)
        while Date() < deadline {
            if capture().components(separatedBy: text).count - 1 == count { return true }
            RunLoop.current.run(until: Date().addingTimeInterval(0.05))
        }
        return false
    }

    func stop() {
        _ = try? run("kill-session", "-t", session)
    }

    @discardableResult
    private func run(_ arguments: String...) throws -> String {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/tmux")
        process.arguments = arguments
        let output = Pipe()
        let error = Pipe()
        process.standardOutput = output
        process.standardError = error
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            let message = String(data: error.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? "tmux failed"
            throw NSError(domain: "FleetExactTmuxSession", code: Int(process.terminationStatus), userInfo: [NSLocalizedDescriptionKey: message])
        }
        return String(data: output.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
    }
}
