// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase N Wave-3 — Lean dispatch / value-transfer matrix (PW3-S-008).
//!
//! Dynamic `Dispatch.*` is `Except` fail-mode; value transfer never uses `getD` (PW3-G-004).
//!
//!   cargo test -p cambrian-transpiler --test test_audit_dispatch_matrix -- --nocapture
//!   CAMBRIAN_TEST_LEAN_BUILD=1 cargo test -p cambrian-transpiler --test test_audit_dispatch_matrix -- --nocapture

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
    audit_root().join("fixtures/pw3_dispatch")
}

fn yaml(name: &str) -> PathBuf {
    fixtures_dir().join(name)
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-dispatch-{}-{}-{}",
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

fn lake_build_lean(yaml_rel: &Path, tag: &str) -> (bool, String) {
    if !lean_build_enabled() {
        return (true, "skip lake build (set CAMBRIAN_TEST_LEAN_BUILD=1)".into());
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
    let lake = Command::new("lake")
        .arg("build")
        .current_dir(&out_dir)
        .output()
        .expect("lake build");
    let combined = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&lake.stdout),
        String::from_utf8_lossy(&lake.stderr)
    );
    let ok = lake.status.success();
    let _ = std::fs::remove_dir_all(&out_dir);
    (ok, combined)
}

fn underfund_raw_lean_exec() -> LeanExec {
    LeanExec {
        expected: "false",
        eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.Wallet
import Cambrian.Generated.World
import Cambrian.Generated.WalletRoutes

#eval Id.run do
  let inst : Wallet.Identity := {}
  let w0 := Cambrian.Generated.World.withWallet
    Cambrian.Generated.World.default inst
    ({ Wallet.State.default with m_balance := 100 })
  let w := { w0 with balances := fun _ => 0#256 }
  let ctx := Cambrian.MsgCtx.default
  let recip : Cambrian.Address := 0x1#160
  pure (Wallet.Routes.sendTooMuch w inst ctx recip).isOk
"#,
    }
}

fn underfund_dynamic_lean_exec() -> LeanExec {
    LeanExec {
        expected: "false",
        eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.Payer
import Cambrian.Generated.Payee
import Cambrian.Generated.World
import Cambrian.Generated.PayerRoutes

#eval Id.run do
  let payee_inst : Payee.Identity := { m_id := 2 }
  let payer_inst : Payer.Identity := { m_id := 1 }
  let dest := Payee.address payee_inst
  let w0 := Cambrian.Generated.World.withPayee
    (Cambrian.Generated.World.withPayer Cambrian.Generated.World.default payer_inst ({ Payer.State.default with m_dest := dest }))
    payee_inst Payee.State.default
  let w0 := { w0 with balances := fun _ => 0 }
  let amt : BitVec 128 := 1000
  pure (Payer.Routes.pay w0 payer_inst Cambrian.MsgCtx.default amt).isOk
"#,
    }
}

fn fail_callee_lean_exec_call_fail() -> LeanExec {
    LeanExec {
        expected: "false",
        eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.Payer
import Cambrian.Generated.Payee
import Cambrian.Generated.World
import Cambrian.Generated.PayerRoutes

#eval Id.run do
  let payee_inst : Payee.Identity := { m_id := 2 }
  let payer_inst : Payer.Identity := { m_id := 1 }
  let dest := Payee.address payee_inst
  let w0 := Cambrian.Generated.World.withPayee
    (Cambrian.Generated.World.withPayer Cambrian.Generated.World.default payer_inst ({ Payer.State.default with m_dest := dest }))
    payee_inst Payee.State.default
  pure (Payer.Routes.callFail w0 payer_inst Cambrian.MsgCtx.default).isOk
"#,
    }
}

fn fail_callee_lean_exec_pay_zero() -> LeanExec {
    LeanExec {
        expected: "false",
        eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.Payer
import Cambrian.Generated.Payee
import Cambrian.Generated.World
import Cambrian.Generated.PayerRoutes

#eval Id.run do
  let payee_inst : Payee.Identity := { m_id := 2 }
  let payer_inst : Payer.Identity := { m_id := 1 }
  let dest := Payee.address payee_inst
  let w0 := Cambrian.Generated.World.withPayee
    (Cambrian.Generated.World.withPayer Cambrian.Generated.World.default payer_inst ({ Payer.State.default with m_dest := dest }))
    payee_inst Payee.State.default
  pure (Payer.Routes.payZero w0 payer_inst Cambrian.MsgCtx.default).isOk
"#,
    }
}

#[test]
fn pw3_dispatch_smoke_fixtures_parse() {
    for entry in std::fs::read_dir(fixtures_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "cam") {
            let src = std::fs::read_to_string(&path).unwrap();
            assert!(!src.is_empty(), "empty fixture {}", path.display());
        }
    }
}

