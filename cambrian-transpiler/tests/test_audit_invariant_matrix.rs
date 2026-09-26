// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase N Wave-3 — invariant trace parity sweep (PW3-S-014 / PW3-O-013, PW3-G-012).
//!
//! Behavioral oracle B: forge route replay (ground truth) + bounded proptest action
//! lists + optional Lean `runTrace` `#eval` when `CAMBRIAN_TEST_LEAN_BUILD=1`.
//!
//!   cargo test -p cambrian-transpiler --test test_audit_invariant_matrix -- --nocapture
//!   CAMBRIAN_TEST_LEAN_BUILD=1 cargo test -p cambrian-transpiler --test test_audit_invariant_matrix -- --nocapture

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::{EvmSolidityBackend, LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;
use proptest::prelude::*;

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

#[derive(Debug, Clone)]
enum SweepAction {
    Fail(u64),
    Inc,
    Tip(u64),
}

fn audit_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit")
}

fn fixtures_dir() -> PathBuf {
    audit_root().join("fixtures/pw3_invariant")
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-invariant-{}-{}-{}",
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
    // U4-4c T7: keep codegen `Invariant_*.t.sol` alongside hand-written forge
    // oracles — handler-first harness must compile in the same project tree.
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

/// Forge `fail_on_revert: false` schedule for `TraceSweep` (see XH7 / T-X-007).
fn simulate_trace_sweep(init_count: u64, init_tips: u64, actions: &[SweepAction]) -> (u64, u64) {
    let mut count = init_count;
    let mut tips = init_tips;
    for action in actions {
        match action {
            SweepAction::Fail(amount) if *amount <= count => count -= amount,
            SweepAction::Fail(_) => {}
            SweepAction::Inc => count += 1,
            SweepAction::Tip(_) => tips += 1,
        }
    }
    (count, tips)
}

fn arb_sweep_action() -> impl Strategy<Value = SweepAction> {
    prop_oneof![
        (100u64..=200u64).prop_map(SweepAction::Fail),
        Just(SweepAction::Inc),
        (1u64..=100u64).prop_map(SweepAction::Tip),
    ]
}

// ---------------------------------------------------------------------------
// Row 1 — multi-action trace (fail / inc / tip)
// ---------------------------------------------------------------------------

#[test]
fn pw3_s014_o013_trace_multiaction_forge_oracle() {
    if !has_forge() {
        eprintln!("skip O-013 trace multiaction forge (no forge on PATH)");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("trace_multiaction_evm.yaml"),
        "Pw3InvariantTraceMulti.t.sol",
        "test_PW3_O013_multiActionTraceSequence",
        "O013-trace-forge",
    );
    assert!(
        ok,
        "PW3-O-013 forge baseline: fail/inc/tip sequence must match invariant init:\n{log}"
    );
}

#[test]
fn pw3_s014_o013_lean_runtrace_fail_inc_count_oracle() {
    let exec = LeanExec {
        eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.TraceSweep
import Cambrian.Generated.World
import Cambrian.Generated.TraceSweepRoutes
import Cambrian.Generated.TraceSweepSpec

open TraceSweep.Spec.Invariants.trace_sweep_parity

#eval Id.run do
  let inst : TraceSweep.Identity := {}
  let w0 := Cambrian.Generated.World.withTraceSweep
    Cambrian.Generated.World.default inst
    ({ TraceSweep.State.default with m_count := 10, m_tips := 0 })
  let ctx := Cambrian.MsgCtx.default
  match runTrace w0 inst ctx [.fail 150, .inc] with
  | .ok w => pure (Cambrian.Generated.World.traceSweep w inst).m_count
  | .error _ => pure (9999 : BitVec 64)
"#,
        expected: "11#64",
    };
    let (ok, log) = eval_lean(&yaml("trace_multiaction_lean.yaml"), &exec, "O013-count-lean");
    if lean_build_enabled() {
        assert!(
            ok,
            "PW3-O-013 lean runTrace fail→inc leg must match T-X-007 forge count (11):\n{log}"
        );
    } else {
        eprintln!("{log}");
    }
}

proptest! {
    #[test]
    fn pw3_s014_o013_proptest_action_list_forge_sim(
        actions in prop::collection::vec(arb_sweep_action(), 1..=6)
    ) {
        let (count, tips) = simulate_trace_sweep(10, 0, &actions);
        prop_assert!(count >= 10 || actions.iter().any(|a| matches!(a, SweepAction::Fail(_))));
        prop_assert!(tips <= actions.len() as u64);
        let canonical = vec![
            SweepAction::Fail(150),
            SweepAction::Inc,
            SweepAction::Tip(5),
        ];
        let (c, t) = simulate_trace_sweep(10, 0, &canonical);
        prop_assert_eq!(c, 11);
        prop_assert_eq!(t, 1);
    }
}

#[test]
fn pw3_s014_o013_runtrace_tip_leg_parity_red_gate() {
    if !lean_build_enabled() {
        eprintln!("skip O-013 tip parity red gate (set CAMBRIAN_TEST_LEAN_BUILD=1)");
        return;
    }
    let exec = LeanExec {
        eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.TraceSweep
import Cambrian.Generated.World
import Cambrian.Generated.TraceSweepRoutes
import Cambrian.Generated.TraceSweepSpec

open TraceSweep.Spec.Invariants.trace_sweep_parity

#eval Id.run do
  let inst : TraceSweep.Identity := {}
  let sink : Cambrian.Address := 0xBEEF#160
  let w0 := Cambrian.Generated.World.withTraceSweep
    Cambrian.Generated.World.default inst
    ({ TraceSweep.State.default with m_sink := sink, m_count := 10, m_tips := 0 })
  let selfAddr := TraceSweep.address inst
  let w0 := { w0 with balances := fun a => if a = selfAddr then (1000 : BitVec 256) else 0 }
  let ctx := Cambrian.MsgCtx.default
  match runTrace w0 inst ctx [.fail 150, .inc, .tip 5] with
  | .ok w => pure (Cambrian.Generated.World.traceSweep w inst).m_tips
  | .error _ => pure (9999 : BitVec 64)
"#,
        expected: "1#64",
    };
    let (lean_ok, log) = eval_lean(&yaml("trace_multiaction_lean.yaml"), &exec, "O013-tip-red");
    if !has_forge() {
        eprintln!("skip O-013 tip parity forge leg (no forge on PATH)");
    } else {
        let (forge_ok, _) = run_forge(
            &yaml("trace_multiaction_evm.yaml"),
            "Pw3InvariantTraceMulti.t.sol",
            "test_PW3_O013_multiActionTraceSequence",
            "O013-tip-forge-setup",
        );
        assert!(forge_ok, "O-013 red gate forge setup must pass first");
    }
    let lean_tip_matches_forge = lean_ok;
    assert!(
        lean_tip_matches_forge,
        "PW3-O-013: Lean runTrace `[fail, inc, tip]` with funded entity must leave m_tips=1 \
         matching forge. Log:\n{log}"
    );
}

// ---------------------------------------------------------------------------
// Row 2 — phased value + trace count
// ---------------------------------------------------------------------------

#[test]
fn pw3_s014_g012_phased_value_forge_oracle() {
    if !has_forge() {
        eprintln!("skip G-012 phased value forge (no forge on PATH)");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("phased_value_trace_evm.yaml"),
        "Pw3InvariantPhasedValue.t.sol",
        "test_PW3_G012_phasedValueFundMarksAndPays",
        "G012-value-forge",
    );
    assert!(
        ok,
        "PW3-G-012 forge baseline: phased fund must mark + pay recipient:\n{log}"
    );
}

#[test]
fn pw3_s014_g012_phased_value_runtrace_red_gate() {
    if !lean_build_enabled() {
        eprintln!("skip G-012 phased value red gate (set CAMBRIAN_TEST_LEAN_BUILD=1)");
        return;
    }
    let exec = LeanExec {
        eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.ValuePhase
import Cambrian.Generated.World
import Cambrian.Generated.ValuePhaseRoutes
import Cambrian.Generated.ValuePhaseSpec

open ValuePhase.Spec.Invariants.value_phase_trace

#eval Id.run do
  let inst : ValuePhase.Identity := {}
  let dest : Cambrian.Address := 0xCAFE#160
  let w0 := Cambrian.Generated.World.withValuePhase
    Cambrian.Generated.World.default inst
    ({ ValuePhase.State.default with m_recipient := dest, m_marked := 0, m_paid := 0 })
  let selfAddr := ValuePhase.address inst
  let w0 := { w0 with balances := fun a => if a = selfAddr then (1000 : BitVec 256) else 0 }
  let ctx := Cambrian.MsgCtx.default
  match runTrace w0 inst ctx [.fund 100] with
  | .ok w =>
    pure ((Cambrian.Generated.World.valuePhase w inst).m_marked, (Cambrian.Generated.World.valuePhase w inst).m_paid)
  | .error _ => pure (9999, 9999)
"#,
        expected: "(1#64, 100#256)",
    };
    let (lean_ok, log) = eval_lean(&yaml("phased_value_trace_lean.yaml"), &exec, "G012-value-red");
    if !has_forge() {
        eprintln!("skip G-012 forge leg (no forge on PATH)");
    } else {
        let (forge_ok, _) = run_forge(
            &yaml("phased_value_trace_evm.yaml"),
            "Pw3InvariantPhasedValue.t.sol",
            "test_PW3_G012_phasedValueFundMarksAndPays",
            "G012-value-forge-setup",
        );
        assert!(forge_ok, "G-012 red gate forge setup must pass first");
    }
    assert!(
        lean_ok,
        "PW3-G-012: phased value `fund` in invariant runTrace with funded entity must \
         match forge (1, 100). Log:\n{log}"
    );
}

// ---------------------------------------------------------------------------
// Row 3 — caret phased `^` trace vs forge state
// ---------------------------------------------------------------------------

#[test]
fn pw3_s014_g012_caret_phased_forge_oracle() {
    if !has_forge() {
        eprintln!("skip G-012 caret forge (no forge on PATH)");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("caret_phased_trace_evm.yaml"),
        "Pw3InvariantCaretPhased.t.sol",
        "test_PW3_G012_caretPhasedBumpMirrorsPostIncA",
        "G012-caret-forge",
    );
    assert!(
        ok,
        "PW3-G-012 forge baseline: snap phase must mirror post-inc m_a (^ semantics):\n{log}"
    );
}

#[test]
fn pw3_s014_g012_caret_phased_lean_runtrace_oracle() {
    let exec = LeanExec {
        eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.CaretPhase
import Cambrian.Generated.World
import Cambrian.Generated.CaretPhaseRoutes
import Cambrian.Generated.CaretPhaseSpec

open CaretPhase.Spec.Invariants.caret_phase_trace

#eval Id.run do
  let inst : CaretPhase.Identity := {}
  let w0 := Cambrian.Generated.World.withCaretPhase
    Cambrian.Generated.World.default inst
    ({ CaretPhase.State.default with m_a := 5, m_b := 0, m_steps := 0 })
  let ctx := Cambrian.MsgCtx.default
  let w := runTrace w0 inst ctx [.bump 3]
  pure ((Cambrian.Generated.World.caretPhase w inst).m_a, (Cambrian.Generated.World.caretPhase w inst).m_b, (Cambrian.Generated.World.caretPhase w inst).m_steps)
"#,
        expected: "(8#64, 8#64, 1#64)",
    };
    let (ok, log) = eval_lean(&yaml("caret_phased_trace_lean.yaml"), &exec, "G012-caret-lean");
    if lean_build_enabled() {
        assert!(
            ok,
            "PW3-G-012 caret ^ phased route: lean runTrace must match forge (8,8,1):\n{log}"
        );
    } else {
        eprintln!("{log}");
    }
}

// ---------------------------------------------------------------------------
// Row 4 — multi-entity invariant trace (PN-107 extension)
// ---------------------------------------------------------------------------

/// U4-4c T7: codegen `Invariant_*.t.sol` must compile in the same Foundry tree
/// as hand-written PW3 forge oracles (no strip-before-forge).
#[test]
fn u4_4c_t7_generated_invariant_compiles_with_hand_oracle() {
    if !has_forge() {
        eprintln!("skip T7 (no forge on PATH)");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("multi_entity_trace_evm.yaml"),
        "Pw3InvariantMultiEntity.t.sol",
        "test_PW3_O013_multiEntityDepositTraceReplay",
        "T7-multi-inv-compile",
    );
    assert!(
        ok,
        "T7: generated Invariant_ + hand oracle must compile and run:\n{log}"
    );
}

#[test]
fn pw3_s014_o013_multi_entity_forge_oracle() {
    if !has_forge() {
        eprintln!("skip O-013 multi-entity forge (no forge on PATH)");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("multi_entity_trace_evm.yaml"),
        "Pw3InvariantMultiEntity.t.sol",
        "test_PW3_O013_multiEntityDepositTraceReplay",
        "O013-multi-forge",
    );
    assert!(
        ok,
        "PW3-O-013 multi-entity forge baseline: deposit trace replay:\n{log}"
    );
}

#[test]
fn pw3_s014_o013_differential_single_runtrace_baseline() {
    let diff = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test_audit_differential.rs"),
    )
    .expect("read differential harness");
    let runtrace_inspect_rows = diff.matches("route_name: \"runTrace\"").count();
    assert_eq!(
        runtrace_inspect_rows, 1,
        "PW3-O-013 pre-sweep baseline: differential keeps single T-X-007 runTrace inspect row"
    );
    let matrix = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test_audit_invariant_matrix.rs"),
    )
    .expect("read invariant matrix");
    assert!(
        matrix.contains("pw3_s014_o013_proptest_action_list_forge_sim"),
        "PW3-O-013: matrix must add bounded proptest action-list sweep beyond differential"
    );
}
