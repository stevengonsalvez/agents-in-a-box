# End-to-End PTY Testing Guide

This document explains how to use the PTY-based end-to-end tests for the TUI application.

## Overview

We use **rexpect** (Rust equivalent of `expect`) to test the actual terminal behavior of the application. These tests spawn the real application in a PTY (pseudo-terminal) and interact with it like a real user would.

## Why PTY Testing?

### Current TestBackend Tests
✅ Fast
✅ Test business logic
✅ No external dependencies
❌ Don't test actual terminal rendering
❌ Don't catch ANSI escape code issues
❌ Don't test event loop timing

### PTY-Based E2E Tests
✅ Test actual terminal behavior
✅ Catch rendering issues
✅ Test timing and responsiveness
✅ Verify user experience
❌ Slower (but still fast enough)
❌ Unix/macOS only (Linux/Mac support)

## Running the Tests

### Run All E2E Tests
```bash
cargo test --test e2e_pty_tests -- --ignored --test-threads=1
```

### Run Specific Test
```bash
cargo test --test e2e_pty_tests test_e2e_new_session_flow -- --ignored
```

### Run with Visual Layout Tests (requires vt100 feature)
```bash
cargo test --test e2e_pty_tests --features vt100-tests -- --ignored
```

### Run with Visual Debug Mode (watch tests in live terminal)
```bash
cargo test test_visual_delete -- --ignored --features visual-debug --nocapture
```
**Note**: This opens a separate terminal window on macOS where you can watch the test execute in real-time (like Playwright headed mode).

### Why `--ignored`?
E2E tests are marked with `#[ignore]` because:
- They spawn actual processes (slower)
- They require terminal/PTY support
- Best run manually or in CI, not on every `cargo test`

### Why `--test-threads=1`?
- Prevents parallel PTY sessions from interfering
- Ensures stable test execution
- Avoids terminal rendering conflicts

## Test Structure

### Basic Test Pattern
```rust
#[test]
#[ignore]
fn test_e2e_example() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Spawn the application
    let mut session = spawn_app()?;

    // 2. Wait for expected output
    session.exp_string("Select a session")?;

    // 3. Send input (simulate key press)
    session.send("n")?;

    // 4. Verify response
    session.exp_string("New Session")?;

    // 5. Clean up
    session.send("\x1b")?; // Escape key

    Ok(())
}
```

### Available Methods

#### Waiting for Output
```rust
// Wait for exact string
session.exp_string("Hello")?;

// Wait for regex pattern
session.exp_regex(r"Creating.*session")?;

// Wait for EOF (app exits)
session.exp_eof()?;
```

#### Sending Input
```rust
// Send single character
session.send("n")?;

// Send Enter key
session.send("\r")?;

// Send Escape key
session.send("\x1b")?;

// Send Control+C
session.send("\x03")?;
```

#### Reading Output
```rust
// Try to read without blocking
let output = session.try_read()?;

// Read line
let line = session.read_line()?;
```

## Example Tests

### Test 1: New Session Flow
```rust
#[test]
#[ignore]
fn test_e2e_new_session_flow() -> Result<(), Box<dyn std::error::Error>> {
    let mut session = spawn_app()?;

    session.exp_string("Select a session")?;
    session.send("n")?;
    session.exp_string("New Session")?;
    session.send("\r")?;
    session.exp_regex("Creating.*session")?;

    Ok(())
}
```

### Test 2: Responsiveness Check
```rust
#[test]
#[ignore]
fn test_e2e_responsive_ui() -> Result<(), Box<dyn std::error::Error>> {
    let mut session = spawn_app()?;
    session.exp_string("Select a session")?;

    let start = std::time::Instant::now();
    session.send("n")?;
    session.exp_string("New Session")?;
    let elapsed = start.elapsed();

    assert!(elapsed < Duration::from_millis(500),
            "Dialog took too long: {:?}", elapsed);

    Ok(())
}
```

