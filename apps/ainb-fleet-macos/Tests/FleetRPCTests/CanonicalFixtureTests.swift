import Foundation
import XCTest
@testable import AINBFleet

final class CanonicalFixtureTests: XCTestCase {
    func testLocalWireSamplesDecodeAndRoundTripSemantically() throws {
        try assertJSON(
            #"{"id":1,"jsonrpc":"2.0","method":"auth/hello","params":{"token":"fixture-token"}}"#,
            as: RPCRequest<AuthHelloParams>.self
        )
        try assertJSON(
            #"{"capability_ids":["fleet.snapshot.read"],"daemon_version":"fixture-daemon","protocol_version":2,"read_compatible":true,"write_compatible":true}"#,
            as: FleetNegotiateResult.self
        )
        try assertJSON(
            #"{"head_revision":42,"sessions":[]}"#,
            as: FleetSnapshot.self
        )
        try assertJSON(
            #"{"action_fingerprint":"sha256:action","action_kind":"structured_answer","created_at":1700000000200,"detail":"fixture delivery","expected_version":7,"idempotency_key":"broadcast-001","request_id":"action-request-001","session_key":"fleet-session-001","session_version":8,"status":"DELIVERED","updated_at":1700000000300}"#,
            as: FleetActionReceipt.self
        )
    }

    /// Pins the snake_case wire contract for `fleet/usage_dashboard`. The daemon
    /// side is Rust, so a renamed key here is a silent cross-language break that
    /// no compiler catches.
    func testUsageDashboardResultRoundTrips() throws {
        let bucket = #"{"input_tokens":50000,"cache_creation_tokens":2000,"cache_read_tokens":5000,"output_tokens":30000,"reasoning_tokens":0,"call_count":150,"session_count":3,"project_count":2,"cost_usd":12.5}"#
        try assertJSON(
            """
            {"state":"ready","generated_at":1700000000000,"start_at":1680000000000,"end_at":1700000000000,\
            "cost_complete":true,"totals":\(bucket),\
            "weekly":[{"week_start":"2026-08-03","bucket":\(bucket)}],\
            "heatmap":[{"date":"2026-08-06","call_count":5,"cost_usd":0.5}],\
            "forecast":{"projected_30d_cost_usd":15,"projected_30d_tokens":120000,"avg_daily_cost_usd":0.5,"avg_daily_tokens":4000,"sample_days":7},\
            "providers":[{"provider":"claude","bucket":\(bucket)}],\
            "models":[{"model":"claude-sonnet-4-5","bucket":\(bucket)}],\
            "projects":[{"project":"ainb","repo":"stevengonsalvez/agents-in-a-box","bucket":\(bucket)}],\
            "sessions":[{"session_id":"claude:ainb:s1","provider":"claude","project":"ainb","bucket":\(bucket)}],\
            "branches":[{"branch":"main","bucket":\(bucket)}],\
            "tools":[{"name":"Read","call_count":50}],\
            "mcp_servers":[{"name":"context7","call_count":10}],\
            "shell_commands":[{"name":"cargo test","call_count":25}]}
            """,
            as: FleetUsageDashboardResult.self
        )
    }

    /// The daemon reports `scanning`/`unavailable` with every aggregate absent.
    /// Those must decode to nil rather than throwing, or the panel goes blank
    /// on exactly the states it exists to explain.
    func testDashboardDecodesWhenAggregatesAreAbsent() throws {
        let json = #"{"state":"scanning","cost_complete":false,"weekly":[],"heatmap":[],"providers":[],"models":[],"projects":[],"sessions":[],"branches":[],"tools":[],"mcp_servers":[],"shell_commands":[],"detail":"Reading provider logs"}"#
        let result = try FleetWire.decoder().decode(FleetUsageDashboardResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.state, .scanning)
        XCTAssertNil(result.totals)
        XCTAssertNil(result.forecast)
        XCTAssertNil(result.generatedAt)
        XCTAssertTrue(result.weekly.isEmpty)
        XCTAssertEqual(result.detail, "Reading provider logs")
        XCTAssertFalse(result.costComplete)
    }

