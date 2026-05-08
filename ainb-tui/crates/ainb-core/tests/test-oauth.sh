#!/bin/bash
# ABOUTME: Comprehensive OAuth token testing script
# Tests OAuth token refresh functionality with multiple modes

set -e

# Colors for output
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
AUTH_DIR="$HOME/.agents-in-a-box/auth"
CREDENTIALS_FILE="$AUTH_DIR/.credentials.json"
BACKUP_FILE="$AUTH_DIR/.credentials.backup.json"

# Help function
show_help() {
    echo "OAuth Token Test Script"
    echo ""
    echo "Usage: $0 [COMMAND]"
    echo ""
    echo "Commands:"
    echo "  status    - Show current token status (default)"
    echo "  refresh   - Try to refresh token (only if needed)"
    echo "  force     - Force refresh even if token is valid"
    echo "  expire    - Set token to expire in 5 minutes (for testing)"
    echo "  startup   - Test automatic refresh on TUI startup"
    echo "  corrupt   - Corrupt refresh token to test error handling"
    echo "  restore   - Restore from backup"
    echo "  watch     - Monitor token status continuously"
    echo "  help      - Show this help message"
    echo ""
    echo "Examples:"
    echo "  $0 status           # Check current token status"
    echo "  $0 force            # Force a token refresh"
    echo "  $0 startup          # Test TUI startup with expired token"
    echo "  $0 expire           # Make token expire soon for testing"
    echo ""
}

# Check prerequisites
check_prerequisites() {
    if [ ! -f "$CREDENTIALS_FILE" ]; then
        echo -e "${RED}Error: No credentials found at $CREDENTIALS_FILE${NC}"
        echo "Please run 'agents-box auth' first to set up authentication"
        exit 1
    fi

    # Check if Docker image exists
    if ! docker image inspect agents-box:claude-dev >/dev/null 2>&1; then
        echo -e "${RED}Error: Docker image agents-box:claude-dev not found${NC}"
        echo "Please build the container first: docker build -t agents-box:claude-dev docker/claude-dev"
        exit 1
    fi
}

# Create backup
create_backup() {
    cp "$CREDENTIALS_FILE" "$BACKUP_FILE"
    echo -e "${BLUE}Backup created at $BACKUP_FILE${NC}"
}

# Show token status
show_status() {
    echo -e "${YELLOW}=== OAuth Token Status ===${NC}"
    echo ""

    # Extract token info
    local EXPIRES_AT=$(jq -r '.claudeAiOauth.expiresAt' "$CREDENTIALS_FILE" 2>/dev/null)
    local ACCESS_TOKEN=$(jq -r '.claudeAiOauth.accessToken' "$CREDENTIALS_FILE" 2>/dev/null | cut -c1-20)
    local REFRESH_TOKEN=$(jq -r '.claudeAiOauth.refreshToken' "$CREDENTIALS_FILE" 2>/dev/null)

    if [ -z "$EXPIRES_AT" ] || [ "$EXPIRES_AT" = "null" ]; then
        echo -e "${RED}Error: No OAuth token found${NC}"
        exit 1
    fi

    # Calculate time remaining
    local NOW=$(date +%s000)
    local TIME_LEFT=$(( (EXPIRES_AT - NOW) / 1000 / 60 ))

    # Convert to human-readable format
    local EXPIRES_DATE=$(date -r $((EXPIRES_AT / 1000)) "+%Y-%m-%d %H:%M:%S" 2>/dev/null || date -d "@$((EXPIRES_AT / 1000))" "+%Y-%m-%d %H:%M:%S" 2>/dev/null)

    echo -e "${GREEN}✓ Access token (first 20 chars): ${ACCESS_TOKEN}...${NC}"
    echo -e "${GREEN}✓ Expires at: $EXPIRES_DATE${NC}"

    if [ $TIME_LEFT -gt 60 ]; then
        local HOURS=$((TIME_LEFT / 60))
        local MINS=$((TIME_LEFT % 60))
        echo -e "${GREEN}✓ Time remaining: ${HOURS}h ${MINS}m${NC}"
    elif [ $TIME_LEFT -gt 0 ]; then
        echo -e "${YELLOW}⚠ Time remaining: ${TIME_LEFT} minutes${NC}"
    else
        echo -e "${RED}✗ Token has expired${NC}"
    fi

    if [ ! -z "$REFRESH_TOKEN" ] && [ "$REFRESH_TOKEN" != "null" ]; then
        echo -e "${GREEN}✓ Refresh token available${NC}"
    else
        echo -e "${RED}✗ No refresh token available${NC}"
    fi
}

