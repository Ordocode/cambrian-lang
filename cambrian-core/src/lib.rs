// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Cambrian Core — language-level types shared across all platform runtimes.
//!
//! Provides:
//! - U256: 256-bit unsigned integer with full arithmetic
//! - Checked arithmetic (cam_add, cam_sub, cam_mul)
//! - CamCast: uniform numeric type casting
//! - Functional HashMap/Vec extensions
//! - Math utilities (muldiv, divmod, min, max, etc.)

pub use std::collections::HashMap;
pub use std::hash::Hash;

// ===========================================================================
// U256 — 256-bit unsigned integer
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct U256 {
    pub hi: u128,
    pub lo: u128,
}

#[allow(non_camel_case_types)]
pub type uint256 = U256;

/// Error parsing a Cambrian integer literal into [`U256`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum U256ParseError {
    Empty,
    InvalidDigit,
    Overflow,
    TooManyDigits,
}

impl std::fmt::Display for U256ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            U256ParseError::Empty => write!(f, "empty integer literal"),
            U256ParseError::InvalidDigit => write!(f, "invalid digit in integer literal"),
            U256ParseError::Overflow => write!(f, "integer literal exceeds U256::MAX"),
            U256ParseError::TooManyDigits => write!(f, "integer literal has too many digits"),
        }
    }
}

impl std::error::Error for U256ParseError {}

impl U256 {
    pub const ZERO: U256 = U256 { hi: 0, lo: 0 };
    pub const ONE: U256 = U256 { hi: 0, lo: 1 };
    pub const MAX: U256 = U256 { hi: u128::MAX, lo: u128::MAX };

    pub const fn new(hi: u128, lo: u128) -> Self { U256 { hi, lo } }
    pub const fn from_u128(v: u128) -> Self { U256 { hi: 0, lo: v } }
    pub const fn from_u64(v: u64) -> Self { U256 { hi: 0, lo: v as u128 } }

    pub fn from_be_bytes(b: &[u8; 32]) -> Self {
        let mut hi_bytes = [0u8; 16];
        let mut lo_bytes = [0u8; 16];
        hi_bytes.copy_from_slice(&b[0..16]);
        lo_bytes.copy_from_slice(&b[16..32]);
        U256 { hi: u128::from_be_bytes(hi_bytes), lo: u128::from_be_bytes(lo_bytes) }
    }

    pub fn to_be_bytes(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[0..16].copy_from_slice(&self.hi.to_be_bytes());
        out[16..32].copy_from_slice(&self.lo.to_be_bytes());
        out
    }

    pub fn is_zero(&self) -> bool { self.hi == 0 && self.lo == 0 }

    /// True when the value fits in `u128` (`hi == 0`).
    pub fn fits_u128(&self) -> bool { self.hi == 0 }

    /// Decimal string for bounds JSON and diagnostics (always base-10, never hex).
    pub fn to_display_decimal(&self) -> String {
        if self.hi == 0 {
            return self.lo.to_string();
        }
        let mut n = *self;
        let ten = U256::from_u128(10);
        let mut digits: Vec<u8> = Vec::new();
        while !n.is_zero() {
            let (q, r) = u256_div_rem(n, ten);
            digits.push(b'0' + (r.lo as u8));
            n = q;
        }
        digits.reverse();
        String::from_utf8(digits).expect("decimal digits are ASCII")
    }

    /// Parse a decimal integer literal (`_` separators allowed).
    pub fn from_decimal_str(s: &str) -> Result<Self, U256ParseError> {
        let s = s.replace('_', "");
        if s.is_empty() {
            return Err(U256ParseError::Empty);
        }
        let mut result = U256::ZERO;
        const TEN: U256 = U256::from_u128(10);
        let max_quotient = U256::MAX / TEN;
        let max_remainder = (U256::MAX % TEN).lo as u32;
        for ch in s.chars() {
            let digit = ch.to_digit(10).ok_or(U256ParseError::InvalidDigit)?;
            if result > max_quotient
                || (result == max_quotient && digit > max_remainder)
            {
                return Err(U256ParseError::Overflow);
            }
            result = result * TEN + U256::from_u64(digit as u64);
        }
        Ok(result)
    }

    /// Parse 1–64 hex digits (no `0x` prefix; `_` allowed).
    pub fn from_hex_digits(s: &str) -> Result<Self, U256ParseError> {
        let s = s.replace('_', "");
        if s.is_empty() {
            return Err(U256ParseError::Empty);
        }
        if s.len() > 64 {
            return Err(U256ParseError::TooManyDigits);
        }
        if !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(U256ParseError::InvalidDigit);
        }
        let padded = format!("{:0>64}", s);
        let hi = u128::from_str_radix(&padded[..32], 16)
            .map_err(|_| U256ParseError::InvalidDigit)?;
        let lo = u128::from_str_radix(&padded[32..], 16)
            .map_err(|_| U256ParseError::InvalidDigit)?;
        Ok(U256::new(hi, lo))
    }

    /// Parse 1–256 binary digits (optional `0b` prefix; `_` allowed).
    pub fn from_binary_digits(s: &str) -> Result<Self, U256ParseError> {
        let body = if let Some(rest) = s.strip_prefix("0b") {
            rest
        } else {
            s
        };
        let body = body.replace('_', "");
        if body.is_empty() {
            return Err(U256ParseError::Empty);
        }
        if body.len() > 256 {
            return Err(U256ParseError::TooManyDigits);
        }
        let mut result = U256::ZERO;
        for ch in body.chars() {
            result = result << 1;
            match ch {
                '0' => {}
                '1' => {
                    result = result + U256::ONE;
                }
                _ => return Err(U256ParseError::InvalidDigit),
            }
        }
        Ok(result)
    }

    pub fn to_u128(&self) -> u128 {
        assert!(self.hi == 0, "U256 too large for u128");
        self.lo
    }

    pub fn to_u64(&self) -> u64 {
        assert!(self.hi == 0 && self.lo <= u64::MAX as u128, "U256 too large for u64");
        self.lo as u64
    }

    pub fn leading_zeros(&self) -> u32 {
        if self.hi == 0 { 128 + self.lo.leading_zeros() } else { self.hi.leading_zeros() }
    }

    pub fn wrapping_add(self, rhs: Self) -> Self {
        let (lo, carry) = self.lo.overflowing_add(rhs.lo);
        let hi = self.hi.wrapping_add(rhs.hi).wrapping_add(carry as u128);
        U256 { hi, lo }
    }

    pub fn wrapping_sub(self, rhs: Self) -> Self {
        let (lo, borrow) = self.lo.overflowing_sub(rhs.lo);
        let hi = self.hi.wrapping_sub(rhs.hi).wrapping_sub(borrow as u128);
        U256 { hi, lo }
    }

    pub fn bitwise_not(self) -> Self {
        U256 { hi: !self.hi, lo: !self.lo }
    }

    pub fn wrapping_mul(self, rhs: Self) -> Self {
        let (res_hi, res_lo) = u256_widening_mul_u128(self.lo, rhs.lo);
        let cross1 = self.hi.wrapping_mul(rhs.lo);
        let cross2 = self.lo.wrapping_mul(rhs.hi);
        let hi = res_hi.wrapping_add(cross1).wrapping_add(cross2);
        U256 { hi, lo: res_lo }
    }
}

impl From<u8> for U256 {
    fn from(v: u8) -> Self { U256::from_u64(v as u64) }
}
impl From<u16> for U256 {
    fn from(v: u16) -> Self { U256::from_u64(v as u64) }
}
impl From<u32> for U256 {
    fn from(v: u32) -> Self { U256::from_u64(v as u64) }
}
impl From<u64> for U256 {
    fn from(v: u64) -> Self { U256::from_u64(v) }
}
impl From<u128> for U256 {
    fn from(v: u128) -> Self { U256::from_u128(v) }
}
impl From<i32> for U256 {
    fn from(v: i32) -> Self {
        assert!(v >= 0, "cannot convert negative i32 to U256");
        U256::from_u64(v as u64)
    }
}

impl std::fmt::Display for U256 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.hi == 0 { write!(f, "{}", self.lo) }
        else { write!(f, "0x{:032x}{:032x}", self.hi, self.lo) }
    }
}

// --- U256 arithmetic operators ---

impl std::ops::Add for U256 {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        let (lo, carry) = self.lo.overflowing_add(rhs.lo);
        let hi = self.hi.checked_add(rhs.hi).expect("U256 add overflow")
            .checked_add(carry as u128).expect("U256 add overflow");
        U256 { hi, lo }
    }
}

impl std::ops::Sub for U256 {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        let (lo, borrow) = self.lo.overflowing_sub(rhs.lo);
        let hi = self.hi.checked_sub(rhs.hi).expect("U256 sub underflow")
            .checked_sub(borrow as u128).expect("U256 sub underflow");
        U256 { hi, lo }
    }
}

impl std::ops::Mul for U256 {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        let (res_hi, res_lo) = u256_widening_mul_u128(self.lo, rhs.lo);
        let cross1 = self.hi.checked_mul(rhs.lo).expect("U256 mul overflow");
        let cross2 = self.lo.checked_mul(rhs.hi).expect("U256 mul overflow");
        assert!(res_hi.checked_add(cross1).and_then(|v| v.checked_add(cross2)).is_some(), "U256 mul overflow");
        let hi = res_hi + cross1 + cross2;
        if self.hi != 0 && rhs.hi != 0 { panic!("U256 mul overflow"); }
        U256 { hi, lo: res_lo }
    }
}

impl std::ops::Div for U256 {
    type Output = Self;
    fn div(self, rhs: Self) -> Self {
        u256_div_rem(self, rhs).0
    }
}

impl std::ops::Rem for U256 {
    type Output = Self;
    fn rem(self, rhs: Self) -> Self {
        u256_div_rem(self, rhs).1
    }
}

pub fn u256_div_rem(n: U256, d: U256) -> (U256, U256) {
    assert!(!d.is_zero(), "U256 division by zero");
    if n < d { return (U256::ZERO, n); }
    if d.hi == 0 && n.hi == 0 { return (U256::from_u128(n.lo / d.lo), U256::from_u128(n.lo % d.lo)); }

    let shift = n.leading_zeros().abs_diff(d.leading_zeros());
    let mut rem = n;
    let mut quot = U256::ZERO;
    let mut divisor = u256_shl(d, shift);

    for i in (0..=shift).rev() {
        if rem >= divisor {
            rem = rem - divisor;
            quot = quot + u256_shl(U256::ONE, i);
        }
        divisor = u256_shr(divisor, 1);
    }
    (quot, rem)
}

pub fn u256_shl(v: U256, shift: u32) -> U256 {
    if shift == 0 { return v; }
    if shift >= 256 { return U256::ZERO; }
    if shift >= 128 {
        U256 { hi: v.lo << (shift - 128), lo: 0 }
    } else {
        U256 { hi: (v.hi << shift) | (v.lo >> (128 - shift)), lo: v.lo << shift }
    }
}

pub fn u256_shr(v: U256, shift: u32) -> U256 {
    if shift == 0 { return v; }
    if shift >= 256 { return U256::ZERO; }
    if shift >= 128 {
        U256 { hi: 0, lo: v.hi >> (shift - 128) }
    } else {
        U256 { hi: v.hi >> shift, lo: (v.lo >> shift) | (v.hi << (128 - shift)) }
    }
}

impl std::ops::BitAnd for U256 {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self { U256 { hi: self.hi & rhs.hi, lo: self.lo & rhs.lo } }
}
impl std::ops::BitOr for U256 {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self { U256 { hi: self.hi | rhs.hi, lo: self.lo | rhs.lo } }
}
impl std::ops::BitXor for U256 {
    type Output = Self;
    fn bitxor(self, rhs: Self) -> Self { U256 { hi: self.hi ^ rhs.hi, lo: self.lo ^ rhs.lo } }
}
impl std::ops::Not for U256 {
    type Output = Self;
    fn not(self) -> Self { self.bitwise_not() }
}
impl std::ops::Shl<u32> for U256 {
    type Output = Self;
    fn shl(self, shift: u32) -> Self { u256_shl(self, shift) }
}
impl std::ops::Shr<u32> for U256 {
    type Output = Self;
    fn shr(self, shift: u32) -> Self { u256_shr(self, shift) }
}
impl std::ops::Shl<U256> for U256 {
    type Output = Self;
    fn shl(self, shift: U256) -> Self {
        if shift.hi != 0 || shift.lo >= 256 { return U256::ZERO; }
        u256_shl(self, shift.lo as u32)
    }
}
impl std::ops::Shr<U256> for U256 {
    type Output = Self;
    fn shr(self, shift: U256) -> Self {
        if shift.hi != 0 || shift.lo >= 256 { return U256::ZERO; }
        u256_shr(self, shift.lo as u32)
    }
}

