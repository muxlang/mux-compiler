#!/usr/bin/env bash
#
# Run compiled Mux programs against the rc-leak-check runtime and fail if any of
# them leaks. This is the reliable local way to reproduce the CI "RC Leak Check"
# job: it builds the runtime with the `rc-leak-check` feature and FORCES it via
# MUX_RUNTIME_LIB, so the check cannot silently link a feature-less runtime.
#
# Why the force matters: setting only MUX_RUNTIME_FEATURES=...,rc-leak-check is
# NOT enough. If a plain `target/debug/libmux_runtime.a` exists (cargo builds it
# as a workspace member without the feature), it shadows the feature-specific
# build, the exit-time assertion never runs, and a leaking program falsely exits
# 0 - a false "leak-free". Pointing MUX_RUNTIME_LIB at the feature-built archive
# removes that footgun.
#
# A leaking program exits 101 with "N reference-counted block(s) still live at
# exit" (mux-runtime's rc-leak-check atexit assertion).
#
# Usage:
#   scripts/leak-check.sh [file.mux ...]   # given programs (default: a sample set)
# Env:
#   MUX_RUNTIME_SRC   path to a mux-runtime checkout (default: ../mux-runtime)

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"

# Keep in sync with mux-runtime's `full` feature plus rc-leak-check (matches the
# CI RC Leak Check job's MUX_RUNTIME_FEATURES).
leak_features="core,json,csv,net,sql,sync,rc-leak-check"
program_timeout=120

runtime_src="${MUX_RUNTIME_SRC:-$repo_root/../mux-runtime}"
if [[ ! -f "$runtime_src/Cargo.toml" ]]; then
  echo "mux-runtime source not found at '$runtime_src'." >&2
  echo "Set MUX_RUNTIME_SRC to a mux-runtime checkout." >&2
  exit 1
fi
runtime_src="$(cd "$runtime_src" && pwd)"

# Pin explicit target directories so the archive/binary paths are deterministic
# and immune to an ambient CARGO_TARGET_DIR (which would otherwise redirect the
# build and leave us forcing MUX_RUNTIME_LIB at a stale, feature-less archive).
# A command-line --target-dir overrides the CARGO_TARGET_DIR env var.
runtime_target="$runtime_src/target"
compiler_target="$repo_root/target"

echo ">>> building rc-leak-check runtime ($leak_features)"
( cd "$runtime_src" &&
  cargo build --no-default-features --features "$leak_features" --target-dir "$runtime_target" )
runtime_lib="$runtime_target/debug/libmux_runtime.a"
if [[ ! -f "$runtime_lib" ]]; then
  echo "Expected runtime archive not found at $runtime_lib" >&2
  exit 1
fi

echo ">>> building compiler"
( cd "$repo_root" && cargo build -p mux-lang --target-dir "$compiler_target" )
mux_bin="$compiler_target/debug/mux"
if [[ ! -x "$mux_bin" ]]; then
  echo "Expected compiler binary not found at $mux_bin" >&2
  exit 1
fi

# Force the leak-check runtime; without this the check can link the wrong lib.
export MUX_RUNTIME_LIB="$runtime_lib"
export MUX_RUNTIME_FEATURES="$leak_features"

if [[ $# -gt 0 ]]; then
  programs=("$@")
else
  # A representative default set (RC-heavy: strings, collections, enums, objects,
  # closures, field defaults). Pass explicit files to check something specific.
  programs=(
    "$repo_root/test_scripts/field_default_expressions.mux"
    "$repo_root/test_scripts/enum_nested_payload.mux"
    "$repo_root/test_scripts/collections.mux"
    "$repo_root/test_scripts/closure_captured_increment.mux"
  )
fi

failures=0
for script in "${programs[@]}"; do
  if [[ ! -f "$script" ]]; then
    # A requested program that does not exist is a failure, not a silent skip:
    # otherwise a misspelled path or renamed sample lets the run report all
    # clean without actually checking anything.
    echo "  MISSING  $script"
    failures=$((failures + 1))
    continue
  fi
  out="$(timeout "$program_timeout" "$mux_bin" run "$script" 2>&1)" && rc=0 || rc=$?
  if [[ "$rc" -eq 101 ]] || grep -q "still live at exit" <<<"$out"; then
    echo "  LEAK  $script"
    grep "still live at exit" <<<"$out" | sed 's/^/        /'
    failures=$((failures + 1))
  elif [[ "$rc" -ne 0 ]]; then
    echo "  ERROR $script (exit $rc)"
    echo "$out" | tail -3 | sed 's/^/        /'
    failures=$((failures + 1))
  else
    echo "  ok    $script"
  fi
done

echo
if [[ "$failures" -gt 0 ]]; then
  echo "rc-leak-check: $failures program(s) leaked or errored."
  exit 1
fi
echo "rc-leak-check: all programs clean."
