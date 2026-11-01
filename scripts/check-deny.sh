#!/usr/bin/env bash
# check-deny.sh — run cargo-deny against the workspace.
#
# Checks enforced (all configured in deny.toml):
#   licenses   — every dep must carry an approved permissive/weak-copyleft license
#   bans       — forbidden crates (io-email, io-keyring, OpenSSL, curl, WebView…)
#   advisories — known CVEs / security advisories (RustSec DB)
#
# Usage:
#   ./scripts/check-deny.sh            # run all checks
#   ./scripts/check-deny.sh licenses   # run one check only

set -euo pipefail

# Always run from the repository root regardless of where the script is invoked.
cd "$(dirname "$(realpath "$0")")/.."

CHECK="${1:-all}"

if ! command -v cargo-deny &>/dev/null; then
    echo "cargo-deny not found. Install it with: cargo install cargo-deny --locked"
    exit 1
fi

echo "==> cargo deny check $CHECK"
cargo deny check "$CHECK"
echo "==> cargo deny passed."