// Cross-type bitwise and shift ops for U256
macro_rules! impl_u256_cross_bitops {
    ($($t:ty),*) => { $(
        impl std::ops::BitAnd<$t> for U256 {
            type Output = U256;
            fn bitand(self, rhs: $t) -> U256 { self & U256::from(rhs as u64) }
        }
        impl std::ops::BitOr<$t> for U256 {
            type Output = U256;
            fn bitor(self, rhs: $t) -> U256 { self | U256::from(rhs as u64) }
        }
        impl std::ops::BitXor<$t> for U256 {
            type Output = U256;
            fn bitxor(self, rhs: $t) -> U256 { self ^ U256::from(rhs as u64) }
        }
        impl std::ops::Shl<$t> for U256 {
            type Output = U256;
            fn shl(self, rhs: $t) -> U256 {
                if rhs as u64 >= 256 { return U256::ZERO; }
                u256_shl(self, rhs as u32)
            }
        }
        impl std::ops::Shr<$t> for U256 {
            type Output = U256;
            fn shr(self, rhs: $t) -> U256 {
                if rhs as u64 >= 256 { return U256::ZERO; }
                u256_shr(self, rhs as u32)
            }
        }
    )* }
}
impl_u256_cross_bitops!(u8, u16, u64);

impl std::ops::BitAnd<u32> for U256 {
    type Output = U256;
    fn bitand(self, rhs: u32) -> U256 { self & U256::from(rhs as u64) }
}
impl std::ops::BitOr<u32> for U256 {
    type Output = U256;
    fn bitor(self, rhs: u32) -> U256 { self | U256::from(rhs as u64) }
}
impl std::ops::BitXor<u32> for U256 {
    type Output = U256;
    fn bitxor(self, rhs: u32) -> U256 { self ^ U256::from(rhs as u64) }
}

impl std::ops::Shl<i32> for U256 {
    type Output = U256;
    fn shl(self, rhs: i32) -> U256 {
        assert!(rhs >= 0, "cannot shift U256 by negative amount");
        u256_shl(self, rhs as u32)
    }
}
impl std::ops::Shr<i32> for U256 {
    type Output = U256;
    fn shr(self, rhs: i32) -> U256 {
        assert!(rhs >= 0, "cannot shift U256 by negative amount");
        u256_shr(self, rhs as u32)
    }
}
impl std::ops::BitAnd<i32> for U256 {
    type Output = U256;
    fn bitand(self, rhs: i32) -> U256 { self & U256::from(rhs) }
}
impl std::ops::BitOr<i32> for U256 {
    type Output = U256;
    fn bitor(self, rhs: i32) -> U256 { self | U256::from(rhs) }
}
impl std::ops::BitXor<i32> for U256 {
    type Output = U256;
    fn bitxor(self, rhs: i32) -> U256 { self ^ U256::from(rhs) }
}

macro_rules! impl_prim_shift_u256 {
    ($($t:ty),*) => { $(
        impl std::ops::Shl<U256> for $t {
            type Output = U256;
            fn shl(self, rhs: U256) -> U256 { U256::from(self as u64) << rhs }
        }
        impl std::ops::Shr<U256> for $t {
            type Output = U256;
            fn shr(self, rhs: U256) -> U256 { U256::from(self as u64) >> rhs }
        }
    )* }
}
impl_prim_shift_u256!(u8, u16, u32, u64);

// Cross-type arithmetic for U256
macro_rules! impl_u256_cross_ops {
    ($($t:ty),*) => { $(
        impl std::ops::Add<$t> for U256 {
            type Output = U256;
            fn add(self, rhs: $t) -> U256 { self + U256::from(rhs) }
        }
        impl std::ops::Sub<$t> for U256 {
            type Output = U256;
            fn sub(self, rhs: $t) -> U256 { self - U256::from(rhs) }
        }
        impl std::ops::Mul<$t> for U256 {
            type Output = U256;
            fn mul(self, rhs: $t) -> U256 { self * U256::from(rhs) }
        }
        impl std::ops::Div<$t> for U256 {
            type Output = U256;
            fn div(self, rhs: $t) -> U256 { self / U256::from(rhs) }
        }
        impl std::ops::Rem<$t> for U256 {
            type Output = U256;
            fn rem(self, rhs: $t) -> U256 { self % U256::from(rhs) }
        }
        impl PartialEq<$t> for U256 {
            fn eq(&self, other: &$t) -> bool { *self == U256::from(*other) }
        }
    )* }
}
impl_u256_cross_ops!(u8, u16, u32, u64);

impl std::ops::Add<i32> for U256 {
    type Output = U256;
    fn add(self, rhs: i32) -> U256 { self + U256::from(rhs) }
}
impl std::ops::Sub<i32> for U256 {
    type Output = U256;
    fn sub(self, rhs: i32) -> U256 { self - U256::from(rhs) }
}
impl std::ops::Mul<i32> for U256 {
    type Output = U256;
    fn mul(self, rhs: i32) -> U256 { self * U256::from(rhs) }
}
impl std::ops::Div<i32> for U256 {
    type Output = U256;
    fn div(self, rhs: i32) -> U256 { self / U256::from(rhs) }
}
impl std::ops::Rem<i32> for U256 {
    type Output = U256;
    fn rem(self, rhs: i32) -> U256 { self % U256::from(rhs) }
}
impl PartialEq<i32> for U256 {
    fn eq(&self, other: &i32) -> bool {
        if *other < 0 { return false; }
        *self == U256::from_u64(*other as u64)
    }
}

impl std::ops::Add<u128> for U256 {
    type Output = U256;
    fn add(self, rhs: u128) -> U256 { self + U256::from_u128(rhs) }
}
impl std::ops::Sub<u128> for U256 {
    type Output = U256;
    fn sub(self, rhs: u128) -> U256 { self - U256::from_u128(rhs) }
}
impl std::ops::Mul<u128> for U256 {
    type Output = U256;
    fn mul(self, rhs: u128) -> U256 { self * U256::from_u128(rhs) }
}
impl std::ops::Div<u128> for U256 {
    type Output = U256;
    fn div(self, rhs: u128) -> U256 { self / U256::from_u128(rhs) }
}
impl std::ops::Rem<u128> for U256 {
    type Output = U256;
    fn rem(self, rhs: u128) -> U256 { self % U256::from_u128(rhs) }
}
impl PartialEq<u128> for U256 {
    fn eq(&self, other: &u128) -> bool { *self == U256::from_u128(*other) }
}

// ===========================================================================
// Widening multiplication helpers (used by U256 Mul and MulDiv)
// ===========================================================================

pub fn u256_widening_mul_u128(a: u128, b: u128) -> (u128, u128) {
    let a_lo = a as u64 as u128;
    let a_hi = a >> 64;
    let b_lo = b as u64 as u128;
    let b_hi = b >> 64;
    let ll = a_lo * b_lo;
    let lh = a_lo * b_hi;
    let hl = a_hi * b_lo;
    let hh = a_hi * b_hi;
    let mid = lh + (ll >> 64);
    let mid2 = (mid as u64 as u128) + hl;
    let lo = ((mid2 as u64 as u128) << 64) | (ll as u64 as u128);
    let hi = hh + (mid >> 64) + (mid2 >> 64);
    (hi, lo)
}

// ===========================================================================
// Checked arithmetic
// ===========================================================================

pub trait CheckedOps: Sized {
    fn cam_checked_add(self, rhs: Self) -> Self;
    fn cam_checked_sub(self, rhs: Self) -> Self;
    fn cam_checked_mul(self, rhs: Self) -> Self;
}

macro_rules! impl_checked_ops {
    ($($t:ty),*) => { $(
        impl CheckedOps for $t {
            fn cam_checked_add(self, rhs: Self) -> Self { self.checked_add(rhs).expect("arithmetic overflow on add") }
            fn cam_checked_sub(self, rhs: Self) -> Self { self.checked_sub(rhs).expect("arithmetic overflow on sub") }
            fn cam_checked_mul(self, rhs: Self) -> Self { self.checked_mul(rhs).expect("arithmetic overflow on mul") }
        }
    )* }
}
impl_checked_ops!(u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize);

impl CheckedOps for U256 {
    fn cam_checked_add(self, rhs: Self) -> Self { self + rhs }
    fn cam_checked_sub(self, rhs: Self) -> Self { self - rhs }
    fn cam_checked_mul(self, rhs: Self) -> Self { self * rhs }
}

pub fn cam_add<T: std::ops::Add<Output = T> + CheckedOps>(a: T, b: T) -> T { a.cam_checked_add(b) }
pub fn cam_sub<T: std::ops::Sub<Output = T> + CheckedOps>(a: T, b: T) -> T { a.cam_checked_sub(b) }
pub fn cam_mul<T: std::ops::Mul<Output = T> + CheckedOps>(a: T, b: T) -> T { a.cam_checked_mul(b) }

// ===========================================================================
// CamCast: uniform numeric type casting
// ===========================================================================

pub trait CamCast<T> { fn cam_cast(self) -> T; }

macro_rules! impl_cam_cast_prim_checked {
    ($($from:ty => [$($to:ty),*]),* $(,)?) => { $($(
        impl CamCast<$to> for $from {
            #[inline] fn cam_cast(self) -> $to {
                <$to>::try_from(self).expect(concat!(
                    "CamCast: ", stringify!($from), " value out of range for ", stringify!($to)
                ))
            }
        }
    )*)* }
}

impl_cam_cast_prim_checked! {
    u8   => [u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize],
    u16  => [u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize],
    u32  => [u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize],
    u64  => [u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize],
    u128 => [u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize],
    usize => [u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize],
    i8   => [u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize],
    i16  => [u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize],
    i32  => [u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize],
    i64  => [u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize],
    i128 => [u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize],
    isize => [u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize]
}

macro_rules! impl_cam_cast_bool {
    ($($to:ty),*) => { $(
        impl CamCast<$to> for bool {
            #[inline] fn cam_cast(self) -> $to { self as $to }
        }
    )* }
}
impl_cam_cast_bool!(u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize);

macro_rules! impl_cam_cast_u256_unsigned {
    ($($prim:ty),*) => { $(
        impl CamCast<$prim> for U256 {
            #[inline] fn cam_cast(self) -> $prim {
                let v = self.to_u128();
                <$prim>::try_from(v).expect(concat!("U256 value too large for ", stringify!($prim)))
            }
        }
        impl CamCast<U256> for $prim {
            #[inline] fn cam_cast(self) -> U256 { U256::from_u128(self as u128) }
        }
    )* }
}
impl_cam_cast_u256_unsigned!(u8, u16, u32, u64, u128, usize);

macro_rules! impl_cam_cast_u256_signed {
    ($($prim:ty),*) => { $(
        impl CamCast<$prim> for U256 {
            #[inline] fn cam_cast(self) -> $prim {
                let v = self.to_u128();
                <$prim>::try_from(v).expect(concat!("U256 value too large for ", stringify!($prim)))
            }
        }
        impl CamCast<U256> for $prim {
            #[inline] fn cam_cast(self) -> U256 {
                assert!(self >= 0, concat!("cannot cast negative ", stringify!($prim), " to U256"));
                U256::from_u128(self as u128)
            }
        }
    )* }
}
impl_cam_cast_u256_signed!(i8, i16, i32, i64, i128, isize);

