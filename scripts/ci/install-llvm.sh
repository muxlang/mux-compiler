#!/usr/bin/env bash
# Install LLVM/clang from apt.llvm.org for the running Ubuntu release.
#
# Extracted from five near-identical inline copies in build.yml (plus more in
# release.yml and mux-website-api's canary). Having one copy matters beyond
# tidiness: the apt source line is derived from `lsb_release -cs`, so a runner
# image moving to a new Ubuntu release has exactly one place that has to be
# right, instead of six that can silently disagree.
#
# Usage: install-llvm.sh [extra components...]
#
# Components are named WITHOUT the version suffix and get it appended, so the
# version lives here only:
#
#   scripts/ci/install-llvm.sh            # llvm-dev, clang, libpolly-dev
#   scripts/ci/install-llvm.sh lld        # ... plus lld-22
#
# LLVM_VERSION overrides the major version (default 22).
set -euo pipefail

llvm_version="${LLVM_VERSION:-22}"

sudo apt-get update
sudo apt-get install -y --no-install-recommends ca-certificates gnupg lsb-release wget

# A literal https:// URL plus --max-redirect=0 means the key download cannot be
# downgraded to http by a redirect.
sudo wget --max-redirect=0 -O /usr/share/keyrings/llvm-snapshot.gpg.key \
  https://apt.llvm.org/llvm-snapshot.gpg.key

codename="$(lsb_release -cs)"
echo "deb [signed-by=/usr/share/keyrings/llvm-snapshot.gpg.key] https://apt.llvm.org/${codename}/ llvm-toolchain-${codename}-${llvm_version} main" \
  | sudo tee /etc/apt/sources.list.d/llvm.list

packages=(
  "llvm-${llvm_version}-dev"
  "clang-${llvm_version}"
  "libpolly-${llvm_version}-dev"
)
for component in "$@"; do
  packages+=("${component}-${llvm_version}")
done

sudo apt-get update
sudo apt-get install -y "${packages[@]}"

echo "Installed LLVM ${llvm_version} for ${codename}: ${packages[*]}"
