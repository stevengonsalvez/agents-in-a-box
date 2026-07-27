import Foundation

struct HangarLocation: Sendable, Equatable {
    let home: URL

    init(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        homeDirectory: URL = FileManager.default.homeDirectoryForCurrentUser
    ) {
        if let configuredHome = environment["AINB_HANGAR_HOME"], !configuredHome.isEmpty {
            home = URL(fileURLWithPath: configuredHome, isDirectory: true)
        } else {
            home = homeDirectory.appendingPathComponent(".agents-in-a-box", isDirectory: true)
        }
    }

    var socketURL: URL {
        home.appendingPathComponent("hangar.sock", isDirectory: false)
    }

    var tokenURL: URL {
        home
            .appendingPathComponent("hangar", isDirectory: true)
            .appendingPathComponent("daemon.token", isDirectory: false)
    }

    func readToken() throws -> String {
        let token = try String(contentsOf: tokenURL, encoding: .utf8)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard !token.isEmpty else {
            throw FleetConnectionError.emptyToken
        }
        return token
    }
}
