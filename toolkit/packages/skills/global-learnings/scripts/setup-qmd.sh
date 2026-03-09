#!/bin/bash
# setup-qmd.sh - Install and configure QMD as a dual search backend
#
# Installs the qmd CLI, adds the learnings collection, and runs initial
# embedding. Idempotent.
set -euo pipefail

LEARNINGS_HOME="${LEARNINGS_HOME:-$HOME/.learnings}"

log() { echo "[setup-qmd] $*"; }

# --- Ensure qmd is installed ---

if command -v qmd &>/dev/null; then
    log "qmd is already installed: $(qmd --version 2>/dev/null || echo 'unknown version')"
else
    log "Installing qmd via npm..."
    if ! command -v npm &>/dev/null; then
        echo "Error: npm is required to install qmd. Install Node.js first." >&2
        exit 1
    fi
    npm install -g @tobilu/qmd
    log "qmd installed successfully"
fi

# --- macOS: ensure brew sqlite for extension support ---

if [ "$(uname)" = "Darwin" ]; then
    if command -v brew &>/dev/null; then
        if ! brew list sqlite &>/dev/null 2>&1; then
            log "Installing sqlite via brew for extension support..."
            brew install sqlite
        else
            log "brew sqlite already installed"
        fi
    else
        log "Warning: Homebrew not found. Install sqlite manually if QMD has issues."
    fi
fi

# --- Add learnings collection ---

DOCS_DIR="$LEARNINGS_HOME/documents"

if [ ! -d "$DOCS_DIR" ]; then
    log "Documents directory not found at $DOCS_DIR"
    log "Run setup-learnings-home.sh first."
    exit 1
fi

# Check if collection already exists
if qmd collection list 2>/dev/null | grep -q "learnings"; then
    log "QMD collection 'learnings' already exists"
else
    log "Adding QMD collection: learnings -> $DOCS_DIR"
    qmd collection add "$DOCS_DIR" --name learnings
fi

# --- Set context hierarchy ---

log "Setting context hierarchy for learnings collection"
qmd context set --collection learnings --hierarchy "clusters > episodes > learnings" 2>/dev/null || true

# --- Run initial embedding ---

log "Running initial embedding..."
qmd embed
log "Done. QMD is configured for the learnings knowledge base."
