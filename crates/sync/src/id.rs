//! Pure-Rust RFC 9562 UUIDv7 generator for `public_id` values (`DATA-MODEL.md` §3, §5).
//!
//! Uses an injected [`Clock`] and [`Rng`] so all minted `public_id`s are deterministic
//! in tests without pulling in additional UUID dependencies.

use debloatmail_core::PublicId;
use debloatmail_engine::{Clock, Rng};

/// Generates a canonical lowercase 8-4-4-4-12 RFC 9562 UUIDv7 [`PublicId`] from a 48-bit
/// Unix millisecond timestamp and an injected [`Rng`].
///
/// Layout (128 bits):
/// - Bits 0..48: `unix_ts_ms` (big-endian)
/// - Bits 48..52: `ver = 0b0111` (7)
/// - Bits 52..64: `rand_a` (12 random bits)
/// - Bits 64..66: `var = 0b10` (RFC 9562 variant)
/// - Bits 66..128: `rand_b` (62 random bits)
pub fn mint_uuid_v7(unix_ms: u64, rng: &mut dyn Rng) -> PublicId {
    let mut bytes = [0u8; 16];

    // 48-bit big-endian Unix epoch in milliseconds
    let ts_bytes = unix_ms.to_be_bytes();
    bytes[0..6].copy_from_slice(&ts_bytes[2..8]);

    // Generate 10 pseudo-random bytes from `rng`
    for chunk in bytes[6..16].chunks_mut(2) {
        let sample = (rng.next_f64().clamp(0.0, 0.999_999_999_999) * 65536.0) as u16;
        let b = sample.to_be_bytes();
        chunk[0] = b[0];
        if chunk.len() > 1 {
            chunk[1] = b[1];
        }
    }

    // Set RFC 9562 version 7 (0b0111 in high nibble of byte 6)
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    // Set RFC 9562 variant 0b10 (in top 2 bits of byte 8)
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    let formatted = format_uuid_bytes(&bytes);
    PublicId::new(formatted)
        .unwrap_or_else(|_| unreachable!("formatted UUIDv7 is always non-empty"))
}

/// Generates a canonical RFC 9562 UUIDv7 [`PublicId`] using `clock.now_secs() * 1000`.
pub fn mint_uuid_v7_from_clock(clock: &dyn Clock, rng: &mut dyn Rng) -> PublicId {
    let unix_ms = (clock.now_secs().max(0) as u64).saturating_mul(1_000);
    mint_uuid_v7(unix_ms, rng)
}

fn format_uuid_bytes(b: &[u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(36);
    for (idx, &byte) in b.iter().enumerate() {
        if idx == 4 || idx == 6 || idx == 8 || idx == 10 {
            out.push('-');
        }
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StepRng(u64);
    impl Rng for StepRng {
        fn next_f64(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
        }
    }

    #[test]
    fn test_uuid_v7_format_version_variant_and_time_ordering() {
        let mut rng = StepRng(42);
        let id1 = mint_uuid_v7(1_700_000_000_000, &mut rng);
        let id2 = mint_uuid_v7(1_700_000_001_000, &mut rng);

        let s1 = id1.as_str();
        let s2 = id2.as_str();
        assert_eq!(s1.len(), 36);
        assert_eq!(s2.len(), 36);

        // Version digit at index 14 must be '7'
        assert_eq!(&s1[14..15], "7");
        // Variant digit at index 19 must be '8', '9', 'a', or 'b'
        let var_char = s1.as_bytes()[19] as char;
        assert!(matches!(var_char, '8' | '9' | 'a' | 'b'));

        // Lexicographical order matches chronological order
        assert!(s1 < s2);
    }
}
