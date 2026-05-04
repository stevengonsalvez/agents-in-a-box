// ABOUTME: Test UI display components including menu bar and help text

use ainb::app::{App, state::View};
use ainb::components::LayoutComponent;
use ainb::components::usage::{UsageProvider, UsageTab};
use ainb::models::{NamedUsage, UsageData, UsagePeriod};
use ratatui::{Terminal, backend::TestBackend};

#[cfg(feature = "test-support")]
fn sample_usage_data() -> UsageData {
    // Augment the shared fixture with the mcp_servers entry this test
    // file's UI assertions look for. The shared fixture stays minimal so
    // unit tests don't depend on mcp surface.
    let mut data = ainb::test_support::sample_usage_data();
    data.mcp_servers = vec![NamedUsage {
        name: "github".to_string(),
        calls: 1,
    }];
    data
}

// Fallback for cargo test invocations without --features test-support.
// Mirrors the prior inline fixture exactly so existing test runs are
// unaffected when the feature is off.
#[cfg(not(feature = "test-support"))]
fn sample_usage_data() -> UsageData {
    use ainb::models::{
        ActivityCategory, ActivityUsage, ModelUsage, ProjectUsage, ProviderCall, SessionUsage,
        TokenBucket,
    };
    use chrono::{TimeZone, Utc};
    let bucket = TokenBucket {
        input_tokens: 100,
        output_tokens: 50,
        call_count: 2,
        session_count: 1,
        project_count: 1,
        cost_usd: Some(0.42),
        ..TokenBucket::default()
    };
    UsageData {
        calls: vec![ProviderCall {
            provider: "claude".to_string(),
            model: "claude-sonnet-4-5".to_string(),
            session_id: "s1".to_string(),
            project: "agents-in-a-box".to_string(),
            project_path: "/tmp/agents-in-a-box".to_string(),
            timestamp: Utc.with_ymd_and_hms(2026, 4, 29, 10, 0, 0).unwrap(),
            input_tokens: 100,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            output_tokens: 50,
            reasoning_tokens: 0,
            cost_usd: Some(0.42),
            tools: vec!["Edit".to_string()],
            bash_commands: vec!["cargo test".to_string()],
            user_message: "implement burndown".to_string(),
            branch: None,
        }],
        daily: vec![(
            chrono::NaiveDate::from_ymd_opt(2026, 4, 29).unwrap(),
            bucket.clone(),
        )],
        weekly: vec![(
            chrono::NaiveDate::from_ymd_opt(2026, 4, 27).unwrap(),
            bucket.clone(),
        )],
        projects: vec![ProjectUsage {
            name: "agents-in-a-box".to_string(),
            path: "/tmp/agents-in-a-box".to_string(),
            bucket: bucket.clone(),
        }],
        grand_total: bucket.clone(),
        sessions: vec![SessionUsage {
            provider: "claude".to_string(),
            project: "agents-in-a-box".to_string(),
            session_id: "s1".to_string(),
            first_timestamp: Utc.with_ymd_and_hms(2026, 4, 29, 10, 0, 0).unwrap(),
            last_timestamp: Utc.with_ymd_and_hms(2026, 4, 29, 10, 10, 0).unwrap(),
            bucket: bucket.clone(),
        }],
        models: vec![ModelUsage {
            model: "claude-sonnet-4-5".to_string(),
            bucket: bucket.clone(),
        }],
        activities: vec![ActivityUsage {
            category: ActivityCategory::Feature,
            bucket: bucket.clone(),
            turns: 2,
            retries: 0,
            edit_turns: 1,
            one_shot_turns: 1,
        }],
        tools: vec![NamedUsage {
            name: "Edit".to_string(),
            calls: 1,
        }],
        shell_commands: vec![NamedUsage {
            name: "cargo test".to_string(),
            calls: 1,
        }],
        mcp_servers: vec![NamedUsage {
            name: "github".to_string(),
            calls: 1,
        }],
        ..UsageData::default()
    }
}

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
async fn test_usage_burndown_render_contains_core_panels() {
    let mut app = App::new();
    app.state.current_view = View::Analytics;
    app.state.usage_state.active_tab = UsageTab::Burndown;
    app.state.usage_state.data = Some(sample_usage_data());

    let backend = TestBackend::new(180, 52);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut layout = LayoutComponent::new();

    terminal
        .draw(|frame| {
            layout.render(frame, &mut app.state);
        })
        .unwrap();

    let content: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();

    assert!(content.contains("Burndown"));
    assert!(content.contains("Daily Activity"));
    assert!(content.contains("By Project"));
    assert!(content.contains("By Activity"));
    assert!(content.contains("By Model"));
    assert!(content.contains("Live Session Ticker"));
    assert!(content.contains("Optimization Recommendations"));
    assert!(content.contains("Budget"));
    assert!(content.contains("Agent Leaderboard"));
}

