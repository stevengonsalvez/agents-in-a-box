import Foundation

enum FleetStatusPresentation {
    static func label(active: Int, needsYou: Int, state: FleetConnectionState, sessions: [FleetSession] = []) -> String {
        "Fleet: \(active) active, \(needsYou) need you. \(semanticState(for: state, sessions: sessions))"
    }

    static func symbol(for state: FleetConnectionState, needsYou: Int, sessions: [FleetSession] = []) -> String {
        guard state.isLive else { return "exclamationmark.triangle.fill" }
        if sessions.contains(where: isDegraded) { return "exclamationmark.circle.fill" }
        return needsYou > 0 ? "bell.badge.fill" : "bolt.circle.fill"
    }

    private static func semanticState(for state: FleetConnectionState, sessions: [FleetSession]) -> String {
        guard state.isLive else { return state.message }
        return sessions.contains(where: isDegraded) ? "Degraded Fleet session health" : state.message
    }

    private static func isDegraded(_ session: FleetSession) -> Bool {
        session.management == .degraded || session.transportHealth != .healthy
    }
}
