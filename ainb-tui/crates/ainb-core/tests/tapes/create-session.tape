Output tests/recordings/create-session.gif
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

# Create new session
Type "n"
Sleep 1s

# Select repository
Down
Down
Enter
Sleep 2s

# Show created session
Sleep 2s

# Quit
Type "q"
Sleep 500ms
