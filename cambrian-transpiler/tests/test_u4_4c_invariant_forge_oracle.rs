// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! U4-4c T6 — opt-in forge oracle on handler-first invariant harness output.
//! Run: `CAMBRIAN_TEST_INVARIANT_FORGE=1 cargo test --release -p cambrian-transpiler --test test_u4_4c_invariant_forge_oracle`

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::{EvmSolidityBackend, OutputBackend};
use cambrian_transpiler::codegen::evm_test_codegen::install_forge_std;
use cambrian_transpiler::project::Project;

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

const FOUNDRY_TOML_FALLBACK: &str = r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
evm_version = "prague"
optimizer = false
"#;

/// (project YAML relative to repo root, short tag for temp dir names)
const ORACLE_PROJECTS: &[(&str, &str)] = &[
    (
        "cambrian-transpiler/tests/fixtures/u4_4c_governor_spike/project.yaml",
        "gov-multi",
    ),
    (
        "contracts/project_invariant_multi.yaml",
        "escrow-two-vaults",
    ),
    (
        "cambrian-transpiler/tests/audit/fixtures/pw3_invariant/multi_entity_trace_evm.yaml",
        "multi-entity-trace",
    ),
    ("stdlib/vault/project.yaml", "vault-multi"),
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn unique_out(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-u4-4c-inv-forge-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ))
}

fn forge_oracle_enabled() -> bool {
    std::env::var("CAMBRIAN_TEST_INVARIANT_FORGE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn has_forge() -> bool {
    Command::new("forge")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn transpile_evm(yaml_rel: &str) -> HashMap<String, String> {
    let yaml = repo_root().join(yaml_rel);
    let project = Project::load(&yaml).unwrap_or_else(|e| panic!("load {yaml_rel}: {e}"));
    EvmSolidityBackend {
        deterministic_addresses: true,
    }
    .gen_project(&project)
    .into_iter()
    .collect()
}

fn write_project_tree(files: &HashMap<String, String>, out_dir: &Path) {
    let _ = std::fs::remove_dir_all(out_dir);
    for (rel, contents) in files {
        let path = out_dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, contents).expect("write sol");
    }
    if !out_dir.join("foundry.toml").exists() {
        std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML_FALLBACK).expect("foundry.toml");
    }
    install_forge_std(out_dir).unwrap_or_else(|e| panic!("forge-std: {e}"));
}

fn run_forge_invariants(yaml_rel: &str, tag: &str) -> (bool, String) {
    let files = transpile_evm(yaml_rel);
    let has_inv = files.keys().any(|p| p.starts_with("test/Invariant_"));
    if !has_inv {
        return (
            false,
            format!("{yaml_rel}: no test/Invariant_*.t.sol emitted"),
        );
    }
    let out_dir = unique_out(tag);
    write_project_tree(&files, &out_dir);

    let forge = Command::new("forge")
        .args(["test", "--match-contract", "Invariant_", "-vv", "--root"])
        .arg(&out_dir)
        .env("FOUNDRY_INVARIANT_RUNS", "32")
        .output()
        .expect("forge test");
    let log = format!(
        "project={yaml_rel}\nroot={}\nstdout:\n{}\nstderr:\n{}",
        out_dir.display(),
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );
    let ok = forge.status.success();
    let _ = std::fs::remove_dir_all(&out_dir);
    (ok, log)
}

#[test]
fn u4_4c_t6_invariant_forge_oracle() {
    if !forge_oracle_enabled() {
        eprintln!("skip u4_4c_t6_invariant_forge_oracle (set CAMBRIAN_TEST_INVARIANT_FORGE=1)");
        return;
    }
    if !has_forge() {
        eprintln!("skip u4_4c_t6_invariant_forge_oracle (forge not on PATH)");
        return;
    }

    let mut failures = Vec::new();
    for (yaml_rel, tag) in ORACLE_PROJECTS {
        let (ok, log) = run_forge_invariants(yaml_rel, tag);
        if !ok {
            failures.push(log);
        }
    }
    assert!(
        failures.is_empty(),
        "T6 forge oracle failures:\n\n{}",
        failures.join("\n---\n")
    );
}
