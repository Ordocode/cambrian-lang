// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! P7 Step G: programs newly admitted on lean after Solidity-language E-rules
//! stopped firing must not degrade to silent Lean sentinels.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn transpile_lean(cam: &str) -> (bool, String, PathBuf) {
    let root = repo_root();
    let cam_path = root.join(cam);
    let out_owned = std::env::temp_dir().join(format!(
        "cambrian-p7-lean-admit-{}-{}",
        std::process::id(),
        cam.replace('/', "_").replace('.', "_")
    ));
    let _ = fs::remove_dir_all(&out_owned);
    fs::create_dir_all(&out_owned).unwrap();

    // Compile-time path from Cargo; follows the active target dir
    // (`target/debug`, `target/llvm-cov-target/debug`, …).
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cambrian-transpiler"));

    let output = Command::new(&bin)
        .args([
            cam_path.to_str().unwrap(),
            "-o",
            out_owned.to_str().unwrap(),
            "--target",
            "lean",
        ])
        .output()
        .expect("run transpiler");

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let ok = output.status.success();
    (ok, format!("{stdout}{stderr}"), out_owned)
}

fn scan_sentinels(dir: &PathBuf) -> Vec<String> {
    let mut hits = Vec::new();
    fn walk(dir: &PathBuf, hits: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, hits);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("lean") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            for (i, line) in text.lines().enumerate() {
                if line.contains("Cambrian.Unsupported")
                    || line.contains("-- skipped:")
                    || line.contains("__L8_")
                    || line.contains("__L9_")
                    || line.contains("__L11_")
                {
                    hits.push(format!("{}:{}:{}", path.display(), i + 1, line.trim()));
                }
            }
        }
    }
    walk(dir, &mut hits);
    hits
}

fn assert_lean_admit(cam: &str) {
    let (ok, log, out) = transpile_lean(cam);
    assert!(
        ok,
        "expected lean admit for {cam}, got rejection:\n{log}"
    );
    let hits = scan_sentinels(&out);
    assert!(
        hits.is_empty(),
        "silent Lean sentinels in admitted {cam}:\n{}",
        hits.join("\n")
    );

    // Lake parity for remaining admit fixtures (PM-006 companion gate).
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() == Ok("1") {
        let status = Command::new("lake")
            .arg("build")
            .current_dir(&out)
            .output()
            .expect("lake build");
        let lake_log = format!(
            "{}{}",
            String::from_utf8_lossy(&status.stdout),
            String::from_utf8_lossy(&status.stderr)
        );
        assert!(
            status.status.success(),
            "lake build failed for admitted {cam}:\n{lake_log}"
        );
    } else {
        eprintln!("lean admit lake gate: skip (set CAMBRIAN_TEST_LEAN_BUILD=1)");
    }

    let _ = fs::remove_dir_all(&out);
}

#[test]
fn lean_admits_e07_filter_map_fold() {
    assert_lean_admit(
        "cambrian-transpiler/tests/audit/fixtures/val_e07_filter_map_fold.cam",
    );
}

#[test]
fn lean_rejects_e17_hashmap_shape_with_l15() {
    let (ok, log, out) = transpile_lean(
        "cambrian-transpiler/tests/audit/fixtures/val_e17_hashmap_shape.cam",
    );
    let _ = fs::remove_dir_all(&out);
    assert!(!ok, "ill-shaped HashMap transform must fail on lean (L15)");
    assert!(
        log.contains("[L15]") || log.contains("L15"),
        "expected L15 diagnostic, got:\n{log}"
    );
}

#[test]
fn lean_admits_e18_tuple_storage() {
    assert_lean_admit("cambrian-transpiler/tests/audit/fixtures/val_e18_tuple_storage.cam");
}

#[test]
fn lean_admits_e23_mixed_tuple() {
    assert_lean_admit(
        "cambrian-transpiler/tests/audit/fixtures/val_e23_mixed_tuple_purefn.cam",
    );
}

#[test]
fn lean_l1_still_rejects_e19_unknown_generic() {
    let (ok, log, out) = transpile_lean(
        "cambrian-transpiler/tests/audit/fixtures/val_e19_unknown_generic.cam",
    );
    let _ = fs::remove_dir_all(&out);
    assert!(!ok, "E19-shaped unknown generic must still fail on lean");
    assert!(
        log.contains("[L1]") || log.contains("L1"),
        "expected L1 diagnostic, got:\n{log}"
    );
}