#[test]
fn pw3_g004_underfund_raw_evm_forge_reverts() {
    if !has_forge() {
        eprintln!("skip PW3-G-004 underfund raw forge");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("underfund_raw_evm.yaml"),
        "Pw3DispatchUnderfundRaw.t.sol",
        "test_PW3_G004_underfundRawTransferReverts",
        "PW3-G-004-raw-evm",
    );
    assert!(ok, "PW3-G-004 row1 EVM: raw underfund must revert:\n{log}");
}

#[test]
fn pw3_g004_underfund_raw_lean_propagates_transfer_failure() {
    let files = transpile_lean(&yaml("underfund_raw_lean.yaml")).expect("lean transpile");
    let routes = files
        .get("Cambrian/Generated/WalletRoutes.lean")
        .expect("WalletRoutes");
    assert!(
        routes.contains("RouteResult")
            && routes.contains("WorldState.transfer")
            && routes.contains("throw (Cambrian.ThrowCode.ofNat 90)")
            && !routes.contains("toOption.getD"),
        "PW3-G-004 row1 Lean baseline: raw transfer must fail-propagate (T-X-002):\n{routes}"
    );
    let (ok, detail) = eval_lean(
        &yaml("underfund_raw_lean.yaml"),
        &underfund_raw_lean_exec(),
        "PW3-G-004-raw-lean",
    );
    assert!(
        ok,
        "PW3-G-004 row1 Lean #eval: underfund raw sendTooMuch.isOk must be false:\n{detail}"
    );
}

#[test]
fn pw3_g004_underfund_dynamic_evm_forge_reverts() {
    if !has_forge() {
        eprintln!("skip PW3-G-004 underfund dynamic forge");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("underfund_dynamic_evm.yaml"),
        "Pw3DispatchUnderfundDynamic.t.sol",
        "test_PW3_G004_underfundDynamicDispatchReverts",
        "PW3-G-004-dyn-evm",
    );
    assert!(ok, "PW3-G-004 row2 EVM: underfund dynamic dispatch must revert:\n{log}");
}

#[test]
fn pw3_g004_underfund_dynamic_lean_codegen_fail_closes() {
    let files = transpile_lean(&yaml("underfund_dynamic_lean.yaml")).expect("lean transpile");
    let routes = files
        .get("Cambrian/Generated/PayerRoutes.lean")
        .expect("PayerRoutes");
    let dispatch = files
        .get("Cambrian/Generated/Dispatch.lean")
        .expect("Dispatch");
    assert!(
        routes.contains("RouteResult")
            && routes.contains("WorldState.transfer")
            && !routes.contains("toOption.getD")
            && routes.contains("let w ←"),
        "PW3-G-004 row2 Lean codegen: dynamic pay must fail-close (no getD):\n{routes}"
    );
    assert!(
        (dispatch.contains("opaque deposit") || dispatch.contains("def deposit"))
            && dispatch.contains("Except Cambrian.ThrowCode"),
        "PW3-G-004 Dispatch module must emit Except wrappers:\n{dispatch}"
    );
}

#[test]
fn pw3_g004_underfund_dynamic_lean_should_fail_closed() {
    let (ok, detail) = eval_lean(
        &yaml("underfund_dynamic_lean.yaml"),
        &underfund_dynamic_lean_exec(),
        "PW3-G-004-dyn-lean-red",
    );
    assert!(
        ok,
        "PW3-G-004 red gate: Lean must not complete pay when transfer fails (EVM reverts):\n{detail}"
    );
}

#[test]
fn pw3_g004_fail_callee_evm_forge_reverts() {
    if !has_forge() {
        eprintln!("skip PW3-G-004 fail callee forge");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("dispatch_fail_callee_evm.yaml"),
        "Pw3DispatchFailCallee.t.sol",
        "test_PW3_G004_dynamicDispatchFailCalleeReverts",
        "PW3-G-004-fail-evm",
    );
    assert!(ok, "PW3-G-004 row3 EVM: dynamic dispatch to failing callee must revert:\n{log}");
}

