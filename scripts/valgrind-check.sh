#!/usr/bin/env bash
#
# valgrind-check.sh - run Mux programs under Valgrind with the project's
# third-party TLS suppressions applied.
#
# Every Mux test program is expected to be leak-clean: 0 bytes "definitely
# lost" and 0 bytes "indirectly lost". The suppression file next to this script
# (valgrind.supp) silences ONLY benign, third-party TLS/crypto noise from
# rustls/ring/ureq (see that file's header for the full rationale); it can never
# hide a leak in Mux's own code.
#
# Usage:
#   scripts/valgrind-check.sh                      # check every test_scripts/*.mux
#   scripts/valgrind-check.sh test_scripts/foo.mux # check a single program
#
# Requires: a built `mux` binary (cargo build -p mux-lang) and `valgrind`.
# On Arch, export DEBUGINFOD_URLS=https://debuginfod.archlinux.org for symbols.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
supp="$repo_root/scripts/valgrind.supp"
mux_bin="$repo_root/target/debug/mux"

if [[ ! -x "$mux_bin" ]]; then
  echo "error: $mux_bin not found; build it first with 'cargo build -p mux-lang'" >&2
  exit 1
fi
command -v valgrind >/dev/null || { echo "error: valgrind not installed" >&2; exit 1; }

# Fail the run on any real leak (definite or indirect). "possibly lost" is left
# to the suppression file, which only excuses third-party TLS allocations.
vg=(valgrind --quiet --error-exitcode=99 --leak-check=full
    --errors-for-leak-kinds=definite,indirect
    "--suppressions=$supp")

run_one() {
  local src="$1" name dir
  name="$(basename "$src" .mux)"
  dir="$(dirname "$src")"
  "$mux_bin" build "$src" >/dev/null 2>&1 || { echo "COMPILE-FAIL $name"; return 1; }
  ( cd "$dir" && "${vg[@]}" "./$name" ) >/dev/null 2>"/tmp/vg-$name.out"
  local rc=$?
  rm -f "$dir/$name" "$dir/$name.ll"
  if [[ $rc -eq 0 ]]; then
    echo "CLEAN $name"
  else
    echo "LEAK  $name (see /tmp/vg-$name.out)"
  fi
  return $rc
}

fail=0
if [[ $# -gt 0 ]]; then
  for src in "$@"; do run_one "$src" || fail=1; done
else
  for src in "$repo_root"/test_scripts/*.mux; do run_one "$src" || fail=1; done
fi
exit $fail