# Run refresh
run_refresh() {
    local DEBUG_FLAG=""
    if [ "${DEBUG:-0}" = "1" ]; then
        DEBUG_FLAG="-e DEBUG=1"
    fi

    echo -e "${YELLOW}Running OAuth token refresh...${NC}"

    docker run --rm \
        -v "$AUTH_DIR:/home/claude-user/.claude" \
        -e "PATH=/home/claude-user/.npm-global/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
        -e "HOME=/home/claude-user" \
        $DEBUG_FLAG \
        -w "/home/claude-user" \
        --user "claude-user" \
        --entrypoint "node" \
        agents-box:claude-dev \
        /app/scripts/oauth-refresh.js

    return $?
}

# Try to refresh (only if needed)
try_refresh() {
    echo -e "${YELLOW}=== OAuth Token Refresh Test ===${NC}"
    echo ""

    show_status
    echo ""

    run_refresh

    if [ $? -eq 0 ]; then
        echo ""
        echo -e "${GREEN}✅ Refresh test completed successfully!${NC}"

        # Show new status
        local NEW_EXPIRES_AT=$(jq -r '.claudeAiOauth.expiresAt' "$CREDENTIALS_FILE" 2>/dev/null)
        if [ ! -z "$NEW_EXPIRES_AT" ] && [ "$NEW_EXPIRES_AT" != "null" ]; then
            local NEW_EXPIRES_DATE=$(date -r $((NEW_EXPIRES_AT / 1000)) "+%Y-%m-%d %H:%M:%S" 2>/dev/null || date -d "@$((NEW_EXPIRES_AT / 1000))" "+%Y-%m-%d %H:%M:%S" 2>/dev/null)
            echo -e "${GREEN}✓ Token now expires at: $NEW_EXPIRES_DATE${NC}"
        fi
    else
        echo -e "${RED}❌ Refresh failed${NC}"
        exit 1
    fi
}

# Force refresh
force_refresh() {
    echo -e "${YELLOW}=== Force Token Refresh ===${NC}"
    echo ""

    create_backup

    # Get current token
    local OLD_ACCESS_TOKEN=$(jq -r '.claudeAiOauth.accessToken' "$CREDENTIALS_FILE" 2>/dev/null | cut -c1-20)
    echo -e "${BLUE}Current token: ${OLD_ACCESS_TOKEN}...${NC}"

    # Temporarily make token expire soon
    echo -e "${YELLOW}Marking token as expiring soon...${NC}"
    local TEMP_CREDS=$(mktemp)
    local TEMP_EXPIRE=$(($(date +%s000) + 300000))
    jq --argjson expire "$TEMP_EXPIRE" '.claudeAiOauth.expiresAt = $expire' "$CREDENTIALS_FILE" > "$TEMP_CREDS"
    cp "$TEMP_CREDS" "$CREDENTIALS_FILE"

    # Run refresh
    run_refresh
    local REFRESH_STATUS=$?

    # Clean up
    rm -f "$TEMP_CREDS"

    if [ $REFRESH_STATUS -eq 0 ]; then
        echo ""
        echo -e "${GREEN}✅ Force refresh completed!${NC}"

        # Check if token changed
        local NEW_ACCESS_TOKEN=$(jq -r '.claudeAiOauth.accessToken' "$CREDENTIALS_FILE" 2>/dev/null | cut -c1-20)

        if [ "$OLD_ACCESS_TOKEN" != "$NEW_ACCESS_TOKEN" ]; then
            echo -e "${GREEN}✓ New token: ${NEW_ACCESS_TOKEN}...${NC}"
        else
            echo -e "${YELLOW}⚠ Token appears unchanged${NC}"
        fi

        # Show new expiration
        local NEW_EXPIRES_AT=$(jq -r '.claudeAiOauth.expiresAt' "$CREDENTIALS_FILE" 2>/dev/null)
        local NEW_EXPIRES_DATE=$(date -r $((NEW_EXPIRES_AT / 1000)) "+%Y-%m-%d %H:%M:%S" 2>/dev/null || date -d "@$((NEW_EXPIRES_AT / 1000))" "+%Y-%m-%d %H:%M:%S" 2>/dev/null)
        echo -e "${GREEN}✓ New expiration: $NEW_EXPIRES_DATE${NC}"
    else
        echo -e "${RED}❌ Force refresh failed${NC}"
        # Try to restore
        if [ -f "$BACKUP_FILE" ]; then
            cp "$BACKUP_FILE" "$CREDENTIALS_FILE"
            echo -e "${YELLOW}Restored from backup${NC}"
        fi
        exit 1
    fi
}

