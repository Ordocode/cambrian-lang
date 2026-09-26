// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Architecture audit wave 2 — EVM↔Lean symmetry invariant tests.
//!
//! Plan: `docs/plans/evm-lean-architecture-audit-wave2.md`
//! Registry: `docs/AUDIT_ARCHITECTURE_WAVE2.md`

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::analysis::route_can_fail_evm_lean;
use cambrian_transpiler::ast::{Expr, RouteAction};
use cambrian_transpiler::codegen::{EvmSolidityBackend, LeanBackend, OutputBackend};
use cambrian_transpiler::codegen::lean::{expr_forces_fail_surface, member_transform_body_checked};
use cambrian_transpiler::codegen::solidity::materialize_typed_expr;
use cambrian_transpiler::ir::{lower_expr, CoerceKind, LowerCtx, ResolvedType, TypedExpr, TypedExprKind};
use cambrian_transpiler::project::Project;
use cambrian_transpiler::validate::{validate, Severity};

const FOUNDRY_TOML: &str = r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
evm_version = "prague"
optimizer = false
"#;

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn audit_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit")
}

fn fixtures_dir() -> PathBuf {
    audit_root().join("fixtures/arch_wave2")
}

fn yaml(name: &str) -> PathBuf {
    fixtures_dir().join(name)
}

fn evm_test_sol(yaml_rel: &Path) -> String {
    let project = Project::load(yaml_rel).expect("load project yaml");
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let inv = cambrian_transpiler::project::InvariantConfig::default();
    let files = cambrian_transpiler::codegen::evm_test_codegen::generate_evm_tests(
        &project.merged,
        det,
        &inv,
    );
    files
        .into_iter()
        .filter(|(p, _)| p.starts_with("test/"))
        .map(|(_, s)| s)
        .collect::<Vec<_>>()
        .join("\n")
}

fn uses_det_predict_markers(test_sol: &str) -> bool {
    test_sol.contains("predictTlsPeer")
        || test_sol.contains("ICambrianFactory(_factory).predict")
}

fn route_let_value(yaml_rel: &Path, entity: &str, route_name: &str) -> Expr {
    let project = Project::load(yaml_rel).expect("load project");
    let route = project
        .merged
        .entities
        .iter()
        .find(|e| e.name == entity)
        .and_then(|e| e.routes.iter().find(|r| r.name == route_name))
        .unwrap_or_else(|| panic!("missing route {entity}.{route_name} in {}", yaml_rel.display()));
    route
        .body
        .all_actions()
        .into_iter()
        .find_map(|a| match a {
            RouteAction::Let { value, .. } => Some(value.clone()),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "missing let binding in route {entity}.{route_name} ({})",
                yaml_rel.display()
            )
        })
}

fn member_transform_body(
    yaml_rel: &Path,
    entity: &str,
    member: &str,
    route: &str,
) -> Expr {
    let project = Project::load(yaml_rel).expect("load project");
    project
        .merged
        .entities
        .iter()
        .find(|e| e.name == entity)
        .and_then(|e| e.members.iter().find(|m| m.name == member))
        .and_then(|m| m.transforms.iter().find(|t| t.route_name == route))
        .map(|t| t.body.clone())
        .unwrap_or_else(|| {
            panic!(
                "missing transform {entity}.{member} in {route} ({})",
                yaml_rel.display()
            )
        })
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-arch-{}-{}-{}",
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

fn run_forge_transpiled(yaml_rel: &Path, match_test: &str, tag: &str) -> (bool, String) {
    let out_dir = unique_out_dir(tag);
    let files = transpile_evm(yaml_rel).expect("evm transpile");
    std::fs::create_dir_all(out_dir.join("src")).unwrap();
    std::fs::create_dir_all(out_dir.join("test")).unwrap();
    std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).unwrap();
    ensure_forge_std(&out_dir);
    for (rel, content) in &files {
        let path = out_dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
    }
    let output = Command::new("forge")
        .args(["test", "--match-test", match_test, "-vv"])
        .current_dir(&out_dir)
        .output()
        .expect("forge test");
    let log = String::from_utf8_lossy(&output.stderr).to_string()
        + &String::from_utf8_lossy(&output.stdout);
    (output.status.success(), log)
}

fn run_forge(yaml_rel: &Path, forge_file: &str, match_test: &str, tag: &str) -> (bool, String) {
    let out_dir = unique_out_dir(tag);
    let files = transpile_evm(yaml_rel).expect("evm transpile");
    std::fs::create_dir_all(out_dir.join("src")).unwrap();
    std::fs::create_dir_all(out_dir.join("test")).unwrap();
    std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).unwrap();
    ensure_forge_std(&out_dir);
    for (rel, content) in &files {
        let path = out_dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
    }
    std::fs::copy(
        audit_root().join("forge").join(forge_file),
        out_dir.join("test").join(forge_file),
    )
    .unwrap_or_else(|e| panic!("copy forge wrapper: {e}"));
    let output = Command::new("forge")
        .args(["test", "--match-test", match_test, "-vv"])
        .current_dir(&out_dir)
        .output()
        .expect("forge test");
    let log = String::from_utf8_lossy(&output.stderr).to_string()
        + &String::from_utf8_lossy(&output.stdout);
    (output.status.success(), log)
}

/// Compile transpiled `src/` only (no generated forge tests) — for compile-time oracle gates.
fn forge_build_evm_src(yaml_rel: &Path, tag: &str) -> (bool, String) {
    let out_dir = unique_out_dir(tag);
    let files = transpile_evm(yaml_rel).expect("evm transpile");
    std::fs::create_dir_all(out_dir.join("src")).unwrap();
    std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).unwrap();
    for (rel, content) in &files {
        if !rel.starts_with("src/") {
            continue;
        }
        let path = out_dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
    }
    let output = Command::new("forge")
        .args(["build"])
        .current_dir(&out_dir)
        .output()
        .expect("forge build");
    let log = String::from_utf8_lossy(&output.stderr).to_string()
        + &String::from_utf8_lossy(&output.stdout);
    (output.status.success(), log)
}

fn eval_lean(yaml_rel: &Path, eval_body: &str, tag: &str) -> (bool, String) {
    if !lean_build_enabled() {
        return (true, "skip: set CAMBRIAN_TEST_LEAN_BUILD=1".into());
    }
    if !has_lake() {
        return (true, "skip: lake not on PATH".into());
    }
    let out_dir = unique_out_dir(tag);
    let files = transpile_lean(yaml_rel).expect("lean transpile");
    for (rel, content) in &files {
        let path = out_dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
    }
    std::fs::write(out_dir.join("Eval.lean"), eval_body).unwrap();
    let build = Command::new("lake")
        .args(["build"])
        .current_dir(&out_dir)
        .output()
        .expect("lake build");
    if !build.status.success() {
        return (
            false,
            format!(
                "lake build failed:\n{}\n{}",
                String::from_utf8_lossy(&build.stderr),
                String::from_utf8_lossy(&build.stdout)
            ),
        );
    }
    let eval = Command::new("lake")
        .args(["env", "lean", "Eval.lean"])
        .current_dir(&out_dir)
        .output()
        .expect("lake env lean");
    let stdout = String::from_utf8_lossy(&eval.stdout).trim().to_string();
    (eval.status.success() && stdout == "true", stdout)
}

fn lean_build_only(yaml_rel: &Path, tag: &str) -> (bool, String) {
    if !lean_build_enabled() {
        return (true, "skip: set CAMBRIAN_TEST_LEAN_BUILD=1".into());
    }
    if !has_lake() {
        return (true, "skip: lake not on PATH".into());
    }
    let out_dir = unique_out_dir(tag);
    let files = transpile_lean(yaml_rel).expect("lean transpile");
    for (rel, content) in &files {
        let path = out_dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
    }
    let build = Command::new("lake")
        .args(["build"])
        .current_dir(&out_dir)
        .output()
        .expect("lake build");
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&build.stderr),
        String::from_utf8_lossy(&build.stdout)
    );
    (!build.status.success(), log)
}

const AX03_01_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.LetDivZero
import Cambrian.Generated.LetDivZeroRoutes

#eval Id.run do
  let inst : LetDivZero.Identity := {}
  let ctx := Cambrian.MsgCtx.default
  let s0 := LetDivZero.State.default
  match LetDivZero.Local.split s0 ctx inst with
  | Except.error _ => pure true
  | Except.ok _ => pure false
"#;

#[test]
fn arch_ax03_01_evm_forge_let_div_zero_reverts() {
    if !has_forge() {
        eprintln!("skip T-ARCH-001 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax03_01_let_div_zero_evm.yaml"),
        "Ax03LetDivZero.t.sol",
        "test_ARCH_AX03_01_letDivZeroReverts",
        "T-ARCH-001",
    );
    assert!(
        ok,
        "T-ARCH-001 EVM: let-div0 must revert (H-AX-03-01):\n{log}"
    );
}

#[test]
fn arch_ax03_01_lean_let_div_zero_matches_evm() {
    let (ok, detail) = eval_lean(
        &yaml("ax03_01_let_div_zero_lean.yaml"),
        AX03_01_LEAN_EVAL,
        "T-ARCH-001",
    );
    assert!(
        ok,
        "T-ARCH-001 Lean: let-div0 must fail like EVM (H-AX-03-01):\n{detail}"
    );
}

#[test]
fn arch_ax03_01_lean_emission_uses_checked_div_in_let_route() {
    let files = transpile_lean(&yaml("ax03_01_let_div_zero_lean.yaml")).expect("lean transpile");
    let routes = files
        .get("Cambrian/Generated/LetDivZeroRoutes.lean")
        .expect("LetDivZeroRoutes.lean");
    assert!(
        routes.contains("checkedDiv"),
        "T-ARCH-001: let-route must use checkedDiv (H-AX-03-01):\n{routes}"
    );
}

const AX08_01_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.FailCallee
import Cambrian.Generated.StaticRelay
import Cambrian.Generated.World
import Cambrian.Generated.StaticRelayRoutes

#eval Id.run do
  let callee_inst : FailCallee.Identity := { cid := 2 }
  let relay_inst : StaticRelay.Identity := { sid := 1 }
  let w0 := Cambrian.Generated.World.withFailCallee
    (Cambrian.Generated.World.withStaticRelay Cambrian.Generated.World.default relay_inst StaticRelay.State.default)
    callee_inst FailCallee.State.default
  match StaticRelay.Routes.relay w0 relay_inst Cambrian.MsgCtx.default 2 0 with
  | Except.ok w1 =>
    let calls := (w1.storage.staticRelay relay_inst).m_calls
    pure (calls == 0)
  | Except.error _ =>
    let calls := (w0.storage.staticRelay relay_inst).m_calls
    pure (calls == 0)
"#;

#[test]
fn arch_ax08_01_evm_forge_static_fail_callee_reverts() {
    if !has_forge() {
        eprintln!("skip T-ARCH-002 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax08_01_static_fail_callee_evm.yaml"),
        "Ax08StaticFailCallee.t.sol",
        "test_ARCH_AX08_01_staticFailCalleeReverts",
        "T-ARCH-002",
    );
    assert!(
        ok,
        "T-ARCH-002 EVM: static typed send to failing callee must revert (H-AX-08-01):\n{log}"
    );
}

#[test]
fn arch_ax08_01_lean_static_fail_callee_matches_evm() {
    let (ok, detail) = eval_lean(
        &yaml("ax08_01_static_fail_callee_lean.yaml"),
        AX08_01_LEAN_EVAL,
        "T-ARCH-002",
    );
    assert!(
        ok,
        "T-ARCH-002 Lean: relay must not commit caller state when callee fails (H-AX-08-01):\n{detail}"
    );
}

#[test]
fn arch_ax08_01_lean_emission_no_except_get_d_on_total_relay() {
    let files =
        transpile_lean(&yaml("ax08_01_static_fail_callee_lean.yaml")).expect("lean transpile");
    let routes = files
        .get("Cambrian/Generated/StaticRelayRoutes.lean")
        .expect("StaticRelayRoutes.lean");
    let relay_body = routes
        .split("def relay")
        .nth(1)
        .unwrap_or(routes.as_str());
    assert!(
        !relay_body.contains("exceptGetD"),
        "T-ARCH-002: total relay must not silently recover cross-entity callee failure (H-AX-08-01):\n{relay_body}"
    );
}

const AX04_02_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.ValueProbe
import Cambrian.Generated.ValueProbeRoutes
import Cambrian.Generated.World

#eval Id.run do
  let inst : ValueProbe.Identity := {}
  let w0 := Cambrian.Generated.World.withValueProbe Cambrian.Generated.World.default inst ValueProbe.State.default
  let ctx_call := { Cambrian.MsgCtx.default with value := 42 }
  match ValueProbe.Routes.capture w0 inst ctx_call with
  | Except.ok (_, v) => pure (v == 42)
  | Except.error _ => pure false
"#;

