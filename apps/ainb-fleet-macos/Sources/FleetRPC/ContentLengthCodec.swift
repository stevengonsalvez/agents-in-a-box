import Foundation

enum ContentLengthCodecError: Error, Equatable {
    case missingContentLength
    case duplicateContentLength
    case unsupportedHeader(String)
    case malformedContentLength
    case oversizedBody
}

enum ContentLengthEncoder {
    static let maximumBodyBytes = 16 * 1024 * 1024

    static func encode(_ body: Data) throws -> Data {
        guard body.count <= maximumBodyBytes else {
            throw ContentLengthCodecError.oversizedBody
        }
        var frame = Data("Content-Length: \(body.count)\r\n\r\n".utf8)
        frame.append(body)
        return frame
    }
}

struct ContentLengthDecoder {
    private var buffer = Data()

    mutating func append(_ chunk: Data) throws -> [Data] {
        buffer.append(chunk)
        var frames: [Data] = []

        while let headerRange = buffer.range(of: Data("\r\n\r\n".utf8)) {
            let header = buffer[..<headerRange.lowerBound]
            let bodyLength = try parseHeader(header)
            let bodyStart = headerRange.upperBound
            let frameLength = bodyStart + bodyLength
            guard buffer.count >= frameLength else {
                return frames
            }
            frames.append(buffer.subdata(in: bodyStart..<frameLength))
            buffer.removeSubrange(..<frameLength)
        }
        return frames
    }

    mutating func reset() {
        buffer.removeAll(keepingCapacity: false)
    }

    private func parseHeader(_ header: Data.SubSequence) throws -> Int {
        guard let text = String(data: header, encoding: .ascii), !text.isEmpty else {
            throw ContentLengthCodecError.missingContentLength
        }

        var length: Int?
        for line in text.components(separatedBy: "\r\n") {
            let pieces = line.split(separator: ":", maxSplits: 1, omittingEmptySubsequences: false)
            guard pieces.count == 2 else {
                throw ContentLengthCodecError.unsupportedHeader(line)
            }
            let name = pieces[0].trimmingCharacters(in: .whitespacesAndNewlines)
            let value = pieces[1].trimmingCharacters(in: .whitespacesAndNewlines)
            guard name.caseInsensitiveCompare("Content-Length") == .orderedSame else {
                throw ContentLengthCodecError.unsupportedHeader(name)
            }
            guard length == nil else {
                throw ContentLengthCodecError.duplicateContentLength
            }
            guard !value.isEmpty, value.allSatisfy(\.isNumber), let parsed = Int(value) else {
                throw ContentLengthCodecError.malformedContentLength
            }
            guard parsed <= ContentLengthEncoder.maximumBodyBytes else {
                throw ContentLengthCodecError.oversizedBody
            }
            length = parsed
        }
        guard let length else {
            throw ContentLengthCodecError.missingContentLength
        }
        return length
    }
}
