# Timestamp fields must end in _at (allowlist: date, since)
bad=$(grep -rnE 'pub [a-z_]+: (Option<)?Timestamp' crates/core/src \
  | grep -vE 'pub [a-z_]+_at:|pub (date|since):')
[ -n "$bad" ] && { echo "Timestamp naming violations:"; echo "$bad"; exit 1; }

# Source-specific terms must not appear in core
grep -rnE 'published' crates/core/src && exit 1
