// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! SMAFD Run #5 lean toolchain debt — regression pins for LG-006 / LG-007.
//! (The LG-004 bounds-sidecar pin lives in `test_lg010_assume_bounds.rs`.)
//!
//! LG-006: invariants with `claim`/`burn` actions ship `:= by sorry` (LG-006b);
//! simple invariants keep `invByCases` ladder with heartbeat-bounded rung + `sorry`
//! fallback (LG-006c — no `try` on rung, parity with LG-007b).
//!
//! LG-007: `test` theorems must use heartbeat-bounded, `try`-wrapped proof ladder
//! on simple statements; nested multi-match / throw expects use theorem-level
//! `:= by sorry` (LG-007b — SMAFD Run #5 handoff).

use cambrian_transpiler::ast;
use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::ProgramParser;

fn parse(source: &str) -> ast::Program {
    let mut p = ProgramParser::new()
        .parse(source)
        .expect("test fixture must parse");
    ast::normalize_program_types(&mut p);
    p
}

fn extras_for(source: &str, entity: &str) -> std::collections::HashMap<String, String> {
    let backend = LeanBackend::default();
    let program = parse(source);
    backend.extra_files(&program, entity).into_iter().collect()
}

/// Multi-step integration-style test (mirrors Run #5 `*.integration.test.cam`).
const LG007_MULTI_STEP_TEST: &str = r#"
entity RehearsalToken {
    routes {
        mint(amt: u64) => []
        burn(amt: u64) => []
    }

    m_balance: u64 {
        in mint(amt) => m_balance + amt
        in burn(amt) => m_balance - amt
    }
}

test "mint then burn" for RehearsalToken with { m_balance: 0 } {
    call mint(100)
    call burn(40)
    expect state { m_balance: 60 }
}
"#;

/// Nested `match` + `expect throw` — reproduces LG-007b class B (proof glue).
const LG007B_NESTED_THROW_TEST: &str = r#"
entity Token {
    routes {
        ok() => []
        fail() where (false) : throw 1 => []
    }

    m_x: u64 {
        in ok() => m_x + 1
    }
}

test "nested revert" for Token with { m_x: 0 } {
    call ok()
    call fail()
    expect throw 1
}
"#;

/// Transfer-only invariant — keeps `invByCases` ladder (lg-001 class).
const LG006_SIMPLE_INVARIANT: &str = r#"
entity Token {
    routes {
        constructor() => []
        transfer(to: address, amount: u64) => []
        approve(spender: address, amount: u64) => []
        view totalSupply() -> u64 => [ return(m_supply) ]
    }

    m_supply: u64 {
        in constructor() => 1000
    }
}

invariant "simple supply" for Token {
    init { m_supply: 1000 }

    action transfer(to: address, amount: u64) { }
    action approve(spender: address, amount: u64) { }

    check totalSupply() == 1000
}
"#;

/// `claim` action — Run #5 class; must not run `invByCases` (simp maxRecDepth).
const LG006_CLAIM_INVARIANT: &str = r#"
entity Token {
    routes {
        constructor() => []
        claim() => []
        view totalSupply() -> u64 => [ return(m_supply) ]
    }

    m_supply: u64 {
        in constructor() => 1000
    }
}

invariant "claim supply" for Token {
    init { m_supply: 1000 }

    action claim() { }

    check totalSupply() == 1000
}
"#;

/// `burn` action — same LG-006b policy as `claim`.
const LG006_BURN_INVARIANT: &str = r#"
entity Token {
    routes {
        constructor() => []
        burn(amount: u64) => []
        view totalSupply() -> u64 => [ return(m_supply) ]
    }

    m_supply: u64 {
        in constructor() => 1000
        in burn(amount) => m_supply - amount
    }
}

invariant "burn supply ceiling" for Token {
    init { m_supply: 1000 }

    action burn(amount: u64) { }

    check totalSupply() <= 1000
}
"#;

fn invariants_section<'a>(spec: &'a str, entity: &str) -> &'a str {
    spec.split(&format!("namespace {entity}.Spec.Invariants"))
        .nth(1)
        .and_then(|s| s.split(&format!("end {entity}.Spec.Invariants")).next())
        .unwrap_or_else(|| panic!("Invariants namespace block for {entity}"))
}

fn invariant_namespace<'a>(spec: &'a str, slug: &str) -> &'a str {
    spec.split(&format!("namespace {slug}"))
        .nth(1)
        .and_then(|s| s.split(&format!("end {slug}")).next())
        .unwrap_or_else(|| panic!("invariant namespace {slug}"))
}

