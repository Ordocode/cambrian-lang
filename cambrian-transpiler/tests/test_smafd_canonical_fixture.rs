// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! SMAFD canonical RehearsalToken project — dual-target layout regression.
//!
//! Fixture: `tests/fixtures/smafd_canonical/` (see README there).

#[path = "harness/mod.rs"]
mod harness;

use std::path::{Path, PathBuf};

use harness::factory_oracle::{assert_factory_setup, extract_setup_sol};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::{EvmSolidityBackend, OutputBackend};
use cambrian_transpiler::project::Project;

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/smafd_canonical")
}

fn unique_out(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-smafd-canonical-{}-{}-{}",
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

fn transpile_evm_files(yaml_name: &str) -> std::collections::HashMap<String, String> {
    let yaml = fixture_dir().join(yaml_name);
    let project = Project::load(&yaml).unwrap_or_else(|e| panic!("load {}: {e}", yaml.display()));
    let det = project.config.deterministic_addresses.unwrap_or(true);
    EvmSolidityBackend {
        deterministic_addresses: det,
    }
    .gen_project(&project)
    .into_iter()
    .collect()
}

fn write_project(files: &std::collections::HashMap<String, String>, out_dir: &Path) {
    for (rel, contents) in files {
        let path = out_dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, contents).expect("write");
    }
}

fn ensure_forge_std(out_dir: &Path) {
    if out_dir.join("lib/forge-std").is_dir() {
        return;
    }
    let _ = Command::new("git")
        .args(["init", "-q"])
        .current_dir(out_dir)
        .status();
    let status = Command::new("forge")
        .args(["install", "foundry-rs/forge-std"])
        .current_dir(out_dir)
        .status()
        .expect("forge install");
    assert!(status.success(), "forge install forge-std failed");
}

#[test]
fn smafd_canonical_catalog_lowers_to_foundry() {
    let files = transpile_evm_files("project.evm.yaml");
    let sol = files
        .get("test/RehearsalToken.t.sol")
        .expect("RehearsalToken.t.sol");
    assert!(
        sol.contains("testFuzz_PROP_RHT_003_TransferConservation"),
        "catalog property must desugar to Foundry fuzz: missing PROP-RHT-003"
    );
    assert!(
        sol.contains("test_PROP_RHT_001_ConstructorInitialMint"),
        "catalog property must desugar to Foundry test: missing PROP-RHT-001"
    );
    assert!(
        sol.contains("private constant INITIAL_SUPPLY ="),
        "entity const INITIAL_SUPPLY must be emitted in Foundry test contract: {sol}"
    );
    assert!(
        sol.contains("private constant ZERO ="),
        "entity const ZERO must be emitted in Foundry test contract: {sol}"
    );
    assert!(
        sol.contains("vm.startPrank("),
        "msg {{ sender }} must use startPrank for multi-call bodies: {sol}"
    );
    assert!(
        sol.contains("vm.assume(") && sol.find("vm.assume(").unwrap() < sol.find("amount - 1").unwrap_or(usize::MAX),
        "assume must be emitted before let bindings that reference fuzz params"
    );
}

#[test]
fn smafd_canonical_test_init_no_factory_redeploy_in_body() {
    let files = transpile_evm_files("project.evm.yaml");
    let sol = files
        .get("test/RehearsalToken.t.sol")
        .expect("RehearsalToken.t.sol");
    let body = sol
        .split("function test_init_mints_full_supply_to_deployer")
        .nth(1)
        .and_then(|s| s.split("function ").next())
        .unwrap_or("");
    let setup = extract_setup_sol(sol);
    assert_factory_setup(setup, "RehearsalToken");
    assert!(
        !setup.contains("deployRehearsalToken(deployer)"),
        "setUp must not reference undeclared deployer: {setup}"
    );
    assert!(
        !body.contains("deployRehearsalToken("),
        "test body must not redeploy via factory: {body}"
    );
    assert!(
        sol.contains("deployRehearsalToken(address(uint160(uint256(0x0000000000000000000000000000000000000000000000000000000000000d01)))")
            || sol.contains("deployRehearsalToken(deployer)"),
        "setUp must factory-deploy with deployer when ctor harness is active: {sol}"
    );
}

#[test]
fn smafd_canonical_det_msg_before_ctor_initialize_as_factory() {
    let files = transpile_evm_files("project.evm.yaml");
    let sol = files
        .get("test/RehearsalToken.t.sol")
        .expect("RehearsalToken.t.sol");
    assert!(
        !sol.contains("vm.prank") || sol.contains("vm.startPrank("),
        "canonical msg→constructor must not prank before initialize in det mode"
    );
    let fuzz_003 = sol
        .split("function testFuzz_PROP_RHT_003_TransferConservation")
        .nth(1)
        .and_then(|s| s.split("function ").next())
        .unwrap_or("");
    assert!(
        sol.contains("deployRehearsalToken(") && sol.contains("vm.startPrank("),
        "canonical det must factory-deploy in setUp and startPrank deployer in tests/fuzz"
    );
    let integration = sol
        .split("function test_integration_distribute_from_deployer_to_three_holders")
        .nth(1)
        .and_then(|s| s.split("function ").next())
        .unwrap_or("");
    assert!(
        !integration.contains("deployRehearsalToken("),
        "integration must not factory-redeploy: {integration}"
    );
    assert!(
        !fuzz_003.contains("deployRehearsalToken("),
        "fuzz body must not CREATE2-redeploy (collision); setUp owns deploy: {fuzz_003}"
    );
}

#[test]
fn smafd_canonical_forge_all_green() {
    if !has_forge() {
        eprintln!("skipping smafd_canonical_forge_all_green: forge not on PATH");
        return;
    }
    let out_dir = unique_out("forge");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("out dir");
    let files = transpile_evm_files("project.evm.yaml");
    write_project(&files, &out_dir);
    ensure_forge_std(&out_dir);
    let forge = Command::new("forge")
        .args(["test", "--root"])
        .arg(&out_dir)
        .output()
        .expect("forge test");
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );
    let _ = std::fs::remove_dir_all(&out_dir);
    assert!(
        forge.status.success(),
        "smafd_canonical forge suite must be all green:\n{log}"
    );
    assert!(
        log.contains("38 passed") || log.contains("39 passed") || log.contains("tests passed"),
        "expected canonical forge suite (38+ tests) all green:\n{log}"
    );
}

#[test]
fn smafd_canonical_invariant_ctor_mint_without_init() {
    let files = transpile_evm_files("project.evm.yaml");
    let inv_sol = files
        .get("test/Invariant_RehearsalToken_CP_002_ctor_mint_only_total_supply.t.sol")
        .expect("CP-002 invariant Foundry artifact");
    assert!(
        inv_sol.contains("Invariant_RehearsalToken_CP_002_ctor_mint_only_total_supply_Test"),
        "CP-002 must emit dedicated invariant test contract: {inv_sol}"
    );
    assert!(
        !inv_sol.contains("m_total_supply = INITIAL_SUPPLY"),
        "CP-002 must not pin state via init {{ }} — ctor-mint only: {inv_sol}"
    );
}
