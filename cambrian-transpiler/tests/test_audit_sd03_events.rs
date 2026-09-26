// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! SD-03 / SMAFD BLS §9 — rehearsal token events reference corpus (forge log oracle).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const FIXTURE: &str = "../contracts/rehearsal_token_events_evm.cam";
const FORGE_TEST: &str = "tests/audit/forge/Sd03RehearsalTokenEvents.t.sol";
const FORGE_CONTRACT: &str = "Sd03RehearsalTokenEventsTest";

const FOUNDRY_TOML: &str = r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
evm_version = "prague"
optimizer = false
"#;

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn unique_out_dir() -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-sd03-{}-{}",
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
        .args(["install", "foundry-rs/forge-std"])
        .current_dir(out_dir)
        .status()
        .expect("forge install");
    assert!(status.success(), "forge install forge-std failed");
}

fn transpile_fixture(out_dir: &Path) {
    let fixture = manifest_dir().join(FIXTURE);
    let output = Command::new(transpiler_bin())
        .arg(&fixture)
        .arg("-o")
        .arg(out_dir)
        .arg("--target")
        .arg("evm")
        .output()
        .expect("transpiler");
    assert!(
        output.status.success(),
        "transpile {} failed:\n{}",
        fixture.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn evm_sd03_rehearsal_events_sol_lower() {
    let out_dir = unique_out_dir();
    let _ = fs::remove_dir_all(&out_dir);
    fs::create_dir_all(&out_dir).expect("out dir");
    transpile_fixture(&out_dir);

    // Entity `src/<Name>.sol` is a re-export stub once a test harness is
    // emitted; the contract body lives in `src/_Harness_project.sol`.
    let mut sol = String::new();
    for entry in fs::read_dir(out_dir.join("src")).expect("src") {
        let path = entry.expect("src entry").path();
        if path.extension().is_some_and(|ext| ext == "sol") {
            sol.push_str(&fs::read_to_string(&path).expect("read sol"));
            sol.push('\n');
        }
    }
    assert!(!sol.is_empty(), "no generated .sol under {}", out_dir.display());

    for decl in [
        "event Transfer(address indexed src, address indexed dst, uint256 value);",
        "event Approval(address indexed owner, address indexed spender, uint256 value);",
        "event Whitelisted(address indexed account, bool status);",
        "event Claimed(address indexed participant, uint256 amount);",
    ] {
        assert!(sol.contains(decl), "missing {decl} in:\n{sol}");
    }

    for emit in [
        "emit Transfer(ZERO, holder, initial_supply);",
        "emit Transfer(msg.sender, to, amount);",
        "emit Approval(msg.sender, spender, amount);",
        "emit Transfer(msg.sender, ZERO, amount);",
        "emit Whitelisted(account, status);",
        "emit Claimed(msg.sender, CLAIM_AMOUNT);",
        "emit Transfer(m_treasury, msg.sender, CLAIM_AMOUNT);",
        "m_whitelisted_exists[",
        "m_balances_exists[",
        "] = true;",
    ] {
        assert!(sol.contains(emit), "missing `{emit}` in:\n{sol}");
    }

    let _ = fs::remove_dir_all(&out_dir);
}

#[test]
fn evm_sd03_rehearsal_forge_logs() {
    if !has_forge() {
        eprintln!("skip evm_sd03_rehearsal_forge_logs: forge not on PATH");
        return;
    }

    let out_dir = unique_out_dir();
    let _ = fs::remove_dir_all(&out_dir);
    fs::create_dir_all(&out_dir).expect("out dir");
    transpile_fixture(&out_dir);
    fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).expect("foundry.toml");

    let test_dir = out_dir.join("test");
    fs::create_dir_all(&test_dir).expect("test dir");
    fs::copy(
        manifest_dir().join(FORGE_TEST),
        test_dir.join("Sd03RehearsalTokenEvents.t.sol"),
    )
    .expect("copy forge test");

    ensure_forge_std(&out_dir);

    let forge = Command::new("forge")
        .args([
            "test",
            "--match-contract",
            FORGE_CONTRACT,
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

    assert!(
        forge.status.success(),
        "SD-03 forge log oracle failed:\n{log}"
    );
}
