import XCTest

final class BuildConfigurationTests: XCTestCase {
    func testArm64AndMacOS14ArePinned() {
        #if arch(arm64)
        #else
        XCTFail("AINBFleet must build arm64")
        #endif

        guard #available(macOS 14.0, *) else {
            return XCTFail("AINBFleet requires macOS 14")
        }
    }
}
