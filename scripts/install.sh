#!/usr/bin/env bash
set -euo pipefail

REPO="muxlang/mux-compiler"
INSTALL_DIR_DEFAULT="${HOME}/.local/bin"
INSTALL_DIR="${MUX_INSTALL_DIR:-$INSTALL_DIR_DEFAULT}"
LIB_DIR_DEFAULT="$(dirname "$INSTALL_DIR")/lib"
LIB_DIR="${MUX_LIB_DIR:-$LIB_DIR_DEFAULT}"
BASE_URL="${MUX_RELEASE_BASE_URL:-https://github.com/${REPO}/releases/latest/download}"
# awk program that extracts the first whitespace-separated field (the checksum).
AWK_FIRST_FIELD='{print $1}'

if [[ "${1:-}" == "--help" ]]; then
  echo "Mux installer"
  echo
  echo "Environment variables:"
  echo "  MUX_INSTALL_DIR   Destination directory for mux binary"
  echo "  MUX_LIB_DIR       Destination directory for mux runtime libraries"
  echo "  MUX_RELEASE_BASE_URL  Override download base URL"
  echo
  exit 0
fi

detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Linux) os="linux" ;;
    Darwin) os="macos" ;;
    *)
      echo "Unsupported operating system: $os"
      exit 1
      ;;
  esac

  case "$arch" in
    x86_64|amd64) arch="x86_64" ;;
    arm64|aarch64) arch="aarch64" ;;
    *)
      echo "Unsupported architecture: $arch"
      exit 1
      ;;
  esac

  echo "${os}-${arch}"
  return 0
}

require_cmd() {
  local cmd
  cmd="$1"

  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "Missing required command: $cmd"
    exit 1
  fi

  return 0
}

require_cmd curl
require_cmd tar

TARGET="$(detect_target)"
ARCHIVE="mux-${TARGET}.tar.gz"
ARCHIVE_URL="${BASE_URL}/${ARCHIVE}"
CHECKSUM_URL="${ARCHIVE_URL}.sha256"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

echo "Downloading ${ARCHIVE}"
curl -fsSL "$ARCHIVE_URL" -o "$tmp_dir/$ARCHIVE"
curl -fsSL "$CHECKSUM_URL" -o "$tmp_dir/$ARCHIVE.sha256"

# Compare the hash directly rather than `sha256sum -c`: the published checksum
# file may reference a path (e.g. "dist/mux-...") that does not exist locally.
if command -v sha256sum >/dev/null 2>&1; then
  expected="$(awk "$AWK_FIRST_FIELD" "$tmp_dir/$ARCHIVE.sha256")"
  actual="$(sha256sum "$tmp_dir/$ARCHIVE" | awk "$AWK_FIRST_FIELD")"
  if [[ "$expected" != "$actual" ]]; then
    echo "Checksum verification failed"
    exit 1
  fi
elif command -v shasum >/dev/null 2>&1; then
  expected="$(awk "$AWK_FIRST_FIELD" "$tmp_dir/$ARCHIVE.sha256")"
  actual="$(shasum -a 256 "$tmp_dir/$ARCHIVE" | awk "$AWK_FIRST_FIELD")"
  if [[ "$expected" != "$actual" ]]; then
    echo "Checksum verification failed"
    exit 1
  fi
else
  echo "Warning: sha256 tool not found; skipping checksum verification"
fi

mkdir -p "$INSTALL_DIR"
mkdir -p "$LIB_DIR"
tar -xzf "$tmp_dir/$ARCHIVE" -C "$tmp_dir"

bundle_root="$tmp_dir/mux-${TARGET}"
bin_path="$bundle_root/bin/mux"
if [[ ! -f "$bin_path" ]]; then
  echo "Could not find mux binary in archive"
  exit 1
fi

cp "$bin_path" "$INSTALL_DIR/mux"
chmod +x "$INSTALL_DIR/mux"

if [[ -d "$bundle_root/lib" ]]; then
  cp -f "$bundle_root/lib"/* "$LIB_DIR/" 2>/dev/null || true
fi

echo "Installed mux to $INSTALL_DIR/mux"
echo "Installed runtime libraries to $LIB_DIR"
if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
  echo "Add this to your shell profile:"
  echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
fi

"$INSTALL_DIR/mux" version
