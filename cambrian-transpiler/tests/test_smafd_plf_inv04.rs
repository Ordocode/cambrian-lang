// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! PL-F-INV-04 — Run #9 cp_004 ctor-mint invariant with explicit `deploy { 0xd01 }`.

use std::path::PathBuf;

use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;

const DEPLOY_DEC: &str = "3329"; // 0xd01
const SENDER_A01_DEC: &str = "2561";
const SENDER_A02_DEC: &str = "2562";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/smafd_plf_inv04")
}

fn transpile_spec() -> String {
    let yaml = fixture_dir().join("project.lean.yaml");
    let project = Project::load(&yaml).unwrap_or_else(|e| panic!("load {}: {e}", yaml.display()));
    LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .find(|(p, _)| p.ends_with("TokenCoreSpec.lean"))
        .map(|(_, c)| c)
        .expect("TokenCoreSpec.lean")
}

fn cp_004_fragment(spec: &str) -> &str {
    let start = spec
        .find("theorem cp_004_ctor_mint_only_supply_cap")
        .expect("cp_004 theorem");
    let rest = &spec[start..];
    rest.split("end cp_004_ctor_mint_only_supply_cap")
        .next()
        .unwrap_or(rest)
}

fn cp_001_fragment(spec: &str) -> &str {
    let start = spec
        .find("theorem cp_001_stateful_supply_four_actor_senders")
        .expect("cp_001 theorem");
    let rest = &spec[start..];
    rest.split("end cp_001_stateful_supply_four_actor_senders")
        .next()
        .unwrap_or(rest)
}

#[test]
fn plf_inv04_cp004_deployer_scoped_ctor() {
    let spec = transpile_spec();
    let frag = cp_004_fragment(&spec);
    assert!(
        frag.contains(&format!(
            "TokenCore.Routes.constructor w inst {{ ctx with sender := {DEPLOY_DEC} }}"
        )),
        "cp_004 ctor must use explicit deploy address (scoped B4):\n{frag}"
    );
    assert!(
        !frag.contains(&format!(
            "let ctx := {{ ctx with sender := {SENDER_A01_DEC} }}"
        )),
        "cp_004 must not bootstrap ctor from trace senders[0]:\n{frag}"
    );
}

#[test]
fn plf_inv04_cp004_deployer_not_in_senders() {
    let spec = transpile_spec();
    let ns = spec
        .split("namespace cp_004_ctor_mint_only_supply_cap")
        .nth(1)
        .expect("cp_004 namespace");
    let senders_block = ns.split("inductive Action").next().expect("senders def");
    assert!(
        senders_block.contains(SENDER_A01_DEC) && senders_block.contains(SENDER_A02_DEC),
        "trace pool must stay catalog senders:\n{senders_block}"
    );
    assert!(
        !senders_block.contains(DEPLOY_DEC),
        "deploy address must not appear in trace senders:\n{senders_block}"
    );
}

#[test]
fn plf_inv04_cp001_legacy_bootstrap_unchanged() {
    let spec = transpile_spec();
    let frag = cp_001_fragment(&spec);
    assert!(
        frag.contains(&format!("let ctx := {{ ctx with sender := {DEPLOY_DEC} }}"))
            && frag.contains("Routes.constructor w inst ctx"),
        "cp_001 without deploy must keep legacy senders[0] bootstrap:\n{frag}"
    );
}
