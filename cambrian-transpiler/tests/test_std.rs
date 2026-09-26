// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Standard library (`std::`) — parser and validator gates (docs/STDLIB.md).

use cambrian_transpiler::validate::{validate, Severity};

fn parse(src: &str) -> cambrian_transpiler::ast::Program {
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("Parse error: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    program
}

fn errors(src: &str) -> Vec<&'static str> {
    let prog = parse(src);
    validate(&prog)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect()
}

fn parses(src: &str) {
    cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("Parse error: {e}"));
}

// ---------------------------------------------------------------------------
// Parser: std::module::fn(...)
// ---------------------------------------------------------------------------

#[test]
fn std_parse_math_min() {
    parses(
        "pure fn f(a: u64, b: u64) -> u64 { std::math::min(a, b) }\nentity E { m_x: u64 {} }",
    );
}

#[test]
fn std_parse_str_format() {
    parses(
        "pure fn f(n: u64) -> String { std::str::format(\"{:06}\", n) }\nentity E { m_x: u64 {} }",
    );
}

#[test]
fn std_parse_str_parse_u64() {
    parses(
        "pure fn f(s: String) -> Option<u64> { std::str::parse_u64(s, 16) }\nentity E { m_x: u64 {} }",
    );
}

#[test]
fn std_parse_str_parse_int() {
    parses(
        "pure fn f(s: String) -> Option<i64> { std::str::parse_int(s, 10) }\nentity E { m_x: u64 {} }",
    );
}

// ---------------------------------------------------------------------------
// V49: bare stdlib calls
// ---------------------------------------------------------------------------

#[test]
fn v49_bare_min_rejected_in_route() {
    let src = r#"
entity E {
    routes {
        go(a: u64, b: u64) -> u64 => [ return(min(a, b)) ]
    }
    m_x: u64 {}
}
"#;
    assert!(errors(src).contains(&"V49"), "expected V49 for bare min, got {:?}", errors(src));
}

#[test]
fn v49_bare_pow_rejected() {
    let src = r#"
entity E {
    routes {
        go(x: u64, y: u64) -> u64 => [ return(pow(x, y)) ]
    }
    m_x: u64 {}
}
"#;
    assert!(errors(src).contains(&"V49"));
}

#[test]
fn v49_user_pure_fn_min_allowed() {
    let src = r#"
pure fn min(a: u8, b: u8) -> u8 { if a < b { a } else { b } }
entity E {
    routes {
        go(x: u8, y: u8) -> u8 => [ return(min(x, y)) ]
    }
    m_x: u8 {}
}
"#;
    assert!(!errors(src).contains(&"V49"));
}

#[test]
fn v49_std_math_min_allowed() {
    let src = r#"
entity E {
    routes {
        go(a: u64, b: u64) -> u64 => [ return(std::math::min(a, b)) ]
    }
    m_x: u64 {}
}
"#;
    assert!(!errors(src).contains(&"V49"), "got {:?}", errors(src));
}

// ---------------------------------------------------------------------------
// V50: implicit string ↔ numeric without std::str bridge
// ---------------------------------------------------------------------------

#[test]
fn v50_string_expr_to_u64_member_rejected() {
    let src = r#"
pure fn field() -> String { "42" }
entity E {
    routes { go() => [] }
    m_x: u64 { in go() => field() }
}
"#;
    assert!(
        errors(src).contains(&"V50"),
        "expected V50 for String → u64 without parse_uint, got {:?}",
        errors(src)
    );
}

#[test]
fn v50_route_param_string_to_u64_member_rejected() {
    let src = r#"
entity E {
    routes { go(s: String) => [] }
    m_x: u64 { in go(s) => s }
}
"#;
    assert!(
        errors(src).contains(&"V50"),
        "expected V50 for route param String → u64 member, got {:?}",
        errors(src)
    );
}

#[test]
fn v50_u64_literal_to_string_member_rejected() {
    let src = r#"
entity E {
    routes { go() => [] }
    m_label: String { in go() => 42 }
}
"#;
    assert!(
        errors(src).contains(&"V50"),
        "expected V50 for u64 literal → String member without format, got {:?}",
        errors(src)
    );
}

