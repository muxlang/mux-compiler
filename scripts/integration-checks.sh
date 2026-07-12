#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"

export LOCAL_UID="${LOCAL_UID:-$(id -u)}"
export LOCAL_GID="${LOCAL_GID:-$(id -g)}"

cleanup() {
  "$repo_root/scripts/integration-down.sh"
  return 0
}
trap cleanup EXIT

"$repo_root/scripts/integration-up.sh"

docker compose -f "$repo_root/infra/docker-compose.integration.yml" run --rm dev \
  ./scripts/run-checks.sh --with-service-tests
