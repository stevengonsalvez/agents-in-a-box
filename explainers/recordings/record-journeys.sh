#!/usr/bin/env bash
# Produce per-journey vhs recordings (TUI + CLI mirror) for the skill manager.
set -uo pipefail

WT=/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_skill-manager
BIN=$WT/ainb-tui/target/debug/ainb
SBX_BASE=/tmp/sm-rec
OUT=$WT/explainers/recordings/journeys
SCRIPT=$WT/scripts/skill-manager-sandbox.sh
BINDIR=$WT/ainb-tui/target/debug
mkdir -p "$OUT"

onboard() { # $1 = root
  mkdir -p "$1/.agents-in-a-box/config"
  printf 'completed = true\ncompleted_at = "2026-05-11T00:00:00+00:00"\nversion = "1.2.1"\nskipped_dependencies = []\ngit_directories = []\n' > "$1/.agents-in-a-box/config/onboarding.toml"
}
up()   { rm -rf "$2"; bash "$SCRIPT" up --tier "$1" --root "$2" >/dev/null 2>&1; onboard "$2"; }
down() { bash "$SCRIPT" down --root "$1" >/dev/null 2>&1; }

# ---- TUI tape: $1=id $2=root $3=extra-env $4..=keypress lines (one per arg) ----
tui_tape() {
  local id="$1" root="$2" extra="$3"; shift 3
  local tape="$OUT/$id.tape"
  {
    echo "Output \"$OUT/$id.gif\""
    echo 'Set Theme "Dracula"'
    echo 'Set FontSize 16'; echo 'Set Width 1400'; echo 'Set Height 800'
    echo 'Set Padding 20'; echo 'Set Shell bash'
    echo 'Hide'
    echo "Type \"HOME=$root AINB_HOME=$root/.agents-in-a-box AINB_TOOL_HOME_CLAUDE=$root/.claude AINB_TOOL_HOME_CODEX=$root/.codex GIT_TERMINAL_PROMPT=0 GIT_ASKPASS=/bin/true $extra $BIN\""
    echo 'Enter'; echo 'Sleep 22s'
    echo 'Show'; echo 'Sleep 1s'
    for line in "$@"; do echo "$line"; done
  } > "$tape"
  vhs "$tape" >/dev/null 2>&1 && [ -s "$OUT/$id.gif" ] && echo "  OK  $id ($(du -h "$OUT/$id.gif" | cut -f1))" || echo "  FAIL $id"
}

# ---- CLI tape: $1=id $2=root $3=extra-env $4..=command lines ----
cli_tape() {
  local id="$1" root="$2" extra="$3"; shift 3
  local tape="$OUT/$id.tape"
  {
    echo "Output \"$OUT/$id.gif\""
    echo 'Set Theme "Dracula"'
    echo 'Set FontSize 16'; echo 'Set Width 1400'; echo 'Set Height 760'
    echo 'Set Padding 20'; echo 'Set Shell bash'
    echo "Type \"source $root/env.sh; export PATH=$BINDIR:\$PATH; $extra clear\""
    echo 'Enter'; echo 'Sleep 1s'
    for line in "$@"; do echo "$line"; done
  } > "$tape"
  vhs "$tape" >/dev/null 2>&1 && [ -s "$OUT/$id.gif" ] && echo "  OK  $id ($(du -h "$OUT/$id.gif" | cut -f1))" || echo "  FAIL $id"
}

echo "== TUI recordings =="

# 1. discover + import (minimal → empty manifest → banner)
R=$SBX_BASE-disc; up minimal "$R"
tui_tape tui-discover-import "$R" "" \
  'Type "m"' 'Sleep 4s' 'Enter' 'Sleep 4s' 'Type "q"' 'Sleep 2s'
down "$R"

# 2. browse + install (full tier + mock catalog pointing first hit at the bare remote)
R=$SBX_BASE-brow; up full "$R"
MOCKURI="git:file://$R/sandbox-remote.git@main/skills/initial-skill"
tui_tape tui-browse-install "$R" "AINB_CATALOG_MOCK=1 AINB_CATALOG_MOCK_INSTALL_URI='$MOCKURI'" \
  'Type "m"' 'Sleep 3s' 'Type "b"' 'Sleep 2s' 'Type "react"' 'Sleep 2s' 'Enter' 'Sleep 3s' 'Enter' 'Sleep 4s' 'Type "q"' 'Sleep 2s'
down "$R"

# 3. own library — seed two own-skills so the Library view is populated
R=$SBX_BASE-lib; up full "$R"
( source "$R/env.sh"; "$BIN" skill library new my-helper >/dev/null 2>&1; "$BIN" skill library new commit-helper >/dev/null 2>&1 )
tui_tape tui-own-library "$R" "" \
  'Type "m"' 'Sleep 3s' 'Type "l"' 'Sleep 4s' 'Type "q"' 'Sleep 2s'
