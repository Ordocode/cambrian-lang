// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase N4 — targeted coverage slices under `src/validate/` (execution tests).

use cambrian_transpiler::target::{Domain, Language, Target};
use cambrian_transpiler::validate::{
    rule_binding, run_rules, RuleBinding, ValidateCtx, RULES,
};

fn parse(src: &str) -> cambrian_transpiler::ast::Program {
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    program
}

// ---------------------------------------------------------------------------
// N4-1: validate/registry.rs — RuleBinding dispatch matrix
// ---------------------------------------------------------------------------

#[test]
fn n4_validate_registry_rule_binding_applies_matrix() {
    let evm_only = RuleBinding::Domains(&[Domain::Evm]);
    assert!(evm_only.applies(Target::Evm));
    assert!(evm_only.applies(Target::Lean));
    assert!(!evm_only.applies(Target::Native));
    assert!(!evm_only.applies(Target::AckiNacki));

    let not_evm = RuleBinding::NotDomains(&[Domain::Evm]);
    assert!(not_evm.applies(Target::Native));
    assert!(not_evm.applies(Target::AckiNacki));
    assert!(!not_evm.applies(Target::Evm));
    assert!(!not_evm.applies(Target::Lean));

    let solidity = RuleBinding::Language(Language::Solidity);
    assert!(solidity.applies(Target::Evm));
    assert!(!solidity.applies(Target::Lean));
    assert!(!solidity.applies(Target::Native));

    let lean_evm_pair = RuleBinding::Pair(Domain::Evm, Language::Lean);
    assert!(lean_evm_pair.applies(Target::Lean));
    assert!(!lean_evm_pair.applies(Target::Evm));
    assert!(!lean_evm_pair.applies(Target::Wasm));

    assert!(RuleBinding::Universal.applies(Target::Wasm));
    assert!(RuleBinding::Targeted.applies(Target::Lean));
}

#[test]
fn n4_validate_registry_universal_only_and_domain_bound_flags() {
    assert!(RuleBinding::Universal.applies_universal_only());
    assert!(!RuleBinding::Targeted.applies_universal_only());
    assert!(!RuleBinding::Domains(&[Domain::Tvm]).applies_universal_only());
    assert!(!RuleBinding::Pair(Domain::Evm, Language::Lean).applies_universal_only());

    assert!(RuleBinding::Universal.is_domain_bound());
    assert!(RuleBinding::Domains(&[Domain::Evm]).is_domain_bound());
    assert!(RuleBinding::NotDomains(&[Domain::Tvm]).is_domain_bound());
    assert!(!RuleBinding::Language(Language::Rust).is_domain_bound());
    assert!(!RuleBinding::Pair(Domain::Evm, Language::Lean).is_domain_bound());
    assert!(!RuleBinding::Targeted.is_domain_bound());
}

#[test]
fn n4_validate_registry_validate_ctx_target_name() {
    let prog = parse("entity E { routes { go() => [] } m_x: u64 { in go() => 0 } }");
    let with = ValidateCtx::with_target(&prog, Target::Evm, true);
    assert_eq!(with.target, Some(Target::Evm));
    assert!(with.deterministic);
    assert_eq!(with.target_name(), "evm");

    let universal = ValidateCtx::universal_only(&prog);
    assert_eq!(universal.target, None);
    assert!(!universal.deterministic);
    assert_eq!(universal.target_name(), "unknown");
}

#[test]
fn n4_validate_registry_rule_binding_lookup_samples() {
    assert!(matches!(rule_binding("V1"), RuleBinding::Universal));
    assert!(matches!(
        rule_binding("I7"),
        RuleBinding::Domains(&[Domain::Tvm])
    ));
    assert!(matches!(
        rule_binding("I18"),
        RuleBinding::Domains(&[Domain::Evm])
    ));
    assert!(matches!(
        rule_binding("L8"),
        RuleBinding::Pair(Domain::Evm, Language::Lean)
    ));
    assert!(matches!(rule_binding("W7"), RuleBinding::Targeted));
    assert!(matches!(rule_binding("E07"), RuleBinding::Language(Language::Solidity)));
    assert!(matches!(rule_binding("I17"), RuleBinding::Language(Language::Solidity)));
}

