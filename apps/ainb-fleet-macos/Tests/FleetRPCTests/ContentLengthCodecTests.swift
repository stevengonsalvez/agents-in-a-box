import Foundation
import XCTest
@testable import AINBFleet

final class ContentLengthCodecTests: XCTestCase {
    func testSplitHeaderAndBodyReassemble() throws {
        var decoder = ContentLengthDecoder()
        XCTAssertEqual(try decoder.append(Data("Content-Len".utf8)), [])
        XCTAssertEqual(try decoder.append(Data("gth: 2\r\n\r\n{}".utf8)), [Data("{}".utf8)])
    }

    func testCoalescedFramesDrainInOrder() throws {
        var decoder = ContentLengthDecoder()
        let first = try ContentLengthEncoder.encode(Data("{}".utf8))
        let second = try ContentLengthEncoder.encode(Data("[]".utf8))
        XCTAssertEqual(try decoder.append(first + second), [Data("{}".utf8), Data("[]".utf8)])
    }

    func testMultibyteUTF8UsesByteLength() throws {
        let body = Data("☕".utf8)
        var decoder = ContentLengthDecoder()
        XCTAssertEqual(try decoder.append(ContentLengthEncoder.encode(body)), [body])
    }

    func testMissingLengthFails() {
        XCTAssertThrowsError(try decode("\r\n\r\n")) { XCTAssertEqual($0 as? ContentLengthCodecError, .missingContentLength) }
    }

    func testDuplicateLengthFails() {
        XCTAssertThrowsError(try decode("Content-Length: 2\r\ncontent-length: 2\r\n\r\n{}")) { XCTAssertEqual($0 as? ContentLengthCodecError, .duplicateContentLength) }
    }

    func testUnsupportedHeaderFails() {
        XCTAssertThrowsError(try decode("Content-Type: application/json\r\n\r\n{}")) {
            XCTAssertEqual($0 as? ContentLengthCodecError, .unsupportedHeader("Content-Type"))
        }
    }

    func testOversizedBodyFailsBeforeAllocation() {
        XCTAssertThrowsError(try decode("Content-Length: 16777217\r\n\r\n")) { XCTAssertEqual($0 as? ContentLengthCodecError, .oversizedBody) }
    }

    func testResetDropsTruncatedFrame() throws {
        var decoder = ContentLengthDecoder()
        XCTAssertEqual(try decoder.append(Data("Content-Length: 2\r\n\r\n{".utf8)), [])
        decoder.reset()
        XCTAssertEqual(try decoder.append(Data("Content-Length: 2\r\n\r\n[]".utf8)), [Data("[]".utf8)])
    }

    private func decode(_ string: String) throws -> [Data] {
        var decoder = ContentLengthDecoder()
        return try decoder.append(Data(string.utf8))
    }
}
