// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! SD-03-TEST-UP — `expect emit` in Cambrian `test` blocks (Foundry `vm.expectEmit`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const FIXTURE: &str = "../contracts/rehearsal_token_events_evm.cam";

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn unique_out_dir() -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-sd03-expect-emit-{}-{}",
        std::process::id(),
        n
    ))
}

fn transpiler_bin() -> PathBuf {
    let mut path = std::env::current_exe().expect("current_exe");
    path.pop();
    path.pop();
    path.push("cambrian-transpiler");
    path
}

fn has_forge() -> bool {
    Command::new("forge")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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

#[test]
fn sd03_expect_emit_lowers_to_vm_expect_emit() {
    let out_dir = unique_out_dir();
    let _ = fs::remove_dir_all(&out_dir);
    fs::create_dir_all(&out_dir).expect("out dir");

    let fixture = manifest_dir().join(FIXTURE);
    let output = Command::new(transpiler_bin())
        .arg(&fixture)
        .arg("-o")
        .arg(&out_dir)
        .arg("--target")
        .arg("evm")
        .output()
        .expect("transpile");
    assert!(
        output.status.success(),
        "transpile failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let test_sol = fs::read_to_string(out_dir.join("test/RehearsalTokenEvents.t.sol"))
        .expect("read generated test");
    assert!(
        test_sol.contains("vm.expectEmit(true, true, true, true)"),
        "missing vm.expectEmit in:\n{test_sol}"
    );
    assert!(
        test_sol.contains("emit RehearsalTokenEvents.Transfer("),
        "missing expected emit in:\n{test_sol}"
    );

    let _ = fs::remove_dir_all(&out_dir);
}

#[test]
fn sd03_expect_emit_forge_green() {
    if !has_forge() {
        panic!("forge required for sd03_expect_emit_forge_green");
    }

    let out_dir = unique_out_dir();
    let _ = fs::remove_dir_all(&out_dir);
    fs::create_dir_all(&out_dir).expect("out dir");

    let fixture = manifest_dir().join(FIXTURE);
    let output = Command::new(transpiler_bin())
        .arg(&fixture)
        .arg("-o")
        .arg(&out_dir)
        .arg("--target")
        .arg("evm")
        .output()
        .expect("transpile");
    assert!(output.status.success(), "transpile failed");

    fs::write(
        out_dir.join("foundry.toml"),
        r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
evm_version = "prague"
optimizer = false
"#,
    )
    .expect("foundry.toml");

    ensure_forge_std(&out_dir);

    let forge = Command::new("forge")
        .args([
            "test",
            "--match-test",
            "test_sd03_expect_emit_self_transfer_zero",
            "-vv",
            "--root",
        ])
        .arg(&out_dir)
        .output()
        .expect("forge test");

    let log = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );

    let _ = fs::remove_dir_all(&out_dir);

    assert!(forge.status.success(), "expect emit forge failed:\n{log}");
}
