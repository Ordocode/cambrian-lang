// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! `std::str::parse_*` — Option semantics, radix + bit-width fit (docs/STDLIB.md §4.1).

use cambrian_transpiler::validate::{validate, Severity};

fn parse(src: &str) -> cambrian_transpiler::ast::Program {
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("Parse error: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    program
}

fn blocking_codes(src: &str) -> Vec<&'static str> {
    let prog = parse(src);
    validate(&prog)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect()
}

fn parses_pure(src: &str) {
    cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("Parse error: {e}"));
}

struct ParseCase {
    id: &'static str,
    /// `some(expected)` or `none` encoded as expected value + is_none flag
    expected: Option<u64>,
    lit: &'static str,
    radix: u64,
    fn_name: &'static str,
}

fn run_parse_case(case: &ParseCase) {
    let src = format!(
        "pure fn unwrap(opt: Option<u64>) -> u64 {{ match opt {{ some(v) => v, none => 0 }} }}\n\
         pure fn probe() -> u64 {{ unwrap(std::str::{}(\"{}\", {})) }}\n\
         entity E {{ m_x: u64 {{}} }}",
        case.fn_name, case.lit, case.radix
    );
    assert!(
        blocking_codes(&src).is_empty(),
        "{}: validator blocked {:?}",
        case.id,
        blocking_codes(&src)
    );
    parses_pure(&src);
}

#[test]
fn parse_u64_decimal_ok() {
    run_parse_case(&ParseCase {
        id: "u64_dec_123",
        expected: Some(123),
        lit: "123",
        radix: 10,
        fn_name: "parse_u64",
    });
}

#[test]
fn parse_u64_hex_ff() {
    run_parse_case(&ParseCase {
        id: "u64_hex_ff",
        expected: Some(255),
        lit: "ff",
        radix: 16,
        fn_name: "parse_u64",
    });
}

#[test]
fn parse_u64_hex_0x_prefix() {
    run_parse_case(&ParseCase {
        id: "u64_0x_ff",
        expected: Some(255),
        lit: "0xff",
        radix: 16,
        fn_name: "parse_u64",
    });
}

#[test]
fn parse_uint_alias() {
    parses_pure(
        "pure fn f(s: String) -> Option<u64> { std::str::parse_uint(s, 10) }\nentity E { m_x: u64 {} }",
    );
}

#[test]
fn parse_int_alias() {
    parses_pure(
        "pure fn f(s: String) -> Option<i64> { std::str::parse_int(s, 10) }\nentity E { m_x: u64 {} }",
    );
}

#[test]
fn parse_all_unsigned_wrappers_parse() {
    for name in ["parse_u8", "parse_u16", "parse_u32", "parse_u64", "parse_u128", "parse_U256"] {
        let src = format!(
            "pure fn f(s: String) -> Option<u64> {{ std::str::{}(s, 10) }}\nentity E {{ m_x: u64 {{}} }}",
            name
        );
        parses_pure(&src);
    }
}

#[test]
fn parse_all_signed_wrappers_parse() {
    for name in ["parse_i8", "parse_i16", "parse_i32", "parse_i64", "parse_i128"] {
        let src = format!(
            "pure fn f(s: String) -> Option<i64> {{ std::str::{}(s, 10) }}\nentity E {{ m_x: u64 {{}} }}",
            name
        );
        parses_pure(&src);
    }
}

#[test]
fn parse_u8_overflow_is_option_none_route() {
    let src = r#"
pure fn is_none(opt: Option<u8>) -> bool {
    match opt { some(_) => false, none => true }
}
pure fn probe() -> bool { is_none(std::str::parse_u8("100", 16)) }
entity E { m_x: u64 {} }
"#;
    assert!(blocking_codes(src).is_empty());
}

#[test]
fn parse_u8_max_ok() {
    let src = r#"
pure fn unwrap(opt: Option<u8>) -> u64 {
    match opt { some(v) => v, none => 0 }
}
pure fn probe() -> u64 { unwrap(std::str::parse_u8("255", 10)) }
entity E { m_x: u64 {} }
"#;
    assert!(blocking_codes(src).is_empty());
}

