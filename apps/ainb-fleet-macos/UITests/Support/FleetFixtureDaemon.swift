import Foundation
import XCTest

/// XCTest-owned process wrapper for the real Rust Fleet daemon fixture.
/// Swift only supplies hook observations on stdin. The daemon owns SQLite,
/// token, Unix socket, revisions, snapshots, and subscription notifications.
final class FleetFixtureDaemon: @unchecked Sendable {
    private(set) var home: URL
    private var process: Process?
    private var input: Pipe?
    private var output: Pipe?
    private var standardError: Pipe?
    private var responseBuffer = Data()
    private var outputTranscript = Data()
    private var errorTranscript = Data()
    private var responseError: String?
    private var responseWaiter: DispatchSemaphore?
    private let responseLock = NSLock()

    init() throws {
        // Unix-domain sockets have a short path limit. XCTest's temporary root is
        // often already long enough to make `hangar.sock` fail to bind.
        home = FileManager.default.temporaryDirectory
            .appendingPathComponent("f\(UUID().uuidString.prefix(4))", isDirectory: true)
        try launch()
    }

    deinit { stop(removeHome: true) }

    func seed(
        eventID: String,
        provider: String = "claude",
        sessionID: String,
        eventType: String,
        cwd: String = "/fixture/workspace",
        payload: [String: Any] = [:],
        observedAt: Int64
    ) throws {
        let command: [String: Any] = [
            "command": "seed",
            "event_id": eventID,
            "provider": provider,
            "session_id": sessionID,
            "event_type": eventType,
            "cwd": cwd,
            "payload": payload,
            "observed_at": observedAt,
        ]
        responseLock.lock()
        responseWaiter = DispatchSemaphore(value: 0)
        responseError = nil
        let waiter = responseWaiter
        responseLock.unlock()
        let body = try JSONSerialization.data(withJSONObject: command)
        guard process?.isRunning == true, let input else {
            throw FixtureError.notRunning(diagnostics())
        }
        input.fileHandleForWriting.write(body + Data([0x0A]))
        guard waiter?.wait(timeout: .now() + 5) == .success else {
            throw FixtureError.timedOutWaitingForSeed(diagnostics())
        }
        responseLock.lock()
        let error = responseError
        responseLock.unlock()
        if let error { throw FixtureError.commandRejected(error, diagnostics()) }
    }

    func stop(removeHome: Bool = false) {
        guard let process else { return }
        if process.isRunning {
            let shutdown = #"{"command":"shutdown"}"# + "\n"
            input?.fileHandleForWriting.write(Data(shutdown.utf8))
            process.terminate()
        }
        process.waitUntilExit()
        self.process = nil
        input = nil
        output = nil
        if removeHome { try? FileManager.default.removeItem(at: home) }
    }

    func restart() throws {
        stop(removeHome: false)
        try launch()
    }

    private func launch() throws {
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        let executable = try Self.fixtureExecutable()
        let input = Pipe()
        let output = Pipe()
        let standardError = Pipe()
        let process = Process()
        process.executableURL = executable
        process.standardInput = input
        process.standardOutput = output
        process.standardError = standardError
        process.environment = ["AINB_HANGAR_HOME": home.path]
        output.fileHandleForReading.readabilityHandler = { [weak self] handle in
            self?.consume(handle.availableData)
        }
        standardError.fileHandleForReading.readabilityHandler = { [weak self] handle in
            self?.consumeStandardError(handle.availableData)
        }
        try process.run()
        self.process = process
        self.input = input
        self.output = output
        self.standardError = standardError
        try waitForDaemonFiles()
    }

    static func fixtureExecutable(
        sourceFilePath: String = #filePath,
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) throws -> URL {
        let executable = fixtureExecutableURL(sourceFilePath: sourceFilePath, environment: environment)
        guard FileManager.default.isExecutableFile(atPath: executable.path) else {
            throw FixtureError.missingExecutable(executable.path)
        }
        return executable
    }

