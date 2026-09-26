// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase N Wave-3 — P7 admit-then-exec matrix (PW3-S-007).
//!
//! E07 error on closure-as-value / bare Range (PW3-G-007; mirrors Lean L2)
//! and W7 non-scalar fuzz/property fallback documentation (PW3-G-008).
//!
//!   cargo test -p cambrian-transpiler --test test_audit_p7_admit_matrix -- --nocapture

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::{EvmSolidityBackend, OutputBackend};
use cambrian_transpiler::project::Project;
use cambrian_transpiler::target::Target;
use cambrian_transpiler::validate::{
    self, check_evm_target_compat_with, check_target_compat, validate_project_config, Diagnostic,
    Severity,
};

const FOUNDRY_TOML: &str = r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
evm_version = "prague"
optimizer = false
"#;

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn audit_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit")
}

fn fixtures_dir() -> PathBuf {
    audit_root().join("fixtures/pw3_p7")
}

fn yaml(name: &str) -> PathBuf {
    fixtures_dir().join(name)
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-p7-{}-{}-{}",
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

fn load_project(yaml_rel: &Path) -> Project {
    Project::load(yaml_rel).expect("load project")
}

fn collect_evm_diagnostics(project: &Project) -> Vec<Diagnostic> {
    let mut diags = validate::validate(&project.merged);
    diags.extend(validate_project_config(&project.config));
    let det = project.config.resolved_deterministic_addresses();
    diags.extend(check_evm_target_compat_with(&project.merged, det));
    diags.extend(check_target_compat(&project.merged, Target::Evm, det));
    diags
}

fn has_e07_error(diags: &[Diagnostic]) -> bool {
    diags
        .iter()
        .any(|d| d.code == "E07" && d.severity == Severity::Error)
}

fn has_w7(diags: &[Diagnostic]) -> bool {
    diags.iter().any(|d| d.code == "W7")
}

fn transpile_evm(project: &Project) -> HashMap<String, String> {
    let det = project.config.resolved_deterministic_addresses();
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    backend.gen_project(project).into_iter().collect()
}

fn run_forge(
    yaml_rel: &Path,
    forge_file: &str,
    match_test: &str,
    tag: &str,
) -> (bool, String) {
    let out_dir = unique_out_dir(tag);
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("out dir");
    let project = load_project(yaml_rel);
    let files = transpile_evm(&project);
    for (rel, contents) in files {
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
    std::fs::copy(&src, test_dir.join(forge_file)).expect("copy forge test");
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

#[test]
fn pw3_p7_smoke_fixtures_parse() {
    for entry in std::fs::read_dir(fixtures_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "cam") {
            let src = std::fs::read_to_string(&path).unwrap();
            assert!(!src.is_empty(), "empty fixture {}", path.display());
        }
    }
}

#[test]
fn pw3_g007_validate_emits_e07_on_closure_admit() {
    let project = load_project(&yaml("closure_admit_evm.yaml"));
    let diags = collect_evm_diagnostics(&project);
    assert!(
        has_e07_error(&diags),
        "PW3-G-007: closure-as-value must emit E07 error:\n{diags:?}"
    );
}

#[test]
fn pw3_g007_validate_emits_e07_on_range_admit() {
    let project = load_project(&yaml("range_admit_evm.yaml"));
    let diags = collect_evm_diagnostics(&project);
    assert!(
        has_e07_error(&diags) && diags.iter().any(|d| d.message.contains("range")),
        "PW3-G-007: bare Range must emit E07 error:\n{diags:?}"
    );
}

#[test]
fn pw3_g007_evm_codegen_closure_revert_sentinel() {
    let project = load_project(&yaml("closure_admit_evm.yaml"));
    let files = transpile_evm(&project);
    let sol = files
        .values()
        .find(|s| s.contains("contract ClosureAdmit"))
        .or_else(|| files.get("src/_pw3-p7-closure-evm_project.sol"))
        .expect("closure sol");
    assert!(
        sol.contains("revert(\"EVM: closure-as-value not supported"),
        "PW3-G-007 must emit runtime revert sentinel:\n{sol}"
    );
}

#[test]
fn pw3_g007_evm_forge_closure_runtime_reverts() {
    if !has_forge() {
        eprintln!("skip PW3-G-007 closure revert forge");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("closure_admit_evm.yaml"),
        "Pw3P7ClosureRevert.t.sol",
        "test_PW3_G007_closureRuntimeRevert",
        "PW3-G-007-closure-revert",
    );
    assert!(
        ok,
        "PW3-G-007 baseline: closure route must revert at runtime:\n{log}"
    );
}

#[test]
fn pw3_g007_evm_forge_range_runtime_reverts() {
    if !has_forge() {
        eprintln!("skip PW3-G-007 range revert forge");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("range_admit_evm.yaml"),
        "Pw3P7RangeRevert.t.sol",
        "test_PW3_G007_rangeRuntimeRevert",
        "PW3-G-007-range-revert",
    );
    assert!(
        ok,
        "PW3-G-007 baseline: range route must revert at runtime:\n{log}"
    );
}

#[test]
fn pw3_g007_admit_then_exec_closure_should_not_revert() {
    let project = load_project(&yaml("closure_admit_evm.yaml"));
    let diags = collect_evm_diagnostics(&project);
    assert!(
        has_e07_error(&diags),
        "PW3-G-007: closure-as-value is rejected at validate (E07 error); no admit-then-exec:\n{diags:?}"
    );
}

#[test]
fn pw3_g007_admit_then_exec_range_should_not_revert() {
    let project = load_project(&yaml("range_admit_evm.yaml"));
    let diags = collect_evm_diagnostics(&project);
    assert!(
        has_e07_error(&diags),
        "PW3-G-007: bare Range is rejected at validate (E07 error); no admit-then-exec:\n{diags:?}"
    );
}

#[test]
fn pw3_g008_w7_documents_option_property_param() {
    let project = load_project(&yaml("w7_fuzz_option_evm.yaml"));
    let diags = collect_evm_diagnostics(&project);
    assert!(
        has_w7(&diags),
        "PW3-G-008 setup: Option property param must emit W7 on EVM:\n{diags:?}"
    );
}
