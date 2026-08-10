#!/usr/bin/env bash
# Make a macOS binary self-contained by bundling the non-system dylibs it
# references and rewriting those references to point inside the package.
#
# Why this exists: llvm-sys links LLVM's static archives, but LLVM's own system
# dependencies come in dynamically. Homebrew's llvm@22 is built with the Z3
# solver, so `mux` ends up with a load command naming an absolute, Homebrew-
# specific, VERSION-specific path:
#
#   /opt/homebrew/opt/z3/lib/libz3.4.16.dylib
#
# That resolves on the build runner and nowhere else. A user installing the
# tarball without Homebrew z3 at exactly that path gets "Abort trap: 6" before
# the compiler prints anything - `mux version` and `mux doctor` both die (see
# issue #378). Windows already solves the same problem by copying LLVM's DLLs
# into the artifact; this is the macOS equivalent.
#
# Deliberately generic: it bundles WHATEVER non-system dylibs are referenced
# rather than special-casing z3, because the next Homebrew formula change would
# otherwise reintroduce this with a different library. That is not theoretical -
# the first real run bundled libzstd alongside z3. It ends by asserting that no
# absolute non-system path survives, so the failure mode is a red build here
# rather than an abort trap on a user's machine.
#
# Usage: bundle-macos-dylibs.sh <binary> <lib-dir>
set -euo pipefail

die() { echo "$*" >&2; exit 1; }

[[ "$#" -eq 2 ]] || die "usage: $0 <binary> <lib-dir>"
binary="$1"
lib_dir="$2"

[[ "$(uname -s)" == "Darwin" ]] || die "this script is macOS only"
[[ -f "$binary" ]] || die "no such binary: $binary"
mkdir -p "$lib_dir"

# Paths under these prefixes ship with macOS and are always present, so they are
# left alone. Anything else (/opt/homebrew, /usr/local, a build directory) has
# to travel with the package.
is_system_path() {
  local path="$1"
  case "$path" in
    /usr/lib/*|/System/Library/*) return 0 ;;
    *) return 1 ;;
  esac
}

# Absolute dependency paths of a Mach-O file, excluding its own LC_ID_DYLIB and
# any reference already made relocatable (@rpath, @loader_path, ...).
dependencies_of() {
  local file="$1" own_id dep
  own_id="$(otool -D "$file" | tail -n +2)"
  otool -L "$file" | tail -n +2 | awk '{print $1}' | while read -r dep; do
    # Only absolute paths need rewriting; @rpath and friends are already fine.
    [[ "$dep" == /* ]] || continue
    # A dylib lists its own install name first; that is identity, not a dep.
    [[ "$dep" == "$own_id" ]] && continue
    echo "$dep"
  done
}

# install_name_tool invalidates a Mach-O signature, and on Apple Silicon an
# invalid signature means the loader refuses the file outright. Ad-hoc signing
# is enough to make it loadable again; the release is not notarized either way.
resign() {
  local file="$1"
  codesign --force --sign - "$file" >/dev/null 2>&1 ||
    die "could not re-sign $file after rewriting its load commands"
}

# Breadth-first over the dependency graph: a bundled dylib has dependencies of
# its own, and those have to come along and be rewritten too.
declare -a queue=("$binary")
declare -a bundled=()

while [[ "${#queue[@]}" -gt 0 ]]; do
  current="${queue[0]}"
  queue=("${queue[@]:1}")

  while read -r dep; do
    [[ -n "$dep" ]] || continue
    is_system_path "$dep" && continue

    name="$(basename "$dep")"
    dest="$lib_dir/$name"

    if [[ ! -f "$dest" ]]; then
      [[ -f "$dep" ]] || die "referenced dylib does not exist: $dep"
      echo "  bundling $name"
      cp "$dep" "$dest"
      chmod u+w "$dest"
      # Its own identity has to be relocatable as well, or anything linking it
      # records the absolute path again.
      install_name_tool -id "@rpath/$name" "$dest"
      resign "$dest"
      bundled+=("$dest")
      queue+=("$dest")
    fi

    install_name_tool -change "$dep" "@rpath/$name" "$current"
    resign "$current"
  done < <(dependencies_of "$current")
done

# Every dylib we SHIP has to have a relocatable install name, not only the ones
# pulled in above. libmux_runtime.dylib is the case that matters: cargo stamps a
# cdylib's install name with its own build path
# (target/release/deps/libmux_runtime-<hash>.dylib), and a program records the
# install name of what it linked against - not the path the linker found it at.
# So every program `mux` compiled on macOS pointed at a build tree that exists
# on exactly one machine, and aborted anywhere else. The -Wl,-rpath the compiler
# already passes cannot help while the recorded name is absolute.
shopt -s nullglob
for shipped in "$lib_dir"/*.dylib; do
  current_id="$(otool -D "$shipped" | tail -n +2)"
  [[ "$current_id" == /* ]] || continue
  echo "  normalizing install name of $(basename "$shipped")"
  install_name_tool -id "@rpath/$(basename "$shipped")" "$shipped"
  resign "$shipped"
done
shopt -u nullglob

if [[ "${#bundled[@]}" -eq 0 ]]; then
  echo "No non-system dylibs referenced; nothing to bundle."
else
  # The loader needs to know where @rpath is. bin/ and lib/ are siblings in the
  # installed layout, which is what scripts/install.sh lays down.
  install_name_tool -add_rpath "@executable_path/../lib" "$binary" 2>/dev/null || true
  resign "$binary"
fi

# The point of the whole exercise: prove nothing absolute and non-system is left
# in any load command. Without this the script could silently miss a case and
# the break would resurface as an abort trap on a user's machine.
leaked=0
shopt -s nullglob
for file in "$binary" "$lib_dir"/*.dylib; do
  while read -r dep; do
    [[ -n "$dep" ]] || continue
    if ! is_system_path "$dep"; then
      echo "::error::$(basename "$file") still references a non-system path: $dep" >&2
      leaked=1
    fi
  done < <(dependencies_of "$file")

  # An absolute install name is the other half of the same failure: it is what a
  # program linking this dylib records, so it breaks the program rather than
  # this file. Checking it here is what makes the packaged-artifact job on a PR
  # catch it, instead of only the install verification on a tag.
  [[ "$file" == "$binary" ]] && continue
  shipped_id="$(otool -D "$file" | tail -n +2)"
  if [[ "$shipped_id" == /* ]]; then
    echo "::error::$(basename "$file") has an absolute install name: $shipped_id" >&2
    leaked=1
  fi
done
shopt -u nullglob
[[ "$leaked" -eq 0 ]] || die "bundling did not make the package self-contained"

echo "Package is self-contained: every dylib reference is a system path or @rpath."
