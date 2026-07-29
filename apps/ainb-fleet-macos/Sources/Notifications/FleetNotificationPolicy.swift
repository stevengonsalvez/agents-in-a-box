import Foundation

struct FleetNotificationPreferences: Codable, Equatable {
    var enabled = true
    var playsSound = true
    var quietHoursEnabled = false
    var quietHoursStart = 22
    var quietHoursEnd = 8

    func shouldDeliver(atHour hour: Int) -> Bool {
        guard enabled, (0...23).contains(hour) else { return false }
        guard quietHoursEnabled, quietHoursStart != quietHoursEnd else { return true }
        let isQuiet = quietHoursStart < quietHoursEnd
            ? (quietHoursStart..<quietHoursEnd).contains(hour)
            : hour >= quietHoursStart || hour < quietHoursEnd
        return !isQuiet
    }
}

struct FleetNotificationEvent: Equatable {
    let sessionKey: String
    let title: String
    let body: String
    let eventID = UUID().uuidString

    var threadIdentifier: String { "fleet.\(sessionKey)" }
    var requestIdentifier: String { "\(threadIdentifier).\(eventID)" }
    var deepLink: URL {
        var components = URLComponents()
        components.scheme = "ainbfleet"
        components.host = "session"
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-._~"))
        components.percentEncodedPath = "/" + sessionKey.addingPercentEncoding(withAllowedCharacters: allowed)!
        return components.url!
    }
}

enum FleetNotificationPolicy {
    static func events(previous: [FleetSession], current: [FleetSession]) -> [FleetNotificationEvent] {
        let previousByKey = Dictionary(uniqueKeysWithValues: previous.map { ($0.sessionKey, $0) })
        return current.compactMap { session in
            guard let before = previousByKey[session.sessionKey],
                  before.lifecycle != session.lifecycle || before.attention != session.attention else { return nil }
            let changedToAttention = before.attention == .none && session.attention != .none
            let completed = session.lifecycle == .turnComplete || session.lifecycle == .exited
            guard changedToAttention || completed || before.lifecycle != session.lifecycle else { return nil }
            let name = session.displayName ?? session.sessionKey
            return FleetNotificationEvent(
                sessionKey: session.sessionKey,
                title: name,
                body: "\(session.lifecycle.rawValue.replacingOccurrences(of: "_", with: " ").capitalized) · \(session.attention.rawValue.capitalized)"
            )
        }
    }
}
