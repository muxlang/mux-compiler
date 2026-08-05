#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"

service_tests_only=0

cargo_cmd=(cargo)
if [[ -x "$repo_root/scripts/dev-cargo.sh" ]] && [[ -z "${LLVM_CONFIG_PATH:-}" ]] && [[ -z "${LLVM_SYS_221_PREFIX:-}" ]]; then
  cargo_cmd=("$repo_root/scripts/dev-cargo.sh")
fi

while [[ $# -gt 0 ]]; do
  case "$1" in
    --service-tests-only)
      service_tests_only=1
      shift
      ;;
    *)
      echo "Unknown argument: $1"
      exit 1
      ;;
  esac
done

run_step() {
  local name="$1"
  shift

  local start end duration
  start="$(date +%s)"
  echo
  echo ">>> ${name}"
  "$@"
  end="$(date +%s)"
  duration="$((end - start))"
  echo "<<< ${name} completed in ${duration}s"

  return 0
}

# mux-runtime is a dependency, and cargo builds only a dependency's rlib -
# not the staticlib compiled Mux programs link against. Build it explicitly
# or every program fails to link with "Could not locate the mux-runtime
# library".
run_step "cargo build" "${cargo_cmd[@]}" build -p mux-runtime -p mux-lang

# --service-tests-only runs the service_integration suite and nothing else.
#
# Its only caller is integration-checks.sh, which exists to reach postgres and
# the echo servers from inside the compose "dev" container. That job runs in
# parallel with Rust Checks, which already runs the full unit suite on the host
# runner - so running the unit suite again in the container added roughly 180s
# to every PR and every push while testing nothing the other job had not.
#
# The service suite is self-contained and does not depend on the unit run or on
# the build above: tests/service_integration.rs builds its own mux-runtime and
# drives the compiler through `cargo run` with
# CARGO_TARGET_DIR=target/service-integration.
#
# MUX_RUN_SERVICE_TESTS is no longer forced to 0. It only existed to stop the
# service tests firing during the general run that this branch no longer
# performs; the container sets it to 1, which is what the suite below needs.
if [[ "$service_tests_only" == "1" ]]; then
  run_step "cargo test --test service_integration" \
    "${cargo_cmd[@]}" test -p mux-lang --test service_integration -- --nocapture
else
  run_step "cargo test" "${cargo_cmd[@]}" test -p mux-lang
fi