#[test]
fn v50_string_member_ref_to_u64_member_rejected() {
    let src = r#"
entity E {
    routes { go() => [] }
    m_label: String { in constructor() => "x" }
    m_x: u64 { in go() => m_label }
}
"#;
    assert!(
        errors(src).contains(&"V50"),
        "expected V50 for String member → u64 member, got {:?}",
        errors(src)
    );
}

#[test]
fn v50_parse_uint_option_to_u64_member_rejected() {
    let src = r#"
entity E {
    routes { go(s: String) => [] }
    m_x: u64 { in go(s) => std::str::parse_uint(s, 10) }
}
"#;
    assert!(
        errors(src).contains(&"V50"),
        "Option<u64> must not assign to u64 member without unwrap, got {:?}",
        errors(src)
    );
}

#[test]
fn v50_pure_fn_parse_bridge_to_u64_member_allowed() {
    let src = r#"
pure fn parsed(s: String) -> u64 {
    match std::str::parse_uint(s, 10) { some(x) => x, none => 0 }
}
entity E {
    routes { go(s: String) => [] }
    m_x: u64 { in go(s) => parsed(s) }
}
"#;
    assert!(
        !errors(src).contains(&"V50"),
        "pure fn parse bridge should satisfy V50, got {:?}",
        errors(src)
    );
}

#[test]
fn v50_format_to_string_member_allowed() {
    let src = r#"
entity E {
    routes { go(n: u64) => [] }
    m_label: String { in go(n) => std::str::format("{}", n) }
}
"#;
    assert!(
        !errors(src).contains(&"V50"),
        "format bridge should satisfy V50, got {:?}",
        errors(src)
    );
}

#[test]
fn v50_plain_address_to_typed_address_rejected() {
    let src = r#"
extern entity Manager { route ping(); }
entity Migration {
    routes { constructor(manager: address) => [] }
    m_manager: Address<Manager> { in constructor(manager) => manager }
}
"#;
    let prog = parse(src);
    let diags = validate(&prog);
    let v50: Vec<_> = diags.iter().filter(|d| d.code == "V50").collect();
    assert_eq!(v50.len(), 1, "expected one V50, got {:?}", diags);
    assert!(
        v50[0].message.contains("Address<Manager>"),
        "must preserve typed address in diagnostic, got: {}",
        v50[0].message
    );
    assert!(
        !v50[0].message.contains("parse_uint") && !v50[0].message.contains("std::str::format"),
        "typed-address V50 must not recommend string conversion, got: {}",
        v50[0].message
    );
}

#[test]
fn v50_typed_address_to_plain_address_rejected() {
    let src = r#"
extern entity Manager { route ping(); }
entity Migration {
    routes { constructor(manager: Address<Manager>) => [] }
    m_raw: address { in constructor(manager) => manager }
}
"#;
    let prog = parse(src);
    let diags = validate(&prog);
    let v50: Vec<_> = diags.iter().filter(|d| d.code == "V50").collect();
    assert_eq!(v50.len(), 1, "expected one V50, got {:?}", diags);
    assert!(
        v50[0].message.contains("Address<Manager>"),
        "must preserve typed address in diagnostic, got: {}",
        v50[0].message
    );
    assert!(
        !v50[0].message.contains("parse_uint"),
        "typed-address V50 must not recommend parse_uint, got: {}",
        v50[0].message
    );
}

#[test]
fn v50_typed_address_wrong_entity_rejected() {
    let src = r#"
extern entity Manager { route ping(); }
extern entity Vault { route ping(); }
entity Migration {
    routes { constructor(manager: Address<Manager>) => [] }
    m_vault: Address<Vault> { in constructor(manager) => manager }
}
"#;
    let prog = parse(src);
    let diags = validate(&prog);
    let v50: Vec<_> = diags.iter().filter(|d| d.code == "V50").collect();
    assert_eq!(v50.len(), 1, "expected one V50, got {:?}", diags);
    assert!(
        v50[0].message.contains("Address<Manager>") && v50[0].message.contains("Address<Vault>"),
        "must name both entities, got: {}",
        v50[0].message
    );
    assert!(
        !v50[0].message.contains("parse_uint"),
        "typed-address V50 must not recommend parse_uint, got: {}",
        v50[0].message
    );
}
