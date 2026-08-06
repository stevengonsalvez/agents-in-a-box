import Foundation

enum FleetRosterFocus: String, Codable, CaseIterable, Identifiable {
    case all, active, needsYou, ask, idle, done

    var id: Self { self }

    var label: String {
        switch self {
        case .all: "All"
        case .active: "Active"
        case .needsYou: "Needs you"
        case .ask: "Ask"
        case .idle: "Idle"
        case .done: "Done"
        }
    }

    func matches(_ session: FleetSession) -> Bool {
        switch self {
        case .all: true
        case .active: session.lifecycle == .starting || session.lifecycle == .running
        case .needsYou: session.attention != .none
        case .ask: session.attention == .ask
        case .idle: session.lifecycle == .idle
        case .done: session.lifecycle == .turnComplete
        }
    }
}

struct FleetRosterFilters: Codable, Equatable {
    var focus: FleetRosterFocus = .all
    var attentionOnly = false
    var lifecycle: LifecycleState?
    var provider: FleetProvider?
    var management: ManagementState?
    var transportHealth: TransportHealth?

    static let all = FleetRosterFilters()
    static let attentionOnly = FleetRosterFilters(attentionOnly: true)
}

enum FleetRosterSort: String, Codable, CaseIterable, Identifiable {
    case priority
    case recent

    var id: Self { self }

    var label: String {
        switch self {
        case .priority: "Attention and lifecycle"
        case .recent: "Most recently updated"
        }
    }
}

struct FleetPresentationPreferences: Codable, Equatable {
    static let currentVersion = 3
    private static let defaultsKey = "ainb.fleet.presentation.v3"

    var version = currentVersion
    var filters = FleetRosterFilters.all
    var sort = FleetRosterSort.priority

    static func load(defaults: UserDefaults = .standard) -> Self {
        guard let data = defaults.data(forKey: defaultsKey),
              let decoded = try? JSONDecoder().decode(Self.self, from: data),
              decoded.version == currentVersion
        else { return Self() }
        return decoded
    }

    func save(defaults: UserDefaults = .standard) {
        guard let data = try? JSONEncoder().encode(self) else { return }
        defaults.set(data, forKey: Self.defaultsKey)
    }
}

enum FleetRosterPresentation {
    static func sessionIdentity(for session: FleetSession) -> FleetSessionIdentity {
        FleetSessionIdentity(session: session)
    }

    static func visibleSessions(
        _ sessions: [FleetSession],
        search: String,
        filters: FleetRosterFilters,
        sort: FleetRosterSort = .priority
    ) -> [FleetSession] {
        sessions
            .filter { matches($0, search: search, filters: filters) }
            .sorted { orderedBefore($0, $1, sort: sort) }
    }

    static func statusLabel(for session: FleetSession) -> String {
        "\(session.lifecycle.rawValue.replacingOccurrences(of: "_", with: " ")) · \(session.attention.rawValue)"
    }

    static func statusSymbol(for session: FleetSession, connection: FleetConnectionState) -> String {
        guard connection.isLive else { return "exclamationmark.triangle" }
        if session.management == .degraded || session.transportHealth != .healthy { return "exclamationmark.circle" }
        if session.attention != .none { return "bell.badge" }
        return "checkmark.circle"
    }

    static func semanticStatus(for session: FleetSession, connection: FleetConnectionState) -> String {
        guard connection.isLive else { return connection.message }
        if session.management == .degraded || session.transportHealth != .healthy {
            return "Degraded: \(statusLabel(for: session)), \(session.management.rawValue), \(session.transportHealth.rawValue)"
        }
        return statusLabel(for: session)
    }

    static func freshnessLabel(for session: FleetSession) -> String {
        ISO8601DateFormatter().string(from: Date(timeIntervalSince1970: TimeInterval(session.lastObservedAt) / 1_000))
    }

    static func matches(_ session: FleetSession, search: String, filters: FleetRosterFilters) -> Bool {
        let query = search.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let searchable = [session.sessionKey, session.displayName ?? "", session.cwd, session.provider.rawValue]
        return (query.isEmpty || searchable.contains { $0.lowercased().contains(query) })
            && filters.focus.matches(session)
            && (!filters.attentionOnly || session.attention != .none)
            && (filters.lifecycle == nil || session.lifecycle == filters.lifecycle)
            && (filters.provider == nil || session.provider == filters.provider)
            && (filters.management == nil || session.management == filters.management)
            && (filters.transportHealth == nil || session.transportHealth == filters.transportHealth)
    }

    private static func orderedBefore(_ lhs: FleetSession, _ rhs: FleetSession, sort: FleetRosterSort) -> Bool {
        switch sort {
        case .priority:
            let left = (lhs.attention == .none ? 0 : 1, lifecyclePriority(lhs.lifecycle), lhs.updatedRevision)
            let right = (rhs.attention == .none ? 0 : 1, lifecyclePriority(rhs.lifecycle), rhs.updatedRevision)
            if left.0 != right.0 { return left.0 > right.0 }
            if left.1 != right.1 { return left.1 > right.1 }
            if left.2 != right.2 { return left.2 > right.2 }
        case .recent:
            if lhs.updatedRevision != rhs.updatedRevision { return lhs.updatedRevision > rhs.updatedRevision }
        }
        return lhs.sessionKey < rhs.sessionKey
    }

    private static func lifecyclePriority(_ value: LifecycleState) -> Int {
        switch value {
        case .starting: return 6
        case .running: return 5
        case .turnComplete: return 4
        case .idle: return 3
        case .exited: return 2
        case .unknown: return 1
        }
    }
}

struct FleetSessionIdentity: Equatable {
    let repository: String
    let worktree: String?
    let branch: String?

    init(session: FleetSession) {
        let path = URL(fileURLWithPath: session.cwd)
        let components = path.pathComponents
        let worktreeIndex = components.lastIndex(of: "worktrees")
        let candidate = worktreeIndex.flatMap { index -> String? in
            let remainder = components.dropFirst(index + 1)
            guard !remainder.isEmpty else { return nil }
            return remainder.first == "by-name" ? remainder.dropFirst().first : remainder.first
        } ?? path.lastPathComponent
        let labels = candidate.split(separator: "--", omittingEmptySubsequences: true).map(String.init)

        if labels.count >= 2 {
            repository = labels[0]
            worktree = candidate
            branch = Self.branchLabel(labels[1])
        } else {
            repository = candidate.isEmpty || candidate == "/" ? (session.displayName ?? session.sessionKey) : candidate
            worktree = candidate.isEmpty || candidate == "/" ? nil : candidate
            branch = nil
        }
    }

    private static func branchLabel(_ value: String) -> String {
        for prefix in ["f", "feat", "fix", "ops", "chore", "docs"] {
            let marker = "\(prefix)-"
            if value.hasPrefix(marker) {
                return "\(prefix)/\(value.dropFirst(marker.count))"
            }
        }
        return value
    }
}