#[test]
#[should_panic(expected = "no RuleEntry")]
fn n4_validate_registry_rule_binding_unknown_panics() {
    let _ = rule_binding("ZZ99_UNREGISTERED");
}

#[test]
fn n4_validate_registry_run_rules_universal_skips_targeted_binding() {
    let prog = parse(
        r#"
        record Payload { x: u64 }

        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }

        property "unsupported fuzz param" (payload: Payload) for E with { m_x: 0 } {
            call go()
        }
    "#,
    );

    let mut universal = Vec::new();
    run_rules(&ValidateCtx::universal_only(&prog), &mut universal);
    assert!(
        !universal.iter().any(|d| d.code == "W7"),
        "Targeted W7 must not run in universal-only dispatch: {universal:?}"
    );

    let mut evm = Vec::new();
    run_rules(
        &ValidateCtx::with_target(&prog, Target::Evm, false),
        &mut evm,
    );
    assert!(
        evm.iter().any(|d| d.code == "W7"),
        "W7 must run when target is set on EVM fuzz: {evm:?}"
    );
}

#[test]
fn n4_validate_registry_run_rules_domain_binding_i7_on_tvm_only() {
    let prog = parse(
        r#"
        entity Vault {
            routes { deposit(amount: u64) => [] }
            m_balance: u64 { in deposit(amount) => m_balance + amount }
        }

        invariant "multi" for { v: Vault, w: Vault } {
            action v.deposit(amount: u64) { bound amount in 0..10 }
            check v.m_balance >= 0
        }
    "#,
    );

    let mut tvm = Vec::new();
    run_rules(
        &ValidateCtx::with_target(&prog, Target::AckiNacki, false),
        &mut tvm,
    );
    assert!(
        tvm.iter().any(|d| d.code == "I7"),
        "I7 must fire on TVM target via run_rules: {tvm:?}"
    );

    let mut evm = Vec::new();
    run_rules(
        &ValidateCtx::with_target(&prog, Target::Evm, false),
        &mut evm,
    );
    assert!(
        !evm.iter().any(|d| d.code == "I7"),
        "I7 must not fire on EVM target: {evm:?}"
    );
}

#[test]
fn n4_validate_registry_rules_table_non_empty() {
    assert!(!RULES.is_empty());
    assert!(RULES.iter().all(|e| !e.codes.is_empty()));
}

// ---------------------------------------------------------------------------
// N4-2: validate/test_checks.rs — invariant / test / property rule branches
// ---------------------------------------------------------------------------

use cambrian_transpiler::validate::{check_target_compat, validate, Severity};

fn diag_codes(prog: &cambrian_transpiler::ast::Program) -> Vec<String> {
    validate(prog)
        .into_iter()
        .map(|d| d.code.to_string())
        .collect()
}

fn diag_codes_evm(prog: &cambrian_transpiler::ast::Program) -> Vec<String> {
    let mut diags = validate(prog);
    diags.extend(check_target_compat(prog, Target::Evm, false));
    diags.into_iter().map(|d| d.code.to_string()).collect()
}

fn has_code(codes: &[String], code: &str) -> bool {
    codes.iter().any(|c| c == code)
}

#[test]
fn n4_test_checks_invariant_i1_unknown_action_route() {
    let prog = parse(
        r#"
        entity Counter {
            routes { increment(n: u64) => [] }
            m_count: u64 { in increment(n) => m_count + n }
        }
        invariant "i1" for Counter {
            action missingRoute(n: u64) { bound n in 0..10 }
            check m_count >= 0
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "I1"),
        "unknown invariant action route must emit I1"
    );
}

#[test]
fn n4_test_checks_invariant_i2_arity_and_type_mismatch() {
    let prog = parse(
        r#"
        entity Counter {
            routes { increment(n: u64) => [] }
            m_count: u64 { in increment(n) => m_count + n }
        }
        invariant "i2" for Counter {
            action increment(n: u64, extra: u64) { bound n in 0..10 }
            action increment(wrong: u128) { bound wrong in 0..10 }
            check m_count >= 0
        }
    "#,
    );
    let diags = validate(&prog);
    assert!(
        diags.iter().any(|d| d.code == "I2" && d.severity == Severity::Error),
        "arity mismatch must emit I2 error: {diags:?}"
    );
    assert!(
        diags.iter().any(|d| d.code == "I2" && d.severity == Severity::Warning),
        "type mismatch must emit I2 warning: {diags:?}"
    );
}

