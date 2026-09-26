// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! P7 domain-parity gate: for every parseable primary in `contracts/`,
//! universal + domain diagnostics must match between `--target evm` and
//! `--target lean` (language/pair bins excluded by construction).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use cambrian_transpiler::target::Target;
use cambrian_transpiler::validate::{
    check_target_compat, rule_binding, validate, Diagnostic, RuleBinding, Severity,
};

fn parse_file(path: &Path) -> Option<cambrian_transpiler::ast::Program> {
    let src = fs::read_to_string(path).ok()?;
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(&src)
        .ok()?;
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    Some(program)
}

fn collect_contract_primaries(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let contracts = root.join("contracts");
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("cam") {
                continue;
            }
            let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if name.contains(".test") || name.contains(".fuzz") || name.contains(".invariant") {
                continue;
            }
            out.push(path);
        }
    }
    walk(&contracts, &mut out);
    out.sort();
    out
}

fn diag_key(d: &Diagnostic) -> String {
    let sev = match d.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };
    format!("{sev}\t{}\t{}", d.code, d.message)
}

fn is_shared_binding(b: RuleBinding) -> bool {
    // Universal + Domains/NotDomains: dispatch depends only on the domain
    // axis, so same-domain cores must agree.
    b.is_domain_bound()
}

fn shared_diag_set(program: &cambrian_transpiler::ast::Program, target: Target) -> BTreeSet<String> {
    let mut diags = validate(program);
    diags.extend(check_target_compat(program, target, false));
    diags
        .into_iter()
        .filter(|d| is_shared_binding(rule_binding(d.code)))
        .map(|d| diag_key(&d))
        .collect()
}

#[test]
fn evm_lean_universal_domain_parity() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf();
    let fixtures = collect_contract_primaries(&root);
    assert!(!fixtures.is_empty(), "no contracts/ primaries found");

    let mut checked = 0usize;
    let mut mismatches = Vec::new();

    for cam in &fixtures {
        let Some(program) = parse_file(cam) else {
            continue;
        };
        let evm = shared_diag_set(&program, Target::Evm);
        let lean = shared_diag_set(&program, Target::Lean);
        if evm != lean {
            let only_evm: Vec<_> = evm.difference(&lean).cloned().collect();
            let only_lean: Vec<_> = lean.difference(&evm).cloned().collect();
            mismatches.push(format!(
                "{}:\n  only-evm ({}):\n    {}\n  only-lean ({}):\n    {}",
                cam.display(),
                only_evm.len(),
                only_evm.join("\n    "),
                only_lean.len(),
                only_lean.join("\n    "),
            ));
        }
        checked += 1;
    }

    assert!(
        mismatches.is_empty(),
        "universal+domain diagnostic parity failed for {checked} fixtures:\n\n{}",
        mismatches.join("\n\n")
    );
}

#[test]
fn registry_covers_emitted_codes_smoke() {
    // Touch rule_binding for a representative set; panics if unregistered.
    for code in [
        "V1", "V34", "V40", "E01", "E07", "E12", "E23", "L1", "L8", "L13", "L16", "T1", "I7", "W1", "W7",
    ] {
        let _ = rule_binding(code);
    }
}
