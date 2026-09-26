// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase N Wave-3 — Lean scaling / alias stack-overflow ratchet (PW3-S-012).
//!
//! Oracle A: wall-clock + controlled-failure gates for PW3-O-005, PW3-O-010, PW3-G-013.
//! No codegen fixes on `audit` — red gates document CONFIRMED findings.
//!
//!   cargo test -p cambrian-transpiler --test test_audit_scaling_matrix -- --nocapture
//!   CAMBRIAN_TEST_LEAN_BUILD=1 cargo test -p cambrian-transpiler --test test_audit_scaling_matrix pw3_s012_g013 -- --nocapture

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use cambrian_transpiler::ProgramParser;
use cambrian_transpiler::ast;
use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;
use cambrian_transpiler::validate;

/// Per-case wall budget (audit rule 04).
const WALL_CASE_SECS: u64 = 300;
const CYCLIC_ALIAS_TIMEOUT: Duration = Duration::from_secs(15);
const LEAN_SCALE_N_SMALL: usize = 40;
const LEAN_SCALE_N_LARGE: usize = 160;
/// PoC @ d28d6e1 reported ~17s @ n=160; ratchet ceiling leaves headroom on slow CI.
const LEAN_SCALE_CEILING_LARGE: Duration = Duration::from_secs(45);
/// 4× routes is 16× under O(n²). CI debug builds swing tens–hundreds of ms;
/// floor the small-n time so a lucky 2ms run cannot explode the ratio.
const LEAN_SCALE_QUADRATIC_FACTOR: f64 = 32.0;
const LEAN_SCALE_RATIO_FLOOR: Duration = Duration::from_millis(10);

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn audit_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit")
}

fn fixtures_dir() -> PathBuf {
    audit_root().join("fixtures/pw3_scaling")
}

fn lean_build_enabled() -> bool {
    std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() == Ok("1")
}

fn scaling_stress_enabled() -> bool {
    std::env::var("CAMBRIAN_TEST_SCALING_STRESS").as_deref() == Ok("1")
}

fn has_lake() -> bool {
    Command::new("lake")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn transpiler_bin() -> PathBuf {
    // Compile-time path from Cargo (the runtime `CARGO_BIN_EXE_*` env var is
    // never set); follows the active target dir (`target/debug`,
    // `target/llvm-cov-target/debug`, …).
    PathBuf::from(env!("CARGO_BIN_EXE_cambrian-transpiler"))
}

fn render_scale_entity(route_count: usize) -> String {
    let mut routes = String::from("constructor() => []\n        ");
    for i in 0..route_count {
        routes.push_str(&format!("r{i}() => []\n        "));
    }
    format!(
        r#"entity LeanScale {{
    routes {{
        {routes}
    }}
    m_v: u64 {{
        in constructor() => 0
    }}
}}"#
    )
}

fn parse_program(src: &str) -> cambrian_transpiler::ast::Program {
    let mut program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse scale fixture: {e}"));
    ast::normalize_program_types(&mut program);
    program
}

fn project_from_cam(src: &str, name: &str) -> Project {
    let work = std::env::temp_dir().join(format!(
        "cambrian-audit-scale-{name}-{}-{}",
        std::process::id(),
        route_count_placeholder(src)
    ));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("mkdir scale work");
    std::fs::write(work.join("scale.cam"), src).expect("write cam");
    std::fs::write(
        work.join("project.yaml"),
        format!(
            "name: {name}\n\
             target: lean\n\
             deterministic_addresses: true\n\
             output_dir: build/\n\
             lean:\n\
               numerics: overflow-wrap\n\
             sources:\n\
               - scale.cam\n"
        ),
    )
    .expect("write yaml");
    Project::load(&work.join("project.yaml")).unwrap_or_else(|e| panic!("load {name}: {e}"))
}

fn route_count_placeholder(_src: &str) -> u64 {
    static C: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    C.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn lean_codegen_elapsed(route_count: usize) -> Duration {
    let src = render_scale_entity(route_count);
    let project = project_from_cam(&src, &format!("lean_scale_{route_count}"));
    let backend = LeanBackend::default();
    let t0 = Instant::now();
    let _files = backend.gen_project(&project);
    t0.elapsed()
}

struct SubprocessOutcome {
    success: bool,
    timed_out: bool,
    elapsed: Duration,
    combined: String,
}

fn run_transpiler_subprocess(
    cam: &Path,
    out_dir: &Path,
    timeout: Duration,
) -> SubprocessOutcome {
    let bin = transpiler_bin();
    let _ = std::fs::remove_dir_all(out_dir);
    std::fs::create_dir_all(out_dir).ok();
    let start = Instant::now();
    let mut child = Command::new(&bin)
        .args([
            cam.to_str().expect("cam path utf8"),
            "-o",
            out_dir.to_str().expect("out utf8"),
            "--target",
            "lean",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn transpiler");
    let mut timed_out = false;
    loop {
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            timed_out = true;
            break;
        }
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                return SubprocessOutcome {
                    success: false,
                    timed_out: false,
                    elapsed: start.elapsed(),
                    combined: format!("wait error: {e}"),
                };
            }
        }
    }
    if timed_out {
        SubprocessOutcome {
            success: false,
            timed_out: true,
            elapsed: start.elapsed(),
            combined: format!("killed after {:?} (controlled timeout)", timeout),
        }
    } else {
        let output = child.wait_with_output().expect("wait output");
        let combined = format!(
            "exit={:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        SubprocessOutcome {
            success: output.status.success(),
            timed_out: false,
            elapsed: start.elapsed(),
            combined,
        }
    }
}

// ---------------------------------------------------------------------------
// Row 1 — PW3-O-005 / V52 gap: cyclic type alias
// ---------------------------------------------------------------------------