    static func fixtureExecutableURL(
        sourceFilePath: String,
        environment: [String: String]
    ) -> URL {
        if let path = environment["AINB_FLEET_FIXTURE_DAEMON"] {
            return URL(fileURLWithPath: path)
        }
        var root = URL(fileURLWithPath: sourceFilePath).deletingLastPathComponent()
        while root.path != "/" {
            let daemonWorkspace = root.appendingPathComponent("ainb-tui", isDirectory: true)
            if FileManager.default.fileExists(atPath: daemonWorkspace.path) {
                return daemonWorkspace.appendingPathComponent("target/debug/examples/fleet_fixture_daemon")
            }
            root.deleteLastPathComponent()
        }
        return URL(fileURLWithPath: sourceFilePath)
            .deletingLastPathComponent()
            .appendingPathComponent("ainb-tui/target/debug/examples/fleet_fixture_daemon")
    }

    private func waitForDaemonFiles() throws {
        let socket = home.appendingPathComponent("hangar.sock").path
        let token = home.appendingPathComponent("hangar/daemon.token").path
        let deadline = Date().addingTimeInterval(5)
        while Date() < deadline {
            if FileManager.default.fileExists(atPath: socket), FileManager.default.fileExists(atPath: token) { return }
            if process?.isRunning == false { throw FixtureError.exited(process?.terminationStatus ?? -1, diagnostics()) }
            Thread.sleep(forTimeInterval: 0.025)
        }
        throw FixtureError.timedOutWaitingForDaemon(home.path, diagnostics())
    }

    private func consume(_ data: Data) {
        guard !data.isEmpty else { return }
        responseLock.lock()
        responseBuffer.append(data)
        outputTranscript.append(data)
        var signals = 0
        while let newline = responseBuffer.firstIndex(of: 0x0A) {
            let line = responseBuffer.prefix(upTo: newline)
            responseBuffer.removeSubrange(...newline)
            guard let object = try? JSONSerialization.jsonObject(with: line) as? [String: Any] else { continue }
            if object["ok"] as? Bool != true {
                responseError = object["error"] as? String ?? String(data: line, encoding: .utf8) ?? "fixture command failed"
            }
            signals += 1
        }
        let waiter = responseWaiter
        responseLock.unlock()
        for _ in 0..<signals { waiter?.signal() }
    }

    private func consumeStandardError(_ data: Data) {
        guard !data.isEmpty else { return }
        responseLock.lock()
        errorTranscript.append(data)
        responseLock.unlock()
    }

    private func diagnostics() -> String {
        responseLock.lock()
        defer { responseLock.unlock() }
        let output = String(data: outputTranscript, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let error = String(data: errorTranscript, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let printedOutput = output.isEmpty ? "<empty>" : output
        let printedError = error.isEmpty ? "<empty>" : error
        return "stdout: \(printedOutput); stderr: \(printedError)"
    }

    enum FixtureError: LocalizedError {
        case missingExecutable(String)
        case timedOutWaitingForDaemon(String, String)
        case timedOutWaitingForSeed(String)
        case commandRejected(String, String)
        case notRunning(String)
        case exited(Int32, String)

        var errorDescription: String? {
            switch self {
            case let .missingExecutable(path): return "Fleet fixture daemon is missing at \(path)"
            case let .timedOutWaitingForDaemon(path, diagnostics): return "Fleet fixture daemon did not create socket and token in \(path). \(diagnostics)"
            case let .timedOutWaitingForSeed(diagnostics): return "Fleet fixture daemon did not acknowledge seed. \(diagnostics)"
            case let .commandRejected(message, diagnostics): return "Fleet fixture daemon rejected command: \(message). \(diagnostics)"
            case let .notRunning(diagnostics): return "Fleet fixture daemon is not running. \(diagnostics)"
            case let .exited(status, diagnostics): return "Fleet fixture daemon exited with status \(status). \(diagnostics)"
            }
        }
    }
}
