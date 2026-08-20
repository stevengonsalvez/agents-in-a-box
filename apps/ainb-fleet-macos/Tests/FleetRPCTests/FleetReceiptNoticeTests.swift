import XCTest
@testable import AINBFleet

/// REGRESSION. A `fleet/action` that round-trips is not an action that landed.
/// The daemon answers a refused action with a successful RPC carrying a
/// non-delivered status plus a `detail`; the surface used to report
/// "Delivered. Confirming Fleet state." for every non-throwing call, so a
/// mirrored picker that refused the answer, sent ZERO keys and left the target
/// session sitting on its question still read as success here.
final class FleetReceiptNoticeTests: XCTestCase {
    func testFailedReceiptIsNotReportedAsDelivered() {
        let notice = FleetStore.controlNotice(
            for: receipt(.failed, detail: "visible picker option order does not match current question"),
            failurePrefix: "Interview"
        )
        XCTAssertFalse(notice.contains("Delivered"), "a FAILED receipt must never read as delivered: \(notice)")
        XCTAssertTrue(notice.contains("failed"), notice)
        XCTAssertTrue(
            notice.contains("visible picker option order does not match current question"),
            "the daemon's reason is the only place the cause exists, so it must be surfaced: \(notice)"
        )
    }

    func testRejectedAndUnknownAlsoSurfaceAsNotDelivered() {
        for status in [ActionReceiptStatus.rejected, .unknown] {
            let notice = FleetStore.controlNotice(for: receipt(status, detail: "nope"), failurePrefix: "Approval")
            XCTAssertFalse(notice.contains("Delivered"), "\(status) must not read as delivered: \(notice)")
            XCTAssertTrue(notice.hasPrefix("Approval "), notice)
        }
    }

    func testDeliveredKeepsTheConfirmingWording() {
        let notice = FleetStore.controlNotice(for: receipt(.delivered, detail: nil), failurePrefix: "Interview")
        XCTAssertEqual(notice, "Delivered. Confirming Fleet state.")
    }

    func testPendingIsNeitherDeliveredNorAFailure() {
        let notice = FleetStore.controlNotice(for: receipt(.pending, detail: nil), failurePrefix: "Interview")
        XCTAssertFalse(notice.contains("Delivered"), notice)
        XCTAssertTrue(notice.contains("awaiting delivery"), notice)
    }

    func testMissingDetailStillProducesAReadableNotice() {
        let notice = FleetStore.controlNotice(for: receipt(.failed, detail: "   "), failurePrefix: "Interview")
        XCTAssertEqual(notice, "Interview failed.")
    }

    func testBroadcastWhereEveryLegFailedIsNotReportedAsClean() {
        let notice = FleetStore.broadcastNotice(for: [
            receipt(.failed, detail: "no live tmux session"),
            receipt(.failed, detail: "no live tmux session"),
        ])
        XCTAssertFalse(notice.hasPrefix("Delivered"), "a total fan-out failure must not read as delivered: \(notice)")
        XCTAssertTrue(notice.contains("0/2 delivered"), notice)
        XCTAssertTrue(notice.contains("no live tmux session"), notice)
    }

    func testPartialBroadcastReportsBothHalves() {
        let notice = FleetStore.broadcastNotice(for: [
            receipt(.delivered, detail: nil),
            receipt(.failed, detail: "pane died"),
        ])
        XCTAssertTrue(notice.contains("1/2 delivered"), notice)
        XCTAssertTrue(notice.contains("1 failed"), notice)
    }

    func testFullyDeliveredBroadcastReadsAsDelivered() {
        let notice = FleetStore.broadcastNotice(for: [receipt(.delivered, detail: nil), receipt(.delivered, detail: nil)])
        XCTAssertEqual(notice, "Delivered to 2 sessions.")
    }

    func testEmptyBroadcastDoesNotClaimDelivery() {
        XCTAssertEqual(FleetStore.broadcastNotice(for: []), "Broadcast reached no sessions.")
    }

    private func receipt(_ status: ActionReceiptStatus, detail: String?) -> FleetActionReceipt {
        FleetActionReceipt(
            requestID: "req-1",
            sessionKey: "claude:abc",
            actionKind: "structured_answer",
            actionFingerprint: "fnv1a64:0",
            expectedVersion: 1,
            idempotencyKey: nil,
            status: status,
            detail: detail,
            sessionVersion: 1,
            createdAt: 0,
            updatedAt: 0
        )
    }
}
