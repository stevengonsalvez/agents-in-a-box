Output tests/recordings/delete-session.gif
Set Theme "Dracula"
Set Width 1280
Set Height 800
Set PlaybackSpeed 0.8
Set FontSize 16
Set Padding 20

# Start the app
Type "cargo run --release"
Enter
Sleep 3s

# Show the session list
Sleep 2s

# Delete a session
Type "d"
Sleep 1s

# Confirm deletion
Enter
Sleep 2s

# Show result
Sleep 1s

# Quit
Type "q"
Sleep 500ms
