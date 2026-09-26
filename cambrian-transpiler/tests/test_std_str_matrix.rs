// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Cross-target semantic gate for `std::str::parse_*` (T-STD-STR-MATRIX).
//!
//! One fixture (`std_str_matrix.cam`) must pass on **every** emission target:
//! EVM (forge), Lean (lake), Container/Rust (`cargo check`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::ast;
use cambrian_transpiler::codegen::{
    EvmSolidityBackend, LeanBackend, OutputBackend,
};
use cambrian_transpiler::project::Project;
use cambrian_transpiler::validate::{check_lean_target_compat, validate, Severity};

const MATRIX_CAM: &str = include_str!("fixtures/std_str_matrix.cam");
const FOUNDRY_TOML: &str = r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
evm_version = "prague"
optimizer = false
optimizer_runs = 200
via_ir = false
"#;

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-std-str-matrix-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ))
}

fn parse_matrix() -> ast::Program {
    let mut p = cambrian_transpiler::ProgramParser::new()
        .parse(MATRIX_CAM)
        .expect("std_str_matrix.cam must parse");
    ast::normalize_program_types(&mut p);
    p
}

fn blocking_codes(prog: &ast::Program) -> Vec<&'static str> {
    validate(prog)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect()
}

fn has_forge() -> bool {
    Command::new("forge")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn has_lake() -> bool {
    Command::new("lake")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ensure_forge_std(out_dir: &Path) {
    cambrian_transpiler::codegen::evm_test_codegen::install_forge_std(out_dir)
        .unwrap_or_else(|e| panic!("{e} in {}", out_dir.display()));
}

fn transpile_evm(out_dir: &Path) {
    let yaml = fixtures_dir().join("std_str_matrix.yaml");
    let project = Project::load(&yaml).expect("load std_str_matrix.yaml");
    let det = project.config.resolved_deterministic_addresses();
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    for (rel, contents) in backend.gen_project(&project) {
        let path = out_dir.join(&rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }
}

fn lean_blocking_codes(prog: &ast::Program) -> Vec<String> {
    let mut codes: Vec<String> = validate(prog)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code.to_string())
        .collect();
    codes.extend(
        check_lean_target_compat(prog)
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.code.to_string()),
    );
    codes
}

fn transpile_lean(out_dir: &Path) {
    let yaml = fixtures_dir().join("std_str_matrix_lean.yaml");
    let project = Project::load(&yaml).expect("load std_str_matrix_lean.yaml");
    let codes = lean_blocking_codes(&project.merged);
    assert!(
        codes.is_empty(),
        "lean validator blocked std_str_matrix: {:?}",
        codes
    );
    for (rel, contents) in LeanBackend::default().gen_project(&project) {
        let path = out_dir.join(&rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }
}

#[test]
fn std_str_matrix_validator_accepts_fixture() {
    let codes = blocking_codes(&parse_matrix());
    assert!(codes.is_empty(), "validator blocked matrix: {:?}", codes);
}

#[test]
fn std_str_matrix_evm_forge() {
    if !has_forge() {
        eprintln!(
            "skipping std_str_matrix_evm_forge: forge not on PATH \
             (coverage / non-Foundry CI images; T-STD-STR-MATRIX EVM gate \
             lives in test-transpiler-phase-m-gates)"
        );
        return;
    }
    let out_dir = unique_out_dir("evm");
    fs::create_dir_all(&out_dir).unwrap();
    fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).unwrap();
    ensure_forge_std(&out_dir);
    transpile_evm(&out_dir);

    let forge_test = fixtures_dir().join("forge/StdStrMatrix.t.sol");
    fs::create_dir_all(out_dir.join("test")).unwrap();
    fs::copy(forge_test, out_dir.join("test/StdStrMatrix.t.sol")).unwrap();

    let log = Command::new("forge")
        .args(["test", "--match-contract", "StdStrMatrixTest", "-vv"])
        .current_dir(&out_dir)
        .output()
        .expect("forge test");
  let stdout = String::from_utf8_lossy(&log.stdout);
    assert!(
        log.status.success(),
        "forge failed for std_str_matrix:\nstdout:\n{}\nstderr:\n{}",
        stdout,
        String::from_utf8_lossy(&log.stderr)
    );
    let _ = fs::remove_dir_all(&out_dir);
}

#[test]
fn std_str_matrix_lean_lake() {
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() != Ok("1") {
        eprintln!("skipping std_str_matrix_lean_lake (set CAMBRIAN_TEST_LEAN_BUILD=1)");
        return;
    }
    if !has_lake() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 but lake not on PATH");
    }
    let out_dir = unique_out_dir("lean");
    fs::create_dir_all(&out_dir).unwrap();
    transpile_lean(&out_dir);

    let routes = fs::read_to_string(out_dir.join("Cambrian/Generated/StdStrMatrixRoutes.lean"))
        .expect("StdStrMatrixRoutes.lean");
    assert!(routes.contains("parseRadixNat?"), "missing parseRadixNat?:\n{}", routes);
    assert!(
        routes.contains("parseRadixSignedNat?"),
        "missing parseRadixSignedNat?:\n{}",
        routes
    );
    assert!(
        routes.contains("formatNat") || routes.contains("Cambrian.format"),
        "missing format lowering in Lean routes:\n{}",
        routes
    );

    let lake_out = Command::new("lake")
        .arg("build")
        .current_dir(&out_dir)
        .output()
        .expect("lake build");
    assert!(
        lake_out.status.success(),
        "lake build failed:\n{}\n{}",
        String::from_utf8_lossy(&lake_out.stdout),
        String::from_utf8_lossy(&lake_out.stderr)
    );
    let _ = fs::remove_dir_all(&out_dir);
}
