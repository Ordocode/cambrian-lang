// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Public `stdlib/` contract gates — Foundry, ABI, vault mutants, token lake.
//!
//! Gated on `CAMBRIAN_TEST_STDLIB=1` (CI job `test-stdlib` / the public-tree
//! mirror). Lake additionally needs `CAMBRIAN_TEST_LEAN_BUILD=1`. Without
//! the env the binary is a no-op so `cargo test` and `test-public-surface`
//! stay the same length.

#[path = "audit/examples_common.rs"]
mod examples_common;

use examples_common::{
    combined_output, run_timed_secs, transpile_project, transpile_project_with, workspace_root,
};

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

const FORGE_TIMEOUT_SECS: u64 = 900;
const LAKE_TIMEOUT_SECS: u64 = 900;
const MUTATION_TIMEOUT_SECS: u64 = 2400;

fn stdlib_enabled() -> bool {
    std::env::var_os("CAMBRIAN_TEST_STDLIB").is_some_and(|v| v == "1")
}

fn lean_build_enabled() -> bool {
    std::env::var_os("CAMBRIAN_TEST_LEAN_BUILD").is_some_and(|v| v == "1")
}

fn has_forge() -> bool {
    Command::new("forge")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn has_git() -> bool {
    Command::new("git")
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

fn has_python3() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn require_stdlib_tools(need_python: bool) {
    assert!(
        has_forge() && has_git(),
        "CAMBRIAN_TEST_STDLIB=1 requires `forge` and `git` on PATH"
    );
    if need_python {
        assert!(
            has_python3(),
            "CAMBRIAN_TEST_STDLIB=1 requires `python3` for scripts/abi-check.py"
        );
    }
}

fn ensure_forge_std(build_dir: &Path) -> Result<(), String> {
    cambrian_transpiler::codegen::evm_test_codegen::install_forge_std(build_dir)
}

fn run_setup(build_dir: &Path) -> (bool, String) {
    let (output, ms) = run_timed_secs(
        {
            let mut cmd = Command::new("bash");
            cmd.arg("setup.sh").current_dir(build_dir);
            cmd
        },
        "setup.sh",
        180,
    );
    let mut combined = format!("wall_ms={ms}\n{}", combined_output(&output));
    if output.status.success() || build_dir.join("lib/forge-std").is_dir() {
        return (true, combined);
    }
    combined.push_str("\nsetup.sh failed; trying forge-std --no-git fallback\n");
    match ensure_forge_std(build_dir) {
        Ok(()) => {
            combined.push_str("fallback: forge-std installed\n");
            (true, combined)
        }
        Err(err) => {
            combined.push_str(&format!("fallback failed: {err}\n"));
            (false, combined)
        }
    }
}

fn parse_forge_passed_count(output: &str) -> Option<u32> {
    for line in output.lines().rev() {
        if let Some(idx) = line.find(" tests passed") {
            let prefix = &line[..idx];
            if let Some((_, count_str)) = prefix.rsplit_once(' ') {
                if let Ok(n) = count_str.parse::<u32>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

fn transpiler_bin(workspace: &Path) -> PathBuf {
    let release = workspace.join("target/release/cambrian-transpiler");
    let debug = workspace.join("target/debug/cambrian-transpiler");
    match (release.is_file(), debug.is_file()) {
        (true, true) => {
            let rt = std::fs::metadata(&release)
                .and_then(|m| m.modified())
                .ok();
            let dt = std::fs::metadata(&debug).and_then(|m| m.modified()).ok();
            if rt >= dt {
                release
            } else {
                debug
            }
        }
        (true, false) => release,
        _ => debug,
    }
}

fn run_abi_check(project_rel: &str, strict: bool) -> Result<String, String> {
    let workspace = workspace_root();
    let mut cmd = Command::new("python3");
    cmd.arg(workspace.join("scripts/abi-check.py"))
        .arg(workspace.join(project_rel))
        .current_dir(workspace);
    if strict {
        cmd.arg("--strict");
    }
    let (output, ms) = run_timed_secs(cmd, "abi-check", 60);
    let log = format!("wall_ms={ms}\n{}", combined_output(&output));
    if output.status.success() {
        Ok(log)
    } else {
        Err(log)
    }
}

fn run_project_forge(project_rel: &str, build_rel: &str) -> Result<(u32, String), String> {
    let workspace = workspace_root();
    let build_dir = workspace.join(build_rel);
    let (transpile_log, _) = transpile_project(workspace, project_rel)?;
    let (setup_ok, setup_log) = run_setup(&build_dir);
    if !setup_ok {
        return Err(format!("setup failed\n{transpile_log}\n{setup_log}"));
    }
    let (forge_output, forge_ms) = run_timed_secs(
        {
            let mut cmd = Command::new("forge");
            cmd.args(["test", "-vv"]).current_dir(&build_dir);
            cmd
        },
        "forge test",
        FORGE_TIMEOUT_SECS,
    );
    let forge_log = format!("wall_ms={forge_ms}\n{}", combined_output(&forge_output));
    if !forge_output.status.success() {
        return Err(format!(
            "forge test failed\n{transpile_log}\n{setup_log}\n{forge_log}"
        ));
    }
    let passed = parse_forge_passed_count(&forge_log).unwrap_or(0);
    Ok((
        passed,
        format!("{transpile_log}\n{setup_log}\n{forge_log}"),
    ))
}

#[test]
fn audit_stdlib_token_evm() {
    if !stdlib_enabled() {
        eprintln!("skip audit_stdlib_token_evm (set CAMBRIAN_TEST_STDLIB=1)");
        return;
    }
    require_stdlib_tools(true);
    let start = Instant::now();
    let (passed, forge_log) = run_project_forge("stdlib/token/project.yaml", "stdlib/token/build")
        .unwrap_or_else(|e| panic!("stdlib/token forge: {e}"));
    assert!(
        passed > 0,
        "stdlib/token forge reported no passing tests:\n{forge_log}"
    );
    run_abi_check("stdlib/token", false)
        .unwrap_or_else(|e| panic!("stdlib/token abi-check: {e}"));
    eprintln!(
        "stdlib/token EVM ALIGNED — forge tests_passed={passed}; wall={}ms",
        start.elapsed().as_millis()
    );
}

#[test]
fn audit_stdlib_vault_evm() {
    if !stdlib_enabled() {
        eprintln!("skip audit_stdlib_vault_evm (set CAMBRIAN_TEST_STDLIB=1)");
        return;
    }
    require_stdlib_tools(true);
    let start = Instant::now();
    let (passed, forge_log) = run_project_forge("stdlib/vault/project.yaml", "stdlib/vault/build")
        .unwrap_or_else(|e| panic!("stdlib/vault forge: {e}"));
    assert!(
        passed > 0,
        "stdlib/vault forge reported no passing tests:\n{forge_log}"
    );
    run_abi_check("stdlib/vault", true)
        .unwrap_or_else(|e| panic!("stdlib/vault abi-check --strict: {e}"));

    let workspace = workspace_root();
    let bin = transpiler_bin(workspace);
    assert!(
        bin.is_file(),
        "transpiler binary missing for mutation-check ({})",
        bin.display()
    );
    let (mut_output, mut_ms) = run_timed_secs(
        {
            let mut cmd = Command::new("bash");
            cmd.arg(workspace.join("scripts/mutation-check.sh"))
                .arg(workspace.join("stdlib/vault"))
                .env("CAMBRIAN_TRANSPILER", &bin)
                .current_dir(workspace);
            cmd
        },
        "mutation-check",
        MUTATION_TIMEOUT_SECS,
    );
    let mut_log = format!("wall_ms={mut_ms}\n{}", combined_output(&mut_output));
    assert!(
        mut_output.status.success(),
        "stdlib/vault mutation-check failed:\n{mut_log}"
    );
    eprintln!(
        "stdlib/vault EVM ALIGNED — forge tests_passed={passed}; mutants caught; wall={}ms",
        start.elapsed().as_millis()
    );
    let _ = forge_log;
}

#[test]
fn audit_stdlib_token_lake() {
    if !stdlib_enabled() {
        eprintln!("skip audit_stdlib_token_lake (set CAMBRIAN_TEST_STDLIB=1)");
        return;
    }
    if !lean_build_enabled() {
        eprintln!("skip audit_stdlib_token_lake (set CAMBRIAN_TEST_LEAN_BUILD=1)");
        return;
    }
    assert!(
        has_lake(),
        "CAMBRIAN_TEST_LEAN_BUILD=1 requires `lake` on PATH"
    );
    let start = Instant::now();
    transpile_project_with(
        workspace_root(),
        "stdlib/token/project.lean.yaml",
        &["--check-lean"],
        LAKE_TIMEOUT_SECS,
    )
    .unwrap_or_else(|e| panic!("stdlib/token lake: {e}"));
    eprintln!(
        "stdlib/token Lean ALIGNED — lake build OK; wall={}ms",
        start.elapsed().as_millis()
    );
}

#[test]
fn audit_stdlib_ci_wired() {
    let ci_path = workspace_root().join(".gitlab-ci.yml");
    if !ci_path.is_file() {
        eprintln!("skip audit_stdlib_ci_wired (.gitlab-ci.yml omitted from public snapshot)");
        return;
    }
    let ci = std::fs::read_to_string(&ci_path).expect("read .gitlab-ci.yml");
    assert!(
        ci.contains("\ntest-stdlib:"),
        "stdlib contract gates must run in GitLab job `test-stdlib`"
    );
    assert!(
        ci.contains("--test test_audit_stdlib"),
        "test-stdlib must invoke --test test_audit_stdlib"
    );
    assert!(
        ci.contains("\ntest-public-tree-stdlib:"),
        "public snapshot must mirror the stdlib job"
    );
}