impl CamCast<U256> for U256 {
    #[inline] fn cam_cast(self) -> U256 { self }
}

// ===========================================================================
// Functional HashMap extensions
// ===========================================================================

pub trait CambrianMapExt<K, V> {
    fn exists(&self, key: K) -> bool;
    fn cam_insert(self, key: K, value: V) -> Self;
    fn cam_remove(self, key: K) -> Self;
    fn cam_update(self, key: K, value: V) -> Self;
    fn cam_keys(&self) -> Vec<K>;
    fn cam_contains(&self, key: K) -> bool;
    fn cam_get(&self, key: K) -> V;
    fn cam_iter(self) -> std::collections::hash_map::IntoIter<K, V>;
}

impl<K: Eq + Hash + Clone, V: Clone + Default> CambrianMapExt<K, V> for HashMap<K, V> {
    fn exists(&self, key: K) -> bool { self.contains_key(&key) }
    fn cam_insert(mut self, key: K, value: V) -> Self { self.insert(key, value); self }
    fn cam_remove(mut self, key: K) -> Self { self.remove(&key); self }
    fn cam_update(mut self, key: K, value: V) -> Self { self.insert(key, value); self }
    fn cam_keys(&self) -> Vec<K> { self.keys().cloned().collect() }
    fn cam_contains(&self, key: K) -> bool { self.contains_key(&key) }
    fn cam_get(&self, key: K) -> V { self.get(&key).cloned().unwrap_or_default() }
    fn cam_iter(self) -> std::collections::hash_map::IntoIter<K, V> { self.into_iter() }
}

pub trait CamVecExt<T> {
    fn cam_contains(&self, item: T) -> bool;
}

impl<T: PartialEq> CamVecExt<T> for Vec<T> {
    fn cam_contains(&self, item: T) -> bool { self.contains(&item) }
}

// Backward-compat aliases
pub use CambrianMapExt as CamHashMapExt;

// ===========================================================================
// Owned-value iteration
// ===========================================================================

pub trait CambrianIter {
    type Item;
    fn cam_iter(&self) -> std::vec::IntoIter<Self::Item>;
}

impl<K: Clone + Eq + Hash, V: Clone> CambrianIter for HashMap<K, V> {
    type Item = (K, V);
    fn cam_iter(&self) -> std::vec::IntoIter<(K, V)> {
        self.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<Vec<_>>().into_iter()
    }
}

impl<T: Clone> CambrianIter for Vec<T> {
    type Item = T;
    fn cam_iter(&self) -> std::vec::IntoIter<T> {
        self.iter().cloned().collect::<Vec<_>>().into_iter()
    }
}

// ===========================================================================
// Math utilities
// ===========================================================================

pub fn min<T: Ord>(a: T, b: T) -> T { if a <= b { a } else { b } }
pub fn max<T: Ord>(a: T, b: T) -> T { if a >= b { a } else { b } }
pub fn abs<T: Ord + Default + std::ops::Neg<Output = T> + Copy>(x: T) -> T {
    if x >= T::default() { x } else { -x }
}
pub fn clamp<T: Ord>(x: T, lo: T, hi: T) -> T {
    if x < lo { lo } else if x > hi { hi } else { x }
}

