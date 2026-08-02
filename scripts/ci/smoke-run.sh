#!/usr/bin/env bash
# Prove a given `mux` can compile and run real programs.
#
# Takes the executable rather than a layout, so it works for anything that
# produces one: a staged dist/ tree (scripts/ci/smoke-packaged.sh) or an install
# performed by scripts/install.sh or install.ps1 from a release artifact. That
# second caller is the point - it is what stops a release shipping a compiler
# that cannot compile hello world.
#
# Usage: smoke-run.sh <path-to-mux-executable> [extra .mux program ...]
#
# Must be run from the repository root: the default programs come from
# test_scripts/.
set -euo pipefail

mux="${1:?usage: smoke-run.sh <path-to-mux-executable> [program ...]}"
shift

if [[ ! -x "$mux" && ! -f "$mux" ]]; then
  echo "no mux executable at $mux" >&2
  printf '::error::no mux executable at %s\n' "$mux"
  exit 1
fi

echo "Smoke-testing: $mux"
"$mux" --version || true

# Misbehaving compiled programs (LLVM UB) hang rather than crash, so every run
# is bounded. macOS ships no `timeout`, so fall back to gtimeout and then to a
# plain background-and-kill - the bound matters more than the tool.
run_bounded() {
  local secs="$1"; shift
  if command -v timeout >/dev/null 2>&1; then
    timeout "$secs" "$@"
  elif command -v gtimeout >/dev/null 2>&1; then
    gtimeout "$secs" "$@"
  else
    # `set -m` puts the command in its own process group so the WHOLE tree can
    # be signalled. Killing just the direct child is not enough: `mux run`
    # spawns the compiled program, which inherits stdout, so a hung grandchild
    # outlives its parent and holds the command-substitution pipe open - the
    # caller then blocks until the job timeout even though the bound expired.
    set -m
    "$@" &
    local pid=$!
    set +m
    # kill -0 first so a pid reused after the command already exited is never
    # signalled. The negative pid targets the process group.
    ( sleep "$secs"; kill -0 "$pid" 2>/dev/null && kill -9 -"$pid" 2>/dev/null ) &
    local watcher=$!
    local rc=0
    wait "$pid" || rc=$?
    kill "$watcher" 2>/dev/null || true
    wait "$watcher" 2>/dev/null || true
    # Sweep anything that outlived the parent. Safe because the group holds only
    # this command's tree.
    kill -9 -"$pid" 2>/dev/null || true
    return "$rc"
  fi
}

# MUX_RUNTIME_LIB is unset deliberately: it wins over every other resolution
# path, so leaving it set would test whatever it points at rather than the
# runtime this install actually shipped. Unset in a subshell rather than via
# `env -u`, which cannot invoke a shell function.
run_program() {
  local program="$1"
  ( unset MUX_RUNTIME_LIB; run_bounded 120 "$mux" run "$program" )
}

# `mux` reports a failed link as "linker command failed with exit code N"; the
# linker's own output now comes with it, but a backtrace adds the compiler-side
# detail. Re-run on failure so the log is actionable the first time.
diagnose() {
  local program="$1"
  echo "--- '$program' failed; re-running with RUST_BACKTRACE=1 for detail ---" >&2
  ( unset MUX_RUNTIME_LIB; RUST_BACKTRACE=1 run_bounded 120 "$mux" run "$program" ) >&2 2>&1 || true
}

printf 'print("hello")\n' > smoke.mux

# Output goes to a file rather than a command substitution: a substitution's pipe
# stays open until every writer exits, so a grandchild outliving `mux run` would
# block the caller past its own timeout.
if ! run_program smoke.mux > smoke.out; then
  diagnose smoke.mux
  printf '::error::%s failed to compile and run smoke.mux\n' "$mux"
  exit 1
fi
out="$(cat smoke.out)"
if [[ "$out" != "hello" ]]; then
  echo "unexpected output: $out" >&2
  printf '::error::%s produced unexpected output: %s\n' "$mux" "$out"
  exit 1
fi

# One program importing nothing, one pulling in a std module, one reaching a
# heavier optional feature. Callers may append more.
programs=(test_scripts/test_std_math.mux test_scripts/test_std_sql_sqlite.mux "$@")
for program in "${programs[@]}"; do
  if ! run_program "$program"; then
    diagnose "$program"
    printf '::error::%s failed on %s\n' "$mux" "$program"
    exit 1
  fi
done

echo "OK: compiled and ran ${#programs[@]} programs plus smoke.mux."
