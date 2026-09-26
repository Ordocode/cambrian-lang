// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM↔Lean differential audit tests (Phase D seed + Phase N Lean `#eval` oracle).
//!
//! Each case shares one `.cam` source, runs Forge on the EVM lowering
//! (ground truth), inspects the Lean lowering (substring adjunct), and —
//! when `CAMBRIAN_TEST_LEAN_BUILD=1` — executes a closed `#eval` term via
//! `lake env lean Eval.lean`.
//!
//! See [docs/AUDIT_EVM_LEAN.md](../../../docs/AUDIT_EVM_LEAN.md) §6 Phase D
//! and [docs/plans/phase-n-harness.md](../../../docs/plans/phase-n-harness.md).

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
optimizer_runs = 200
via_ir = false
"#;

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

#[allow(dead_code)]
enum LeanDivergenceSignal {
    /// Lean diverges when the route body contains this substring.
    ForbiddenSubstring(&'static str),
    /// Lean diverges when the route body lacks this substring.
    RequiredSubstring(&'static str),
    /// Lean diverges when `needle` appears at least `min` times (silent overwrite).
    MinSubstringCount {
        needle: &'static str,
        min: usize,
    },
}

/// Shared EVM↔Lean semantics for one differential row (PW3-O-012).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedOutcome {
    Bool(bool),
    U64(u64),
}

impl ExpectedOutcome {
    fn lean_stdout_substring(self) -> String {
        match self {
            ExpectedOutcome::Bool(b) => b.to_string(),
            ExpectedOutcome::U64(n) => n.to_string(),
        }
    }

