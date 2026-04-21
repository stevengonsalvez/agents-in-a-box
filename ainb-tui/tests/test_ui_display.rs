// ABOUTME: Test UI display components including menu bar and help text

use ainb::app::{App, state::View};
use ainb::components::LayoutComponent;
use ratatui::{Terminal, backend::TestBackend};

#[tokio::test]
async fn test_bottom_menu_bar_shows_refresh_key() {
    let mut app = App::new();
    app.state.load_mock_data();

    // Force the view to SessionList to bypass auth check in tests
    app.state.current_view = View::SessionList;
    app.state.auth_setup_state = None; // Clear any auth setup state

    let backend = TestBackend::new(180, 40); // Wider to fit full menu bar
    let mut terminal = Terminal::new(backend).unwrap();
    let mut layout = LayoutComponent::new();

    // Render the UI
    terminal
        .draw(|frame| {
            layout.render(frame, &mut app.state);
        })
        .unwrap();

    // Get the rendered buffer content
    let buffer = terminal.backend().buffer();
    let content: String = buffer.content().iter().map(ratatui::buffer::Cell::symbol).collect();

    // Debug output to see what was actually rendered
    let printable_content = content
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .collect::<String>();

    // Check that the bottom bar contains the refresh key
    assert!(
        content.contains("[f]refresh"),
        "Bottom menu bar should contain '[f]refresh' but content was: {}",
        printable_content
    );

    // Also check for other expected menu items to ensure we're looking at the right place
    assert!(
        content.contains("[n]ew"),
        "Should contain '[n]ew' but content was: {}",
        printable_content
    );
    assert!(
        content.contains("[?]help"),
        "Should contain '[?]help' but content was: {}",
        printable_content
    );
    assert!(
        content.contains("[q]uit"),
        "Should contain '[q]uit' but content was: {}",
        printable_content
    );
}

#[tokio::test]
async fn test_help_screen_shows_refresh_key() {
    let mut app = App::new();
    app.state.load_mock_data();

    // Show help
    app.state.help_visible = true;

    let backend = TestBackend::new(180, 40); // Wider to fit full menu bar
    let mut terminal = Terminal::new(backend).unwrap();
    let mut layout = LayoutComponent::new();

    // Render the UI with help visible
    terminal
        .draw(|frame| {
            layout.render(frame, &mut app.state);
        })
        .unwrap();

    // Get the rendered buffer content
    let buffer = terminal.backend().buffer();
    let content: String = buffer.content().iter().map(ratatui::buffer::Cell::symbol).collect();

    // Check that help contains the refresh key
    assert!(
        content.contains("f          Refresh workspaces"),
        "Help screen should contain 'f          Refresh workspaces' but content was: {}",
        content
            .chars()
            .filter(|c| c.is_ascii_graphic() || *c == ' ')
            .collect::<String>()
    );

    // Check that it's under Session Actions
    assert!(
        content.contains("Session Actions:"),
        "Should contain 'Session Actions:' section"
    );

    // Verify other help items are present
    assert!(
        content.contains("Navigation:"),
        "Should contain 'Navigation:' section"
    );
    assert!(
        content.contains("General:"),
        "Should contain 'General:' section"
    );
}

#[tokio::test]
async fn test_refresh_key_in_help_under_session_actions() {
    let mut app = App::new();
    app.state.load_mock_data();
    app.state.help_visible = true;

    let backend = TestBackend::new(180, 40); // Wider to fit full menu bar
    let mut terminal = Terminal::new(backend).unwrap();
    let mut layout = LayoutComponent::new();

    terminal
        .draw(|frame| {
            layout.render(frame, &mut app.state);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let content: String = buffer.content().iter().map(ratatui::buffer::Cell::symbol).collect();

    // Find the position of "Session Actions:"
    let session_actions_pos = content.find("Session Actions:");
    assert!(
        session_actions_pos.is_some(),
        "Should find 'Session Actions:' section"
    );

    // Find the position of "Views:" which comes after Session Actions
    let views_pos = content.find("Views:");
    assert!(views_pos.is_some(), "Should find 'Views:' section");

    // Find the refresh key entry
    let refresh_pos = content.find("f          Refresh workspaces");
    assert!(refresh_pos.is_some(), "Should find refresh key entry");

    // Verify that refresh key appears between Session Actions and Views
    let session_pos = session_actions_pos.unwrap();
    let view_pos = views_pos.unwrap();
    let ref_pos = refresh_pos.unwrap();

    assert!(
        ref_pos > session_pos && ref_pos < view_pos,
        "Refresh key should appear between Session Actions and Views sections. \
        Session Actions at {session_pos}, Refresh at {ref_pos}, Views at {view_pos}"
    );
}
