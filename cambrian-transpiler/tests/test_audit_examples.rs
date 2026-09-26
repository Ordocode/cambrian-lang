// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase G audit examples harness — real-world EVM anchors (T-G-001+).
//!
//! See [docs/AUDIT_EVM_LEAN.md](../../../docs/AUDIT_EVM_LEAN.md) §6 Phase G.

#[path = "audit/examples_common.rs"]
mod examples_common;

use examples_common::{
    audit_root, combined_output, run_timed, transpile_project, workspace_root,
};

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

struct ExampleForgeAnchor {
    id: &'static str,
    project: &'static str,
    build: &'static str,
    repro_subdir: &'static str,
}

const GOVERNOR: ExampleForgeAnchor = ExampleForgeAnchor {
    id: "T-G-001",
    project: "examples/governor/project.yaml",
    build: "examples/governor/build",
    repro_subdir: "governor_forge",
};

const UNISWAP_V2: ExampleForgeAnchor = ExampleForgeAnchor {
    id: "T-G-002",
    project: "examples/uniswap-v2/project.yaml",
    build: "examples/uniswap-v2/build",
    repro_subdir: "uniswap_v2_forge",
};

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

fn ensure_forge_std(build_dir: &Path) -> Result<(), String> {
    cambrian_transpiler::codegen::evm_test_codegen::install_forge_std(build_dir)
}

