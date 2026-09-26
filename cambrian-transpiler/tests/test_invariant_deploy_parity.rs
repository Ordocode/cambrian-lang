// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! P1 — invariant `deploy { }` harness parity (Lean scoped bootstrap + EVM factory/prank).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::{EvmSolidityBackend, LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;
#[cfg(feature = "revm")]
use cambrian_transpiler::{
    codegen::{
        cargo_fuzz_codegen::generate_revm_fuzz_targets,
        evm_revm_test_codegen::generate_revm_tests,
    },
    project::{CoverageFuzzConfig, FuzzConfig, InvariantConfig, RevmTestConfig},
};

const DEPLOY_HEX: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000d01";
const SENDER_A01_HEX: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000a01";
const SENDER_A02_HEX: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000a02";
const DEPLOY_DEC: &str = "3329"; // Lean literal form for 0xd01

const FOUNDRY_TOML: &str = r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
evm_version = "prague"
optimizer = false
"#;

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/invariant_deploy_parity")
}

fn unique_out(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-inv-deploy-parity-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ))
}

fn has_forge() -> bool {
    Command::new("forge")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn load_evm_files() -> HashMap<String, String> {
    let yaml = fixture_dir().join("project.evm.yaml");
    let project = Project::load(&yaml).unwrap_or_else(|e| panic!("load: {e}"));
    EvmSolidityBackend {
        deterministic_addresses: true,
    }
    .gen_project(&project)
    .into_iter()
    .collect()
}

fn load_lean_spec() -> String {
    let yaml = fixture_dir().join("project.lean.yaml");
    let project = Project::load(&yaml).unwrap_or_else(|e| panic!("load: {e}"));
    LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .find(|(p, _)| p.ends_with("MintTokenSpec.lean"))
        .map(|(_, c)| c)
        .expect("MintTokenSpec.lean")
}

fn invariant_forge_sol(files: &HashMap<String, String>) -> String {
    files
        .iter()
        .find(|(p, _)| p.contains("Invariant_") && p.ends_with(".t.sol"))
        .map(|(_, s)| s.clone())
        .expect("Invariant_*.t.sol")
}

#[test]
fn lean_explicit_deploy_uses_scoped_ctor_ctx() {
    let spec = load_lean_spec();
    let thm = spec
        .split("theorem supply_cap_explicit_deploy")
        .nth(1)
        .expect("invariant theorem");
    assert!(
        thm.contains("Routes.constructor w inst { ctx with sender := ")
            && thm.contains(DEPLOY_DEC),
        "Lean must scope deploy sender into ctor call only (B4):\n{thm}"
    );
    assert!(
        !thm.contains("let ctx := { ctx with sender"),
        "explicit deploy must not rebind outer trace ctx:\n{thm}"
    );
}

#[test]
fn evm_det_factory_and_initialize_use_deploy_not_senders() {
    let files = load_evm_files();
    let sol = invariant_forge_sol(&files);
    assert!(
        sol.contains("new CambrianFactory()"),
        "Tier A invariant setUp must deploy CambrianFactory (U4-4b):\n{sol}"
    );
    assert!(
        sol.contains(&format!("deployMintToken(address(uint160(uint256({DEPLOY_HEX})))")),
        "factory.deploy* ctor arg must come from deploy block (B3), not senders[0]:\n{sol}"
    );
    assert!(
        !sol.contains(".initialize("),
        "factory.deploy* already initializes; setUp must not call .initialize:\n{sol}"
    );
    assert!(
        !sol.contains(&format!("new MintToken(address(uint160(uint256({DEPLOY_HEX})))")),
        "Tier A must not use fake-factory new Entity(deployAddr):\n{sol}"
    );
    assert!(
        sol.contains(&format!("targetSender(address(uint160(uint256({SENDER_A01_HEX})))"))
            && sol.contains(&format!(
                "targetSender(address(uint160(uint256({SENDER_A02_HEX})))"
            )),
        "trace pool must list senders only:\n{sol}"
    );
    assert!(
        !sol.contains(&format!("targetSender(address(uint160(uint256({DEPLOY_HEX})))")),
        "deploy address must not enter targetSender pool:\n{sol}"
    );
}

#[test]
fn forge_invariant_supply_cap_runs_green() {
    if !has_forge() {
        eprintln!("skip forge_invariant_supply_cap_runs_green (forge not on PATH)");
        return;
    }
    let files = load_evm_files();
    let out_dir = unique_out("forge");
    let _ = std::fs::remove_dir_all(&out_dir);
    for (rel, contents) in &files {
        let path = out_dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, contents).expect("write");
    }
    std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).expect("foundry.toml");
    cambrian_transpiler::codegen::evm_test_codegen::install_forge_std(&out_dir)
        .unwrap_or_else(|e| panic!("forge-std: {e}"));

    let forge = Command::new("forge")
        .args(["test", "--match-contract", "Invariant_MintToken", "-vv", "--root"])
        .arg(&out_dir)
        .output()
        .expect("forge test");
    let log = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );
    assert!(forge.status.success(), "forge invariant must pass:\n{log}");
}