#[test]
fn n4_test_checks_invariant_i3_non_boolean_check() {
    let prog = parse(
        r#"
        entity Counter {
            routes { increment(n: u64) => [] }
            m_count: u64 { in increment(n) => m_count + n }
        }
        invariant "i3" for Counter {
            action increment(n: u64) { bound n in 0..10 }
            check (1, 2)
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "I3"),
        "tuple literal in check must emit I3"
    );
}

#[test]
fn n4_test_checks_invariant_i4_forbidden_action_body_step() {
    let prog = parse(
        r#"
        entity Counter {
            routes { increment(n: u64) => [] }
            m_count: u64 { in increment(n) => m_count + n }
        }
        invariant "i4" for Counter {
            action increment(n: u64) {
                bound n in 0..10
                call increment(1)
            }
            check m_count >= 0
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "I4"),
        "call inside invariant action body must emit I4"
    );
}

#[test]
fn n4_test_checks_invariant_i5_requires_action_and_check() {
    let no_actions = parse(
        r#"
        entity Counter {
            routes { increment(n: u64) => [] }
            m_count: u64 { in increment(n) => m_count + n }
        }
        invariant "no actions" for Counter {
            check m_count >= 0
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&no_actions), "I5"),
        "invariant without actions must emit I5"
    );

    let no_checks = parse(
        r#"
        entity Counter {
            routes { increment(n: u64) => [] }
            m_count: u64 { in increment(n) => m_count + n }
        }
        invariant "no checks" for Counter {
            action increment(n: u64) { bound n in 0..10 }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&no_checks), "I5"),
        "invariant without checks must emit I5"
    );
}

#[test]
fn n4_test_checks_invariant_i6_senders_must_be_literal() {
    let prog = parse(
        r#"
        entity Counter {
            routes { increment(n: u64) => [] }
            m_count: u64 { in increment(n) => m_count + n }
        }
        invariant "i6" for Counter {
            senders { 1 + 2 }
            action increment(n: u64) { bound n in 0..10 }
            check m_count >= 0
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "I6"),
        "non-literal senders entry must emit I6"
    );
}

#[test]
fn n4_test_checks_invariant_i12_derived_body_only_let() {
    let prog = parse(
        r#"
        entity Counter {
            routes { increment(n: u64) => [] }
            m_count: u64 { in increment(n) => m_count + n }
        }
        invariant "i12" for Counter {
            derived snap() -> u64 {
                let x = m_count
                assume true
                return x
            }
            action increment(n: u64) { bound n in 0..10 }
            check m_count >= 0
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "I12"),
        "derived query with assume must emit I12"
    );
}

#[test]
fn n4_test_checks_invariant_i13_duplicate_track_binding() {
    let prog = parse(
        r#"
        entity Counter {
            routes { increment(n: u64) => [] }
            m_count: u64 { in increment(n) => m_count + n }
        }
        invariant "i13" for Counter {
            track {
                let snap = m_count
                let snap = m_count + 1
            }
            action increment(n: u64) { bound n in 0..10 }
            check m_count >= 0
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "I13"),
        "duplicate track binding must emit I13"
    );
}

#[test]
fn n4_test_checks_invariant_i14_unknown_exclude_selector() {
    let prog = parse(
        r#"
        entity Counter {
            routes { increment(n: u64) => [] reset() => [] }
            m_count: u64 {
                in increment(n) => m_count + n
                in reset() => 0
            }
        }
        invariant "i14" for Counter {
            exclude selectors { ghostRoute }
            action increment(n: u64) { bound n in 0..10 }
            action reset() { }
            check m_count >= 0
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "I14"),
        "unknown exclude selector must emit I14"
    );
}

#[test]
fn n4_test_checks_plain_test_t15_t16_and_t11_t12() {
    let prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        test "illegal steps" for E with { m_x: 0 } {
            skip if true
            advanceTime(10)
            bound n in 0..1
            assume true
            call go()
            expect state { m_x: 0 }
        }
    "#,
    );
    let codes = diag_codes(&prog);
    assert!(has_code(&codes, "T15"), "skip if in plain test must emit T15");
    assert!(has_code(&codes, "T16"), "advanceTime in plain test must emit T16");
    assert!(has_code(&codes, "T11"), "bound in plain test must emit T11");
    assert!(has_code(&codes, "T12"), "assume in plain test must emit T12");
}

