// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! S6 — factory deploy mint recipient via explicit ctor param (BUG-U4 / Session A).
//!
//! Fixture: `tests/audit/fixtures/smafd_s6/`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::{EvmSolidityBackend, OutputBackend};
use cambrian_transpiler::project::Project;

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/audit/fixtures/smafd_s6")
}

fn unique_out(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-smafd-s6-{}-{}-{}",
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

fn transpile(yaml_name: &str) -> (Project, Vec<(String, String)>) {
    let yaml = fixture_dir().join(yaml_name);
    let project = Project::load(&yaml).unwrap_or_else(|e| panic!("load {}: {e}", yaml.display()));
    let det = project.config.resolved_deterministic_addresses();
    let files = EvmSolidityBackend {
        deterministic_addresses: det,
    }
    .gen_project(&project);
    (project, files)
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

fn run_forge(out_dir: &Path) -> String {
    let output = Command::new("forge")
        .args(["test", "-vv"])
        .current_dir(out_dir)
        .output()
        .expect("forge test");
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "forge test must pass for canonical param-less ctor harness:\n{log}"
    );
    log
}

#[test]
fn smafd_s6_det_codegen_factory_deploy_in_setup() {
    let (_project, files) = transpile("project.evm.det.yaml");
    let sol = files
        .iter()
        .find(|(p, _)| p == "test/S6Token.t.sol")
        .map(|(_, c)| c.as_str())
        .expect("S6Token.t.sol");
    assert!(
        sol.contains("new CambrianFactory()"),
        "det setUp must deploy CambrianFactory: {sol}"
    );
    assert!(
        sol.contains("deployS6Token("),
        "det harness must use factory.deployS6Token: {sol}"
    );
    assert!(
        !sol.contains("skipped: `call constructor"),
        "canonical ctor call must not be silently skipped: {sol}"
    );
    assert!(
        sol.contains("deployS6Token(") && sol.contains("0d01"),
        "hoisted det setUp must factory-deploy with deployer recipient: {sol}"
    );
}

#[test]
fn smafd_s6_det_forge_canonical_ctor_mint() {
    if !has_forge() {
        eprintln!("skipping smafd_s6_det_forge_canonical_ctor_mint: forge not on PATH");
        return;
    }
    let (_project, files) = transpile("project.evm.det.yaml");
    let out = unique_out("det");
    write_project(&files, &out);
    ensure_forge_std(&out);
    let log = run_forge(&out);
    assert!(
        log.contains("1 passed") || log.contains("2 passed") || log.contains("3 passed"),
        "expected S6Token tests green: {log}"
    );
}