    /// Tokens-without-price is a real daemon state (unknown model pricing). It
    /// must stay nil and never collapse to 0.0, which would read as "free".
    func testDashboardKeepsUnpricedCostNilRatherThanZero() throws {
        let bucket = #"{"input_tokens":10,"cache_creation_tokens":0,"cache_read_tokens":0,"output_tokens":10,"reasoning_tokens":0,"call_count":1,"session_count":1,"project_count":1}"#
        let json = """
        {"state":"partial","cost_complete":false,"totals":\(bucket),"weekly":[],"heatmap":[{"date":"2026-08-06","call_count":1}],\
        "providers":[],"models":[],"projects":[],"sessions":[],"branches":[],"tools":[],"mcp_servers":[],"shell_commands":[]}
        """
        let result = try FleetWire.decoder().decode(FleetUsageDashboardResult.self, from: Data(json.utf8))
        XCTAssertNil(result.totals?.costUSD)
        XCTAssertEqual(result.totals?.totalTokens, 20)
        XCTAssertNil(result.heatmap.first?.costUSD)
    }

    /// The heatmap is a week-per-column grid, so a day landing in the wrong row
    /// is invisible to the compiler and plausible on screen. 2026-08-03 is a
    /// Monday and 2026-08-09 the Sunday that closes the same ISO week.
    func testHeatmapPlacesDaysInMondayFirstWeekdayRows() throws {
        let cells = [
            FleetHeatmapCell(date: "2026-08-03", callCount: 1, costUSD: nil),  // Monday
            FleetHeatmapCell(date: "2026-08-06", callCount: 2, costUSD: nil),  // Thursday
            FleetHeatmapCell(date: "2026-08-09", callCount: 3, costUSD: nil),  // Sunday
            FleetHeatmapCell(date: "2026-08-10", callCount: 4, costUSD: nil),  // next Monday
        ]
        let weeks = FleetHeatmapLayout.weekColumns(cells)

        XCTAssertEqual(weeks.count, 2, "four days spanning a Sunday boundary make two columns")
        XCTAssertEqual(weeks[0].weekStart, "2026-08-03")
        XCTAssertEqual(weeks[0].days[0]?.callCount, 1, "Monday is row 0")
        XCTAssertEqual(weeks[0].days[3]?.callCount, 2, "Thursday is row 3")
        XCTAssertEqual(weeks[0].days[6]?.callCount, 3, "Sunday is row 6, closing the week")
        XCTAssertNil(weeks[0].days[1], "a day with no data stays an empty slot")
        XCTAssertEqual(weeks[1].weekStart, "2026-08-10")
        XCTAssertEqual(weeks[1].days[0]?.callCount, 4, "the next Monday opens a new column")
        XCTAssertTrue(weeks.allSatisfy { $0.days.count == 7 }, "every column is a full week")
    }

    /// Idle weeks must occupy a column, not vanish.
    ///
    /// The daemon ships a SPARSE heatmap (only days with calls). Emitting one
    /// column per populated week collapses real gaps: on a live corpus a
    /// two-month break across November and December rendered as adjacent
    /// columns and read as unbroken activity. The x-axis is only meaningful if
    /// it is a calendar.
    func testHeatmapKeepsIdleWeeksAsEmptyColumns() throws {
        // 2026-08-03 and 2026-08-24 are Mondays three weeks apart, so the two
        // weeks between them are genuinely idle.
        let weeks = FleetHeatmapLayout.weekColumns([
            FleetHeatmapCell(date: "2026-08-03", callCount: 5, costUSD: nil),
            FleetHeatmapCell(date: "2026-08-24", callCount: 9, costUSD: nil),
        ])

        XCTAssertEqual(weeks.count, 4, "two active weeks plus the two idle ones between them")
        XCTAssertEqual(weeks.map(\.weekStart), [
            "2026-08-03", "2026-08-10", "2026-08-17", "2026-08-24",
        ], "columns advance one calendar week at a time")
        XCTAssertEqual(weeks[0].days[0]?.callCount, 5)
        XCTAssertEqual(weeks[3].days[0]?.callCount, 9)
        XCTAssertTrue(
            weeks[1].days.allSatisfy { $0 == nil } && weeks[2].days.allSatisfy { $0 == nil },
            "an idle week is present but empty, so the gap is visible"
        )
    }

