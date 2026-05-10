#!/usr/bin/env bash
# generate-catalog.sh — regenerate toolkit/catalog.yaml from the filesystem.
#
# catalog.yaml is the SOURCE OF TRUTH for "what this toolkit owns" — the
# internal set. Anything in ~/.claude/skills/ (or other tool homes) that is
# NOT listed here is, by construction, external (plugin / nanoclaw / npx /
# personal CLI wrapper) and gets filtered out of orphan reports.
#
# external-dependencies.yaml continues to track installation reproducibility
# for external plugins/skills; it does NOT define the internal set.
#
# Usage:  bash toolkit/bin/generate-catalog.sh
#         (run from any cwd; the script resolves toolkit/ from its own path)
#
# The output is deterministic and byte-stable across runs (everything sorted),
# so re-running with no source changes produces no git diff.

set -euo pipefail

# Resolve the toolkit/ root from this script's location.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TOOLKIT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUT="$TOOLKIT_ROOT/catalog.yaml"

cd "$TOOLKIT_ROOT"

# ---- helpers ----------------------------------------------------------------

# List immediate-child directory names under $1, sorted, one per line.
list_dirs() {
    [ -d "$1" ] || return 0
    find "$1" -mindepth 1 -maxdepth 1 -type d -exec basename {} \; | sort
}

# List files (not dirs) matching $2 under $1, returning basenames without ext.
list_files_basename() {
    [ -d "$1" ] || return 0
    find "$1" -maxdepth 1 -type f -name "$2" -exec basename {} \; \
        | sed 's/\.[^.]*$//' | sort
}

# Emit YAML list under header $1, where stdin is one item per line.
# Skips the header entirely if there are no items.
emit_list() {
    local header="$1"
    local items
    items=$(cat)
    if [ -z "$items" ]; then
        return 0
    fi
    echo "$header"
    while IFS= read -r item; do
        [ -n "$item" ] && echo "    - $item"
    done <<< "$items"
}

# ---- header -----------------------------------------------------------------

cat > "$OUT" <<'EOF'
# AUTO-GENERATED — do not edit by hand.
# Source of truth: filesystem. Regenerate with:
#   bash toolkit/bin/generate-catalog.sh
#
# This file lists everything THIS REPO owns (the "internal" set). Anything
# installed into ~/.{claude,codex,copilot}/skills/ that does not appear here
# is external (plugin / nanoclaw / npx / personal). External installation
# reproducibility lives in toolkit/external-dependencies.yaml.
#
apiVersion: catalog/v1
metadata:
  name: agents-in-a-box
  description: Filesystem-derived manifest of toolkit-owned skills, plugins, agents, workflows, and utilities
  generator: bin/generate-catalog.sh

EOF

# ---- skills -----------------------------------------------------------------

{
    echo "components:"
    echo ""
    list_dirs "packages/skills" | emit_list "  skills:"
} >> "$OUT"

# ---- plugins (with sub-skills) ---------------------------------------------

if [ -d "packages/plugins" ]; then
    plugins=$(list_dirs "packages/plugins")
    if [ -n "$plugins" ]; then
        echo "" >> "$OUT"
        echo "  plugins:" >> "$OUT"
        while IFS= read -r plugin; do
            [ -z "$plugin" ] && continue
            echo "    - name: $plugin" >> "$OUT"
            subskills=$(list_dirs "packages/plugins/$plugin/skills")
            if [ -n "$subskills" ]; then
                echo "      skills:" >> "$OUT"
                while IFS= read -r ss; do
                    [ -n "$ss" ] && echo "        - $ss" >> "$OUT"
                done <<< "$subskills"
            fi
        done <<< "$plugins"
    fi
fi

# ---- workflows --------------------------------------------------------------

