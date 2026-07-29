import Foundation
import UserNotifications

@MainActor
final class FleetNotificationCenter {
    private static let preferencesKey = "ainb.fleet.notifications.v1"
    private let center: UNUserNotificationCenter
    private let defaults: UserDefaults

    init(center: UNUserNotificationCenter = .current(), defaults: UserDefaults = .standard) {
        self.center = center
        self.defaults = defaults
    }

    var preferences: FleetNotificationPreferences {
        get {
            guard let data = defaults.data(forKey: Self.preferencesKey),
                  let preferences = try? JSONDecoder().decode(FleetNotificationPreferences.self, from: data)
            else { return FleetNotificationPreferences() }
            return preferences
        }
        set {
            guard let data = try? JSONEncoder().encode(newValue) else { return }
            defaults.set(data, forKey: Self.preferencesKey)
        }
    }

    func requestAuthorization() async -> Bool {
        (try? await center.requestAuthorization(options: [.alert, .sound, .badge])) ?? false
    }

    func deliver(_ events: [FleetNotificationEvent]) async {
        let preferences = preferences
        let currentHour = Calendar.current.component(.hour, from: .now)
        guard preferences.shouldDeliver(atHour: currentHour) else { return }
        for event in events {
            let content = UNMutableNotificationContent()
            content.title = event.title
            content.body = event.body
            content.sound = preferences.playsSound ? .default : nil
            content.threadIdentifier = event.threadIdentifier
            content.userInfo = ["session_key": event.sessionKey, "deep_link": event.deepLink.absoluteString]
            let request = UNNotificationRequest(identifier: "fleet.\(event.sessionKey).\(UUID().uuidString)", content: content, trigger: nil)
            try? await center.add(request)
        }
    }
}
