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

# Runtime library naming is per-platform, and the shared library is not
# optional: a program using libm (pow, say) fails to link without it, because
# no -lm is passed.
exe_suffix=""
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    exe_suffix=".exe"
    runtime_libs=(mux_runtime.lib mux_runtime.dll)
    ;;
  Darwin)
    runtime_libs=(libmux_runtime.a libmux_runtime.dylib)
    ;;
  *)
    runtime_libs=(libmux_runtime.a libmux_runtime.so)
    ;;
esac

rm -rf "$dist"
mkdir -p "$dist/bin" "$dist/lib"
cp "${target_dir}/mux${exe_suffix}" "$dist/bin/"

found_lib=0
for name in "${runtime_libs[@]}"; do
  lib="${target_dir}/${name}"
  if [[ -f "$lib" ]]; then
    cp "$lib" "$dist/lib/"
    # Windows resolves a DLL next to the executable, not from lib/.
    [[ "$name" == *.dll ]] && cp "$lib" "$dist/bin/"
    found_lib=1
  fi
done
if [[ "$found_lib" -eq 0 ]]; then
  echo "no runtime library (${runtime_libs[*]}) in ${target_dir} - was 'cargo build -p mux-runtime' run?" >&2
  printf '::error::no runtime library found in %s\n' "$target_dir"
  exit 1
fi

# mux.exe loads LLVM dynamically on Windows, so a real install ships those DLLs
# beside it. Copying them here keeps this a test of the packaged layout rather
# than of whatever happens to be on PATH.
if [[ "$exe_suffix" == ".exe" && -n "${LLVM_SYS_221_PREFIX:-}" ]]; then
  llvm_bin="${LLVM_SYS_221_PREFIX}/bin"
  if [[ -d "$llvm_bin" ]]; then
    find "$llvm_bin" -maxdepth 1 -type f \
      \( -name 'LLVM*.dll' -o -name 'libclang*.dll' -o -name 'clang*.dll' \) \
      -exec cp {} "$dist/bin/" \; 2>/dev/null || true
  fi
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
