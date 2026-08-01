#!/usr/bin/env bash
# Fail if SonarCloud reports any new issue on this pull request.
#
# The free SonarCloud plan locks every project to the built-in "Sonar way" gate,
# which only fails on rating/coverage thresholds and so lets new issues (a single
# code smell, say) through. This enforces "zero new issues" ourselves.
#
# Usage: sonar-new-issues.sh <sonar-project-key> <pr-number>
# Requires SONAR_TOKEN in the environment.
set -euo pipefail

project_key="${1:?usage: sonar-new-issues.sh <sonar-project-key> <pr-number>}"
pr_number="${2:?usage: sonar-new-issues.sh <sonar-project-key> <pr-number>}"
: "${SONAR_TOKEN:?SONAR_TOKEN must be set}"

# Query the issues search endpoint rather than measures: its .total is always
# present (0 for a clean PR), so "no new issues" is never confused with "the
# measure was omitted from the response". An earlier version read
# api/measures/component and ended its jq with `// "0"`, which silently turned a
# response carrying no measures into a pass.
api="https://sonarcloud.io/api/issues/search?componentKeys=${project_key}&pullRequest=${pr_number}&resolved=false&inNewCodePeriod=true&ps=1"

# Retry to ride out indexing lag. A count that is still unreadable after all
# attempts fails closed rather than passing.
count=""
for _ in 1 2 3 4 5; do
  if resp="$(curl -sf -u "${SONAR_TOKEN}:" "$api")"; then
    count="$(printf '%s' "$resp" | jq -r '.total // empty')"
    [ -n "$count" ] && break
  fi
  sleep 5
done

if [ -z "$count" ]; then
  echo "::error::Could not read the new-issue count for PR #${pr_number} from SonarCloud after retries."
  exit 1
fi

echo "New SonarCloud issues on PR #${pr_number}: ${count}"
if [ "$count" -gt 0 ]; then
  echo "::error::${count} new SonarCloud issue(s) introduced on this PR. See https://sonarcloud.io/project/issues?id=${project_key}&pullRequest=${pr_number}&resolved=false"
  exit 1
fi
