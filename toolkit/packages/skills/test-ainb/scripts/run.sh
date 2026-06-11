#!/usr/bin/env bash
# test-ainb runner — wraps ainb's 5-layer testing strategy.
# Usage: see --help
set -uo pipefail

# ────────────────────────────────────────────────────────────────────
# defaults
# ────────────────────────────────────────────────────────────────────
LAYER="quick"
COMPONENT=""
PLUGIN=""
VHS_TAPE=""
UPDATE=0
CI=0
WORKSPACE=""

# colours (only if tty)
if [[ -t 1 ]]; then
  C_HDR=$'\033[1;36m'
  C_OK=$'\033[1;32m'
  C_FAIL=$'\033[1;31m'
  C_SKIP=$'\033[2;37m'
  C_DIM=$'\033[2m'
  C_RST=$'\033[0m'
else
  C_HDR="" C_OK="" C_FAIL="" C_SKIP="" C_DIM="" C_RST=""
fi

# ────────────────────────────────────────────────────────────────────
# help
# ────────────────────────────────────────────────────────────────────
print_help() {
  cat <<'EOF'
test-ainb — 5-layer test runner for the ainb Rust workspace

USAGE
  test-ainb [FLAGS]

LAYERS
  L1  unit               cargo test --workspace --lib --bins --tests
  L2  insta snapshot     cargo test --workspace --test 'snapshot_*'
  L3  mock-plugin tile   cargo test --workspace --test 'plugin_tile_*'
  L4  real-plugin spawn  cargo test --workspace --test 'real_plugin_*' -- --ignored
  L5  vhs recording      vhs <tape>.tape -o <tape>.gif

FLAGS
  --layer <L1|L2|L3|L4|L5|all|quick>
        Pick layers to run. Default: quick (= L1 + L2).
        all = L1 + L2 + L3 only (L4 + L5 require explicit opt-in).
  --component <name>     Run snapshot test for one component (implies L2).
  --plugin <name>        Run real-plugin tile test for one plugin (implies L4).
  --vhs <tape|all>       Run vhs tape from tests/vhs/bsp/<tape>.tape (implies L5).
  --update               Update insta snapshots / regenerate vhs tapes.
  --ci                   CI subset: L1 + L2 only, --test-threads=1.
  --workspace <path>     Override workspace autodetect.
  -h, --help             Show this help.

EXAMPLES
  test-ainb                                 # quick: L1 + L2
  test-ainb --layer all                     # L1 + L2 + L3
  test-ainb --layer L1                      # unit tests only
  test-ainb --component session_list        # snapshot one component
  test-ainb --plugin burndown               # real-plugin spawn for burndown
  test-ainb --vhs split-vertical            # record one vhs tape
  test-ainb --vhs all                       # record every vhs tape in tests/vhs/bsp/
  test-ainb --layer L2 --update             # accept new insta snapshots
  test-ainb --ci                            # what CI runs

NOTES
  - Workspace autodetect walks up from cwd looking for ainb-tui/Cargo.toml.
  - L4 (real-plugin) and L5 (vhs) are NEVER auto-run by --layer all; opt in explicitly.
  - Stevie's worktree-volatile workflow: skill respects whichever ainb-tui worktree cwd is inside.
EOF
}

# ────────────────────────────────────────────────────────────────────
# argv parse
# ────────────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
  case "$1" in
    --layer)      LAYER="$2"; shift 2 ;;
    --component)  COMPONENT="$2"; LAYER="L2"; shift 2 ;;
    --plugin)     PLUGIN="$2"; LAYER="L4"; shift 2 ;;
    --vhs)        VHS_TAPE="$2"; LAYER="L5"; shift 2 ;;
    --update)     UPDATE=1; shift ;;
    --ci)         CI=1; LAYER="quick"; shift ;;
    --workspace)  WORKSPACE="$2"; shift 2 ;;
    -h|--help)    print_help; exit 0 ;;
    *)            echo "unknown arg: $1" >&2; print_help; exit 2 ;;
  esac
done

# ────────────────────────────────────────────────────────────────────
# workspace autodetect (walk up looking for ainb-tui/Cargo.toml)
# ────────────────────────────────────────────────────────────────────
find_workspace() {
  if [[ -n "$WORKSPACE" ]]; then
    if [[ -f "$WORKSPACE/Cargo.toml" ]]; then
      echo "$WORKSPACE"; return 0
    elif [[ -f "$WORKSPACE/ainb-tui/Cargo.toml" ]]; then
      echo "$WORKSPACE/ainb-tui"; return 0
    else
      echo "" ; return 1
    fi
  fi
  local d
  d="$(pwd)"
  while [[ "$d" != "/" ]]; do
    if [[ -f "$d/ainb-tui/Cargo.toml" ]]; then echo "$d/ainb-tui"; return 0; fi
    if [[ -f "$d/Cargo.toml" ]] && grep -q '^name = "ainb"' "$d/Cargo.toml" 2>/dev/null; then
      echo "$d"; return 0
    fi
    d="$(dirname "$d")"
  done
  return 1
}

