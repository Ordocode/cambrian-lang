// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! P4c acceptance — HashMap operations, ghost `_keys` sync, iteration / chains.

use cambrian_transpiler::ast;
use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::validate::{check_lean_target_compat, Severity};
use cambrian_transpiler::ProgramParser;

fn parse(source: &str) -> ast::Program {
    let mut p = ProgramParser::new()
        .parse(source)
        .expect("fixture must parse");
    ast::normalize_program_types(&mut p);
    p
}

fn entity_file(source: &str, entity: &str) -> String {
    let backend = LeanBackend::default();
    let program = parse(source);
    backend
        .extra_files(&program, entity)
        .into_iter()
        .find(|(path, _)| path == &format!("Cambrian/Generated/{}.lean", entity))
        .map(|(_, content)| content)
        .unwrap_or_default()
}

fn routes_file(source: &str, entity: &str) -> String {
    let backend = LeanBackend::default();
    let program = parse(source);
    backend
        .extra_files(&program, entity)
        .into_iter()
        .find(|(path, _)| path == &format!("Cambrian/Generated/{}Routes.lean", entity))
        .map(|(_, content)| content)
        .unwrap_or_default()
}

const KEYS_EVM_CAM: &str = include_str!("../../contracts/keys_evm.cam");
const REGISTRY_CAM: &str = include_str!("../../contracts/registry.cam");

#[test]
fn lean_p4c_keys_evm_emits_ghost_keys_and_sync() {
    let state = entity_file(KEYS_EVM_CAM, "Registry");
    assert!(
        state.contains("m_balances_keys : List (Cambrian.Address)"),
        "iterated HashMap must emit ghost keys with parenthesised key type (P4c):\n{}",
        state
    );
    let routes = routes_file(KEYS_EVM_CAM, "Registry");
    assert!(
        routes.contains("pushKeyIfNew"),
        "register route must maintain keys list (P4c):\n{}",
        routes
    );
    assert!(
        routes.contains("removeKey"),
        "unregister route must maintain keys list (P4c):\n{}",
        routes
    );
    assert!(
        routes.contains("let ks := s.m_balances_keys"),
        "listKeys must lower keys().collect() to ghost list (P4c):\n{}",
        routes
    );
    assert!(
        !routes.contains("Unsupported"),
        "keys_evm routes must not contain Unsupported stubs (P4c):\n{}",
        routes
    );
}

#[test]
fn lean_p4c_registry_map_ops_without_keys_ghost() {
    let state = entity_file(REGISTRY_CAM, "Registry");
    assert!(
        state.contains("m_entries : Cambrian.AddressMap"),
        "registry HashMap must lower to AddressMap (P4c):\n{}",
        state
    );
    assert!(
        !state.contains("m_entries_keys"),
        "non-iterated HashMap must not emit ghost keys (P4c):\n{}",
        state
    );
    let members = entity_file(REGISTRY_CAM, "Registry");
    assert!(
        members.contains("AddressMap.empty"),
        "constructor must lower empty map to AddressMap.empty (P4c):\n{}",
        members
    );
    assert!(
        members.contains("AddressMap.insert"),
        "putEntry transform must use AddressMap.insert (P4c):\n{}",
        members
    );
    let routes = routes_file(REGISTRY_CAM, "Registry");
    assert!(
        routes.contains("AddressMap.lookup"),
        "getEntry must index via lookup (P4c):\n{}",
        routes
    );
}

#[test]
fn lean_p4c_relaxed_l2_allows_collect_chain() {
    let prog = parse(KEYS_EVM_CAM);
    let l2: Vec<_> = check_lean_target_compat(&prog)
        .into_iter()
        .filter(|d| d.code == "L2" && d.severity == Severity::Error)
        .collect();
    assert!(l2.is_empty(), "keys_evm must not hit L2 on .collect chain (P4c): {:?}", l2);
}

const STDLIB_DEMO_CAM: &str = include_str!("../../contracts/stdlib_demo.cam");

#[test]
fn lean_explicit_hashmap_new_lowers_to_addressmap_empty() {
    // `HashMap::new()` parses as an enum variant, so without an explicit
    // case the Lean backend emitted `HashMap.new` — an identifier the
    // Prelude never defines, which fails `lake build` for every contract
    // holding a mapping. The EVM backend has always special-cased it.
    let members = entity_file(STDLIB_DEMO_CAM, "StdlibDemo");
    assert!(
        !members.contains("HashMap.new"),
        "HashMap::new() must not reach Lean as an enum constructor:\n{}",
        members
    );
    assert!(
        members.contains("Cambrian.AddressMap.empty"),
        "HashMap::new() must lower to the Prelude's empty map:\n{}",
        members
    );
}

/// Fail-mode member transform (`/` in a called pure fn) must still track
/// `let inner = m[k]` as a map so nested `.exists` / `.update` lower
/// instead of `Cambrian.Unsupported` (contracts/dex.cam sentinel scan).
#[test]
fn lean_nested_hashmap_let_inner_lowers_in_fail_mode() {
    let src = r#"
pure fn q(a: u64, b: u64) -> u64 { a / b }
entity Nested {
    routes {
        constructor() => []
        bump(k: u64, inner_k: u64) => []
    }
    m_nested: HashMap<u64, HashMap<u64, u64>> {
        in bump(k, inner_k) => {
            let inner = if m_nested.exists(k) { m_nested[k] } else { {} };
            let current = if inner.exists(inner_k) { inner[inner_k] } else { 0 };
            m_nested.update(k, inner.update(inner_k, current + q(1, 1)))
        }
    }
    m_x: u64 { in constructor() => 0 }
}
"#;
    let members = entity_file(src, "Nested");
    assert!(
        !members.contains("Cambrian.Unsupported"),
        "nested let-bound HashMap methods must lower in fail-mode:\n{members}"
    );
    assert!(
        members.contains("AddressMap.contains") && members.contains("AddressMap.insert"),
        "inner.exists / inner.update must use AddressMap:\n{members}"
    );
}