    fn describe(self) -> String {
        match self {
            ExpectedOutcome::Bool(b) => format!("Bool({b})"),
            ExpectedOutcome::U64(n) => format!("U64({n})"),
        }
    }
}

/// Route-body substring oracle (Lean inspect leg); optional when exec-only row.
struct LeanRouteInspect {
    routes_file: &'static str,
    route_name: &'static str,
    signal: LeanDivergenceSignal,
}

/// Closed `#eval` body (dump-first per case). Expected stdout comes from
/// `DifferentialCase::expected_outcome` — not a separate per-leg constant.
struct LeanExec {
    eval_lean: &'static str,
}

struct DifferentialCase {
    id: &'static str,
    lean_hypothesis: &'static str,
    evm_project_yaml: &'static str,
    lean_project_yaml: &'static str,
    forge_test_file: &'static str,
    forge_match_test: &'static str,
    expected_outcome: ExpectedOutcome,
    lean_inspect: Option<LeanRouteInspect>,
    lean_exec: LeanExec,
}

const CASES: &[DifferentialCase] = &[
    DifferentialCase {
        id: "T-X-002",
        lean_hypothesis: "LEAN-H3",
        evm_project_yaml: "x_h2_underfunded_transfer_evm.yaml",
        lean_project_yaml: "x_h2_underfunded_transfer_lean.yaml",
        forge_test_file: "XH2UnderfundedTransfer.t.sol",
        forge_match_test: "test_X002_underfundedRawTransferReverts",
        expected_outcome: ExpectedOutcome::Bool(false),
        lean_inspect: Some(LeanRouteInspect {
            routes_file: "Cambrian/Generated/WalletRoutes.lean",
            route_name: "sendTooMuch",
            signal: LeanDivergenceSignal::ForbiddenSubstring("toOption.getD w"),
        }),
        lean_exec: LeanExec {
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
        },
    },
    DifferentialCase {
        id: "T-X-001",
        lean_hypothesis: "LEAN-H5",
        evm_project_yaml: "x_h1_double_deploy_evm.yaml",
        lean_project_yaml: "x_h1_double_deploy_lean.yaml",
        forge_test_file: "XH1DoubleDeploy.t.sol",
        forge_match_test: "test_X001_secondCreate2DeployAtSameSaltReverts",
        expected_outcome: ExpectedOutcome::Bool(false),
        lean_inspect: Some(LeanRouteInspect {
            routes_file: "Cambrian/Generated/DeployerRoutes.lean",
            route_name: "respawn",
            signal: LeanDivergenceSignal::RequiredSubstring("vault_deployed"),
        }),
        lean_exec: LeanExec {
            eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.Deployer
import Cambrian.Generated.Vault
import Cambrian.Generated.World
import Cambrian.Generated.DeployerRoutes

#eval Id.run do
  let inst : Deployer.Identity := { m_id := 0 }
  let w0 := Cambrian.Generated.World.withDeployer
    Cambrian.Generated.World.default inst Deployer.State.default
  let ctx := Cambrian.MsgCtx.default
  pure (Deployer.Routes.respawn w0 inst ctx 7).isOk
"#,
        },
    },
    DifferentialCase {
        id: "T-X-003",
        lean_hypothesis: "X-H5",
        evm_project_yaml: "x_h3_deploy_init_params_evm.yaml",
        lean_project_yaml: "x_h3_deploy_init_params_lean.yaml",
        forge_test_file: "XH3DeployInitParams.t.sol",
        forge_match_test: "test_X003_deployInitParamsSetsMemberBalance",
        expected_outcome: ExpectedOutcome::U64(42),
        lean_inspect: Some(LeanRouteInspect {
            routes_file: "Cambrian/Generated/DeployerRoutes.lean",
            route_name: "spawn",
            signal: LeanDivergenceSignal::RequiredSubstring("m_balance := initial"),
        }),
        lean_exec: LeanExec {
            eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.Deployer
import Cambrian.Generated.Vault
import Cambrian.Generated.World
import Cambrian.Generated.DeployerRoutes

#eval Id.run do
  let inst : Deployer.Identity := { m_id := 0 }
  let w0 := Cambrian.Generated.World.withDeployer
    Cambrian.Generated.World.default inst Deployer.State.default
  let ctx := Cambrian.MsgCtx.default
  match Deployer.Routes.spawn w0 inst ctx 7 42 with
  | .ok w =>
    let vid : Vault.Identity := { m_id := 7 }
    pure (Cambrian.Generated.World.vault w vid).m_balance
  | .error _ => pure (0 : BitVec 64)
"#,
        },
    },
    DifferentialCase {
        id: "T-X-005",
        lean_hypothesis: "LEAN-H7",
        evm_project_yaml: "x_h5_dynamic_dispatch_value_evm.yaml",
        lean_project_yaml: "x_h5_dynamic_dispatch_value_lean.yaml",
        forge_test_file: "XH5DynamicDispatchValue.t.sol",
        forge_match_test: "test_X005_dynamicDispatchValueDebitsPayerAndCreditsPayee",
        expected_outcome: ExpectedOutcome::U64(950),
        lean_inspect: Some(LeanRouteInspect {
            routes_file: "Cambrian/Generated/PayerRoutes.lean",
            route_name: "pay",
            signal: LeanDivergenceSignal::RequiredSubstring("Cambrian.WorldState.transfer"),
        }),
        lean_exec: LeanExec {
            eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.World

#eval Id.run do
  let src : Cambrian.Address := 0x1#160
  let dst : Cambrian.Address := 0x2#160
  let w0 : Cambrian.Generated.World :=
    { Cambrian.Generated.World.default with
      balances := fun a => if a = src then (1000 : BitVec 256) else 0 }
  match Cambrian.WorldState.transfer w0 src dst 50 with
  | .ok w => pure (w.balances src)
  | .error _ => pure (9999 : BitVec 256)
"#,
        },
    },
    DifferentialCase {
        id: "T-X-006",
        lean_hypothesis: "LEAN-H4",
        evm_project_yaml: "x_h6_typed_send_ctx_value_evm.yaml",
        lean_project_yaml: "lean_h4_typed_send_value.yaml",
        forge_test_file: "XH6TypedSendCtxValue.t.sol",
        forge_match_test: "test_X006_typedSendValueCreditsPayeeSpent",
        expected_outcome: ExpectedOutcome::U64(50),
        lean_inspect: Some(LeanRouteInspect {
            routes_file: "Cambrian/Generated/PayerRoutes.lean",
            route_name: "pay",
            signal: LeanDivergenceSignal::RequiredSubstring("withValue"),
        }),
        lean_exec: LeanExec {
            eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.Payer
import Cambrian.Generated.Payee
import Cambrian.Generated.World
import Cambrian.Generated.PayerRoutes

#eval Id.run do
  let payer : Payer.Identity := { m_id := 1 }
  let payee : Payee.Identity := { m_id := 2 }
  let w0 := Cambrian.Generated.World.withPayer
    Cambrian.Generated.World.default payer Payer.State.default
  let w0 := Cambrian.Generated.World.withPayee w0 payee Payee.State.default
  let payerAddr := Payer.address payer
  let w0 := { w0 with balances := fun a => if a = payerAddr then (1000 : BitVec 256) else 0 }
  let ctx := Cambrian.MsgCtx.default
  match Payer.Routes.pay w0 payer ctx 2 50 with
  | .ok w' => pure (Cambrian.Generated.World.payee w' payee).m_spent
  | .error _ => pure (0 : Nat)
"#,
        },
    },
    DifferentialCase {
        id: "T-X-007",
        lean_hypothesis: "LEAN-H9",
        evm_project_yaml: "x_h7_runtrace_continue_evm.yaml",
        lean_project_yaml: "x_h7_runtrace_continue_lean.yaml",
        forge_test_file: "XH7RuntraceContinue.t.sol",
        forge_match_test: "test_X007_traceContinuesAfterFailRevertThenInc",
        expected_outcome: ExpectedOutcome::U64(11),
        lean_inspect: Some(LeanRouteInspect {
            routes_file: "Cambrian/Generated/TraceContSpec.lean",
            route_name: "runTrace",
            signal: LeanDivergenceSignal::ForbiddenSubstring("| .error e => .error e"),
        }),
        lean_exec: LeanExec {
            eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.TraceCont
import Cambrian.Generated.World
import Cambrian.Generated.TraceContRoutes
import Cambrian.Generated.TraceContSpec

open TraceCont.Spec.Invariants.trace_continues_after_revert

#eval Id.run do
  let inst : TraceCont.Identity := {}
  let w0 := Cambrian.Generated.World.withTraceCont
    Cambrian.Generated.World.default inst
    ({ TraceCont.State.default with m_count := 10 })
  let ctx := Cambrian.MsgCtx.default
  match runTrace w0 inst ctx [.fail 150, .inc] with
  | .ok w => pure (Cambrian.Generated.World.traceCont w inst).m_count
  | .error _ => pure (9999 : BitVec 64)
"#,
        },
    },
    DifferentialCase {
        id: "T-X-009",
        lean_hypothesis: "LEAN-H1",
        evm_project_yaml: "x_h11_return_shortcircuit_evm.yaml",
        lean_project_yaml: "x_h11_return_shortcircuit_lean.yaml",
        forge_test_file: "XH11ReturnShortcircuit.t.sol",
        forge_match_test: "test_X009_throwShortCircuitsAfterFirstEmit",
        expected_outcome: ExpectedOutcome::Bool(false),
        lean_inspect: Some(LeanRouteInspect {
            routes_file: "Cambrian/Generated/ShortCircuitRoutes.lean",
            route_name: "go",
            signal: LeanDivergenceSignal::ForbiddenSubstring("ShortCircuit_Logged 2"),
        }),
        lean_exec: LeanExec {
            eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.ShortCircuit
import Cambrian.Generated.World
import Cambrian.Generated.ShortCircuitRoutes

#eval Id.run do
  let inst : ShortCircuit.Identity := {}
  let w0 := Cambrian.Generated.World.withShortCircuit
    Cambrian.Generated.World.default inst ShortCircuit.State.default
  let ctx := Cambrian.MsgCtx.default
  pure (ShortCircuit.Routes.go w0 inst ctx 1).isOk
"#,
        },
    },
    DifferentialCase {
        id: "T-X-010",
        lean_hypothesis: "LEAN-H10",
        evm_project_yaml: "x_h12_route_fail_mode_evm.yaml",
        lean_project_yaml: "x_h12_route_fail_mode_lean.yaml",
        forge_test_file: "XH12RouteFailMode.t.sol",
        forge_match_test: "test_X010_nestedFailModeRevertAndPass",
        expected_outcome: ExpectedOutcome::Bool(false),
        lean_inspect: Some(LeanRouteInspect {
            routes_file: "Cambrian/Generated/FailSurfaceRoutes.lean",
            route_name: "invoke",
            signal: LeanDivergenceSignal::ForbiddenSubstring("Cambrian.exceptGetD"),
        }),
        lean_exec: LeanExec {
            eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.FailSurface
import Cambrian.Generated.World
import Cambrian.Generated.FailSurfaceRoutes

#eval Id.run do
  let inst : FailSurface.Identity := {}
  let w0 := Cambrian.Generated.World.withFailSurface
    Cambrian.Generated.World.default inst FailSurface.State.default
  let ctx := Cambrian.MsgCtx.default
  pure (FailSurface.Routes.invoke w0 inst ctx 600).isOk
"#,
        },
    },
    DifferentialCase {
        id: "PW3-O-001",
        lean_hypothesis: "PW3-O-001",
        evm_project_yaml: "x_pw3_option_zero_evm.yaml",
        lean_project_yaml: "x_pw3_option_zero_lean.yaml",
        forge_test_file: "Pw3OptionZeroClassify.t.sol",
        forge_match_test: "test_PW3_O001_setSomeZeroClassifiesAsSome",
        expected_outcome: ExpectedOutcome::Bool(true),
        lean_inspect: None,
        lean_exec: LeanExec {
            eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.OptZero
import Cambrian.Generated.OptZeroRoutes

#eval Id.run do
  let inst : OptZero.Identity := {}
  let ctx := Cambrian.MsgCtx.default
  let s0 := OptZero.State.default
  let s1 := OptZero.Local.setSome s0 ctx inst 0
  let (_, r) := OptZero.Local.classify s1 ctx inst
  pure (r == 222)
"#,
        },
    },
];

