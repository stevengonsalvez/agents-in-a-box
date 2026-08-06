import XCTest
@testable import AINBFleet

final class FleetRosterPresentationTests: XCTestCase {
    func testAttentionThenLifecycleThenRevisionThenKeyOrder() {
        let sessions = [
            session(key: "idle", lifecycle: .idle),
            session(key: "running-old", lifecycle: .running, revision: 2),
            session(key: "running-new", lifecycle: .running, revision: 3),
            session(key: "attention", lifecycle: .exited, attention: .ask),
        ]
        XCTAssertEqual(FleetRosterPresentation.visibleSessions(sessions, search: "", filters: .all).map(\.sessionKey), ["attention", "running-new", "running-old", "idle"])
    }

    func testOnlyCanonicalFieldsParticipateInSearchAndFilters() {
        let value = session(key: "codex-1", lifecycle: .running, attention: .approval)
        XCTAssertTrue(FleetRosterPresentation.matches(value, search: "codex", filters: .attentionOnly))
        XCTAssertTrue(FleetRosterPresentation.matches(value, search: "/workspace", filters: .all))
        XCTAssertFalse(FleetRosterPresentation.matches(value, search: "request", filters: .all))
    }

    func testCanonicalLifecycleProviderManagementAndTransportFilters() {
        let value = session(key: "codex-1", lifecycle: .running, attention: .approval)
        XCTAssertTrue(FleetRosterPresentation.matches(value, search: "", filters: FleetRosterFilters(lifecycle: .running, provider: .codex, management: .managed, transportHealth: .healthy)))
        XCTAssertFalse(FleetRosterPresentation.matches(value, search: "", filters: FleetRosterFilters(lifecycle: .idle)))
        XCTAssertFalse(FleetRosterPresentation.matches(value, search: "", filters: FleetRosterFilters(provider: .claude)))
    }

    func testFocusFiltersReturnOnlyVisibleSessionKinds() {
        let sessions = [
            session(key: "active", lifecycle: .running),
            session(key: "ask", lifecycle: .idle, attention: .ask),
            session(key: "idle", lifecycle: .idle),
            session(key: "done", lifecycle: .turnComplete),
        ]

        XCTAssertEqual(
            FleetRosterPresentation.visibleSessions(sessions, search: "", filters: FleetRosterFilters(focus: .ask)).map(\.sessionKey),
            ["ask"]
        )
        XCTAssertEqual(
            FleetRosterPresentation.visibleSessions(sessions, search: "", filters: FleetRosterFilters(focus: .active)).map(\.sessionKey),
            ["active"]
        )
    }

