#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"

export LOCAL_UID="${LOCAL_UID:-$(id -u)}"
export LOCAL_GID="${LOCAL_GID:-$(id -g)}"

mkdir -p "$repo_root/.docker-cache/cargo/registry" "$repo_root/.docker-cache/cargo/git" "$repo_root/.docker-cache/target"
mkdir -p "$repo_root/.docker-home/.cache"

"$repo_root/scripts/wait-for-integration.sh"

# The dev image is the same one CI publishes to GHCR. Building it locally
# compiles cargo-insta and cargo-llvm-cov from source, which is minutes, so CI
# pulls the published copy instead.
#
# Opt-in rather than the default: a developer editing the Dockerfile must get
# their edit, not a stale published image. CI sets MUX_CI_PULL_IMAGE=1; locally
# the behaviour is unchanged.
if [[ "${MUX_CI_PULL_IMAGE:-0}" == "1" ]]; then
  docker compose -f "$repo_root/infra/docker-compose.integration.yml" pull dev
else
  docker compose -f "$repo_root/infra/docker-compose.integration.yml" build dev
fi

docker compose -f "$repo_root/infra/docker-compose.integration.yml" up -d dev
