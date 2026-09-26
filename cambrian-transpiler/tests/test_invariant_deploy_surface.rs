// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! P0 — invariant `deploy { }` surface (parse, merge, validation).

use cambrian_transpiler::ast::Expr;
use cambrian_transpiler::merge_catalog::merge_catalog_instantiations;
use cambrian_transpiler::validate::validate;
use cambrian_core::U256;

const ENTITY: &str = r"
entity Token {
    routes {
        constructor(mint_to: address) => []
        burn(amount: u64) => []
    }
    m_total_supply: u64 {
        in constructor(mint_to) => 1_000_000
        in burn(amount) => m_total_supply - amount
    }
    m_balances: HashMap<address, u64> { in constructor(mint_to) => HashMap::insert(m_balances, mint_to, 1_000_000) }
}
";

fn parse(src: &str) -> cambrian_transpiler::ast::Program {
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("Parse error: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    program
}

fn codes(src: &str) -> Vec<String> {
    let program = parse(src);
    let diags = validate(&program);
    diags.into_iter().map(|d| d.code.to_string()).collect()
}

#[test]
fn deploy_block_parses_in_single_entity_invariant() {
    let src = format!(
        r#"{ENTITY}
invariant "deploy surface" for Token {{
    deploy {{ 0x0000000000000000000000000000000000000000000000000000000000000d01 }}
    senders {{
        0x0000000000000000000000000000000000000000000000000000000000000a01,
        0x0000000000000000000000000000000000000000000000000000000000000a02
    }}
    action burn(amount: u64) {{ bound amount in 0..1000 }}
    check m_total_supply <= 1_000_000
}}
"#
    );
    let program = parse(&src);
    let inv = &program.invariants[0];
    assert_eq!(inv.deploy.len(), 1);
    assert!(matches!(inv.deploy[0], Expr::IntLiteral(v) if v == U256::from(0xd01u64)));
    assert_eq!(inv.senders.len(), 2);
}

#[test]
fn deploy_block_rejects_multiple_addresses_v58() {
    let src = format!(
        r#"{ENTITY}
invariant "too many deployers" for Token {{
    deploy {{
        0x0000000000000000000000000000000000000000000000000000000000000d01,
        0x0000000000000000000000000000000000000000000000000000000000000d02
    }}
    action burn(amount: u64) {{ bound amount in 0..1 }}
    check m_total_supply <= 1_000_000
}}
"#
    );
    assert!(codes(&src).contains(&"V58".to_string()));
}

#[test]
fn deploy_block_forbidden_on_system_invariant_v59() {
    let src = r#"
entity A { routes { go() => [] } m_x: u64 { in go() => 0 } }
entity B { routes { go() => [] } m_y: u64 { in go() => 0 } }
invariant "system deploy" for { a: A, b: B } {
    deploy { 0x0000000000000000000000000000000000000000000000000000000000000d01 }
    action a.go() { }
    action b.go() { }
    check a.m_x + b.m_y >= 0
}
"#;
    assert!(codes(src).contains(&"V59".to_string()));
}

#[test]
fn ctor_bootstrap_without_deploy_emits_w13() {
    let src = format!(
        r#"{ENTITY}
invariant "legacy bootstrap" for Token {{
    senders {{ 0x0000000000000000000000000000000000000000000000000000000000000a01 }}
    action burn(amount: u64) {{ bound amount in 0..1 }}
    check m_total_supply <= 1_000_000
}}
"#
    );
    assert!(codes(&src).contains(&"W13".to_string()));
}

#[test]
fn explicit_deploy_suppresses_w13() {
    let src = format!(
        r#"{ENTITY}
invariant "explicit deploy" for Token {{
    deploy {{ 0x0000000000000000000000000000000000000000000000000000000000000d01 }}
    senders {{ 0x0000000000000000000000000000000000000000000000000000000000000a01 }}
    action burn(amount: u64) {{ bound amount in 0..1 }}
    check m_total_supply <= 1_000_000
}}
"#
    );
    assert!(!codes(&src).contains(&"W13".to_string()));
}

#[test]
#[test]
fn w15_illposed_member_eq_const_with_decreasing_action() {
    let src = format!(
        r#"{ENTITY}
invariant "ill-posed eq" for Token {{
    action burn(amount: u64) {{ bound amount in 1..10 }}
    check m_total_supply == 1_000_000
}}
"#
    );
    assert!(codes(&src).contains(&"W15".to_string()));
}

#[test]
fn w15_silent_when_action_does_not_decrease_checked_member() {
    let src = r#"
entity T {
    routes {
        constructor() => []
        transfer(to: address, amount: u64) => []
    }
    m_total_supply: u64 { in constructor() => 100 }
    m_aux: u64 { in constructor() => 0 in transfer(to, amount) => m_aux + amount }
}
invariant "transfer does not move supply" for T {
    action transfer(to: address, amount: u64) { bound amount in 1..10 }
    check m_total_supply == 100
}
"#;
    assert!(!codes(src).contains(&"W15".to_string()));
}

#[test]
fn w15_silent_on_view_supply_sum_equality() {
    let src = format!(
        r#"{ENTITY}
invariant "sum tracks supply" for Token {{
    action burn(amount: u64) {{ bound amount in 1..10 }}
    check m_total_supply == m_total_supply
}}
"#
    );
    assert!(!codes(&src).contains(&"W15".to_string()));
}

#[test]
fn empty_supplemental_init_clears_catalog_pins() {
    let catalog_src = format!(
        r#"{ENTITY}
invariant "INV base" for Token {{
    init {{ m_total_supply: 1_000_000 }}
    action burn(amount: u64) {{ bound amount in 0..1 }}
    check m_total_supply <= 1_000_000
}}
"#
    );
    let overlay_src = r#"
invariant "CP genesis only" #[instantiates("INV")] for Token {
    init { }
}
"#;
    let mut program = parse(&catalog_src);
    let overlay = parse(overlay_src);
    program.invariants.extend(overlay.invariants);
    merge_catalog_instantiations(&mut program);
    let merged = program
        .invariants
        .iter()
        .find(|i| i.name == "CP genesis only")
        .expect("merged supplemental invariant");
    assert!(merged.instances[0].init.is_empty());
    assert!(merged.instances[0].init_specified);
}

fn merge_invariant_deploy_overlay_replaces_catalog() {
    let catalog_src = format!(
        r#"{ENTITY}
invariant "INV base" for Token {{
    deploy {{ 0x0000000000000000000000000000000000000000000000000000000000000d00 }}
    action burn(amount: u64) {{ bound amount in 0..1 }}
    check m_total_supply <= 1_000_000
}}
"#
    );
    let overlay_src = r#"
invariant "CP overlay" #[instantiates("INV")] for Token {
    deploy { 0x0000000000000000000000000000000000000000000000000000000000000d01 }
}
"#;
    let mut program = parse(&catalog_src);
    let overlay = parse(overlay_src);
    program.invariants.extend(overlay.invariants);
    merge_catalog_instantiations(&mut program);
    let merged = program
        .invariants
        .iter()
        .find(|i| i.name == "CP overlay")
        .expect("merged supplemental invariant");
    assert_eq!(merged.deploy.len(), 1);
    assert!(matches!(
        merged.deploy[0],
        Expr::IntLiteral(v) if v == U256::from(0xd01u64)
    ));
}
