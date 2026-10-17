#!/usr/bin/env bash

set -euo pipefail

# Ensure we are in the project root
cd "$(dirname "$(realpath "$0")")/.."

# Format code
cargo fmt --all

# Lint with clippy, treating warnings as errors
cargo clippy --workspace --all-targets -- -D warnings

# Run full test suite
cargo test --workspace

# Optionally run cargo audit (if cargo-audit is installed)
if command -v cargo-audit >/dev/null 2>&1; then
  echo "Running cargo audit..."
  cargo audit
else
  echo "cargo-audit not installed; skipping audit."
fi