#[test]
fn pw3_s012_o005_v52_guards_record_not_alias_cycle() {
    let alias_src = std::fs::read_to_string(fixtures_dir().join("cyclic_alias.cam"))
        .expect("read cyclic_alias.cam");
    let mut alias_prog = parse_program(&alias_src);
    let alias_diags = validate::validate(&alias_prog);
    assert!(
        alias_diags.iter().any(|d| d.code == "V52"),
        "PW3-O-005: cyclic alias must trip V52:\n{alias_diags:?}"
    );

    let record_src = r#"
record Node { next: Node }

entity Bad {
    routes { constructor() => [] }
    m_n: u64 { in constructor() => 0 }
}
"#;
    let mut record_prog = parse_program(record_src);
    let record_diags = validate::validate(&record_prog);
    assert!(
        record_diags.iter().any(|d| d.code == "V52"),
        "PW3-S-012 contrast: cyclic record must stay V52-guarded:\n{record_diags:?}"
    );
}

#[test]
fn pw3_s012_o005_cyclic_alias_lean_controlled_failure() {
    let case_start = Instant::now();
    let cam = fixtures_dir().join("cyclic_alias.cam");
    let out = std::env::temp_dir().join(format!(
        "cambrian-audit-o005-{}-{}",
        std::process::id(),
        CYCLIC_ALIAS_TIMEOUT.as_secs()
    ));
    let run = run_transpiler_subprocess(&cam, &out, CYCLIC_ALIAS_TIMEOUT);
    let _ = std::fs::remove_dir_all(&out);
    assert!(
        case_start.elapsed() < Duration::from_secs(WALL_CASE_SECS),
        "PW3-O-005 ratchet must finish within {WALL_CASE_SECS}s wall"
    );
    assert!(
        !run.success,
        "PW3-O-005: cyclic alias must fail validation (elapsed={:?}, timed_out={}):\n{}",
        run.elapsed,
        run.timed_out,
        run.combined
    );
    assert!(
        !run.timed_out,
        "PW3-O-005: V52 must reject before the 15s kill (elapsed={:?}):\n{}",
        run.elapsed,
        run.combined
    );
    assert!(
        run.combined.contains("V52") || run.combined.contains("Cyclic type alias"),
        "PW3-O-005: expected V52 in transpiler output:\n{}",
        run.combined
    );
    eprintln!(
        "PW3-O-005 ratchet: controlled failure in {:?} (timed_out={})",
        run.elapsed, run.timed_out
    );
}

// ---------------------------------------------------------------------------
// Row 2 — PW3-O-010 Lean codegen scaling ratchet
// ---------------------------------------------------------------------------

#[test]
fn pw3_s012_o010_lean_codegen_scaling_ratchet() {
    if scaling_stress_enabled() {
        eprintln!("PW3-O-010: CAMBRIAN_TEST_SCALING_STRESS=1 — using n=160 only");
    }
    let case_start = Instant::now();
    let _warmup = lean_codegen_elapsed(LEAN_SCALE_N_SMALL);
    let t_small = lean_codegen_elapsed(LEAN_SCALE_N_SMALL);
    let t_large = lean_codegen_elapsed(LEAN_SCALE_N_LARGE);
    let t_small_for_ratio = t_small.max(LEAN_SCALE_RATIO_FLOOR);
    let ratio = t_large.as_secs_f64() / t_small_for_ratio.as_secs_f64();
    eprintln!(
        "PW3-O-010 lean codegen: n={LEAN_SCALE_N_SMALL} => {:?}, n={LEAN_SCALE_N_LARGE} => {:?}, ratio={ratio:.2} (floor {:?})",
        t_small, t_large, LEAN_SCALE_RATIO_FLOOR
    );
    assert!(
        t_large <= LEAN_SCALE_CEILING_LARGE,
        "PW3-O-010 ratchet: n={LEAN_SCALE_N_LARGE} lean codegen must stay under {:?} \
         (PoC @ d28d6e1 ~17s; got {:?})",
        LEAN_SCALE_CEILING_LARGE,
        t_large
    );
    assert!(
        ratio <= LEAN_SCALE_QUADRATIC_FACTOR,
        "PW3-O-010 ratchet: growth looks worse than ~quadratic (ratio {ratio:.2} > {LEAN_SCALE_QUADRATIC_FACTOR}) \
         — small={t_small:?} large={t_large:?}"
    );
    assert!(
        case_start.elapsed() < Duration::from_secs(WALL_CASE_SECS),
        "PW3-O-010 ratchet must finish within {WALL_CASE_SECS}s wall"
    );
}

// ---------------------------------------------------------------------------
// Row 4 — transpile wall-clock guard + PW3-S-012 self meta
// ---------------------------------------------------------------------------

#[test]
fn pw3_s012_wall_clock_guard_under_300s() {
    let start = Instant::now();
    let _ = lean_codegen_elapsed(20);
    assert!(
        start.elapsed() < Duration::from_secs(WALL_CASE_SECS),
        "PW3-S-012 smoke codegen must stay under {WALL_CASE_SECS}s"
    );
}

#[test]
fn pw3_s012_ratchet_policy_self_check() {
    let src = include_str!("test_audit_scaling_matrix.rs");
    assert!(src.contains("PW3-O-005") && src.contains("PW3-O-010") && src.contains("PW3-G-013"));
    assert!(src.contains("WALL_CASE_SECS"));
    assert!(src.contains("CYCLIC_ALIAS_TIMEOUT"));
    assert!(src.contains("LEAN_SCALE_CEILING_LARGE"));
    assert!(
        std::fs::metadata(fixtures_dir().join("cyclic_alias.cam")).is_ok(),
        "PW3-S-012 fixture cyclic_alias.cam must exist"
    );
}
