import SwiftUI

struct FleetSettingsView: View {
    @Binding var presentation: FleetPresentationPreferences

    var body: some View {
        Form {
            Text("Fleet presentation preferences remain local to this Mac.")
            Toggle("Needs you filter", isOn: $presentation.filters.attentionOnly)
            Picker("Roster sort", selection: $presentation.sort) {
                ForEach(FleetRosterSort.allCases) { sort in
                    Text(sort.label).tag(sort)
                }
            }
            Text("Launch at login requires a signed production app identity.").foregroundStyle(.secondary)
        }
        .padding()
        .frame(width: 420)
    }
}