#[test]
fn n4_test_checks_property_t15_t16_and_t12_non_boolean() {
    let prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        property "bad body" (n: u64) for E with { m_x: 0 } {
            skip if n > 0
            advanceTime(5)
            assume (1, 2)
            call go()
            test { n: 1 }
        }
    "#,
    );
    let codes = diag_codes(&prog);
    assert!(has_code(&codes, "T15"), "skip if in property must emit T15");
    assert!(has_code(&codes, "T16"), "advanceTime in property must emit T16");
    assert!(has_code(&codes, "T12"), "non-boolean assume in property must emit T12");
}

#[test]
fn n4_test_checks_context_spec_t20_t23_and_sys_aliases() {
    let prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        property "ctx" (n: u64) for E ctx { sys::chain_id: *, sys::blockNumber: *, sys::bogus: 1, sys::now: *, sys::now: 2 } {
            call go()
            test { n: 0 }
        }
    "#,
    );
    let codes = diag_codes(&prog);
    assert!(has_code(&codes, "T20"), "unknown ctx field must emit T20");
    assert!(has_code(&codes, "T23"), "duplicate ctx field must emit T23");
}

#[test]
fn n4_test_checks_emit_w7_invariant_action_params_ackinacki() {
    let prog = parse(
        r#"
        record Payload { x: u64 }
        entity Counter {
            routes { increment(n: u64) => [] }
            m_count: u64 { in increment(n) => m_count + n }
        }
        invariant "w7" for Counter {
            action increment(p: Payload) { bound p in 0..1 }
            check m_count >= 0
        }
    "#,
    );
    let mut diags = Vec::new();
    run_rules(
        &ValidateCtx::with_target(&prog, Target::AckiNacki, false),
        &mut diags,
    );
    assert!(
        diags.iter().any(|d| d.code == "W7"),
        "unsupported invariant action param must emit W7 on Acki Nacki: {diags:?}"
    );
}

#[test]
fn n4_test_checks_fuzz_param_supported_types_no_w7() {
    let prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        property "primitives" (a: u8, b: u64, c: bool, d: address, e: string) for E {
            call go()
            fuzz { a in 0..1, b in 0..1, c in 0..1, d in 0..1, e in 0..1 }
        }
    "#,
    );
    let mut diags = Vec::new();
    run_rules(
        &ValidateCtx::with_target(&prog, Target::Evm, false),
        &mut diags,
    );
    assert!(
        !diags.iter().any(|d| d.code == "W7"),
        "primitive fuzz params should not warn W7: {diags:?}"
    );
}

#[test]
fn n4_test_checks_emit_w7_foundry_label_only() {
    let prog = parse(
        r#"
        record Payload { x: u64 }
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        property "w7 label" (p: Payload) for E {
            call go()
            fuzz { p in 0..1 }
        }
    "#,
    );
    let mut diags = Vec::new();
    run_rules(
        &ValidateCtx::with_target(&prog, Target::Evm, false),
        &mut diags,
    );
    let w7: Vec<_> = diags.iter().filter(|d| d.code == "W7").collect();
    assert!(
        w7.iter().all(|d| !d.message.contains("revm")),
        "EVM W7 must not name the opt-in revm harness: {w7:?}"
    );
    assert!(
        w7.len() == 1 && w7[0].message.contains("Foundry"),
        "EVM W7 must mention Foundry backend: {w7:?}"
    );
}

#[test]
fn n4_test_checks_invariant_i11_complex_expr_and_action_assume() {
    let prog = parse(
        r#"
        entity Vault {
            routes { deposit(amount: u64) => [] }
            m_balance: u64 { in deposit(amount) => m_balance + amount }
        }
        invariant "i11" for { v: Vault, w: Vault } {
            action v.deposit(amount: u64) {
                bound amount in 0..10
                assume m_balance > 0
            }
            check m_balance >= 0
        }
    "#,
    );
    let codes = diag_codes(&prog);
    assert!(
        codes.iter().filter(|c| *c == "I11").count() >= 2,
        "bare member refs in multi-instance assume/check must emit I11: {codes:?}"
    );
}

