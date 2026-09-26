// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase N Wave-3 — storage layout matrix (PW3-S-001).
//!
//! Compares `storage_layout::compute_layout` (used by Foundry/revm test seeding)
//! against `forge inspect … storage-layout`. Mismatch on HEAD = CONFIRMED finding
//! (PW3-O-003 / PW3-O-004); fixes land on `kernel-adapter-refactor` after triage.
//!
//! Run:
//!   cargo test -p cambrian-transpiler --test test_audit_layout_matrix -- --nocapture

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::evm_test_codegen;
use cambrian_transpiler::codegen::solidity::evm;
use cambrian_transpiler::codegen::storage_layout::compute_layout_with_program;
use cambrian_transpiler::project::InvariantConfig;
use cambrian_transpiler::ProgramParser;

/// Project name for `_{name}_project.sol` + factory harness imports.
const LAYOUT_PROJECT_NAME: &str = "pw3_layout";

const FOUNDRY_TOML: &str = r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
evm_version = "prague"
optimizer = false
"#;

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit/fixtures/pw3_layout")
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-layout-{}-{}-{}",
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

fn parse_fixture(name: &str) -> cambrian_transpiler::ast::Program {
    let path = fixtures_dir().join(name);
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
    let mut program = ProgramParser::new()
        .parse(&src)
        .unwrap_or_else(|e| panic!("parse {name}: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    cambrian_transpiler::desugar::desugar_properties(&mut program);
    program
}

fn entity_contract_name(program: &cambrian_transpiler::ast::Program) -> String {
    program.entities.first().expect("entity").name.clone()
}

fn transpile_cam_to(out_dir: &Path, cam_path: &Path) -> String {
    let src = std::fs::read_to_string(cam_path).expect("read cam");
    let mut program = ProgramParser::new()
        .parse(&src)
        .unwrap_or_else(|e| panic!("parse {}: {e}", cam_path.display()));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    cambrian_transpiler::desugar::desugar_properties(&mut program);
    let contract = entity_contract_name(&program);
    let det = true;
    let project_file = format!("_{}_project.sol", LAYOUT_PROJECT_NAME);
    let project_sol = evm::gen_evm_solidity_opts(&program, det, true);
    let src_dir = out_dir.join("src");
    std::fs::create_dir_all(&src_dir).ok();
    std::fs::write(src_dir.join(&project_file), project_sol).expect("write project sol");
    let stub = format!(
        "// SPDX-License-Identifier: UNLICENSED\npragma solidity ^0.8.24;\n// Re-export entity from combined project file.\nimport \"./{}\";\n",
        project_file
    );
    std::fs::write(src_dir.join(format!("{contract}.sol")), stub).expect("write entity stub");
    let inv_cfg = InvariantConfig::default();
    for (rel, contents) in evm_test_codegen::generate_evm_tests_for_project(
        &program,
        det,
        &inv_cfg,
        Some(LAYOUT_PROJECT_NAME),
        false,
    ) {
        let path = out_dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, contents).expect("write generated");
    }
    for (rel, contents) in [
        (
            "foundry.toml",
            evm_test_codegen::generate_foundry_toml(None, None, None),
        ),
        ("setup.sh", evm_test_codegen::generate_setup_sh()),
    ] {
        std::fs::write(out_dir.join(rel), contents).expect("write meta");
    }
    contract
}

fn patch_enum_ctor_sol(out_dir: &Path, contract: &str) {
    let paths = [
        out_dir.join(format!("src/{contract}.sol")),
        out_dir.join(format!("src/_{}_project.sol", LAYOUT_PROJECT_NAME)),
    ];
    for sol_path in paths {
        if !sol_path.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&sol_path).expect("read sol");
        let patched = text
            .replace("= Red;", "= Color.Red;")
            .replace("= Green;", "= Color.Green;");
        if patched != text {
            std::fs::write(&sol_path, patched).expect("patch enum ctor");
        }
    }
}

