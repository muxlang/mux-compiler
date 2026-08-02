#!/usr/bin/env bash
# Resolve the PR that the triggering Build run belongs to, from trusted
# workflow_run metadata only - never from artifact contents, which a fork
# fully controls. Writes `number=<n>` (or empty, if no unique match) to
# $GITHUB_OUTPUT.
#
# Requires GH_TOKEN, REPO, HEAD_SHA, HEAD_OWNER, HEAD_BRANCH in the
# environment.
set -euo pipefail

# Match the triggering PR by its trusted head owner:branch, then confirm the
# head SHA and require exactly one open match. This is reliable for forks
# (workflow_run.pull_requests[] is empty for them) and cannot be influenced
# by artifact contents.
mapfile -t hits < <(
  gh api "repos/$REPO/pulls?state=open&head=$HEAD_OWNER:$HEAD_BRANCH&per_page=100" \
    --jq '.[] | "\(.number) \(.head.sha)"' \
    | awk -v s="$HEAD_SHA" '$2 == s { print $1 }'
)
if [[ "${#hits[@]}" -eq 1 ]]; then
  echo "number=${hits[0]}" >> "$GITHUB_OUTPUT"
else
  echo "Expected exactly one open PR for $HEAD_OWNER:$HEAD_BRANCH@$HEAD_SHA, found ${#hits[@]}; nothing to comment."
  echo "number=" >> "$GITHUB_OUTPUT"
fi