#[test]
fn arch_ax04_02_evm_forge_msg_value_deal_oracle_passes() {
    if !has_forge() {
        eprintln!("skip T-ARCH-003 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge_transpiled(
        &yaml("ax04_02_msg_value_harness_evm.yaml"),
        "test_msg_value_deal_not_msg_value",
        "T-ARCH-003",
    );
    assert!(
        ok,
        "T-ARCH-003 EVM: generated harness must pass expect return 42 (H-AX-04-02):\n{log}"
    );
}

#[test]
fn arch_ax04_02_emission_evm_deal_and_call_value_lean_one_shot_ctx() {
    let evm = transpile_evm(&yaml("ax04_02_msg_value_harness_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax04_02_msg_value_harness_lean.yaml")).expect("lean transpile");
    let forge_test = evm
        .iter()
        .find(|(p, _)| p.ends_with("ValueProbe.t.sol"))
        .map(|(_, c)| c.as_str())
        .expect("ValueProbe.t.sol");
    let spec = lean
        .get("Cambrian/Generated/ValueProbeSpec.lean")
        .expect("ValueProbeSpec.lean");
    assert!(
        forge_test.contains("vm.deal(") && forge_test.contains("capture{value: 42}"),
        "T-ARCH-003 EVM harness must vm.deal and pass {{value: N}} on next call (H-AX-04-02):\n{forge_test}"
    );
    assert!(
        spec.contains("let ctx_call := { ctx with value := 42 }")
            && !spec.contains("let ctx := { ctx with value := 42 }"),
        "T-ARCH-003 Lean spec must one-shot ctx_call.value for next call only (H-AX-04-02):\n{spec}"
    );
}

#[test]
fn arch_ax04_02_lean_capture_reads_one_shot_ctx_value() {
    let (ok, detail) = eval_lean(
        &yaml("ax04_02_msg_value_harness_lean.yaml"),
        AX04_02_LEAN_EVAL,
        "T-ARCH-003",
    );
    assert!(
        ok,
        "T-ARCH-003 Lean: capture must return msg.value from one-shot ctx_call (H-AX-04-02):\n{detail}"
    );
}

const AX10_01_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.InstProbe
import Cambrian.Generated.InstProbeRoutes
import Cambrian.Generated.World

#eval Id.run do
  let inst : InstProbe.Identity := {}
  let w0 := Cambrian.Generated.World.withInstProbe Cambrian.Generated.World.default inst InstProbe.State.default
  let w1 := InstProbe.Routes.bump w0 inst Cambrian.MsgCtx.default
  let x := (w1.storage.instProbe inst).m_x
  pure (x == 1)
"#;

const AX10_01_INVALID_BODY_OVERRIDE: &str = r#"
entity InstProbe {
    routes {
        constructor() => []
        bump() => []
    }
    m_x: u64 {
        in constructor() => 0
        in bump() => m_x + 1
    }
}
property "BASE bump once" () for InstProbe {
    call bump()
    expect state { m_x: 1 }
}
#[instantiates("BASE bump once")]
test "CP bump to zero" for InstProbe {
    call bump()
    expect state { m_x: 0 }
}
"#;

#[test]
fn arch_ax10_01_validate_t37_rejects_body_override() {
    assert!(
        validate_has_error(AX10_01_INVALID_BODY_OVERRIDE, "T37"),
        "T-ARCH-004: linked test must not override catalog call/expect (H-AX-10-01)"
    );
}

#[test]
fn arch_ax10_01_evm_forge_linked_test_uses_catalog_expect() {
    if !has_forge() {
        eprintln!("skip T-ARCH-004 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge_transpiled(
        &yaml("ax10_01_instantiates_body_evm.yaml"),
        "test_BASE_bump_once___CP_linked_pins_only",
        "T-ARCH-004",
    );
    assert!(
        ok,
        "T-ARCH-004 EVM: linked #[instantiates] test must use catalog expect m_x=1 (H-AX-10-01):\n{log}"
    );
}

#[test]
fn arch_ax10_01_emission_lean_keeps_catalog_not_supplemental() {
    let evm = transpile_evm(&yaml("ax10_01_instantiates_body_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax10_01_instantiates_body_lean.yaml")).expect("lean transpile");
    let forge_test = evm
        .iter()
        .find(|(p, _)| p.ends_with("InstProbe.t.sol"))
        .map(|(_, c)| c.as_str())
        .expect("InstProbe.t.sol");
    let spec = lean
        .get("Cambrian/Generated/InstProbeSpec.lean")
        .expect("InstProbeSpec.lean");
    assert!(
        forge_test.contains("assertEq(_instProbe.m_x(), 1"),
        "T-ARCH-004 EVM must use catalog expect m_x=1 (H-AX-10-01):\n{forge_test}"
    );
    assert!(
        spec.contains("m_x = 1"),
        "T-ARCH-004 Lean must emit catalog property m_x=1 (H-AX-10-01):\n{spec}"
    );
    assert!(
        !spec.contains("Spec.Tests"),
        "T-ARCH-004 Lean must not desugar #[instantiates] test into Spec.Tests (H-AX-10-01):\n{spec}"
    );
}

#[test]
fn arch_ax10_01_lean_exec_after_bump_matches_catalog() {
    let (ok, detail) = eval_lean(
        &yaml("ax10_01_instantiates_body_lean.yaml"),
        AX10_01_LEAN_EVAL,
        "T-ARCH-004",
    );
    assert!(
        ok,
        "T-ARCH-004 Lean: post-bump state must match catalog expect m_x=1 (H-AX-10-01):\n{detail}"
    );
}

const AX04_04_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.SenderZero
import Cambrian.Generated.SenderZeroSpec
import Cambrian.Generated.World

#eval Id.run do
  let inst : SenderZero.Identity := {}
  let w0 := Cambrian.Generated.World.withSenderZero
    Cambrian.Generated.World.default inst ({ SenderZero.State.default with m_zero := 0 })
  let ctx := { Cambrian.MsgCtx.default with sender := (1 : Cambrian.Address) }
  match SenderZero.Spec.Invariants.no_zero_hits.step w0 inst ctx .countIfZero with
  | .error _ => pure ((w0.storage.senderZero inst).m_zero == 0)
  | .ok w1 => pure ((w1.storage.senderZero inst).m_zero == 0)
"#;

#[test]
fn arch_ax04_04_evm_forge_invariant_passes_without_senders() {
    if !has_forge() {
        eprintln!("skip T-ARCH-005 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge_transpiled(
        &yaml("ax04_04_invariant_sender_zero_evm.yaml"),
        "invariant_no_zero_hits",
        "T-ARCH-005",
    );
    assert!(
        ok,
        "T-ARCH-005 EVM: invariant must pass when handler sender is non-zero (H-AX-04-04):\n{log}"
    );
}

#[test]
fn arch_ax04_04_lean_emission_empty_senders_uses_ambient_ctx() {
    let evm = transpile_evm(&yaml("ax04_04_invariant_sender_zero_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax04_04_invariant_sender_zero_lean.yaml")).expect("lean transpile");
    let handler = evm
        .iter()
        .find(|(p, _)| p.contains("Invariant_SenderZero"))
        .map(|(_, c)| c.as_str())
        .expect("invariant handler sol");
    let spec = lean
        .get("Cambrian/Generated/SenderZeroSpec.lean")
        .expect("SenderZeroSpec.lean");
    assert!(
        !handler.contains("vm.prank("),
        "T-ARCH-005 EVM handler must not pin senders without senders {{}} (H-AX-04-04):\n{handler}"
    );
    assert!(
        spec.contains("def senders : List Cambrian.Address :=\n  []"),
        "T-ARCH-005 Lean must emit empty senders list without fallback (H-AX-04-04):\n{spec}"
    );
    assert!(
        !spec.contains("(sender_idx : Nat)")
            && !spec.contains("senders.getD (sender_idx"),
        "T-ARCH-005 Lean step must use ambient ctx without sender_idx (H-AX-04-04):\n{spec}"
    );
}

#[test]
fn arch_ax04_04_lean_invariant_step_honors_ambient_sender() {
    let (ok, detail) = eval_lean(
        &yaml("ax04_04_invariant_sender_zero_lean.yaml"),
        AX04_04_LEAN_EVAL,
        "T-ARCH-005",
    );
    assert!(
        ok,
        "T-ARCH-005 Lean: invariant step with ambient sender=0 must not increment m_zero (H-AX-04-04):\n{detail}"
    );
}

const AX07_01_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.VecOob
import Cambrian.Generated.VecOobRoutes
import Cambrian.Generated.World

#eval Id.run do
  let inst : VecOob.Identity := {}
  let w0 := Cambrian.Generated.World.withVecOob Cambrian.Generated.World.default inst VecOob.State.default
  let w1 := VecOob.Routes.constructor w0 inst Cambrian.MsgCtx.default
  match VecOob.Routes.readAt w1 inst Cambrian.MsgCtx.default 5 with
  | Except.error _ => pure true
  | Except.ok _ => pure false
"#;

#[test]
fn arch_ax07_01_evm_forge_vec_oob_reverts() {
    if !has_forge() {
        eprintln!("skip T-ARCH-006 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax07_01_vec_oob_evm.yaml"),
        "Ax07VecOob.t.sol",
        "test_ARCH_AX07_01_vecIndexOobReverts",
        "T-ARCH-006",
    );
    assert!(
        ok,
        "T-ARCH-006 EVM: OOB Vec index must revert Panic 0x32 (H-AX-07-01):\n{log}"
    );
}

#[test]
fn arch_ax07_01_lean_emission_uses_checked_list_get() {
    let files = transpile_lean(&yaml("ax07_01_vec_oob_lean.yaml")).expect("lean transpile");
    let routes = files
        .get("Cambrian/Generated/VecOobRoutes.lean")
        .expect("VecOobRoutes.lean");
    assert!(
        routes.contains("checkedListGet"),
        "T-ARCH-006: Lean Vec index must use checkedListGet (H-AX-07-01):\n{routes}"
    );
}

#[test]
fn arch_ax07_01_lean_oob_read_errors_like_evm_revert() {
    let (ok, detail) = eval_lean(
        &yaml("ax07_01_vec_oob_lean.yaml"),
        AX07_01_LEAN_EVAL,
        "T-ARCH-006",
    );
    assert!(
        ok,
        "T-ARCH-006 Lean: OOB read must error like EVM Panic 0x32 (H-AX-07-01):\n{detail}"
    );
}

const AX03_02_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.ForDivZero
import Cambrian.Generated.ForDivZeroRoutes

#eval Id.run do
  let inst : ForDivZero.Identity := {}
  let ctx := Cambrian.MsgCtx.default
  let s0 := ForDivZero.State.default
  match ForDivZero.Local.foldDiv s0 ctx inst with
  | Except.error _ => pure true
  | Except.ok _ => pure false
"#;

#[test]
fn arch_ax03_02_evm_forge_for_div_zero_reverts() {
    if !has_forge() {
        eprintln!("skip T-ARCH-007 forge (for): forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax03_02_for_div_zero_evm.yaml"),
        "Ax03ForDivZero.t.sol",
        "test_ARCH_AX03_02_forDivZeroReverts",
        "T-ARCH-007-for",
    );
    assert!(
        ok,
        "T-ARCH-007 EVM: for-body div0 must revert (H-AX-03-02):\n{log}"
    );
}

#[test]
fn arch_ax03_02_lean_for_div_zero_matches_evm() {
    let (ok, detail) = eval_lean(
        &yaml("ax03_02_for_div_zero_lean.yaml"),
        AX03_02_LEAN_EVAL,
        "T-ARCH-007-for",
    );
    assert!(
        ok,
        "T-ARCH-007 Lean: for-body div0 must fail like EVM (H-AX-03-02):\n{detail}"
    );
}

#[test]
fn arch_ax03_02_lean_emission_uses_checked_div_in_for_route() {
    let files = transpile_lean(&yaml("ax03_02_for_div_zero_lean.yaml")).expect("lean transpile");
    let routes = files
        .get("Cambrian/Generated/ForDivZeroRoutes.lean")
        .expect("ForDivZeroRoutes.lean");
    assert!(
        routes.contains("checkedDiv"),
        "T-ARCH-007: for-route must use checkedDiv (H-AX-03-02):\n{routes}"
    );
}

const AX03_03_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.MatchDivZero
import Cambrian.Generated.MatchDivZeroRoutes

#eval Id.run do
  let inst : MatchDivZero.Identity := {}
  let ctx := Cambrian.MsgCtx.default
  let s0 := MatchDivZero.State.default
  let mode := DivMode.zero
  match MatchDivZero.Local.pick s0 ctx inst mode with
  | Except.error _ => pure true
  | Except.ok _ => pure false
"#;

#[test]
fn arch_ax03_03_evm_forge_match_div_zero_reverts() {
    if !has_forge() {
        eprintln!("skip T-ARCH-007 forge (match): forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax03_03_match_div_zero_evm.yaml"),
        "Ax03MatchDivZero.t.sol",
        "test_ARCH_AX03_03_matchDivZeroReverts",
        "T-ARCH-007-match",
    );
    assert!(
        ok,
        "T-ARCH-007 EVM: match-arm div0 must revert (H-AX-03-03):\n{log}"
    );
}

#[test]
fn arch_ax03_03_lean_match_div_zero_matches_evm() {
    let (ok, detail) = eval_lean(
        &yaml("ax03_03_match_div_zero_lean.yaml"),
        AX03_03_LEAN_EVAL,
        "T-ARCH-007-match",
    );
    assert!(
        ok,
        "T-ARCH-007 Lean: match-arm div0 must fail like EVM (H-AX-03-03):\n{detail}"
    );
}

#[test]
fn arch_ax03_03_lean_emission_uses_checked_div_in_match_route() {
    let files = transpile_lean(&yaml("ax03_03_match_div_zero_lean.yaml")).expect("lean transpile");
    let routes = files
        .get("Cambrian/Generated/MatchDivZeroRoutes.lean")
        .expect("MatchDivZeroRoutes.lean");
    assert!(
        routes.contains("checkedDiv"),
        "T-ARCH-007: match-route must use checkedDiv (H-AX-03-03):\n{routes}"
    );
}

const AX07_02_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.SilentZeroProbe
import Cambrian.Generated.ZeroGate
import Cambrian.Generated.World
import Cambrian.Generated.SilentZeroProbeRoutes

#eval Id.run do
  let probe_inst : SilentZeroProbe.Identity := { pid := 1 }
  let gate_inst : ZeroGate.Identity := { gid := 2 }
  let w0 := Cambrian.Generated.World.withZeroGate
    (Cambrian.Generated.World.withSilentZeroProbe Cambrian.Generated.World.default probe_inst SilentZeroProbe.State.default)
    gate_inst ZeroGate.State.default
  match SilentZeroProbe.Routes.relay w0 probe_inst Cambrian.MsgCtx.default 2 0 with
  | Except.ok w1 =>
    pure ((w1.storage.silentZeroProbe probe_inst).m_steps == 0)
  | Except.error _ =>
    pure ((w0.storage.silentZeroProbe probe_inst).m_steps == 0)
"#;

#[test]
fn arch_ax07_02_evm_forge_missing_slot_silent_zero_callee_probe() {
    if !has_forge() {
        eprintln!("skip T-ARCH-008 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax07_02_silent_zero_evm.yaml"),
        "Ax07SilentZero.t.sol",
        "test_ARCH_AX07_02_missingSlotSilentZeroThenRelayReverts",
        "T-ARCH-008",
    );
    assert!(
        ok,
        "T-ARCH-008 EVM: silent map 0 + failing callee must revert relay (H-AX-07-02):\n{log}"
    );
}

#[test]
fn arch_ax07_02_lean_emission_no_except_get_d_on_total_relay() {
    let files =
        transpile_lean(&yaml("ax07_02_silent_zero_lean.yaml")).expect("lean transpile");
    let routes = files
        .get("Cambrian/Generated/SilentZeroProbeRoutes.lean")
        .expect("SilentZeroProbeRoutes.lean");
    let relay_body = routes
        .split("def relay")
        .nth(1)
        .unwrap_or(routes.as_str());
    assert!(
        !relay_body.contains("exceptGetD"),
        "T-ARCH-008: relay must propagate callee failure, not exceptGetD (H-AX-07-02):\n{relay_body}"
    );
    assert!(
        relay_body.contains('←'),
        "T-ARCH-008: fail-promoted relay must bind callee with ← (H-AX-07-02):\n{relay_body}"
    );
}

#[test]
fn arch_ax07_02_lean_relay_commits_steps_after_silent_zero() {
    let (ok, detail) = eval_lean(
        &yaml("ax07_02_silent_zero_lean.yaml"),
        AX07_02_LEAN_EVAL,
        "T-ARCH-008",
    );
    assert!(
        ok,
        "T-ARCH-008 Lean: relay after silent-zero arg must not commit m_steps (H-AX-07-02):\n{detail}"
    );
}

fn validate_has_error(src: &str, code: &str) -> bool {
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    validate(&program)
        .into_iter()
        .any(|d| d.severity == Severity::Error && d.code == code)
}

const AX07_03_INVALID_IF_WITHOUT_ELSE: &str = r#"
entity IfBoolAlign {
    routes {
        constructor() => []
        pick(on: bool) -> bool => [
            let v = if on { true };
            return(v)
        ]
    }
    m_n: u64 { in constructor() => 0 in pick(_) => m_n }
}
"#;

const AX07_03_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.IfBoolAlign
import Cambrian.Generated.IfBoolAlignRoutes
import Cambrian.Generated.World

#eval Id.run do
  let inst : IfBoolAlign.Identity := {}
  let w0 := Cambrian.Generated.World.withIfBoolAlign Cambrian.Generated.World.default inst IfBoolAlign.State.default
  let w1 := IfBoolAlign.Routes.constructor w0 inst Cambrian.MsgCtx.default
  let w2 := IfBoolAlign.Routes.pick w1 inst Cambrian.MsgCtx.default true
  let (_, t) := IfBoolAlign.Routes.getFlag w2 inst Cambrian.MsgCtx.default
  let w3 := IfBoolAlign.Routes.pick w2 inst Cambrian.MsgCtx.default false
  let (_, f) := IfBoolAlign.Routes.getFlag w3 inst Cambrian.MsgCtx.default
  pure (t == true && f == false)
"#;

#[test]
fn arch_ax07_03_validate_v48_blocks_if_without_else_in_route() {
    assert!(
        validate_has_error(AX07_03_INVALID_IF_WITHOUT_ELSE, "V48"),
        "T-ARCH-009: value-context if-without-else must be rejected (H-AX-07-03 / V48)"
    );
}

#[test]
fn arch_ax07_03_evm_forge_bool_if_else_aligned() {
    if !has_forge() {
        eprintln!("skip T-ARCH-009 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax07_03_if_no_else_evm.yaml"),
        "Ax07IfNoElse.t.sol",
        "test_ARCH_AX07_03_boolIfElsePickAligned",
        "T-ARCH-009",
    );
    assert!(
        ok,
        "T-ARCH-009 EVM: bool if/else pick must be aligned (H-AX-07-03 REFUTED):\n{log}"
    );
}

#[test]
fn arch_ax07_03_lean_emission_explicit_else_not_unit() {
    let files = transpile_lean(&yaml("ax07_03_if_no_else_lean.yaml")).expect("lean transpile");
    let entity = files
        .get("Cambrian/Generated/IfBoolAlign.lean")
        .expect("IfBoolAlign.lean");
    assert!(
        entity.contains("if on then true else false"),
        "T-ARCH-009: Lean member pick must use explicit else branch (H-AX-07-03):\n{entity}"
    );
    assert!(
        !entity.contains("else ()"),
        "T-ARCH-009: valid source must not lower to implicit unit else (H-AX-07-03):\n{entity}"
    );
}

#[test]
fn arch_ax07_03_lean_eval_bool_if_else_aligned() {
    let (ok, detail) = eval_lean(
        &yaml("ax07_03_if_no_else_lean.yaml"),
        AX07_03_LEAN_EVAL,
        "T-ARCH-009",
    );
    assert!(
        ok,
        "T-ARCH-009 Lean: bool if/else pick must match EVM (H-AX-07-03 REFUTED):\n{detail}"
    );
}

const AX01_01_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.UnderfundCreditor
import Cambrian.Generated.UnderfundPayer
import Cambrian.Generated.World
import Cambrian.Generated.UnderfundPayerRoutes

#eval Id.run do
  let creditor : UnderfundCreditor.Identity := {}
  let payer : UnderfundPayer.Identity := {}
  let w0 := Cambrian.Generated.World.withUnderfundCreditor
    (Cambrian.Generated.World.withUnderfundPayer Cambrian.Generated.World.default payer UnderfundPayer.State.default)
    creditor UnderfundCreditor.State.default
  let payerAddr := UnderfundPayer.address payer
  let w0 := { w0 with balances := fun a => if a = payerAddr then (100 : BitVec 256) else 0 }
  match UnderfundPayer.Routes.tryPay w0 payer Cambrian.MsgCtx.default 1000 with
  | Except.error _ => pure true
  | Except.ok w1 =>
    let spent := (w1.storage.underfundCreditor creditor).m_spent
    let done := (w1.storage.underfundPayer payer).m_done
    pure (spent == 0 && done == 0)
"#;

#[test]
fn arch_bc00_lean_ax01_yaml_emits_executable_create2_address() {
    let files =
        transpile_lean(&yaml("ax01_01_underfund_typed_call_lean.yaml")).expect("lean transpile");
    let evm = files.get("Cambrian/Evm.lean").expect("Cambrian/Evm.lean");
    assert!(
        evm.contains("def create2Address") && !evm.contains("opaque create2Address"),
        "W2-BC-00: ax01 lean yaml must use executable intrinsics for #eval addresses:\n{evm}"
    );
}

#[test]
fn arch_ax01_01_evm_forge_underfunded_typed_call_reverts() {
    if !has_forge() {
        eprintln!("skip T-ARCH-010 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax01_01_underfund_typed_call_evm.yaml"),
        "Ax01UnderfundTypedCall.t.sol",
        "test_ARCH_AX01_01_underfundedTypedCallReverts",
        "T-ARCH-010",
    );
    assert!(
        ok,
        "T-ARCH-010 EVM: underfunded typed send must revert (H-AX-01-01):\n{log}"
    );
}

#[test]
fn arch_ax01_01_lean_emission_call_propagates_transfer_failure() {
    let files =
        transpile_lean(&yaml("ax01_01_underfund_typed_call_lean.yaml")).expect("lean transpile");
    let routes = files
        .get("Cambrian/Generated/UnderfundPayerRoutes.lean")
        .expect("UnderfundPayerRoutes.lean");
    let try_pay = routes
        .split("def tryPay")
        .nth(1)
        .unwrap_or(routes.as_str());
    assert!(
        try_pay.contains("Cambrian.WorldState.call"),
        "T-ARCH-010: value-bearing typed send must use WorldState.call (H-AX-01-01):\n{try_pay}"
    );
    assert!(
        try_pay.contains("Cambrian.RouteResult"),
        "T-ARCH-010: value-bearing send promotes caller to RouteResult (H-AX-01-01):\n{try_pay}"
    );
    assert!(
        try_pay.contains("← Cambrian.WorldState.call"),
        "T-ARCH-010: underfunded transfer must propagate via ← (H-AX-01-01):\n{try_pay}"
    );
    assert!(
        !try_pay.contains("exceptGetD"),
        "T-ARCH-010: must not swallow transfer failure via exceptGetD (H-AX-01-01):\n{try_pay}"
    );
}

#[test]
fn arch_ax01_01_lean_underfunded_call_commits_callee_value() {
    let (ok, detail) = eval_lean(
        &yaml("ax01_01_underfund_typed_call_lean.yaml"),
        AX01_01_LEAN_EVAL,
        "T-ARCH-010",
    );
    assert!(
        ok,
        "T-ARCH-010 Lean: underfunded call must not credit callee / commit caller (H-AX-01-01):\n{detail}"
    );
}

const AX01_03_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.ValueSpawner
import Cambrian.Generated.ValueVault
import Cambrian.Generated.World
import Cambrian.Generated.ValueSpawnerRoutes

#eval Id.run do
  let inst : ValueSpawner.Identity := { m_id := 1 }
  let vid : ValueVault.Identity := { m_id := 42 }
  let w0 := Cambrian.Generated.World.withValueSpawner
    Cambrian.Generated.World.default inst ValueSpawner.State.default
  let spawnerAddr := ValueSpawner.address inst
  let w0 := { w0 with balances := fun a => if a = spawnerAddr then (100 : BitVec 256) else 0 }
  match ValueSpawner.Routes.spawn w0 inst Cambrian.MsgCtx.default 42 1000 with
  | Except.error _ => pure true
  | Except.ok w =>
    let spawns := (w.storage.valueSpawner inst).m_spawns
    let deployed := w.storage.valueVault_deployed vid
    pure (spawns == 0 && !deployed)
"#;

#[test]
fn arch_ax01_03_evm_forge_underfunded_deploy_value_reverts() {
    if !has_forge() {
        eprintln!("skip T-ARCH-011 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax01_03_deploy_value_underfund_evm.yaml"),
        "Ax01DeployValueUnderfund.t.sol",
        "test_ARCH_AX01_03_underfundedDeployValueReverts",
        "T-ARCH-011",
    );
    assert!(
        ok,
        "T-ARCH-011 EVM: underfunded deploy value must revert (H-AX-01-03):\n{log}"
    );
}

#[test]
fn arch_ax01_03_lean_emission_deploy_propagates_transfer_failure() {
    let files = transpile_lean(&yaml("ax01_03_deploy_value_underfund_lean.yaml")).expect("lean transpile");
    let routes = files
        .get("Cambrian/Generated/ValueSpawnerRoutes.lean")
        .expect("ValueSpawnerRoutes.lean");
    let spawn_body = routes
        .split("def spawn")
        .nth(1)
        .unwrap_or(routes.as_str());
    assert!(
        spawn_body.contains("Cambrian.WorldState.call"),
        "T-ARCH-011: deploy value must use WorldState.call (H-AX-01-03):\n{spawn_body}"
    );
    assert!(
        spawn_body.contains("← Cambrian.WorldState.call"),
        "T-ARCH-011: underfunded deploy value must propagate via ← (H-AX-01-03):\n{spawn_body}"
    );
    assert!(
        !spawn_body.contains("exceptGetD"),
        "T-ARCH-011: must not install after failed value transfer (H-AX-01-03):\n{spawn_body}"
    );
}

#[test]
fn arch_ax01_03_lean_underfunded_deploy_commits_spawn() {
    let (ok, detail) = eval_lean(
        &yaml("ax01_03_deploy_value_underfund_lean.yaml"),
        AX01_03_LEAN_EVAL,
        "T-ARCH-011",
    );
    assert!(
        ok,
        "T-ARCH-011 Lean: underfunded deploy must not install vault / commit spawns (H-AX-01-03):\n{detail}"
    );
}

const AX04_01_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.SenderGate
import Cambrian.Generated.SenderGateRoutes
import Cambrian.Generated.World

#eval Id.run do
  let inst : SenderGate.Identity := {}
  let w0 := Cambrian.Generated.World.withSenderGate Cambrian.Generated.World.default inst SenderGate.State.default
  let ctx0 := Cambrian.MsgCtx.default
  match SenderGate.Routes.gate w0 inst ctx0 with
  | Except.error _ => pure true
  | Except.ok w1 =>
    let ctx1 := { ctx0 with sender := (0x1 : Cambrian.Address) }
    let (_, ok) := SenderGate.Routes.readOk w1 inst ctx1
    pure ok
"#;

#[test]
fn arch_ax04_01_evm_forge_msg_before_ctor_and_guard_reverts() {
    if !has_forge() {
        eprintln!("skip T-ARCH-012 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax04_01_ctor_before_msg_evm.yaml"),
        "Ax04CtorBeforeMsg.t.sol",
        "test_ARCH_AX04_01",
        "T-ARCH-012",
    );
    assert!(
        ok,
        "T-ARCH-012 EVM: msg-before-ctor must pass; ctor-with-zero-sender must revert (H-AX-04-01):\n{log}"
    );
}

#[test]
fn arch_ax04_01_lean_emission_spec_ctor_before_msg_order() {
    let files = transpile_lean(&yaml("ax04_01_ctor_before_msg_lean.yaml")).expect("lean transpile");
    let spec = files
        .get("Cambrian/Generated/SenderGateSpec.lean")
        .expect("SenderGateSpec.lean");
    let bad = spec
        .split("theorem gate_before_msg_reverts")
        .nth(1)
        .unwrap_or(spec.as_str());
    let gate_pos = bad.find("Routes.gate").unwrap_or(usize::MAX);
    let msg_pos = bad.find("ctx with sender").unwrap_or(usize::MAX);
    assert!(
        gate_pos < msg_pos && gate_pos != usize::MAX && msg_pos != usize::MAX,
        "T-ARCH-012: bad-order test must emit gate before msg pin (H-AX-04-01):\n{bad}"
    );
    assert!(
        bad.contains("match SenderGate.Routes.gate"),
        "T-ARCH-012: spec must match on guarded init route (H-AX-04-01):\n{bad}"
    );
    assert!(
        bad.contains(".error _ => False"),
        "T-ARCH-012: failed init route must make theorem goal False (H-AX-04-01 REFUTED):\n{bad}"
    );
}

#[test]
fn arch_ax04_01_lean_gate_at_zero_sender_blocks_bad_order_proof() {
    let (ok, detail) = eval_lean(
        &yaml("ax04_01_ctor_before_msg_lean.yaml"),
        AX04_01_LEAN_EVAL,
        "T-ARCH-012",
    );
    assert!(
        ok,
        "T-ARCH-012 Lean: gate at zero sender must fail; spec must not prove readOk (H-AX-04-01 REFUTED):\n{detail}"
    );
}

const AX01_02_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.NoPayCallee
import Cambrian.Generated.ValueSender
import Cambrian.Generated.World
import Cambrian.Generated.ValueSenderRoutes

#eval Id.run do
  let callee : NoPayCallee.Identity := {}
  let inst : ValueSender.Identity := {}
  let w0 := Cambrian.Generated.World.withNoPayCallee
    (Cambrian.Generated.World.withValueSender Cambrian.Generated.World.default inst ValueSender.State.default)
    callee NoPayCallee.State.default
  match ValueSender.Routes.send w0 inst Cambrian.MsgCtx.default with
  | Except.error _ => pure true
  | Except.ok w1 =>
    let sent := (w1.storage.valueSender inst).m_sent
    let hits := (w1.storage.noPayCallee callee).m_hits
    pure (sent == 0 && hits == 0)
"#;

#[test]
fn arch_ax01_02_evm_forge_build_rejects_value_to_nonpayable_ping() {
    if !has_forge() {
        eprintln!("skip T-ARCH-013 forge: forge not on PATH");
        return;
    }
    let (ok, log) = forge_build_evm_src(
        &yaml("ax01_02_nonpayable_value_evm.yaml"),
        "T-ARCH-013",
    );
    assert!(
        !ok && log.contains("non-payable"),
        "T-ARCH-013 EVM: value send to non-payable ping must fail forge build (H-AX-01-02):\n{log}"
    );
}

#[test]
fn arch_ax01_02_lean_emission_call_credits_nonpayable_callee() {
    let evm = transpile_evm(&yaml("ax01_02_nonpayable_value_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax01_02_nonpayable_value_lean.yaml")).expect("lean transpile");
    let project = evm
        .iter()
        .find(|(p, _)| p.ends_with("_project.sol"))
        .map(|(_, c)| c.as_str())
        .expect("project sol");
    let routes = lean
        .get("Cambrian/Generated/ValueSenderRoutes.lean")
        .expect("ValueSenderRoutes.lean");
    let send_body = routes.split("def send").nth(1).unwrap_or(routes.as_str());
    assert!(
        project.contains("function ping() external;")
            && project.contains("ping{value: 1}"),
        "T-ARCH-013 EVM must emit non-payable interface + value call (H-AX-01-02):\n{project}"
    );
    assert!(
        send_body.contains("ThrowCode.ofNat 13"),
        "T-ARCH-013 Lean send must reject value to non-payable callee (H-AX-01-02):\n{send_body}"
    );
}

#[test]
fn arch_ax01_02_lean_send_commits_when_evm_would_not_compile() {
    let (ok, detail) = eval_lean(
        &yaml("ax01_02_nonpayable_value_lean.yaml"),
        AX01_02_LEAN_EVAL,
        "T-ARCH-013",
    );
    assert!(
        ok,
        "T-ARCH-013 Lean: send with value to non-payable callee must not commit (H-AX-01-02):\n{detail}"
    );
}

const AX04_03_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.MapPinProbe
import Cambrian.Generated.MapPinProbeRoutes
import Cambrian.Generated.World

#eval Id.run do
  let inst : MapPinProbe.Identity := {}
  let seed := (1 : Cambrian.Address)
  let w0 := Cambrian.Generated.World.withMapPinProbe
    Cambrian.Generated.World.default inst
    ({ MapPinProbe.State.default with
        m_balances := Cambrian.AddressMap.insert Cambrian.AddressMap.empty seed (10 : BitVec 64) })
  let w1 := MapPinProbe.Routes.credit w0 inst Cambrian.MsgCtx.default seed 5
  let (_, v) := MapPinProbe.Routes.read w1 inst Cambrian.MsgCtx.default seed
  pure (v == (15 : BitVec 64))
"#;

#[test]
fn arch_ax04_03_evm_forge_mapping_pin_applies_pinned_balance() {
    if !has_forge() {
        eprintln!("skip T-ARCH-014 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge_transpiled(
        &yaml("ax04_03_mapping_pin_evm.yaml"),
        "test_mapping_pin_credit",
        "T-ARCH-014",
    );
    assert!(
        ok,
        "T-ARCH-014 EVM: pinned mapping credit must read 15 (H-AX-04-03):\n{log}"
    );
}

#[test]
fn arch_ax04_03_emission_evm_vm_store_vs_lean_apply_mapping_pin() {
    let evm = transpile_evm(&yaml("ax04_03_mapping_pin_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax04_03_mapping_pin_lean_spec.yaml")).expect("lean transpile");
    let forge_test = evm
        .iter()
        .find(|(p, _)| p.ends_with("MapPinProbe.t.sol"))
        .map(|(_, c)| c.as_str())
        .expect("MapPinProbe.t.sol");
    let spec = lean
        .get("Cambrian/Generated/MapPinProbeSpec.lean")
        .expect("MapPinProbeSpec.lean");
    assert!(
        forge_test.contains("keccak256(abi.encode(") && forge_test.contains("vm.store"),
        "T-ARCH-014 EVM harness must seed mapping pin via vm.store (H-AX-04-03):\n{forge_test}"
    );
    assert!(
        spec.contains("m_balances := ({ SEED := 10 }")
            || spec.contains("m_balances := ({ seed := 10 }"),
        "T-ARCH-014 Lean spec must apply mapping pin in world init (H-AX-04-03):\n{spec}"
    );
}

#[test]
fn arch_ax04_03_lean_pinned_mapping_read_matches_credit() {
    let (ok, detail) = eval_lean(
        &yaml("ax04_03_mapping_pin_lean.yaml"),
        AX04_03_LEAN_EVAL,
        "T-ARCH-014",
    );
    assert!(
        ok,
        "T-ARCH-014 Lean: pinned mapping credit must read 15 (H-AX-04-03):\n{detail}"
    );
}

const AX01_04_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.ChildOwner
import Cambrian.Generated.ParentSpawner
import Cambrian.Generated.ParentSpawnerRoutes
import Cambrian.Generated.World

#eval Id.run do
  let inst : ParentSpawner.Identity := { m_id := 1 }
  let w0 := Cambrian.Generated.World.withParentSpawner
    Cambrian.Generated.World.default inst ParentSpawner.State.default
  let factory_addr := ParentSpawner.address inst
  let external := (factory_addr + 1)
  let ctx := { Cambrian.MsgCtx.default with sender := external }
  match ParentSpawner.Routes.spawn w0 inst ctx with
  | Except.error _ => pure false
  | Except.ok w =>
    let child : ChildOwner.Identity := {}
    let owner := (w.storage.childOwner child).m_owner
    pure (owner == factory_addr && owner != external)
"#;

#[test]
fn arch_ax01_04_evm_forge_deployed_child_owner_is_factory() {
    if !has_forge() {
        eprintln!("skip T-ARCH-015 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax01_04_deploy_sender_evm.yaml"),
        "Ax01DeploySender.t.sol",
        "test_ARCH_AX01_04",
        "T-ARCH-015",
    );
    assert!(
        ok,
        "T-ARCH-015 EVM: deployed child m_owner must be factory not parent (H-AX-01-04):\n{log}"
    );
}

#[test]
fn arch_ax01_04_lean_emission_deploy_uses_factory_sender() {
    let evm = transpile_evm(&yaml("ax01_04_deploy_sender_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax01_04_deploy_sender_lean.yaml")).expect("lean transpile");
    let project = evm
        .iter()
        .find(|(p, _)| p.ends_with("_project.sol"))
        .map(|(_, c)| c.as_str())
        .expect("project sol");
    let routes = lean
        .get("Cambrian/Generated/ParentSpawnerRoutes.lean")
        .expect("ParentSpawnerRoutes.lean");
    let spawn_body = routes.split("def spawn").nth(1).unwrap_or(routes.as_str());
    assert!(
        project.contains("address next_m_owner = msg.sender"),
        "T-ARCH-015 EVM child init must bind msg.sender (H-AX-01-04):\n{project}"
    );
    assert!(
        spawn_body.contains("ParentSpawner.address inst"),
        "T-ARCH-015 Lean deploy init must use factory/deployer address (H-AX-01-04):\n{spawn_body}"
    );
    assert!(
        !spawn_body.contains("m_owner := ctx.sender"),
        "T-ARCH-015 Lean deploy must not use outer route ctx.sender (H-AX-01-04):\n{spawn_body}"
    );
}

#[test]
fn arch_ax01_04_lean_deploy_child_owner_is_factory_not_route_caller() {
    let (ok, detail) = eval_lean(
        &yaml("ax01_04_deploy_sender_lean.yaml"),
        AX01_04_LEAN_EVAL,
        "T-ARCH-015",
    );
    assert!(
        ok,
        "T-ARCH-015 Lean: deployed child owner must be deployer address not route ctx.sender (H-AX-01-04):\n{detail}"
    );
}

const AX06_01_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.KeyOrderProbe
import Cambrian.Generated.KeyOrderProbeRoutes
import Cambrian.Generated.World

#eval Id.run do
  let inst : KeyOrderProbe.Identity := {}
  let w0 := Cambrian.Generated.World.withKeyOrderProbe
    Cambrian.Generated.World.default inst KeyOrderProbe.State.default
  let w1 := KeyOrderProbe.Routes.put w0 inst Cambrian.MsgCtx.default 1 10
  let w2 := KeyOrderProbe.Routes.put w1 inst Cambrian.MsgCtx.default 2 20
  let (_, fk) := KeyOrderProbe.Routes.firstKey w2 inst Cambrian.MsgCtx.default
  pure (fk == (1 : BitVec 64))
"#;

#[test]
fn arch_ax06_01_evm_forge_first_inserted_key_wins() {
    if !has_forge() {
        eprintln!("skip T-ARCH-016 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge_transpiled(
        &yaml("ax06_01_keys_order_evm.yaml"),
        "test_first_inserted_key_wins_on_evm",
        "T-ARCH-016",
    );
    assert!(
        ok,
        "T-ARCH-016 EVM: keys().fold first-key pick must return FIFO first insert (H-AX-06-01):\n{log}"
    );
}

#[test]
fn arch_ax06_01_emission_evm_fifo_push_vs_lean_lifo_prepend() {
    let evm = transpile_evm(&yaml("ax06_01_keys_order_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax06_01_keys_order_lean.yaml")).expect("lean transpile");
    let sol = evm
        .iter()
        .find(|(p, _)| p.ends_with("_project.sol"))
        .map(|(_, c)| c.as_str())
        .expect("project sol");
    let routes = lean
        .get("Cambrian/Generated/KeyOrderProbeRoutes.lean")
        .expect("KeyOrderProbeRoutes.lean");
    let put_body = routes.split("def put").nth(1).unwrap_or(routes.as_str());
    let first_body = routes.split("def firstKey").nth(1).unwrap_or(routes.as_str());
    assert!(
        sol.contains("m_keys.push") || sol.contains("_keys.push"),
        "T-ARCH-016 EVM put must append to parallel keys array (H-AX-06-01):\n{sol}"
    );
    assert!(
        put_body.contains("pushKeyIfNew"),
        "T-ARCH-016 Lean put must prepend ghost keys via pushKeyIfNew (H-AX-06-01):\n{put_body}"
    );
    assert!(
        first_body.contains("List.foldl") && first_body.contains("m_keys"),
        "T-ARCH-016 Lean firstKey must fold ghost keys list (H-AX-06-01):\n{first_body}"
    );
}

#[test]
fn arch_ax06_01_lean_first_key_matches_evm_fifo_not_lifo() {
    let (ok, detail) = eval_lean(
        &yaml("ax06_01_keys_order_lean.yaml"),
        AX06_01_LEAN_EVAL,
        "T-ARCH-016",
    );
    assert!(
        ok,
        "T-ARCH-016 Lean: firstKey must match EVM FIFO first insert (H-AX-06-01):\n{detail}"
    );
}

const AX05_01_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.SignMixProbe
import Cambrian.Generated.SignMixProbeRoutes
import Cambrian.Generated.World

#eval Id.run do
  let inst : SignMixProbe.Identity := {}
  let w0 := Cambrian.Generated.World.withSignMixProbe
    Cambrian.Generated.World.default inst SignMixProbe.State.default
  let (_, b) := SignMixProbe.Routes.highLessThan w0 inst Cambrian.MsgCtx.default 1
  pure (b == false)
"#;

#[test]
fn arch_ax05_01_evm_forge_build_accepts_mixed_sign_lt_with_promotion() {
    if !has_forge() {
        eprintln!("skip T-ARCH-017 forge: forge not on PATH");
        return;
    }
    let (ok, log) = forge_build_evm_src(&yaml("ax05_01_sign_mix_evm.yaml"), "T-ARCH-017");
    assert!(
        ok,
        "T-ARCH-017 EVM: mixed-sign `<` must compile with SignPromote (H-AX-05-01):\n{log}"
    );
}

#[test]
fn arch_ax05_01_emission_evm_plain_lt_vs_lean_slt() {
    let evm = transpile_evm(&yaml("ax05_01_sign_mix_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax05_01_sign_mix_lean.yaml")).expect("lean transpile");
    let project = evm
        .iter()
        .find(|(p, _)| p.ends_with("_project.sol"))
        .map(|(_, c)| c.as_str())
        .expect("project sol");
    let routes = lean
        .get("Cambrian/Generated/SignMixProbeRoutes.lean")
        .expect("SignMixProbeRoutes.lean");
    let cmp_body = routes
        .split("def highLessThan")
        .nth(1)
        .unwrap_or(routes.as_str());
    assert!(
        project.contains("int256(uint256(HIGH))") && project.contains("int256(i)"),
        "T-ARCH-017 EVM must SignPromote mixed-sign `<` operands (H-AX-05-01):\n{project}"
    );
    assert!(
        cmp_body.contains(".slt"),
        "T-ARCH-017 Lean compare must use signed `.slt` (H-AX-05-01):\n{cmp_body}"
    );
}

#[test]
fn arch_ax05_01_lean_signed_lt_matches_evm_promoted_compare() {
    let (ok, detail) = eval_lean(
        &yaml("ax05_01_sign_mix_lean.yaml"),
        AX05_01_LEAN_EVAL,
        "T-ARCH-017",
    );
    assert!(
        ok,
        "T-ARCH-017 Lean: signed `.slt` must match EVM SignPromote compare false (H-AX-05-01):\n{detail}"
    );
}

const AX06_02_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.KeysUpdateProbe
import Cambrian.Generated.KeysUpdateProbeRoutes
import Cambrian.Generated.World

#eval Id.run do
  let inst : KeysUpdateProbe.Identity := {}
  let w0 := Cambrian.Generated.World.withKeysUpdateProbe
    Cambrian.Generated.World.default inst KeysUpdateProbe.State.default
  let w1 := KeysUpdateProbe.Routes.seedBoth w0 inst Cambrian.MsgCtx.default
  let (_, n) := KeysUpdateProbe.Routes.countKeys w1 inst Cambrian.MsgCtx.default
  pure (n == (2 : BitVec 64))
"#;

#[test]
fn arch_ax06_02_evm_forge_chained_insert_key_count() {
    if !has_forge() {
        eprintln!("skip T-ARCH-018 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge_transpiled(
        &yaml("ax06_02_keys_update_evm.yaml"),
        "test_chained_insert_key_count",
        "T-ARCH-018",
    );
    assert!(
        ok,
        "T-ARCH-018 EVM: keys().fold count after chained insert must be 2 (H-AX-06-02):\n{log}"
    );
}

#[test]
fn arch_ax06_02_emission_evm_dual_push_vs_lean_single_push_key() {
    let evm = transpile_evm(&yaml("ax06_02_keys_update_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax06_02_keys_update_lean.yaml")).expect("lean transpile");
    let sol = evm
        .iter()
        .find(|(p, _)| p.ends_with("_project.sol"))
        .map(|(_, c)| c.as_str())
        .expect("project sol");
    let routes = lean
        .get("Cambrian/Generated/KeysUpdateProbeRoutes.lean")
        .expect("KeysUpdateProbeRoutes.lean");
    let seed_body = routes.split("def seedBoth").nth(1).unwrap_or(routes.as_str());
    let count_body = routes.split("def countKeys").nth(1).unwrap_or(routes.as_str());
    assert!(
        sol.matches("m_keys.push").count() >= 2,
        "T-ARCH-018 EVM seedBoth must push both keys into `_keys` (H-AX-06-02):\n{sol}"
    );
    assert!(
        seed_body.contains("pushKeyIfNew")
            && seed_body.contains("s.m_keys")
            && seed_body.contains(" 1")
            && seed_body.contains(" 2"),
        "T-ARCH-018 Lean seedBoth must track both chained insert keys (H-AX-06-02):\n{seed_body}"
    );
    assert!(
        count_body.contains("m_keys"),
        "T-ARCH-018 Lean countKeys must fold ghost `m_keys` (H-AX-06-02):\n{count_body}"
    );
}

#[test]
fn arch_ax06_02_lean_key_count_undercounts_chained_insert() {
    let (ok, detail) = eval_lean(
        &yaml("ax06_02_keys_update_lean.yaml"),
        AX06_02_LEAN_EVAL,
        "T-ARCH-018",
    );
    assert!(
        ok,
        "T-ARCH-018 Lean: ghost `m_keys` count must match EVM map size 2 (H-AX-06-02):\n{detail}"
    );
}

const AX05_02_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.LetAddProbe
import Cambrian.Generated.LetAddProbeRoutes

#eval Id.run do
  let inst : LetAddProbe.Identity := {}
  let s0 := LetAddProbe.State.default
  let ctx := Cambrian.MsgCtx.default
  match LetAddProbe.Local.addViaLet s0 ctx inst (BitVec.allOnes 64) 1 with
  | Except.error _ => pure true
  | Except.ok _ => pure false
"#;

#[test]
fn arch_ax05_02_evm_forge_let_u64_add_max_plus_one_reverts() {
    if !has_forge() {
        eprintln!("skip T-ARCH-019 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax05_02_let_u64_overflow_evm.yaml"),
        "Ax05LetU64Overflow.t.sol",
        "test_ARCH_AX05_02_letU64AddMaxPlusOneReverts",
        "T-ARCH-019",
    );
    assert!(
        ok,
        "T-ARCH-019 EVM: let-bound u64 add must revert at MAX+1 (H-AX-05-02):\n{log}"
    );
}

#[test]
fn arch_ax05_02_emission_evm_decl_let_vs_lean_wrap_add() {
    let evm = transpile_evm(&yaml("ax05_02_let_u64_overflow_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax05_02_let_u64_overflow_lean.yaml")).expect("lean transpile");
    let sol = evm
        .iter()
        .find(|(p, _)| p.ends_with("_project.sol"))
        .map(|(_, c)| c.as_str())
        .expect("project sol");
    let routes = lean
        .get("Cambrian/Generated/LetAddProbeRoutes.lean")
        .expect("LetAddProbeRoutes.lean");
    let body = routes.split("def addViaLet").nth(1).unwrap_or(routes.as_str());
    assert!(
        sol.contains("uint64 sum = (a + b);"),
        "T-ARCH-019 EVM let must bind checked uint64 add (InferSolMode::Decl) (H-AX-05-02):\n{sol}"
    );
    assert!(
        body.contains("checkedAdd"),
        "T-ARCH-019 Lean overflow-panic let add must use checkedAdd (H-AX-05-02):\n{body}"
    );
}

#[test]
fn arch_ax05_02_lean_let_u64_add_errors_when_evm_reverts() {
    let (ok, detail) = eval_lean(
        &yaml("ax05_02_let_u64_overflow_lean.yaml"),
        AX05_02_LEAN_EVAL,
        "T-ARCH-019",
    );
    assert!(
        ok,
        "T-ARCH-019 Lean: let u64 add must error when EVM reverts (H-AX-05-02):\n{detail}"
    );
}

const AX06_03_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.DivMemberProbe
import Cambrian.Generated.DivMemberProbeRoutes

#eval Id.run do
  let inst : DivMemberProbe.Identity := {}
  let s0 := DivMemberProbe.State.default
  let ctx := Cambrian.MsgCtx.default
  match DivMemberProbe.Local.split s0 ctx inst 0 with
  | Except.error _ => pure true
  | Except.ok _ => pure false
"#;

#[test]
fn arch_ax06_03_evm_forge_member_div_zero_reverts() {
    if !has_forge() {
        eprintln!("skip T-ARCH-020 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax06_03_member_div_evm.yaml"),
        "Ax06MemberDivZero.t.sol",
        "test_ARCH_AX06_03_memberDivZeroReverts",
        "T-ARCH-020",
    );
    assert!(
        ok,
        "T-ARCH-020 EVM: member div0 must revert (H-AX-06-03 ground truth):\n{log}"
    );
}

#[test]
fn arch_ax06_03_lean_route_fails_on_member_div_zero() {
    let (ok, detail) = eval_lean(
        &yaml("ax06_03_member_div_lean.yaml"),
        AX06_03_LEAN_EVAL,
        "T-ARCH-020",
    );
    assert!(
        ok,
        "T-ARCH-020 Lean gen_project: split(0) must fail via RouteResult (H-AX-06-03):\n{detail}"
    );
}

#[test]
fn arch_ax06_03_predictor_mirror_stale_nat_tls_hides_div0_fail_surface() {
    let nat_yaml = yaml("ax06_03_tls_nat_lean.yaml");
    let _ = transpile_lean(&nat_yaml).expect("nat lean transpile seeds TLS");
    let body = member_transform_body(&nat_yaml, "DivMemberProbe", "m_a", "split");
    let project = Project::load(&nat_yaml).expect("load nat project");
    let stale = member_transform_body_checked(&body, &project.merged.pure_fns, true, false);
    assert!(
        stale,
        "T-ARCH-020 predictor mirror must classify div0 member as checked when overflow_panic=true (H-AX-06-03 stale TLS):\nclassified={stale}"
    );
    let _ = transpile_lean(&yaml("ax06_03_member_div_lean.yaml"))
        .expect("overflow-panic transpile refreshes TLS");
}

#[test]
fn arch_ax03_04_evm_forge_match_fail_pure_reverts() {
    if !has_forge() {
        eprintln!("skip T-ARCH-021 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax03_04_match_fail_pure_evm.yaml"),
        "Ax03MatchFailPure.t.sol",
        "test_ARCH_AX03_04_matchFailPureReverts",
        "T-ARCH-021",
    );
    assert!(
        ok,
        "T-ARCH-021 EVM: match-arm fail-pure div0 must revert (H-AX-03-04):\n{log}"
    );
}

#[test]
fn arch_ax03_04_detector_skips_match_fail_pure_arm() {
    let lean_yaml = yaml("ax03_04_match_fail_pure_lean.yaml");
    let project = Project::load(&lean_yaml).expect("load project");
    let value = route_let_value(&lean_yaml, "MatchFailPure", "pick");
    let detected = expr_forces_fail_surface(&value, &project.merged.pure_fns);
    assert!(
        detected,
        "T-ARCH-021: match arm fail-pure must force fail surface (H-AX-03-04):\nclassified={detected}"
    );
}

#[test]
fn arch_ax03_04_lean_lake_build_succeeds_after_fail_pure_match_fix() {
    let (failed, detail) = lean_build_only(
        &yaml("ax03_04_match_fail_pure_lean.yaml"),
        "T-ARCH-021",
    );
    if detail.starts_with("skip:") {
        eprintln!("skip T-ARCH-021 lake build: {detail}");
        return;
    }
    assert!(
        !failed,
        "T-ARCH-021 Lean: match arm fail-pure must lake build after RouteResult homogenization (H-AX-03-04):\n{detail}"
    );
}

#[test]
fn arch_ax03_04_lean_emission_route_must_be_fail_mode() {
    let files =
        transpile_lean(&yaml("ax03_04_match_fail_pure_lean.yaml")).expect("lean transpile");
    let routes = files
        .get("Cambrian/Generated/MatchFailPureRoutes.lean")
        .expect("MatchFailPureRoutes.lean");
    assert!(
        routes.contains("RouteResult") || routes.contains(">>="),
        "T-ARCH-021: match-route with fail-pure arm must lower fail-aware (H-AX-03-04):\n{routes}"
    );
}

fn typed_has_widen_coerce(te: &TypedExpr) -> bool {
    fn walk(te: &TypedExpr) -> bool {
        match &te.kind {
            TypedExprKind::Coerce {
                kind: CoerceKind::Widen,
                ..
            } => true,
            TypedExprKind::BinOp { lhs, rhs, .. } => walk(lhs) || walk(rhs),
            TypedExprKind::Coerce { expr, .. } | TypedExprKind::Cast { expr, .. } => walk(expr),
            TypedExprKind::FieldAccess { base, .. } => walk(base),
            _ => false,
        }
    }
    walk(te)
}

fn route_return_expr(yaml_rel: &Path, entity: &str, route_name: &str) -> Expr {
    let project = Project::load(yaml_rel).expect("load project");
    let route = project
        .merged
        .entities
        .iter()
        .find(|e| e.name == entity)
        .and_then(|e| e.routes.iter().find(|r| r.name == route_name))
        .unwrap_or_else(|| panic!("missing route {entity}.{route_name} in {}", yaml_rel.display()));
    route
        .body
        .all_actions()
        .into_iter()
        .find_map(|a| match a {
            RouteAction::Return { values } if values.len() == 1 => Some(values[0].clone()),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "missing unary return in route {entity}.{route_name} ({})",
                yaml_rel.display()
            )
        })
}

#[test]
fn arch_ax05_03_ir_widen_coerce_lost_after_materialize_roundtrip() {
    let yaml = yaml("ax05_03_coerce_widen_evm.yaml");
    let project = Project::load(&yaml).expect("load project");
    let entity = project
        .merged
        .entities
        .iter()
        .find(|e| e.name == "CoerceWidenProbe")
        .expect("CoerceWidenProbe entity");
    let route = entity
        .routes
        .iter()
        .find(|r| r.name == "mix")
        .expect("mix route");
    let return_expr = route_return_expr(&yaml, "CoerceWidenProbe", "mix");
    let ctx = LowerCtx::new(&project.merged, entity, route);
    let typed = lower_expr(&return_expr, &ctx);
    assert!(
        typed_has_widen_coerce(&typed),
        "T-ARCH-022 setup: mixed-width binop must carry widen Coerce in IR (H-AX-05-03)"
    );
    let materialized = materialize_typed_expr(&typed);
    let retyped = lower_expr(&materialized, &ctx);
    assert!(
        typed_has_widen_coerce(&retyped),
        "T-ARCH-022: widen Coerce must survive IR→materialize→lower round-trip (H-AX-05-03):\ninitial={typed:?}\nretyped={retyped:?}"
    );
}

#[test]
fn arch_ax05_03_ir_hex_literal_kind_lost_after_materialize_roundtrip() {
    let yaml = yaml("ax05_03_coerce_widen_evm.yaml");
    let project = Project::load(&yaml).expect("load project");
    let entity = project
        .merged
        .entities
        .iter()
        .find(|e| e.name == "CoerceWidenProbe")
        .expect("CoerceWidenProbe entity");
    let route = entity
        .routes
        .iter()
        .find(|r| r.name == "mix")
        .expect("mix route");
    let ctx = LowerCtx::new(&project.merged, entity, route);
    let hex_ir = TypedExpr {
        ty: ResolvedType::simple("u64"),
        kind: TypedExprKind::HexLiteral("ff".into()),
    };
    let materialized = materialize_typed_expr(&hex_ir);
    let retyped = lower_expr(&materialized, &ctx);
    assert!(
        matches!(retyped.kind, TypedExprKind::HexLiteral(_)),
        "T-ARCH-022: HexLiteral IR kind must survive materialize round-trip (H-AX-05-03):\nmaterialized={materialized:?}\nretyped={retyped:?}"
    );
}

const AX03_05_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.ThrowProbe
import Cambrian.Generated.ThrowProbeRoutes

#eval Id.run do
  let inst : ThrowProbe.Identity := {}
  let s0 := ThrowProbe.State.default
  let ctx := Cambrian.MsgCtx.default
  match ThrowProbe.Local.bail s0 ctx inst with
  | Except.error _ => pure true
  | Except.ok _ => pure false
"#;

const SKIPPED_THROW_MARKER: &str = "skipped: route inferred non-failing";

#[test]
fn arch_ax03_05_evm_forge_explicit_throw_reverts() {
    if !has_forge() {
        eprintln!("skip T-ARCH-023 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax03_05_explicit_throw_evm.yaml"),
        "Ax03ExplicitThrow.t.sol",
        "test_ARCH_AX03_05_explicitThrowReverts",
        "T-ARCH-023",
    );
    assert!(
        ok,
        "T-ARCH-023 EVM: explicit throw must revert (H-AX-03-05 ground truth):\n{log}"
    );
}

#[test]
fn arch_ax03_05_lean_explicit_throw_matches_evm() {
    let (ok, detail) = eval_lean(
        &yaml("ax03_05_explicit_throw_lean.yaml"),
        AX03_05_LEAN_EVAL,
        "T-ARCH-023",
    );
    assert!(
        ok,
        "T-ARCH-023 Lean: explicit throw must fail like EVM (H-AX-03-05):\n{detail}"
    );
}

#[test]
fn arch_ax03_05_throw_route_is_fail_mode_and_emits_throw() {
    let lean_yaml = yaml("ax03_05_explicit_throw_lean.yaml");
    let project = Project::load(&lean_yaml).expect("load project");
    let entity = project
        .merged
        .entities
        .iter()
        .find(|e| e.name == "ThrowProbe")
        .expect("ThrowProbe entity");
    let route = entity
        .routes
        .iter()
        .find(|r| r.name == "bail")
        .expect("bail route");
    assert!(
        route_can_fail_evm_lean(entity, route),
        "T-ARCH-023: route with bare throw must be fail-mode (H-AX-03-05)"
    );
    let files = transpile_lean(&lean_yaml).expect("lean transpile");
    let routes = files
        .values()
        .find(|content| content.contains("def bail"))
        .expect("Lean route emission for bail");
    assert!(
        !routes.contains(SKIPPED_THROW_MARKER),
        "T-ARCH-023: throw route must not emit skipped-throw comment (H-AX-03-05):\n{routes}"
    );
    assert!(
        routes.contains("throw (Cambrian.ThrowCode.ofNat 42)"),
        "T-ARCH-023: throw route must emit ThrowCode (H-AX-03-05):\n{routes}"
    );
}

#[test]
fn arch_ax03_05_wave2_lean_emissions_never_skip_throw() {
    for entry in std::fs::read_dir(fixtures_dir()).expect("read arch_wave2 fixtures") {
        let path = entry.expect("dir entry").path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with("_lean.yaml") {
            continue;
        }
        let files = transpile_lean(&path).unwrap_or_else(|e| {
            panic!("T-ARCH-023: lean transpile {name} (H-AX-03-05): {e}")
        });
        for (rel, content) in &files {
            if !rel.ends_with(".lean") {
                continue;
            }
            assert!(
                !content.contains(SKIPPED_THROW_MARKER),
                "T-ARCH-023: wave-2 Lean emission must not skip throw (H-AX-03-05): {name} → {rel}"
            );
        }
    }
}

fn arch_ax08_03_cam_src() -> String {
    fs::read_to_string(fixtures_dir().join("ax08_03_multi_from_throw.cam"))
        .expect("read ax08_03_multi_from_throw.cam")
}

#[test]
fn arch_ax08_03_validator_rejects_mixed_from_throw_codes() {
    assert!(
        validate_has_error(&arch_ax08_03_cam_src(), "V60"),
        "T-ARCH-025: multi-from with distinct `: throw N` must fail V60 (H-AX-08-03)"
    );
}

#[test]
fn arch_ax08_03_project_load_rejects_mixed_from_throw_codes() {
    for yaml_name in [
        "ax08_03_multi_from_throw_evm.yaml",
        "ax08_03_multi_from_throw_lean.yaml",
    ] {
        let project = Project::load(&yaml(yaml_name)).expect("load project");
        let has_v60 = validate(&project.merged)
            .into_iter()
            .any(|d| d.severity == Severity::Error && d.code == "V60");
        assert!(
            has_v60,
            "T-ARCH-025: {} must fail V60 on mixed from throw codes (H-AX-08-03)",
            yaml_name
        );
    }
}

const AX01_06_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.RawPayer
import Cambrian.Generated.RawPayerRoutes
import Cambrian.Generated.ValueVault
import Cambrian.Generated.World

#eval Id.run do
  let vault_inst : ValueVault.Identity := {}
  let payer_inst : RawPayer.Identity := {}
  let w0 := Cambrian.Generated.World.withValueVault
    (Cambrian.Generated.World.withRawPayer
      Cambrian.Generated.World.default
      payer_inst RawPayer.State.default)
    vault_inst ValueVault.State.default
  let payerAddr := RawPayer.address payer_inst
  let w0 := { w0 with balances := fun a => if a = payerAddr then (100 : BitVec 256) else 0 }
  let ctx := Cambrian.MsgCtx.default
  match RawPayer.Routes.pay w0 payer_inst ctx (5 : BitVec 128) with
  | Except.error _ => pure false
  | Except.ok w1 => pure ((w1.storage.valueVault vault_inst).m_deposits == 5)
"#;

#[test]
fn arch_ax01_06_evm_forge_raw_transfer_runs_receive_transform() {
    if !has_forge() {
        eprintln!("skip T-ARCH-026 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax01_06_raw_transfer_receive_evm.yaml"),
        "Ax01RawTransferReceive.t.sol",
        "test_ARCH_AX01_06_rawTransferCreditsReceiveTransform",
        "T-ARCH-026",
    );
    assert!(
        ok,
        "T-ARCH-026 EVM: raw transfer must credit receive transform (H-AX-01-06):\n{log}"
    );
}

#[test]
fn arch_ax01_06_lean_raw_transfer_runs_receive_transform() {
    let (ok, detail) = eval_lean(
        &yaml("ax01_06_raw_transfer_receive_lean.yaml"),
        AX01_06_LEAN_EVAL,
        "T-ARCH-026",
    );
    assert!(
        ok,
        "T-ARCH-026 Lean: raw value transfer must credit receive transform (H-AX-01-06):\n{detail}"
    );
}

#[test]
fn arch_ax01_06_emission_lean_value_call_dispatches_receive() {
    let lean = transpile_lean(&yaml("ax01_06_raw_transfer_receive_lean.yaml")).expect("lean transpile");
    let routes = lean
        .get("Cambrian/Generated/RawPayerRoutes.lean")
        .expect("RawPayerRoutes.lean");
    let pay = routes
        .split("def pay")
        .nth(1)
        .unwrap_or(routes.as_str());
    assert!(
        pay.contains("Cambrian.WorldState.call"),
        "T-ARCH-026 Lean: raw value transfer must use WorldState.call (H-AX-01-06):\n{pay}"
    );
    assert!(
        pay.contains("ValueVault.Routes.receive"),
        "T-ARCH-026 Lean: raw value transfer must dispatch receive route (H-AX-01-06):\n{pay}"
    );
}

const AX04_05_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.ExclSenderProbe
import Cambrian.Generated.ExclSenderProbeRoutes
import Cambrian.Generated.World
import Cambrian.Generated.ExclSenderProbeSpec

#eval Id.run do
  let inst : ExclSenderProbe.Identity := {}
  let w0 := Cambrian.Generated.World.withExclSenderProbe
    Cambrian.Generated.World.default inst ({ ExclSenderProbe.State.default with m_excluded_hits := 0 })
  let ctx := Cambrian.MsgCtx.default
  match ExclSenderProbe.Spec.Invariants.excluded_never_hits.step w0 inst ctx (.touchExcluded 1) with
  | .error _ => pure ((w0.storage.exclSenderProbe inst).m_excluded_hits == 0)
  | .ok w1 => pure ((Cambrian.Generated.World.exclSenderProbe w1 inst).m_excluded_hits == 0)
"#;

#[test]
fn arch_ax04_05_evm_forge_invariant_passes_with_exclude_senders() {
    if !has_forge() {
        eprintln!("skip T-ARCH-027 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge_transpiled(
        &yaml("ax04_05_exclude_senders_evm.yaml"),
        "invariant_excluded_never_hits",
        "T-ARCH-027",
    );
    assert!(
        ok,
        "T-ARCH-027 EVM: excludeSender must keep invariant green (H-AX-04-05):\n{log}"
    );
}

#[test]
fn arch_ax04_05_emission_evm_exclude_vs_lean_filtered_senders() {
    let evm = transpile_evm(&yaml("ax04_05_exclude_senders_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax04_05_exclude_senders_lean.yaml")).expect("lean transpile");
    let handler = evm
        .iter()
        .find(|(p, _)| p.contains("Invariant_ExclSenderProbe"))
        .map(|(_, c)| c.as_str())
        .expect("invariant handler sol");
    let spec = lean
        .get("Cambrian/Generated/ExclSenderProbeSpec.lean")
        .expect("ExclSenderProbeSpec.lean");
    assert!(
        handler.contains("excludeSender(EXCLUDED)"),
        "T-ARCH-027 EVM handler must wire excludeSender (H-AX-04-05):\n{handler}"
    );
    assert!(
        spec.contains("[ExclSenderProbe.ALLOWED]")
            && !spec.contains("ExclSenderProbe.EXCLUDED"),
        "T-ARCH-027 Lean must subtract excluded senders from pool (H-AX-04-05):\n{spec}"
    );
}

#[test]
fn arch_ax04_05_lean_step_skips_excluded_sender() {
    let (ok, detail) = eval_lean(
        &yaml("ax04_05_exclude_senders_lean.yaml"),
        AX04_05_LEAN_EVAL,
        "T-ARCH-027",
    );
    assert!(
        ok,
        "T-ARCH-027 Lean: step must not credit excluded sender (H-AX-04-05):\n{detail}"
    );
}

const AX04_06_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.MintProbe
import Cambrian.Generated.MintProbeRoutes
import Cambrian.Generated.World
import Cambrian.Generated.MintProbeSpec

#eval Id.run do
  let inst : MintProbe.Identity := {}
  let w0 := Cambrian.Generated.World.withMintProbe
    Cambrian.Generated.World.default inst ({ MintProbe.State.default with m_total := 50#128 })
  let ctx := Cambrian.MsgCtx.default
  let w1 := MintProbe.Spec.Invariants.pinned_total_stable.step w0 inst ctx .constructor
  pure ((Cambrian.Generated.World.mintProbe w1 inst).m_total == 50#128)
"#;

#[test]
fn arch_ax04_06_evm_forge_invariant_passes_with_ctor_action_noop() {
    if !has_forge() {
        eprintln!("skip T-ARCH-028 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge_transpiled(
        &yaml("ax04_06_invariant_ctor_action_evm.yaml"),
        "invariant_pinned_total_stable",
        "T-ARCH-028",
    );
    assert!(
        ok,
        "T-ARCH-028 EVM: init-route invariant action must be no-op (H-AX-04-06):\n{log}"
    );
}

#[test]
fn arch_ax04_06_emission_evm_noop_vs_lean_constructor_step_noop() {
    let evm = transpile_evm(&yaml("ax04_06_invariant_ctor_action_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax04_06_invariant_ctor_action_lean.yaml")).expect("lean transpile");
    let handler = evm
        .iter()
        .find(|(p, _)| p.contains("Invariant_MintProbe"))
        .map(|(_, c)| c.as_str())
        .expect("invariant handler sol");
    let spec = lean
        .get("Cambrian/Generated/MintProbeSpec.lean")
        .expect("MintProbeSpec.lean");
    let handler_only = handler
        .split("contract MintProbeHandler")
        .nth(1)
        .and_then(|s| s.split("contract Invariant").next())
        .unwrap_or(handler);
    assert!(
        handler_only.contains("init route: entity already constructed"),
        "T-ARCH-028 EVM handler must skip init-route forward call (H-AX-04-06):\n{handler_only}"
    );
    assert!(
        !handler_only.contains("_mintProbe."),
        "T-ARCH-028 EVM handler must not call entity from cam_init_constructor (H-AX-04-06):\n{handler_only}"
    );
    assert!(
        !spec.contains("MintProbe.Routes.constructor w inst ctx"),
        "T-ARCH-028 Lean step must no-op init-route action (H-AX-04-06):\n{spec}"
    );
}

#[test]
fn arch_ax04_06_lean_step_constructor_preserves_pinned_total() {
    let (ok, detail) = eval_lean(
        &yaml("ax04_06_invariant_ctor_action_lean.yaml"),
        AX04_06_LEAN_EVAL,
        "T-ARCH-028",
    );
    assert!(
        ok,
        "T-ARCH-028 Lean: constructor step must preserve pinned init total (H-AX-04-06):\n{detail}"
    );
}

const AX04_07_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.OwnerProbe
import Cambrian.Generated.OwnerProbeRoutes
import Cambrian.Generated.World

#eval Id.run do
  let inst : OwnerProbe.Identity := {}
  let w0 := Cambrian.Generated.World.withOwnerProbe
    Cambrian.Generated.World.default inst OwnerProbe.State.default
  let ctx := { Cambrian.MsgCtx.default with sender := OwnerProbe.ALICE }
  let w1 := OwnerProbe.Routes.constructor w0 inst ctx OwnerProbe.ALICE
  let (_, owner) := OwnerProbe.Routes.getOwner w1 inst ctx
  pure (owner == OwnerProbe.ALICE)
"#;

#[test]
fn arch_ax04_07_evm_forge_fusion_owner_is_test_contract() {
    if !has_forge() {
        eprintln!("skip T-ARCH-029 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax04_07_ctor_fusion_owner_evm.yaml"),
        "Ax04CtorFusionOwner.t.sol",
        "test_ARCH_AX04_07_fusionInitializeSetsOwnerToTestContract",
        "T-ARCH-029",
    );
    assert!(
        ok,
        "T-ARCH-029 EVM: det fusion initialize must set owner to test contract (H-AX-04-07):\n{log}"
    );
}

#[test]
fn arch_ax04_07_evm_transpiled_test_fails_cambrian_expect() {
    if !has_forge() {
        eprintln!("skip T-ARCH-029 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge_transpiled(
        &yaml("ax04_07_ctor_fusion_owner_evm.yaml"),
        "test_msg_ctor_fusion_owner",
        "T-ARCH-029",
    );
    assert!(
        !ok,
        "T-ARCH-029 EVM: Cambrian expect return ALICE must fail under det fusion (H-AX-04-07):\n{log}"
    );
}

#[test]
fn arch_ax04_07_emission_det_fusion_initialize_before_prank() {
    let evm = transpile_evm(&yaml("ax04_07_ctor_fusion_owner_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax04_07_ctor_fusion_owner_lean.yaml")).expect("lean transpile");
    let test_sol = evm
        .iter()
        .find(|(p, _)| p.ends_with("OwnerProbe.t.sol"))
        .map(|(_, c)| c.as_str())
        .expect("OwnerProbe.t.sol");
    let spec = lean
        .get("Cambrian/Generated/OwnerProbeSpec.lean")
        .expect("OwnerProbeSpec.lean");
    assert!(
        test_sol.contains("deployOwnerProbe("),
        "T-ARCH-029 EVM: det harness must redeploy via factory.deployOwnerProbe (H-AX-04-07):\n{test_sol}"
    );
    let deploy_pos = test_sol.find("deployOwnerProbe(").unwrap_or(usize::MAX);
    let prank_pos = test_sol.find("vm.startPrank(").unwrap_or(usize::MAX);
    assert!(
        deploy_pos < prank_pos && deploy_pos != usize::MAX && prank_pos != usize::MAX,
        "T-ARCH-029 EVM: factory deploy must precede startPrank in test body (H-AX-04-07):\n{test_sol}"
    );
    assert!(
        spec.contains("ctx with sender := OwnerProbe.ALICE")
            && spec.contains("OwnerProbe.Routes.constructor w inst ctx OwnerProbe.ALICE"),
        "T-ARCH-029 Lean: spec must pin sender before constructor call (H-AX-04-07):\n{spec}"
    );
}

#[test]
fn arch_ax04_07_lean_constructor_honors_msg_sender() {
    let (ok, detail) = eval_lean(
        &yaml("ax04_07_ctor_fusion_owner_lean.yaml"),
        AX04_07_LEAN_EVAL,
        "T-ARCH-029",
    );
    assert!(
        ok,
        "T-ARCH-029 Lean: constructor must honor ctx.sender for m_owner (H-AX-04-07):\n{detail}"
    );
}

const AX04_08_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.Host
import Cambrian.Generated.Peer
import Cambrian.Generated.PeerRoutes
import Cambrian.Generated.World

#eval Id.run do
  let peer_inst : Peer.Identity := {}
  let host_inst : Host.Identity := {}
  let w0 := Cambrian.Generated.World.withPeer
    (Cambrian.Generated.World.withHost Cambrian.Generated.World.default host_inst Host.State.default)
    peer_inst Peer.State.default
  let ctx := Cambrian.MsgCtx.default
  let w1 := Peer.Routes.bump w0 peer_inst ctx
  let (_, n_spec) := Peer.Routes.getBump w1 peer_inst ctx
  pure (n_spec == 1)
"#;

#[test]
fn arch_ax04_08_evm_forge_transpiled_peer_bump_visible() {
    if !has_forge() {
        eprintln!("skip T-ARCH-030 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge_transpiled(
        &yaml("ax04_08_peer_steps_evm.yaml"),
        "test_peer_bump_visible",
        "T-ARCH-030",
    );
    assert!(
        ok,
        "T-ARCH-030 EVM: deploy peer + bump + getBump must return 1 (H-AX-04-08):\n{log}"
    );
}

#[test]
fn arch_ax04_08_emission_lean_lowers_peer_deploy_and_calls() {
    let evm = transpile_evm(&yaml("ax04_08_peer_steps_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax04_08_peer_steps_lean.yaml")).expect("lean transpile");
    let forge_test = evm
        .iter()
        .find(|(p, _)| p.ends_with("Host.t.sol"))
        .map(|(_, c)| c.as_str())
        .expect("Host.t.sol");
    let spec = lean
        .get("Cambrian/Generated/HostSpec.lean")
        .expect("HostSpec.lean");
    assert!(
        forge_test.contains("deployPeer()")
            && forge_test.contains(".bump()")
            && forge_test.contains("getBump()")
            && forge_test.contains("assertEq(_ret_1, 1"),
        "T-ARCH-030 EVM harness must run full peer deploy/call sequence (H-AX-04-08):\n{forge_test}"
    );
    assert!(
        spec.contains("let peer_inst : Peer.Identity := {}")
            && spec.contains("World.withPeer w peer_inst")
            && spec.contains("Peer.Routes.bump w peer_inst ctx")
            && spec.contains("Peer.Routes.getBump w peer_inst ctx"),
        "T-ARCH-030 Lean spec must lower deploy peer + qualified calls (H-AX-04-08):\n{spec}"
    );
    assert!(
        spec.contains("__e0 := _result_0 = 1")
            && !spec.contains("needs a second contract"),
        "T-ARCH-030 Lean spec must prove getBump == 1, not vacuous True (H-AX-04-08):\n{spec}"
    );
}

#[test]
fn arch_ax04_08_lean_spec_path_matches_evm_aligned_bump() {
    let (ok, detail) = eval_lean(
        &yaml("ax04_08_peer_steps_lean.yaml"),
        AX04_08_LEAN_EVAL,
        "T-ARCH-030",
    );
    assert!(
        ok,
        "T-ARCH-030 Lean: peer bump + getBump must return 1 like EVM (H-AX-04-08):\n{detail}"
    );
}

const AX01_05_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.SelfReceiver
import Cambrian.Generated.SelfReceiverRoutes
import Cambrian.Generated.World

#eval Id.run do
  let inst : SelfReceiver.Identity := {}
  let w0 := Cambrian.Generated.World.withSelfReceiver
    Cambrian.Generated.World.default inst SelfReceiver.State.default
  let addr := SelfReceiver.address inst
  let w0 := { w0 with balances := fun a => if a = addr then (100 : BitVec 256) else 0 }
  let ctx := Cambrian.MsgCtx.default
  match SelfReceiver.Routes.paySelf w0 inst ctx (5 : BitVec 128) with
  | Except.error _ => pure false
  | Except.ok w1 => pure ((w1.storage.selfReceiver inst).m_deposits == 5)
"#;

#[test]
fn arch_ax01_05_evm_forge_self_transfer_runs_receive() {
    if !has_forge() {
        eprintln!("skip T-ARCH-031 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax01_05_self_transfer_evm.yaml"),
        "Ax01SelfTransfer.t.sol",
        "test_ARCH_AX01_05_selfTransferRunsReceiveTransform",
        "T-ARCH-031",
    );
    assert!(
        ok,
        "T-ARCH-031 EVM: self-transfer must run receive transform (H-AX-01-05):\n{log}"
    );
}

#[test]
fn arch_ax01_05_emission_lean_self_transfer_dispatches_receive() {
    let evm = transpile_evm(&yaml("ax01_05_self_transfer_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax01_05_self_transfer_lean.yaml")).expect("lean transpile");
    let project = evm
        .get("src/_arch-ax01-05-evm_project.sol")
        .expect("evm project sol");
    let routes = lean
        .get("Cambrian/Generated/SelfReceiverRoutes.lean")
        .expect("SelfReceiverRoutes.lean");
    let pay = routes
        .split("def paySelf")
        .nth(1)
        .unwrap_or(routes.as_str());
    assert!(
        project.contains("predictSelfReceiver()")
            && project.contains(".call{value: amount}"),
        "T-ARCH-031 EVM: paySelf must call predicted self address with value (H-AX-01-05):\n{project}"
    );
    assert!(
        pay.contains("Cambrian.WorldState.call")
            && pay.contains("SelfReceiver.Routes.receive"),
        "T-ARCH-031 Lean: self value transfer must dispatch receive (H-AX-01-05):\n{pay}"
    );
}

#[test]
fn arch_ax01_05_lean_pay_self_credits_receive_like_evm() {
    let (ok, detail) = eval_lean(
        &yaml("ax01_05_self_transfer_lean.yaml"),
        AX01_05_LEAN_EVAL,
        "T-ARCH-031",
    );
    assert!(
        ok,
        "T-ARCH-031 Lean: paySelf must credit receive transform like EVM (H-AX-01-05):\n{detail}"
    );
}

const AX10_04_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.EmitProbe
import Cambrian.Generated.EmitProbeRoutes
import Cambrian.Generated.World

#eval Id.run do
  let inst : EmitProbe.Identity := {}
  let w0 := Cambrian.Generated.World.withEmitProbe
    Cambrian.Generated.World.default inst EmitProbe.State.default
  let w1 := EmitProbe.Routes.log w0 inst Cambrian.MsgCtx.default 7
  pure (w1.events.length == 1)
"#;

#[test]
fn arch_ax10_04_evm_forge_expect_emit_passes() {
    if !has_forge() {
        eprintln!("skip T-ARCH-032 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge_transpiled(
        &yaml("ax10_04_expect_emit_evm.yaml"),
        "test_expect_logged",
        "T-ARCH-032",
    );
    assert!(
        ok,
        "T-ARCH-032 EVM: expect emit must pass via vm.expectEmit (H-AX-10-04):\n{log}"
    );
}

#[test]
fn arch_ax10_04_emission_evm_expect_emit_vs_lean_events_predicate() {
    let evm = transpile_evm(&yaml("ax10_04_expect_emit_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax10_04_expect_emit_lean.yaml")).expect("lean transpile");
    let forge_test = evm
        .iter()
        .find(|(p, _)| p.ends_with("EmitProbe.t.sol"))
        .map(|(_, c)| c.as_str())
        .expect("EmitProbe.t.sol");
    let spec = lean
        .get("Cambrian/Generated/EmitProbeSpec.lean")
        .expect("EmitProbeSpec.lean");
    assert!(
        forge_test.contains("vm.expectEmit(") && forge_test.contains("emit EmitProbe.Logged(7)"),
        "T-ARCH-032 EVM: test must lower expect emit to vm.expectEmit (H-AX-10-04):\n{forge_test}"
    );
    assert!(
        spec.contains("w.events.getLast?")
            && spec.contains("EmitProbe_Logged")
            && !spec.contains("observability deferred on Lean target"),
        "T-ARCH-032 Lean: expect emit must assert on w.events (H-AX-10-04):\n{spec}"
    );
}

#[test]
fn arch_ax10_04_lean_spec_records_emit_in_event_log() {
    let (ok, detail) = eval_lean(
        &yaml("ax10_04_expect_emit_lean.yaml"),
        AX10_04_LEAN_EVAL,
        "T-ARCH-032",
    );
    assert!(
        ok,
        "T-ARCH-032 Lean: log route must append to w.events (H-AX-10-04):\n{detail}"
    );
}

const AX10_02_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.CtxProbe
import Cambrian.Generated.CtxProbeSpec

#eval Id.run do
  let senders := CtxProbe.Spec.Invariants.overlay_deploy_ctx.senders
  pure (senders.length == 1 && senders.head? == some CtxProbe.FUZZ)
"#;

#[test]
fn arch_ax10_02_evm_forge_invariant_overlay_passes() {
    if !has_forge() {
        eprintln!("skip T-ARCH-033 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge_transpiled(
        &yaml("ax10_02_invariant_overlay_evm.yaml"),
        "invariant_overlay_deploy_ctx",
        "T-ARCH-033",
    );
    assert!(
        ok,
        "T-ARCH-033 EVM: merged overlay invariant must pass under Foundry (H-AX-10-02):\n{log}"
    );
}

#[test]
fn arch_ax10_02_emission_evm_target_sender_excludes_overlay_deploy_ctx() {
    let evm = transpile_evm(&yaml("ax10_02_invariant_overlay_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax10_02_invariant_overlay_lean.yaml")).expect("lean transpile");
    let handler = evm
        .iter()
        .find(|(p, _)| p.contains("Invariant_CtxProbe_overlay_deploy_ctx"))
        .map(|(_, c)| c.as_str())
        .expect("overlay invariant handler sol");
    let spec = lean
        .get("Cambrian/Generated/CtxProbeSpec.lean")
        .expect("CtxProbeSpec.lean");
    assert!(
        handler.contains("targetSender(FUZZ)")
            && handler.contains("deployCtxProbe()")
            && !handler.contains("targetSender(DEPLOY)"),
        "T-ARCH-033 EVM: overlay deploy ctx must not widen targetSender (H-AX-10-02):\n{handler}"
    );
    assert!(
        spec.contains("def senders : List Cambrian.Address :=\n  [CtxProbe.FUZZ]")
            && spec.contains("sender := CtxProbe.DEPLOY")
            && !spec.contains("targetSender"),
        "T-ARCH-033 Lean: ctor ctx uses DEPLOY but step senders stay catalog-only (H-AX-10-02):\n{spec}"
    );
}

#[test]
fn arch_ax10_02_lean_step_senders_exclude_overlay_ctx_deployer() {
    let (ok, detail) = eval_lean(
        &yaml("ax10_02_invariant_overlay_lean.yaml"),
        AX10_02_LEAN_EVAL,
        "T-ARCH-033",
    );
    assert!(
        ok,
        "T-ARCH-033 Lean: overlay deploy ctx must not widen authorised senders (H-AX-10-02):\n{detail}"
    );
}

#[test]
fn arch_ax10_03_emission_evm_factory_guard_vs_lean_create2_stub() {
    let evm = transpile_evm(&yaml("ax10_03_ch_factory_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax10_03_ch_factory_lean.yaml")).expect("lean transpile");
    let project = evm
        .iter()
        .find(|(p, _)| p.contains("_arch-ax10-03-evm_project.sol"))
        .map(|(_, c)| c.as_str())
        .expect("combined evm project sol");
    let entity = lean
        .get("Cambrian/Generated/DetProbe.lean")
        .expect("DetProbe.lean");
    assert!(
        project.contains("require(factory_ != address(0), \"zero factory\")")
            && project.contains("constructor(address factory_)"),
        "T-ARCH-034 EVM: deterministic mode must emit CH factory zero guard (H-AX-10-03):\n{project}"
    );
    assert!(
        entity.contains("Cambrian.create2Address deployer")
            && !entity.contains("zero factory")
            && !entity.contains("factory_"),
        "T-ARCH-034 Lean: address model uses create2Address without factory guard (H-AX-10-03):\n{entity}"
    );
}

const AX02_02_LEAN_EVAL: &str = r#"
import Cambrian.Prelude
import Cambrian.Generated.Vault
import Cambrian.Generated.VaultRoutes
import Cambrian.Generated.World

#eval Id.run do
  let inst : Vault.Identity := { m_id := 1 }
  let vaultAddr := Vault.address inst
  let sender := (0x0000000000000000000000000000000000000001 : Cambrian.Address)
  let w0 := Cambrian.Generated.World.withVault
    Cambrian.Generated.World.default inst ({ Vault.State.default with m_bal := 5 })
  let w0 := { w0 with balances := fun a => if a = vaultAddr then (100 : BitVec 256) else 0 }
  let ctx : Cambrian.MsgCtx := { Cambrian.MsgCtx.default with sender := sender }
  match Vault.Routes.withdraw w0 inst ctx 1 with
  | Except.ok w1 => pure ((w1.storage.vault inst).m_bal == 4)
  | Except.error _ => pure false
"#;

#[test]
fn arch_ax02_01_evm_forge_phased_bump_passes() {
    if !has_forge() {
        eprintln!("skip T-ARCH-036 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge_transpiled(
        &yaml("ax02_01_phased_temporal_evm.yaml"),
        "test_increment_once",
        "T-ARCH-036",
    );
    assert!(
        ok,
        "T-ARCH-036 EVM: phased bump must pass on non-reentrant path (H-AX-02-01):\n{log}"
    );
}

#[test]
fn arch_ax02_01_emission_evm_phased_blocks_vs_lean_atomic_world_step() {
    let evm = transpile_evm(&yaml("ax02_01_phased_temporal_evm.yaml")).expect("evm transpile");
    let lean = transpile_lean(&yaml("ax02_01_phased_temporal_lean.yaml")).expect("lean transpile");
    let project = evm
        .iter()
        .find(|(p, _)| p.contains("_arch-ax02-01-evm_project.sol"))
        .map(|(_, c)| c.as_str())
        .expect("combined evm project sol");
    let routes = lean
        .get("Cambrian/Generated/SnapProbeRoutes.lean")
        .expect("SnapProbeRoutes.lean");
    assert!(
        project.contains("// Phase: prep") && project.contains("// Phase: inc"),
        "T-ARCH-036 EVM: phased route must emit explicit phase blocks (H-AX-02-01):\n{project}"
    );
    assert!(
        routes.contains("def bump_prep")
            && routes.contains("def bump_inc")
            && routes.contains("SnapProbe.Local.bump_prep")
            && routes.contains("SnapProbe.Local.bump_inc")
            && !routes.contains("external"),
        "T-ARCH-036 Lean: phases chain inside one Routes.bump World step (H-AX-02-01):\n{routes}"
    );
}

#[test]
fn arch_ax02_02_evm_forge_phased_reentrancy_drains() {
    if !has_forge() {
        eprintln!("skip T-ARCH-035 forge: forge not on PATH");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("ax02_02_phased_reentrancy_evm.yaml"),
        "Ax02PhasedReentrancy.t.sol",
        "test_arch_ax02_02_phasedReentrancyDrains",
        "T-ARCH-035",
    );
    assert!(
        ok,
        "T-ARCH-035 EVM: phased pull/settle must allow reentrancy drain (H-AX-02-02 / PN-101 class):\n{log}"
    );
}

#[test]
fn arch_ax02_02_lean_withdraw_is_atomic_honest_debit() {
    let (ok, detail) = eval_lean(
        &yaml("ax02_02_phased_reentrancy_lean.yaml"),
        AX02_02_LEAN_EVAL,
        "T-ARCH-035",
    );
    assert!(
        ok,
        "T-ARCH-035 Lean: single withdraw step debits once without reentrancy (H-AX-02-02):\n{detail}"
    );
}

#[test]
fn arch_ax09_01_active_ctx_sequential_gen_isolated() {
    let testgen_src = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/codegen/evm_test_codegen.rs"),
    )
    .expect("read evm_test_codegen.rs");
    assert!(
        testgen_src.contains("reset_active_ctx("),
        "T-ARCH-037 setup: generate_evm_tests must reset ACTIVE_CTX (H-AX-09-01):\n{testgen_src}"
    );

    let b_alone = evm_test_sol(&yaml("ax09_01_active_ctx_b_evm.yaml"));
    assert!(
        uses_det_predict_markers(&b_alone),
        "T-ARCH-037 setup: det program B must emit predict markers (U4-6 mandatory det) (H-AX-09-01):\n{b_alone}"
    );

    let a_sol = evm_test_sol(&yaml("ax09_01_active_ctx_a_evm.yaml"));
    assert!(
        uses_det_predict_markers(&a_sol),
        "T-ARCH-037 setup: det program A must emit predict markers (H-AX-09-01):\n{a_sol}"
    );

    let _ = transpile_evm(&yaml("ax09_01_active_ctx_a_evm.yaml")).expect("prime det transpile");
    let b_after_a = evm_test_sol(&yaml("ax09_01_active_ctx_b_evm.yaml"));

    assert_eq!(
        b_alone, b_after_a,
        "T-ARCH-037 REFUTED/ALIGNED: sequential gen must not leak ACTIVE_CTX into later output (H-AX-09-01)"
    );
}