#[test]
fn n4_test_checks_invariant_i16_trace_in_track_and_advance_time() {
    let prog = parse(
        r#"
        entity Counter {
            routes { increment(n: u64) => [] }
            m_count: u64 { in increment(n) => m_count + n }
        }
        invariant "i16 track" for Counter {
            track { let t = trace::length }
            action increment(n: u64) {
                bound n in 0..10
                advanceTime(trace::length)
            }
            check m_count >= 0
        }
    "#,
    );
    let codes = diag_codes(&prog);
    assert!(
        codes.iter().filter(|c| *c == "I16").count() >= 2,
        "trace:: in track and advanceTime must emit I16: {codes:?}"
    );
}

#[test]
fn n4_test_checks_invariant_i15_trace_length_unknown_field() {
    let prog = parse(
        r#"
        entity Counter {
            routes { increment(n: u64) => [] }
            m_count: u64 { in increment(n) => m_count + n }
        }
        invariant "i15 field" for Counter {
            action increment(n: u64) {
                bound n in 0..10
                assume trace::bogus >= 0
            }
            check m_count >= 0
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "I15"),
        "trace::field other than length must emit I15"
    );
}

// ---------------------------------------------------------------------------
// N4-8: validate/test_checks.rs — multi-entity invariant I8–I10
// ---------------------------------------------------------------------------

#[test]
fn n4_test_checks_invariant_i8_duplicate_instance_names() {
    let prog = parse(
        r#"
        entity Vault {
            routes { deposit(amount: u64) => [] }
            m_balance: u64 { in deposit(amount) => m_balance + amount }
        }

        invariant "dup instances" for { v: Vault, v: Vault } {
            action v.deposit(amount: u64) { bound amount in 0..10 }
            check v.m_balance >= 0
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "I8"),
        "duplicate instance names in multi-entity invariant must emit I8"
    );
}

#[test]
fn n4_test_checks_invariant_i9_unknown_instance_entity() {
    let prog = parse(
        r#"
        entity Vault {
            routes { deposit(amount: u64) => [] }
            m_balance: u64 { in deposit(amount) => m_balance + amount }
        }

        invariant "unknown entity" for { v: Vault, t: NoSuch } {
            action v.deposit(amount: u64) { bound amount in 0..10 }
            check v.m_balance >= 0
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "I9"),
        "unknown entity on instance declaration must emit I9"
    );
}

#[test]
fn n4_test_checks_invariant_i9_unknown_action_instance() {
    let prog = parse(
        r#"
        entity Vault {
            routes { deposit(amount: u64) => [] }
            m_balance: u64 { in deposit(amount) => m_balance + amount }
        }
        entity Treasury {
            routes { sweep(amount: u64) => [] }
            m_reserve: u64 { in sweep(amount) => m_reserve + amount }
        }

        invariant "unknown action instance" for { v: Vault, t: Treasury } {
            action ghost.deposit(amount: u64) { bound amount in 0..10 }
            check v.m_balance >= 0
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "I9"),
        "action referencing undeclared instance must emit I9"
    );
}

#[test]
fn n4_test_checks_invariant_i18_duplicate_identity_tuple() {
    let prog = parse(
        r#"
        entity Vault {
            routes { deposit(amount: u64) => [] }
            identity m_vault_id: u64
            m_balance: u64 { in deposit(amount) => m_balance + amount }
        }

        invariant "dup id" for { v: Vault, a: Vault } {
            init v { m_vault_id: 7, m_balance: 0 }
            init a { m_vault_id: 7, m_balance: 0 }
            action v.deposit(amount: u64) { bound amount in 0..10 }
            check v.m_balance >= 0
        }
    "#,
    );
    assert!(
        has_code(&diag_codes_evm(&prog), "I18"),
        "duplicate identity tuple must emit I18 on EVM domain"
    );
}

#[test]
fn n4_test_checks_invariant_i18_identity_less_duplicate_entity() {
    let prog = parse(
        r#"
        entity Vault {
            routes { deposit(amount: u64) => [] }
            m_balance: u64 { in deposit(amount) => m_balance + amount }
        }

        invariant "no identity" for { v: Vault, a: Vault } {
            init v { m_balance: 0 }
            init a { m_balance: 0 }
            action v.deposit(amount: u64) { bound amount in 0..10 }
            check v.m_balance >= 0
        }
    "#,
    );
    assert!(
        has_code(&diag_codes_evm(&prog), "I18"),
        "two instances of identity-less entity must emit I18 on EVM domain"
    );
}