# Set token to expire soon
set_expire_soon() {
    echo -e "${YELLOW}=== Setting Token to Expire Soon ===${NC}"
    echo ""

    create_backup

    # Set to expire in 5 minutes
    local EXPIRE_TIME=$(($(date +%s000) + 300000))
    jq --argjson expire "$EXPIRE_TIME" '.claudeAiOauth.expiresAt = $expire' "$CREDENTIALS_FILE" > "$CREDENTIALS_FILE.tmp"
    mv "$CREDENTIALS_FILE.tmp" "$CREDENTIALS_FILE"

    echo -e "${GREEN}✓ Token set to expire in 5 minutes${NC}"
    echo -e "${BLUE}This will trigger automatic refresh in the TUI${NC}"
    echo ""

    show_status
}

# Corrupt refresh token for error testing
corrupt_token() {
    echo -e "${YELLOW}=== Corrupting Refresh Token ===${NC}"
    echo ""

    create_backup

    # Corrupt the refresh token
    jq '.claudeAiOauth.refreshToken = "invalid-refresh-token"' "$CREDENTIALS_FILE" > "$CREDENTIALS_FILE.tmp"
    mv "$CREDENTIALS_FILE.tmp" "$CREDENTIALS_FILE"

    echo -e "${YELLOW}Refresh token corrupted for testing${NC}"
    echo -e "${BLUE}Try running refresh to test error handling:${NC}"
    echo "  $0 refresh"
    echo ""
    echo -e "${YELLOW}Remember to restore after testing:${NC}"
    echo "  $0 restore"
}

# Restore from backup
restore_backup() {
    if [ ! -f "$BACKUP_FILE" ]; then
        echo -e "${RED}Error: No backup found at $BACKUP_FILE${NC}"
        exit 1
    fi

    cp "$BACKUP_FILE" "$CREDENTIALS_FILE"
    echo -e "${GREEN}✓ Credentials restored from backup${NC}"
    echo ""

    show_status
}

