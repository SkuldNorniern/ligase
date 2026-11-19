#!/usr/bin/env bash
# release notes from the commit subjects since the last v* tag, grouped by
# type, on stdout. `packaging/changelog.sh [tag being cut]`
set -euo pipefail

current=${1:-}
prev=$(git tag -l 'v*' --sort=-v:refname | grep -vx "${current:-^$}" | head -n 1 || true)
range=${prev:+$prev..HEAD}
range=${range:-HEAD}

section() {
  git log "$range" --pretty=format:"%s" \
    | grep -E "^$1(\([^)]*\))?: " \
    | sed -E "s/^$1(\([^)]*\))?: /- /" \
    || true
}

echo "## What's changed"
echo
for pair in "feat:Features" "fix:Fixes" "perf:Performance" "style:Look" "docs:Docs" "refactor:Refactor" "build:Build"; do
  items=$(section "${pair%%:*}")
  if [[ -n "$items" ]]; then
    echo "### ${pair#*:}"
    echo "$items"
    echo
  fi
done