#[test]
fn n4_test_checks_invariant_i10_init_unknown_member() {
    let prog = parse(
        r#"
        entity Vault {
            routes { deposit(amount: u64) => [] }
            m_balance: u64 { in deposit(amount) => m_balance + amount }
        }

        invariant "bad init key" for { v: Vault, w: Vault } {
            init v { not_a_member: 0 }
            action v.deposit(amount: u64) { bound amount in 0..10 }
            check v.m_balance >= 0
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "I10"),
        "init key that is not an entity member must emit I10 in multi-entity form"
    );
}

#[test]
fn n4_test_checks_property_duplicate_instance_binding_t11() {
    let prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        property "dup bind" (n: u64) for E {
            call go()
            test "t" { n: 1, n: 2 }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "T11"),
        "duplicate instance binding must emit T11"
    );
}

#[test]
fn n4_test_checks_property_w8_forall_unpinned_test_instance() {
    let prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        property "forall warn" (n: u64) for E with { m_x: * } {
            call go()
            test "t" { n: 0 }
        }
    "#,
    );
    assert!(
        diag_codes_evm(&prog).iter().any(|c| c == "W8"),
        "unpinned forall field in concrete test must warn W8"
    );
}

// ---------------------------------------------------------------------------
// N4-3: validate/entity.rs — route/member/rescue/extern rule branches
// ---------------------------------------------------------------------------

fn diag_codes_with_severity(prog: &cambrian_transpiler::ast::Program) -> Vec<(String, Severity)> {
    validate(prog)
        .into_iter()
        .map(|d| (d.code.to_string(), d.severity))
        .collect()
}

fn has_warning(codes: &[(String, Severity)], code: &str) -> bool {
    codes
        .iter()
        .any(|(c, s)| c == code && *s == Severity::Warning)
}

#[test]
fn n4_entity_v2_empty_route_without_member_transforms() {
    let prog = parse(
        r#"
        entity E {
            routes {
                idle() => []
                go() => []
            }
            m_x: u64 { in go() => 0 }
        }
    "#,
    );
    assert!(
        has_warning(&diag_codes_with_severity(&prog), "V2"),
        "empty route without transforms must warn V2"
    );
}

#[test]
fn n4_entity_w4_deprecated_on_deploy_bounce_route() {
    let prog = parse(
        r#"
        entity E {
            routes {
                onDeployBounce(body: CamData) => []
            }
            m_x: u64 {}
        }
    "#,
    );
    assert!(
        has_warning(&diag_codes_with_severity(&prog), "W4"),
        "legacy onDeployBounce route must warn W4"
    );
}

#[test]
fn n4_entity_call_route_action_is_not_w6() {
    let prog = parse(
        r#"
        entity E {
            routes {
                go() => [ call onUpgrade() ]
                onUpgrade() => []
            }
            m_x: u64 { in onUpgrade() => 0 }
        }
    "#,
    );
    assert!(
        !has_warning(&diag_codes_with_severity(&prog), "W6"),
        "`call` to a route is a supported in-contract call, not a deprecated form"
    );
}

#[test]
fn n4_entity_v8_orphan_rescue_without_recover_handler() {
    let prog = parse(
        r#"
        entity E {
            routes {
                go(dest: address) => [
                    rescue orphan_tag: ~> dest
                ]
            }
            m_x: u64 {}
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "V8"),
        "rescue tag without recover handler must emit V8"
    );
}

#[test]
fn n4_entity_v8_orphan_recover_without_rescue_action() {
    let prog = parse(
        r#"
        entity E {
            routes {
                recover orphan_tag(body: CamData) => []
                go() => []
            }
            m_x: u64 { in go() => 0 }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "V8"),
        "recover handler without matching rescue must emit V8"
    );
}

#[test]
fn n4_entity_v8_on_bounce_references_unknown_route() {
    let prog = parse(
        r#"
        entity E {
            routes {
                onBounce ghost(body: CamData) => []
            }
            m_x: u64 {}
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "V8"),
        "onBounce tag for unknown route must emit V8"
    );
}