#[test]
fn pw3_g004_fail_callee_lean_should_fail_closed() {
    let files = transpile_lean(&yaml("dispatch_fail_callee_lean.yaml")).expect("lean transpile");
    let dispatch = files
        .get("Cambrian/Generated/Dispatch.lean")
        .expect("Dispatch");
    assert!(
        dispatch.contains("def failAlways") && dispatch.contains("Except Cambrian.ThrowCode"),
        "PW3-G-004 row3 Lean Dispatch must be Except (fail-close):\n{dispatch}"
    );
    let (ok, detail) = eval_lean(
        &yaml("dispatch_fail_callee_lean.yaml"),
        &fail_callee_lean_exec_call_fail(),
        "PW3-G-004-fail-lean-red",
    );
    assert!(
        ok,
        "PW3-G-004 red gate: Lean must not model failing callee dispatch as success:\n{detail}"
    );
}

#[test]
fn pw3_g004_no_value_fail_evm_forge_reverts() {
    if !has_forge() {
        eprintln!("skip PW3-G-004 no-value fail forge");
        return;
    }
    let (ok, log) = run_forge(
        &yaml("dispatch_no_value_fail_evm.yaml"),
        "Pw3DispatchNoValueFail.t.sol",
        "test_PW3_G004_dynamicDispatchZeroValueFailCalleeReverts",
        "PW3-G-004-noval-evm",
    );
    assert!(ok, "PW3-G-004 row4 EVM: zero-value failing dispatch must revert:\n{log}");
}

#[test]
fn pw3_g004_no_value_fail_lean_should_fail_closed() {
    let (ok, detail) = eval_lean(
        &yaml("dispatch_no_value_fail_lean.yaml"),
        &fail_callee_lean_exec_pay_zero(),
        "PW3-G-004-noval-lean-red",
    );
    assert!(
        ok,
        "PW3-G-004 red gate: Lean must not swallow zero-value failing dispatch:\n{detail}"
    );
}

#[test]
fn pw3_g004_send_value_shapes_evm_transpile_smoke() {
    let files = transpile_evm(&yaml("send_value_shapes_evm.yaml")).expect("evm transpile");
    let project = files
        .get("src/_pw3-dispatch-send-value-shapes-evm_project.sol")
        .expect("project sol");
    assert!(
        project.contains("function pay(") && project.contains("bump{value:"),
        "PW3-G-004 row5 EVM: send_value_shapes must emit value-carrying typed send:\n{project}"
    );
}

#[test]
fn pw3_g004_send_value_shapes_lean_codegen_world_state_call() {
    let files = transpile_lean(&yaml("send_value_shapes_lean.yaml")).expect("lean transpile");
    let routes = files
        .get("Cambrian/Generated/PayerRoutes.lean")
        .expect("PayerRoutes");
    assert!(
        routes.contains("Cambrian.WorldState.call")
            && routes.contains("callParams")
            && routes.contains("withValue"),
        "PW3-G-004 row5 Lean: send_value_shapes must lower value-carrying typed call:\n{routes}"
    );
}

#[test]
fn pw3_g004_send_value_shapes_lake_smoke() {
    let (ok, log) = lake_build_lean(
        &yaml("send_value_shapes_full_lean.yaml"),
        "PW3-G-004-svs-lake",
    );
    assert!(
        ok,
        "PW3-G-004 row5 escrow send_value_shapes lake build:\n{log}"
    );
}

#[test]
fn pw3_g004_view_send_value_lake_smoke() {
    let (ok, log) = lake_build_lean(
        &yaml("view_send_value_lean.yaml"),
        "PW3-G-004-vsv-lake",
    );
    assert!(
        ok,
        "PW3-G-004 row6 escrow view_send_value legacy lake build:\n{log}"
    );
}

#[test]
fn pw3_g004_view_send_value_codegen_value_carrying_call() {
    let files = transpile_lean(&yaml("view_send_value_lean.yaml")).expect("lean transpile");
    let routes = files
        .get("Cambrian/Generated/CallerRoutes.lean")
        .expect("CallerRoutes");
    assert!(
        routes.contains("WorldState.call") && routes.contains("withValue"),
        "PW3-G-004 row6 view_send_value must emit value-carrying call lowering:\n{routes}"
    );
}
