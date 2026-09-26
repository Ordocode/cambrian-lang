// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM semantic gate for phase-1 `std::math` (STD-EVM-MATRIX-1, docs/STDLIB.md §3).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::{EvmSolidityBackend, OutputBackend};
use cambrian_transpiler::project::Project;

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

fn unique_out_dir() -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-std-evm-matrix-{}-{}",
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

fn transpile_project(yaml_path: &Path, out_dir: &Path) {
    let project = Project::load(yaml_path).expect("load project");
    let det = project.config.resolved_deterministic_addresses();
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    for (rel, contents) in backend.gen_project(&project) {
        let path = out_dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }
}

#[test]
fn std_evm_math_matrix_forge_gate() {
    if !has_forge() {
        eprintln!("skipping std_evm_math_matrix_forge_gate (`forge` not on PATH)");
        return;
    }

    let out_dir = unique_out_dir();
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).unwrap();

    let yaml = fixtures_dir().join("std_math_matrix.yaml");
    transpile_project(&yaml, &out_dir);

    std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).unwrap();

    let test_dir = out_dir.join("test");
    std::fs::create_dir_all(&test_dir).unwrap();
    let src_test = fixtures_dir().join("forge/StdMathMatrix.t.sol");
    std::fs::copy(&src_test, test_dir.join("StdMathMatrix.t.sol")).unwrap();

    ensure_forge_std(&out_dir);

    let forge = Command::new("forge")
        .args(["test", "--match-contract", "StdMathMatrixTest", "-vv", "--root"])
        .arg(&out_dir)
        .output()
        .expect("forge test");

    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );

    let _ = std::fs::remove_dir_all(&out_dir);

    assert!(
        forge.status.success(),
        "STD-EVM-MATRIX-1 forge gate failed:\n{log}"
    );
}