#[test]
fn n4_entity_v43_temporal_in_route_where_clause() {
    let prog = parse(
        r#"
        entity E {
            routes {
                go() where (^m_x > 0) : throw 1 => []
            }
            m_x: u64 { in go() => 0 }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "V43"),
        "temporal ref in route where must emit V43"
    );
}

#[test]
fn n4_entity_v43_temporal_in_from_clause_args() {
    let prog = parse(
        r#"
        entity E {
            routes {
                go() from Self(^m_x) => []
            }
            m_x: u64 { in go() => 0 }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "V43"),
        "temporal ref in from clause must emit V43"
    );
}

#[test]
fn n4_entity_v43_temporal_in_member_default() {
    let prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 = ^m_y { in go() => 0 }
            m_y: u64 { in go() => 0 }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "V43"),
        "temporal ref in member default must emit V43"
    );
}

#[test]
fn n4_entity_v43_temporal_in_phased_where() {
    let prog = parse(
        r#"
        entity E {
            routes {
                go() => [
                    setup: []
                    work where (^m_x > 0) : throw 1: []
                ]
            }
            m_x: u64 { in go() => 0 }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "V43"),
        "temporal ref in per-phase where must emit V43"
    );
}

#[test]
fn n4_entity_v45_distinct_deploy_args_warn() {
    let prog = parse(
        r#"
        entity Child {
            identity id: u64
            routes { constructor() => [] }
            m_x: u64 { in constructor() => id }
        }
        entity Factory {
            routes {
                spawn(id: u64) => [
                    deploy Child(id)
                    deploy Child(id + 1)
                ]
            }
            m_y: u64 {}
        }
    "#,
    );
    assert!(
        has_warning(&diag_codes_with_severity(&prog), "V45"),
        "distinct duplicate deploy args must warn V45"
    );
}

#[test]
fn n4_entity_v45_identical_deploy_in_rescue_branch() {
    let prog = parse(
        r#"
        entity Child {
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 0 }
        }
        entity Factory {
            routes {
                spawn() => [
                    deploy Child()
                    rescue retry: deploy Child()
                ]
            }
            m_y: u64 {}
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "V45"),
        "identical deploy in main and rescue branch must emit V45"
    );
}

#[test]
fn n4_entity_v26_update_code_not_terminal_in_conditional() {
    let prog = parse(
        r#"
        use gosh
        entity E {
            routes {
                go(use_upgrade: bool) => [
                    if use_upgrade => [
                        gosh::updateCode(0x01, 0x02) with onUpgrade()
                        gosh::commit()
                    ] else [
                        gosh::commit()
                    ]
                ]
                private onUpgrade() => []
            }
            m_x: u64 { in onUpgrade() => 0 }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "V26"),
        "actions after updateCode inside conditional must emit V26"
    );
}

#[test]
fn n4_entity_v20_empty_string_compared_to_address_member() {
    let prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_owner: address {
                in go() => { if m_owner == "" { m_owner } else { msg::sender } }
            }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "V20"),
        "empty string compared to address member must emit V20"
    );
}

#[test]
fn n4_entity_v24_var_call_extern_entity_void_route() {
    let prog = parse(
        r#"
        extern entity Remote {
            route ping();
        }
        entity E {
            routes {
                constructor(r: Address<Remote>) => []
                go() => [
                    var x = ping() ~> m_r;
                ]
            }
            m_r: Address<Remote> { in constructor(r) => r }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "V24"),
        "var call to void extern route must emit V24"
    );
}

// ---------------------------------------------------------------------------
// N4-4: validate/test_checks.rs — plain `test` T1–T9 + property T6/T17–T19
// ---------------------------------------------------------------------------

#[test]
fn n4_test_checks_plain_t1_unknown_entity() {
    let prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        test "missing entity" for Ghost {
            call go()
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "T1"),
        "plain test for unknown entity must emit T1"
    );
}

#[test]
fn n4_test_checks_plain_t2_unknown_route() {
    let prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        test "missing route" for E {
            call missing()
            expect state { m_x: 0 }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "T2"),
        "plain test calling unknown route must emit T2"
    );
}

