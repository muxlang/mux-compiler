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
#   Leg C (--globals): guard the module-global teardown regression from #284,
#     where module-private constants sat at a permanent refcount of 1 and stayed
#     "still reachable" at exit - a leak Leg A cannot see, since its policy
#     ignores still-reachable memory (reachable is not lost). Leg C compiles two
#     fixtures under infra/valgrind-globals/ - probe.mux (module-private heap
#     constants, same-module and imported) and control.mux (the same runtime
#     surface with no module constants) - and compares their "still reachable"
#     byte totals. Both share the same fixed runtime buffer, so the totals match
#     exactly when teardown is correct; a regression makes probe.mux exceed the
#     baseline. Leg C is PR-blocking: a mismatch exits nonzero.
#
# Usage: scripts/valgrind-checks.sh [--programs] [--compiler] [--globals]
#        (default: all three legs)

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

# Leg C needs the full leak report, not the pass/fail policy the other legs use:
# it parses the "still reachable" summary line, so it must not run --quiet (which
# hides the summary) and must not set --error-exitcode (still-reachable memory is
# not an error to gate on directly - the byte comparison is what gates).
# shellcheck disable=SC2054
globals_valgrind_flags=(
  --leak-check=full
  --show-leak-kinds=all
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
  echo "Usage: scripts/valgrind-checks.sh [--programs] [--compiler] [--globals]"
  echo "  --programs   Leg A only: compiled test programs (PR-blocking)."
  echo "  --compiler   Leg B only: the compiler itself (report-only)."
  echo "  --globals    Leg C only: module-global still-reachable check (PR-blocking)."
  echo "  (no flags)   Run all three legs."
}

parse_args() {
  local arg
  run_programs=0
  run_compiler=0
  run_globals=0
  while [[ $# -gt 0 ]]; do
    arg="$1"
    case "$arg" in
      --programs) run_programs=1 ;;
      --compiler) run_compiler=1 ;;
      --globals) run_globals=1 ;;
      -h|--help) usage; exit 0 ;;
      *) echo "Unknown argument: $arg" >&2; usage >&2; exit 1 ;;
    esac
    shift
  done
  if [[ "$run_programs" -eq 0 && "$run_compiler" -eq 0 && "$run_globals" -eq 0 ]]; then
    run_programs=1
    run_compiler=1
    run_globals=1
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
  # mux-runtime too: cargo builds only a dependency's rlib, not the
  # staticlib compiled programs link against.
  ( cd "$repo_root" && "${cargo_cmd[@]}" build -p mux-runtime -p mux-lang )
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
# under Valgrind. Sets classify_status; leaves cleanup to the caller.
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
    classify_status="COMPILE-FAIL"
    return 0
  fi
  # The compiler can exit zero while the link step still fails to produce a
  # binary; treat that as a compile failure, not a Valgrind EXIT-127.
  if [[ ! -x "$bin" ]]; then
    echo "mux build reported success but produced no executable at $bin" >>"$compile_log"
    rm -f "$bin" "$ll"
    classify_status="COMPILE-FAIL"
    return 0
  fi

  rc=0
  # Suppress only benign third-party TLS/crypto noise (rustls/ring/ureq); the
  # file is anchored to those library frames and cannot hide a leak in Mux code.
  ( cd "$dir" && timeout "$program_timeout" valgrind "${valgrind_flags[@]}" \
      --suppressions="$repo_root/infra/valgrind-programs.supp" "./$name" ) \
    >"$run_log" 2>&1 || rc=$?
  rm -f "$bin" "$ll"

  case "$rc" in
    0) classify_status="OK" ;;
    99) classify_status="MEMORY-ERROR" ;;
    124) classify_status="TIMEOUT" ;;
    *) classify_status="EXIT-$rc" ;;
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
    classify_status=""
    classify_program "$script"
    status="$classify_status"
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

