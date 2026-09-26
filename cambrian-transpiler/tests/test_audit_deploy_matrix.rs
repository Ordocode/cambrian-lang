// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase N Wave-3 — deploy / address parity matrix (PW3-S-004).
//!
//! Rows: Native `compute_address` vs EVM CREATE2 (G-003 / PN-110, **TODO** with
//! G-015) and V45 semantic duplicate deploy (G-002, **FIXED**).
//! PW3-G-003 tests are not in GitLab `test-transpiler-phase-o-gates` (CI filters
//! to `pw3_g002`); the address-parity red gate is `#[ignore]`.
//!
//!   cargo test -p cambrian-transpiler --test test_audit_deploy_matrix -- --nocapture
//!   cargo test -p cambrian-transpiler --test test_audit_deploy_matrix pw3_g002 -- --nocapture

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::{
    EvmSolidityBackend, OutputBackend,
};
use cambrian_transpiler::project::Project;
use cambrian_transpiler::validate::{validate, Severity};

const FOUNDRY_TOML: &str = r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
evm_version = "prague"
optimizer = false
"#;

/// EVM CREATE2 `predictPredictTarget(42)` @ HEAD (re-measured PW3-S-004).
const EVM_PREDICT_TARGET_42: [u8; 20] = [
    0x87, 0x5f, 0xd8, 0xd4, 0x14, 0x66, 0x54, 0xc9, 0xd0, 0x16, 0x26, 0x03, 0xc1, 0x02, 0xec, 0x7d,
    0x14, 0xef, 0x42, 0x4a,
];

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn audit_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit")
}

fn fixtures_dir() -> PathBuf {
    audit_root().join("fixtures/pw3_deploy")
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-deploy-{}-{}-{}",
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

fn ensure_forge_std(out_dir: &Path) {
    cambrian_transpiler::codegen::evm_test_codegen::install_forge_std(out_dir)
        .unwrap_or_else(|e| panic!("{e} in {}", out_dir.display()));
}

fn load_yaml(name: &str) -> Project {
    let path = fixtures_dir().join(name);
    Project::load(&path).unwrap_or_else(|e| panic!("load {}: {e}", path.display()))
}

fn transpile_evm(yaml: &str) -> HashMap<String, String> {
    let project = load_yaml(yaml);
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    backend.gen_project(&project).into_iter().collect()
}

fn run_forge(yaml: &str, forge_file: &str, match_test: &str, tag: &str) -> (bool, String) {
    let out_dir = unique_out_dir(tag);
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("out dir");
    for (rel, contents) in transpile_evm(yaml) {
        let path = out_dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&path, contents).expect("write sol");
    }
    if !out_dir.join("foundry.toml").exists() {
        std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).expect("foundry.toml");
    }
    let test_dir = out_dir.join("test");
    std::fs::create_dir_all(&test_dir).expect("test dir");
    let src = audit_root().join("forge").join(forge_file);
    std::fs::copy(&src, test_dir.join(forge_file)).expect("copy forge");
    ensure_forge_std(&out_dir);
    let forge = Command::new("forge")
        .args(["test", "--match-test", match_test, "-vv", "--root"])
        .arg(&out_dir)
        .output()
        .expect("forge test");
    let combined = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );
    let ok = forge.status.success();
    let _ = std::fs::remove_dir_all(&out_dir);
    (ok, combined)
}

fn read_cam(name: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(name)).expect("read cam")
}

fn parse_cam(src: &str) -> cambrian_transpiler::ast::Program {
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    program
}

fn hex_addr(bytes: &[u8; 20]) -> String {
    format!("0x{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

// ---------------------------------------------------------------------------
// Smoke + documentation (green)
// ---------------------------------------------------------------------------

#[test]
fn pw3_deploy_matrix_smoke_fixtures_parse() {
    for name in ["predict_target.cam", "semantic_double_deploy.cam"] {
        let src = read_cam(name);
        parse_cam(&src);
        assert!(!src.is_empty());
    }
}

#[test]
fn pw3_g003_evm_codegen_exposes_predict_oracle() {
    let files = transpile_evm("predict_target_evm.yaml");
    let factory = files
        .values()
        .find(|s| s.contains("contract CambrianFactory"))
        .expect("CambrianFactory");
    assert!(
        factory.contains("predictPredictTarget"),
        "EVM must expose CREATE2 predict* runtime oracle:\n{factory}"
    );
}

#[test]
fn pw3_g002_v45_semantic_equal_deploy_is_error() {
    let src = read_cam("semantic_double_deploy.cam");
    let prog = parse_cam(&src);
    let diags = validate(&prog);
    let v45_errors = diags
        .iter()
        .filter(|d| d.code == "V45" && d.severity == Severity::Error)
        .count();
    assert!(
        v45_errors >= 1,
        "PW3-G-002: deploy Vault(0) + Vault(0+0) must be V45 error (const-fold equal)"
    );
}

// ---------------------------------------------------------------------------
// EVM ground truth (green)
// ---------------------------------------------------------------------------

#[test]
fn pw3_g003_evm_forge_predict_target_id42() {
    if !has_forge() {
        eprintln!("skip PW3-G-003 forge predict");
        return;
    }
    let (ok, log) = run_forge(
        "predict_target_evm.yaml",
        "Pw3PredictTarget.t.sol",
        "test_PW3_G003_predictTargetId42",
        "PW3-G-003",
    );
    assert!(ok, "PW3-G-003 EVM CREATE2 predict oracle:\n{log}");
}

#[test]
fn pw3_g002_evm_forge_semantic_duplicate_deploy_reverts() {
    if !has_forge() {
        eprintln!("skip PW3-G-002 forge");
        return;
    }
    let (ok, log) = run_forge(
        "semantic_double_deploy_evm.yaml",
        "Pw3SemanticDoubleDeploy.t.sol",
        "test_PW3_G002_semanticDuplicateDeployReverts",
        "PW3-G-002",
    );
    assert!(
        ok,
        "PW3-G-002 EVM: semantically equal duplicate deploy must revert:\n{log}"
    );
}

#[test]
fn pw3_g002_v45_should_error_on_semantic_duplicate_deploy() {
    let prog = parse_cam(&read_cam("semantic_double_deploy.cam"));
    let has_v45_error = validate(&prog)
        .iter()
        .any(|d| d.code == "V45" && d.severity == Severity::Error);
    assert!(
        has_v45_error,
        "PW3-G-002: validator should reject semantically equal duplicate deploys with V45 error"
    );
}

#[test]
fn pw3_g002_g003_todo_policy_self_check() {
    let native_src = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/test_audit_deploy_matrix_native.rs"
    );
    let src = match std::fs::read_to_string(native_src) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("PW3-G-003 skip: native deploy-matrix file omitted from public snapshot");
            return;
        }
    };
    assert!(
        src.contains("#[ignore = \"PW3-G-003 TODO with PW3-G-015: Native/EVM address parity only useful with Native/EVM integration\"]"),
        "PW3-G-003 address-parity red gate must stay ignored (TODO with G-015)"
    );
}
