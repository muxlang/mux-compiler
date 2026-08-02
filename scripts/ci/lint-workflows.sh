#!/usr/bin/env bash
# Lint .github/workflows/**.yml with actionlint. actionlint also shellchecks
# every inline `run:` bash block itself, as long as a `shellcheck` binary is on
# PATH - there is nothing else to wire up for that half of context #29's ask.
#
# actionlint has no severity flag, so shellcheck's style/info findings are
# excluded here to match the threshold build.yml's own standalone shellcheck
# check already uses (`--severity=warning` on install.sh); warning and error
# still fail the job.
#
# actionlint's shellcheck integration only ever reaches inline `run:` blocks -
# it has no visibility into a script an inline block merely calls. Plan.md
# step 0 moves CI logic out of YAML into scripts/ci/*.sh specifically so it
# can be linted "trivially" - that promise is empty unless those files get
# checked directly too, which is what the loop below does.
set -euo pipefail

actionlint_version="1.7.12"
actionlint_sha256="8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8"

os="$(uname -s)"
arch="$(uname -m)"
if [[ "$os" != "Linux" || "$arch" != "x86_64" ]]; then
  echo "lint-workflows.sh only runs on Linux x86_64 (got ${os}/${arch})" >&2
  exit 1
fi

# Those findings on the run: blocks are what make this gate more than a YAML
# syntax check; a silently-disabled integration would report a clean run
# having checked nothing.
if ! command -v shellcheck >/dev/null; then
  echo "shellcheck not found on PATH; actionlint would silently skip its shellcheck integration" >&2
  exit 1
fi

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

archive="actionlint_${actionlint_version}_linux_amd64.tar.gz"
# --proto "=https": -L follows redirects, and without pinning the protocol a
# redirect could silently downgrade the transfer to plain HTTP (CWE-757). The
# sha256 check below still catches a tampered file, but this stops the
# downgrade itself rather than only detecting its result.
# --retry 5 --retry-all-errors: a single attempt under `set -e` fails the
# whole job on any transient DNS/network/GitHub Releases hiccup before any
# workflow gets linted. Matches the 5-attempt retry already used for the
# SonarCloud issue-count check in these same workflows.
curl -sSfL --proto "=https" --retry 5 --retry-all-errors -o "${workdir}/${archive}" \
  "https://github.com/rhysd/actionlint/releases/download/v${actionlint_version}/${archive}"
echo "${actionlint_sha256}  ${workdir}/${archive}" | sha256sum -c -
tar -xzf "${workdir}/${archive}" -C "${workdir}" actionlint

# style/info findings are excluded (see the file header); actionlint's own
# rules (runner-label, syntax-check, ...) never match this pattern, so they
# always fail the job regardless of level.
"${workdir}/actionlint" -color -ignore 'SC[0-9]+:(style|info):'

# Direct shellcheck pass over the scripts actionlint cannot see into. Same
# warning-and-above threshold as everywhere else in this file.
shellcheck_failed=0
for f in scripts/ci/*.sh; do
  [[ -e "$f" ]] || continue
  shellcheck --severity=warning "$f" || shellcheck_failed=1
done
if [[ "$shellcheck_failed" -ne 0 ]]; then
  echo "shellcheck reported warning-or-above issues in scripts/ci/*.sh" >&2
  exit 1
fi