    /// The scanner's project key is a mangled absolute path. Rendered raw it is
    /// unreadable at panel width, every row shares a long identical prefix, and
    /// it puts the operator's username on screen.
    func testProjectLabelStripsTheOperatorsHomePrefix() throws {
        let home = URL(fileURLWithPath: "/Users/someone", isDirectory: true)

        XCTAssertEqual(
            FleetProjectLabel.display(
                "Users-someone-.agents-in-a-box-worktrees-by-name-proj--f-atc--96da95da",
                repo: nil,
                home: home
            ),
            "agents-in-a-box-worktrees-by-name-proj--f-atc--96da95da",
            "the home prefix and its separators are dropped, the distinguishing tail stays"
        )
        // The leading-separator form the scanner also emits.
        XCTAssertEqual(
            FleetProjectLabel.display("-Users-someone--claude-jobs-tmp-wt", repo: nil, home: home),
            "claude-jobs-tmp-wt"
        )
        XCTAssertFalse(
            FleetProjectLabel.display("Users-someone-d-git-thing", repo: nil, home: home).contains("someone"),
            "the username must not survive into the label"
        )
        // A real repo name is better than any derived label, so it wins.
        XCTAssertEqual(
            FleetProjectLabel.display("Users-someone-d-git-thing", repo: "owner/thing", home: home),
            "owner/thing"
        )
        // A path outside the home is left alone rather than mangled further.
        XCTAssertEqual(
            FleetProjectLabel.display("opt-shared-thing", repo: nil, home: home),
            "opt-shared-thing"
        )
    }

    func testHeatmapSkipsUnparseableDatesRatherThanCrashing() throws {
        let weeks = FleetHeatmapLayout.weekColumns([
            FleetHeatmapCell(date: "not-a-date", callCount: 9, costUSD: nil),
            FleetHeatmapCell(date: "2026-08-03", callCount: 1, costUSD: nil),
        ])
        XCTAssertEqual(weeks.count, 1)
        XCTAssertEqual(weeks[0].days[0]?.callCount, 1)
    }

    func testReplayStateRejectsInconsistentForms() {
        XCTAssertThrowsError(try FleetWire.decoder().decode(FleetReplayState.self, from: Data(#"{"state":"complete","reason":"bootstrap"}"#.utf8)))
        XCTAssertThrowsError(try FleetWire.decoder().decode(FleetReplayState.self, from: Data(#"{"state":"snapshot_reset"}"#.utf8)))
        XCTAssertThrowsError(try FleetWire.decoder().decode(FleetReplayState.self, from: Data(#"{"state":"future"}"#.utf8)))
        XCTAssertThrowsError(try FleetWire.decoder().decode(FleetReplayState.self, from: Data(#"{"state":"snapshot_reset","reason":"bootstrap","extra":true}"#.utf8)))
    }

    private func assertJSON<T: Codable>(_ json: String, as type: T.Type, file: StaticString = #filePath, line: UInt = #line) throws {
        let original = Data(json.utf8)
        let decoded = try FleetWire.decoder().decode(T.self, from: original)
        let encoded = try FleetWire.encoder().encode(decoded)
        XCTAssertEqual(try canonicalJSON(original), try canonicalJSON(encoded), "wire sample drift", file: file, line: line)
    }

    private func canonicalJSON(_ data: Data) throws -> Data {
        try JSONSerialization.data(withJSONObject: JSONSerialization.jsonObject(with: data), options: [.sortedKeys])
    }
}