fn ensure_forge_std(out_dir: &Path) {
    cambrian_transpiler::codegen::evm_test_codegen::install_forge_std(out_dir)
        .unwrap_or_else(|e| panic!("{e} in {}", out_dir.display()));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SolcSlot {
    slot: u64,
    offset: u8,
}

fn forge_storage_entry(out_dir: &Path, contract: &str, label: &str) -> Option<SolcSlot> {
    let project_sol = format!("src/_{}_project.sol", LAYOUT_PROJECT_NAME);
    let out = Command::new("forge")
        .args([
            "inspect",
            &format!("{project_sol}:{contract}"),
            "storage-layout",
            "--json",
        ])
        .current_dir(out_dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let entry = json
        .get("storage")?
        .as_array()?
        .iter()
        .find(|e| e.get("label").and_then(|l| l.as_str()) == Some(label))?;
    let slot = entry
        .get("slot")
        .and_then(|s| s.as_u64().or_else(|| s.as_str()?.parse().ok()))?;
    let offset = entry
        .get("offset")
        .and_then(|o| o.as_u64())
        .unwrap_or(0) as u8;
    Some(SolcSlot { slot, offset })
}

fn forge_build(out_dir: &Path) -> Result<(), String> {
    ensure_forge_std(out_dir);
    if !out_dir.join("foundry.toml").exists() {
        std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).expect("foundry.toml");
    }
    let out = Command::new("forge")
        .args(["build", "--root"])
        .arg(out_dir)
        .output()
        .expect("forge build");
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

fn assert_layout_matches_solc(
    fixture: &str,
    probe: &str,
    fallback: SolcSlot,
    tag: &str,
) {
    let program = parse_fixture(fixture);
    let entity = program.entities.first().expect("entity");
    let layout = compute_layout_with_program(entity, true, &program);
    let computed = layout
        .lookup(probe)
        .unwrap_or_else(|| panic!("{tag}: compute_layout missing {probe}"));
    let computed_slot = SolcSlot {
        slot: computed.slot,
        offset: computed.offset,
    };

    if has_forge() {
        let out = unique_out_dir(tag);
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        let cam = fixtures_dir().join(fixture);
        let contract = transpile_cam_to(&out, &cam);
        patch_enum_ctor_sol(&out, &contract);
        forge_build(&out).unwrap_or_else(|log| panic!("{tag}: forge build failed:\n{log}"));
        let solc = forge_storage_entry(&out, &contract, probe)
            .unwrap_or_else(|| panic!("{tag}: forge inspect missing {probe}"));
        assert_eq!(
            computed_slot, solc,
            "{tag}: compute_layout({probe})={computed_slot:?} must match solc {solc:?} (fixture {fixture})"
        );
        let _ = std::fs::remove_dir_all(&out);
    } else {
        assert_eq!(
            computed_slot, fallback,
            "{tag}: compute_layout({probe})={computed_slot:?} must match solc fallback {fallback:?}"
        );
    }
}

#[test]
fn pw3_layout_baseline_u64_pair_m_x() {
    assert_layout_matches_solc(
        "u64_pair.cam",
        "m_x",
        SolcSlot { slot: 0, offset: 8 },
        "PW3-baseline-u64-pair",
    );
}

#[test]
fn pw3_layout_baseline_entity_record_m_x() {
    assert_layout_matches_solc(
        "entity_record.cam",
        "m_x",
        SolcSlot { slot: 2, offset: 0 },
        "PW3-baseline-entity-record",
    );
}

#[test]
fn pw3_layout_finding_unit_enum_m_x_slot() {
    assert_layout_matches_solc(
        "unit_enum.cam",
        "m_x",
        SolcSlot { slot: 0, offset: 1 },
        "PW3-O-003-unit-enum",
    );
}

#[test]
fn pw3_layout_finding_option_u64_m_x_slot() {
    assert_layout_matches_solc(
        "option_u64.cam",
        "m_x",
        SolcSlot { slot: 1, offset: 0 },
        "PW3-O-003-option-u64",
    );
}

#[test]
fn pw3_layout_baseline_prog_record_m_x() {
    assert_layout_matches_solc(
        "prog_record.cam",
        "m_x",
        SolcSlot { slot: 1, offset: 0 },
        "PW3-baseline-prog-record",
    );
}

#[test]
fn pw3_layout_finding_unit_enum_generated_forge_test() {
    if !has_forge() {
        eprintln!("pw3_layout_finding_unit_enum_generated_forge_test: skip (no forge)");
        return;
    }
    let out = unique_out_dir("unit-enum-test");
    let cam = fixtures_dir().join("unit_enum.cam");
    let contract = transpile_cam_to(&out, &cam);
    patch_enum_ctor_sol(&out, &contract);
    forge_build(&out).expect("forge build src+test");
    if !out.join("test").exists() {
        panic!("PW3-O-014: expected generated Foundry test for unit_enum.cam");
    }
    let forge = Command::new("forge")
        .args(["test", "-vv", "--root"])
        .arg(&out)
        .output()
        .expect("forge test");
    assert!(
        forge.status.success(),
        "PW3-O-014: seeded forge test must pass when layout matches solc:\n{}{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn pw3_layout_matrix_smoke_all_fixtures_parse() {
    for entry in std::fs::read_dir(fixtures_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "cam") {
            let name = path.file_name().unwrap().to_string_lossy();
            parse_fixture(&name);
        }
    }
}