WS_DIR="$(find_workspace)" || true
if [[ -z "$WS_DIR" ]]; then
  echo "${C_FAIL}error:${C_RST} couldn't find ainb-tui workspace from $(pwd)" >&2
  echo "hint: pass --workspace <path> or cd into an ainb worktree" >&2
  exit 3
fi

# ────────────────────────────────────────────────────────────────────
# tooling probes (warn only; don't abort until layer that needs it)
# ────────────────────────────────────────────────────────────────────
have_vhs()        { command -v vhs >/dev/null 2>&1; }
have_cargo_insta(){ cargo --list 2>/dev/null | grep -q '\binsta\b'; }

# ────────────────────────────────────────────────────────────────────
# layer banner + result tracking
# ────────────────────────────────────────────────────────────────────
declare -a SUMMARY_KEYS=()
declare -A SUMMARY_RESULTS=()
declare -A SUMMARY_TIMES=()

banner() {
  local layer="$1" name="$2"
  echo ""
  echo "${C_HDR}═══════════════════════════════════════════════════${C_RST}"
  echo "${C_HDR}  LAYER ${layer} — ${name}${C_RST}"
  echo "${C_HDR}═══════════════════════════════════════════════════${C_RST}"
}

show_cmd() { echo "${C_DIM}> $*${C_RST}"; }

run_cmd() {
  # $1 = layer label (e.g. "L1"), rest = command argv
  local layer="$1"; shift
  local start_t end_t elapsed
  start_t=$(date +%s)
  show_cmd "$@"
  ( cd "$WS_DIR" && "$@" )
  local rc=$?
  end_t=$(date +%s)
  elapsed=$((end_t - start_t))
  SUMMARY_KEYS+=("$layer")
  SUMMARY_TIMES["$layer"]="${elapsed}s"
  if [[ $rc -eq 0 ]]; then
    SUMMARY_RESULTS["$layer"]="ok"
    echo ""
    echo "${C_OK}✓ Layer ${layer} passed${C_RST}  ${C_DIM}(${elapsed}s)${C_RST}"
  else
    SUMMARY_RESULTS["$layer"]="fail"
    echo ""
    echo "${C_FAIL}✗ Layer ${layer} FAILED${C_RST}  ${C_DIM}(exit ${rc}, ${elapsed}s)${C_RST}"
  fi
  return $rc
}

skip_layer() {
  local layer="$1" reason="$2"
  SUMMARY_KEYS+=("$layer")
  SUMMARY_RESULTS["$layer"]="skip"
  SUMMARY_TIMES["$layer"]="$reason"
}

# ────────────────────────────────────────────────────────────────────
# layer implementations
# ────────────────────────────────────────────────────────────────────
run_L1() {
  banner "L1" "unit (cargo test)"
  # --lib + --bins for inline #[cfg(test)] modules, --tests for integration
  # tests in crates/ainb-core/tests/*.rs (e.g. bsp_*.rs from BSP epic).
  # Always single-threaded — ainb-core has pre-existing test-isolation
  # races (git/worktree integration tests share global git state) that
  # surface intermittently under cargo's default parallel-in-process
  # execution. Stability outweighs the parallelism speedup.
  # AINB_SKIP_DU_SCAN bypasses the orphan-worktree size scan in
  # session_recovery that otherwise spawns `du -sm` against multi-GB
  # cargo target/ trees in sibling worktrees.
  export AINB_SKIP_DU_SCAN=1
  local args=(test --workspace --lib --bins --tests -- --test-threads=1)
  run_cmd "L1" cargo "${args[@]}"
}

run_L2() {
  banner "L2" "insta snapshot"
  if ! have_cargo_insta && [[ $UPDATE -eq 1 ]]; then
    echo "${C_FAIL}cargo-insta missing — install: cargo install cargo-insta${C_RST}"
    SUMMARY_KEYS+=("L2"); SUMMARY_RESULTS["L2"]="fail"; SUMMARY_TIMES["L2"]="no cargo-insta"
    return 1
  fi
  local test_filter="snapshot_*"
  if [[ -n "$COMPONENT" ]]; then test_filter="snapshot_${COMPONENT}"; fi
  if [[ $UPDATE -eq 1 ]]; then
    run_cmd "L2" cargo insta test --workspace --test "$test_filter" --accept
  else
    local args=(test --workspace --test "$test_filter")
    if [[ $CI -eq 1 ]]; then args+=(-- --test-threads=1); fi
    run_cmd "L2" cargo "${args[@]}"
  fi
}

run_L3() {
  banner "L3" "mock-plugin compositing"
  run_cmd "L3" cargo test --workspace --test 'plugin_tile_*'
}

run_L4() {
  banner "L4" "real-plugin spawn"
  local test_filter="real_plugin_*"
  if [[ -n "$PLUGIN" ]]; then test_filter="real_plugin_${PLUGIN}"; fi
  run_cmd "L4" cargo test --workspace --test "$test_filter" -- --ignored --test-threads=1
}

