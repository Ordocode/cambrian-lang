// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! P4b acceptance — collections, iteration, extern entity, multi-entity invariants.

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

fn spec_for(source: &str, entity: &str) -> String {
    let backend = LeanBackend::default();
    let program = parse(source);
    backend
        .extra_files(&program, entity)
        .into_iter()
        .find(|(p, _)| p == &format!("Cambrian/Generated/{}Spec.lean", entity))
        .map(|(_, c)| c)
        .unwrap_or_default()
}

const MAP_CAM: &str = include_str!("../../contracts/lean_map.cam");

const EXTERN_CAM: &str = include_str!("../../contracts/extern_token_caller.cam");

#[test]
fn lean_p4b_hashmap_lowers_to_address_map() {
    let out = entity_file(MAP_CAM, "Registry");
    assert!(
        out.contains("Cambrian.AddressMap"),
        "HashMap members must lower to AddressMap (P4b):\n{}",
        out
    );
    assert!(
        !out.contains("m_balances_keys"),
        "non-iterated HashMap must not emit ghost keys (P4c):\n{}",
        out
    );
}

#[test]
fn lean_p4b_relaxed_l1_allows_collections() {
    let prog = parse(MAP_CAM);
    let errs: Vec<_> = check_lean_target_compat(&prog)
        .into_iter()
        .filter(|d| d.code == "L1" && d.severity == Severity::Error)
        .collect();
    assert!(errs.is_empty(), "L1 must allow Vec/HashMap/Option on Lean (P4b): {:?}", errs);
}

#[test]
fn lean_p4b_extern_module_emitted() {
    let backend = LeanBackend::default();
    let program = parse(EXTERN_CAM);
    let files: std::collections::HashMap<_, _> = backend
        .extra_files(&program, "Caller")
        .into_iter()
        .collect();
    let ext = files
        .get("Cambrian/Generated/Extern.lean")
        .expect("Extern.lean must exist for extern entity (P4b)");
    assert!(
        ext.contains("opaque transfer") && ext.contains("Except Cambrian.ThrowCode"),
        "extern routes must be opaque Except axioms (P4b / PN-106):\n{}",
        ext
    );
    // Regression guard: the axiom namespace must be valid Lean 4
    // (`namespace Routes`, not the ill-typed `namespace Routes where`),
    // and the binders must use `opaque <name>` (not `opaque def`).
    assert!(
        ext.contains("namespace Routes\n") && !ext.contains("namespace Routes where"),
        "extern Routes namespace must be plain `namespace Routes`:\n{}",
        ext
    );
    assert!(
        !ext.contains("opaque def"),
        "`opaque def` is not valid Lean 4 — use `opaque <name>`:\n{}",
        ext
    );
    let routes = files
        .get("Cambrian/Generated/CallerRoutes.lean")
        .expect("CallerRoutes.lean");
    assert!(
        routes.contains("RouteResult")
            && (routes.contains("Except.bind") || routes.contains("←")),
        "PN-106: ping/checkBalance must be fail-mode and bind extern with ← / Except.bind:\n{}",
        routes
    );
}

#[test]
fn lean_p4b_multi_entity_invariant_emitted() {
    let inv_cam = include_str!("../../contracts/escrow_two_vaults.invariant.cam");
    let escrow = include_str!("../../contracts/escrow_two_vaults.cam");
    let source = format!("{}\n{}", escrow, inv_cam);
    let spec = spec_for(&source, "Vault");
    assert!(
        spec.contains("inductive Action") && spec.contains("v_deposit"),
        "multi-entity invariant must emit qualified Action constructors (P4b):\n{}",
        spec
    );
    assert!(
        !spec.contains("TODO(P4): multi-entity"),
        "multi-entity invariant skip comment must be gone (P4b):\n{}",
        spec
    );
    assert!(
        spec.contains("Vault.Routes.deposit") || spec.contains("Treasury.Routes.credit"),
        "multi-entity step must dispatch to instance routes (P4b):\n{}",
        spec
    );
}