    func testVersionedPresentationPreferencesRestoreOnlyCurrentSchema() {
        let defaults = UserDefaults(suiteName: #function)!
        defaults.removePersistentDomain(forName: #function)
        var preferences = FleetPresentationPreferences()
        preferences.filters.provider = .codex
        preferences.sort = .recent
        preferences.save(defaults: defaults)
        let restored = FleetPresentationPreferences.load(defaults: defaults)
        XCTAssertEqual(restored.filters.provider, .codex)
        XCTAssertEqual(restored.sort, .recent)

        preferences.version = 0
        preferences.save(defaults: defaults)
        XCTAssertEqual(FleetPresentationPreferences.load(defaults: defaults), FleetPresentationPreferences())
    }

    func testRecentSortUsesRevisionThenSessionKey() {
        let sessions = [
            session(key: "bravo", lifecycle: .idle, revision: 4),
            session(key: "alpha", lifecycle: .running, revision: 4),
            session(key: "newest", lifecycle: .idle, revision: 5),
        ]
        XCTAssertEqual(
            FleetRosterPresentation.visibleSessions(sessions, search: "", filters: .all, sort: .recent).map(\.sessionKey),
            ["newest", "alpha", "bravo"]
        )
    }

    func testFreshnessUsesDaemonMilliseconds() {
        let value = session(key: "fresh", lifecycle: .idle, observedAt: 1_700_000_000_000)
        XCTAssertEqual(FleetRosterPresentation.freshnessLabel(for: value), "2023-11-14T22:13:20Z")
    }

    func testSessionWorkCountRoundTripsFromDaemonWire() throws {
        let value = session(key: "working", lifecycle: .running, activeWorkCount: 3)
        let decoded = try JSONDecoder().decode(FleetSession.self, from: JSONEncoder().encode(value))
        XCTAssertEqual(decoded.activeWorkCount, 3)
    }

    func testSessionIdentityUsesWorktreeRepositoryAndBranch() {
        let value = session(key: "claude:uuid", lifecycle: .idle, cwd: "/Users/stevie/.agents-in-a-box/worktrees/by-name/agents-in-a-box--f-minor-bugs--abcd/apps/ainb-fleet-macos")
        let identity = FleetRosterPresentation.sessionIdentity(for: value)
        XCTAssertEqual(identity.repository, "agents-in-a-box")
        XCTAssertEqual(identity.worktree, "agents-in-a-box--f-minor-bugs--abcd")
        XCTAssertEqual(identity.branch, "f/minor-bugs")
    }

    func testStatusIconPrefersDegradedSessionOverAttention() {
        let value = session(key: "degraded", lifecycle: .running, attention: .ask, management: .degraded)
        XCTAssertEqual(FleetStatusPresentation.symbol(for: .live(daemonVersion: "test", writeCompatible: true), needsYou: 1, sessions: [value]), "exclamationmark.circle.fill")
        XCTAssertEqual(FleetStatusPresentation.label(active: 1, needsYou: 1, state: .live(daemonVersion: "test", writeCompatible: true), sessions: [value]), "Fleet: 1 active, 1 need you. Degraded Fleet session health")
    }

    func testStatusIconTreatsNonHealthyTransportAsDegraded() {
        let value = session(key: "transport", lifecycle: .running, attention: .ask, transportHealth: .unknown)
        XCTAssertEqual(FleetStatusPresentation.symbol(for: .live(daemonVersion: "test", writeCompatible: true), needsYou: 1, sessions: [value]), "exclamationmark.circle.fill")
    }

    func testStatusIconPrefersUnavailableConnectionOverDegradedSession() {
        let value = session(key: "degraded", lifecycle: .running, management: .degraded)
        XCTAssertEqual(FleetStatusPresentation.symbol(for: .unavailable(message: "offline"), needsYou: 0, sessions: [value]), "exclamationmark.triangle.fill")
    }

    func testLifecycleNotificationPolicyGroupsBySessionAndSkipsInitialSnapshot() {
        let previous = session(key: "codex:alpha", lifecycle: .running, revision: 1)
        let current = session(key: "codex:alpha", lifecycle: .turnComplete, revision: 2)
        XCTAssertEqual(FleetNotificationPolicy.events(previous: [], current: [current]), [])
        let event = FleetNotificationPolicy.events(previous: [previous], current: [current]).first
        XCTAssertEqual(event?.threadIdentifier, "fleet.codex:alpha")
        XCTAssertEqual(event?.requestIdentifier, event?.requestIdentifier)
        XCTAssertNotEqual(
            event?.requestIdentifier,
            FleetNotificationPolicy.events(previous: [previous], current: [current]).first?.requestIdentifier
        )
    }

    func testNotificationPreferencesRespectEnabledAndQuietHours() {
        var preferences = FleetNotificationPreferences()
        XCTAssertTrue(preferences.shouldDeliver(atHour: 23))

        preferences.enabled = false
        XCTAssertFalse(preferences.shouldDeliver(atHour: 12))

        preferences.enabled = true
        preferences.quietHoursEnabled = true
        XCTAssertFalse(preferences.shouldDeliver(atHour: 23))
        XCTAssertFalse(preferences.shouldDeliver(atHour: 7))
        XCTAssertTrue(preferences.shouldDeliver(atHour: 8))
    }

    func testNotificationDeepLinkEncodesOpaqueSessionKey() {
        let event = FleetNotificationEvent(sessionKey: "codex/a?b#c", title: "Codex", body: "Complete")
        XCTAssertEqual(event.deepLink.absoluteString, "ainbfleet://session/codex%2Fa%3Fb%23c")
    }

    private func session(key: String, lifecycle: LifecycleState, cwd: String = "/workspace", attention: AttentionState = .none, activeWorkCount: Int64? = nil, management: ManagementState = .managed, transportHealth: TransportHealth = .healthy, observedAt: Int64 = 1, revision: Int64 = 1) -> FleetSession {
        FleetSession(sessionKey: key, provider: .codex, providerSessionID: nil, tmuxTarget: nil, processStartFingerprint: nil, cwd: cwd, displayName: key, lifecycle: lifecycle, activeWorkCount: activeWorkCount, attention: attention, currentRequestFingerprint: nil, currentRequest: nil, management: management, transportHealth: transportHealth, capabilities: FleetCapabilities(structuredAnswer: false, approvals: false, sendPrompt: false, continueTurn: false, retry: false, interrupt: false, start: false, stop: false, restart: false, kill: false, archive: false, tmuxAttach: false, tmuxText: false, verifiedPicker: false), provenance: .authoritative, confidence: .high, discoveredAt: observedAt, lastObservedAt: observedAt, lifecycleUpdatedAt: observedAt, attentionUpdatedAt: observedAt, version: 1, updatedRevision: revision)
    }
}
