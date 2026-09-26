// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! SD-01 — EVM predicate lowering unified with route/pure-fn ComputeExpr.
//!
//! Plan: `docs/plans/sd01-predicate-pure-unification.md`

use cambrian_transpiler::ast;
use cambrian_transpiler::codegen::OutputBackend;
use cambrian_transpiler::ProgramParser;

const SD01_INLINE_FOLD: &str = r#"
entity Tiny {
    routes {
        constructor() => []
        view totalSupply() -> u128 => [ return(m_supply) ]
    }
    m_supply: u128 { in constructor() => 1000 }
    m_bal: HashMap<address, u128> {
        in constructor() => HashMap::new()
    }
}

invariant "balance supply" for Tiny {
    action constructor() { }
    check m_bal.keys().fold(0, |acc, k| acc + m_bal[k]) == totalSupply()
}
"#;

const SD01_INFER_VIEW: &str = r#"
entity Tiny {
    routes {
        constructor() => []
        totalSupply() -> u128 => [ return(m_supply) ]
    }
    m_supply: u128 { in constructor() => 1000 }
}

invariant "supply positive" for Tiny {
    action constructor() { }
    check totalSupply() > 0
}
"#;

const SD01_LIBRARY_FOLD: &str = r#"
library TokenSpec {
    pure fn sum_balances(m: HashMap<address, u128>) -> u128 {
        m.keys().fold(0, |acc, k| acc + m[k])
    }
}

entity Tiny {
    routes {
        constructor() => []
        view totalSupply() -> u128 => [ return(m_supply) ]
    }
    m_supply: u128 { in constructor() => 1000 }
    m_bal: HashMap<address, u128> {
        in constructor() => HashMap::new()
    }
}

invariant "balance supply lib" for Tiny {
    action constructor() { }
    check TokenSpec::sum_balances(m_bal) == totalSupply()
}
"#;

fn parse(source: &str) -> ast::Program {
    let mut p = ProgramParser::new()
        .parse(source)
        .expect("fixture must parse");
    ast::normalize_program_types(&mut p);
    p
}

const HARNESS_PROJECT_SOL: &str = "src/_Harness_project.sol";

fn evm_files(source: &str) -> std::collections::HashMap<String, String> {
    use cambrian_transpiler::codegen::evm_test_codegen::generate_evm_tests;
    use cambrian_transpiler::project::InvariantConfig;

    let program = parse(source);
    let mut files = std::collections::HashMap::new();
    for (path, content) in generate_evm_tests(&program, true, &InvariantConfig::default()) {
        files.insert(path, content);
    }
    files
}

/// Entity bodies live in the combined harness project file (U4-6 Step 11 stubs).
fn entity_sol(files: &std::collections::HashMap<String, String>) -> String {
    files
        .get(HARNESS_PROJECT_SOL)
        .cloned()
        .unwrap_or_else(|| {
            let keys: Vec<_> = files.keys().collect();
            panic!("harness project sol missing; keys={keys:?}")
        })
}

fn lean_spec(source: &str) -> String {
    use cambrian_transpiler::codegen::LeanBackend;
    use cambrian_transpiler::project::Project;

    let dir = std::env::temp_dir().join(format!(
        "cambrian-sd01-lean-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tiny.cam"), source).unwrap();
    std::fs::write(
        dir.join("project.yaml"),
        "name: tiny\ntarget: evm\nsources:\n  - tiny.cam\n",
    )
    .unwrap();
    let project = Project::load(&dir.join("project.yaml")).unwrap();
    let files: std::collections::HashMap<String, String> = LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    files
        .get("Cambrian/Generated/TinySpec.lean")
        .cloned()
        .unwrap_or_else(|| format!("keys: {:?}", files.keys().collect::<Vec<_>>()))
}

