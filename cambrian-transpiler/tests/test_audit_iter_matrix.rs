// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase N Wave-3 — iterator wide-fold / truncation matrix (PW3-S-015 / PW3-O-007, PW3-G-014).
//!
//! Extends T-F-004 with pinned rows + forge/`#eval` cross-oracles. O-007 narrowing
//! `as u64` panics 0x11 (checked). Remaining red gates: wide iterator chains (G-014)
//! and filter.map.filter.fold beyond T-F-004.
//!
//!   cargo test -p cambrian-transpiler --test test_audit_iter_matrix -- --nocapture
//!   CAMBRIAN_TEST_LEAN_BUILD=1 cargo test -p cambrian-transpiler --test test_audit_iter_matrix -- --nocapture

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
    audit_root().join("fixtures/pw3_iter")
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-iter-{}-{}-{}",
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

fn yaml(name: &str) -> PathBuf {
    fixtures_dir().join(name)
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
    Ok(backend.gen_project(&project).into_iter().collect())
}

fn transpile_lean(yaml_rel: &Path) -> Result<HashMap<String, String>, String> {
    let project = Project::load(yaml_rel).map_err(|e| format!("load project: {e}"))?;
    Ok(LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .collect())
}

fn project_sol_key(yaml_rel: &Path) -> String {
    let project = Project::load(yaml_rel).expect("load yaml for project name");
    let name = project
        .config
        .name
        .clone()
        .unwrap_or_else(|| "audit-project".to_string());
    format!("src/_{name}_project.sol")
}

