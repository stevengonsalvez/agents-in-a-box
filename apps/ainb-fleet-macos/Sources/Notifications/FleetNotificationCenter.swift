import Foundation
import UserNotifications

@MainActor
final class FleetNotificationCenter {
    private let center: UNUserNotificationCenter

    init(center: UNUserNotificationCenter = .current()) {
        self.center = center
    }

    func requestAuthorization() async -> Bool {
        (try? await center.requestAuthorization(options: [.alert, .sound, .badge])) ?? false
    }

    func deliver(_ events: [FleetNotificationEvent]) async {
        for event in events {
            let content = UNMutableNotificationContent()
            content.title = event.title
            content.body = event.body
            content.threadIdentifier = event.threadIdentifier
            content.userInfo = ["session_key": event.sessionKey, "deep_link": event.deepLink.absoluteString]
            let request = UNNotificationRequest(identifier: "fleet.\(event.sessionKey).\(UUID().uuidString)", content: content, trigger: nil)
            try? await center.add(request)
        }
    }
}