run_L5() {
  banner "L5" "vhs recording"
  if ! have_vhs; then
    echo "${C_FAIL}vhs missing — install: brew install vhs${C_RST}"
    SUMMARY_KEYS+=("L5"); SUMMARY_RESULTS["L5"]="fail"; SUMMARY_TIMES["L5"]="no vhs"
    return 1
  fi
  local tape_dir="${WS_DIR}/tests/vhs/bsp"
  local out_dir="${tape_dir}/out"
  mkdir -p "$out_dir"
  local tapes=()
  if [[ "$VHS_TAPE" == "all" ]]; then
    while IFS= read -r f; do tapes+=("$f"); done < <(find "$tape_dir" -maxdepth 1 -name '*.tape' 2>/dev/null)
  elif [[ -n "$VHS_TAPE" ]]; then
    tapes=("${tape_dir}/${VHS_TAPE}.tape")
  else
    echo "${C_FAIL}--vhs requires a tape name or 'all'${C_RST}"
    SUMMARY_KEYS+=("L5"); SUMMARY_RESULTS["L5"]="fail"; SUMMARY_TIMES["L5"]="missing tape"
    return 1
  fi
  if [[ ${#tapes[@]} -eq 0 ]]; then
    echo "${C_FAIL}no tapes found in ${tape_dir}${C_RST}"
    SUMMARY_KEYS+=("L5"); SUMMARY_RESULTS["L5"]="fail"; SUMMARY_TIMES["L5"]="no tapes"
    return 1
  fi
  local any_fail=0
  for tape in "${tapes[@]}"; do
    if [[ ! -f "$tape" ]]; then
      echo "${C_FAIL}tape not found: $tape${C_RST}"; any_fail=1; continue
    fi
    local base; base="$(basename "$tape" .tape)"
    local out_gif="${out_dir}/${base}.gif"
    show_cmd vhs "$tape" -o "$out_gif"
    if ! vhs "$tape" -o "$out_gif"; then any_fail=1; fi
  done
  SUMMARY_KEYS+=("L5")
  if [[ $any_fail -eq 0 ]]; then
    SUMMARY_RESULTS["L5"]="ok"
    SUMMARY_TIMES["L5"]="${#tapes[@]} tape(s)"
    echo "${C_OK}✓ Layer L5 recorded ${#tapes[@]} tape(s) → ${out_dir}${C_RST}"
  else
    SUMMARY_RESULTS["L5"]="fail"
    SUMMARY_TIMES["L5"]="see output"
    return 1
  fi
}

# ────────────────────────────────────────────────────────────────────
# dispatch
# ────────────────────────────────────────────────────────────────────
echo "${C_DIM}workspace: ${WS_DIR}${C_RST}"

OVERALL_RC=0
case "$LAYER" in
  L1)        run_L1 || OVERALL_RC=$? ;;
  L2)        run_L2 || OVERALL_RC=$? ;;
  L3)        run_L3 || OVERALL_RC=$? ;;
  L4)        run_L4 || OVERALL_RC=$? ;;
  L5)        run_L5 || OVERALL_RC=$? ;;
  quick)
    run_L1 || OVERALL_RC=$?
    run_L2 || OVERALL_RC=$?
    skip_layer "L3" "not in quick"
    skip_layer "L4" "use --layer L4 to run"
    skip_layer "L5" "use --vhs <tape> to run"
    ;;
  all)
    run_L1 || OVERALL_RC=$?
    run_L2 || OVERALL_RC=$?
    run_L3 || OVERALL_RC=$?
    skip_layer "L4" "use --layer L4 to run"
    skip_layer "L5" "use --vhs <tape> to run"
    ;;
  *)
    echo "${C_FAIL}unknown layer: $LAYER${C_RST}"
    print_help
    exit 2
    ;;
esac

# ────────────────────────────────────────────────────────────────────
# summary
# ────────────────────────────────────────────────────────────────────
echo ""
echo "${C_HDR}═══════════════════════════════════════════════════${C_RST}"
echo "${C_HDR}  SUMMARY${C_RST}"
echo "${C_HDR}═══════════════════════════════════════════════════${C_RST}"
for key in "${SUMMARY_KEYS[@]}"; do
  local_name=""
  case "$key" in
    L1) local_name="unit                " ;;
    L2) local_name="snapshot            " ;;
    L3) local_name="mock-plugin         " ;;
    L4) local_name="real-plugin         " ;;
    L5) local_name="vhs                 " ;;
  esac
  result="${SUMMARY_RESULTS[$key]:-?}"
  time_or_reason="${SUMMARY_TIMES[$key]:-}"
  case "$result" in
    ok)   printf "  %s %s ${C_OK}✓${C_RST}  ${C_DIM}(%s)${C_RST}\n" "$key" "$local_name" "$time_or_reason" ;;
    fail) printf "  %s %s ${C_FAIL}✗${C_RST}  ${C_DIM}(%s)${C_RST}\n" "$key" "$local_name" "$time_or_reason" ;;
    skip) printf "  %s %s ${C_SKIP}— skipped${C_RST}  ${C_DIM}(%s)${C_RST}\n" "$key" "$local_name" "$time_or_reason" ;;
    *)    printf "  %s %s ${C_DIM}?${C_RST}\n" "$key" "$local_name" ;;
  esac
done
echo ""

exit $OVERALL_RC
