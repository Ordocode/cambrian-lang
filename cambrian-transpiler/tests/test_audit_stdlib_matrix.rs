// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase N Wave-3 — stdlib cross-oracle matrix (PW3-S-006).
//!
//! Edges for `std::math` div0 / pow overflow / abs(MIN) (PW3-G-009) and
//! `std::str::parse_*` radix (PW3-G-010 / PN-109).
//!
//!   cargo test -p cambrian-transpiler --test test_audit_stdlib_matrix -- --nocapture
//!   CAMBRIAN_TEST_LEAN_BUILD=1 cargo test -p cambrian-transpiler --test test_audit_stdlib_matrix matches_evm -- --nocapture

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::{EvmSolidityBackend, LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;
use cambrian_transpiler::validate::check_lean_target_compat;

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
    audit_root().join("fixtures/pw3_stdlib")
}

fn yaml(name: &str) -> PathBuf {
    fixtures_dir().join(name)
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-stdlib-{}-{}-{}",
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
    let det = project.config.resolved_deterministic_addresses();
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    Ok(backend.gen_project(&project).into_iter().collect())
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

fn g009_divc_lean_exec() -> LeanExec {
    LeanExec {
        eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.StdDivcZero
import Cambrian.Generated.StdDivcZeroRoutes

#eval Id.run do
  let inst : StdDivcZero.Identity := {}
  let ctx := Cambrian.MsgCtx.default
  let s0 := StdDivcZero.State.default
  match StdDivcZero.Local.divcZero s0 ctx inst with
  | Except.error _ => pure true
  | Except.ok _ => pure false
"#,
        expected: "true",
    }
}

fn g009_pow_lean_exec() -> LeanExec {
    LeanExec {
        eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.StdPowOverflow
import Cambrian.Generated.StdPowOverflowRoutes

#eval Id.run do
  let inst : StdPowOverflow.Identity := {}
  let ctx := Cambrian.MsgCtx.default
  let s0 := StdPowOverflow.State.default
  let (_, r) := StdPowOverflow.Local.powOverflow s0 ctx inst
  pure (r == 0)
"#,
        expected: "false",
    }
}

fn g010_parse_lean_exec() -> LeanExec {
    LeanExec {
        eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.StdParseRadix
import Cambrian.Generated.StdParseRadixRoutes

#eval Id.run do
  let inst : StdParseRadix.Identity := {}
  let ctx := Cambrian.MsgCtx.default
  let s0 := StdParseRadix.State.default
  let (_, d) := StdParseRadix.Local.parseDecFf s0 ctx inst
  let (_, h) := StdParseRadix.Local.parseHexFf s0 ctx inst
  pure (d == 999 && h == 255)
"#,
        expected: "true",
    }
}

#[test]
fn pw3_stdlib_matrix_smoke_fixtures_parse() {
    for entry in std::fs::read_dir(fixtures_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "cam") {
            let src = std::fs::read_to_string(&path).unwrap();
            assert!(!src.is_empty(), "empty fixture {}", path.display());
        }
    }
}

#[test]
fn pw3_g009_lean_divc_codegen_documents_zero_guard() {
    let files = transpile_lean(&yaml("stdlib_divc_zero_lean.yaml")).expect("lean transpile");
    let routes = files
        .get("Cambrian/Generated/StdDivcZeroRoutes.lean")
        .expect("routes");
    assert!(
        routes.contains("checkedDivc"),
        "PW3-G-009: std::math::divc must lower via Cambrian.checkedDivc:\n{routes}"
    );
}

#[test]
fn pw3_g010_l13_rejects_sha256_on_lean() {
    let src = r#"
entity ShaGate {
    routes {
        constructor() => []
        go() -> u64 => [
            return(std::crypto::sha256("hi"))
        ]
    }
    m_x: u64 { in constructor() => 0 }
}
"#;
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .expect("parse sha256 fixture");
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    let diags = check_lean_target_compat(&program);
    assert!(
        diags.iter().any(|d| d.code == "L13"),
        "PW3-G-010: std::crypto::sha256 must be rejected on Lean (L13):\n{diags:?}"
    );
}

#[test]
fn pw3_g009_evm_forge_std_divc_zero_reverts() {
    if !has_forge() {
        eprintln!("skip PW3-G-009 divc forge");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("stdlib_divc_zero_evm.yaml"),
        "Pw3StdDivcZero.t.sol",
        "test_PW3_G009_stdDivcZeroReverts",
        "PW3-G-009-divc",
    );
    assert!(ok, "PW3-G-009 EVM: std::math::divc(1,0) must revert:\n{log}");
}

#[test]
fn pw3_g009_evm_forge_std_pow_overflow_reverts() {
    if !has_forge() {
        eprintln!("skip PW3-G-009 pow forge");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("stdlib_pow_overflow_evm.yaml"),
        "Pw3StdPowOverflow.t.sol",
        "test_PW3_G009_stdPowOverflowWrapsToZero",
        "PW3-G-009-pow",
    );
    assert!(ok, "PW3-G-009 EVM: std::math::pow(2,64) wraps to 0 (documented baseline):\n{log}");
}

#[test]
fn pw3_g009_evm_forge_std_abs_i64_min_returns_self() {
    if !has_forge() {
        eprintln!("skip PW3-G-009 abs forge");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("stdlib_abs_i64_min_evm.yaml"),
        "Pw3StdAbsI64Min.t.sol",
        "test_PW3_G009_stdAbsI64MinReturnsSelf",
        "PW3-G-009-abs",
    );
    assert!(
        ok,
        "PW3-G-009 EVM: std::math::abs(i64::MIN) returns MIN (documented baseline):\n{log}"
    );
}

#[test]
fn pw3_g010_evm_forge_parse_radix_discriminates() {
    if !has_forge() {
        eprintln!("skip PW3-G-010 parse forge");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("stdlib_parse_radix_evm.yaml"),
        "Pw3StdParseRadix.t.sol",
        "test_PW3_G010_parseRadixDecFailsHexSucceeds",
        "PW3-G-010-parse",
    );
    assert!(
        ok,
        "PW3-G-010 EVM: parse_uint radix dec vs hex:\n{log}"
    );
}

#[test]
fn pw3_g009_lean_std_divc_zero_matches_evm() {
    let (ok, detail) = eval_lean(
        &yaml("stdlib_divc_zero_lean.yaml"),
        &g009_divc_lean_exec(),
        "PW3-G-009-divc-lean",
    );
    assert!(
        ok,
        "PW3-G-009 Lean: std::math::divc must not return 0 on zero divisor (EVM reverts):\n{detail}"
    );
}

#[test]
fn pw3_g009_lean_std_pow_overflow_matches_evm() {
    let (ok, detail) = eval_lean(
        &yaml("stdlib_pow_overflow_lean.yaml"),
        &g009_pow_lean_exec(),
        "PW3-G-009-pow-lean",
    );
    assert!(
        ok,
        "PW3-G-009 Lean: std::math::pow(2,64) must fail or not wrap to 0 like EVM revert:\n{detail}"
    );
}

#[test]
fn pw3_g010_lean_parse_radix_matches_evm() {
    let (ok, detail) = eval_lean(
        &yaml("stdlib_parse_radix_lean.yaml"),
        &g010_parse_lean_exec(),
        "PW3-G-010-parse-lean",
    );
    assert!(
        ok,
        "PW3-G-010 Lean: parse_uint radix must match EVM (999 dec / 255 hex):\n{detail}"
    );
}
