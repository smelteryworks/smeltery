//! RFC 6238 time-based one-time passwords (HMAC-SHA1, six digits, 30-second steps from the Unix epoch) and the
//! RFC 4648 base32 the authenticator apps read the secret in.

use hmac::{Hmac, Mac};
use sha1::Sha1;
use smeltery_core::crypto::constant_time_eq;

/// Seconds per step.
pub(crate) const PERIOD: i64 = 30;
/// Digits per code.
pub(crate) const DIGITS: u32 = 6;

/// RFC 4648 base32 without padding (the form `otpauth://` URIs carry).
pub(crate) fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut buffer: u64 = 0;
    let mut bits = 0u32;
    for byte in bytes {
        buffer = (buffer << 8) | u64::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let index = usize::try_from((buffer >> bits) & 31).unwrap_or(0);
            out.push(char::from(*ALPHABET.get(index).unwrap_or(&b'A')));
        }
    }
    if bits > 0 {
        let index = usize::try_from((buffer << (5 - bits)) & 31).unwrap_or(0);
        out.push(char::from(*ALPHABET.get(index).unwrap_or(&b'A')));
    }
    out
}

/// RFC 4648 base32 back to bytes (letters in either case, no padding); `None` for anything else.
pub(crate) fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut buffer: u64 = 0;
    let mut bits = 0u32;
    for c in text.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        buffer = (buffer << 5) | u64::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buffer >> bits) & 0xff).ok()?);
        }
    }
    Some(out)
}

/// RFC 4226 HOTP: the six-digit code of `secret` at `counter`.
pub(crate) fn hotp(secret: &[u8], counter: u64) -> String {
    let Ok(mut mac) = <Hmac<Sha1> as Mac>::new_from_slice(secret) else {
        // HMAC takes keys of any length; this cannot happen.
        return String::new();
    };
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest.last().copied().unwrap_or(0) & 0x0f);
    let part = digest.get(offset..offset + 4).unwrap_or(&[0, 0, 0, 0]);
    let binary = (u32::from(part.first().copied().unwrap_or(0) & 0x7f) << 24)
        | (u32::from(part.get(1).copied().unwrap_or(0)) << 16)
        | (u32::from(part.get(2).copied().unwrap_or(0)) << 8)
        | u32::from(part.get(3).copied().unwrap_or(0));
    format!("{:06}", binary % 10u32.pow(DIGITS))
}

/// The time step of the Unix time `now`.
pub(crate) fn step(now: i64) -> i64 {
    now.div_euclid(PERIOD)
}

/// The code of `secret` at time step `step` (empty for a negative step).
pub(crate) fn code_at(secret: &[u8], step: i64) -> String {
    u64::try_from(step).map_or_else(|_| String::new(), |counter| hotp(secret, counter))
}

/// The step of `secret` whose code is `code`, among `current - window ..= current + window`; `None` when none.
/// Every candidate is computed and compared in constant time (no early exit).
pub(crate) fn matching_step(secret: &[u8], code: &str, current: i64, window: u8) -> Option<i64> {
    let code = code.trim();
    let wanted = code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit());
    let window = i64::from(window);
    let mut found = None;
    for candidate in current - window..=current + window {
        let same = constant_time_eq(&code_at(secret, candidate), code);
        if same && wanted {
            found = Some(candidate);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    const RFC_SECRET: &[u8] = b"12345678901234567890";

    #[test]
    fn hotp_matches_rfc_4226_appendix_d() {
        let expected = [
            "755224", "287082", "359152", "969429", "338314", "254676", "287922", "162583",
            "399871", "520489",
        ];
        for (counter, code) in expected.iter().enumerate() {
            assert_eq!(hotp(RFC_SECRET, counter as u64), *code, "counter {counter}");
        }
    }

    #[test]
    fn totp_matches_rfc_6238_appendix_b_sha1_rows() {
        // The SHA-1 rows of Appendix B (eight digits there; their last six here).
        for (time, eight) in [
            (59_i64, "94287082"),
            (1_111_111_109, "07081804"),
            (1_111_111_111, "14050471"),
            (1_234_567_890, "89005924"),
            (2_000_000_000, "69279037"),
            (20_000_000_000, "65353130"),
        ] {
            assert_eq!(code_at(RFC_SECRET, step(time)), &eight[2..], "T = {time}");
        }
    }

    #[test]
    fn base32_round_trips_and_matches_rfc_4648() {
        assert_eq!(base32(b""), "");
        assert_eq!(base32(b"f"), "MY");
        assert_eq!(base32(b"fo"), "MZXQ");
        assert_eq!(base32(b"foo"), "MZXW6");
        assert_eq!(base32(b"foob"), "MZXW6YQ");
        assert_eq!(base32(b"fooba"), "MZXW6YTB");
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32(RFC_SECRET), "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(base32_decode(&base32(&bytes)).unwrap(), bytes);
        assert_eq!(base32_decode("mzxw6ytboi").unwrap(), b"foobar");
        assert!(base32_decode("MZ1").is_none());
    }

    #[test]
    fn only_codes_inside_the_window_match_and_bad_input_never_does() {
        let now = step(1_234_567_890);
        let code = code_at(RFC_SECRET, now);
        assert_eq!(matching_step(RFC_SECRET, &code, now, 1), Some(now));
        assert_eq!(matching_step(RFC_SECRET, &code, now + 1, 1), Some(now));
        assert_eq!(matching_step(RFC_SECRET, &code, now - 1, 1), Some(now));
        assert_eq!(matching_step(RFC_SECRET, &code, now + 2, 1), None);
        assert_eq!(matching_step(RFC_SECRET, &code, now + 2, 2), Some(now));
        assert_eq!(matching_step(RFC_SECRET, &code, now, 0), Some(now));
        assert_eq!(
            matching_step(RFC_SECRET, &format!(" {code} "), now, 0),
            Some(now)
        );
        for bad in ["", "12345", "1234567", "abcdef", "-12345"] {
            assert_eq!(matching_step(RFC_SECRET, bad, now, 2), None, "{bad}");
        }
    }
}