#[cfg(feature = "revm")]
fn load_revm_invariant_rs() -> String {
    let yaml = fixture_dir().join("project.evm.yaml");
    let project = Project::load(&yaml).unwrap_or_else(|e| panic!("load: {e}"));
    let files = generate_revm_tests(
        &project.merged,
        "mint_token",
        true,
        &RevmTestConfig {
            enabled: true,
            ..Default::default()
        },
        &FuzzConfig::default(),
        &InvariantConfig::default(),
    );
    files
        .into_iter()
        .find(|(p, _)| p == "revm-tests/tests/mint_token.rs")
        .map(|(_, c)| c)
        .expect("revm-tests/tests/mint_token.rs")
}

#[cfg(feature = "revm")]
#[test]
fn revm_proptest_uses_deploy_factory_and_initialize_sender() {
    let rs = load_revm_invariant_rs();
    let deploy_addr = "address!(\"0x0000000000000000000000000000000000000d01\")";
    assert!(
        rs.contains("let ctor: Bytes = (") && rs.contains(deploy_addr),
        "det factory must be deploy address:\n{rs}"
    );
    assert!(
        rs.contains(&format!("t.set_sender({deploy_addr})")),
        "initialize must run as deploy sender:\n{rs}"
    );
    assert!(
        rs.contains(&format!("initializeCall {{ mint_to: {deploy_addr} }}")),
        "ctor address arg must come from deploy:\n{rs}"
    );
    assert!(
        rs.contains("_senders")
            && rs.contains("address!(\"0x0000000000000000000000000000000000000a01\")")
            && rs.contains("address!(\"0x0000000000000000000000000000000000000a02\")"),
        "revm trace pool must list senders only:\n{rs}"
    );
}

#[cfg(feature = "revm")]
fn load_cargo_fuzz_target_rs() -> String {
    let yaml = fixture_dir().join("project.evm.yaml");
    let project = Project::load(&yaml).unwrap_or_else(|e| panic!("load: {e}"));
    let files = generate_revm_fuzz_targets(
        &project.merged,
        "mint_token",
        &CoverageFuzzConfig {
            enabled: true,
            ..Default::default()
        },
        &InvariantConfig::default(),
        true,
    );
    files
        .into_iter()
        .find(|(p, _)| p.contains("fuzz_targets"))
        .map(|(_, c)| c)
        .expect("cargo-fuzz target")
}

#[cfg(feature = "revm")]
#[test]
fn cargo_fuzz_revm_uses_deploy_setup_not_early_return() {
    let rs = load_cargo_fuzz_target_rs();
    let deploy_addr = "address!(\"0x0000000000000000000000000000000000000d01\")";
    assert!(
        !rs.contains("EVM-only (PL-F-INV-04 P1c); Acki Nacki fuzz skipped"),
        "revm cargo-fuzz must not punt explicit deploy:\n{rs}"
    );
    assert!(
        rs.contains(&format!("t.set_sender({deploy_addr})"))
            && rs.contains(&format!("initializeCall {{ mint_to: {deploy_addr} }}")),
        "cargo-fuzz must mirror revm deploy setup:\n{rs}"
    );
}
