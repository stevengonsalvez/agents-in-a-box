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
        case .active: session.lifecycle != .exited
        case .needsYou: session.attention != .none
        case .ask: session.attention == .ask
        case .idle: session.lifecycle == .idle
        case .done: session.lifecycle == .turnComplete
        }
    }
}

struct FleetRosterFilters: Codable, Equatable {
    var focus: FleetRosterFocus = .active
    var attentionOnly = false
    var lifecycle: LifecycleState?
    var provider: FleetProvider?
    var management: ManagementState?
    var transportHealth: TransportHealth?

    static let all = FleetRosterFilters(focus: .all)
    static let attentionOnly = FleetRosterFilters(focus: .all, attentionOnly: true)
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
    var filters = FleetRosterFilters()
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

    /// How long a running session may go without a fresh model observation
    /// before the chip is presented as historical rather than current. Claude
    /// only refreshes the pair at the end of a turn, so a long turn is expected
    /// to drift; fifteen minutes is past that.
    static let modelStaleAfterMilliseconds: Int64 = 900_000

    /// The model/effort pair as one chip, or nil when the daemon has never
    /// observed either. Absence renders as absence: callers must omit the chip
    /// entirely rather than substitute a dash or "unknown", which would read as
    /// reported data.
    static func modelLabel(for session: FleetSession) -> String? {
        let model = reported(session.model).map(shortModelName)
        let effort = reported(session.reasoningEffort)
        switch (model, effort) {
        case let (.some(model), .some(effort)): return "\(model) · \(effort)"
        case let (.some(model), .none): return model
        case let (.none, .some(effort)): return "\(effort) effort"
        case (.none, .none): return nil
        }
    }

    /// True when a live session's pair is old enough that it describes a past
    /// turn. Compared against the daemon's own clock, never the client's.
    static func modelIsStale(for session: FleetSession) -> Bool {
        session.lifecycle == .running
            && session.lastObservedAt - (session.modelUpdatedAt ?? 0) > modelStaleAfterMilliseconds
    }

    /// Freshness of the model observation for help text, e.g. "22 minutes ago".
    /// nil when the daemon never reported one.
    static func modelAsOfLabel(for session: FleetSession, now: Date = Date()) -> String? {
        guard let updatedAt = session.modelUpdatedAt, updatedAt > 0 else { return nil }
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .full
        return formatter.localizedString(
            for: Date(timeIntervalSince1970: TimeInterval(updatedAt) / 1_000),
            relativeTo: now
        )
    }

    /// The pair with the age of the observation, for a detail surface that has
    /// the room for it. nil when nothing was ever observed.
    static func modelDetail(for session: FleetSession) -> String? {
        guard let label = modelLabel(for: session) else { return nil }
        guard let asOf = modelAsOfLabel(for: session) else { return label }
        return "\(label) · as of \(asOf)"
    }

    /// Tooltip for the model chip. Only a stale chip earns an explanation; a
    /// current one would just repeat the chip back at the operator.
    static func modelHelp(for session: FleetSession) -> String? {
        guard let label = modelLabel(for: session) else { return nil }
        guard modelIsStale(for: session), let asOf = modelAsOfLabel(for: session) else { return label }
        return "\(label), as of \(asOf)"
    }

    /// The row's VoiceOver value. A session with no observed model reads exactly
    /// as it did before the chip existed, so the chip cannot make a row that
    /// reports nothing sound like it reports something.
    static func rowAccessibilityValue(for session: FleetSession, connection: FleetConnectionState) -> String {
        let status = semanticStatus(for: session, connection: connection)
        guard let help = modelHelp(for: session) else { return status }
        return "\(status), \(help)"
    }

    private static func reported(_ value: String?) -> String? {
        guard let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines), !trimmed.isEmpty else { return nil }
        return trimmed
    }

    /// Every provider id is left verbatim except Claude's redundant vendor
    /// prefix, which costs a third of the chip's width on every Claude row.
    private static func shortModelName(_ value: String) -> String {
        value.hasPrefix("claude-") ? String(value.dropFirst("claude-".count)) : value
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
        let identity = sessionIdentity(for: session)
        let searchable = [
            session.sessionKey,
            session.displayName ?? "",
            session.cwd,
            session.provider.rawValue,
            identity.repository,
            identity.worktree ?? "",
            identity.branch ?? "",
            session.model ?? "",
        ]
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

    var accessibilityLabel: String {
        [repository, worktree, branch].compactMap { $0 }.joined(separator: ", ")
    }

    var contextLabel: String {
        [branch, worktree].compactMap { $0 }.joined(separator: " · ")
    }

    var displayLabel: String {
        contextLabel.isEmpty ? repository : "\(repository) · \(contextLabel)"
    }

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
            worktree = nil
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