#[test]
fn n4_test_checks_plain_t3_wrong_call_arity() {
    let prog = parse(
        r#"
        entity E {
            routes { go(n: u64) => [] }
            m_x: u64 { in go(n) => n }
        }
        test "bad arity" for E {
            call go()
            expect state { m_x: 0 }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "T3"),
        "plain test with wrong call arity must emit T3"
    );
}

#[test]
fn n4_test_checks_plain_t4_unknown_init_and_expect_members() {
    let init_prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        test "bad init" for E with { m_unknown: 0 } {
            call go()
            expect state { m_x: 0 }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&init_prog), "T4"),
        "unknown member in test init must emit T4"
    );

    let expect_prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        test "bad expect" for E {
            call go()
            expect state { m_unknown: 0 }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&expect_prog), "T4"),
        "unknown member in expect state must emit T4"
    );
}

#[test]
fn n4_test_checks_plain_t5_expect_before_call_variants() {
    let prog = parse(
        r#"
        entity E {
            routes {
                go() -> u64 => [ return(m_x) ]
            }
            m_x: u64 { in go() => 0 }
        }
        test "expects first" for E with { m_x: 0 } {
            expect state { m_x: 0 }
            expect throw 1
            expect return 0
            expect effects []
            call go()
        }
    "#,
    );
    let codes = diag_codes(&prog);
    assert!(
        codes.iter().filter(|c| *c == "T5").count() >= 4,
        "expect steps before call must emit T5 for each variant: {codes:?}"
    );
}

#[test]
fn n4_test_checks_plain_t6_test_without_call() {
    let prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        test "no call" for E {
            expect state { m_x: 0 }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "T6"),
        "plain test without call must emit T6"
    );
}

#[test]
fn n4_test_checks_plain_t8_unknown_context_namespace() {
    let prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        test "bad ctx ns" for E {
            bogus { field: 1 }
            call go()
            expect state { m_x: 0 }
        }
    "#,
    );
    assert!(
        diag_codes_with_severity(&prog)
            .iter()
            .any(|(c, s)| c == "T8" && *s == Severity::Warning),
        "unknown context namespace in plain test must warn T8"
    );
}

#[test]
fn n4_test_checks_plain_t9_unknown_registry_entity() {
    let prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        test "bad registry" for E {
            registry Ghost {
                code_hash: 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa,
                code_depth: 1,
                wasm_hash: 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
            }
            call go()
            expect state { m_x: 0 }
        }
    "#,
    );
    assert!(
        diag_codes_with_severity(&prog)
            .iter()
            .any(|(c, s)| c == "T9" && *s == Severity::Warning),
        "unknown registry entity in plain test must warn T9"
    );
}

#[test]
fn n4_test_checks_property_t1_unknown_entity() {
    let prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        property "ghost" (n: u64) for Ghost {
            call go()
            test { n: 0 }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "T1"),
        "property for unknown entity must emit T1"
    );
}

#[test]
fn n4_test_checks_property_t6_missing_call() {
    let prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        property "no call" (n: u64) for E with { m_x: 0 } {
            test { n: 0 }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "T6"),
        "property without call must emit T6"
    );
}

#[test]
fn n4_test_checks_property_t17_instance_binding_shape() {
    let prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        property "shape" (n: u64) for E {
            call go()
            test { n in 0..1 }
            fuzz { n: 1 }
        }
    "#,
    );
    let codes = diag_codes(&prog);
    assert!(has_code(&codes, "T17"), "range in test instance must emit T17");
    assert!(
        codes.iter().filter(|c| *c == "T17").count() >= 2,
        "concrete binding in fuzz instance must also emit T17: {codes:?}"
    );
}

#[test]
fn n4_test_checks_property_t18_test_missing_param_binding() {
    let prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        property "unbound" (a: u64, b: u64) for E {
            call go()
            test "partial" { a: 1 }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "T18"),
        "test instance missing parameter binding must emit T18"
    );
}

#[test]
fn n4_test_checks_property_t19_forall_non_member_field() {
    let prog = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
        property "bad forall" (n: u64) for E with { m_x: 0, ghost: * } {
            call go()
            test { n: 0 }
        }
    "#,
    );
    assert!(
        has_code(&diag_codes(&prog), "T19"),
        "forall over non-member field must emit T19"
    );
}
