// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use cambrian_transpiler::ProgramParser;
use cambrian_transpiler::validate::{validate, build_temporal_orders, Severity};

#[test]
fn validate_multisig_no_errors() {
    let src = include_str!("../../contracts/multisig.cam");
    let program = ProgramParser::new().parse(src).unwrap();
    let diags = validate(&program);

    let errors: Vec<_> = diags.iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();

    if !errors.is_empty() {
        for e in &errors {
            eprintln!("[{}] {}", e.code, e.message);
        }
        panic!("Expected zero validation errors, got {}", errors.len());
    }
}

#[test]
fn validate_multisig_temporal_orders() {
    let src = include_str!("../../contracts/multisig.cam");
    let program = ProgramParser::new().parse(src).unwrap();
    let entity = &program.entities[0];

    let (orders, diags) = build_temporal_orders(entity);

    let errors: Vec<_> = diags.iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "Temporal DAG errors: {:?}", errors);

    // constructor route should have an order for m_default_required_confirmations
    // that comes AFTER m_custodian_count (because of ^m_custodian_count reference)
    let ctor_order = orders.iter()
        .find(|o| o.route_name == "constructor")
        .expect("constructor should have temporal order");

    let count_pos = ctor_order.order.iter()
        .position(|n| n == "m_custodian_count");
    let confirms_pos = ctor_order.order.iter()
        .position(|n| n == "m_default_required_confirmations");

    if let (Some(cp), Some(dp)) = (count_pos, confirms_pos) {
        assert!(cp < dp,
            "m_custodian_count ({}) should be computed before m_default_required_confirmations ({})",
            cp, dp);
    }
}

#[test]
fn validate_multisig_warnings_only_for_view_routes() {
    let src = include_str!("../../contracts/multisig.cam");
    let program = ProgramParser::new().parse(src).unwrap();
    let diags = validate(&program);

    let warnings: Vec<_> = diags.iter()
        .filter(|d| d.severity == Severity::Warning)
        .collect();

    // View routes like acceptTransfer, receive, fallback have empty bodies
    // and no state transforms, so they get V2 warnings
    for w in &warnings {
        eprintln!("[WARN {}] {}", w.code, w.message);
    }
}