fn audit_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit")
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-diff-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ))
}

fn lean_build_enabled() -> bool {
    std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() == Ok("1")
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

fn ensure_forge_std(out_dir: &Path) {
    cambrian_transpiler::codegen::evm_test_codegen::install_forge_std(out_dir)
        .unwrap_or_else(|e| panic!("{e} in {}", out_dir.display()));
}

fn transpile_evm(yaml_path: &Path, out_dir: &Path) -> Result<(), String> {
    let project = Project::load(yaml_path).map_err(|e| format!("load project: {e}"))?;
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    let files = backend.gen_project(&project);
    for (rel, contents) in files {
        let path = out_dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, contents).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

fn transpile_lean(yaml_path: &Path) -> Result<HashMap<String, String>, String> {
    let project = Project::load(yaml_path).map_err(|e| format!("load project: {e}"))?;
    let backend = LeanBackend::default();
    Ok(backend.gen_project(&project).into_iter().collect())
}

fn extract_route_body(lean: &str, route_name: &str) -> Option<String> {
    let needle = format!("def {route_name} ");
    let start = lean.find(&needle)?;
    let rest = &lean[start..];
    let end = rest.find("\n\n").unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

struct ForgeRun {
    ok: bool,
    combined: String,
}

fn run_forge_case(case: &DifferentialCase) -> ForgeRun {
    let out_dir = unique_out_dir(case.id);
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let yaml_path = audit_root().join("fixtures").join(case.evm_project_yaml);
    if let Err(err) = transpile_evm(&yaml_path, &out_dir) {
        let _ = std::fs::remove_dir_all(&out_dir);
        return ForgeRun {
            ok: false,
            combined: err,
        };
    }

    // Hand-written audit forge tests replace generated `Invariant_*.t.sol` files
    // (some fixtures hit known EVM invariant-codegen bugs; see T-X-007).
    if let Ok(entries) = std::fs::read_dir(out_dir.join("test")) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("Invariant_") && name.ends_with(".t.sol") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    if !out_dir.join("foundry.toml").exists() {
        std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).expect("foundry.toml");
    }

    let test_dir = out_dir.join("test");
    std::fs::create_dir_all(&test_dir).expect("test dir");
    let src_test = audit_root().join("forge").join(case.forge_test_file);
    std::fs::copy(&src_test, test_dir.join(case.forge_test_file)).expect("copy forge test");
    ensure_forge_std(&out_dir);

    let forge = Command::new("forge")
        .args([
            "test",
            "--match-test",
            case.forge_match_test,
            "-vv",
            "--root",
        ])
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
    ForgeRun { ok, combined }
}

struct LeanInspect {
    ok: bool,
    detail: String,
}

fn inspect_lean_case(case: &DifferentialCase) -> LeanInspect {
    let inspect = match &case.lean_inspect {
        Some(spec) => spec,
        None => {
            return LeanInspect {
                ok: true,
                detail: format!("{}: no Lean route inspect leg (exec-only row)", case.id),
            };
        }
    };
    let yaml_path = audit_root().join("fixtures").join(case.lean_project_yaml);
    let files = match transpile_lean(&yaml_path) {
        Ok(f) => f,
        Err(err) => {
            return LeanInspect {
                ok: false,
                detail: err,
            };
        }
    };
    let lean = match files.get(inspect.routes_file) {
        Some(s) => s,
        None => {
            return LeanInspect {
                ok: false,
                detail: format!("missing {}", inspect.routes_file),
            };
        }
    };
    let body = match extract_route_body(lean, inspect.route_name) {
        Some(b) => b,
        None => {
            return LeanInspect {
                ok: false,
                detail: format!("route `{}` not found", inspect.route_name),
            };
        }
    };
    let diverged = match &inspect.signal {
        LeanDivergenceSignal::ForbiddenSubstring(needle) => {
            if body.contains(needle) {
                LeanInspect {
                    ok: false,
                    detail: format!(
                        "Lean route `{}` contains `{}`\n{}",
                        inspect.route_name, needle, body
                    ),
                }
            } else {
                LeanInspect {
                    ok: true,
                    detail: format!(
                        "Lean route `{}` has no `{}`",
                        inspect.route_name, needle
                    ),
                }
            }
        }
        LeanDivergenceSignal::RequiredSubstring(needle) => {
            if body.contains(needle) {
                LeanInspect {
                    ok: true,
                    detail: format!(
                        "Lean route `{}` contains `{}`",
                        inspect.route_name, needle
                    ),
                }
            } else {
                LeanInspect {
                    ok: false,
                    detail: format!(
                        "Lean route `{}` missing required `{}`\n{}",
                        inspect.route_name, needle, body
                    ),
                }
            }
        }
        LeanDivergenceSignal::MinSubstringCount { needle, min } => {
            let count = body.matches(needle).count();
            if count >= *min {
                LeanInspect {
                    ok: false,
                    detail: format!(
                        "Lean route `{}` has {} occurrences of `{}` (>= {})\n{}",
                        inspect.route_name, count, needle, min, body
                    ),
                }
            } else {
                LeanInspect {
                    ok: true,
                    detail: format!(
                        "Lean route `{}` has {} occurrences of `{}` (< {})",
                        inspect.route_name, count, needle, min
                    ),
                }
            }
        }
    };
    diverged
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

/// Transpile `yaml_rel` (under `tests/audit/fixtures/`), write `Eval.lean`,
/// `lake build`, then `lake env lean Eval.lean`. Gated: without
/// `CAMBRIAN_TEST_LEAN_BUILD=1` returns ok with a skip message.
fn eval_lean_case_at(
    yaml_rel: &str,
    exec: &LeanExec,
    expected: ExpectedOutcome,
    tag: &str,
) -> LeanInspect {
    let expected_sub = expected.lean_stdout_substring();
    if !lean_build_enabled() {
        return LeanInspect {
            ok: true,
            detail: format!(
                "skip lean #eval (set CAMBRIAN_TEST_LEAN_BUILD=1); expected contains `{expected_sub}`"
            ),
        };
    }
    if !has_lake() {
        return LeanInspect {
            ok: false,
            detail: "CAMBRIAN_TEST_LEAN_BUILD=1 but `lake` not on PATH".into(),
        };
    }

    let yaml_path = audit_root().join("fixtures").join(yaml_rel);
    let files = match transpile_lean(&yaml_path) {
        Ok(f) => f,
        Err(err) => {
            return LeanInspect {
                ok: false,
                detail: err,
            };
        }
    };

    let out_dir = unique_out_dir(tag);
    let _ = std::fs::remove_dir_all(&out_dir);
    if let Err(err) = std::fs::create_dir_all(&out_dir) {
        return LeanInspect {
            ok: false,
            detail: format!("mkdir {}: {err}", out_dir.display()),
        };
    }
    if let Err(err) = write_lean_project(&files, &out_dir) {
        let _ = std::fs::remove_dir_all(&out_dir);
        return LeanInspect {
            ok: false,
            detail: err,
        };
    }
    if let Err(err) = std::fs::write(out_dir.join("Eval.lean"), exec.eval_lean.trim_start()) {
        let _ = std::fs::remove_dir_all(&out_dir);
        return LeanInspect {
            ok: false,
            detail: format!("write Eval.lean: {err}"),
        };
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
        return LeanInspect {
            ok: false,
            detail,
        };
    }

    let eval = Command::new("lake")
        .args(["env", "lean", "Eval.lean"])
        .current_dir(&out_dir)
        .output()
        .expect("lake env lean");
    let stdout = String::from_utf8_lossy(&eval.stdout);
    let stderr = String::from_utf8_lossy(&eval.stderr);
    let combined = format!("stdout:\n{stdout}\nstderr:\n{stderr}");
    let ok = eval.status.success() && stdout.contains(&expected_sub);
    let _ = std::fs::remove_dir_all(&out_dir);
    if ok {
        LeanInspect {
            ok: true,
            detail: format!(
                "#eval stdout contains `{expected_sub}` ({})\n{combined}",
                expected.describe()
            ),
        }
    } else {
        LeanInspect {
            ok: false,
            detail: format!(
                "#eval expected stdout to contain `{expected_sub}` ({}) (status={:?})\n{combined}",
                expected.describe(),
                eval.status.code()
            ),
        }
    }
}

fn eval_lean_case(case: &DifferentialCase) -> LeanInspect {
    eval_lean_case_at(
        case.lean_project_yaml,
        &case.lean_exec,
        case.expected_outcome,
        case.id,
    )
}

struct DiffRun {
    forge_ok: bool,
    lean_ok: bool,
    detail: String,
}

fn run_case(case: &DifferentialCase) -> DiffRun {
    let forge = run_forge_case(case);
    let inspect = inspect_lean_case(case);
    let eval = eval_lean_case(case);
    let lean_ok = if lean_build_enabled() {
        eval.ok
    } else {
        inspect.ok
    };
    let diverged = forge.ok && !lean_ok;
    let detail = format!(
        "expected {} (forge PASS validates same semantics)\nEVM (Forge): {}\n{}\n\nLean inspect: {}\n{}\n\nLean #eval: {}\n{}\n\nDivergence: {}",
        case.expected_outcome.describe(),
        if forge.ok { "PASS" } else { "FAIL" },
        forge.combined,
        if inspect.ok { "PASS" } else { "FAIL" },
        inspect.detail,
        if eval.ok { "PASS" } else { "FAIL" },
        eval.detail,
        if diverged {
            "YES — EVM matches spec, Lean does not"
        } else if !forge.ok && !lean_ok {
            "both fail checks"
        } else if forge.ok && lean_ok {
            "none"
        } else {
            "EVM failed but Lean passed (unexpected)"
        }
    );
    DiffRun {
        forge_ok: forge.ok,
        lean_ok,
        detail,
    }
}

#[test]
fn audit_differential_every_case_has_lean_exec() {
    for case in CASES {
        assert!(
            !case.lean_exec.eval_lean.trim().is_empty(),
            "{} missing LeanExec.eval_lean",
            case.id
        );
        assert!(
            case.lean_exec.eval_lean.contains("#eval"),
            "{} LeanExec must contain #eval",
            case.id
        );
        assert!(
            !case.expected_outcome.lean_stdout_substring().is_empty(),
            "{} missing ExpectedOutcome",
            case.id
        );
    }
    assert!(
        CASES.len() >= 9,
        "expected Phase D differential cases + PW3-O-001 to carry ExpectedOutcome"
    );
}

/// PW3-O-012 meta-gate: one `expected_outcome` per case; no duplicate per-leg literals.
#[test]
fn audit_differential_pw3_o012_single_expected_policy() {
    let src = include_str!("test_audit_differential.rs");
    assert!(
        src.contains("enum ExpectedOutcome"),
        "PW3-O-012: differential harness must define ExpectedOutcome"
    );
    assert!(
        src.contains("expected_outcome:"),
        "PW3-O-012: cases must declare expected_outcome"
    );
    assert!(
        !src.contains("expected: \"false\"") && !src.contains("expected: \"true\""),
        "PW3-O-012: LeanExec must not carry duplicate expected literals"
    );
    for case in CASES {
        assert!(
            case.forge_match_test.starts_with("test_"),
            "{} missing forge_match_test",
            case.id
        );
    }
    let pw3 = CASES
        .iter()
        .find(|c| c.id == "PW3-O-001")
        .expect("PW3-O-001 row");
    assert_eq!(
        pw3.expected_outcome,
        ExpectedOutcome::Bool(true),
        "PW3-O-001 classify==222 must share Bool(true) across legs"
    );
}

#[test]
fn audit_differential_lean_eval_all_cases() {
    if !lean_build_enabled() {
        eprintln!("skip lean #eval all-cases (set CAMBRIAN_TEST_LEAN_BUILD=1)");
        return;
    }
    let mut failures = Vec::new();
    for case in CASES {
        let run = eval_lean_case(case);
        if !run.ok {
            failures.push(format!("{}: {}", case.id, run.detail));
        }
    }
    assert!(
        failures.is_empty(),
        "Lean #eval oracle failed for {} case(s):\n{}",
        failures.len(),
        failures.join("\n---\n")
    );
}

#[test]
fn audit_differential_pn103_overflow_panic_eval() {
    let exec = LeanExec {
        eval_lean: r#"
import Cambrian.Prelude
import Cambrian.Generated.Adder
import Cambrian.Generated.World
import Cambrian.Generated.AdderRoutes

#eval Id.run do
  let inst : Adder.Identity := {}
  let w0 := Cambrian.Generated.World.withAdder
    Cambrian.Generated.World.default inst
    ({ Adder.State.default with m_x := (BitVec.allOnes 256) })
  let ctx := Cambrian.MsgCtx.default
  pure (Adder.Routes.bump w0 inst ctx 1).isOk
"#,
    };
    let run = eval_lean_case_at(
        "phase_n/pn_arith_max_plus_one_lean.yaml",
        &exec,
        ExpectedOutcome::Bool(false),
        "PN-103",
    );
    assert!(
        run.ok,
        "PN-103 overflow-panic #eval (MAX+1 → isOk=false):\n{}",
        run.detail
    );
}

#[test]
fn audit_differential_hypotheses_report() {
    if !has_forge() {
        eprintln!(
            "skipping audit_differential_hypotheses_report: forge not on PATH \
             (Lean CI image has lake only; Forge+inspect lives in \
             test-transpiler-audit-differential)"
        );
        return;
    }
    let mut confirmed = Vec::new();
    let mut report = Vec::new();
    for case in CASES {
        let run = run_case(case);
        let status = if run.forge_ok && run.lean_ok {
            "ALIGNED"
        } else if run.forge_ok && !run.lean_ok {
            "DIVERGED (Lean hypothesis confirmed)"
        } else {
            "INCONCLUSIVE"
        };
        report.push(format!(
            "{} / {}: {}\n{}",
            case.id, case.lean_hypothesis, status, run.detail
        ));
        if run.forge_ok && !run.lean_ok {
            confirmed.push(format!(
                "{} {}: CONFIRMED divergence\n{}",
                case.id, case.lean_hypothesis, run.detail
            ));
        }
    }
    eprintln!("=== Differential audit report ===\n{}", report.join("\n---\n"));
    if !confirmed.is_empty() {
        panic!(
            "Differential audit: {} confirmed divergence(s):\n\n{}",
            confirmed.len(),
            confirmed.join("\n---\n")
        );
    }
}

#[test]
fn audit_differential_x002_underfunded_transfer() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[0];
    let run = run_case(case);
    assert!(
        run.forge_ok && run.lean_ok,
        "{} {} diverged or inconclusive:\n{}",
        case.id,
        case.lean_hypothesis,
        run.detail
    );
}

#[test]
fn audit_differential_x001_double_deploy() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[1];
    let run = run_case(case);
    assert!(
        run.forge_ok && run.lean_ok,
        "{} {} diverged or inconclusive:\n{}",
        case.id,
        case.lean_hypothesis,
        run.detail
    );
}

#[test]
fn audit_differential_x003_deploy_init_params() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[2];
    let run = run_case(case);
    assert!(
        run.forge_ok && run.lean_ok,
        "{} {} diverged or inconclusive:\n{}",
        case.id,
        case.lean_hypothesis,
        run.detail
    );
}