#[tokio::test]
async fn test_usage_burndown_cache_hit_uses_cache_reads_only() {
    let mut data = sample_usage_data();
    data.grand_total.input_tokens = 100;
    data.grand_total.cache_creation_tokens = 900;
    data.grand_total.cache_read_tokens = 0;
    data.daily[0].1.input_tokens = 100;
    data.daily[0].1.cache_creation_tokens = 900;
    data.daily[0].1.cache_read_tokens = 0;

    let mut app = App::new();
    app.state.current_view = View::Analytics;
    app.state.usage_state.active_tab = UsageTab::Burndown;
    app.state.usage_state.data = Some(data);

    let backend = TestBackend::new(180, 52);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut layout = LayoutComponent::new();

    terminal
        .draw(|frame| {
            layout.render(frame, &mut app.state);
        })
        .unwrap();

    let content: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();

    assert!(content.contains("Cache hit"));
    assert!(content.contains("0.0%"));
}

#[tokio::test]
async fn test_usage_burndown_projection_uses_elapsed_custom_range_days() {
    let mut data = sample_usage_data();
    data.grand_total.cost_usd = Some(10.0);
    data.daily[0].1.cost_usd = Some(10.0);

    let mut app = App::new();
    app.state.current_view = View::Analytics;
    app.state.usage_state.active_tab = UsageTab::Burndown;
    app.state.usage_state.period = UsagePeriod::Custom {
        from: chrono::NaiveDate::from_ymd_opt(2026, 4, 23).unwrap(),
        to: chrono::NaiveDate::from_ymd_opt(2026, 4, 29).unwrap(),
    };
    app.state.usage_state.data = Some(data);

    let backend = TestBackend::new(180, 52);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut layout = LayoutComponent::new();

    terminal
        .draw(|frame| {
            layout.render(frame, &mut app.state);
        })
        .unwrap();

    let content: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();

    assert!(content.contains("Projected month"));
    assert!(content.contains("$42.86"));
    assert!(!content.contains("$300.00"));
}

#[tokio::test]
async fn test_usage_burndown_narrow_render_does_not_panic() {
    let mut app = App::new();
    app.state.current_view = View::Analytics;
    app.state.usage_state.active_tab = UsageTab::Burndown;
    app.state.usage_state.data = Some(sample_usage_data());

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut layout = LayoutComponent::new();

    terminal
        .draw(|frame| {
            layout.render(frame, &mut app.state);
        })
        .unwrap();

    let content: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();

    assert!(content.contains("Burndown"));
    assert!(content.contains("Daily Activity"));
}

#[tokio::test]
async fn test_usage_render_uses_loaded_data_before_legacy_provider_gate() {
    let mut app = App::new();
    app.state.current_view = View::Analytics;
    app.state.usage_state.active_tab = UsageTab::Burndown;
    app.state.usage_state.provider = UsageProvider::Gemini;
    app.state.usage_state.data = Some(sample_usage_data());

    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut layout = LayoutComponent::new();

    terminal
        .draw(|frame| {
            layout.render(frame, &mut app.state);
        })
        .unwrap();

    let content: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();

    assert!(content.contains("Burndown"));
    assert!(content.contains("Daily Activity"));
    assert!(!content.contains("No Data"));
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