fn invariant_test_sol(files: &std::collections::HashMap<String, String>, slug: &str) -> String {
    files
        .iter()
        .find(|(k, _)| k.contains("Invariant") && k.contains(slug))
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| {
            let keys: Vec<_> = files.keys().collect();
            panic!("invariant test not found for {slug}; keys={keys:?}")
        })
}

#[test]
fn sd01_inline_fold_check_not_require_true() {
    let files = evm_files(SD01_INLINE_FOLD);
    let entity = entity_sol(&files);
    assert!(
        entity.contains("function _cam_inv_balance_supply_0()"),
        "entity must emit SD-01 predicate helper:\n{entity}"
    );
    let inv = invariant_test_sol(&files, "balance_supply");
    assert!(
        !inv.contains("require(true"),
        "must not silently pass vacuous invariant (SD-01):\n{inv}"
    );
    assert!(
        inv.contains("_cam_inv_balance_supply_0"),
        "invariant must call entity-side helper:\n{inv}"
    );
}

#[test]
fn sd01_lean_inline_fold_spec_pinned() {
    let spec = lean_spec(SD01_INLINE_FOLD);
    assert!(
        spec.contains("foldl") || spec.contains("List.map"),
        "Lean must lower fold in check (baseline):\n{spec}"
    );
}

#[test]
fn sd01_infer_view_on_readonly_route() {
    let files = evm_files(SD01_INFER_VIEW);
    let entity = entity_sol(&files);
    assert!(
        entity.contains("function totalSupply() external view"),
        "read-only route without `view` must infer view (SD-01a):\n{entity}"
    );
    let inv = invariant_test_sol(&files, "supply_positive");
    assert!(
        inv.contains("totalSupply()"),
        "invariant must call inferred-view route:\n{inv}"
    );
}

#[test]
fn sd01_library_fold_check_not_require_true() {
    let files = evm_files(SD01_LIBRARY_FOLD);
    let inv = invariant_test_sol(&files, "balance_supply_lib");
    assert!(
        inv.contains("TokenSpec.sum_balances"),
        "invariant must call library fn with keys sidecar:\n{inv}"
    );
    assert!(
        !inv.contains("require(true"),
        "library predicate must not be vacuous:\n{inv}"
    );
}

const SD01_MULTI_TRACE: &str = r#"
entity VaultA {
    routes {
        constructor() => []
        deposit(amount: u64) => []
    }
    m_balance: u64 {
        in constructor() => 0
        in deposit(amount) => m_balance + amount
    }
}

entity VaultB {
    routes {
        constructor() => []
        deposit(amount: u64) => []
    }
    m_balance: u64 {
        in constructor() => 0
        in deposit(amount) => m_balance + amount
    }
}

invariant "dual vault trace" for { va: VaultA, vb: VaultB } {
    action va.deposit(amount: u64) { }
    action vb.deposit(amount: u64) { }
    check va.m_balance + vb.m_balance >= 0
    check trace::length <= 64
}
"#;

const SD01_UNSUPPORTED: &str = r#"
entity Tiny {
    routes {
        constructor() => []
    }
    m_x: u64 { in constructor() => 0 }
}

invariant "bad pred" for Tiny {
    action constructor() { }
    check 0 == (|x| x)
}
"#;

const SD01_MULTI_LIB_FOLD: &str = r#"
library TokenSpec {
    pure fn sum_balances(m: HashMap<address, u128>) -> u128 {
        m.keys().fold(0, |acc, k| acc + m[k])
    }
}

entity Tok {
    routes {
        constructor() => []
        view totalSupply() -> u128 => [ return(m_supply) ]
    }
    m_supply: u128 { in constructor() => 1000 }
    m_bal: HashMap<address, u128> {
        in constructor() => HashMap::new()
    }
}

entity Vault {
    routes {
        constructor() => []
        ping() => []
    }
    m_n: u128 { in constructor() => 0 }
}

invariant "tok supply" for { tok: Tok, v: Vault } {
    action tok.constructor() { }
    action v.ping() { }
    check TokenSpec::sum_balances(tok.m_bal) == tok.totalSupply()
}
"#;

