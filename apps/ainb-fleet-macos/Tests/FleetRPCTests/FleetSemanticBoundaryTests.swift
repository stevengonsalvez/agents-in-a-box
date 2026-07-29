import Foundation
import XCTest
@testable import AINBFleet

final class FleetSemanticBoundaryTests: XCTestCase {
    func testOpaquePayloadCannotChangeSemanticState() throws {
        let baseline = semanticBaseline()
        let first = FleetProjectionReducer.live(try sampleEvent(revision: 5, eventID: "next", payload: "{\"provider\":\"one\"}"), from: baseline)
        let second = FleetProjectionReducer.live(try sampleEvent(revision: 5, eventID: "next", payload: "{\"provider\":\"two\",\"nested\":[1,true]}"), from: baseline)
        XCTAssertEqual(first, second)
    }

    func testOpaqueCurrentRequestCannotChangeCapabilityState() throws {
        let first = FleetProjectionReducer.snapshot(try sampleSnapshot(head: 5, currentRequest: "{\"provider\":\"one\"}"), from: semanticBaseline())
        let second = FleetProjectionReducer.snapshot(try sampleSnapshot(head: 5, currentRequest: "{\"provider\":\"two\",\"nested\":[1,true]}"), from: semanticBaseline())
        XCTAssertEqual(first.snapshot?.sessions.first?.capabilities, second.snapshot?.sessions.first?.capabilities)
        XCTAssertEqual(first.committedRevision, second.committedRevision)
    }

    func testRPCErrorDataCannotEnableWrite() throws {
        let response = try FleetWire.decoder().decode(RPCResponse<FleetNegotiateResult>.self, from: Data("{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32603,\"message\":\"failed\",\"data\":{\"write_compatible\":true}}}".utf8))
        XCTAssertNil(response.result)
        XCTAssertEqual(response.error?.code, -32603)
    }

    func testRequestIDPassThroughCannotChangeReducerOrCapabilityState() throws {
        let firstIdentity = try identity("{\"provider\":\"one\"}")
        let secondIdentity = try identity("[\"provider\",2]")
        let numericIdentity = try identity("1.00")
        let first = try encodedApprove(firstIdentity)
        let second = try encodedApprove(secondIdentity)
        let numeric = try encodedApprove(numericIdentity)
        XCTAssertNotEqual(first, second)
        let baseline = semanticBaseline()
        XCTAssertEqual(FleetProjectionReducer.live(try sampleEvent(revision: 5, eventID: "next"), from: baseline), FleetProjectionReducer.live(try sampleEvent(revision: 5, eventID: "next"), from: baseline))
        XCTAssertEqual(baseline.snapshot?.sessions.first?.capabilities, semanticBaseline().snapshot?.sessions.first?.capabilities)
        XCTAssertEqual(try FleetWire.decoder().decode(FleetActionParams.self, from: first).action, .approve(requestFingerprint: "f1", requestIdentity: firstIdentity))
        XCTAssertEqual(try FleetWire.decoder().decode(FleetActionParams.self, from: second).action, .approve(requestFingerprint: "f1", requestIdentity: secondIdentity))
        XCTAssertEqual(try FleetWire.decoder().decode(FleetActionParams.self, from: numeric).action, .approve(requestFingerprint: "f1", requestIdentity: numericIdentity))
    }

    private func semanticBaseline() -> FleetProjection {
        FleetProjectionReducer.bootstrap(FleetSubscribeResult(snapshot: try! sampleSnapshot(head: 4), replay: [], replayState: .snapshotReset(reason: .bootstrap)))
    }

    private func identity(_ requestID: String) throws -> FleetRequestIdentity {
        try FleetWire.decoder().decode(FleetRequestIdentity.self, from: Data("{\"request_id\":\(requestID),\"thread_id\":\"t\",\"turn_id\":\"u\",\"item_id\":\"i\"}".utf8))
    }

    private func encodedApprove(_ identity: FleetRequestIdentity) throws -> Data {
        try FleetWire.encoder().encode(FleetActionParams(sessionKey: "s1", expectedVersion: 3, requestID: "action", action: .approve(requestFingerprint: "f1", requestIdentity: identity)))
    }
}