if [ -d "packages/workflows" ]; then
    wfs=$(list_dirs "packages/workflows")
    if [ -n "$wfs" ]; then
        echo "" >> "$OUT"
        echo "  workflows:" >> "$OUT"
        while IFS= read -r wf; do
            [ -z "$wf" ] && continue
            echo "    - name: $wf" >> "$OUT"
            cmds=$(list_files_basename "packages/workflows/$wf/commands" "*.md")
            if [ -n "$cmds" ]; then
                echo "      commands:" >> "$OUT"
                while IFS= read -r c; do
                    [ -n "$c" ] && echo "        - $c" >> "$OUT"
                done <<< "$cmds"
            fi
        done <<< "$wfs"
    fi
fi

# ---- agents -----------------------------------------------------------------

if [ -d "packages/agents" ]; then
    echo "" >> "$OUT"
    echo "  agents:" >> "$OUT"

    # Root-level agents (.md files directly under packages/agents/)
    root_agents=$(list_files_basename "packages/agents" "*.md")
    if [ -n "$root_agents" ]; then
        echo "    root:" >> "$OUT"
        while IFS= read -r a; do
            [ -n "$a" ] && echo "      - $a" >> "$OUT"
        done <<< "$root_agents"
    fi

    # Categorized agents (subdirs containing .md files)
    for category_dir in packages/agents/*/; do
        [ -d "$category_dir" ] || continue
        category=$(basename "$category_dir")
        agents=$(list_files_basename "$category_dir" "*.md")
        if [ -n "$agents" ]; then
            echo "    $category:" >> "$OUT"
            while IFS= read -r a; do
                [ -n "$a" ] && echo "      - $a" >> "$OUT"
            done <<< "$agents"
        fi
    done
fi

# ---- utilities (commands, hooks, templates, output-styles, utils) ----------

if [ -d "packages/utilities" ]; then
    echo "" >> "$OUT"
    echo "  utilities:" >> "$OUT"

    for sub in commands hooks output-styles utils; do
        dir="packages/utilities/$sub"
        [ -d "$dir" ] || continue
        # Items can be either flat files or subdirs (commands/ has subdirs).
        files=$(list_files_basename "$dir" "*.md" 2>/dev/null || true)
        files+=$'\n'$(list_files_basename "$dir" "*.sh" 2>/dev/null || true)
        files+=$'\n'$(list_files_basename "$dir" "*.py" 2>/dev/null || true)
        dirs=$(list_dirs "$dir" 2>/dev/null || true)

        if [ -n "$dirs" ] && [ -z "$(echo "$files" | tr -d '[:space:]')" ]; then
            # Pure subdir layout (e.g. utilities/commands/{session,git,...}/)
            echo "    $sub:" >> "$OUT"
            while IFS= read -r d; do
                [ -z "$d" ] && continue
                echo "      $d:" >> "$OUT"
                items=$(list_files_basename "$dir/$d" "*.md")
                items+=$'\n'$(list_files_basename "$dir/$d" "*.sh")
                items+=$'\n'$(list_files_basename "$dir/$d" "*.py")
                items=$(printf '%s\n' "$items" | grep -v '^$' | sort -u)
                while IFS= read -r i; do
                    [ -n "$i" ] && echo "        - $i" >> "$OUT"
                done <<< "$items"
            done <<< "$dirs"
        else
            # Flat file layout
            items=$(printf '%s\n' "$files" | grep -v '^$' | sort -u)
            if [ -n "$items" ]; then
                echo "    $sub:" >> "$OUT"
                while IFS= read -r i; do
                    [ -n "$i" ] && echo "      - $i" >> "$OUT"
                done <<< "$items"
            fi
        fi
    done
fi

# ---- summary ---------------------------------------------------------------

skill_count=$(list_dirs "packages/skills" | wc -l | tr -d ' ')
plugin_skill_count=0
if [ -d "packages/plugins" ]; then
    while IFS= read -r p; do
        [ -z "$p" ] && continue
        n=$(list_dirs "packages/plugins/$p/skills" | wc -l | tr -d ' ')
        plugin_skill_count=$((plugin_skill_count + n))
    done < <(list_dirs "packages/plugins")
fi

echo "" >&2
echo "✓ wrote $OUT" >&2
echo "  bundled skills: $skill_count" >&2
echo "  plugin sub-skills: $plugin_skill_count" >&2
echo "  total internal: $((skill_count + plugin_skill_count))" >&2
