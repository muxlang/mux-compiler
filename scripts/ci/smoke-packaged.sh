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

# The macOS binary picks up whatever dylibs Homebrew's LLVM drags in, by
# absolute path. Bundling them here is what makes this a test of a package that
# could actually be installed elsewhere - without it the staged layout only
# works because the build machine happens to have Homebrew (issue #378). The
# script asserts self-containment, so this is also where that regresses loudly.
if [[ "$(uname -s)" == "Darwin" ]]; then
  "$(dirname "$0")/bundle-macos-dylibs.sh" "$dist/bin/mux" "$dist/lib"
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

# Everything past staging is shared with the release install-verification job,
# which has a mux installed by scripts/install.sh rather than a staged dist/.
exec "$(dirname "$0")/smoke-run.sh" "./$dist/bin/mux${exe_suffix}"
