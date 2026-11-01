#!/usr/bin/env bash

set -euo pipefail

# Ensure we run from repository root
cd "$(dirname "$(realpath "$0")")/.."

# Timestamp fields must end in _at (allowlist: date, since, last_activity per DATA-MODEL.md §4.1)
bad=$(grep -rnE 'pub [a-z_]+: (Option<)?Timestamp' crates/core/src \
  | grep -vE 'pub [a-z_]+_at:|pub (date|since|last_activity):' || true)
if [ -n "$bad" ]; then
  echo "Timestamp naming violations:"
  echo "$bad"
  exit 1
fi

# Source-specific terms must not appear in core
if grep -rnE 'published' crates/core/src; then
  echo "Violation: source-specific term 'published' found in crates/core/src"
  exit 1
fi

echo "All invariants verified successfully."
exit 0