const SD01_LIB_COLLISION: &str = r#"
library TokenSpec {
    pure fn sum_balances(m: HashMap<address, u128>) -> u128 {
        m.keys().fold(0, |acc, k| acc + m[k])
    }
}

library OtherSpec {
    pure fn sum_balances(m: HashMap<u64, u128>) -> u128 {
        m.keys().fold(0, |acc, k| acc + m[k])
    }
}

entity Tiny {
    routes {
        constructor() => []
        view total() -> u128 => [
            return(TokenSpec::sum_balances(m_bal) + OtherSpec::sum_balances(m_nums))
        ]
    }
    m_bal: HashMap<address, u128> { in constructor() => HashMap::new() }
    m_nums: HashMap<u64, u128> { in constructor() => HashMap::new() }
}
"#;

#[test]
fn sd01_multi_entity_trace_length_is_inline_not_helper() {
    use cambrian_transpiler::codegen::gen_evm_solidity;
    use cambrian_transpiler::target::Target;
    use cambrian_transpiler::validate::check_target_compat;

    let program = parse(SD01_MULTI_TRACE);
    let diags = check_target_compat(&program, Target::Evm, true);
    assert!(
        !diags.iter().any(|d| d.code == "I17"),
        "trace::length must stay Inline, not I17: {diags:?}"
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        !sol.contains("_traceLen"),
        "trace counters must not be emitted on the entity helper:\n{sol}"
    );
    assert!(
        !sol.contains("function _cam_inv_dual_vault_trace_1("),
        "trace::length check must not become an entity helper:\n{sol}"
    );
}

#[test]
fn sd01_unsupported_check_emits_i17_not_panic() {
    use cambrian_transpiler::target::Target;
    use cambrian_transpiler::validate::check_target_compat;

    let program = parse(SD01_UNSUPPORTED);
    let evm = check_target_compat(&program, Target::Evm, false);
    assert!(
        evm.iter().any(|d| d.code == "I17"),
        "unsupported check must emit I17 on EVM: {evm:?}"
    );
    let lean = check_target_compat(&program, Target::Lean, false);
    assert!(
        !lean.iter().any(|d| d.code == "I17"),
        "I17 is Language Solidity, must not fire on Lean: {lean:?}"
    );
}

#[test]
fn sd01_multi_entity_library_fold_helper_on_owner() {
    use cambrian_transpiler::codegen::evm_test_codegen::generate_evm_tests;
    use cambrian_transpiler::codegen::gen_evm_solidity;
    use cambrian_transpiler::project::InvariantConfig;

    let program = parse(SD01_MULTI_LIB_FOLD);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function _cam_inv_tok_supply_0()"),
        "Tok must emit owned helper:\n{sol}"
    );
    let mut files = std::collections::HashMap::new();
    for (path, content) in generate_evm_tests(&program, true, &InvariantConfig::default()) {
        files.insert(path, content);
    }
    let inv = invariant_test_sol(&files, "tok_supply");
    assert!(
        inv.contains("_tok._cam_inv_tok_supply_0"),
        "Foundry multi must call helper on tok instance:\n{inv}"
    );
    assert!(
        !inv.contains("require(true"),
        "multi-entity library fold must not be vacuous:\n{inv}"
    );
}

#[test]
fn sd01_two_libraries_same_fn_name_distinct_keys() {
    use cambrian_transpiler::codegen::gen_evm_solidity;

    let program = parse(SD01_LIB_COLLISION);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("TokenSpec.sum_balances(m_bal, m_bal_keys)"),
        "TokenSpec call must weave address-map keys:\n{sol}"
    );
    assert!(
        sol.contains("OtherSpec.sum_balances(m_nums, m_nums_keys)"),
        "OtherSpec call must weave u64-map keys:\n{sol}"
    );
    assert!(
        !sol.contains("TokenSpec.sum_balances(m_nums")
            && !sol.contains("OtherSpec.sum_balances(m_bal"),
        "sidecars must not cross libraries:\n{sol}"
    );
}

