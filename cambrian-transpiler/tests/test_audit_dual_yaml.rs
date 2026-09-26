// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! P8 audit-fixture dual-YAML policy: EVM-domain audit cams must have
//! both an `*_evm.yaml` and an `*_lean.yaml` twin (or equivalent yaml
//! pair that sets `target: evm` / `target: lean` over the same `.cam`).

#[path = "corpus/mod.rs"]
mod corpus;

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

/// Existing single-target audit fixtures, grandfathered until dual YAMLs
/// are added. Graduation panic when a listed stem gains both targets.
const AUDIT_SINGLE_TARGET_GRANDFATHER: &[&str] = &[
    // Empty: x_h4 / x_h8 / x_h6 now have canonical `*_lean.yaml` twins.
    // Differential T-X-006 still also references `lean_h4_typed_send_value.yaml`
    // (a distinct Lean-shaped fixture), which is fine — dual-YAML only
    // requires the stem's own `_evm.yaml` + `_lean.yaml` pair.
];

fn audit_fixtures_dir() -> PathBuf {
    corpus::repo_root().join("cambrian-transpiler/tests/audit/fixtures")
}

fn is_policy_cam(stem: &str) -> bool {
    stem.starts_with("x_") || stem.starts_with("fuzz_") || stem.starts_with("std_")
}

fn yaml_targets_for_cam(dir: &PathBuf, stem: &str) -> (bool, bool, Vec<String>) {
    let mut has_evm = false;
    let mut has_lean = false;
    let mut names = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return (false, false, names);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let Ok(txt) = fs::read_to_string(&path) else {
            continue;
        };
        if !txt.contains(&format!("{stem}.cam")) {
            continue;
        }
        names.push(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        );
        if txt.contains("target: evm") || txt.contains("target:evm") {
            has_evm = true;
        }
        if txt.contains("target: lean") || txt.contains("target:lean") {
            has_lean = true;
        }
    }
    (has_evm, has_lean, names)
}

#[test]
fn audit_evm_domain_fixtures_have_dual_yaml() {
    let dir = audit_fixtures_dir();
    assert!(dir.is_dir(), "missing {}", dir.display());

    let grandfather: HashSet<&str> = AUDIT_SINGLE_TARGET_GRANDFATHER.iter().copied().collect();
    let mut violations = Vec::new();
    let mut graduated = Vec::new();
    let mut checked = 0usize;

    let Ok(entries) = fs::read_dir(&dir) else {
        panic!("cannot read {}", dir.display());
    };
    let mut cams: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("cam"))
        .collect();
    cams.sort();

    for cam in cams {
        let stem = cam.file_stem().unwrap().to_string_lossy().into_owned();
        if !is_policy_cam(&stem) {
            continue;
        }
        checked += 1;
        let (has_evm, has_lean, names) = yaml_targets_for_cam(&dir, &stem);
        let dual = has_evm && has_lean;
        let gf = grandfather.contains(stem.as_str());
        if dual && gf {
            graduated.push(stem.clone());
            continue;
        }
        if dual {
            continue;
        }
        if gf {
            // Grandfathered single-target — still require at least one yaml.
            if names.is_empty() {
                violations.push(format!(
                    "{stem}: grandfathered but no yaml references this .cam"
                ));
            }
            continue;
        }
        violations.push(format!(
            "{stem}: need both target:evm and target:lean yaml twins (found evm={has_evm} lean={has_lean} yamls={names:?})"
        ));
    }

    assert!(checked > 0, "no x_/fuzz_/std_ audit cams found");
    assert!(
        graduated.is_empty(),
        "grandfathered audit fixtures now have dual yaml — remove from AUDIT_SINGLE_TARGET_GRANDFATHER:\n  {}",
        graduated.join("\n  ")
    );
    assert!(
        violations.is_empty(),
        "audit dual-YAML policy violations ({checked} cams checked):\n  {}",
        violations.join("\n  ")
    );
}
