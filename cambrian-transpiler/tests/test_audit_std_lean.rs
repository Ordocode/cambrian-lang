// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase L Wave 6 — audit Lean `std::math` + `std::str` lake gate (T-STD-LEAN-001).
//!
//! Reuses conformance fixture `tests/fixtures/std_lean.cam`. See `docs/AUDIT_EVM_LEAN.md` §6 Phase L.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;
use cambrian_transpiler::validate::{check_lean_target_compat, validate, Severity};

const TEST_ID: &str = "T-STD-LEAN-001";
const HYPOTHESIS: &str = "STD-H-LEAN-1";
const YAML_NAME: &str = "std_lean.yaml";
const CAM_NAME: &str = "std_lean.cam";
const REPRO_SUBDIR: &str = "std_lean_repro/t_std_lean_001";

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn audit_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit")
}

fn unique_out_dir() -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-std-lean-{}-{}",
        std::process::id(),
        n
    ))
}

fn lean_build_enabled() -> bool {
    std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() == Ok("1")
}

fn has_lake() -> bool {
    Command::new("lake")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn blocking_codes(prog: &cambrian_transpiler::ast::Program) -> Vec<String> {
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

fn transpile_std_lean_project() -> HashMap<String, String> {
    let yaml = fixtures_dir().join(YAML_NAME);
    let project = Project::load(&yaml).expect("load std_lean fixture project");
    let codes = blocking_codes(&project.merged);
    assert!(
        codes.is_empty(),
        "{}: validator blocked std_lean fixture: {:?}",
        TEST_ID,
        codes
    );
    LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .collect()
}

fn write_project_files(out_dir: &Path, files: &HashMap<String, String>) {
    for (rel, content) in files {
        let path = out_dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir lean out");
        }
        fs::write(path, content).expect("write lean file");
    }
}

fn scan_unsupported(files: &HashMap<String, String>) -> Vec<String> {
    let mut hits = Vec::new();
    for (rel, content) in files {
        if content.contains("Cambrian.Unsupported") {
            hits.push(rel.clone());
        }
    }
    hits
}

fn write_repro(files: &HashMap<String, String>, lake_log: &str, unsupported: &[String]) {
    let dir = audit_root().join(REPRO_SUBDIR);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create repro dir");

    let yaml_src = fixtures_dir().join(YAML_NAME);
    let cam_src = fixtures_dir().join(CAM_NAME);
    let _ = fs::copy(&yaml_src, dir.join(YAML_NAME));
    let _ = fs::copy(&cam_src, dir.join(CAM_NAME));

    let gen_dir = dir.join("generated");
    write_project_files(&gen_dir, files);

    fs::write(dir.join("lake.log"), lake_log).expect("write lake.log");

    let unsupported_line = if unsupported.is_empty() {
        "none".to_string()
    } else {
        unsupported.join(", ")
    };

    let note = format!(
        "# {TEST_ID} / {HYPOTHESIS} repro\n\n\
         Lean lake gate failed for `std_lean.cam` (math + str).\n\n\
         Fixture: `tests/fixtures/{CAM_NAME}`\n\
         Unsupported hits: {unsupported_line}\n\n\
         Re-run:\n\
         `CAMBRIAN_TEST_LEAN_BUILD=1 cargo test -p cambrian-transpiler --test test_audit_std_lean audit_std_lean_matrix_lake -- --nocapture`\n\n\
         --- lake log ---\n\
         {lake_log}\n",
    );
    fs::write(dir.join("NOTE.md"), note).expect("write NOTE.md");
}

#[test]
fn audit_std_lean_matrix_lake() {
    if !lean_build_enabled() {
        eprintln!(
            "skipping audit_std_lean_matrix_lake (set CAMBRIAN_TEST_LEAN_BUILD=1)"
        );
        return;
    }
    if !has_lake() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }

    let files = transpile_std_lean_project();
    let unsupported = scan_unsupported(&files);

    let out_dir = unique_out_dir();
    let _ = fs::remove_dir_all(&out_dir);
    fs::create_dir_all(&out_dir).expect("create out dir");
    write_project_files(&out_dir, &files);

    let lake = Command::new("lake")
        .arg("build")
        .current_dir(&out_dir)
        .output()
        .expect("invoke lake");

    let lake_log = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&lake.stdout),
        String::from_utf8_lossy(&lake.stderr)
    );

    let lake_ok = lake.status.success();
    let unsupported_ok = unsupported.is_empty();
    let ok = lake_ok && unsupported_ok;

    if !ok {
        write_repro(&files, &lake_log, &unsupported);
    }

    let _ = fs::remove_dir_all(&out_dir);

    let verdict = if ok {
        "PASS — STD-H-LEAN-1 REFUTED (std math+str lake)"
    } else if !unsupported_ok {
        "FAIL — STD-H-LEAN-1 CONFIRMED (Cambrian.Unsupported)"
    } else {
        "FAIL — STD-H-LEAN-1 CONFIRMED (lake build)"
    };

    eprintln!(
        "{TEST_ID} {HYPOTHESIS}: {verdict}\n{lake_log}",
    );

    if !unsupported_ok {
        let repro = audit_root().join(REPRO_SUBDIR);
        panic!(
            "{TEST_ID} {HYPOTHESIS} CONFIRMED — Cambrian.Unsupported in: {:?}; repro: {}\n{lake_log}",
            unsupported,
            repro.display(),
        );
    }

    let repro = audit_root().join(REPRO_SUBDIR);
    assert!(
        lake_ok,
        "{TEST_ID} {HYPOTHESIS} CONFIRMED — lake build failed; repro: {}\n{lake_log}",
        repro.display(),
    );
}
