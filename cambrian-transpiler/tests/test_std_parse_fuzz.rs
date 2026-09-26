// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Proptest for `std::str::parse_*` semantics (reference vs Rust `from_str_radix` + fit).

use proptest::prelude::*;

/// Reference unsigned parse — mirrors `_cam_try_parse_radix` (EVM / STDLIB §4.1).
fn ref_try_parse_unsigned(s: &str, radix: u64, max_bits: u16) -> Option<u128> {
    if radix > 36 && radix != 0 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut base = radix;
    if bytes.len() >= 2 && bytes[0] == b'0' && (bytes[1] == b'x' || bytes[1] == b'X') {
        if base != 0 && base != 16 {
            return None;
        }
        base = 16;
        i = 2;
    } else if base == 0 {
        base = 10;
    }
    if base < 2 || base > 36 {
        return None;
    }
    if i >= bytes.len() {
        return None;
    }
    let max_v = if max_bits >= 128 {
        u128::MAX
    } else {
        ((1u128 << max_bits) - 1)
    };
    let mut n = 0u128;
    while i < bytes.len() {
        let c = bytes[i];
        let digit = if c.is_ascii_digit() {
            let d = c - b'0';
            if d >= base as u8 {
                return None;
            }
            u128::from(d)
        } else if c.is_ascii_uppercase() {
            let d = c - 55;
            if d >= base as u8 {
                return None;
            }
            u128::from(d)
        } else if c.is_ascii_lowercase() {
            let d = c - 87;
            if d >= base as u8 {
                return None;
            }
            u128::from(d)
        } else {
            return None;
        };
        let next = n * base as u128 + digit;
        if next > max_v {
            return None;
        }
        n = next;
        i += 1;
    }
    Some(n)
}

/// Reference signed parse — mirrors `_cam_try_parse_radix_signed`.
fn ref_try_parse_signed(s: &str, radix: u64, max_bits: u16) -> Option<i128> {
    if radix > 36 && radix != 0 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut neg = false;
    let mut i = 0;
    if !bytes.is_empty() && bytes[0] == b'-' {
        neg = true;
        i = 1;
    }
    let rest = &s[i..];
    let mag = ref_try_parse_unsigned(rest, radix, max_bits)?;
    let max_pos = if max_bits >= 128 {
        i128::MAX
    } else {
        ((1i128 << (max_bits - 1)) - 1)
    };
    let max_mag = if neg { max_pos + 1 } else { max_pos };
    if mag > max_mag as u128 {
        return None;
    }
    let v = if neg {
        -(mag as i128)
    } else {
        mag as i128
    };
    if v < -(max_pos + 1) || v > max_pos {
        return None;
    }
    Some(v)
}

fn rust_parse_unsigned(s: &str, radix: u32, max_bits: u16) -> Option<u128> {
    let Ok(v) = u128::from_str_radix(s, radix) else {
        return None;
    };
    let max_v = if max_bits >= 128 {
        u128::MAX
    } else {
        (1u128 << max_bits) - 1
    };
    if v > max_v {
        None
    } else {
        Some(v)
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn fuzz_parse_u8_decimal_fit(v in 0u64..=260) {
        let s = v.to_string();
        let ok = ref_try_parse_unsigned(&s, 10, 8).is_some();
        prop_assert_eq!(ok, v <= 255);
        if v <= 255 {
            prop_assert_eq!(ref_try_parse_unsigned(&s, 10, 8), Some(v as u128));
        }
    }

    #[test]
    fn fuzz_parse_u16_decimal_fit(v in 0u64..=66000) {
        let s = v.to_string();
        let ok = ref_try_parse_unsigned(&s, 10, 16).is_some();
        prop_assert_eq!(ok, v <= 65535);
    }

    #[test]
    fn fuzz_parse_i8_decimal_fit(v in -130i32..=130) {
        let s = v.to_string();
        let ok = ref_try_parse_signed(&s, 10, 8).is_some();
        prop_assert_eq!(ok, v >= -128 && v <= 127);
    }

    #[test]
    fn fuzz_ref_matches_rust_u64_decimal(s in "[0-9]{0,20}") {
        if s.is_empty() {
            prop_assert!(ref_try_parse_unsigned(&s, 10, 64).is_none());
        } else {
            let r = ref_try_parse_unsigned(&s, 10, 64);
            let std = rust_parse_unsigned(&s, 10, 64);
            prop_assert_eq!(r, std);
        }
    }

    #[test]
    fn fuzz_radix_bounds(radix in 0u64..=40) {
        let r = ref_try_parse_unsigned("10", radix, 64);
        if radix >= 2 && radix <= 36 || radix == 0 {
            prop_assert!(r.is_some());
        } else {
            prop_assert!(r.is_none());
        }
    }

    #[test]
    fn fuzz_invalid_char_none(
        prefix in "[0-9]{0,4}",
        bad in "[^0-9]{1,2}",
        suffix in "[0-9]{0,4}"
    ) {
        let s = format!("{}{}{}", prefix, bad, suffix);
        prop_assert!(ref_try_parse_unsigned(&s, 10, 64).is_none());
        // A leading `-` is a valid signed-prefix (e.g. `-0`, `-42`), not an
        // "invalid character". Only assert signed failure when that form is
        // not a well-formed signed decimal.
        let leading_signed_ok = s.as_bytes().first() == Some(&b'-')
            && s.len() > 1
            && s.as_bytes()[1..].iter().all(|c| c.is_ascii_digit());
        if !leading_signed_ok {
            prop_assert!(ref_try_parse_signed(&s, 10, 64).is_none());
        }
    }
}

#[test]
fn ref_parse_width_smoke() {
    assert_eq!(ref_try_parse_unsigned("255", 10, 8), Some(255));
    assert_eq!(ref_try_parse_unsigned("256", 10, 8), None);
    assert_eq!(ref_try_parse_signed("-128", 10, 8), Some(-128));
    assert_eq!(ref_try_parse_signed("-129", 10, 8), None);
    assert_eq!(ref_try_parse_unsigned("ff", 16, 8), Some(255));
    assert_eq!(ref_try_parse_unsigned("100", 16, 8), None);
}
