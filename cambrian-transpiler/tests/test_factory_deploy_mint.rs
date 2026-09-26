// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! BUG-U4 — factory `deploy*(holder)` mint recipient regression (U4-0 / U4-1 / U4-3).

#[path = "harness/mod.rs"]
mod harness;

use std::path::{Path, PathBuf};

use harness::factory_oracle::{assert_factory_setup, assert_no_sut_legacy_new, extract_setup_sol};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::evm_test_codegen::generate_evm_tests_for_project;
use cambrian_transpiler::codegen::{EvmSolidityBackend, OutputBackend};
use cambrian_transpiler::project::{InvariantConfig, Project};
use cambrian_transpiler::validate::{check_evm_target_compat_with, Severity};

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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/factory_deploy_mint")
}

fn audit_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/audit")
}

fn unique_out(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-factory-mint-{}-{}-{}",
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

fn transpile_evm() -> std::collections::HashMap<String, String> {
    let yaml = fixture_dir().join("project.yaml");
    let project = Project::load(&yaml).expect("load factory_deploy_mint project");
    EvmSolidityBackend {
        deterministic_addresses: true,
    }
    .gen_project(&project)
    .into_iter()
    .collect()
}

fn project_combined_sol(files: &std::collections::HashMap<String, String>) -> String {
    files
        .iter()
        .find(|(p, _)| p.contains("_project.sol"))
        .map(|(_, c)| c.as_str())
        .expect("combined project sol")
        .to_string()
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
        .args(["install", "foundry-rs/forge-std", "--no-git"])
        .current_dir(out_dir)
        .status()
        .expect("forge install");
    assert!(status.success(), "forge install forge-std failed");
}

fn run_forge_probe(match_test: &str) -> (bool, String) {
    let out_dir = unique_out("BUG-U4");
    let files = transpile_evm();
    std::fs::create_dir_all(out_dir.join("src")).unwrap();
    std::fs::create_dir_all(out_dir.join("test")).unwrap();
    std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).unwrap();
    ensure_forge_std(&out_dir);
    for (rel, content) in &files {
        let path = out_dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }
    std::fs::copy(
        audit_root().join("forge/factory_deploy_mint_probe.t.sol"),
        out_dir.join("test/factory_deploy_mint_probe.t.sol"),
    )
    .expect("copy forge probe");
    let output = Command::new("forge")
        .args(["test", "--match-test", match_test, "-vv"])
        .current_dir(&out_dir)
        .output()
        .expect("forge test");
    let log = String::from_utf8_lossy(&output.stderr).to_string()
        + &String::from_utf8_lossy(&output.stdout);
    (output.status.success(), log)
}

#[test]
fn bug_u4_harness_setup_uses_cambrian_factory_deploy() {
    let yaml = fixture_dir().join("project.yaml");
    let project = Project::load(&yaml).expect("load factory_deploy_mint project");
    let mut merged = project.merged.clone();
    let test_src = r#"
test "holder view" for MintProbe {
    call holder()
}
"#;
    let test_prog = cambrian_transpiler::ProgramParser::new()
        .parse(test_src)
        .expect("parse harness test");
    merged.tests.extend(test_prog.tests);
    let files = generate_evm_tests_for_project(
        &merged,
        true,
        &InvariantConfig::default(),
        Some(project.name().as_str()),
        false,
    );
    let sol = files
        .iter()
        .find(|(p, _)| p == "test/MintProbe.t.sol")
        .map(|(_, c)| c.as_str())
        .expect("MintProbe.t.sol");
    let setup = extract_setup_sol(sol);
    assert_factory_setup(setup, "MintProbe");
    assert_no_sut_legacy_new(setup, "MintProbe");
}

#[test]
fn bug_u4_v62_rejects_msg_sender_in_init_transform() {
    let src = r"entity BadMint {
    routes { constructor() => [] }
    m_owner: address { in constructor() => msg::sender }
}";
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .expect("parse BadMint");
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    let codes = check_evm_target_compat_with(&program, true)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect::<Vec<_>>();
    assert!(
        codes.contains(&"V62"),
        "BUG-U4 class (RHT.cam) must trip V62 under deterministic mode, got {:?}",
        codes
    );
}

#[test]
fn bug_u4_emit_initialize_path_not_ctor_args_in_new() {
    let combined = project_combined_sol(&transpile_evm());
    assert!(
        combined.contains("function deployMintProbe(address holder)"),
        "deploy must expose init-route param:\n{combined}"
    );
    assert!(
        combined.contains("new MintProbe{salt: bytes32(0), value:")
            && combined.contains("(address(this))"),
        "CREATE2 new must pass only factory_ (+ identity), not holder:\n{combined}"
    );
    assert!(
        combined.contains("_instance.initialize(holder)"),
        "holder must flow through initialize(), not new args:\n{combined}"
    );
    assert!(
        combined.contains("function predictMintProbe()"),
        "predict must remain identity-only:\n{combined}"
    );
}

#[test]
fn bug_u4_forge_factory_deploy_mints_to_holder() {
    if !has_forge() {
        eprintln!("skip BUG-U4 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge_probe("test_factory_deploy_mints_to_holder_not_factory");
    assert!(
        ok,
        "BUG-U4: factory.deployMintProbe(holder) must credit holder (U4-0):\n{log}"
    );
}
