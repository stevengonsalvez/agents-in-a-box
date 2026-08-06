import AppKit
import SwiftUI

struct FleetProviderIcon: View {
    let provider: FleetProvider
    let size: CGFloat

    var body: some View {
        Group {
            if let image = FleetProviderIconAsset.image(for: provider) {
                Image(nsImage: image)
                    .resizable()
                    .scaledToFit()
            } else {
                Image(systemName: fallbackSymbol)
                    .resizable()
                    .scaledToFit()
                    .foregroundStyle(.secondary)
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }

    private var fallbackSymbol: String {
        switch provider {
        case .acp: "point.3.connected.trianglepath.dotted"
        case .unknown: "terminal"
        case .claude, .codex, .copilot: "questionmark.circle"
        }
    }
}

private enum FleetProviderIconAsset {
    static func image(for provider: FleetProvider) -> NSImage? {
        let name: String? = switch provider {
        case .claude: "claude"
        case .codex: "codex"
        case .copilot: "copilot"
        case .acp, .unknown: nil
        }
        guard let name else { return nil }

        let url = Bundle.main.url(forResource: name, withExtension: "png")
            ?? Bundle.main.url(forResource: name, withExtension: "png", subdirectory: "ProviderIcons")
        return url.flatMap(NSImage.init(contentsOf:))
    }
}