# Compile one Leg C fixture in place (so its co-located imports resolve) and run
# the binary under Valgrind, printing its "still reachable" byte total to stdout.
# Diagnostics go to stderr; returns nonzero if the fixture cannot be compiled or
# the total cannot be parsed. Removes the binary and IR it produces; imported
# module artifacts are cleaned by the caller, which knows the fixture set.
measure_still_reachable() {
  local script="$1"
  local dir name bin ll log bytes
  dir="$(dirname "$script")"
  name="$(basename "$script" .mux)"
  bin="$dir/$name"
  ll="$dir/$name.ll"
  log="$(mktemp)"

  rm -f "$bin" "$ll"
  if ! "$mux_bin" build "$script" >"$log" 2>&1 || [[ ! -x "$bin" ]]; then
    echo "Leg C: failed to compile fixture $script" >&2
    cat "$log" >&2
    rm -f "$bin" "$ll" "$log"
    return 1
  fi

  # Run in the fixture directory so relative imports resolve. still-reachable is
  # deterministic, so no exit-code handling is needed here - only the report.
  local rc=0
  ( cd "$dir" && timeout "$program_timeout" valgrind "${globals_valgrind_flags[@]}" "./$name" ) \
    >/dev/null 2>"$log" || rc=$?
  if [[ "$rc" -eq 124 ]]; then
    echo "Leg C: timed out after ${program_timeout}s running $name (fixture may have hung)" >&2
  fi
  rm -f "$bin" "$ll"

  bytes="$(grep -m1 'still reachable:' "$log" |
    sed -E 's/.*still reachable:[[:space:]]*([0-9,]+).*/\1/' | tr -d ',')"
  if [[ -z "$bytes" ]]; then
    echo "Leg C: could not parse 'still reachable' total for $script" >&2
    cat "$log" >&2
    rm -f "$log"
    return 1
  fi
  rm -f "$log"
  printf '%s' "$bytes"
}

run_leg_globals() {
  echo
  echo "=== Leg C: module-global still-reachable regression ==="
  local fixtures_dir probe_bytes control_bytes
  fixtures_dir="$repo_root/infra/valgrind-globals"
  g_failure=0

  probe_bytes="$(measure_still_reachable "$fixtures_dir/probe.mux")" || g_failure=1
  control_bytes="$(measure_still_reachable "$fixtures_dir/control.mux")" || g_failure=1
  # Clear artifacts the fixtures and their imported module leave behind.
  rm -f "$fixtures_dir"/probe "$fixtures_dir"/control "$fixtures_dir"/consts_module \
    "$fixtures_dir"/probe.ll "$fixtures_dir"/control.ll "$fixtures_dir"/consts_module.ll

  if [[ "$g_failure" -ne 0 ]]; then
    echo "  Leg C could not complete (see errors above)."
    return
  fi

  echo "  probe.mux   still reachable: ${probe_bytes} bytes"
  echo "  control.mux still reachable: ${control_bytes} bytes"
  if [[ "$probe_bytes" -ne "$control_bytes" ]]; then
    g_failure=1
    local delta=$(( probe_bytes - control_bytes ))
    local abs_delta=$(( delta < 0 ? -delta : delta ))
    if [[ "$delta" -gt 0 ]]; then
      echo "  FAIL: module constants add ${abs_delta} still-reachable bytes over baseline."
      echo "        A module-private constant is not released at global teardown (see #284)."
    else
      echo "  FAIL: probe has ${abs_delta} fewer still-reachable bytes than control (unexpected; check fixtures)."
    fi
  else
    echo "  PASS: module globals add no still-reachable bytes over baseline."
  fi
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
  if [[ "$run_globals" -eq 1 ]]; then
    run_leg_globals
  fi

  local exit_code=0
  if [[ "$run_programs" -eq 1 && "${a_failures:-0}" -gt 0 ]]; then
    echo
    echo "Leg A found ${a_failures} failing program(s)." >&2
    exit_code=1
  fi
  if [[ "$run_globals" -eq 1 && "${g_failure:-0}" -ne 0 ]]; then
    echo
    echo "Leg C failed (see the Leg C output above)." >&2
    exit_code=1
  fi
  if [[ "$exit_code" -eq 0 ]]; then
    echo
    echo "Valgrind checks complete."
  fi
  exit "$exit_code"
}

main "$@"
