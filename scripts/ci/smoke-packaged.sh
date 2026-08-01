#!/usr/bin/env bash
# Stage a binary install from a build profile and prove it can compile and run
# real programs from that layout.
#
# This reproduces what scripts/install.sh produces - the compiler plus the
# bundled runtime library in bin/ and lib/, and nothing else. No other job runs
# the compiler from that layout, which is how a release that could not compile
# hello world reached users.
#
# Usage: smoke-packaged.sh [profile]     (profile defaults to debug)
set -euo pipefail

profile="${1:-debug}"
target_dir="target/${profile}"
dist="dist"

exe_suffix=""
[[ "$(uname -s)" == MINGW* || "$(uname -s)" == MSYS* ]] && exe_suffix=".exe"

rm -rf "$dist"
mkdir -p "$dist/bin" "$dist/lib"
cp "${target_dir}/mux${exe_suffix}" "$dist/bin/"

# Copy whichever runtime libraries this platform produced: .a everywhere, plus
# .so on Linux or .dylib on macOS. The shared library is not optional - a
# program using libm (pow, say) fails to link without it, because no -lm is
# passed.
found_lib=0
for lib in "${target_dir}"/libmux_runtime.a \
           "${target_dir}"/libmux_runtime.so \
           "${target_dir}"/libmux_runtime.dylib; do
  if [[ -f "$lib" ]]; then
    cp "$lib" "$dist/lib/"
    found_lib=1
  fi
done
if [[ "$found_lib" -eq 0 ]]; then
  echo "no libmux_runtime.* found in ${target_dir} - was 'cargo build -p mux-runtime' run?" >&2
  printf '::error::no libmux_runtime.* found in %s\n' "$target_dir"
  exit 1
fi

echo "Staged install:"
ls -l "$dist/bin" "$dist/lib"

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
    "$@" &
    local pid=$!
    # kill -0 first so a pid reused after the command already exited is never
    # signalled. The watcher is reaped so a fast command returns immediately
    # rather than waiting out the full bound.
    ( sleep "$secs"; kill -0 "$pid" 2>/dev/null && kill -9 "$pid" 2>/dev/null ) &
    local watcher=$!
    local rc=0
    wait "$pid" || rc=$?
    kill "$watcher" 2>/dev/null || true
    wait "$watcher" 2>/dev/null || true
    return "$rc"
  fi
}

# MUX_RUNTIME_LIB is unset deliberately: it wins over every other resolution
# path, so leaving it set would test whatever library it points at instead of
# the one just staged in ../lib. Unset in a subshell rather than via `env -u`,
# which cannot invoke a shell function.
run_packaged() {
  ( unset MUX_RUNTIME_LIB; run_bounded 120 "./$dist/bin/mux${exe_suffix}" run "$1" )
}

printf 'print("hello")\n' > smoke.mux

# One program importing nothing, one pulling in a std module, one reaching a
# heavier optional feature. All must link the bundled library from ../lib rather
# than looking anywhere else.
out="$(run_packaged smoke.mux)"
if [[ "$out" != "hello" ]]; then
  echo "unexpected output: $out" >&2
  printf '::error::packaged compiler produced unexpected output: %s\n' "$out"
  exit 1
fi

run_packaged test_scripts/test_std_math.mux
run_packaged test_scripts/test_std_sql_sqlite.mux

echo "Packaged layout compiled and ran all smoke programs."