#[test]
fn audit_differential_x005_dynamic_dispatch_value() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = CASES
        .iter()
        .find(|c| c.id == "T-X-005")
        .expect("T-X-005 case");
    let run = run_case(case);
    assert!(
        run.forge_ok && run.lean_ok,
        "{} {} diverged or inconclusive:\n{}",
        case.id,
        case.lean_hypothesis,
        run.detail
    );
}

#[test]
fn audit_differential_x006_typed_send_ctx_value() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = CASES
        .iter()
        .find(|c| c.id == "T-X-006")
        .expect("T-X-006 case");
    let run = run_case(case);
    assert!(
        run.forge_ok && run.lean_ok,
        "{} {} diverged or inconclusive:\n{}",
        case.id,
        case.lean_hypothesis,
        run.detail
    );
}

#[test]
fn audit_differential_x007_runtrace_continue() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = CASES
        .iter()
        .find(|c| c.id == "T-X-007")
        .expect("T-X-007 case");
    let run = run_case(case);
    assert!(
        run.forge_ok && run.lean_ok,
        "{} {} diverged or inconclusive:\n{}",
        case.id,
        case.lean_hypothesis,
        run.detail
    );
}

#[test]
fn audit_differential_x009_return_shortcircuit() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = CASES
        .iter()
        .find(|c| c.id == "T-X-009")
        .expect("T-X-009 case");
    let run = run_case(case);
    assert!(
        run.forge_ok && run.lean_ok,
        "{} {} diverged or inconclusive:\n{}",
        case.id,
        case.lean_hypothesis,
        run.detail
    );
}