### Test 3: Visual Layout (with vt100)
```rust
#[cfg(feature = "vt100-tests")]
#[test]
#[ignore]
fn test_e2e_visual_layout() -> Result<(), Box<dyn std::error::Error>> {
    use vt100::Parser;

    let mut session = spawn_app()?;
    let mut parser = Parser::new(40, 120, 0);

    session.exp_string("Select a session")?;
    let output = session.try_read()?;
    parser.process(output.as_bytes());

    let screen = parser.screen();
    let contents = screen.contents();

    assert!(contents.contains("Select a session"));

    Ok(())
}
```

## Debugging Tests

### Enable Verbose Output
```bash
RUST_LOG=debug cargo test --test e2e_pty_tests test_name -- --ignored --nocapture
```

### Common Issues

#### Test Hangs
- Check timeout in `spawn_app()` (default: 10 seconds)
- Verify expected string actually appears
- Use `try_read()` to see what output is available

#### Expected String Not Found
```rust
// Debug: Print what we got
let output = session.try_read()?;
println!("Got output: {:?}", output);
```

#### PTY Not Available
- Run on Unix/macOS/Linux (not Windows)
- Or use WSL on Windows

## CI/CD Integration

### GitHub Actions Example
```yaml
name: E2E Tests

on: [push, pull_request]

jobs:
  e2e:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
      - name: Run E2E Tests
        run: cargo test --test e2e_pty_tests -- --ignored --test-threads=1
```

## Best Practices

### ✅ Do
- Use `#[ignore]` for E2E tests
- Add timeouts to prevent hanging
- Clean up (press Escape, quit) at end of test
- Use `--test-threads=1` to prevent conflicts
- Test happy paths and common workflows

### ❌ Don't
- Don't test every edge case (use unit tests for that)
- Don't make tests too long (split into multiple tests)
- Don't rely on exact timing (use reasonable timeouts)
- Don't test internal implementation details

## Visual Testing Modes

### Silent Mode (Default)
```bash
cargo test --test e2e_pty_tests -- --ignored
```
- Headless execution
- Fast, CI-friendly
- Like Playwright headless

### Live Visual Debug Mode
```bash
cargo test test_visual_delete -- --ignored --features visual-debug
```
- Opens terminal window
- Watch test execute live
- Like Playwright headed mode
- macOS/Linux/WSL only

### Screen Verification Mode
```bash
cargo test --features vt100-tests -- --ignored
```
- Parse terminal state with vt100
- Verify exact layout
- Check colors, cursor position

## Creating Demos with VHS

### Record All Demos
```bash
./scripts/record-demos.sh
```

### Record Single Demo
```bash
vhs tests/tapes/delete-session.tape
```

### Tape File Format
```tape
Output path/to/output.gif
Set Theme "Dracula"
Set Width 1280
Set Height 800

Type "command"
Enter
Sleep 2s
```

See [VHS Documentation](https://github.com/charmbracelet/vhs) for full syntax.

## Comparison with Other Approaches

### TestBackend (Current)
```rust
// Fast, tests logic
let mut ui = UITestFramework::new().await;
ui.press_key(KeyCode::Char('n')).unwrap();
ui.process_async().await.unwrap();
assert_eq!(ui.current_view(), &View::NewSession);
```

### rexpect (PTY-based)
```rust
// Tests actual UX
let mut session = spawn_app()?;
session.exp_string("Select a session")?;
session.send("n")?;
session.exp_string("New Session")?;
```

### Microsoft TUI Test (Node.js)
```typescript
// Cross-platform, snapshots
await terminal.spawn('cargo run');
await terminal.waitForText('Select a session');
await terminal.keyboard.type('n');
await expect(terminal).toContainText('New Session');
```

## Next Steps

1. **Start with rexpect** - Add basic E2E tests
2. **Add vt100 later** - If you need visual verification
3. **Consider tui-test** - If you need Windows support

## Resources

- [rexpect GitHub](https://github.com/rust-cli/rexpect)
- [vt100 crate](https://docs.rs/vt100)
- [Microsoft TUI Test](https://github.com/microsoft/tui-test)
- [Testing TUI Apps Blog](https://blog.waleedkhan.name/testing-tui-apps/)

## Questions?

- Check the test examples in `tests/e2e_pty_tests.rs`
- Look at rexpect documentation
- Ask on the project's issue tracker