#[test]
fn parse_u8_256_none() {
    let src = r#"
pure fn is_none(opt: Option<u8>) -> bool {
    match opt { some(_) => false, none => true }
}
pure fn probe() -> bool { is_none(std::str::parse_u8("256", 10)) }
entity E { m_x: u64 {} }
"#;
    assert!(blocking_codes(src).is_empty());
}

#[test]
fn parse_invalid_digit_none() {
    let src = r#"
pure fn is_none(opt: Option<u64>) -> bool {
    match opt { some(_) => false, none => true }
}
pure fn probe() -> bool { is_none(std::str::parse_u64("12a3", 10)) }
entity E { m_x: u64 {} }
"#;
    assert!(blocking_codes(src).is_empty());
}

#[test]
fn parse_empty_string_none() {
    let src = r#"
pure fn is_none(opt: Option<u64>) -> bool {
    match opt { some(_) => false, none => true }
}
pure fn probe() -> bool { is_none(std::str::parse_u64("", 10)) }
entity E { m_x: u64 {} }
"#;
    assert!(blocking_codes(src).is_empty());
}

#[test]
fn parse_i8_negative_ok() {
    let src = r#"
pure fn unwrap(opt: Option<i8>) -> i64 {
    match opt { some(v) => v, none => 0 }
}
pure fn probe() -> i64 { unwrap(std::str::parse_i8("-128", 10)) }
entity E { m_x: u64 {} }
"#;
    assert!(blocking_codes(src).is_empty());
}

#[test]
fn parse_i8_underflow_none() {
    let src = r#"
pure fn is_none(opt: Option<i8>) -> bool {
    match opt { some(_) => false, none => true }
}
pure fn probe() -> bool { is_none(std::str::parse_i8("-129", 10)) }
entity E { m_x: u64 {} }
"#;
    assert!(blocking_codes(src).is_empty());
}

#[test]
fn parse_radix_2_ok() {
    let src = r#"
pure fn unwrap(opt: Option<u8>) -> u64 {
    match opt { some(v) => v, none => 0 }
}
pure fn probe() -> u64 { unwrap(std::str::parse_u8("11111111", 2)) }
entity E { m_x: u64 {} }
"#;
    assert!(blocking_codes(src).is_empty());
}

#[test]
fn parse_0x_with_wrong_radix_none() {
    let src = r#"
pure fn is_none(opt: Option<u64>) -> bool {
    match opt { some(_) => false, none => true }
}
pure fn probe() -> bool { is_none(std::str::parse_u64("0xff", 10)) }
entity E { m_x: u64 {} }
"#;
    assert!(blocking_codes(src).is_empty());
}

fn none_probe_unsigned(fn_name: &str, lit: &str, radix: u64) {
    let src = format!(
        r#"
pure fn is_none(opt: Option<u64>) -> bool {{
    match opt {{ some(_) => false, none => true }}
}}
pure fn probe() -> bool {{ is_none(std::str::{}("{}", {})) }}
entity E {{ m_x: u64 {{}} }}
"#,
        fn_name, lit, radix
    );
    assert!(blocking_codes(&src).is_empty(), "none probe blocked: {}", fn_name);
}

fn none_probe_signed(fn_name: &str, lit: &str, radix: u64) {
    let src = format!(
        r#"
pure fn is_none(opt: Option<i64>) -> bool {{
    match opt {{ some(_) => false, none => true }}
}}
pure fn probe() -> bool {{ is_none(std::str::{}("{}", {})) }}
entity E {{ m_x: u64 {{}} }}
"#,
        fn_name, lit, radix
    );
    assert!(blocking_codes(&src).is_empty(), "none probe blocked: {}", fn_name);
}

fn unwrap_unsigned(fn_name: &str, lit: &str, radix: u64) {
    let src = format!(
        r#"
pure fn unwrap(opt: Option<u64>) -> u64 {{
    match opt {{ some(v) => v, none => 0 }}
}}
pure fn probe() -> u64 {{ unwrap(std::str::{}("{}", {})) }}
entity E {{ m_x: u64 {{}} }}
"#,
        fn_name, lit, radix
    );
    assert!(blocking_codes(&src).is_empty(), "unwrap blocked: {}", fn_name);
}