down "$R"

# 4. sync an edit — install + edit deployed file + drop the conflict shadow so
#    [s] routes to Sync (not the dual-purpose conflict-flip) and shows the toast
R=$SBX_BASE-sync; up full "$R"
( source "$R/env.sh"; "$BIN" skill install "git:file://$R/sandbox-remote.git@main/skills/initial-skill" --targets claude --yes >/dev/null 2>&1 )
echo "Local tweak $(date)" >> "$R/.claude/skills/initial-skill/SKILL.md"
python3 - "$R/.agents-in-a-box/manifest.yaml" <<'PY'
import sys,re; p=sys.argv[1]; t=open(p).read(); open(p,'w').write(re.sub(r'\n\s*shadowed_by:.*','',t))
PY
tui_tape tui-sync-edit "$R" "" \
  'Type "m"' 'Sleep 3s' 'Type "s"' 'Sleep 4s' 'Type "q"' 'Sleep 2s'
down "$R"

# 5. update + remove (full tier)
R=$SBX_BASE-upd; up full "$R"
tui_tape tui-update-remove "$R" "" \
  'Type "m"' 'Sleep 3s' 'Type "u"' 'Sleep 3s' 'Type "r"' 'Sleep 4s' 'Type "q"' 'Sleep 2s'
down "$R"

# 6. search + nav (full tier, >=2 units)
R=$SBX_BASE-srch; up full "$R"
tui_tape tui-search-nav "$R" "" \
  'Type "m"' 'Sleep 3s' 'Down' 'Sleep 2s' 'Up' 'Sleep 2s' 'Type "/"' 'Sleep 2s' 'Type "commit"' 'Sleep 3s' 'Escape' 'Sleep 2s' 'Type "q"' 'Sleep 2s'
down "$R"

echo "== CLI recordings =="

# 7. discover import CLI
R=$SBX_BASE-cdisc; up minimal "$R"
cli_tape cli-discover-import "$R" "" \
  'Type "ainb migrate --discover"' 'Enter' 'Sleep 4s'
down "$R"

# 8. browse + install CLI (mock catalog)
R=$SBX_BASE-cbrow; up full "$R"
cli_tape cli-browse-install "$R" "export AINB_CATALOG_MOCK=1;" \
  'Type "ainb skill browse react"' 'Enter' 'Sleep 4s' \
  "Type \"ainb skill install git:file://$R/sandbox-remote.git@main/skills/initial-skill --targets claude --yes\"" 'Enter' 'Sleep 4s'
down "$R"

# 9. library CLI
R=$SBX_BASE-clib; up full "$R"
cli_tape cli-own-library "$R" "" \
  'Type "ainb skill library new my-helper"' 'Enter' 'Sleep 3s' \
  'Type "ainb skill library list"' 'Enter' 'Sleep 3s'
down "$R"

# 10. sync edit CLI — pre-install + edit the deployed file (invisible, avoids vhs
#     quote-escaping traps); tape shows sync -> new remote commit
R=$SBX_BASE-csync; up full "$R"
( source "$R/env.sh"; "$BIN" skill install "git:file://$R/sandbox-remote.git@main/skills/initial-skill" --targets claude --yes >/dev/null 2>&1 )
sleep 1  # ensure the edit's mtime is strictly newer than the fetched cache (sync uses mtime)
echo "# local edit $(date)" >> "$R/.claude/skills/initial-skill/SKILL.md"
cli_tape cli-sync-edit "$R" "" \
  'Type "ainb skill sync --to-repo --yes"' 'Enter' 'Sleep 4s' \
  "Type \"git -C $R/sandbox-remote.git log --oneline | head -3\"" 'Enter' 'Sleep 3s'
down "$R"

# 11. update + remove CLI — pre-install so check/remove act on a real installed unit
R=$SBX_BASE-cupd; up full "$R"
( source "$R/env.sh"; "$BIN" skill install "git:file://$R/sandbox-remote.git@main/skills/initial-skill" --targets claude --yes >/dev/null 2>&1 )
cli_tape cli-update-remove "$R" "" \
  'Type "ainb skill check"' 'Enter' 'Sleep 3s' \
  "Type \"ainb skill remove git:file://$R/sandbox-remote.git@main/skills/initial-skill --yes\"" 'Enter' 'Sleep 4s'
down "$R"

echo "== done =="
ls -la "$OUT"/*.gif 2>/dev/null | awk '{print $NF, $5}'
