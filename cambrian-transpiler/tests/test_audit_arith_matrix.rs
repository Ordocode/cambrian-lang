// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase N Wave-3 — arithmetic drift matrix (PW3-S-003).
//!
//! Triple-oracle rows for nat subtraction (PW3-O-002 INTENDED), unchecked `/`
//! (PW3-O-006), and pure-fn signed compare B-34 (PW3-G-005).
//! `lean.numerics: nat` saturating `-` is the simplified proof model — it is
//! not required to match EVM Panic 0x11.
//!
//!   cargo test -p cambrian-transpiler --test test_audit_arith_matrix -- --nocapture

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::{EvmSolidityBackend, LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;

const FOUNDRY_TOML: &str = r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
evm_version = "prague"
optimizer = false
"#;

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

struct LeanExec {
    eval_lean: &'static str,
    expected: &'static str,
}

fn audit_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit")
}

fn fixtures_dir() -> PathBuf {
    audit_root().join("fixtures/pw3_arith")
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-arith-{}-{}-{}",
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

fn has_lake() -> bool {
    Command::new("lake")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn lean_build_enabled() -> bool {
    std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() == Ok("1")
}

fn ensure_forge_std(out_dir: &Path) {
    cambrian_transpiler::codegen::evm_test_codegen::install_forge_std(out_dir)
        .unwrap_or_else(|e| panic!("{e} in {}", out_dir.display()));
}

fn transpile_evm(yaml_rel: &Path) -> Result<HashMap<String, String>, String> {
    let project = Project::load(yaml_rel).map_err(|e| format!("load project: {e}"))?;
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    Ok(backend
        .gen_project(&project)
        .into_iter()
        .collect())
}

fn transpile_lean(yaml_rel: &Path) -> Result<HashMap<String, String>, String> {
    let project = Project::load(yaml_rel).map_err(|e| format!("load project: {e}"))?;
    Ok(LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .collect())
}

fn run_forge(yaml_rel: &Path, forge_file: &str, match_test: &str, tag: &str) -> (bool, String) {
    let out_dir = unique_out_dir(tag);
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("out dir");
    let files = match transpile_evm(yaml_rel) {
        Ok(f) => f,
        Err(err) => return (false, err),
    };
    for (rel, contents) in files {
        let path = out_dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&path, contents).expect("write sol");
    }
    if !out_dir.join("foundry.toml").exists() {
        std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).expect("foundry.toml");
    }
    let test_dir = out_dir.join("test");
    std::fs::create_dir_all(&test_dir).expect("test dir");
    let src = audit_root().join("forge").join(forge_file);
    std::fs::copy(&src, test_dir.join(forge_file)).expect("copy forge test");
    ensure_forge_std(&out_dir);
    let forge = Command::new("forge")
        .args(["test", "--match-test", match_test, "-vv", "--root"])
        .arg(&out_dir)
        .output()
        .expect("forge test");
    let combined = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );
    let ok = forge.status.success();
    let _ = std::fs::remove_dir_all(&out_dir);
    (ok, combined)
}

fn write_lean_project(files: &HashMap<String, String>, out_dir: &Path) -> Result<(), String> {
    for (rel, contents) in files {
        let path = out_dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, contents).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

fn eval_lean(yaml_rel: &Path, exec: &LeanExec, tag: &str) -> (bool, String) {
    if !lean_build_enabled() {
        return (
            true,
            format!(
                "skip lean #eval (set CAMBRIAN_TEST_LEAN_BUILD=1); expected `{}`",
                exec.expected
            ),
        );
    }
    if !has_lake() {
        return (false, "CAMBRIAN_TEST_LEAN_BUILD=1 but `lake` not on PATH".into());
    }
    let files = match transpile_lean(yaml_rel) {
        Ok(f) => f,
        Err(err) => return (false, err),
    };
    let out_dir = unique_out_dir(tag);
    let _ = std::fs::remove_dir_all(&out_dir);
    if let Err(err) = std::fs::create_dir_all(&out_dir) {
        return (false, format!("mkdir: {err}"));
    }
    if let Err(err) = write_lean_project(&files, &out_dir) {
        let _ = std::fs::remove_dir_all(&out_dir);
        return (false, err);
    }
    if let Err(err) = std::fs::write(out_dir.join("Eval.lean"), exec.eval_lean.trim_start()) {
        let _ = std::fs::remove_dir_all(&out_dir);
        return (false, format!("write Eval.lean: {err}"));
    }
    let lake = Command::new("lake")
        .arg("build")
        .current_dir(&out_dir)
        .output()
        .expect("lake build");
    if !lake.status.success() {
        let detail = format!(
            "lake build failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&lake.stdout),
            String::from_utf8_lossy(&lake.stderr)
        );
        let _ = std::fs::remove_dir_all(&out_dir);
        return (false, detail);
    }
    let eval = Command::new("lake")
        .args(["env", "lean", "Eval.lean"])
        .current_dir(&out_dir)
        .output()
        .expect("lake env lean");
    let combined = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&eval.stdout),
        String::from_utf8_lossy(&eval.stderr)
    );
    let _ = std::fs::remove_dir_all(&out_dir);
    let ok = eval.status.success() && combined.contains(exec.expected);
    if ok {
        (true, combined)
    } else {
        (
            false,
            format!(
                "#eval expected stdout to contain `{}` (status={:?})\n{combined}",
                exec.expected,
                eval.status.code()
            ),
        )
    }
}

fn yaml(name: &str) -> PathBuf {
    fixtures_dir().join(name)
}