const HASHMAP_INDEX_MULTI: &str = r#"
entity Ledger {
    routes {
        set(k: u64, v: u64) => []
    }
    m_vals: HashMap<u64, u64> {}
}

invariant "map slot" for { l: Ledger } {
    action l.set(k: u64, v: u64) {
        bound k in 0..10
        bound v in 0..100
    }
    check l.m_vals[1] >= 0
}
"#;

const HASHMAP_INDEX_CROSS: &str = r#"
entity Ledger {
    routes {
        set(k: u64, v: u64) => []
    }
    m_vals: HashMap<u64, u64> {}
}

invariant "map read ops" for { left: Ledger, right: Ledger } {
    action left.set(k: u64, v: u64) { bound k in 0..5 bound v in 0..20 }
    action right.set(k: u64, v: u64) { bound k in 5..10 bound v in 0..20 }
    check left.m_vals[1] + right.m_vals[2] >= 0
}
"#;

const CAST_MULTI: &str = r#"
entity Counter {
    routes { bump(n: u64) => [] }
    m_count: u64 { in bump(n) => m_count + n }
}

invariant "cast check" for { c: Counter } {
    action c.bump(n: u64) { bound n in 1..3 }
    check (c.m_count as u128) >= 0
}
"#;

const NS_CROSS: &str = r#"
entity Counter {
    routes { bump(n: u64) => [] }
    m_count: u64 { in bump(n) => m_count + n }
}

invariant "ns cross" for { a: Counter, b: Counter } {
    action a.bump(n: u64) { bound n in 1..3 }
    action b.bump(n: u64) { bound n in 1..3 }
    check std::math::min(a.m_count, b.m_count) >= 0
}
"#;

fn hashmap_index_not_vacuous(src: &str) {
    assert!(
        !src.contains("U256::ZERO >=") && !src.contains("require(true"),
        "HashMap index check must not collapse to a constant true:\n{src}"
    );
}

#[test]
fn hashmap_index_foundry_multi_keyed_getter() {
    let files = evm_files(HASHMAP_INDEX_MULTI);
    let inv = invariant_test_sol(&files, "map_slot");
    hashmap_index_not_vacuous(&inv);
    assert!(
        inv.contains("_l.m_vals(1)") || inv.contains("_cam_inv_"),
        "Foundry multi HashMap index must be a keyed getter or helper:\n{inv}"
    );
}

#[test]
fn hashmap_index_foundry_cross_instance_keyed_getters() {
    let files = evm_files(HASHMAP_INDEX_CROSS);
    let inv = invariant_test_sol(&files, "map_read");
    hashmap_index_not_vacuous(&inv);
    assert!(
        (inv.contains("_left.m_vals(1)") && inv.contains("_right.m_vals(2)"))
            || inv.contains("_cam_inv_"),
        "cross-instance HashMap index must use per-instance getters or a helper:\n{inv}"
    );
}

#[test]
fn cast_multi_check_not_require_true() {
    let files = evm_files(CAST_MULTI);
    let inv = invariant_test_sol(&files, "cast_check");
    hashmap_index_not_vacuous(&inv);
    assert!(
        inv.contains("_c.m_count") || inv.contains("_cam_inv_"),
        "cast check must read instance state or a helper:\n{inv}"
    );
}

#[test]
fn namespaced_cross_instance_is_i17() {
    use cambrian_transpiler::target::Target;
    use cambrian_transpiler::validate::check_target_compat;

    let program = parse(NS_CROSS);
    let evm = check_target_compat(&program, Target::Evm, false);
    assert!(
        evm.iter().any(|d| d.code == "I17"),
        "cross-instance namespaced call must be I17, not a vacuous check: {evm:?}"
    );
}
