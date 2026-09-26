// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! CAM-H-02 — `#[instantiates]` validation (T25–T33, T37).

use cambrian_transpiler::validate::{
    catalog_pre_merge_check, check_catalog_links, check_instantiates_goal_divergence,
    check_noop_instantiates, validate, Severity,
};

const ENTITY: &str = r"
entity E {
    routes {
        go() => []
        view totalSupply() -> u64 => [ return(m_x) ]
    }
    m_x: u64 { in go() => 0 }
}
";

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

fn link_errors(src: &str) -> Vec<&'static str> {
    let prog = parse(src);
    let mut diags = check_catalog_links(&prog);
    diags.extend(check_instantiates_goal_divergence(&prog));
    diags
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect()
}

fn link_warnings(src: &str) -> Vec<&'static str> {
    let prog = parse(src);
    check_noop_instantiates(&prog)
        .into_iter()
        .map(|d| d.code)
        .collect()
}

#[test]
fn valid_test_instantiates_property() {
    let src = format!(
        r#"{ENTITY}
property "PROP-1 base" () for E {{
    call go()
    expect state {{ m_x: 0 }}
}}
#[instantiates("PROP-1")]
test "linked pins only" for E {{
}}
"#
    );
    assert!(link_errors(&src).is_empty());
}

#[test]
fn t37_linked_test_body_overrides_catalog() {
    let src = format!(
        r#"{ENTITY}
property "PROP-1 base" () for E {{
    call go()
    expect state {{ m_x: 0 }}
}}
#[instantiates("PROP-1")]
test "bad override" for E {{
    call go()
    expect state {{ m_x: 1 }}
}}
"#
    );
    assert!(link_errors(&src).contains(&"T37"));
    assert!(errors(&src).contains(&"T37"));
}

#[test]
fn t29_missing_stem() {
    let src = format!(
        r#"{ENTITY}
#[instantiates("MISSING")]
test "linked" for E {{
    call go()
}}
"#
    );
    assert!(link_errors(&src).contains(&"T29"));
}

#[test]
fn t28_property_param_mismatch() {
    let src = format!(
        r#"{ENTITY}
property "PROP-1 base" (amount: u64) for E {{
    assume amount > 0
    call go()
}}
#[instantiates("PROP-1")]
property "CP-1 narrow" () for E {{
    call go()
}}
"#
    );
    assert!(link_errors(&src).contains(&"T28"));
}

#[test]
fn t33_test_links_invariant_not_property() {
    let src = format!(
        r#"{ENTITY}
invariant "INV-1 fixed" for E {{
    action go() {{ }}
    check m_x == 0
}}
#[instantiates("INV-1")]
test "bad link" for E {{
    call go()
}}
"#
    );
    assert!(link_errors(&src).contains(&"T33"));
}

#[test]
fn w12_empty_linked_property_shell() {
    let src = format!(
        r#"{ENTITY}
property "PROP-1 base" (amount: u64) for E {{
    assume amount > 0
    call go()
}}
#[instantiates("PROP-1")]
property "CP-1 empty shell" () for E {{
}}
"#
    );
    assert!(link_warnings(&src).contains(&"W12"));
}

#[test]
fn w12_absent_when_property_has_assume_delta() {
    let src = format!(
        r#"{ENTITY}
property "PROP-1 base" (amount: u64) for E {{
    assume amount > 0
    call go()
}}
#[instantiates("PROP-1")]
property "CP-1 narrow" (amount: u64) for E {{
    assume amount <= 10
}}
"#
    );
    assert!(link_warnings(&src).is_empty());
}

#[test]
fn w12_absent_for_invariant_fanout_overlay() {
    let src = format!(
        r#"{ENTITY}
invariant "INV-1 fixed" for E {{
    action go() {{ }}
    check m_x == 0
}}
invariant "CP-2 overlay only" #[instantiates("INV-1")] for E {{
}}
"#
    );
    assert!(link_warnings(&src).is_empty());
}

#[test]
fn catalog_pre_merge_check_collects_w12() {
    let src = format!(
        r#"{ENTITY}
property "PROP-1 base" () for E {{
    call go()
}}
#[instantiates("PROP-1")]
property "CP-1 shell" () for E {{
}}
"#
    );
    let prog = parse(&src);
    let codes: Vec<_> = catalog_pre_merge_check(&prog)
        .iter()
        .map(|d| d.code)
        .collect();
    assert!(codes.contains(&"W12"));
}

#[test]
fn invariant_instantiates_before_keyword_parses() {
    let src = format!(
        r#"{ENTITY}
invariant "INV-1 fixed" for E {{
    action go() {{ }}
    check m_x == 0
}}
#[instantiates("INV-1")]
invariant "CP-2 overlay" for E {{
}}
"#
    );
    let prog = parse(&src);
    let inv = prog
        .invariants
        .iter()
        .find(|i| i.name.contains("CP-2"))
        .expect("overlay invariant");
    assert_eq!(inv.instantiates.as_deref(), Some("INV-1"));
}

#[test]
fn parse_and_pretty_roundtrip_instantiates() {
    let src = format!(
        r#"{ENTITY}
property "PROP-1 base" () for E {{
    call go()
}}
#[instantiates("PROP-1")]
test "linked" for E {{
    call go()
}}
"#
    );
    let prog = parse(&src);
    let printed = cambrian_transpiler::pretty::pretty_print(&prog);
    assert!(printed.contains("#[instantiates(\"PROP-1\")]"));
}
