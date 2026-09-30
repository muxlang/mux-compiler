#!/usr/bin/env bash
# Prove a given `mux` can serve LSP requests and compile and run real programs.
#
# Takes the executable rather than a layout, so it works for anything that
# produces one: a staged dist/ tree (scripts/ci/smoke-packaged.sh) or an install
# performed by scripts/install.sh or install.ps1 from a release artifact. That
# second caller is the point - it is what stops a release shipping a compiler
# that cannot compile hello world.
#
# Usage: smoke-run.sh <path-to-mux-executable> [extra .mux program ...]
#
# Must be run from the repository root: the default programs come from
# test_scripts/.
set -euo pipefail

mux="${1:?usage: smoke-run.sh <path-to-mux-executable> [program ...]}"
shift

smoke_tmp="$(mktemp -d "${TMPDIR:-/tmp}/mux-smoke.XXXXXX")"
cleanup() {
  rm -rf -- "$smoke_tmp"
}
trap cleanup EXIT INT TERM

if [[ ! -x "$mux" && ! -f "$mux" ]]; then
  echo "no mux executable at $mux" >&2
  printf '::error::no mux executable at %s\n' "$mux"
  exit 1
fi

echo "Smoke-testing: $mux"
"$mux" version > /dev/null

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

# Keep this on the packaged binary in the cross-platform install matrix. It
# proves the language server starts and completes its protocol lifecycle
# without relying on the runtime archive used by compiled Mux programs.
if ! (
  unset MUX_RUNTIME_LIB
  run_bounded 20 python3 - "$mux" 2> "$smoke_tmp/lsp.stderr" <<'PY'
import json
import os
import select
import subprocess
import sys
import time


def read_exact(stream, length, deadline):
    result = bytearray()
    while len(result) < length:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([stream], [], [], remaining)[0]:
            raise TimeoutError("timed out waiting for an LSP response")
        chunk = os.read(stream.fileno(), length - len(result))
        if not chunk:
            raise EOFError("Mux LSP server closed stdout before replying")
        result.extend(chunk)
    return bytes(result)


def read_message(stream):
    deadline = time.monotonic() + 5
    headers = bytearray()
    while not headers.endswith(b"\r\n\r\n"):
        headers.extend(read_exact(stream, 1, deadline))
    content_length = next(
        int(line.split(b":", 1)[1].strip())
        for line in headers.split(b"\r\n")
        if line.lower().startswith(b"content-length:")
    )
    return json.loads(read_exact(stream, content_length, deadline))


def send_message(stream, message):
    body = json.dumps(message, separators=(",", ":")).encode()
    stream.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
    stream.flush()


process = subprocess.Popen(
    [sys.argv[1], "lsp"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    bufsize=0,
)
try:
    send_message(
        process.stdin,
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"capabilities": {}}},
    )
    initialized = read_message(process.stdout)
    capabilities = initialized.get("result", {}).get("capabilities", {})
    if (
        initialized.get("id") != 1
        or capabilities.get("textDocumentSync") != 1
        or capabilities.get("hoverProvider") is not True
    ):
        raise RuntimeError(f"unexpected initialize response: {initialized}")

    send_message(process.stdin, {"jsonrpc": "2.0", "method": "initialized", "params": {}})
    send_message(process.stdin, {"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": None})
    shutdown = read_message(process.stdout)
    if shutdown.get("id") != 2 or shutdown.get("result") is not None:
        raise RuntimeError(f"unexpected shutdown response: {shutdown}")

    send_message(process.stdin, {"jsonrpc": "2.0", "method": "exit", "params": None})
    process.stdin.close()
    if process.wait(timeout=5) != 0:
        raise RuntimeError(f"Mux LSP exited with status {process.returncode}")
except Exception:
    if process.poll() is None:
        process.kill()
    process.wait()
    sys.stderr.write(process.stderr.read().decode(errors="replace"))
    raise
else:
    process.stderr.close()
    process.stdout.close()
PY
); then
  cat "$smoke_tmp/lsp.stderr" >&2
  printf '::error::%s failed the packaged LSP lifecycle smoke\n' "$mux"
  exit 1
fi
echo "OK: completed the LSP initialize/shutdown lifecycle without MUX_RUNTIME_LIB."

# MUX_RUNTIME_LIB is unset deliberately: it wins over every other resolution
# path, so leaving it set would test whatever it points at rather than the
# runtime this install actually shipped. Unset in a subshell rather than via
# `env -u`, which cannot invoke a shell function.
run_program() {
  local program="$1"
  ( unset MUX_RUNTIME_LIB; run_bounded 120 "$mux" run "$program" )
}

# `mux` reports a failed link as "linker command failed with exit code N"; the
# linker's own output now comes with it, but a backtrace adds the compiler-side
# detail. Re-run on failure so the log is actionable the first time.
diagnose() {
  local program="$1"
  echo "--- '$program' failed; re-running with RUST_BACKTRACE=1 for detail ---" >&2
  ( unset MUX_RUNTIME_LIB; RUST_BACKTRACE=1 run_bounded 120 "$mux" run "$program" ) >&2 2>&1 || true
}

smoke_source="$smoke_tmp/smoke.mux"
smoke_output="$smoke_tmp/smoke.out"
printf 'print("hello")\n' > "$smoke_source"

# Output goes to a file rather than a command substitution: a substitution's pipe
# stays open until every writer exits, so a grandchild outliving `mux run` would
# block the caller past its own timeout.
if ! run_program "$smoke_source" > "$smoke_output"; then
  diagnose "$smoke_source"
  printf '::error::%s failed to compile and run smoke.mux\n' "$mux"
  exit 1
fi
out="$(cat "$smoke_output")"
if [[ "$out" != "hello" ]]; then
  echo "unexpected output: $out" >&2
  printf '::error::%s produced unexpected output: %s\n' "$mux" "$out"
  exit 1
fi

# One program importing nothing, one pulling in a std module, one reaching a
# heavier optional feature. Callers may append more.
programs=(test_scripts/test_std_math.mux test_scripts/test_std_sql_sqlite.mux "$@")
for program in "${programs[@]}"; do
  if ! run_program "$program"; then
    diagnose "$program"
    printf '::error::%s failed on %s\n' "$mux" "$program"
    exit 1
  fi
done

echo "OK: compiled and ran ${#programs[@]} programs plus smoke.mux."
