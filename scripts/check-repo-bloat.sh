#!/usr/bin/env bash
# Fails when build artifacts or oversized blobs leak into git history.
#
# Motivated by the Flatpak OSTree store: content-addressed .filez objects were
# committed to gh-pages on every deploy, growing history to 276 MB for an
# 8.6 MB site. Run on every PR and push to main.

set -euo pipefail

LIMIT_FILE=5242880      # 5 MiB, largest legitimate tracked file today: 3.77 MB icon.xcf
LIMIT_HISTORY=62914560  # 60 MiB of blob data across all refs, current: ~29 MB

failures=0
fail() {
  echo "::error::$*"
  failures=$((failures + 1))
}

artifacts=$(git ls-files -- repo _site '*.filez' | wc -l)
[ "$artifacts" -eq 0 ] || fail "$artifacts tracked build artifacts (repo/, _site/, *.filez) — they belong on gh-pages only"

oversized=$(git ls-tree -r -l HEAD | awk -F'\t' -v lim="$LIMIT_FILE" '
  { n = split($1, a, / +/); if (a[n] + 0 > lim) printf "%s (%.2f MB) ", $2, a[n] / 1048576 }
')
[ -z "$oversized" ] || fail "tracked files over $((LIMIT_FILE / 1048576)) MB: $oversized"

history=$(git rev-list --objects --all |
  git cat-file --batch-check='%(objecttype) %(objectname) %(objectsize) %(rest)' |
  awk '$1 == "blob" { s += $3 } END { printf "%d", s }')

echo "history blob bytes: $((history / 1048576)) MB (limit $((LIMIT_HISTORY / 1048576)) MB)"
[ "$history" -le "$LIMIT_HISTORY" ] ||
  fail "history holds $((history / 1048576)) MB of blobs (limit $((LIMIT_HISTORY / 1048576)) MB) — artifacts leaked into history"

echo "tracked artifacts: $artifacts, failures: $failures"
exit $((failures > 0))