# Test startup with expired token
test_startup_refresh() {
    echo -e "${YELLOW}=== Testing OAuth Refresh on TUI Startup ===${NC}"
    echo ""

    create_backup

    # Get current token info
    local OLD_ACCESS_TOKEN=$(jq -r '.claudeAiOauth.accessToken' "$CREDENTIALS_FILE" 2>/dev/null | cut -c1-20)
    echo -e "${BLUE}Current token: ${OLD_ACCESS_TOKEN}...${NC}"

    # Set token to expire in 25 minutes (triggers 30-min refresh buffer)
    echo -e "${YELLOW}Setting token to expire in 25 minutes...${NC}"
    local EXPIRE_TIME=$(($(date +%s000) + 1500000))  # 25 minutes from now
    jq --argjson expire "$EXPIRE_TIME" '.claudeAiOauth.expiresAt = $expire' "$CREDENTIALS_FILE" > "$CREDENTIALS_FILE.tmp"
    mv "$CREDENTIALS_FILE.tmp" "$CREDENTIALS_FILE"

    echo ""
    echo -e "${YELLOW}Starting TUI with soon-to-expire token...${NC}"
    echo -e "${BLUE}The TUI should:${NC}"
    echo -e "  1. Detect the token needs refresh"
    echo -e "  2. Automatically refresh it on startup"
    echo -e "  3. NOT show the auth setup screen"
    echo -e "  4. Go directly to the session list"
    echo ""
    echo -e "${YELLOW}Running: RUST_LOG=info cargo run${NC}"
    echo -e "${BLUE}Watch for these log messages:${NC}"
    echo -e "  - 'OAuth token needs refresh on startup'"
    echo -e "  - 'OAuth tokens refreshed successfully on startup'"
    echo -e "  - 'Found refresh token - can refresh if needed'"
    echo ""
    echo -e "${GREEN}Press Ctrl+C to exit the TUI when done testing${NC}"
    echo ""

    # Run the TUI with info logging
    RUST_LOG=info cargo run

    # After TUI exits, check if token was refreshed
    echo ""
    echo -e "${YELLOW}Checking results...${NC}"

    local NEW_ACCESS_TOKEN=$(jq -r '.claudeAiOauth.accessToken' "$CREDENTIALS_FILE" 2>/dev/null | cut -c1-20)
    local NEW_EXPIRES_AT=$(jq -r '.claudeAiOauth.expiresAt' "$CREDENTIALS_FILE" 2>/dev/null)

    if [ "$OLD_ACCESS_TOKEN" != "$NEW_ACCESS_TOKEN" ]; then
        echo -e "${GREEN}✅ Success! Token was refreshed on startup${NC}"
        echo -e "${GREEN}  Old token: ${OLD_ACCESS_TOKEN}...${NC}"
        echo -e "${GREEN}  New token: ${NEW_ACCESS_TOKEN}...${NC}"

        # Show new expiration
        local NEW_EXPIRES_DATE=$(date -r $((NEW_EXPIRES_AT / 1000)) "+%Y-%m-%d %H:%M:%S" 2>/dev/null || date -d "@$((NEW_EXPIRES_AT / 1000))" "+%Y-%m-%d %H:%M:%S" 2>/dev/null)
        echo -e "${GREEN}  New expiration: $NEW_EXPIRES_DATE${NC}"
    else
        echo -e "${YELLOW}⚠ Token was not refreshed (may not have needed it)${NC}"
    fi

    # Option to restore
    echo ""
    echo -e "${BLUE}To restore original credentials:${NC}"
    echo "  $0 restore"
}

# Watch token status
watch_status() {
    echo -e "${YELLOW}Monitoring token status (Ctrl+C to stop)...${NC}"
    echo ""

    while true; do
        clear
        show_status
        echo ""
        echo -e "${BLUE}Refreshing every 10 seconds...${NC}"
        sleep 10
    done
}

# Main execution
check_prerequisites

# Parse command
COMMAND="${1:-status}"

case "$COMMAND" in
    status)
        show_status
        ;;
    refresh)
        try_refresh
        ;;
    force)
        force_refresh
        ;;
    expire)
        set_expire_soon
        ;;
    startup)
        test_startup_refresh
        ;;
    corrupt)
        corrupt_token
        ;;
    restore)
        restore_backup
        ;;
    watch)
        watch_status
        ;;
    help|--help|-h)
        show_help
        ;;
    *)
        echo -e "${RED}Unknown command: $COMMAND${NC}"
        echo ""
        show_help
        exit 1
        ;;
esac
