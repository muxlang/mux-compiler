#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"

with_service_tests=0

cargo_cmd=(cargo)
if [[ -x "$repo_root/scripts/dev-cargo.sh" ]] && [[ -z "${LLVM_CONFIG_PATH:-}" ]] && [[ -z "${LLVM_SYS_221_PREFIX:-}" ]]; then
  cargo_cmd=("$repo_root/scripts/dev-cargo.sh")
fi

while [[ $# -gt 0 ]]; do
  case "$1" in
    --with-service-tests)
      with_service_tests=1
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
if [[ "$with_service_tests" == "1" ]]; then
  run_step "cargo test" env MUX_RUN_SERVICE_TESTS=0 "${cargo_cmd[@]}" test -p mux-lang
else
  run_step "cargo test" "${cargo_cmd[@]}" test -p mux-lang
fi

if [[ "$with_service_tests" == "1" ]]; then
  run_step "cargo test --test service_integration" \
    "${cargo_cmd[@]}" test -p mux-lang --test service_integration -- --nocapture
fi
