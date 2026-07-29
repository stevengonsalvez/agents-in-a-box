import Foundation

struct FleetProjection: Equatable {
    let snapshot: FleetSnapshot?
    let committedRevision: Int64
    let seenEventIDs: Set<String>
    let needsSnapshot: Bool
    let needsResubscribe: Bool

    static let empty = FleetProjection(snapshot: nil, committedRevision: 0, seenEventIDs: [], needsSnapshot: false, needsResubscribe: false)
}

enum FleetProjectionReducer {
    static func bootstrap(_ result: FleetSubscribeResult) -> FleetProjection {
        let seenEventIDs: Set<String>
        switch result.replayState {
        case .snapshotReset:
            guard result.replay.isEmpty else { return resubscribeRequired(from: .empty) }
            seenEventIDs = []
        case .complete:
            guard replayIsContiguous(result.replay, through: result.snapshot.headRevision) else {
                return resubscribeRequired(from: .empty)
            }
            seenEventIDs = Set(result.replay.map(\.eventID))
        }
        return authoritativeSnapshot(result.snapshot, seenEventIDs: seenEventIDs)
    }

    static func live(_ event: FleetEvent, from projection: FleetProjection) -> FleetProjection {
        guard !projection.needsResubscribe else { return projection }
        guard !projection.seenEventIDs.contains(event.eventID) else { return projection }
        guard event.revision > projection.committedRevision else { return projection }
        guard event.revision == projection.committedRevision + 1 else {
            return resubscribeRequired(from: projection)
        }
        return FleetProjection(
            snapshot: projection.snapshot,
            committedRevision: event.revision,
            seenEventIDs: projection.seenEventIDs.union([event.eventID]),
            needsSnapshot: true,
            needsResubscribe: false
        )
    }

    static func snapshot(_ snapshot: FleetSnapshot, from projection: FleetProjection) -> FleetProjection {
        guard !projection.needsResubscribe else { return projection }
        guard snapshot.headRevision >= projection.committedRevision else { return projection }
        return authoritativeSnapshot(snapshot, seenEventIDs: [])
    }

    static func resyncRequired(from projection: FleetProjection) -> FleetProjection {
        resubscribeRequired(from: projection)
    }

    private static func authoritativeSnapshot(_ snapshot: FleetSnapshot, seenEventIDs: Set<String>) -> FleetProjection {
        FleetProjection(snapshot: snapshot, committedRevision: snapshot.headRevision, seenEventIDs: seenEventIDs, needsSnapshot: false, needsResubscribe: false)
    }

    private static func resubscribeRequired(from projection: FleetProjection) -> FleetProjection {
        FleetProjection(snapshot: projection.snapshot, committedRevision: projection.committedRevision, seenEventIDs: projection.seenEventIDs, needsSnapshot: true, needsResubscribe: true)
    }

    private static func replayIsContiguous(_ replay: [FleetEvent], through headRevision: Int64) -> Bool {
        guard let first = replay.first else { return true }
        guard replay.last?.revision == headRevision else { return false }
        var previous = first.revision - 1
        var eventIDs = Set<String>()
        for event in replay {
            guard event.revision == previous + 1, eventIDs.insert(event.eventID).inserted else { return false }
            previous = event.revision
        }
        return true
    }
}
