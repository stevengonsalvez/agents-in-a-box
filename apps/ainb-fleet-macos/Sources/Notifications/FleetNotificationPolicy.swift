import Foundation

struct FleetNotificationEvent: Equatable {
    let sessionKey: String
    let title: String
    let body: String

    var threadIdentifier: String { "fleet.\(sessionKey)" }
    var deepLink: URL { URL(string: "ainbfleet://session/\(sessionKey)")! }
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
