// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! SD-02 layer B — PROP-CORE-008 infinite allowance on `smafd_sd02` fixture.
//!
//! Fixture: `tests/audit/fixtures/smafd_sd02/sd02_allowance_token.cam` +
//!          `sd02_infinite_allowance.cam`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::{EvmSolidityBackend, OutputBackend};
use cambrian_transpiler::project::Project;

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

const U256_MAX_HEX_NIBBLES: &str =
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/audit/fixtures/smafd_sd02")
}

fn unique_out(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-sd02-infinite-allowance-{}-{}-{}",
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

fn load_project(yaml_name: &str) -> Project {
    let yaml = fixture_dir().join(yaml_name);
    Project::load(&yaml).unwrap_or_else(|e| panic!("load {}: {e}", yaml.display()))
}

fn write_project(files: &[(String, String)], out_dir: &Path) {
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
fn sd02_infinite_allowance_property_transpiles_evm() {
    let project = load_project("project.evm.yaml");
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let files = EvmSolidityBackend {
        deterministic_addresses: det,
    }
    .gen_project(&project);
    let test_sol = files
        .iter()
        .find(|(p, _)| p.contains("Sd02AllowanceToken.t.sol"))
        .map(|(_, c)| c.as_str())
        .expect("Sd02AllowanceToken forge harness");
    assert!(
        test_sol.contains("PROP-CORE-008 InfiniteAllowance")
            || test_sol.contains("test_PROP_CORE_008_InfiniteAllowance")
            || test_sol.contains("testFuzz_PROP_CORE_008_InfiniteAllowance"),
        "PROP-CORE-008 property must lower to forge harness:\n{test_sol}"
    );
    assert!(
        test_sol.contains(U256_MAX_HEX_NIBBLES),
        "approve(U256::MAX) must lower full-width in harness:\n{test_sol}"
    );
    let project_sol = files
        .iter()
        .find(|(p, _)| p.contains("_project.sol"))
        .map(|(_, c)| c.as_str())
        .expect("combined project .sol");
    assert!(
        project_sol.contains("spend_allowance") || project_sol.contains("spendAllowance"),
        "infinite allowance helper must appear in entity lowering:\n{project_sol}"
    );
}

#[test]
fn sd02_infinite_allowance_forge_green() {
    if !has_forge() {
        eprintln!("skipping sd02_infinite_allowance_forge_green: forge not on PATH");
        return;
    }
    let out_dir = unique_out("forge");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("out dir");
    let project = load_project("project.evm.yaml");
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let files = EvmSolidityBackend {
        deterministic_addresses: det,
    }
    .gen_project(&project);
    write_project(&files, &out_dir);
    ensure_forge_std(&out_dir);
    let forge = Command::new("forge")
        .args(["test", "--match-contract", "Sd02AllowanceTokenTest", "--root"])
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
        "Sd02AllowanceToken forge suite must pass (PROP-CORE-008 layer B):\n{log}"
    );
    assert!(
        log.contains("PROP-CORE-008") || log.contains("InfiniteAllowance"),
        "forge log must mention PROP-CORE-008 property:\n{log}"
    );
    assert!(
        log.contains("finite allowance still decrements") || log.contains("finite_allowance"),
        "control test must run alongside infinite allowance:\n{log}"
    );
}
