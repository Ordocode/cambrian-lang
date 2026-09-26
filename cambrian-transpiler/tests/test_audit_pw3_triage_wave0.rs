// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Wave 0 validator batch for PW3 forge-249 triage.
//!
//! Run from repo root:
//!   cargo test -p cambrian-transpiler --test test_audit_pw3_triage_wave0 validate_batch -- --nocapture
//!
//! Input: `tests/audit/repro/pw3_forge_249/triage_validate_manifest.jsonl`
//! Output: `tests/audit/repro/pw3_forge_249/triage_validate_results.jsonl`

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use cambrian_transpiler::target::Target;
use cambrian_transpiler::using_rewrite::apply_using_rewrites;
use cambrian_transpiler::validate::{self, Severity};
use cambrian_transpiler::ProgramParser;
use serde::Deserialize;

#[derive(Deserialize)]
struct ManifestRow {
    test_name: String,
    cam: String,
    deterministic: bool,
}

#[derive(serde::Serialize)]
struct ResultRow {
    test_name: String,
    validator_verdict: String,
}

fn repro_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/audit/repro/pw3_forge_249")
}

fn validate_cam(cam: &str, det: bool) -> String {
    let mut program = match ProgramParser::new().parse(cam) {
        Ok(p) => p,
        Err(e) => return format!("PARSE_ERR:{e}"),
    };
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    apply_using_rewrites(&mut program);

    let mut diags = validate::validate(&program);
    diags.extend(validate::check_target_compat(&program, Target::Evm, det));

    let errors = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code.to_string())
        .collect::<std::collections::BTreeSet<_>>();

    if errors.is_empty() {
        "OK".to_string()
    } else {
        format!("ERR:{}", errors.into_iter().collect::<Vec<_>>().join(","))
    }
}

#[test]
fn validate_batch() {
    let dir = repro_dir();
    let manifest_path = dir.join("triage_validate_manifest.jsonl");
    if !manifest_path.exists() {
        eprintln!(
            "skip validate_batch: missing {} (run triage_wave0.py first)",
            manifest_path.display()
        );
        return;
    }

    let out_path = dir.join("triage_validate_results.jsonl");
    let mut out = File::create(&out_path).expect("create results");

    let manifest = BufReader::new(File::open(&manifest_path).expect("open manifest"));
    for line in manifest.lines() {
        let line = line.expect("read manifest line");
        if line.trim().is_empty() {
            continue;
        }
        let row: ManifestRow = serde_json::from_str(&line).expect("parse manifest row");
        let verdict = validate_cam(&row.cam, row.deterministic);
        let result = ResultRow {
            test_name: row.test_name,
            validator_verdict: verdict,
        };
        writeln!(out, "{}", serde_json::to_string(&result).expect("serialize")).unwrap();
    }

    eprintln!("wrote {}", out_path.display());
}