fn run_setup(build_dir: &Path) -> (bool, String, bool) {
    let (output, ms) = run_timed(
        {
            let mut cmd = Command::new("bash");
            cmd.arg("setup.sh").current_dir(build_dir);
            cmd
        },
        "setup.sh",
    );
    let mut combined = format!("wall_ms={ms}\n{}", combined_output(&output));
    let mut used_fallback = false;
    if !output.status.success() {
        combined.push_str(
            "\nTRACKED: setup.sh failed; trying forge-std --no-git fallback\n",
        );
        match ensure_forge_std(build_dir) {
            Ok(()) => {
                used_fallback = true;
                combined.push_str("fallback: forge-std installed\n");
            }
            Err(err) => combined.push_str(&format!("fallback failed: {err}\n")),
        }
    }
    let ok = output.status.success() || build_dir.join("lib/forge-std").is_dir();
    (ok, combined, used_fallback)
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

fn save_example_repro(
    anchor: &ExampleForgeAnchor,
    transpile_log: &str,
    setup_log: &str,
    forge_log: &str,
    verdict: &str,
) -> PathBuf {
    let repro_dir = audit_root()
        .join("examples_repro")
        .join(anchor.repro_subdir);
    let _ = std::fs::remove_dir_all(&repro_dir);
    std::fs::create_dir_all(&repro_dir).expect("create example repro dir");
    std::fs::write(repro_dir.join("transpile.log"), transpile_log).expect("write transpile.log");
    std::fs::write(repro_dir.join("setup.log"), setup_log).expect("write setup.log");
    std::fs::write(repro_dir.join("forge.log"), forge_log).expect("write forge.log");
    let note = format!(
        "{} {} forge anchor — {verdict}\n\
         project: {}\n\
         build dir: {}\n\
         \n\
         Re-run:\n\
           cargo run -p cambrian-transpiler --bin cambrian-transpiler --release -- --project {}\n\
           cd {} && bash setup.sh && forge test -vv\n",
        anchor.id,
        anchor.repro_subdir,
        anchor.project,
        anchor.build,
        anchor.project,
        anchor.build,
    );
    std::fs::write(repro_dir.join("NOTE.md"), note).expect("write NOTE.md");
    repro_dir
}

struct ExampleForgeRun {
    verdict: &'static str,
    transpile_ms: u128,
    setup_ms: u128,
    forge_ms: u128,
    tests_passed: Option<u32>,
    setup_fallback: bool,
    logs: String,
}

fn run_example_forge(anchor: &ExampleForgeAnchor) -> Result<ExampleForgeRun, ExampleForgeRun> {
    let workspace = workspace_root();
    let build_dir = workspace.join(anchor.build);

    let (transpile_log, transpile_ms) = match transpile_project(workspace, anchor.project) {
        Ok(v) => v,
        Err(log) => {
            let repro = save_example_repro(anchor, &log, "", "", "TRACKED transpile failed");
            return Err(ExampleForgeRun {
                verdict: "TRACKED",
                transpile_ms: 0,
                setup_ms: 0,
                forge_ms: 0,
                tests_passed: None,
                setup_fallback: false,
                logs: format!("repro: {}\n{log}", repro.display()),
            });
        }
    };

    let (setup_ok, setup_log, setup_fallback) = run_setup(&build_dir);
    let setup_ms = setup_log
        .lines()
        .find_map(|l| l.strip_prefix("wall_ms=").and_then(|n| n.parse().ok()))
        .unwrap_or(0);

    if !setup_ok {
        let repro = save_example_repro(anchor, &transpile_log, &setup_log, "", "TRACKED setup failed");
        return Err(ExampleForgeRun {
            verdict: "TRACKED",
            transpile_ms,
            setup_ms,
            forge_ms: 0,
            tests_passed: None,
            setup_fallback,
            logs: format!("repro: {}\n{setup_log}", repro.display()),
        });
    }

    let (forge_output, forge_ms) = run_timed(
        {
            let mut cmd = Command::new("forge");
            cmd.args(["test", "-vv"]).current_dir(&build_dir);
            cmd
        },
        "forge test",
    );
    let forge_log = format!("wall_ms={forge_ms}\n{}", combined_output(&forge_output));
    let tests_passed = parse_forge_passed_count(&forge_log);

    if forge_output.status.success() {
        Ok(ExampleForgeRun {
            verdict: "ALIGNED",
            transpile_ms,
            setup_ms,
            forge_ms,
            tests_passed,
            setup_fallback,
            logs: forge_log,
        })
    } else {
        let exit = forge_output.status.code();
        let verdict = if exit == Some(124) {
            "TRACKED timeout"
        } else {
            "CONFIRMED"
        };
        let repro = save_example_repro(anchor, &transpile_log, &setup_log, &forge_log, verdict);
        Err(ExampleForgeRun {
            verdict: if exit == Some(124) {
                "TRACKED"
            } else {
                "CONFIRMED"
            },
            transpile_ms,
            setup_ms,
            forge_ms,
            tests_passed,
            setup_fallback,
            logs: format!("repro: {}\n{forge_log}", repro.display()),
        })
    }
}

fn report_example_forge(
    anchor: &ExampleForgeAnchor,
    result: Result<ExampleForgeRun, ExampleForgeRun>,
    wall_ms: u128,
) {
    let repro_path = format!("tests/audit/examples_repro/{}", anchor.repro_subdir);
    match result {
        Ok(run) => {
            eprintln!(
                "{} ALIGNED — {} forge test PASS; tests_passed={:?}; \
                 transpile={}ms setup={}ms forge={}ms setup_fallback={}; wall={}ms",
                anchor.id,
                anchor.repro_subdir,
                run.tests_passed,
                run.transpile_ms,
                run.setup_ms,
                run.forge_ms,
                run.setup_fallback,
                wall_ms,
            );
        }
        Err(run) => {
            eprintln!(
                "{} {} — {} forge anchor FAIL; tests_passed={:?}; \
                 transpile={}ms setup={}ms forge={}ms setup_fallback={}; wall={}ms\n{}",
                anchor.id,
                run.verdict,
                anchor.repro_subdir,
                run.tests_passed,
                run.transpile_ms,
                run.setup_ms,
                run.forge_ms,
                run.setup_fallback,
                wall_ms,
                run.logs,
            );
            panic!(
                "{} {} — see logs above and {}",
                anchor.id, run.verdict, repro_path
            );
        }
    }
}

#[test]
fn audit_governor_forge() {
    if !has_forge() || !has_git() {
        eprintln!("T-G-001 SKIP — forge or git not on PATH");
        return;
    }
    let start = Instant::now();
    report_example_forge(&GOVERNOR, run_example_forge(&GOVERNOR), start.elapsed().as_millis());
}

#[test]
fn audit_uniswap_v2_forge() {
    if !has_forge() || !has_git() {
        eprintln!("T-G-002 SKIP — forge or git not on PATH");
        return;
    }
    let start = Instant::now();
    report_example_forge(
        &UNISWAP_V2,
        run_example_forge(&UNISWAP_V2),
        start.elapsed().as_millis(),
    );
}

/// T-G-004 CLOSED (2026-07-22): `UniswapV2Factory` is project-only (needs
/// `UniswapV2Pair` in program for `UniswapV2Pair.address(...)`). The Lean
/// completeness harness lists it in `standalone_skip` rather than
/// `known_gaps`; cover via `examples/uniswap-v2/project.lean.yaml`.
#[test]
fn audit_uniswap_v2_factory_project_only() {
    eprintln!(
        "T-G-004 CLOSED — Factory excluded from standalone completeness scan \
         (`standalone_skip` in test_lean_completeness.rs); known_gaps() empty"
    );
}
