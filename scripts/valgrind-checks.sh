#!/usr/bin/env bash
set -euo pipefail

# Memory-checks the Mux toolchain with Valgrind.
#
#   Leg A (--programs): compile every test_scripts/*.mux program and run the
#     resulting native binary under Valgrind. Reference counting bugs surface
#     here as leaks (missed decrefs) or invalid reads/writes (double frees).
#     Leg A is PR-blocking: any failure exits nonzero.
#
#   Leg B (--compiler): run the compiler itself under Valgrind while it compiles
#     a few representative scripts, filtering statically linked LLVM noise via
#     infra/valgrind-llvm.supp. Leg B is REPORT-ONLY: it prints findings but
#     never changes the exit code, because the LLVM allocations linked into the
#     mux binary are hard to suppress precisely and are not the bug class we
#     gate on. Promote it to fatal once its baseline is characterized.
#
# Usage: scripts/valgrind-checks.sh [--programs] [--compiler]   (default: both)

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"

# Same leak/error policy across every leg: definitely-lost and indirectly-lost
# leaks and memory errors fail; still-reachable and possibly-lost are ignored.
# The commas below are part of the flag values, not array separators.
# shellcheck disable=SC2054
valgrind_flags=(
  --quiet
  --error-exitcode=99
  --leak-check=full
  --errors-for-leak-kinds=definite,indirect
  --show-leak-kinds=definite,indirect
)

# Compiled Mux programs hang on undefined behavior instead of crashing, and
# Valgrind slows execution by ~20x, so every program run is time-boxed.
program_timeout=120

# Representative scripts for Leg B: small, medium, large.
compiler_sample_scripts=(
  "test_scripts/arithmetic.mux"
  "test_scripts/collections.mux"
  "test_scripts/test_std_dsa.mux"
)

usage() {
  echo "Usage: scripts/valgrind-checks.sh [--programs] [--compiler]"
  echo "  --programs   Leg A only: compiled test programs (PR-blocking)."
  echo "  --compiler   Leg B only: the compiler itself (report-only)."
  echo "  (no flags)   Run both legs."
}

parse_args() {
  run_programs=0
  run_compiler=0
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --programs) run_programs=1 ;;
      --compiler) run_compiler=1 ;;
      -h|--help) usage; exit 0 ;;
      *) echo "Unknown argument: $1" >&2; usage >&2; exit 1 ;;
    esac
    shift
  done
  if [[ "$run_programs" -eq 0 && "$run_compiler" -eq 0 ]]; then
    run_programs=1
    run_compiler=1
  fi
}

select_cargo_cmd() {
  cargo_cmd=(cargo)
  if [[ -x "$repo_root/scripts/dev-cargo.sh" ]] &&
     [[ -z "${LLVM_CONFIG_PATH:-}" ]] &&
     [[ -z "${LLVM_SYS_221_PREFIX:-}" ]]; then
    cargo_cmd=("$repo_root/scripts/dev-cargo.sh")
  fi
}

resolve_mux_bin() {
  local target_dir
  if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    target_dir="$CARGO_TARGET_DIR"
  elif [[ "${cargo_cmd[0]}" == *dev-cargo.sh ]]; then
    target_dir="$repo_root/target/dev-cargo"
  else
    target_dir="$repo_root/target"
  fi
  mux_bin="$target_dir/debug/mux"
}

build_compiler() {
  echo
  echo ">>> building compiler"
  ( cd "$repo_root" && "${cargo_cmd[@]}" build -p mux-lang )
  resolve_mux_bin
  if [[ ! -x "$mux_bin" ]]; then
    echo "Could not find the built compiler at $mux_bin" >&2
    exit 1
  fi
}

require_valgrind() {
  if ! command -v valgrind >/dev/null 2>&1; then
    echo "valgrind is not installed. Install it (e.g. apt-get install valgrind)." >&2
    exit 1
  fi
}

