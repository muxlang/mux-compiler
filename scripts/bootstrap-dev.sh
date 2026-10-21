#!/usr/bin/env bash
set -euo pipefail

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  sudo_cmd=""
else
  sudo_cmd="sudo"
fi

llvm22_ready() {
  local config
  if command -v llvm-config-22 >/dev/null 2>&1; then
    config="llvm-config-22"
  elif command -v llvm-config >/dev/null 2>&1; then
    config="llvm-config"
  else
    return 1
  fi

  local version major
  version="$("$config" --version 2>/dev/null || true)"
  major="${version%%.*}"
  [[ "$major" == "22" ]]
}

if [[ -f /etc/arch-release ]]; then
  if llvm22_ready; then
    echo "LLVM 22 is already installed on Arch Linux"
  else
    # Arch tracks the newest LLVM in the unversioned packages. Install the
    # versioned packages so LLVM 22 remains available alongside the system
    # toolchain as Arch advances to newer LLVM releases.
    echo "Installing LLVM 22 toolchain for Arch Linux via pacman"
    $sudo_cmd pacman -S --needed llvm22 clang22 lld22

    if ! llvm22_ready; then
      echo "LLVM 22 installation did not complete successfully."
      echo "Expected llvm-config version 22.x after install."
      exit 1
    fi
  fi
elif [[ -f /etc/debian_version ]]; then
  echo "Installing LLVM 22 toolchain for Debian/Ubuntu"
  $sudo_cmd apt-get update
  $sudo_cmd apt-get install -y --no-install-recommends ca-certificates gnupg lsb-release wget
  $sudo_cmd wget --max-redirect=0 -O /usr/share/keyrings/llvm-snapshot.gpg.key \
    https://apt.llvm.org/llvm-snapshot.gpg.key
  echo "deb [signed-by=/usr/share/keyrings/llvm-snapshot.gpg.key] https://apt.llvm.org/$(lsb_release -cs)/ llvm-toolchain-$(lsb_release -cs)-22 main" \
    | $sudo_cmd tee /etc/apt/sources.list.d/llvm.list >/dev/null
  $sudo_cmd apt-get update
  $sudo_cmd apt-get install -y llvm-22 llvm-22-dev clang-22 lld-22 libpolly-22-dev
elif [[ "$(uname -s)" == "Darwin" ]]; then
  echo "Installing LLVM 22 toolchain with Homebrew"
  brew install llvm@22
else
  echo "Unsupported OS for automatic setup. Install LLVM 22 and clang 22 manually."
  exit 1
fi

echo
echo "Installed contributor dependencies."

# Set up git pre-commit hooks
echo "Configuring git hooks..."
git config core.hooksPath .github/hooks

echo "Git hooks configured."
echo "Use scripts/dev-cargo.sh for source builds without manual env vars."
echo "Example: scripts/dev-cargo.sh build"
