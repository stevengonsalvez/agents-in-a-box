# Claude-in-a-Box Tests

This directory contains test scripts for various Claude-in-a-Box functionalities.

## OAuth Token Testing

### Quick Start

```bash
# Check current token status
./tests/test-oauth.sh status

# Force a token refresh (for testing)
./tests/test-oauth.sh force

# Watch token status in real-time
./tests/test-oauth.sh watch
```

### Available Commands

| Command | Description | Use Case |
|---------|-------------|----------|
| `status` | Show current token status | Check when token expires |
| `refresh` | Try to refresh token (only if needed) | Test normal refresh flow |
| `force` | Force refresh even if token is valid | Test refresh mechanism |
| `expire` | Set token to expire in 5 minutes | Test TUI auto-refresh |
| `startup` | Test automatic refresh on TUI startup | Verify startup refresh works |
| `corrupt` | Corrupt refresh token | Test error handling |
| `restore` | Restore from backup | Recover after testing |
| `watch` | Monitor token status continuously | Real-time monitoring |

### Testing Scenarios

#### 1. Test Automatic Refresh on Startup
```bash
# Test that TUI refreshes expired tokens on startup
./tests/test-oauth.sh startup

# This will:
# - Set token to expire soon
# - Start the TUI
# - Verify token gets refreshed automatically
```

#### 2. Test Automatic Refresh in TUI
```bash
# Make token expire soon
./tests/test-oauth.sh expire

# Run TUI with debug logging
RUST_LOG=info cargo run

# Watch for automatic refresh (happens within 5 minutes)
```

#### 3. Test Force Refresh
```bash
# Force a refresh to get new token
./tests/test-oauth.sh force

# Verify token changed
./tests/test-oauth.sh status
```

#### 4. Test Error Handling
```bash
# Corrupt the refresh token
./tests/test-oauth.sh corrupt

# Try to refresh (should fail gracefully)
./tests/test-oauth.sh refresh

# Restore working credentials
./tests/test-oauth.sh restore
```

#### 5. Test with Running Containers
```bash
# Start a session
claude-box session start

# In another terminal, force refresh
./tests/test-oauth.sh force

# Verify session still works with new token
```

### Debug Mode

Enable detailed debug output:
```bash
DEBUG=1 ./tests/test-oauth.sh refresh
```

### Continuous Monitoring

Watch token status in real-time:
```bash
./tests/test-oauth.sh watch
```

## Other Tests

Additional test scripts will be added here as functionality expands.
