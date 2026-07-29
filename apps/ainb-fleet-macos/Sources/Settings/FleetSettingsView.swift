import SwiftUI

struct FleetSettingsView: View {
    @Binding var presentation: FleetPresentationPreferences
    @State private var notifications = FleetNotificationCenter()
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
    }
}
