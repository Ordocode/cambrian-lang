// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! P8 corpus self-check: lists are accurate, names exist, graduation panics.

#[path = "corpus/mod.rs"]
mod corpus;

use cambrian_transpiler::target::{Domain, Target};
use cambrian_transpiler::validate::{check_target_compat, validate, Severity};
use std::collections::HashSet;
use std::fs;

fn parse_file(path: &std::path::Path) -> Option<cambrian_transpiler::ast::Program> {
    let src = fs::read_to_string(path).ok()?;
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(&src)
        .ok()?;
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    Some(program)
}

fn error_codes(program: &cambrian_transpiler::ast::Program, target: Target) -> HashSet<String> {
    let mut diags = validate(program);
    diags.extend(check_target_compat(program, target, false));
    diags
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code.to_string())
        .collect()
}

#[test]
fn corpus_lists_reference_existing_files() {
    let primaries: HashSet<_> = corpus::contracts_primaries()
        .into_iter()
        .map(|f| f.key)
        .collect();
    let mut missing = Vec::new();
    for (key, _, _, _) in corpus::REJECTED_ON {
        if !primaries.contains(*key) {
            missing.push(format!("REJECTED_ON:{key}"));
        }
    }
    for (key, _, _) in corpus::CORE_REJECTS {
        if !primaries.contains(*key) {
            missing.push(format!("CORE_REJECTS:{key}"));
        }
    }
    for key in corpus::PROJECT_ONLY {
        if !primaries.contains(*key) {
            missing.push(format!("PROJECT_ONLY:{key}"));
        }
    }
    assert!(
        missing.is_empty(),
        "corpus list entries with no matching contracts/ primary:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn rejected_on_entries_fail_with_recorded_code() {
    let by_key: std::collections::BTreeMap<_, _> = corpus::contracts_primaries()
        .into_iter()
        .map(|f| (f.key.clone(), f))
        .collect();
    let mut failures = Vec::new();
    for (key, domain, code, reason) in corpus::REJECTED_ON {
        let Some(fix) = by_key.get(*key) else {
            failures.push(format!("{key}: missing file"));
            continue;
        };
        let Some(program) = parse_file(&fix.path) else {
            failures.push(format!("{key}: failed to parse"));
            continue;
        };
        for target in Target::cores_of(*domain) {
            let codes = error_codes(&program, target);
            if !codes.contains(*code) {
                failures.push(format!(
                    "{key} on {}: expected error {code} ({reason}), got {codes:?}",
                    target.name()
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "REJECTED_ON self-check failed:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn core_rejects_still_reject_with_recorded_code() {
    let by_key: std::collections::BTreeMap<_, _> = corpus::contracts_primaries()
        .into_iter()
        .map(|f| (f.key.clone(), f))
        .collect();
    let mut failures = Vec::new();
    let mut graduated = Vec::new();
    for (key, target, code) in corpus::CORE_REJECTS {
        let Some(fix) = by_key.get(*key) else {
            failures.push(format!("{key}: missing file"));
            continue;
        };
        let Some(program) = parse_file(&fix.path) else {
            failures.push(format!("{key}: failed to parse"));
            continue;
        };
        let codes = error_codes(&program, *target);
        if codes.is_empty() {
            graduated.push(format!(
                "{key} on {} no longer rejects (was {code}) — remove from CORE_REJECTS",
                target.name()
            ));
        } else if !codes.contains(*code) {
            failures.push(format!(
                "{key} on {}: expected error {code}, got {codes:?}",
                target.name()
            ));
        }
    }
    assert!(
        graduated.is_empty(),
        "CORE_REJECTS graduation:\n  {}",
        graduated.join("\n  ")
    );
    assert!(
        failures.is_empty(),
        "CORE_REJECTS self-check failed:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn dual_core_projects_yamls_exist() {
    let projects = corpus::dual_core_projects();
    assert!(!projects.is_empty());
    for (key, _, yamls) in projects {
        assert!(
            yamls.len() >= 2,
            "{key}: dual-core project needs ≥2 yaml twins"
        );
    }
}

#[test]
fn domain_fixtures_exclude_rejected_and_project_only() {
    let evm = corpus::domain_fixtures(Domain::Evm);
    let keys: HashSet<_> = evm.iter().map(|f| f.key.as_str()).collect();
    assert!(!keys.contains("multisig"));
    assert!(!keys.contains("upgrade"));
    assert!(!keys.contains("proj_counter"));
    assert!(!keys.contains("det_counter"));
    assert!(!keys.contains("escrow_two_vaults"));
    assert!(!keys.contains("marketplace/broker"));
    assert!(!keys.contains("marketplace/ledger"));
    assert!(!keys.contains("guardian_vault/vault"));
    assert!(!keys.contains("det_addressof"));
    assert!(!keys.contains("identity_pair/identity_pair"));
    assert!(!keys.contains("guardian_vault/guardian"));
    assert!(!keys.contains("bouncer"));
    assert!(keys.contains("counter"));
    assert!(keys.contains("lean_pair"));
}

/// `lean_p3_lake_build_smoke`'s contracts/ primary paths must resolve to
/// real corpus primaries (no dead names). Multi-file / companion / 
/// tests/fixtures entries are exempt.
#[test]
fn lake_smoke_contract_primaries_exist_in_corpus() {
    // Keep in sync with `lean_p3_lake_build_smoke` in test_codegen_lean_world.rs
    // — only the *primary* contracts/ sources (not .fuzz/.invariant companions).
    const LAKE_SMOKE_PRIMARIES: &[&str] = &[
        "counter",
        "predictable",
        "phased_vault",
        "lean_self_call",
        "lean_call",
        "lean_send_return",
        "lean_conditional",
        "lean_pair",
        "lean_deploy",
        "det_guardian",
        "det_locker",
        "lean_cross_send",
        "lean_map",
        "keys_evm",
        "registry",
        "escrow_two_vaults",
        "airdrop_evm",
        "fold_evm",
        "erc20_errors_evm",
        "erc20_events_evm",
        "eth_vault_evm",
        "staking",
        "voting",
        "wallet",
        "payment_channel",
        "phased_predictable",
        "enum_data",
        "det_reentrancy_order",
        "invariant_lending",
    ];
    let primaries: HashSet<_> = corpus::contracts_primaries()
        .into_iter()
        .map(|f| f.key)
        .collect();
    let mut missing = Vec::new();
    for key in LAKE_SMOKE_PRIMARIES {
        if !primaries.contains(*key) {
            missing.push(*key);
        }
    }
    assert!(
        missing.is_empty(),
        "lean_p3_lake_build_smoke references unknown contracts/ primaries: {missing:?}"
    );
}