fn run_forge(
    yaml_rel: &Path,
    forge_file: &str,
    match_test: &str,
    tag: &str,
) -> (bool, String) {
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

fn forge_build_only(yaml_rel: &Path, tag: &str) -> (bool, String) {
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
    std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).expect("foundry.toml");
    ensure_forge_std(&out_dir);
    let forge = Command::new("forge")
        .args(["build", "--root"])
        .arg(&out_dir)
        .output()
        .expect("forge build");
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

// ---------------------------------------------------------------------------
// Row 1 — O-007 per-step uint64 fold truncate
// ---------------------------------------------------------------------------

#[test]
fn pw3_s015_o007_fold_trunc_forge_oracle() {
    if !has_forge() {
        eprintln!("skip O-007 fold trunc forge (no forge on PATH)");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("fold_trunc_u64_evm.yaml"),
        "Pw3IterFoldTrunc.t.sol",
        "test_PW3_O007_truncFoldLargeElements",
        "O007-forge",
    );
    assert!(
        ok,
        "PW3-O-007 forge baseline: per-step u64 fold exec oracle:\n{log}"
    );
}

#[test]
fn pw3_s015_o007_unchecked_uint64_cast_red_gate() {
    let files = transpile_evm(&yaml("fold_trunc_u64_evm.yaml")).expect("transpile O-007");
    let key = project_sol_key(&yaml("fold_trunc_u64_evm.yaml"));
    let sol = files.get(&key).unwrap_or_else(|| {
        files
            .values()
            .find(|s| s.contains("trunc_fold"))
            .expect("trunc_fold in emitted sol")
    });
    let uses_safe_wide_accumulator = !sol.contains("uint64((acc +");
    assert!(
        uses_safe_wide_accumulator,
        "PW3-O-007: EVM fold must not emit unchecked `uint64((acc + x))`; use a range-checked downcast:\n{sol}"
    );
}

#[test]
fn pw3_s015_o007_fold_trunc_lean_eval_oracle() {
    let exec = LeanExec {
        eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.FoldTrunc
import Cambrian.Generated.World
import Cambrian.Generated.FoldTruncRoutes

#eval Id.run do
  let inst : FoldTrunc.Identity := {}
  let w := Cambrian.Generated.World.withFoldTrunc
    Cambrian.Generated.World.default inst FoldTrunc.State.default
  let ctx := Cambrian.MsgCtx.default
  let xs : List Cambrian.U256 := [
    (BitVec.allOnes 256),
    (2 : Cambrian.U256),
    (1 : Cambrian.U256)
  ]
  match FoldTrunc.Routes.run w inst ctx xs with
  | Except.error _ => pure true
  | Except.ok _ => pure false
"#,
        expected: "true",
    };
    let (ok, log) = eval_lean(&yaml("fold_trunc_u64_lean.yaml"), &exec, "O007-lean");
    if lean_build_enabled() {
        assert!(
            ok,
            "PW3-O-007 lean trunc_fold #eval must fail (not wrap to 2):\n{log}"
        );
    } else {
        eprintln!("{log}");
    }
}

// ---------------------------------------------------------------------------
// Row 2 — G-014 map.filter.map.fold (extends S-010)
// ---------------------------------------------------------------------------

#[test]
fn pw3_s015_g014_iter_chain_wide_lean_eval_oracle() {
    let exec = LeanExec {
        eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.ChainWide
import Cambrian.Generated.World
import Cambrian.Generated.ChainWideRoutes

#eval Id.run do
  let inst : ChainWide.Identity := {}
  let w := Cambrian.Generated.World.withChainWide
    Cambrian.Generated.World.default inst ChainWide.State.default
  let ctx := Cambrian.MsgCtx.default
  let items : List (BitVec 64) := [1, 2, 3, 4]
  let (_, v) := ChainWide.Routes.run w inst ctx items
  pure v
"#,
        expected: "24",
    };
    let (ok, log) = eval_lean(&yaml("iter_chain_wide_lean.yaml"), &exec, "G014-lean");
    if lean_build_enabled() {
        assert!(
            ok,
            "PW3-G-014 lean wide_sum #eval must be 24 (escrow baseline):\n{log}"
        );
    } else {
        eprintln!("{log}");
    }
}

#[test]
fn pw3_s015_g014_wide_chain_forge_exec_red_gate() {
    if !has_forge() {
        eprintln!("skip G-014 wide chain forge red gate (no forge on PATH)");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("iter_chain_wide_evm.yaml"),
        "Pw3IterChainWide.t.sol",
        "test_PW3_G014_wideSumMatchesExpected",
        "G014-forge",
    );
    assert!(
        ok,
        "PW3-G-014: map.filter.map.fold must compile+exec on EVM. Log:\n{log}"
    );
}

// ---------------------------------------------------------------------------
// Row 3 — beyond T-F-004 filter.map.filter.fold
// ---------------------------------------------------------------------------

#[test]
fn pw3_s015_beyond_tf004_deep_chain_compile_red_gate() {
    if !has_forge() {
        eprintln!("skip beyond T-F-004 compile red gate (no forge on PATH)");
        return;
    }
    let (ok, log) = forge_build_only(&yaml("filter_map_filter_fold_evm.yaml"), "deep-chain");
    assert!(
        ok,
        "PW3-S-015: filter.map.filter.fold beyond T-F-004 shape must compile on EVM. Log:\n{log}"
    );
}

// ---------------------------------------------------------------------------
// Row 4 — T-F-004 anchor green baseline + proptest hook
// ---------------------------------------------------------------------------

#[test]
fn pw3_s015_tf004_anchor_forge_oracle() {
    if !has_forge() {
        eprintln!("skip T-F-004 anchor forge (no forge on PATH)");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("tf004_anchor_evm.yaml"),
        "Pw3IterTf004Anchor.t.sol",
        "test_PW3_S015_tf004AnchorSum",
        "TF004-anchor",
    );
    assert!(
        ok,
        "PW3-S-015 T-F-004 anchor: filter().map().fold() green baseline:\n{log}"
    );
}

#[test]
fn pw3_s015_tf004_scope_documented_in_fuzz_harness() {
    let fuzz = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test_audit_fuzz.rs"),
    )
    .expect("read fuzz harness");
    assert!(
        fuzz.contains("fn audit_fuzz_iterator_chains")
            && fuzz.contains("filter().map().fold()"),
        "PW3-S-015: T-F-004 baseline must remain in test_audit_fuzz.rs"
    );
    assert!(
        fuzz.contains("fn audit_fuzz_iterator_wide_chains")
            || fuzz.contains("fn audit_fuzz_iterator_trunc_fold"),
        "PW3-S-015: extended iterator proptest rows required in test_audit_fuzz.rs"
    );
}
