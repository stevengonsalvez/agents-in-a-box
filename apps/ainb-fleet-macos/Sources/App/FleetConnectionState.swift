import Foundation

enum FleetConnectionState: Equatable {
    case connecting
    case live(daemonVersion: String, writeCompatible: Bool)
    case stale(lastUpdated: Date, reason: String)
    case unavailable(message: String)
    case readIncompatible(daemonVersion: String, protocolVersion: UInt32)

    var isLive: Bool {
        if case .live = self { return true }
        return false
    }

    var message: String {
        switch self {
        case .connecting: return "Connecting to local Fleet daemon"
        case let .live(daemonVersion, _): return "Live, daemon \(daemonVersion)"
        case let .stale(lastUpdated, reason):
            return "Stale since \(lastUpdated.formatted(date: .omitted, time: .shortened)): \(reason)"
        case let .unavailable(message): return message
        case let .readIncompatible(daemonVersion, protocolVersion):
            return "Daemon \(daemonVersion) uses Fleet protocol \(protocolVersion), which this app cannot read"
        }
    }
}
