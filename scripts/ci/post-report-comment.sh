#!/usr/bin/env bash
# Assemble and post the sticky PR report comment. Only these artifacts are
# consumed; any other pr-comment-* artifact a fork may have uploaded is
# ignored. Each raw log is treated as UNTRUSTED and escaped/capped HERE.
# Fields: artifact|logfile|job name|kind
#
# Requires GH_TOKEN, REPO, PR, RUN_ID, HEAD_SHA, RUN_URL in the environment.
set -euo pipefail

specs=(
  "pr-comment-valgrind|valgrind.log|Valgrind Memory Checks|gated"
)
jobs_json="$(gh api "repos/$REPO/actions/runs/$RUN_ID/jobs?per_page=100" --jq '.jobs')"
# When the report jobs were path-skipped there are no artifacts. Only
# surface that explicitly for PRs that actually changed the CI/report
# workflows (the case where verifying the pipeline matters); staying quiet
# on unrelated PRs (docs, etc.) avoids comment noise.
touched_ci="$(gh api --paginate "repos/$REPO/pulls/$PR/files?per_page=100" \
  --jq '.[].filename' | grep -Ec '^\.github/workflows/' || true)"

# INJECTION MODEL: the chart below must contain ONLY data this step can
# prove safe. In practice that is two integers parsed out of the log and
# range-checked against each other - never a string read from an artifact.
# Counts that fail validation produce a note, not a chart. The one place raw
# fork text appears is the escaped <details> block.

# Render the fork-controlled raw log inside a code fence it cannot break out
# of: open with MORE backticks than the longest backtick run inside (a fence
# only closes on an equal-or-longer run).
emit_raw_details() {
  local logf="$1" summary="$2" content maxrun fence_len fence
  content="$(tail -n 300 "$logf")"
  maxrun="$(printf '%s' "$content" | { grep -oE '`+' || true; } | awk '{ if (length > m) m = length } END { print m + 0 }')"
  fence_len=3
  [[ "${maxrun:-0}" -ge 3 ]] && fence_len=$(( maxrun + 1 ))
  fence="$(printf '`%.0s' $(seq "$fence_len"))"
  printf '<details><summary>%s</summary>\n\n' "$summary"
  printf '%stext\n' "$fence"
  printf '%s\n' "${content:-(no output captured)}"
  printf '%s\n\n</details>\n\n' "$fence"
}

# Leg A prints "N program(s), M failure(s)"; take those two integers only. A
# log that stops matching means the valgrind job changed its output format:
# say so in the comment. Dropping the chart silently is how a broken report
# goes unnoticed. Both notes are trusted literals - no fork-controlled text
# reaches the body through this path.
emit_valgrind_pie() {
  local logf="$1" line total fails clean
  line="$(grep -Eo '[0-9]+ program\(s\), [0-9]+ failure\(s\)' "$logf" | tail -n 1 || true)"
  if [[ -z "$line" ]]; then
    printf '_Chart unavailable: no "N program(s), M failure(s)" summary line in the valgrind log._\n\n'
    return 0
  fi
  total="$(printf '%s' "$line" | grep -Eo '^[0-9]+' || true)"
  fails="$(printf '%s' "$line" | grep -Eo '[0-9]+ failure' | grep -Eo '^[0-9]+' || true)"
  if ! { [[ "$total" =~ ^[0-9]+$ ]] && [[ "$fails" =~ ^[0-9]+$ ]] && [[ "$total" -gt 0 ]] && [[ "$fails" -le "$total" ]]; }; then
    printf '_Chart unavailable: the valgrind summary line did not parse into usable counts._\n\n'
    return 0
  fi
  clean=$(( total - fails ))
  printf '```mermaid\n'
  # Mermaid's default pie palette is dark blue/purple, which reads as
  # arbitrary. Force clean=green, leaking=red: pie1 colors the first slice
  # below, pie2 the second.
  printf '%s\n' "%%{init: {'themeVariables': {'pie1': '#2da44e', 'pie2': '#cf222e'}}}%%"
  printf 'pie showData\n'
  printf '    title Leg A: compiled programs under Valgrind (%s total)\n' "$total"
  printf '    "Clean" : %s\n' "$clean"
  printf '    "Leaking" : %s\n' "$fails"
  printf '```\n\n'
}

# Identifies our own report comment so each push updates it in place instead
# of appending one per push. Invisible in rendered markdown.
MARKER="<!-- mux-ci-report -->"

: > body.md
wrote=0
for spec in "${specs[@]}"; do
  IFS='|' read -r art logname title kind <<< "$spec"
  logf="reports/$art/$logname"
  concl="$(printf '%s' "$jobs_json" | jq -r --arg n "$title" 'map(select(.name == $n)) | .[0].conclusion // ""')"
  if [[ ! -f "$logf" ]]; then
    # No log: job was path-skipped or produced nothing. Note it only on
    # workflow-touching PRs.
    if [[ "${touched_ci:-0}" -gt 0 ]]; then
      case "$concl" in
        skipped) note="Skipped - no relevant source changed in this PR." ;;
        "") note="Job not found in this run." ;;
        *) note="No report produced (job conclusion: $concl)." ;;
      esac
      printf '## %s - skipped\n\n_%s_\n\n' "$title" "$note" >> body.md
      wrote=1
    fi
    continue
  fi
  if [[ "$kind" == "gated" ]]; then
    case "$concl" in
      success) status="PASSED" ;;
      failure) status="FAILED" ;;
      *) status="${concl:-unknown}" ;;
    esac
  else
    status="report-only"
  fi
  {
    printf '## %s - %s\n\n' "$title" "$status"
    if [[ "$art" == "pr-comment-valgrind" ]]; then
      emit_valgrind_pie "$logf"
      emit_raw_details "$logf" "Full valgrind output (last 300 lines)"
    else
      emit_raw_details "$logf" "Output (last 300 lines)"
    fi
  } >> body.md
  wrote=1
done
if [[ "$wrote" -eq 0 ]]; then
  echo "Nothing to report for this PR; not commenting."
  exit 0
fi
printf '_Commit `%s` - [full run](%s)_\n' "$HEAD_SHA" "$RUN_URL" >> body.md
printf '\n%s\n' "$MARKER" >> body.md

# Update our own previous comment instead of stacking one per push. The
# author filter is load-bearing: anyone can post a comment containing the
# marker, and only a github-actions[bot] comment is ours to edit.
existing="$(gh api --paginate --slurp "repos/$REPO/issues/$PR/comments?per_page=100" \
  | jq -r --arg m "$MARKER" \
      '[.[][] | select(.user.login == "github-actions[bot]" and (.body | contains($m)))] | .[0].id // empty')"
if [[ -n "$existing" ]]; then
  gh api -X PATCH "repos/$REPO/issues/comments/$existing" -F body=@body.md --silent
  echo "Updated report comment $existing on PR #$PR."
else
  gh pr comment "$PR" --repo "$REPO" --body-file body.md
fi
