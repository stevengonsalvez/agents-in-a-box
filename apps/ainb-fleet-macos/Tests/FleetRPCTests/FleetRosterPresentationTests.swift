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

    func testSearchIncludesSessionIdentityAndCanonicalFields() {
        let value = session(
            key: "codex-1",
            lifecycle: .running,
            cwd: "/Users/stevie/.agents-in-a-box/worktrees/by-name/agents-in-a-box--f-minor-bugs--abcd",
            attention: .approval
        )
        XCTAssertTrue(FleetRosterPresentation.matches(value, search: "codex", filters: .attentionOnly))
        XCTAssertTrue(FleetRosterPresentation.matches(value, search: "/users/stevie", filters: .all))
        XCTAssertTrue(FleetRosterPresentation.matches(value, search: "agents-in-a-box", filters: .all))
        XCTAssertTrue(FleetRosterPresentation.matches(value, search: "f/minor-bugs", filters: .all))
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
            Set(FleetRosterPresentation.visibleSessions(sessions, search: "", filters: FleetRosterFilters(focus: .active)).map(\.sessionKey)),
            Set(["active", "ask", "idle", "done"])
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

    func testDefaultFocusShowsEveryNonExitedSession() {
        let sessions = [
            session(key: "starting", lifecycle: .starting),
            session(key: "running", lifecycle: .running),
            session(key: "idle", lifecycle: .idle),
            session(key: "ask", lifecycle: .idle, attention: .ask),
            session(key: "done", lifecycle: .turnComplete),
            session(key: "unknown", lifecycle: .unknown),
            session(key: "exited", lifecycle: .exited),
        ]
        XCTAssertEqual(FleetRosterFilters().focus, .active)
        XCTAssertEqual(
            Set(FleetRosterPresentation.visibleSessions(sessions, search: "", filters: FleetRosterFilters()).map(\.sessionKey)),
            Set(["starting", "running", "idle", "ask", "done", "unknown"])
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
        XCTAssertEqual(identity.accessibilityLabel, "agents-in-a-box, agents-in-a-box--f-minor-bugs--abcd, f/minor-bugs")
        XCTAssertEqual(identity.contextLabel, "f/minor-bugs · agents-in-a-box--f-minor-bugs--abcd")
        XCTAssertEqual(identity.displayLabel, "agents-in-a-box · f/minor-bugs · agents-in-a-box--f-minor-bugs--abcd")
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

    private func session(key: String, provider: FleetProvider = .codex, lifecycle: LifecycleState, cwd: String = "/workspace", attention: AttentionState = .none, activeWorkCount: Int64? = nil, management: ManagementState = .managed, transportHealth: TransportHealth = .healthy, observedAt: Int64 = 1, revision: Int64 = 1, model: String? = nil, reasoningEffort: String? = nil, modelUpdatedAt: Int64? = nil) -> FleetSession {
        FleetSession(sessionKey: key, provider: provider, providerSessionID: nil, tmuxTarget: nil, processStartFingerprint: nil, cwd: cwd, displayName: key, lifecycle: lifecycle, activeWorkCount: activeWorkCount, attention: attention, currentRequestFingerprint: nil, currentRequest: nil, management: management, transportHealth: transportHealth, capabilities: FleetCapabilities(structuredAnswer: false, approvals: false, sendPrompt: false, continueTurn: false, retry: false, interrupt: false, start: false, stop: false, restart: false, kill: false, archive: false, tmuxAttach: false, tmuxText: false, verifiedPicker: false), provenance: .authoritative, confidence: .high, discoveredAt: observedAt, lastObservedAt: observedAt, lifecycleUpdatedAt: observedAt, attentionUpdatedAt: observedAt, version: 1, updatedRevision: revision, model: model, reasoningEffort: reasoningEffort, modelUpdatedAt: modelUpdatedAt)
    }

    func testModelLabelRendersPair() {
        let value = session(key: "claude:1", lifecycle: .running, model: "claude-opus-5", reasoningEffort: "xhigh")
        XCTAssertEqual(FleetRosterPresentation.modelLabel(for: value), "opus-5 · xhigh")
    }

    func testModelLabelRendersModelOnly() {
        let value = session(key: "codex:1", lifecycle: .running, model: "gpt-5.6-terra")
        XCTAssertEqual(
            FleetRosterPresentation.modelLabel(for: value),
            "gpt-5.6-terra",
            "only Claude's vendor prefix is stripped; every other id stays verbatim"
        )
    }

    func testModelLabelRendersEffortOnly() {
        let value = session(key: "claude:2", lifecycle: .running, reasoningEffort: "high")
        XCTAssertEqual(FleetRosterPresentation.modelLabel(for: value), "high effort")
    }

    /// Absence must be absence. Returning "" (or "unknown", or a dash) makes the
    /// row render a chip that looks like a reported value, which is exactly the
    /// failure this rule exists to prevent.
    func testModelLabelIsNilWhenAbsent() {
        XCTAssertNil(FleetRosterPresentation.modelLabel(for: session(key: "legacy:1", lifecycle: .running)))
        XCTAssertNil(
            FleetRosterPresentation.modelLabel(for: session(key: "legacy:2", lifecycle: .running, model: "  ", reasoningEffort: "")),
            "a blank string from the daemon is still nothing observed"
        )
    }

    /// Claude only refreshes the pair at the end of a turn, so a long-running
    /// session legitimately drifts. Past fifteen minutes the chip is history and
    /// the row must say so rather than present it as current.
    func testModelStaleWhenRunningAndOlderThanFifteenMinutes() {
        let observedAt: Int64 = 1_700_000_000_000
        let fresh = session(key: "fresh", lifecycle: .running, observedAt: observedAt, model: "claude-opus-5", modelUpdatedAt: observedAt - 899_000)
        let stale = session(key: "stale", lifecycle: .running, observedAt: observedAt, model: "claude-opus-5", modelUpdatedAt: observedAt - 901_000)
        let finished = session(key: "done", lifecycle: .turnComplete, observedAt: observedAt, model: "claude-opus-5", modelUpdatedAt: observedAt - 901_000)

        XCTAssertFalse(FleetRosterPresentation.modelIsStale(for: fresh))
        XCTAssertTrue(FleetRosterPresentation.modelIsStale(for: stale))
        XCTAssertFalse(FleetRosterPresentation.modelIsStale(for: finished), "only a running session can have drifted since its last turn")
        XCTAssertNotNil(FleetRosterPresentation.modelAsOfLabel(for: stale))
        XCTAssertNil(
            FleetRosterPresentation.modelAsOfLabel(for: session(key: "never", lifecycle: .running, model: "claude-opus-5")),
            "no observation time means no as-of phrase to show"
        )
    }

    /// Both roster surfaces read this. A session with nothing observed must read
    /// EXACTLY as it did before the chip existed, or the chip has taught
    /// VoiceOver to announce a model that was never reported.
    func testRowAccessibilityValueOnlyGrowsWhenAModelWasObserved() {
        let connection = FleetConnectionState.live(daemonVersion: "test", writeCompatible: true)
        let bare = session(key: "claude:none", lifecycle: .idle, attention: .ask)
        XCTAssertEqual(
            FleetRosterPresentation.rowAccessibilityValue(for: bare, connection: connection),
            FleetRosterPresentation.semanticStatus(for: bare, connection: connection)
        )

        let reported = session(key: "claude:one", lifecycle: .idle, attention: .ask, model: "claude-opus-5", reasoningEffort: "xhigh", modelUpdatedAt: 1)
        XCTAssertEqual(
            FleetRosterPresentation.rowAccessibilityValue(for: reported, connection: connection),
            "\(FleetRosterPresentation.semanticStatus(for: reported, connection: connection)), opus-5 · xhigh"
        )
        XCTAssertEqual(FleetRosterPresentation.modelHelp(for: reported), "opus-5 · xhigh", "a current chip needs no tooltip beyond itself")
    }

    /// The detail surface has the room to always carry the observation age, so
    /// an operator can tell a live pair from one pinned by a long turn.
    func testModelDetailCarriesTheObservationAge() {
        let value = session(key: "claude:1", lifecycle: .running, observedAt: 1_700_000_000_000, model: "claude-opus-5", reasoningEffort: "high", modelUpdatedAt: 1_699_999_000_000)
        let detail = FleetRosterPresentation.modelDetail(for: value)
        XCTAssertEqual(detail?.hasPrefix("opus-5 · high · as of "), true, detail ?? "nil")
        XCTAssertNil(FleetRosterPresentation.modelDetail(for: session(key: "claude:2", lifecycle: .running)))
    }

    func testSearchMatchesOnModel() {
        let value = session(key: "codex:1", lifecycle: .running, model: "claude-opus-5")
        XCTAssertTrue(FleetRosterPresentation.matches(value, search: "opus", filters: .all))
        XCTAssertFalse(FleetRosterPresentation.matches(value, search: "sonnet", filters: .all))
        XCTAssertFalse(
            FleetRosterPresentation.matches(session(key: "codex:2", lifecycle: .running), search: "opus", filters: .all),
            "a session with no model must not match a model query"
        )
    }
    /// The roster went permanently empty because `attentionOnly` persisted from
    /// the other window while the notch showed no control for it, and the empty
    /// state said only "No matching sessions". Naming the active filter is what
    /// makes that recoverable, so the naming itself is pinned.
    func testEmptyStateNamesTheFiltersThatAreHidingRows() {
        var filters = FleetRosterFilters.all
        filters.attentionOnly = true
        XCTAssertEqual(FleetRosterEmptyState.activeFilterNames(filters), ["needs you"])

        filters.provider = .claude
        filters.lifecycle = .idle
        XCTAssertEqual(
            FleetRosterEmptyState.activeFilterNames(filters),
            ["needs you", "lifecycle idle", "provider claude"],
            "every active filter is named, so nothing hides a row invisibly"
        )

        // Nothing structural set: a search term is the only thing left.
        XCTAssertEqual(FleetRosterEmptyState.activeFilterNames(.all), ["a search term"])
    }

    func testAntigravityProviderFilterAndEmptyState() {
        let agy = session(key: "antigravity:1", provider: .antigravity, lifecycle: .running)
        let codex = session(key: "codex:1", provider: .codex, lifecycle: .running)

        XCTAssertTrue(FleetRosterPresentation.matches(agy, search: "", filters: FleetRosterFilters(provider: .antigravity)))
        XCTAssertFalse(FleetRosterPresentation.matches(codex, search: "", filters: FleetRosterFilters(provider: .antigravity)))
        XCTAssertTrue(FleetRosterPresentation.matches(agy, search: "antigravity", filters: .all))

        var filters = FleetRosterFilters.all
        filters.provider = .antigravity
        XCTAssertEqual(
            FleetRosterEmptyState.activeFilterNames(filters),
            ["provider antigravity"]
        )
    }
}
