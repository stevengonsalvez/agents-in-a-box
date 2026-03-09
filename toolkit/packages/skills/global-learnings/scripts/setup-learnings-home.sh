#!/bin/bash
# setup-learnings-home.sh - Migrate global learnings to agent-agnostic ~/.learnings/
#
# Creates the directory structure, migrates existing documents, sets up
# backward-compat symlink, and configures shell profile. Idempotent.
set -euo pipefail

LEARNINGS_HOME="${LEARNINGS_HOME:-$HOME/.learnings}"
OLD_HOME="$HOME/.claude/global-learnings"

log() { echo "[setup-learnings-home] $*"; }

# --- Create directory structure ---

log "Ensuring directory structure at $LEARNINGS_HOME"
mkdir -p "$LEARNINGS_HOME/documents/episodes"
mkdir -p "$LEARNINGS_HOME/documents/learnings"
mkdir -p "$LEARNINGS_HOME/documents/clusters"
mkdir -p "$LEARNINGS_HOME/nano_graphrag_cache"
mkdir -p "$LEARNINGS_HOME/cli"

# --- Init git repo ---

if [ ! -d "$LEARNINGS_HOME/.git" ]; then
    log "Initializing git repository"
    git -C "$LEARNINGS_HOME" init --quiet
fi

# --- .gitignore ---

GITIGNORE="$LEARNINGS_HOME/.gitignore"
if [ ! -f "$GITIGNORE" ]; then
    log "Creating .gitignore"
    cat > "$GITIGNORE" <<'EOF'
nano_graphrag_cache/
.venv/
__pycache__/
*.pyc
EOF
    git -C "$LEARNINGS_HOME" add .gitignore
    git -C "$LEARNINGS_HOME" commit -m "Add .gitignore" --quiet 2>/dev/null || true
fi

# --- Migrate existing documents ---

if [ -d "$OLD_HOME/documents" ] && [ "$OLD_HOME" != "$LEARNINGS_HOME" ]; then
    # Only migrate if old path is not a symlink (avoid circular migration)
    if [ ! -L "$OLD_HOME" ]; then
        migrated=0
        for doc in "$OLD_HOME/documents"/*.md; do
            [ -f "$doc" ] || continue
            basename="$(basename "$doc")"
            dest="$LEARNINGS_HOME/documents/learnings/$basename"
            if [ ! -f "$dest" ]; then
                cp "$doc" "$dest"
                migrated=$((migrated + 1))
            fi
            # Also copy entity sidecar if present
            sidecar="${doc%.md}.entities.yaml"
            if [ -f "$sidecar" ]; then
                sidecar_dest="$LEARNINGS_HOME/documents/learnings/$(basename "$sidecar")"
                [ -f "$sidecar_dest" ] || cp "$sidecar" "$sidecar_dest"
            fi
        done
        if [ "$migrated" -gt 0 ]; then
            log "Migrated $migrated documents from $OLD_HOME/documents/ to $LEARNINGS_HOME/documents/learnings/"
            git -C "$LEARNINGS_HOME" add -A
            git -C "$LEARNINGS_HOME" commit -m "Migrate documents from legacy location" --quiet 2>/dev/null || true
        fi
    fi
fi

# --- Copy CLI scripts ---

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
for script in learnings learnings_cli.py graph_engine.py entity_store.py graspologic_shim.py requirements.txt setup.py; do
    src="$SCRIPT_DIR/$script"
    if [ -f "$src" ]; then
        cp "$src" "$LEARNINGS_HOME/cli/$script"
    fi
done
# Ensure the wrapper is executable
[ -f "$LEARNINGS_HOME/cli/learnings" ] && chmod +x "$LEARNINGS_HOME/cli/learnings"
log "CLI scripts copied to $LEARNINGS_HOME/cli/"

# --- Backward-compat symlink ---

if [ ! -L "$OLD_HOME" ] && [ "$OLD_HOME" != "$LEARNINGS_HOME" ]; then
    # If old directory exists as a real dir, back it up
    if [ -d "$OLD_HOME" ]; then
        backup="$OLD_HOME.bak.$(date +%s)"
        log "Backing up existing $OLD_HOME to $backup"
        mv "$OLD_HOME" "$backup"
    fi
    mkdir -p "$(dirname "$OLD_HOME")"
    ln -sfn "$LEARNINGS_HOME" "$OLD_HOME"
    log "Created symlink $OLD_HOME -> $LEARNINGS_HOME"
elif [ -L "$OLD_HOME" ]; then
    log "Symlink $OLD_HOME already exists"
fi

# --- Shell profile export ---

add_export_to_profile() {
    local profile="$1"
    local export_line="export LEARNINGS_HOME=\"$LEARNINGS_HOME\""
    if [ -f "$profile" ]; then
        if ! grep -qF "LEARNINGS_HOME" "$profile"; then
            echo "" >> "$profile"
            echo "# Global learnings knowledge base" >> "$profile"
            echo "$export_line" >> "$profile"
            log "Added LEARNINGS_HOME export to $profile"
        else
            log "LEARNINGS_HOME already configured in $profile"
        fi
    fi
}

# Detect shell and add to appropriate profile
if [ -f "$HOME/.zshrc" ]; then
    add_export_to_profile "$HOME/.zshrc"
elif [ -f "$HOME/.bashrc" ]; then
    add_export_to_profile "$HOME/.bashrc"
elif [ -f "$HOME/.bash_profile" ]; then
    add_export_to_profile "$HOME/.bash_profile"
else
    log "No shell profile found. Add manually: export LEARNINGS_HOME=\"$LEARNINGS_HOME\""
fi

log "Done. LEARNINGS_HOME=$LEARNINGS_HOME"
log "Restart your shell or run: export LEARNINGS_HOME=\"$LEARNINGS_HOME\""