fn tests_section<'a>(spec: &'a str, entity: &str) -> &'a str {
    spec.split(&format!("namespace {entity}.Spec.Tests"))
        .nth(1)
        .and_then(|s| s.split(&format!("end {entity}.Spec.Tests")).next())
        .unwrap_or_else(|| panic!("Tests namespace block for {entity}"))
}

#[test]
fn smafd_lg007_test_proof_ladder_try_and_heartbeats() {
    let extras = extras_for(LG007_MULTI_STEP_TEST, "RehearsalToken");
    let spec = extras
        .get("Cambrian/Generated/RehearsalTokenSpec.lean")
        .expect("RehearsalTokenSpec.lean must be generated");

    let tests_section = tests_section(spec, "RehearsalToken");

    assert!(
        tests_section.contains("set_option maxHeartbeats 40000"),
        "simple test ladder must heartbeat-bound the cheap simp rung (LG-007):\n{tests_section}",
    );
    assert!(
        tests_section.contains("contextual := true"),
        "simple test ladder must use contextual simp:\n{tests_section}",
    );
    assert!(
        tests_section.contains("__e0\n:= by\n"),
        "proof must follow goal immediately without blank line (LG-007b):\n{tests_section}",
    );
    assert!(
        !tests_section.contains("| (simp [cambrian_route_simp"),
        "bare simp rung must not appear in tests (LG-007):\n{tests_section}",
    );
}

#[test]
fn smafd_lg007b_nested_throw_theorem_level_sorry() {
    let extras = extras_for(LG007B_NESTED_THROW_TEST, "Token");
    let spec = extras
        .get("Cambrian/Generated/TokenSpec.lean")
        .expect("TokenSpec.lean must be generated");

    let tests_section = tests_section(spec, "Token");

    assert!(
        tests_section.contains(":= by sorry\n"),
        "nested match + expect throw must ship sorry-only proof (LG-007b):\n{tests_section}",
    );
    assert!(
        !tests_section.contains("False := by\n  first"),
        "proof ladder must not dedent under nested match arm (LG-007b):\n{tests_section}",
    );
    assert!(
        !tests_section.contains("n = 1 := by"),
        "throw-code arm must not glue := by onto goal line (LG-007b):\n{tests_section}",
    );
}

#[test]
fn smafd_lg006_simple_invariant_keeps_ladder() {
    let extras = extras_for(LG006_SIMPLE_INVARIANT, "Token");
    let spec = extras
        .get("Cambrian/Generated/TokenSpec.lean")
        .expect("TokenSpec.lean must be generated");

    let inv = invariants_section(spec, "Token");
    let ns = invariant_namespace(inv, "simple_supply");

    assert!(
        ns.contains("scoped macro \"invByCases\""),
        "simple invariant must emit invByCases (LG-006):\n{ns}",
    );
    assert!(
        ns.contains("(set_option maxHeartbeats 80000 in (invByCases; done))"),
        "simple invariant ladder must heartbeat-bound invByCases (LG-006c):\n{ns}",
    );
    assert!(
        !ns.contains("| try (set_option maxHeartbeats 80000 in (invByCases; done))"),
        "invByCases rung must not be try-wrapped (LG-006c / LG-007b parity):\n{ns}",
    );
    assert!(
        ns.contains("| sorry"),
        "simple invariant must retain sorry fallback:\n{ns}",
    );
}

#[test]
fn smafd_lg006_claim_invariant_sorry_stub() {
    let extras = extras_for(LG006_CLAIM_INVARIANT, "Token");
    let spec = extras
        .get("Cambrian/Generated/TokenSpec.lean")
        .expect("TokenSpec.lean must be generated");

    let inv = invariants_section(spec, "Token");
    let ns = invariant_namespace(inv, "claim_supply");

    assert!(
        ns.contains(":= by sorry\n"),
        "claim invariant must ship sorry-only proof (LG-006b):\n{ns}",
    );
    assert!(
        !ns.contains("invByCases"),
        "claim invariant must not emit invByCases macro (LG-006b):\n{ns}",
    );
}

#[test]
fn smafd_lg006_burn_invariant_sorry_stub() {
    let extras = extras_for(LG006_BURN_INVARIANT, "Token");
    let spec = extras
        .get("Cambrian/Generated/TokenSpec.lean")
        .expect("TokenSpec.lean must be generated");

    let inv = invariants_section(spec, "Token");
    let ns = invariant_namespace(inv, "burn_supply_ceiling");

    assert!(
        ns.contains(":= by sorry\n"),
        "burn invariant must ship sorry-only proof (LG-006b):\n{ns}",
    );
    assert!(
        !ns.contains("invByCases"),
        "burn invariant must not emit invByCases macro (LG-006b):\n{ns}",
    );
}
