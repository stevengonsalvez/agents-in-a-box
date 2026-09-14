#!/bin/bash
# ABOUTME: Authentication setup script for agents-in-a-box
# Runs OAuth login and stores credentials for container sessions

set -e

# Check if running in non-interactive mode
NON_INTERACTIVE=${NON_INTERACTIVE:-false}

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

log() {
    echo -e "${BLUE}[agents-box auth]${NC} $1"
}

warn() {
    echo -e "${YELLOW}[agents-box auth]${NC} $1"
}

error() {
    echo -e "${RED}[agents-box auth]${NC} $1"
}

success() {
    echo -e "${GREEN}[agents-box auth]${NC} $1"
}

log "Starting Claude authentication setup for agents-in-a-box"

# Set up environment for claude-user
export PATH="/home/claude-user/.npm-global/bin:$PATH"
export HOME=/home/claude-user

# Check if claude command is available
if ! command -v claude >/dev/null 2>&1; then
    error "Claude CLI not found in PATH: $PATH"
    error "Available commands:"
    ls -la /home/claude-user/.npm-global/bin/
    exit 1
fi

log "Claude CLI found at: $(which claude)"
log "Claude CLI version: $(claude --version 2>&1 || echo 'version check failed')"

# Ensure the .claude directory exists
mkdir -p /home/claude-user/.claude

# Check if credentials already exist
if [ -f /home/claude-user/.claude/.credentials.json ] && [ -s /home/claude-user/.claude/.credentials.json ]; then
    log "Existing credentials found. Checking if they contain valid OAuth tokens..."

    # Check if credentials contain required OAuth fields using jq
    if jq -e '.claudeAiOauth.accessToken' /home/claude-user/.claude/.credentials.json >/dev/null 2>&1; then
        # Check if token appears to be valid (not empty and looks like a token)
        ACCESS_TOKEN=$(jq -r '.claudeAiOauth.accessToken' /home/claude-user/.claude/.credentials.json 2>/dev/null)
        if [ -n "$ACCESS_TOKEN" ] && [ ${#ACCESS_TOKEN} -gt 10 ]; then
            success "Valid OAuth credentials found!"
            success "Authentication setup complete - you can now use claude-box sessions"
            exit 0
        else
            warn "Credentials file exists but access token appears invalid"
        fi
    else
        warn "Credentials file exists but doesn't contain valid OAuth tokens"
    fi

    log "Will re-authenticate to get fresh OAuth credentials..."
    rm -f /home/claude-user/.claude/.credentials.json
fi

log "No valid credentials found. Starting authentication process..."

# Check which authentication method to use (OAuth by default)
AUTH_METHOD=${AUTH_METHOD:-oauth}

if [ "$AUTH_METHOD" = "oauth" ]; then
    log ""
    log "Starting OAuth authentication flow..."
    log "This will generate a URL for you to open in your browser."
    log ""

    # Step 1: Generate OAuth URL using our custom OAuth script
    log "Generating OAuth URL..."
    OAUTH_URL=$(node /app/scripts/oauth-start.js)

    if [ $? -ne 0 ] || [ -z "$OAUTH_URL" ]; then
        error "Failed to generate OAuth URL"
        exit 1
    fi

    log ""
    success "Please copy the following URL and open it in your browser:"
    echo ""
    echo "$OAUTH_URL"
    echo ""

    # Step 2: Prompt for authorization code
    log "After completing the OAuth flow in your browser, you will be redirected to a page."
    log "Copy the authorization code from the URL or page and paste it here."
    echo ""
    echo -n "Enter authorization code: "
    # -s: the code is a credential, so it is not echoed onto the screen or
    # left in the terminal's scrollback.
    read -rs AUTHORIZATION_CODE
    echo ""

    if [ -z "$AUTHORIZATION_CODE" ]; then
        error "Authorization code is required"
        exit 1
    fi

    # Step 3: Complete OAuth exchange using our custom OAuth script
    log ""
    log "Exchanging authorization code for tokens..."

    if node /app/scripts/oauth-finish.js "$AUTHORIZATION_CODE"; then
        AUTH_SUCCESS=0
        log "OAuth token exchange completed successfully"
    else
        AUTH_SUCCESS=1
        error "OAuth token exchange failed"
    fi

elif [ "$AUTH_METHOD" = "token" ]; then
    log ""
    log "Starting API token authentication..."
    log "You'll be prompted to enter your Anthropic API token."
    log ""
    log "If you don't have an API token, get one from: https://console.anthropic.com/"
    log ""

    # Run Claude setup-token command (interactive)
    claude setup-token
    AUTH_SUCCESS=$?
else
    error "Unknown authentication method: $AUTH_METHOD"
    error "Supported methods: oauth, token"
    exit 1
fi

if [ $AUTH_SUCCESS -eq 0 ]; then
    success "Authentication successful!"

    # Check if credentials file was created successfully
    log "Verifying OAuth credentials were saved..."

    # Verify credentials were created
    if [ -f /home/claude-user/.claude/.credentials.json ] && [ -s /home/claude-user/.claude/.credentials.json ]; then
        success "Credentials saved to ~/.agents-in-a-box/auth/.credentials.json"

        # The OAuth credentials file contains everything needed for authentication
        # No additional .claude.json configuration file is required

        success ""
        success "🎉 Authentication setup complete!"
        success "You can now use agents-box sessions with these credentials."
        success ""
        success "To start a development session, run:"
        success "  agents-box session start"
    else
        error "Authentication succeeded but credentials file not found!"
        error "This may indicate an issue with the authentication process."
        exit 1
    fi
else
    error "Authentication failed!"
    error "Please try running the auth setup again."
    if [ "$NON_INTERACTIVE" = "true" ]; then
        error "Make sure you completed the OAuth flow in your browser."
    fi
    exit 1
fi