pub fn cam_sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Sha256, Digest};
    let result = Sha256::digest(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

// ===========================================================================
// CamDiv / CamMulDiv — wide multiplication and division
// ===========================================================================

pub trait CamDiv: Sized + Copy + PartialEq + std::ops::Div<Output = Self> + std::ops::Rem<Output = Self> + std::ops::Sub<Output = Self> + std::ops::Add<Output = Self> {
    fn zero() -> Self;
    fn one() -> Self;
    fn two() -> Self;
}

macro_rules! impl_cam_div {
    ($($t:ty),*) => { $(
        impl CamDiv for $t {
            fn zero() -> Self { 0 }
            fn one() -> Self { 1 }
            fn two() -> Self { 2 }
        }
    )* }
}
impl_cam_div!(u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize);

impl CamDiv for U256 {
    fn zero() -> Self { U256::ZERO }
    fn one() -> Self { U256::ONE }
    fn two() -> Self { U256 { hi: 0, lo: 2 } }
}

pub trait CamMulDiv: CamDiv {
    fn wide_muldiv(self, y: Self, z: Self) -> Self;
    fn wide_muldivmod(self, y: Self, z: Self) -> (Self, Self);
}

macro_rules! impl_muldiv_via {
    ($t:ty, $wide:ty) => {
        impl CamMulDiv for $t {
            fn wide_muldiv(self, y: Self, z: Self) -> Self {
                ((self as $wide).checked_mul(y as $wide).expect("muldiv overflow") / z as $wide) as $t
            }
            fn wide_muldivmod(self, y: Self, z: Self) -> (Self, Self) {
                let w = (self as $wide).checked_mul(y as $wide).expect("muldiv overflow");
                ((w / z as $wide) as $t, (w % z as $wide) as $t)
            }
        }
    }
}

impl_muldiv_via!(u8, u16);
impl_muldiv_via!(u16, u32);
impl_muldiv_via!(u32, u64);
impl_muldiv_via!(u64, u128);
impl_muldiv_via!(i8, i16);
impl_muldiv_via!(i16, i32);
impl_muldiv_via!(i32, i64);
impl_muldiv_via!(i64, i128);

impl CamMulDiv for u128 {
    fn wide_muldiv(self, y: Self, z: Self) -> Self {
        if self == 0 || y == 0 { return Self::zero(); }
        u128_wide_muldiv(self, y, z)
    }
    fn wide_muldivmod(self, y: Self, z: Self) -> (Self, Self) {
        if self == 0 || y == 0 { return (0, 0); }
        let q = u128_wide_muldiv(self, y, z);
        let product = u256_widening_mul_u128(self, y);
        let (_, rem) = u256_div_rem(U256 { hi: product.0, lo: product.1 }, U256::from_u128(z));
        (q, rem.to_u128())
    }
}

fn u128_to_i128_checked(val: u128, negative: bool) -> i128 {
    if negative {
        if val == (i128::MAX as u128) + 1 { i128::MIN }
        else if val > (i128::MAX as u128) + 1 { panic!("muldiv result overflow for i128") }
        else { -(val as i128) }
    } else {
        assert!(val <= i128::MAX as u128, "muldiv result overflow for i128");
        val as i128
    }
}

impl CamMulDiv for i128 {
    fn wide_muldiv(self, y: Self, z: Self) -> Self {
        if self == 0 || y == 0 { return Self::zero(); }
        let sign = (self < 0) ^ (y < 0) ^ (z < 0);
        let r = cam_muldiv(self.unsigned_abs(), y.unsigned_abs(), z.unsigned_abs());
        u128_to_i128_checked(r, sign)
    }
    fn wide_muldivmod(self, y: Self, z: Self) -> (Self, Self) {
        if self == 0 || y == 0 { return (0, 0); }
        let (qu, ru) = cam_muldivmod(self.unsigned_abs(), y.unsigned_abs(), z.unsigned_abs());
        let q_sign = (self < 0) ^ (y < 0) ^ (z < 0);
        let r_sign = (self < 0) ^ (y < 0);
        (u128_to_i128_checked(qu, q_sign), u128_to_i128_checked(ru, r_sign))
    }
}

impl CamMulDiv for isize {
    fn wide_muldiv(self, y: Self, z: Self) -> Self {
        cam_muldiv(self as i64, y as i64, z as i64) as isize
    }
    fn wide_muldivmod(self, y: Self, z: Self) -> (Self, Self) {
        let (q, r) = cam_muldivmod(self as i64, y as i64, z as i64);
        (q as isize, r as isize)
    }
}

impl CamMulDiv for usize {
    fn wide_muldiv(self, y: Self, z: Self) -> Self {
        cam_muldiv(self as u64, y as u64, z as u64) as usize
    }
    fn wide_muldivmod(self, y: Self, z: Self) -> (Self, Self) {
        let (q, r) = cam_muldivmod(self as u64, y as u64, z as u64);
        (q as usize, r as usize)
    }
}

impl CamMulDiv for U256 {
    fn wide_muldiv(self, y: Self, z: Self) -> Self {
        if self.is_zero() || y.is_zero() { return U256::ZERO; }
        let product = u512_mul(self, y);
        u512_div_by_u256(product, z)
    }
    fn wide_muldivmod(self, y: Self, z: Self) -> (Self, Self) {
        if self.is_zero() || y.is_zero() { return (U256::ZERO, U256::ZERO); }
        let product = u512_mul(self, y);
        let q = u512_div_by_u256(product, z);
        let qz = u512_mul(q, z);
        let r = u512_sub(product, qz);
        (q, r.1)
    }
}

// --- 512-bit helpers ---

fn u512_mul(a: U256, b: U256) -> (U256, U256) {
    let (ll_hi, ll_lo) = u256_widening_mul_u128(a.lo, b.lo);
    let (lh_hi, lh_lo) = u256_widening_mul_u128(a.lo, b.hi);
    let (hl_hi, hl_lo) = u256_widening_mul_u128(a.hi, b.lo);
    let (hh_hi, hh_lo) = u256_widening_mul_u128(a.hi, b.hi);

    let p0 = ll_lo;
    let (p1, carry1) = ll_hi.overflowing_add(lh_lo);
    let (p1, carry1b) = p1.overflowing_add(hl_lo);
    let c1 = carry1 as u128 + carry1b as u128;
    let (p2, carry2) = lh_hi.overflowing_add(hl_hi);
    let (p2, carry2b) = p2.overflowing_add(hh_lo);
    let (p2, carry2c) = p2.overflowing_add(c1);
    let c2 = carry2 as u128 + carry2b as u128 + carry2c as u128;
    let p3 = hh_hi + c2;

    (U256 { hi: p3, lo: p2 }, U256 { hi: p1, lo: p0 })
}

fn u512_div_by_u256(n: (U256, U256), d: U256) -> U256 {
    let (n_hi, n_lo) = n;
    if n_hi.is_zero() {
        return u256_div_rem(n_lo, d).0;
    }
    let n_lz = n_hi.leading_zeros();
    let d_lz = d.leading_zeros();
    if n_lz > d_lz + 256 { return U256::ZERO; }
    let shift = (256 + d_lz).saturating_sub(n_lz);

    let mut rem_hi = n_hi;
    let mut rem_lo = n_lo;
    let mut quot = U256::ZERO;

    for i in (0..=shift).rev() {
        let (dsh_hi, dsh_lo) = u512_shl_u256(d, i);
        if u512_gte((rem_hi, rem_lo), (dsh_hi, dsh_lo)) {
            let (rh, rl) = u512_sub((rem_hi, rem_lo), (dsh_hi, dsh_lo));
            rem_hi = rh;
            rem_lo = rl;
            assert!(i < 256, "wide_muldiv result exceeds U256::MAX");
            quot = quot + u256_shl(U256::ONE, i);
        }
    }
    quot
}

fn u512_shl_u256(v: U256, shift: u32) -> (U256, U256) {
    if shift == 0 { return (U256::ZERO, v); }
    if shift >= 512 { return (U256::ZERO, U256::ZERO); }
    if shift >= 256 {
        (u256_shl(v, shift - 256), U256::ZERO)
    } else {
        let lo = u256_shl(v, shift);
        let hi = if shift == 0 { U256::ZERO } else { u256_shr(v, 256 - shift) };
        (hi, lo)
    }
}

fn u512_gte(a: (U256, U256), b: (U256, U256)) -> bool {
    a.0 > b.0 || (a.0 == b.0 && a.1 >= b.1)
}

fn u512_sub(a: (U256, U256), b: (U256, U256)) -> (U256, U256) {
    let borrow = a.1 < b.1;
    let lo = if borrow {
        U256::MAX - b.1 + a.1 + U256::ONE
    } else {
        a.1 - b.1
    };
    let hi = a.0 - b.0 - U256::from_u128(borrow as u128);
    (hi, lo)
}

fn u128_wide_muldiv(x: u128, y: u128, z: u128) -> u128 {
    let (hi, lo) = u256_widening_mul_u128(x, y);
    let result = u256_div_rem(U256 { hi, lo }, U256::from_u128(z)).0;
    result.to_u128()
}

pub fn cam_muldiv<T: CamMulDiv>(x: T, y: T, z: T) -> T { x.wide_muldiv(y, z) }
pub fn cam_muldivmod<T: CamMulDiv>(x: T, y: T, z: T) -> (T, T) { x.wide_muldivmod(y, z) }

pub fn cam_divmod<T: CamDiv>(x: T, y: T) -> (T, T) { (x / y, x % y) }
pub fn cam_divc<T: CamDiv + PartialOrd>(x: T, y: T) -> T {
    assert!(x >= T::zero() && y > T::zero(), "cam_divc requires non-negative x and positive y");
    if x == T::zero() { T::zero() } else { (x - T::one()) / y + T::one() }
}
pub fn cam_divr<T: CamDiv + PartialOrd>(x: T, y: T) -> T {
    assert!(x >= T::zero() && y > T::zero(), "cam_divr requires non-negative x and positive y");
    let q = x / y;
    let r = x % y;
    let half_ceil = y - y / T::two();
    if r >= half_ceil { q + T::one() } else { q }
}

pub fn cam_sign<T: PartialOrd + Default>(x: T) -> i8 {
    let zero = T::default();
    if x > zero { 1 } else if x < zero { -1 } else { 0 }
}

pub fn cam_minmax<T: Ord>(a: T, b: T) -> (T, T) {
    if a <= b { (a, b) } else { (b, a) }
}

pub fn cam_modpow2(x: u128, n: u32) -> u128 {
    if n >= 128 { x } else { x & ((1u128 << n) - 1) }
}

pub fn cam_pow<T: CamDiv + std::ops::Mul<Output = T>>(base: T, exp: T) -> T {
    let mut result = T::one();
    let mut b = base;
    let mut e = exp;
    while e != T::zero() {
        if e % T::two() != T::zero() {
            result = result * b;
        }
        e = e / T::two();
        if e != T::zero() {
            b = b * b;
        }
    }
    result
}

// ===========================================================================
// Big-endian serialization traits (BeSerialize / BeDeserialize)
// ===========================================================================

pub trait BeSerialize {
    fn ser_be(&self) -> Vec<u8>;
}
pub trait BeDeserialize: Sized {
    fn try_de_be(data: &[u8], off: &mut usize) -> Option<Self>;

    fn de_be(data: &[u8], off: &mut usize) -> Self {
        Self::try_de_be(data, off).expect("BeDeserialize: insufficient data")
    }
}

macro_rules! impl_be_uint {
    ($t:ty, $sz:expr) => {
        impl BeSerialize for $t {
            fn ser_be(&self) -> Vec<u8> { <$t>::to_be_bytes(*self).to_vec() }
        }
        impl BeDeserialize for $t {
            fn try_de_be(data: &[u8], off: &mut usize) -> Option<Self> {
                if data.len() < *off + $sz { return None; }
                let mut buf = [0u8; $sz];
                buf.copy_from_slice(&data[*off..*off + $sz]);
                *off += $sz;
                Some(<$t>::from_be_bytes(buf))
            }
        }
    }
}

impl_be_uint!(u16, 2);
impl_be_uint!(u32, 4);
impl_be_uint!(u64, 8);
impl_be_uint!(u128, 16);
impl_be_uint!(i16, 2);
impl_be_uint!(i32, 4);
impl_be_uint!(i64, 8);
impl_be_uint!(i128, 16);

impl BeSerialize for u8 {
    fn ser_be(&self) -> Vec<u8> { vec![*self] }
}
impl BeDeserialize for u8 {
    fn try_de_be(data: &[u8], off: &mut usize) -> Option<Self> {
        if data.len() < *off + 1 { return None; }
        let v = data[*off]; *off += 1; Some(v)
    }
}
impl BeSerialize for i8 {
    fn ser_be(&self) -> Vec<u8> { vec![*self as u8] }
}
impl BeDeserialize for i8 {
    fn try_de_be(data: &[u8], off: &mut usize) -> Option<Self> {
        if data.len() < *off + 1 { return None; }
        let v = data[*off] as i8; *off += 1; Some(v)
    }
}
impl BeSerialize for bool {
    fn ser_be(&self) -> Vec<u8> { vec![if *self { 1 } else { 0 }] }
}
impl BeDeserialize for bool {
    fn try_de_be(data: &[u8], off: &mut usize) -> Option<Self> {
        if data.len() < *off + 1 { return None; }
        let v = data[*off] != 0; *off += 1; Some(v)
    }
}
impl BeSerialize for [u8; 32] {
    fn ser_be(&self) -> Vec<u8> { self.to_vec() }
}
impl BeDeserialize for [u8; 32] {
    fn try_de_be(data: &[u8], off: &mut usize) -> Option<Self> {
        if data.len() < *off + 32 { return None; }
        let mut buf = [0u8; 32];
        buf.copy_from_slice(&data[*off..*off + 32]);
        *off += 32;
        Some(buf)
    }
}
impl BeSerialize for U256 {
    fn ser_be(&self) -> Vec<u8> { self.to_be_bytes().to_vec() }
}
impl BeDeserialize for U256 {
    fn try_de_be(data: &[u8], off: &mut usize) -> Option<Self> {
        if data.len() < *off + 32 { return None; }
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&data[*off..*off + 32]);
        *off += 32;
        Some(U256::from_be_bytes(&bytes))
    }
}
impl BeSerialize for String {
    fn ser_be(&self) -> Vec<u8> {
        let b = self.as_bytes();
        let mut out = (b.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(b);
        out
    }
}
impl BeDeserialize for String {
    fn try_de_be(data: &[u8], off: &mut usize) -> Option<Self> {
        let len = <u32 as BeDeserialize>::try_de_be(data, off)? as usize;
        if data.len() < *off + len { return None; }
        let s = String::from_utf8_lossy(&data[*off..*off + len]).to_string();
        *off += len;
        Some(s)
    }
}
impl<T: BeSerialize> BeSerialize for Vec<T> {
    fn ser_be(&self) -> Vec<u8> {
        let mut out = (self.len() as u32).to_be_bytes().to_vec();
        for item in self { out.extend_from_slice(&item.ser_be()); }
        out
    }
}
impl<T: BeDeserialize> BeDeserialize for Vec<T> {
    fn try_de_be(data: &[u8], off: &mut usize) -> Option<Self> {
        let len = <u32 as BeDeserialize>::try_de_be(data, off)? as usize;
        (0..len).map(|_| T::try_de_be(data, off)).collect()
    }
}
impl<T: BeSerialize> BeSerialize for Option<T> {
    fn ser_be(&self) -> Vec<u8> {
        match self {
            None => vec![0],
            Some(v) => { let mut out = vec![1]; out.extend_from_slice(&v.ser_be()); out }
        }
    }
}
impl<T: BeDeserialize> BeDeserialize for Option<T> {
    fn try_de_be(data: &[u8], off: &mut usize) -> Option<Self> {
        if data.len() < *off + 1 { return None; }
        let tag = data[*off]; *off += 1;
        if tag == 0 { Some(None) } else { Some(Some(T::try_de_be(data, off)?)) }
    }
}
impl<K: BeSerialize + Eq + Hash, V: BeSerialize> BeSerialize for HashMap<K, V> {
    fn ser_be(&self) -> Vec<u8> {
        let mut out = (self.len() as u32).to_be_bytes().to_vec();
        for (k, v) in self { out.extend_from_slice(&k.ser_be()); out.extend_from_slice(&v.ser_be()); }
        out
    }
}
impl<K: BeDeserialize + Eq + Hash, V: BeDeserialize> BeDeserialize for HashMap<K, V> {
    fn try_de_be(data: &[u8], off: &mut usize) -> Option<Self> {
        let len = <u32 as BeDeserialize>::try_de_be(data, off)? as usize;
        let mut m = HashMap::with_capacity(len);
        for _ in 0..len { m.insert(K::try_de_be(data, off)?, V::try_de_be(data, off)?); }
        Some(m)
    }
}

macro_rules! impl_tuple_be {
    ($($idx:tt : $T:ident),+) => {
        impl<$($T: BeSerialize),+> BeSerialize for ($($T,)+) {
            fn ser_be(&self) -> Vec<u8> {
                let mut out = Vec::new();
                $(out.extend_from_slice(&self.$idx.ser_be());)+
                out
            }
        }
        impl<$($T: BeDeserialize),+> BeDeserialize for ($($T,)+) {
            fn try_de_be(data: &[u8], off: &mut usize) -> Option<Self> {
                Some(($($T::try_de_be(data, off)?,)+))
            }
        }
    };
}

impl_tuple_be!(0: A, 1: B);
impl_tuple_be!(0: A, 1: B, 2: C);
impl_tuple_be!(0: A, 1: B, 2: C, 3: D);
impl_tuple_be!(0: A, 1: B, 2: C, 3: D, 4: E);

// ---------------------------------------------------------------------------
// tvm-cell integration (behind "tvm" feature)
// ---------------------------------------------------------------------------

#[cfg(feature = "tvm")]
impl BeSerialize for tvm_cell::Cell {
    fn ser_be(&self) -> Vec<u8> {
        let boc = tvm_cell::write_boc(self).expect("TvmCell::ser_be: write_boc failed");
        let mut out = (boc.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(&boc);
        out
    }
}

#[cfg(feature = "tvm")]
impl BeDeserialize for tvm_cell::Cell {
    fn try_de_be(data: &[u8], off: &mut usize) -> Option<Self> {
        let len = <u32 as BeDeserialize>::try_de_be(data, off)? as usize;
        if len == 0 {
            return Some(tvm_cell::Cell::default());
        }
        if data.len() < *off + len { return None; }
        let boc = &data[*off..*off + len];
        *off += len;
        tvm_cell::read_single_root_boc(boc).ok()
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u256_basic_arithmetic() {
        let a = U256::from_u128(100);
        let b = U256::from_u128(200);
        assert_eq!(a + b, U256::from_u128(300));
        assert_eq!(b - a, U256::from_u128(100));
        assert_eq!(a * b, U256::from_u128(20000));
        assert_eq!(b / a, U256::from_u128(2));
        assert_eq!(b % U256::from_u128(150), U256::from_u128(50));
    }

    #[test]
    fn u256_large_values() {
        let a = U256 { hi: 1, lo: 0 };
        let b = U256::from_u128(1);
        let sum = a + b;
        assert_eq!(sum.hi, 1);
        assert_eq!(sum.lo, 1);
    }

    #[test]
    fn u256_be_bytes_roundtrip() {
        let v = U256 { hi: 0xdeadbeef, lo: 0xcafebabe };
        let bytes = v.to_be_bytes();
        let v2 = U256::from_be_bytes(&bytes);
        assert_eq!(v, v2);
    }

    #[test]
    fn u256_display() {
        assert_eq!(format!("{}", U256::from_u128(42)), "42");
        let big = U256 { hi: 1, lo: 0 };
        assert!(format!("{}", big).starts_with("0x"));
    }

    #[test]
    fn u256_cross_ops() {
        let a = U256::from_u128(100);
        assert_eq!(a + 50u64, U256::from_u128(150));
        assert_eq!(a - 10u32, U256::from_u128(90));
        assert_eq!(a * 3u64, U256::from_u128(300));
        assert_eq!(a / 10u64, U256::from_u128(10));
    }

    #[test]
    fn u256_comparisons() {
        let a = U256::from_u128(100);
        assert!(a == 100u64);
        assert!(a > U256::from_u128(50));
        assert!(a < U256::from_u128(200));
    }

    #[test]
    fn cam_cast_u256() {
        let v: U256 = CamCast::<U256>::cam_cast(42u64);
        assert_eq!(v, U256::from_u128(42));
        let n: u64 = CamCast::<u64>::cam_cast(U256::from_u128(100));
        assert_eq!(n, 100);
    }

    #[test]
    fn checked_ops_u256() {
        let a = U256::from_u128(10);
        let b = U256::from_u128(20);
        assert_eq!(cam_add(a, b), U256::from_u128(30));
        assert_eq!(cam_sub(b, a), U256::from_u128(10));
        assert_eq!(cam_mul(a, b), U256::from_u128(200));
    }

    #[test]
    fn hashmap_ext_basic() {
        let m: HashMap<String, u64> = HashMap::new();
        let m = m.cam_insert("a".to_string(), 10);
        assert!(m.exists("a".to_string()));
        assert_eq!(m.cam_get("a".to_string()), 10);
        assert_eq!(m.cam_get("b".to_string()), 0);
    }

    #[test]
    fn cam_muldiv_basic() {
        assert_eq!(cam_muldiv(10u64, 20u64, 5u64), 40u64);
    }

    #[test]
    fn cam_divmod_basic() {
        assert_eq!(cam_divmod(17u64, 5u64), (3, 2));
    }

    #[test]
    fn u256_shl_shr() {
        let v = U256::from_u128(1);
        let shifted = v << 128u32;
        assert_eq!(shifted, U256 { hi: 1, lo: 0 });
        let back = shifted >> 128u32;
        assert_eq!(back, v);
    }

    #[test]
    #[should_panic(expected = "U256 add overflow")]
    fn u256_add_overflow() {
        let _ = U256::MAX + U256::ONE;
    }

    #[test]
    #[should_panic(expected = "U256 sub underflow")]
    fn u256_sub_underflow() {
        let _ = U256::ZERO - U256::ONE;
    }

    #[test]
    fn cam_pow_basic() {
        assert_eq!(cam_pow(2u64, 10u64), 1024u64);
        assert_eq!(cam_pow(3u128, 5u128), 243u128);
        assert_eq!(cam_pow(U256::from_u128(2), U256::from_u128(0)), U256::ONE);
        assert_eq!(cam_pow(U256::from_u128(2), U256::from_u128(8)), U256::from_u128(256));
        assert_eq!(cam_pow(U256::from_u128(10), U256::from_u128(18)), U256::from_u128(1_000_000_000_000_000_000));
    }

    // ===================================================================
    // U256: cross-type arithmetic (all primitive partner types)
    // ===================================================================

    #[test]
    fn u256_cross_u8() {
        let a = U256::from_u128(200);
        assert_eq!(a + 5u8, U256::from_u128(205));
        assert_eq!(a - 10u8, U256::from_u128(190));
        assert_eq!(a * 2u8, U256::from_u128(400));
        assert_eq!(a / 4u8, U256::from_u128(50));
        assert_eq!(a % 7u8, U256::from_u128(200 % 7));
    }

    #[test]
    fn u256_cross_u16() {
        let a = U256::from_u128(50000);
        assert_eq!(a + 1000u16, U256::from_u128(51000));
        assert_eq!(a - 500u16, U256::from_u128(49500));
        assert_eq!(a * 3u16, U256::from_u128(150000));
        assert_eq!(a / 100u16, U256::from_u128(500));
    }

    #[test]
    fn u256_cross_u128() {
        let a = U256::from_u128(u128::MAX);
        let b = U256::from_u128(1);
        let sum = a + b;
        assert_eq!(sum.hi, 1);
        assert_eq!(sum.lo, 0);

        let c = U256::from_u128(1_000_000_000_000_000_000u128);
        assert_eq!(c * 2u128, U256::from_u128(2_000_000_000_000_000_000u128));
        assert_eq!(c / 1_000_000u128, U256::from_u128(1_000_000_000_000u128));
    }

    #[test]
    fn u256_cross_i32() {
        let a = U256::from_u128(100);
        assert_eq!(a + 50i32, U256::from_u128(150));
        assert_eq!(a - 10i32, U256::from_u128(90));
        assert_eq!(a * 3i32, U256::from_u128(300));
        assert_eq!(a / 10i32, U256::from_u128(10));
    }

    // ===================================================================
    // U256: comparison operators — exhaustive
    // ===================================================================

    #[test]
    fn u256_comparison_eq_ne() {
        let a = U256::from_u128(42);
        let b = U256::from_u128(42);
        let c = U256::from_u128(99);
        assert!(a == b);
        assert!(a != c);
        assert!(!(a != b));
        assert!(!(a == c));
    }

    #[test]
    fn u256_comparison_ordering() {
        let small = U256::from_u128(10);
        let big = U256::from_u128(1000);
        assert!(small < big);
        assert!(small <= big);
        assert!(big > small);
        assert!(big >= small);
        assert!(small <= small);
        assert!(small >= small);
    }

    #[test]
    fn u256_comparison_hi_lo_boundary() {
        let lo_max = U256::from_u128(u128::MAX);
        let hi_one = U256 { hi: 1, lo: 0 };
        assert!(lo_max < hi_one);
        assert!(hi_one > lo_max);
        assert!(lo_max != hi_one);

        let a = U256 { hi: 1, lo: 100 };
        let b = U256 { hi: 1, lo: 200 };
        assert!(a < b);
        assert!(b > a);

        let c = U256 { hi: 2, lo: 0 };
        assert!(c > b);
    }

    #[test]
    fn u256_comparison_cross_type() {
        let a = U256::from_u128(255);
        assert!(a == 255u64);
        assert!(a > U256::from_u128(0));
        assert!(a < U256::from_u128(256));
        assert!(U256::ZERO == 0u64);
    }

    // ===================================================================
    // U256: bitwise operations
    // ===================================================================

    #[test]
    fn u256_bitand() {
        let a = U256::from_u128(0xFF00);
        let b = U256::from_u128(0x0FF0);
        assert_eq!(a & b, U256::from_u128(0x0F00));
    }

    #[test]
    fn u256_bitor() {
        let a = U256::from_u128(0xFF00);
        let b = U256::from_u128(0x00FF);
        assert_eq!(a | b, U256::from_u128(0xFFFF));
    }

    #[test]
    fn u256_bitxor() {
        let a = U256::from_u128(0b1100);
        let b = U256::from_u128(0b1010);
        assert_eq!(a ^ b, U256::from_u128(0b0110));
    }

    #[test]
    fn u256_bitwise_cross_types() {
        let a = U256::from_u128(0xFF);
        assert_eq!(a & 0x0Fu8, U256::from_u128(0x0F));
        assert_eq!(a | 0x100u16, U256::from_u128(0x1FF));
        assert_eq!(a ^ 0xFFu64, U256::ZERO);
    }

    #[test]
    fn u256_shift_cross_boundary() {
        let one = U256::from_u128(1);
        let shifted = one << 200u32;
        assert!(shifted.hi > 0);
        assert_eq!(shifted.lo, 0);
        let back = shifted >> 200u32;
        assert_eq!(back, one);
    }

    #[test]
    fn u256_shift_zero() {
        let v = U256::from_u128(42);
        assert_eq!(v << 0u32, v);
        assert_eq!(v >> 0u32, v);
    }

    #[test]
    fn u256_shift_by_128() {
        let v = U256::from_u128(0xABCD);
        let shifted = v << 128u32;
        assert_eq!(shifted.hi, 0xABCD);
        assert_eq!(shifted.lo, 0);
        let back = shifted >> 128u32;
        assert_eq!(back, v);
    }

    // ===================================================================
    // U256: edge cases
    // ===================================================================

    #[test]
    fn u256_mul_by_zero() {
        assert_eq!(U256::MAX * U256::ZERO, U256::ZERO);
        assert_eq!(U256::ZERO * U256::from_u128(999), U256::ZERO);
    }

    #[test]
    fn u256_div_by_one() {
        assert_eq!(U256::MAX / U256::ONE, U256::MAX);
        assert_eq!(U256::from_u128(42) / U256::ONE, U256::from_u128(42));
    }

    #[test]
    fn u256_mod_self() {
        let v = U256::from_u128(12345);
        assert_eq!(v % v, U256::ZERO);
    }

    #[test]
    fn u256_identity_ops() {
        let v = U256::from_u128(42);
        assert_eq!(v + U256::ZERO, v);
        assert_eq!(v - U256::ZERO, v);
        assert_eq!(v * U256::ONE, v);
    }

    #[test]
    #[should_panic]
    fn u256_div_by_zero() {
        let _ = U256::from_u128(1) / U256::ZERO;
    }

    #[test]
    #[should_panic]
    fn u256_mod_by_zero() {
        let _ = U256::from_u128(1) % U256::ZERO;
    }

    #[test]
    #[should_panic(expected = "U256 mul overflow")]
    fn u256_mul_overflow() {
        let _ = U256::MAX * U256::from_u128(2);
    }

    // ===================================================================
    // CamCast: all primitive round-trips
    // ===================================================================

    #[test]
    fn cam_cast_u256_from_all_primitives() {
        let v8: U256 = CamCast::<U256>::cam_cast(255u8);
        assert_eq!(v8, U256::from_u128(255));
        let v16: U256 = CamCast::<U256>::cam_cast(65535u16);
        assert_eq!(v16, U256::from_u128(65535));
        let v32: U256 = CamCast::<U256>::cam_cast(0xDEADBEEFu32);
        assert_eq!(v32, U256::from_u128(0xDEADBEEF));
        let v64: U256 = CamCast::<U256>::cam_cast(u64::MAX);
        assert_eq!(v64, U256::from_u128(u64::MAX as u128));
        let v128: U256 = CamCast::<U256>::cam_cast(u128::MAX);
        assert_eq!(v128, U256::from_u128(u128::MAX));
    }

    #[test]
    fn cam_cast_u256_to_primitives() {
        let v = U256::from_u128(42);
        assert_eq!(CamCast::<u8>::cam_cast(v), 42u8);
        assert_eq!(CamCast::<u16>::cam_cast(v), 42u16);
        assert_eq!(CamCast::<u32>::cam_cast(v), 42u32);
        assert_eq!(CamCast::<u64>::cam_cast(v), 42u64);
        assert_eq!(CamCast::<u128>::cam_cast(v), 42u128);
    }

    #[test]
    fn cam_cast_u256_identity() {
        let v = U256 { hi: 0xABCD, lo: 0x1234 };
        let cast: U256 = CamCast::<U256>::cam_cast(v);
        assert_eq!(cast, v);
    }

    // ===================================================================
    // CamMulDiv for U256 (512-bit intermediate)
    // ===================================================================

    #[test]
    fn cam_muldiv_u256_basic() {
        let a = U256::from_u128(100);
        let b = U256::from_u128(200);
        let c = U256::from_u128(50);
        assert_eq!(cam_muldiv(a, b, c), U256::from_u128(400));
    }

    #[test]
    fn cam_muldiv_u256_avoids_overflow() {
        let a = U256::from_u128(u128::MAX);
        let b = U256::from_u128(2);
        let c = U256::from_u128(2);
        assert_eq!(cam_muldiv(a, b, c), U256::from_u128(u128::MAX));
    }

    #[test]
    fn cam_muldivmod_u256() {
        let (q, r) = cam_muldivmod(U256::from_u128(10), U256::from_u128(7), U256::from_u128(3));
        assert_eq!(q, U256::from_u128(23));
        assert_eq!(r, U256::from_u128(1));
    }

    #[test]
    fn cam_divmod_u256() {
        let (q, r) = cam_divmod(U256::from_u128(17), U256::from_u128(5));
        assert_eq!(q, U256::from_u128(3));
        assert_eq!(r, U256::from_u128(2));
    }

    #[test]
    fn cam_divc_u256() {
        assert_eq!(cam_divc(U256::from_u128(10), U256::from_u128(3)), U256::from_u128(4));
        assert_eq!(cam_divc(U256::from_u128(9), U256::from_u128(3)), U256::from_u128(3));
        assert_eq!(cam_divc(U256::ZERO, U256::from_u128(5)), U256::ZERO);
    }

    #[test]
    fn cam_divr_u256() {
        assert_eq!(cam_divr(U256::from_u128(10), U256::from_u128(3)), U256::from_u128(3));
        assert_eq!(cam_divr(U256::from_u128(11), U256::from_u128(3)), U256::from_u128(4));
    }

    // ===================================================================
    // cam_pow: edge cases and larger values
    // ===================================================================

    #[test]
    fn cam_pow_zero_exponent() {
        assert_eq!(cam_pow(0u64, 0u64), 1u64);
        assert_eq!(cam_pow(999u64, 0u64), 1u64);
        assert_eq!(cam_pow(U256::ZERO, U256::ZERO), U256::ONE);
    }

    #[test]
    fn cam_pow_one_exponent() {
        assert_eq!(cam_pow(42u64, 1u64), 42u64);
        assert_eq!(cam_pow(U256::from_u128(99), U256::ONE), U256::from_u128(99));
    }

    #[test]
    fn cam_pow_base_one() {
        assert_eq!(cam_pow(1u64, 1000u64), 1u64);
        assert_eq!(cam_pow(U256::ONE, U256::from_u128(999)), U256::ONE);
    }

    #[test]
    fn cam_pow_u256_large() {
        let fp18 = U256::from_u128(1_000_000_000_000_000_000u128);
        let result = cam_pow(U256::from_u128(10), U256::from_u128(18));
        assert_eq!(result, fp18);
    }

    // ===================================================================
    // Checked ops: cam_add, cam_sub, cam_mul for primitives
    // ===================================================================

    #[test]
    fn checked_ops_u64() {
        assert_eq!(cam_add(10u64, 20u64), 30u64);
        assert_eq!(cam_sub(20u64, 5u64), 15u64);
        assert_eq!(cam_mul(6u64, 7u64), 42u64);
    }

    #[test]
    fn checked_ops_u128() {
        assert_eq!(cam_add(10u128, 20u128), 30u128);
        assert_eq!(cam_sub(20u128, 5u128), 15u128);
        assert_eq!(cam_mul(6u128, 7u128), 42u128);
    }

    #[test]
    #[should_panic]
    fn checked_add_u64_overflow() {
        cam_add(u64::MAX, 1u64);
    }

    #[test]
    #[should_panic]
    fn checked_sub_u64_underflow() {
        cam_sub(0u64, 1u64);
    }

    #[test]
    #[should_panic]
    fn checked_mul_u64_overflow() {
        cam_mul(u64::MAX, 2u64);
    }

    // ===================================================================
    // CamDiv trait: zero, one, two helpers
    // ===================================================================

    #[test]
    fn cam_div_helpers_u256() {
        assert_eq!(U256::zero(), U256::ZERO);
        assert_eq!(U256::one(), U256::ONE);
        assert_eq!(U256::two(), U256::from_u128(2));
    }

    #[test]
    fn cam_div_helpers_primitives() {
        assert_eq!(u64::zero(), 0u64);
        assert_eq!(u64::one(), 1u64);
        assert_eq!(u64::two(), 2u64);
        assert_eq!(u128::zero(), 0u128);
        assert_eq!(u128::one(), 1u128);
    }

    // ===================================================================
    // FP18-style arithmetic simulation (what accumulator contracts do)
    // ===================================================================

    #[test]
    fn fp18_multiply_and_divide() {
        let fp18 = U256::from_u128(1_000_000_000_000_000_000);
        let half = U256::from_u128(500_000_000_000_000_000);
        let product = cam_muldiv(fp18, half, fp18);
        assert_eq!(product, half);
    }

    #[test]
    fn fp18_square() {
        let val = U256::from_u128(500_000_000_000_000_000); // 0.5 in FP18
        let fp18 = U256::from_u128(1_000_000_000_000_000_000);
        let sq = val * val / fp18;
        assert_eq!(sq, U256::from_u128(250_000_000_000_000_000)); // 0.25
    }

    #[test]
    fn fp18_taylor_term() {
        let fp18 = U256::from_u128(1_000_000_000_000_000_000);
        let r = U256::from_u128(500_000_000_000_000_000); // 0.5
        let t1 = r;
        let t2 = t1 * r / fp18 / U256::from_u128(2);
        // t2 = 0.5 * 0.5 / 2 = 0.125 = 125_000_000_000_000_000
        assert_eq!(t2, U256::from_u128(125_000_000_000_000_000));
    }

    #[test]
    fn u256_large_multiplication() {
        let a = U256::from_u128(10u128.pow(18));
        let b = U256::from_u128(10u128.pow(18));
        let product = a * b;
        assert_eq!(product, U256::from_u128(10u128.pow(36)));
    }

    // AUDIT regression tests

    #[test]
    fn cam_pow_u256_2_pow_255_no_spurious_panic() {
        let result = cam_pow(U256::from_u128(2), U256::from_u128(255));
        assert_eq!(result, U256 { hi: 1u128 << 127, lo: 0 });
    }

    #[test]
    fn cam_pow_u256_2_pow_128_no_panic() {
        let result = cam_pow(U256::from_u128(2), U256::from_u128(128));
        assert_eq!(result, U256 { hi: 1, lo: 0 });
    }

    #[test]
    #[should_panic(expected = "negative")]
    fn u256_from_negative_i32_panics() {
        let _ = U256::from(-1i32);
    }

    #[test]
    fn u256_from_positive_i32_ok() {
        assert_eq!(U256::from(42i32), U256::from_u128(42));
    }

    #[test]
    #[should_panic(expected = "negative")]
    fn u256_add_negative_i32_panics() {
        let _ = U256::from_u128(100) + (-1i32);
    }

    #[test]
    #[should_panic(expected = "negative")]
    fn u256_mul_negative_i32_panics() {
        let _ = U256::from_u128(100) * (-1i32);
    }

    #[test]
    fn u256_eq_negative_i32_no_panic() {
        assert!(!(U256::from_u128(100) == -1i32));
    }

    #[test]
    #[should_panic]
    fn cam_cast_negative_i32_to_u256_panics() {
        let _: U256 = CamCast::<U256>::cam_cast(-1i32);
    }

    #[test]
    #[should_panic]
    fn cam_cast_negative_i8_to_u256_panics() {
        let _: U256 = CamCast::<U256>::cam_cast(-1i8);
    }

    #[test]
    #[should_panic]
    fn cam_cast_u256_to_u8_overflow_panics() {
        let _: u8 = CamCast::<u8>::cam_cast(U256::from_u128(256));
    }

    #[test]
    #[should_panic]
    fn cam_cast_u256_to_u64_overflow_panics() {
        let _: u64 = CamCast::<u64>::cam_cast(U256::from_u128(u64::MAX as u128 + 1));
    }

    #[test]
    fn cam_cast_u256_to_prim_exact_max_ok() {
        assert_eq!(CamCast::<u8>::cam_cast(U256::from_u128(255)), 255u8);
        assert_eq!(CamCast::<u64>::cam_cast(U256::from_u128(u64::MAX as u128)), u64::MAX);
    }

    #[test]
    #[should_panic]
    fn u256_wide_muldiv_overflow_panics() {
        let _ = cam_muldiv(U256::MAX, U256::MAX, U256::ONE);
    }

    #[test]
    #[should_panic]
    fn i128_wide_muldiv_overflow_panics() {
        let _ = cam_muldiv(-1i128, i128::MIN, 1i128);
    }

    #[test]
    fn u256_shl_by_large_u256_returns_zero() {
        let v = U256::from_u128(1);
        let large_shift = U256::from_u128((1u128 << 32) + 1);
        assert_eq!(v << large_shift, U256::ZERO);
    }

    #[test]
    fn cam_modpow2_n_eq_128() {
        assert_eq!(cam_modpow2(42, 128), 42);
    }

    #[test]
    fn cam_modpow2_n_gt_128() {
        assert_eq!(cam_modpow2(u128::MAX, 200), u128::MAX);
    }

    #[test]
    fn cam_divr_near_max_no_overflow() {
        let result = cam_divr(u64::MAX, 4u64);
        assert_eq!(result, u64::MAX / 4 + 1);
    }

    #[test]
    fn u256_not_bitwise() {
        assert_eq!(!U256::from_u128(0), U256::MAX);
        assert_eq!(!U256::MAX, U256::ZERO);
    }

    // ===================================================================
    // BUG-1: Shl/Shr<u64> must not truncate large shift amounts
    // ===================================================================

    #[test]
    fn u256_shl_u64_large_returns_zero() {
        let v = U256::from_u128(1);
        assert_eq!(v << 256u64, U256::ZERO);
        assert_eq!(v << 300u64, U256::ZERO);
        assert_eq!(v << u64::MAX, U256::ZERO);
        // 0x1_0000_0001u64 would truncate to 1 with `as u32` — must return ZERO
        assert_eq!(v << 0x1_0000_0001u64, U256::ZERO);
    }

    #[test]
    fn u256_shr_u64_large_returns_zero() {
        let v = U256::MAX;
        assert_eq!(v >> 256u64, U256::ZERO);
        assert_eq!(v >> u64::MAX, U256::ZERO);
        assert_eq!(v >> 0x1_0000_0001u64, U256::ZERO);
    }

    #[test]
    fn u256_shl_u64_normal_works() {
        assert_eq!(U256::from_u128(1) << 128u64, U256 { hi: 1, lo: 0 });
        assert_eq!(U256::from_u128(1) << 0u64, U256::from_u128(1));
        assert_eq!(U256::from_u128(1) << 255u64, U256 { hi: 1u128 << 127, lo: 0 });
    }

    // ===================================================================
    // BUG-2: CamCast prim→prim is now checked (no silent truncation)
    // ===================================================================

    #[test]
    #[should_panic(expected = "out of range")]
    fn cam_cast_u16_to_u8_overflow_panics() {
        let _: u8 = CamCast::<u8>::cam_cast(256u16);
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn cam_cast_negative_i32_to_u64_panics() {
        let _: u64 = CamCast::<u64>::cam_cast(-1i32);
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn cam_cast_large_u64_to_i32_panics() {
        let _: i32 = CamCast::<i32>::cam_cast(u64::MAX);
    }

    #[test]
    fn cam_cast_prim_widening_ok() {
        assert_eq!(CamCast::<u16>::cam_cast(255u8), 255u16);
        assert_eq!(CamCast::<u64>::cam_cast(42u32), 42u64);
        assert_eq!(CamCast::<i64>::cam_cast(42i32), 42i64);
        assert_eq!(CamCast::<u128>::cam_cast(100u64), 100u128);
    }

    #[test]
    fn cam_cast_prim_identity_ok() {
        assert_eq!(CamCast::<u8>::cam_cast(42u8), 42u8);
        assert_eq!(CamCast::<i32>::cam_cast(-5i32), -5i32);
    }

    #[test]
    fn cam_cast_prim_narrowing_exact_max() {
        assert_eq!(CamCast::<u8>::cam_cast(255u16), 255u8);
        assert_eq!(CamCast::<i8>::cam_cast(127i16), 127i8);
    }

    #[test]
    fn cam_cast_bool_ok() {
        assert_eq!(CamCast::<u8>::cam_cast(true), 1u8);
        assert_eq!(CamCast::<u32>::cam_cast(false), 0u32);
        assert_eq!(CamCast::<i64>::cam_cast(true), 1i64);
    }

    // ===================================================================
    // BUG-3: BitAnd/BitOr/BitXor<u32> for U256 now exists
    // ===================================================================

    #[test]
    fn u256_bitwise_u32() {
        assert_eq!(U256::from_u128(0xFF) & 0x0Fu32, U256::from_u128(0x0F));
        assert_eq!(U256::from_u128(0xF0) | 0x0Fu32, U256::from_u128(0xFF));
        assert_eq!(U256::from_u128(0xFF) ^ 0xFFu32, U256::ZERO);
    }

    // ===================================================================
    // BUG-4: PartialEq<i32> returns false for negative (not panic)
    // ===================================================================

    #[test]
    fn u256_eq_negative_i32_returns_false() {
        assert!(U256::from_u128(5) != -1i32);
        assert!(U256::ZERO != -100i32);
        assert!(U256::MAX != -1i32);
    }

    #[test]
    fn u256_eq_positive_i32_works() {
        assert!(U256::from_u128(42) == 42i32);
        assert!(U256::from_u128(0) == 0i32);
        assert!(U256::from_u128(42) != 43i32);
    }

    // ===================================================================
    // BUG-5: cam_divc/cam_divr reject negative operands
    // ===================================================================

    #[test]
    #[should_panic(expected = "cam_divc requires")]
    fn cam_divc_negative_x_panics() {
        cam_divc(-4i32, 3i32);
    }

    #[test]
    #[should_panic(expected = "cam_divc requires")]
    fn cam_divc_negative_y_panics() {
        cam_divc(4i32, -3i32);
    }

    #[test]
    #[should_panic(expected = "cam_divr requires")]
    fn cam_divr_negative_x_panics() {
        cam_divr(-4i32, 3i32);
    }

    #[test]
    #[should_panic(expected = "cam_divr requires")]
    fn cam_divr_negative_y_panics() {
        cam_divr(4i32, -3i32);
    }

    #[test]
    fn cam_divc_positive_signed_ok() {
        assert_eq!(cam_divc(10i32, 3i32), 4i32);
        assert_eq!(cam_divc(9i32, 3i32), 3i32);
        assert_eq!(cam_divc(0i32, 5i32), 0i32);
    }

    #[test]
    fn cam_divr_positive_signed_ok() {
        assert_eq!(cam_divr(10i32, 3i32), 3i32);
        assert_eq!(cam_divr(11i32, 3i32), 4i32);
        assert_eq!(cam_divr(0i32, 5i32), 0i32);
    }

    // ===================================================================
    // Phase 2: Comprehensive coverage expansion
    // ===================================================================

    // --- U256 constructors ---

    #[test]
    fn u256_new_constructor() {
        let v = U256::new(0xAA, 0xBB);
        assert_eq!(v.hi, 0xAA);
        assert_eq!(v.lo, 0xBB);
    }

    #[test]
    fn u256_from_u128_constructor() {
        assert_eq!(U256::from_u128(0), U256::ZERO);
        assert_eq!(U256::from_u128(1), U256::ONE);
        assert_eq!(U256::from_u128(u128::MAX), U256 { hi: 0, lo: u128::MAX });
    }

    #[test]
    fn u256_from_u64_constructor() {
        assert_eq!(U256::from_u64(0), U256::ZERO);
        assert_eq!(U256::from_u64(u64::MAX), U256::from_u128(u64::MAX as u128));
    }

    // --- wrapping arithmetic ---

    #[test]
    fn u256_wrapping_add_no_overflow() {
        assert_eq!(U256::from_u128(10).wrapping_add(U256::from_u128(20)), U256::from_u128(30));
    }

    #[test]
    fn u256_wrapping_add_overflow() {
        assert_eq!(U256::MAX.wrapping_add(U256::ONE), U256::ZERO);
        assert_eq!(U256::MAX.wrapping_add(U256::from_u128(2)), U256::ONE);
    }

    #[test]
    fn u256_wrapping_sub_no_underflow() {
        assert_eq!(U256::from_u128(20).wrapping_sub(U256::from_u128(5)), U256::from_u128(15));
    }

    #[test]
    fn u256_wrapping_sub_underflow() {
        assert_eq!(U256::ZERO.wrapping_sub(U256::ONE), U256::MAX);
        assert_eq!(U256::ZERO.wrapping_sub(U256::from_u128(2)), U256::MAX - U256::ONE);
    }

    #[test]
    fn u256_wrapping_mul_no_overflow() {
        assert_eq!(U256::from_u128(6).wrapping_mul(U256::from_u128(7)), U256::from_u128(42));
    }

    #[test]
    fn u256_wrapping_mul_overflow() {
        let result = U256::MAX.wrapping_mul(U256::from_u128(2));
        assert_eq!(result, U256::MAX - U256::ONE);
    }

    // --- to_u64/to_u128 panic paths ---

    #[test]
    #[should_panic(expected = "too large for u64")]
    fn u256_to_u64_overflow_panics() {
        U256::from_u128(u64::MAX as u128 + 1).to_u64();
    }

    #[test]
    #[should_panic(expected = "too large for u128")]
    fn u256_to_u128_overflow_panics() {
        U256 { hi: 1, lo: 0 }.to_u128();
    }

    #[test]
    fn u256_to_u64_exact_max_ok() {
        assert_eq!(U256::from_u128(u64::MAX as u128).to_u64(), u64::MAX);
    }

    #[test]
    fn u256_to_u128_exact_max_ok() {
        assert_eq!(U256::from_u128(u128::MAX).to_u128(), u128::MAX);
    }

    // --- from_be_bytes / to_be_bytes roundtrip edge cases ---

    #[test]
    fn u256_be_bytes_roundtrip_zero() {
        let v = U256::ZERO;
        assert_eq!(U256::from_be_bytes(&v.to_be_bytes()), v);
    }

    #[test]
    fn u256_be_bytes_roundtrip_max() {
        let v = U256::MAX;
        assert_eq!(U256::from_be_bytes(&v.to_be_bytes()), v);
    }

    #[test]
    fn u256_be_bytes_roundtrip_one() {
        let v = U256::ONE;
        let bytes = v.to_be_bytes();
        assert_eq!(bytes[31], 1);
        assert!(bytes[..31].iter().all(|b| *b == 0));
        assert_eq!(U256::from_be_bytes(&bytes), v);
    }

    #[test]
    fn u256_be_bytes_roundtrip_hi_only() {
        let v = U256 { hi: u128::MAX, lo: 0 };
        assert_eq!(U256::from_be_bytes(&v.to_be_bytes()), v);
    }

    // --- leading_zeros ---

    #[test]
    fn u256_leading_zeros() {
        assert_eq!(U256::ZERO.leading_zeros(), 256);
        assert_eq!(U256::ONE.leading_zeros(), 255);
        assert_eq!(U256::MAX.leading_zeros(), 0);
        assert_eq!(U256 { hi: 1, lo: 0 }.leading_zeros(), 127);
    }

    // --- is_zero ---

    #[test]
    fn u256_is_zero() {
        assert!(U256::ZERO.is_zero());
        assert!(!U256::ONE.is_zero());
        assert!(!U256::MAX.is_zero());
        assert!(!U256 { hi: 1, lo: 0 }.is_zero());
        assert!(!U256 { hi: 0, lo: 1 }.is_zero());
    }

    // --- Display ---

    #[test]
    fn u256_display_small() {
        assert_eq!(format!("{}", U256::ZERO), "0");
        assert_eq!(format!("{}", U256::ONE), "1");
        assert_eq!(format!("{}", U256::from_u128(999)), "999");
    }

    #[test]
    fn u256_display_large() {
        let v = U256 { hi: 0x1234, lo: 0xABCD };
        let s = format!("{}", v);
        assert!(s.starts_with("0x"));
        assert_eq!(s.len(), 2 + 64); // "0x" + 64 hex chars
    }

    // --- Division edge cases ---

    #[test]
    fn u256_div_large_by_small() {
        assert_eq!(U256::MAX / U256::from_u128(2), U256 { hi: u128::MAX / 2, lo: u128::MAX });
        assert_eq!(U256::MAX / U256::from_u128(3), U256::MAX / U256::from_u128(3)); // self-consistent
    }

    #[test]
    fn u256_div_rem_hi_boundary() {
        let n = U256 { hi: u128::MAX, lo: 0 };
        let (q, r) = u256_div_rem(n, U256::from_u128(7));
        assert_eq!(q * U256::from_u128(7) + r, n);
    }

    #[test]
    fn u256_rem_cross_types() {
        assert_eq!(U256::from_u128(10) % 3u16, U256::from_u128(1));
        assert_eq!(U256::from_u128(10) % 3u128, U256::from_u128(1));
    }

    // --- min, max, abs, clamp ---

    #[test]
    fn min_max_basic() {
        assert_eq!(min(3, 7), 3);
        assert_eq!(max(3, 7), 7);
        assert_eq!(min(5, 5), 5);
        assert_eq!(max(5, 5), 5);
    }

    #[test]
    fn min_max_u256() {
        assert_eq!(min(U256::from_u128(10), U256::from_u128(20)), U256::from_u128(10));
        assert_eq!(max(U256::from_u128(10), U256::from_u128(20)), U256::from_u128(20));
    }

    #[test]
    fn abs_basic() {
        assert_eq!(abs(-5i32), 5);
        assert_eq!(abs(0i32), 0);
        assert_eq!(abs(5i32), 5);
        assert_eq!(abs(-100i64), 100);
    }

    #[test]
    fn clamp_basic() {
        assert_eq!(clamp(5, 1, 10), 5);
        assert_eq!(clamp(0, 1, 10), 1);
        assert_eq!(clamp(15, 1, 10), 10);
        assert_eq!(clamp(1, 1, 10), 1);
        assert_eq!(clamp(10, 1, 10), 10);
    }

    // --- cam_sha256 ---

    #[test]
    fn cam_sha256_known_input() {
        let hash = cam_sha256(b"hello");
        assert_eq!(hash[0], 0x2c);
        assert_eq!(hash[1], 0xf2);
        assert_ne!(cam_sha256(b"hello"), cam_sha256(b"world"));
    }

    #[test]
    fn cam_sha256_empty() {
        let hash = cam_sha256(b"");
        assert_eq!(hash.len(), 32);
        assert_eq!(hash[0], 0xe3); // SHA256 of empty string starts with 0xe3
    }

    #[test]
    fn cam_sha256_deterministic() {
        assert_eq!(cam_sha256(b"test"), cam_sha256(b"test"));
    }

    // --- cam_sign ---

    #[test]
    fn cam_sign_positive() {
        assert_eq!(cam_sign(5i32), 1);
        assert_eq!(cam_sign(1i64), 1);
        assert_eq!(cam_sign(u64::MAX), 1);
    }

    #[test]
    fn cam_sign_zero() {
        assert_eq!(cam_sign(0i32), 0);
        assert_eq!(cam_sign(0u64), 0);
    }

    #[test]
    fn cam_sign_negative() {
        assert_eq!(cam_sign(-3i32), -1);
        assert_eq!(cam_sign(-100i64), -1);
    }

    // --- cam_minmax ---

    #[test]
    fn cam_minmax_basic() {
        assert_eq!(cam_minmax(7, 3), (3, 7));
        assert_eq!(cam_minmax(3, 7), (3, 7));
        assert_eq!(cam_minmax(5, 5), (5, 5));
    }

    #[test]
    fn cam_minmax_u256() {
        let a = U256::from_u128(100);
        let b = U256::from_u128(200);
        assert_eq!(cam_minmax(a, b), (a, b));
        assert_eq!(cam_minmax(b, a), (a, b));
    }

    // --- CamCast U256 ↔ signed prims ---

    #[test]
    fn cam_cast_u256_to_i32() {
        assert_eq!(CamCast::<i32>::cam_cast(U256::from_u128(42)), 42i32);
        assert_eq!(CamCast::<i32>::cam_cast(U256::ZERO), 0i32);
    }

    #[test]
    #[should_panic]
    fn cam_cast_u256_to_i32_overflow_panics() {
        let _: i32 = CamCast::<i32>::cam_cast(U256::from_u128(i32::MAX as u128 + 1));
    }

    #[test]
    fn cam_cast_u256_to_i64() {
        assert_eq!(CamCast::<i64>::cam_cast(U256::from_u128(999)), 999i64);
    }

    #[test]
    fn cam_cast_u256_to_i128() {
        assert_eq!(CamCast::<i128>::cam_cast(U256::from_u128(999)), 999i128);
    }

    #[test]
    #[should_panic]
    fn cam_cast_u256_to_i128_overflow_panics() {
        let _: i128 = CamCast::<i128>::cam_cast(U256::from_u128(i128::MAX as u128 + 1));
    }

    // --- CamMulDiv for various types ---

    #[test]
    fn cam_muldiv_u128() {
        assert_eq!(cam_muldiv(u128::MAX, 2u128, 2u128), u128::MAX);
        assert_eq!(cam_muldiv(1000u128, 2000u128, 500u128), 4000u128);
    }

    #[test]
    fn cam_muldiv_i128() {
        assert_eq!(cam_muldiv(1000i128, 2000i128, 500i128), 4000i128);
    }

    #[test]
    fn cam_muldiv_i64() {
        assert_eq!(cam_muldiv(-10i64, 20i64, 5i64), -40i64);
        assert_eq!(cam_muldiv(10i64, -20i64, 5i64), -40i64);
        assert_eq!(cam_muldiv(-10i64, -20i64, 5i64), 40i64);
    }

    #[test]
    fn cam_muldiv_usize() {
        assert_eq!(cam_muldiv(10usize, 20usize, 5usize), 40usize);
    }

    #[test]
    fn cam_muldiv_isize() {
        assert_eq!(cam_muldiv(10isize, 20isize, 5isize), 40isize);
    }

    #[test]
    fn cam_muldivmod_u128() {
        let (q, r) = cam_muldivmod(u128::MAX, 3u128, 7u128);
        let product = U256::from_u128(u128::MAX) * U256::from_u128(3);
        let expected_q = (product / U256::from_u128(7)).to_u128();
        let expected_r = (product % U256::from_u128(7)).to_u128();
        assert_eq!(q, expected_q);
        assert_eq!(r, expected_r);
    }

    #[test]
    fn cam_muldivmod_i128() {
        let (q, r) = cam_muldivmod(100i128, 7i128, 3i128);
        assert_eq!(q, 233);
        assert_eq!(r, 1);
    }

    // --- HashMap extensions ---

    #[test]
    fn hashmap_cam_remove() {
        let m: HashMap<String, u64> = HashMap::new();
        let m = m.cam_insert("a".to_string(), 10);
        let m = m.cam_remove("a".to_string());
        assert!(!m.exists("a".to_string()));
        assert_eq!(m.cam_get("a".to_string()), 0);
    }

    #[test]
    fn hashmap_cam_update() {
        let m: HashMap<String, u64> = HashMap::new();
        let m = m.cam_insert("a".to_string(), 10);
        let m = m.cam_update("a".to_string(), 20);
        assert_eq!(m.cam_get("a".to_string()), 20);
    }

    #[test]
    fn hashmap_cam_keys() {
        let m: HashMap<u32, u64> = HashMap::new();
        let m = m.cam_insert(1, 10).cam_insert(2, 20).cam_insert(3, 30);
        let mut keys = m.cam_keys();
        keys.sort();
        assert_eq!(keys, vec![1, 2, 3]);
    }

    #[test]
    fn hashmap_cam_contains() {
        let m: HashMap<u32, u64> = HashMap::new();
        let m = m.cam_insert(1, 10);
        assert!(m.cam_contains(1));
        assert!(!m.cam_contains(2));
    }

    #[test]
    fn hashmap_cam_iter_owned() {
        let m: HashMap<u32, u64> = HashMap::new();
        let m = m.cam_insert(1, 10).cam_insert(2, 20);
        let collected: HashMap<u32, u64> = CambrianMapExt::cam_iter(m.clone()).collect();
        assert_eq!(collected, m);
    }

    // --- Vec extensions ---

    #[test]
    fn vec_cam_contains() {
        let v = vec![1, 2, 3, 4, 5];
        assert!(v.cam_contains(3));
        assert!(!v.cam_contains(6));
    }

    // --- CambrianIter ---

    #[test]
    fn vec_cambrian_iter() {
        let v = vec![10, 20, 30];
        let collected: Vec<i32> = CambrianIter::cam_iter(&v).collect();
        assert_eq!(collected, vec![10, 20, 30]);
    }

    #[test]
    fn hashmap_cambrian_iter() {
        let m: HashMap<u32, u64> = HashMap::new();
        let m = m.cam_insert(1, 10);
        let pairs: Vec<(u32, u64)> = CambrianIter::cam_iter(&m).collect();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0], (1, 10));
    }

    // --- Shift by i32 ---

    #[test]
    fn u256_shl_shr_i32() {
        assert_eq!(U256::ONE << 1i32, U256::from_u128(2));
        assert_eq!(U256::from_u128(4) >> 1i32, U256::from_u128(2));
    }

    #[test]
    #[should_panic(expected = "negative")]
    fn u256_shl_negative_i32_panics() {
        let _ = U256::ONE << -1i32;
    }

    // --- Shift with prim << U256 ---

    #[test]
    fn prim_shl_u256() {
        assert_eq!(1u32 << U256::from_u128(10), U256::from_u128(1024));
        assert_eq!(1u64 << U256::from_u128(0), U256::ONE);
    }

    #[test]
    fn prim_shr_u256() {
        assert_eq!(1024u64 >> U256::from_u128(10), U256::ONE);
    }

    // --- Shr<U256> for U256 ---

    #[test]
    fn u256_shr_by_u256() {
        assert_eq!(U256::from_u128(256) >> U256::from_u128(4), U256::from_u128(16));
        assert_eq!(U256::MAX >> U256::from_u128(256), U256::ZERO);
    }

    // --- PartialEq with u128 ---

    #[test]
    fn u256_partial_eq_u128() {
        assert!(U256::from_u128(42) == 42u128);
        assert!(U256::from_u128(42) != 43u128);
        assert!(U256 { hi: 1, lo: 0 } != 0u128);
    }

    // --- cam_pow edge cases ---

    #[test]
    #[should_panic]
    fn cam_pow_u256_overflow_panics() {
        let _ = cam_pow(U256::from_u128(2), U256::from_u128(256));
    }

    #[test]
    fn cam_pow_base_one_huge_exp() {
        assert_eq!(cam_pow(U256::ONE, U256::MAX), U256::ONE);
    }

    // --- checked ops panics for U256 ---

    #[test]
    #[should_panic(expected = "U256 add overflow")]
    fn cam_add_u256_overflow() {
        cam_add(U256::MAX, U256::ONE);
    }

    #[test]
    #[should_panic(expected = "U256 sub underflow")]
    fn cam_sub_u256_underflow() {
        cam_sub(U256::ZERO, U256::ONE);
    }

    // --- cam_divc edge cases ---

    #[test]
    fn cam_divc_one_over_max() {
        assert_eq!(cam_divc(U256::ONE, U256::MAX), U256::ONE);
    }

    #[test]
    fn cam_divc_exact_division() {
        assert_eq!(cam_divc(U256::from_u128(10), U256::from_u128(5)), U256::from_u128(2));
    }

    // --- cam_divr edge cases ---

    #[test]
    fn cam_divr_exact_division() {
        assert_eq!(cam_divr(U256::from_u128(10), U256::from_u128(5)), U256::from_u128(2));
    }

    #[test]
    fn cam_divr_ties_round_up() {
        assert_eq!(cam_divr(U256::from_u128(5), U256::from_u128(2)), U256::from_u128(3));
    }

    #[test]
    fn cam_divr_zero_numerator() {
        assert_eq!(cam_divr(U256::ZERO, U256::from_u128(5)), U256::ZERO);
    }

    // --- cam_modpow2 ---

    #[test]
    fn cam_modpow2_basic() {
        assert_eq!(cam_modpow2(255, 8), 255);
        assert_eq!(cam_modpow2(256, 8), 0);
        assert_eq!(cam_modpow2(0, 8), 0);
    }

    // --- FP18 near-overflow ---

    #[test]
    fn fp18_near_overflow_muldiv() {
        let large = U256::from_u128(10u128.pow(38));
        let fp18 = U256::from_u128(10u128.pow(18));
        let result = cam_muldiv(large, large, fp18);
        assert_eq!(result, U256::from_u128(10u128.pow(38)) * U256::from_u128(10u128.pow(20)));
    }

    #[test]
    fn fp18_small_value_precision() {
        let tiny = U256::from_u128(1); // 10^-18 in FP18
        let fp18 = U256::from_u128(10u128.pow(18));
        let result = cam_muldiv(tiny, fp18, fp18);
        assert_eq!(result, tiny);
    }

    // ===================================================================
    // S5b-0: U256 literal parse / display SSOT
    // ===================================================================

    const MAX_DECIMAL: &str =
        "115792089237316195423570985008687907853269984665640564039457584007913129639935";

    #[test]
    fn u256_from_decimal_str_max_roundtrip() {
        let v = U256::from_decimal_str(MAX_DECIMAL).expect("MAX decimal");
        assert_eq!(v, U256::MAX);
        assert_eq!(v.to_display_decimal(), MAX_DECIMAL);
    }

    #[test]
    fn u256_from_decimal_str_small_and_underscores() {
        assert_eq!(
            U256::from_decimal_str("1_000").expect("parse"),
            U256::from_u128(1000)
        );
        assert_eq!(
            U256::from_decimal_str("42").expect("parse"),
            U256::from_u128(42)
        );
    }

    #[test]
    fn u256_from_decimal_str_overflow() {
        let over = format!("{}0", MAX_DECIMAL);
        assert_eq!(
            U256::from_decimal_str(&over),
            Err(U256ParseError::Overflow)
        );
    }

    #[test]
    fn u256_from_decimal_str_empty_and_invalid() {
        assert_eq!(U256::from_decimal_str(""), Err(U256ParseError::Empty));
        assert_eq!(
            U256::from_decimal_str("12a3"),
            Err(U256ParseError::InvalidDigit)
        );
    }

    #[test]
    fn u256_from_hex_digits_max_and_partial() {
        assert_eq!(
            U256::from_hex_digits("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF")
                .expect("64 hex"),
            U256::MAX
        );
        assert_eq!(
            U256::from_hex_digits("FF").expect("short hex"),
            U256::from_u128(0xFF)
        );
        assert_eq!(
            U256::from_hex_digits("F_F").expect("underscore hex"),
            U256::from_u128(0xFF)
        );
    }

    #[test]
    fn u256_from_hex_digits_errors() {
        assert_eq!(U256::from_hex_digits(""), Err(U256ParseError::Empty));
        assert_eq!(
            U256::from_hex_digits(&"F".repeat(65)),
            Err(U256ParseError::TooManyDigits)
        );
        assert_eq!(
            U256::from_hex_digits("GG"),
            Err(U256ParseError::InvalidDigit)
        );
    }

    #[test]
    fn u256_from_binary_digits_max_bit() {
        let mut bits = String::from("1");
        bits.extend(std::iter::repeat('0').take(255));
        let v = U256::from_binary_digits(&bits).expect("2^255");
        assert_eq!(v, U256 { hi: 1u128 << 127, lo: 0 });
    }

    #[test]
    fn u256_from_binary_digits_with_prefix() {
        assert_eq!(
            U256::from_binary_digits("0b1010").expect("0b"),
            U256::from_u128(10)
        );
    }

    #[test]
    fn u256_from_binary_digits_errors() {
        assert_eq!(U256::from_binary_digits(""), Err(U256ParseError::Empty));
        assert_eq!(
            U256::from_binary_digits(&"1".repeat(257)),
            Err(U256ParseError::TooManyDigits)
        );
        assert_eq!(
            U256::from_binary_digits("102"),
            Err(U256ParseError::InvalidDigit)
        );
    }

    #[test]
    fn u256_fits_u128() {
        assert!(U256::from_u128(42).fits_u128());
        assert!(!U256::MAX.fits_u128());
        assert!(!U256 { hi: 1, lo: 0 }.fits_u128());
    }

    #[test]
    fn u256_to_display_decimal_wide() {
        let v = U256 { hi: 1, lo: 0 };
        assert_eq!(
            v.to_display_decimal(),
            "340282366920938463463374607431768211456"
        );
    }
}

#[cfg(test)]
mod u256_parse_proptest {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn decimal_roundtrip_u128(v in 0u128..=u128::MAX) {
            let s = v.to_string();
            let parsed = U256::from_decimal_str(&s).expect("decimal");
            prop_assert_eq!(parsed, U256::from_u128(v));
            prop_assert_eq!(parsed.to_display_decimal(), s);
        }

        #[test]
        fn hex_roundtrip_u128(v in 0u128..=u128::MAX) {
            let s = format!("{:x}", v);
            let parsed = U256::from_hex_digits(&s).expect("hex");
            prop_assert_eq!(parsed, U256::from_u128(v));
        }

        #[test]
        fn binary_roundtrip_u8(v in 0u8..=255u8) {
            let s = format!("{:b}", v);
            let parsed = U256::from_binary_digits(&s).expect("bin");
            prop_assert_eq!(parsed, U256::from_u128(v as u128));
        }
    }
}
