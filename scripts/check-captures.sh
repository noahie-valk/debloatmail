#!/usr/bin/env bash
# check-captures.sh — the sanitizer gate (docs/PROBES.md §8).
#
# Fails if a committed capture under tests/imap/recorded/ could contain real
# correspondence. Crude on purpose: seconds to run, no toolchain, and a false
# positive is far cheaper than a leaked mailbox. The full sanitizer is the
# Rust implementation; this is the tripwire.
set -euo pipefail

root="tests/imap/recorded"
fail=0

note() { printf 'check-captures: %s\n' "$*" >&2; fail=1; }

if [ ! -d "$root" ]; then
  echo "check-captures: $root absent, nothing to check"
  exit 0
fi

# 1. No unsanitized captures at any depth. Mirrors .gitignore; catches `git add -f`.
while IFS= read -r -d '' d; do
  note "unsanitized capture directory committed: ${d#"$PWD"/}"
done < <(find "$root" -type d -name raw -print0)

# 2. Reserved names only (RFC 2606 / RFC 6761).
reserved='@([[:alnum:]-]+\.)*(example\.(com|org|net)|invalid)$'
while IFS= read -r -d '' f; do
  offenders=$(grep -aoE '[[:alnum:]._%+-]+@[[:alnum:].-]+' "$f" \
    | grep -avE "$reserved" \
    | sort -u | head -3 || true)
  if [ -n "$offenders" ]; then
    note "non-reserved address in ${f#"$PWD"/}:"
    printf '  %s\n' "$offenders" >&2
  fi
done < <(find "$root" -type f -not -name '*.md' -print0)

# 3. No base64 run long enough to be a real attachment (PROBES.md §8).
while IFS= read -r -d '' f; do
  long=$(awk 'length > 65536 {print NR": "length; exit}' "$f" || true)
  if [ -n "$long" ]; then
    note "line over 64 KiB in ${f#"$PWD"/} (line/len $long) — attachment not stubbed"
  fi
done < <(find "$root" -type f -not -name '*.md' -print0)

# 4. Size ceiling: the corpus is structure, not content.
while IFS= read -r -d '' f; do
  note "capture over 2 MiB: ${f#"$PWD"/}"
done < <(find "$root" -type f -size +2M -print0)

# 5. Every capture needs a sibling README naming provider, date, and case.
while IFS= read -r -d '' d; do
  [ -f "$d/README.md" ] || note "capture without README.md: ${d#"$PWD"/}"
done < <(find "$root" -mindepth 3 -maxdepth 3 -type d -print0)

if [ "$fail" -ne 0 ]; then
  echo "check-captures: FAILED — see docs/PROBES.md §8" >&2
  exit 1
fi
echo "check-captures: ok"
