// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! B7 escrow name-validator contract tests (CAMBRIAN_R3_SPEC §7 item B7).
//!
//! One negative case per rule (N1–N6), plus the gating contract (escrow-only
//! rules stay silent under the default profile; the `__cbr_` reservation fires
//! unconditionally) and positive smokes proving the rules do not reject the
//! shapes real corpora use. Mirrors the `test_validate.rs` style: inline
//! `.cam` sources, assert on diagnostic codes.

use cambrian_transpiler::validate::{check_reserved_names, Severity};

fn parse(src: &str) -> cambrian_transpiler::ast::Program {
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("Parse error: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    program
}

fn codes(src: &str, predictable: bool) -> Vec<&'static str> {
    let prog = parse(src);
    check_reserved_names(&prog, predictable)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect()
}

fn has(src: &str, predictable: bool, code: &str) -> bool {
    codes(src, predictable).contains(&code)
}

fn no_codes(src: &str, predictable: bool) {
    let c = codes(src, predictable);
    assert!(c.is_empty(), "Expected no name diagnostics, got: {c:?}");
}

// ===================================================================
// N1 — entity name ∉ {Cambrian} ∪ core roots
// ===================================================================

#[test]
fn n1_entity_named_core_root_rejected() {
    // `List` is a Lean core root the model references — the zone root would
    // shadow it.
    assert!(has(
        "entity List { routes { go() => [] } m_x: u64 { in go() => 0 } }",
        true,
        "N1",
    ));
}

#[test]
fn n1_entity_named_cambrian_rejected() {
    assert!(has(
        "entity Cambrian { routes { go() => [] } m_x: u64 { in go() => 0 } }",
        true,
        "N1",
    ));
}

// ===================================================================
// N2 — component-wise prefix freedom of zone roots
// ===================================================================

#[test]
fn n2_entity_shares_name_with_top_level_type_rejected() {
    // Entity `Token` and top-level record `Token` mint the same zone root.
    assert!(has(
        "record Token { x: u64 }\n\
         entity Token { routes { go() => [] } m_x: u64 { in go() => 0 } }",
        true,
        "N2",
    ));
}

#[test]
fn n2_component_wise_not_string_prefix() {
    // Regression guard: `ERC20` and `ERC20Votes` are DIFFERENT single
    // components — NOT a prefix relationship — so the real governor/uniswap
    // shape must pass (spec §2: `Foo` ≠ prefix `FooBar`).
    no_codes(
        "entity ERC20 { routes { go() => [] } m_x: u64 { in go() => 0 } }\n\
         entity ERC20Votes { routes { go() => [] } m_y: u64 { in go() => 0 } }",
        true,
    );
}

// ===================================================================
// N3 — reserved `statement` leaf
// ===================================================================

#[test]
fn n3_member_named_statement_rejected() {
    assert!(has(
        "entity E { routes { go() => [] } statement: u64 { in go() => 0 } }",
        true,
        "N3",
    ));
}

// ===================================================================
// N4 — reserved `__cbr_` binder prefix (UNCONDITIONAL)
// ===================================================================

#[test]
fn n4_member_with_cbr_prefix_rejected() {
    assert!(has(
        "entity E { routes { go() => [] } __cbr_foo: u64 { in go() => 0 } }",
        true,
        "N4",
    ));
}

#[test]
fn n4_fires_under_legacy_profile_too() {
    // N4 is upstream-safe / unconditional: it must fire even without escrow.
    assert!(has(
        "entity E { routes { go() => [] } __cbr_foo: u64 { in go() => 0 } }",
        false,
        "N4",
    ));
}

// ===================================================================
// N5 — reserved generated names as entity member / type / constant
// ===================================================================

#[test]
fn n5_constant_named_spec_rejected() {
    assert!(has(
        "entity E { const Spec: u64 = 0 m_x: u64 {} }",
        true,
        "N5",
    ));
}

#[test]
fn n5_member_named_state_rejected() {
    assert!(has(
        "entity E { routes { go() => [] } State: u64 { in go() => 0 } }",
        true,
        "N5",
    ));
}

// ===================================================================
// N6 — top-level type name ∉ {Cambrian} ∪ core roots ∪ generated names
// ===================================================================

#[test]
fn n6_top_level_type_named_core_root_rejected() {
    assert!(has(
        "record List { x: u64 }\n\
         entity E { routes { go() => [] } m_x: u64 { in go() => 0 } }",
        true,
        "N6",
    ));
}

#[test]
fn n6_top_level_type_named_generated_name_rejected() {
    assert!(has(
        "record Members { x: u64 }\n\
         entity E { routes { go() => [] } m_x: u64 { in go() => 0 } }",
        true,
        "N6",
    ));
}

// ===================================================================
// Gating — escrow-only rules stay silent under legacy
// ===================================================================

#[test]
fn escrow_rules_silent_under_legacy() {
    // Entity named a core root, member `statement`, constant `Spec`, top-level
    // type `List` — all escrow-gated, so NONE fire under the default profile.
    no_codes(
        "entity List { routes { go() => [] } statement: u64 { in go() => 0 } }",
        false,
    );
    no_codes("entity E { const Spec: u64 = 0 m_x: u64 {} }", false);
    no_codes(
        "record List { x: u64 }\n\
         entity E { routes { go() => [] } m_x: u64 { in go() => 0 } }",
        false,
    );
}

// ===================================================================
// Positive — real corpus shapes validate under predictable profile
// ===================================================================

#[test]
fn positive_counter_shape_ok() {
    no_codes(
        "entity Counter {\n\
             routes {\n\
                 increment(amount: u64) => []\n\
                 getCount() -> u64 => [ return(m_count) ]\n\
             }\n\
             m_count: u64 { in increment(amount) => m_count + amount }\n\
         }",
        true,
    );
}

#[test]
fn positive_complex_program_shape_ok() {
    // The `valid_complex_entity` shape from test_validate.rs: top-level record
    // / enum / type-alias + entity with const / macro / members — none of the
    // names are reserved, so escrow validation is clean.
    no_codes(
        r#"
pure fn helper(x: u64, y: u64) -> u64 { x + y }

record Data { value: u64, flag: bool }

enum Status { Active, Done }

type Balance = u128

entity Complex {
    const MAX: u64 = 100

    routes {
        setup(val: u64) => []
        view get_val() -> u64 => [return(m_value)]
    }

    m_value: u64 {
        in setup(val) => val
    }

    m_status: Status {
        in setup(_) => Status::Active
    }
}
"#,
        true,
    );
}

#[test]
fn positive_governor_top_level_enums_ok() {
    // Governor's real top-level enums (OpState, ProposalState) alongside
    // multiple entities — distinct, non-reserved names — must pass.
    no_codes(
        "enum OpState { Unset, Pending, Done }\n\
         enum ProposalState { Pending, Active, Succeeded }\n\
         entity Governor { routes { go() => [] } m_x: u64 { in go() => 0 } }\n\
         entity TimelockController { routes { go() => [] } m_y: u64 { in go() => 0 } }",
        true,
    );
}
