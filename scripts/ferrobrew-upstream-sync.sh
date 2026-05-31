#!/usr/bin/env bash
#
# Computes the upstream Homebrew Ruby changes since the last synced commit, for the
# Claude-powered porting bot (.github/workflows/ferrobrew-upstream-sync.yml).
#
# Writes a human-readable summary to $OUT_DIR/upstream-changes.md and the full Ruby diff to
# $OUT_DIR/upstream-ruby.diff, then prints `new_sha=<sha>` and `has_changes=<true|false>` on
# stdout (consumed via $GITHUB_OUTPUT). Nothing else goes to stdout.
set -euo pipefail

UPSTREAM_URL="${UPSTREAM_URL:-https://github.com/Homebrew/brew.git}"
UPSTREAM_BRANCH="${UPSTREAM_BRANCH:-main}"
STATE_FILE="${STATE_FILE:-.ferrobrew/sync-state.json}"
OUT_DIR="${OUT_DIR:-.ferrobrew/sync}"
mkdir -p "${OUT_DIR}"

if ! git remote | grep -qx upstream
then
  git remote add upstream "${UPSTREAM_URL}"
fi
git fetch --quiet upstream "${UPSTREAM_BRANCH}"
NEW_SHA="$(git rev-parse "upstream/${UPSTREAM_BRANCH}")"

LAST_SHA="$(python3 -c "import json; print(json.load(open('${STATE_FILE}')).get('last_synced_sha') or '')" 2>/dev/null || true)"

if [[ -z "${LAST_SHA}" ]]
then
  echo "No previous sync recorded; baseline established at ${NEW_SHA}. Nothing to port yet." \
    >"${OUT_DIR}/upstream-changes.md"
  : >"${OUT_DIR}/upstream-ruby.diff"
  echo "new_sha=${NEW_SHA}"
  echo "has_changes=false"
  exit 0
fi

RANGE="${LAST_SHA}..${NEW_SHA}"
{
  echo "# Upstream Homebrew changes to consider porting"
  echo
  echo "Range: \`${RANGE}\`"
  echo
  echo "## Commits touching Library/Homebrew (Ruby)"
  git log --no-merges --oneline "${RANGE}" -- Library/Homebrew 2>/dev/null || echo "(range unavailable)"
  echo
  echo "## Changed Ruby files (name + status)"
  git diff --name-status "${RANGE}" -- 'Library/Homebrew/**/*.rb' 2>/dev/null || true
} >"${OUT_DIR}/upstream-changes.md"

git diff "${RANGE}" -- 'Library/Homebrew/**/*.rb' >"${OUT_DIR}/upstream-ruby.diff" 2>/dev/null || true

if [[ -s "${OUT_DIR}/upstream-ruby.diff" ]]
then
  echo "new_sha=${NEW_SHA}"
  echo "has_changes=true"
else
  echo "new_sha=${NEW_SHA}"
  echo "has_changes=false"
fi
