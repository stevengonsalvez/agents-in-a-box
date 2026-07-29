import SwiftUI

struct FleetSettingsView: View {
    @Binding var presentation: FleetPresentationPreferences
    @State private var notifications = FleetNotificationCenter()
    @State private var notificationPreferences = FleetNotificationPreferences()
    @State private var notificationStatus = ""

    var body: some View {
        Form {
            Text("Fleet presentation preferences remain local to this Mac.")
            Toggle("Needs you filter", isOn: $presentation.filters.attentionOnly)
            Picker("Roster sort", selection: $presentation.sort) {
                ForEach(FleetRosterSort.allCases) { sort in
                    Text(sort.label).tag(sort)
                }
            }
            Section("Notifications") {
                Toggle("Lifecycle notifications", isOn: $notificationPreferences.enabled)
                    .accessibilityIdentifier("fleet.notifications.enabled")
                Toggle("Notification sound", isOn: $notificationPreferences.playsSound)
                    .disabled(!notificationPreferences.enabled)
                Toggle("Quiet hours", isOn: $notificationPreferences.quietHoursEnabled)
                    .disabled(!notificationPreferences.enabled)
                if notificationPreferences.quietHoursEnabled {
                    Picker("From", selection: $notificationPreferences.quietHoursStart) {
                        ForEach(0..<24, id: \.self) { hour in Text(hourLabel(hour)).tag(hour) }
                    }
                    Picker("Until", selection: $notificationPreferences.quietHoursEnd) {
                        ForEach(0..<24, id: \.self) { hour in Text(hourLabel(hour)).tag(hour) }
                    }
                }
                Button("Enable lifecycle notifications") {
                    Task {
                        notificationStatus = await notifications.requestAuthorization()
                            ? "Lifecycle notifications enabled."
                            : "Notifications remain disabled in macOS settings."
                    }
                }
                if !notificationStatus.isEmpty { Text(notificationStatus).foregroundStyle(.secondary) }
            }
            Text("Launch at login requires a signed production app identity.").foregroundStyle(.secondary)
        }
        .padding()
        .frame(width: 420)
        .onAppear { notificationPreferences = notifications.preferences }
        .onChange(of: notificationPreferences) { _, preferences in notifications.preferences = preferences }
    }

    private func hourLabel(_ hour: Int) -> String { String(format: "%02d:00", hour) }
}
