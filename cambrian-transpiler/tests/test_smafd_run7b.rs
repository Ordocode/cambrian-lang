// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! SMAFD Run #7b residual — PL-F-INV-02 stateful invariant constructor bootstrap.

use std::path::PathBuf;

use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/smafd_run7b")
}

fn transpile_spec() -> String {
    let yaml = fixture_dir().join("project.lean.yaml");
    let project = Project::load(&yaml).unwrap_or_else(|e| panic!("load {}: {e}", yaml.display()));
    let entity = project
        .merged
        .entities
        .first()
        .map(|e| e.name.clone())
        .expect("entity");
    let backend = LeanBackend::default();
    let files = backend.gen_project(&project);
    files
        .into_iter()
        .find(|(p, _)| p.ends_with(&format!("{entity}Spec.lean")))
        .map(|(_, c)| c)
        .expect("Spec.lean")
}

fn cp_002_fragment(spec: &str) -> &str {
    let start = spec
        .find("theorem cp_002_stateful_balance_sum_equals_supply")
        .expect("cp_002 theorem");
    let rest = &spec[start..];
    rest.split("namespace")
        .next()
        .unwrap_or(rest)
}

#[test]
fn smafd_plf_inv02_cp002_constructor_bootstrap() {
    let spec = transpile_spec();
    let frag = cp_002_fragment(&spec);
    assert!(
        frag.contains("Routes.constructor w inst ctx"),
        "cp_002 must run constructor bootstrap before trace check:\n{frag}"
    );
    assert!(
        frag.contains("sumBalances") && frag.contains("totalSupply"),
        "check must relate sumBalances and totalSupply (no weakening):\n{frag}"
    );
}