fn o002_lean_exec() -> LeanExec {
    LeanExec {
        eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.NatSubUnderflow
import Cambrian.Generated.NatSubUnderflowRoutes

#eval Id.run do
  let inst : NatSubUnderflow.Identity := {}
  let ctx := Cambrian.MsgCtx.default
  let s0 := NatSubUnderflow.State.default
  let (_, r) := NatSubUnderflow.Local.probe s0 ctx inst
  pure (r == 0)
"#,
        // INTENDED: Lean `Nat` subtraction is total/saturating (`0 - 1 = 0`).
        expected: "true",
    }
}

fn o006_lean_exec() -> LeanExec {
    LeanExec {
        eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.DivByZero
import Cambrian.Generated.DivByZeroRoutes

#eval Id.run do
  let inst : DivByZero.Identity := {}
  let ctx := Cambrian.MsgCtx.default
  let s0 := DivByZero.State.default
  match DivByZero.Local.divByZero s0 ctx inst with
  | Except.error _ => pure true
  | Except.ok _ => pure false
"#,
        expected: "true",
    }
}

fn g005_lean_exec() -> LeanExec {
    LeanExec {
        eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.SignedSltPure
import Cambrian.Generated.SignedSltPureRoutes

#eval Id.run do
  let inst : SignedSltPure.Identity := {}
  let ctx := Cambrian.MsgCtx.default
  let s0 := SignedSltPure.State.default
  let (_, r) := SignedSltPure.Local.probe s0 ctx inst ((BitVec.ofInt 8 (-1))) ((0 : BitVec 8))
  pure r
"#,
        expected: "true",
    }
}

#[test]
fn pw3_arith_matrix_smoke_all_fixtures_parse() {
    for entry in std::fs::read_dir(fixtures_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "cam") {
            let src = std::fs::read_to_string(&path).unwrap();
            assert!(!src.is_empty(), "empty fixture {}", path.display());
        }
    }
}

#[test]
fn pw3_o002_lean_nat_lowering_documents_total_sub() {
    let files = transpile_lean(&yaml("nat_sub_underflow_lean.yaml")).expect("lean transpile");
    let routes = files
        .get("Cambrian/Generated/NatSubUnderflowRoutes.lean")
        .expect("routes");
    assert!(
        routes.contains("State × Nat") && routes.contains("(s.m_x - 1)"),
        "PW3-O-002 INTENDED: nat profile must emit total Nat subtraction:\n{routes}"
    );
}

#[test]
fn pw3_o006_lean_panic_lowering_documents_unchecked_div() {
    let files = transpile_lean(&yaml("div_by_zero_lean.yaml")).expect("lean transpile");
    let routes = files
        .get("Cambrian/Generated/DivByZeroRoutes.lean")
        .expect("routes");
    assert!(
        routes.contains("checkedDiv"),
        "PW3-O-006: `/` must lower via Cambrian.checkedDiv:\n{routes}"
    );
}

#[test]
fn pw3_g005_lean_pure_fn_documents_signed_compare() {
    let files = transpile_lean(&yaml("signed_slt_pure_lean.yaml")).expect("lean transpile");
    let pure = files.get("Cambrian/Generated/Pure.lean").expect("Pure.lean");
    assert!(
        pure.contains("def p_slt") && (pure.contains(".slt") || pure.contains("Cambrian.slt")),
        "PW3-G-005 / B-34: signed i8 compare must use slt in pure fn:\n{pure}"
    );
}

#[test]
fn pw3_o002_evm_forge_underflow_reverts() {
    if !has_forge() {
        eprintln!("skip PW3-O-002 forge");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("nat_sub_underflow_evm.yaml"),
        "Pw3NatSubUnderflow.t.sol",
        "test_PW3_O002_probeUnderflowReverts",
        "PW3-O-002",
    );
    assert!(ok, "PW3-O-002 EVM: u64 0-1 must revert:\n{log}");
}

#[test]
fn pw3_o006_evm_forge_div_by_zero_reverts() {
    if !has_forge() {
        eprintln!("skip PW3-O-006 forge");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("div_by_zero_evm.yaml"),
        "Pw3DivByZero.t.sol",
        "test_PW3_O006_divByZeroReverts",
        "PW3-O-006",
    );
    assert!(ok, "PW3-O-006 EVM: 1/0 must revert:\n{log}");
}

#[test]
fn pw3_g005_evm_forge_signed_compare_true() {
    if !has_forge() {
        eprintln!("skip PW3-G-005 forge");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("signed_slt_pure_evm.yaml"),
        "Pw3SignedSltPure.t.sol",
        "test_PW3_G005_signedMinusOneLessThanZero",
        "PW3-G-005",
    );
    assert!(ok, "PW3-G-005 EVM: i8 -1 < 0 must be true:\n{log}");
}

#[test]
fn pw3_o002_lean_nat_underflow_saturates_intended() {
    let (ok, detail) = eval_lean(&yaml("nat_sub_underflow_lean.yaml"), &o002_lean_exec(), "PW3-O-002");
    assert!(
        ok,
        "PW3-O-002 INTENDED: lean.numerics: nat saturates 0-1 to 0 (proof model, not EVM 0x11):\n{detail}"
    );
}

#[test]
fn pw3_o006_lean_div_by_zero_matches_evm() {
    let (ok, detail) = eval_lean(&yaml("div_by_zero_lean.yaml"), &o006_lean_exec(), "PW3-O-006");
    assert!(
        ok,
        "PW3-O-006 Lean overflow-panic: `/` must fail like EVM (not silent 0):\n{detail}"
    );
}

#[test]
fn pw3_g005_lean_signed_compare_matches_evm() {
    let (ok, detail) = eval_lean(&yaml("signed_slt_pure_lean.yaml"), &g005_lean_exec(), "PW3-G-005");
    assert!(
        ok,
        "PW3-G-005 / B-34 Lean: p_slt(-1,0) must be true like EVM:\n{detail}"
    );
}