fn unwrap_signed(fn_name: &str, lit: &str, radix: u64) {
    let src = format!(
        r#"
pure fn unwrap(opt: Option<i64>) -> i64 {{
    match opt {{ some(v) => v, none => 0 }}
}}
pure fn probe() -> i64 {{ unwrap(std::str::{}("{}", {})) }}
entity E {{ m_x: u64 {{}} }}
"#,
        fn_name, lit, radix
    );
    assert!(blocking_codes(&src).is_empty(), "unwrap blocked: {}", fn_name);
}

#[test]
fn parse_unsigned_width_edges_validate() {
    none_probe_unsigned("parse_u8", "256", 10);
    unwrap_unsigned("parse_u8", "255", 10);
    unwrap_unsigned("parse_u8", "0", 10);
    none_probe_unsigned("parse_u16", "65536", 10);
    unwrap_unsigned("parse_u16", "65535", 10);
    none_probe_unsigned("parse_u32", "4294967296", 10);
    unwrap_unsigned("parse_u32", "4294967295", 10);
    none_probe_unsigned("parse_u64", "18446744073709551616", 10);
    unwrap_unsigned("parse_u64", "18446744073709551615", 10);
    unwrap_unsigned("parse_u128", "18446744073709551615", 10);
    unwrap_unsigned("parse_U256", "18446744073709551615", 10);
}

#[test]
fn parse_signed_width_edges_validate() {
    unwrap_signed("parse_i8", "-128", 10);
    unwrap_signed("parse_i8", "127", 10);
    none_probe_signed("parse_i8", "-129", 10);
    none_probe_signed("parse_i8", "128", 10);
    unwrap_signed("parse_i16", "-32768", 10);
    unwrap_signed("parse_i16", "32767", 10);
    none_probe_signed("parse_i16", "-32769", 10);
    none_probe_signed("parse_i16", "32768", 10);
    unwrap_signed("parse_i32", "-2147483648", 10);
    unwrap_signed("parse_i32", "2147483647", 10);
    none_probe_signed("parse_i32", "-2147483649", 10);
    unwrap_signed("parse_i64", "-9223372036854775808", 10);
    unwrap_signed("parse_i64", "9223372036854775807", 10);
    unwrap_signed("parse_i128", "-9223372036854775808", 10);
}

#[test]
fn parse_radix_edges_validate() {
    unwrap_unsigned("parse_u8", "11111111", 2);
    unwrap_unsigned("parse_u8", "z", 36);
    none_probe_unsigned("parse_u64", "10", 1);
    none_probe_unsigned("parse_u64", "10", 37);
    unwrap_unsigned("parse_u64", "10", 0);
    unwrap_unsigned("parse_u64", "0xff", 16);
    unwrap_unsigned("parse_u64", "0XFF", 16);
}

#[test]
fn parse_rejects_bad_input_validate() {
    none_probe_unsigned("parse_u64", "", 10);
    none_probe_unsigned("parse_u64", "12a3", 10);
    none_probe_unsigned("parse_u64", "0xff", 10);
    none_probe_unsigned("parse_u64", "++1", 10);
    none_probe_signed("parse_i64", "--1", 10);
    none_probe_signed("parse_i64", "1-", 10);
}

#[test]
fn parse_alias_match_validate() {
    unwrap_unsigned("parse_uint", "42", 10);
    unwrap_signed("parse_int", "-42", 10);
}

#[test]
fn parse_route_body_match_validate() {
    let src = r#"
entity E {
    routes {
        constructor() => []
        probe() -> u64 => [
            let v = match std::str::parse_u64("42", 10) { some(x) => x, none => 0 };
            return(v)
        ]
    }
    m_x: u64 { in constructor() => 0 }
}
"#;
    assert!(blocking_codes(src).is_empty());
}