#[test]
fn audit_differential_x010_route_fail_mode() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = CASES
        .iter()
        .find(|c| c.id == "T-X-010")
        .expect("T-X-010 case");
    let run = run_case(case);
    assert!(
        run.forge_ok && run.lean_ok,
        "{} {} diverged or inconclusive:\n{}",
        case.id,
        case.lean_hypothesis,
        run.detail
    );
}

fn differential_case(id: &str) -> &'static DifferentialCase {
    CASES
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("missing differential case {id}"))
}

#[test]
fn audit_differential_pw3_o001_lean_eval_some_zero() {
    let case = differential_case("PW3-O-001");
    let run = eval_lean_case(case);
    assert!(
        run.ok,
        "PW3-O-001 Lean #eval: setSome(0) then classify must yield 222 (expected {}):\n{}",
        case.expected_outcome.describe(),
        run.detail
    );
}

#[test]
fn audit_differential_pw3_o001_evm_forge_some_zero() {
    if !has_forge() {
        eprintln!("skip PW3-O-001 forge (no forge on PATH)");
        return;
    }
    let case = differential_case("PW3-O-001");
    let run = run_forge_case(case);
    assert!(
        run.ok,
        "PW3-O-001 EVM forge: setSome(0) then classify must match {} (forge assertEq 222):\n{}",
        case.expected_outcome.describe(),
        run.combined
    );
}