# Compile one script in place (so relative imports resolve) and run its binary
# under Valgrind. Echoes a status token; leaves cleanup to the caller.
classify_program() {
  local script="$1"
  local dir name bin ll rc
  dir="$(dirname "$script")"
  name="$(basename "$script" .mux)"
  bin="$dir/$name"
  ll="$dir/$name.ll"

  rm -f "$bin" "$ll"
  if ! "$mux_bin" build "$script" >"$compile_log" 2>&1; then
    rm -f "$bin" "$ll"
    echo "COMPILE-FAIL"
    return 0
  fi
  # The compiler can exit zero while the link step still fails to produce a
  # binary; treat that as a compile failure, not a Valgrind EXIT-127.
  if [[ ! -x "$bin" ]]; then
    echo "mux build reported success but produced no executable at $bin" >>"$compile_log"
    rm -f "$bin" "$ll"
    echo "COMPILE-FAIL"
    return 0
  fi

  rc=0
  ( cd "$dir" && timeout "$program_timeout" valgrind "${valgrind_flags[@]}" "./$name" ) \
    >"$run_log" 2>&1 || rc=$?
  rm -f "$bin" "$ll"

  case "$rc" in
    0) echo "OK" ;;
    99) echo "MEMORY-ERROR" ;;
    124) echo "TIMEOUT" ;;
    *) echo "EXIT-$rc" ;;
  esac
}

run_leg_a() {
  echo
  echo "=== Leg A: compiled programs under Valgrind ==="
  local script status
  a_names=()
  a_status=()
  a_failures=0
  for script in "$repo_root"/test_scripts/*.mux; do
    [[ -e "$script" ]] || continue
    status="$(classify_program "$script")"
    a_names+=("$(basename "$script")")
    a_status+=("$status")
    if [[ "$status" == "OK" ]]; then
      echo "  PASS  $(basename "$script")"
    else
      a_failures=$((a_failures + 1))
      echo "  FAIL  $(basename "$script")  [$status]"
      echo "----- valgrind/compile output for $(basename "$script") -----"
      if [[ "$status" == "COMPILE-FAIL" ]]; then
        cat "$compile_log" 2>/dev/null || true
      else
        cat "$run_log" 2>/dev/null || true
      fi
      echo "----- end output -----"
    fi
  done
}

print_leg_a_summary() {
  echo
  echo "=== Leg A summary ==="
  local i
  for i in "${!a_names[@]}"; do
    printf '  %-12s %s\n' "${a_status[$i]}" "${a_names[$i]}"
  done
  echo "  ${#a_names[@]} program(s), ${a_failures} failure(s)"
}

run_leg_b() {
  echo
  echo "=== Leg B: compiler under Valgrind (report-only) ==="
  local supp script rc tmp_dir out
  supp="$repo_root/infra/valgrind-llvm.supp"
  tmp_dir="$(mktemp -d)"
  out="$tmp_dir/mux-legb"
  for script in "${compiler_sample_scripts[@]}"; do
    echo
    echo ">>> valgrind mux build $script"
    rc=0
    valgrind "${valgrind_flags[@]}" --suppressions="$supp" \
      "$mux_bin" build "$repo_root/$script" -o "$out" || rc=$?
    rm -f "$out" "$repo_root/${script%.mux}.ll"
    if [[ "$rc" -eq 0 ]]; then
      echo "  clean: $script"
    else
      echo "  Valgrind reported findings for $script (exit $rc) - report-only, not failing."
    fi
  done
  rm -rf "$tmp_dir"
}

main() {
  parse_args "$@"
  require_valgrind

  compile_log="$(mktemp)"
  run_log="$(mktemp)"
  trap 'rm -f "$compile_log" "$run_log"' EXIT

  select_cargo_cmd
  build_compiler

  if [[ "$run_programs" -eq 1 ]]; then
    run_leg_a
    print_leg_a_summary
  fi
  if [[ "$run_compiler" -eq 1 ]]; then
    run_leg_b
  fi

  if [[ "$run_programs" -eq 1 && "${a_failures:-0}" -gt 0 ]]; then
    echo
    echo "Leg A found ${a_failures} failing program(s)." >&2
    exit 1
  fi
  echo
  echo "Valgrind checks complete."
}

main "$@"