#[test]
fn audit_differential_pw3_o001_cross_target_parity() {
    let case = differential_case("PW3-O-001");
    let lean = eval_lean_case(case);
    assert!(
        lean.ok,
        "PW3-O-001 Lean leg must pass before cross-target check (expected {}):\n{}",
        case.expected_outcome.describe(),
        lean.detail
    );
    if !has_forge() {
        eprintln!("skip PW3-O-001 cross-target forge leg (no forge on PATH)");
        return;
    }
    let forge = run_forge_case(case);
    assert!(
        forge.ok,
        "PW3-O-001 cross-target: EVM forge must match shared expected {}:\n{}",
        case.expected_outcome.describe(),
        forge.combined
    );
}

#[test]
fn audit_differential_pw3_o001_evm_lowering_uses_tagged_option() {
    let case = differential_case("PW3-O-001");
    let yaml_path = audit_root().join("fixtures").join(case.evm_project_yaml);
    let project = Project::load(&yaml_path).expect("load pw3 evm yaml");
    let backend = EvmSolidityBackend {
        deterministic_addresses: project
            .config
            .deterministic_addresses
            .unwrap_or(true),
    };
    let files = backend.gen_project(&project);
    let project_key = format!("src/_{}_project.sol", project.name());
    let sol = files
        .iter()
        .find(|(p, _)| p == &project_key)
        .map(|(_, c)| c.as_str())
        .expect("project sol");
    assert!(
        sol.contains("struct Option_uint64")
            && sol.contains("Option_uint64 public m_t")
            && sol.contains(".tag == Option_uint64_Tag.None")
            && !sol.contains("(m_t == 0 ?"),
        "PW3-O-001: Option<u64> must lower to a tagged struct, not a 0-sentinel:\n{sol}"
    );
}

#[test]
fn audit_differential_pw3_o001_lean_lowering_preserves_option() {
    let case = differential_case("PW3-O-001");
    let yaml_path = audit_root().join("fixtures").join(case.lean_project_yaml);
    let files = transpile_lean(&yaml_path).expect("lean transpile");
    let routes = files
        .get("Cambrian/Generated/OptZeroRoutes.lean")
        .expect("OptZeroRoutes");
    let body = extract_route_body(routes, "classify").expect("classify body");
    assert!(
        body.contains("Option.none") && body.contains("Option.some"),
        "PW3-O-001 Lean classify must pattern-match Option:\n{body}"
    );
}
