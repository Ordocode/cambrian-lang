// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Audit hypothesis tests — EVM execution via Forge only.
//!
//! Each case transpiles a minimal project, copies a hand-written Forge
//! test, runs `forge test`, and records pass/fail. A failing test means
//! the hypothesis is **confirmed** (code diverges from expected semantics).
//!
//! See [docs/AUDIT_EVM_LEAN.md](../../../docs/AUDIT_EVM_LEAN.md).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::{EvmSolidityBackend, OutputBackend};
use cambrian_transpiler::project::Project;
use cambrian_transpiler::ast::{RouteAction, RouteBody};
use cambrian_transpiler::validate::{self, Severity};

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

struct AuditCase {
    id: &'static str,
    hypothesis: &'static str,
    project_yaml: &'static str,
    forge_test_file: &'static str,
    match_test: &'static str,
    /// Wrap the first `call` in `invokeWithRescue` with `rescue call_failed:`.
    /// The grammar does not allow this surface form; injection exercises the
    /// codegen path audited by EVM-H3.
    inject_rescue_callroute: bool,
}

const CASES: &[AuditCase] = &[
    AuditCase {
        id: "T-EVM-001",
        hypothesis: "EVM-H1",
        project_yaml: "evm_h1_deploy_value.yaml",
        forge_test_file: "EvmH1DeployValue.t.sol",
        match_test: "test_EVM_H1_deployForwardsValueToInstance",
        inject_rescue_callroute: false,
    },
    AuditCase {
        id: "T-EVM-002",
        hypothesis: "EVM-H2",
        project_yaml: "evm_h2_frontrun.yaml",
        forge_test_file: "EvmH2Frontrun.t.sol",
        match_test: "test_EVM_H2_attackerCannotPoisonVaultInit",
        inject_rescue_callroute: false,
    },
    AuditCase {
        id: "T-EVM-003",
        hypothesis: "EVM-H3",
        project_yaml: "evm_h3_rescue_callroute.yaml",
        forge_test_file: "EvmH3RescueCallRoute.t.sol",
        match_test: "test_EVM_H3_rescueCallRouteRunsRecoverOnCalleeRevert",
        inject_rescue_callroute: true,
    },
    AuditCase {
        id: "T-EVM-004",
        hypothesis: "EVM-H4",
        project_yaml: "evm_h4_interface_payable.yaml",
        forge_test_file: "EvmH4InterfacePayable.t.sol",
        match_test: "test_EVM_H4_typedSendWithValueCreditsPayee",
        inject_rescue_callroute: false,
    },
    AuditCase {
        id: "T-EVM-005",
        hypothesis: "EVM-H5",
        project_yaml: "evm_h5_det_payable.yaml",
        forge_test_file: "EvmH5DetPayable.t.sol",
        match_test: "test_EVM_H5_receiveRecordsPayment",
        inject_rescue_callroute: false,
    },
    AuditCase {
        id: "T-EVM-006",
        hypothesis: "EVM-H6",
        project_yaml: "evm_h6_phased_receive.yaml",
        forge_test_file: "EvmH6PhasedReceive.t.sol",
        match_test: "test_EVM_H6_phasedReceiveRecordsPayment",
        inject_rescue_callroute: false,
    },
    AuditCase {
        id: "T-EVM-007",
        hypothesis: "EVM-H7",
        project_yaml: "evm_h7_transform_let.yaml",
        forge_test_file: "EvmH7TransformLet.t.sol",
        match_test: "test_EVM_H7_bumpUsesPreRouteSnapshotForSecondMember",
        inject_rescue_callroute: false,
    },
    AuditCase {
        id: "T-EVM-008",
        hypothesis: "EVM-H8",
        project_yaml: "evm_h8_match_priority.yaml",
        forge_test_file: "EvmH8MatchPriority.t.sol",
        match_test: "test_EVM_H8_matchUsesFirstMatchingArm",
        inject_rescue_callroute: false,
    },
    AuditCase {
        id: "T-EVM-009",
        hypothesis: "EVM-H9",
        project_yaml: "evm_h9_if_no_else.yaml",
        forge_test_file: "EvmH9IfNoElse.t.sol",
        match_test: "test_EVM_H9_ifWithoutElseDefaultsBoolToFalse",
        inject_rescue_callroute: false,
    },
    AuditCase {
        id: "T-EVM-012",
        hypothesis: "EVM-H12",
        project_yaml: "evm_h12_raw_send_sig.yaml",
        forge_test_file: "EvmH12RawSendSig.t.sol",
        match_test: "test_EVM_H12_rawNamedSendSetsOwnerOnPlainAddress",
        inject_rescue_callroute: false,
    },
    AuditCase {
        id: "T-EVM-013",
        hypothesis: "EVM-H13",
        project_yaml: "evm_h13_double_deploy.yaml",
        forge_test_file: "EvmH13DoubleDeploy.t.sol",
        match_test: "test_EVM_H13_twoDeploysSameEntityCompileAndRun",
        inject_rescue_callroute: false,
    },
    AuditCase {
        id: "T-EVM-011",
        hypothesis: "EVM-H11",
        project_yaml: "evm_h11_nested_contains.yaml",
        forge_test_file: "EvmH11NestedContains.t.sol",
        match_test: "test_EVM_H11_nestedContainsCompilesAndReturnsFalseOnEmpty",
        inject_rescue_callroute: false,
    },
    AuditCase {
        id: "T-EVM-014",
        hypothesis: "EVM-H14",
        project_yaml: "evm_h14_phased_extern_var.yaml",
        forge_test_file: "EvmH14PhasedExternVar.t.sol",
        match_test: "test_EVM_H14_phasedExternVarCallReturnsBool",
        inject_rescue_callroute: false,
    },
    AuditCase {
        id: "T-EVM-015",
        hypothesis: "EVM-H15",
        project_yaml: "evm_h15_reserved_route.yaml",
        forge_test_file: "EvmH15ReservedRoute.t.sol",
        match_test: "test_EVM_H15_reservedRouteNameIsCallable",
        inject_rescue_callroute: false,
    },
    AuditCase {
        id: "T-EVM-017",
        hypothesis: "EVM-H18",
        project_yaml: "evm_h18_let_binding_leak.yaml",
        forge_test_file: "EvmH18LetBindingLeak.t.sol",
        match_test: "test_EVM_H18_routeBStoresBoolAfterRouteAQuotientLet",
        inject_rescue_callroute: false,
    },
    AuditCase {
        id: "T-EVM-010",
        hypothesis: "EVM-H10",
        project_yaml: "evm_h10_tuple_destructure.yaml",
        forge_test_file: "EvmH10TupleDestructure.t.sol",
        match_test: "test_EVM_H10_memberTransformTupleDestructureCompilesAndSplits",
        inject_rescue_callroute: false,
    },
    AuditCase {
        id: "T-EVM-016",
        hypothesis: "EVM-H17",
        project_yaml: "evm_h17_hex_address.yaml",
        forge_test_file: "EvmH17HexAddress.t.sol",
        match_test: "test_EVM_H17_addressShapedHexBindsAsBytes32",
        inject_rescue_callroute: false,
    },
];

fn audit_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit")
}

fn inject_rescue_callroute(project: &mut Project) {
    let entity = project
        .merged
        .entities
        .iter_mut()
        .find(|e| e.name == "Guard")
        .expect("Guard entity");
    let route = entity
        .routes
        .iter_mut()
        .find(|r| r.name == "invokeWithRescue")
        .expect("invokeWithRescue route");
    let RouteBody::Unphased(actions) = &mut route.body else {
        panic!("invokeWithRescue must be unphased");
    };
    let pos = actions
        .iter()
        .position(|a| matches!(a, RouteAction::CallRoute { .. }))
        .expect("invokeWithRescue must contain call");
    let call = actions.remove(pos);
    actions.insert(
        pos,
        RouteAction::Rescue {
            tag: "call_failed".to_string(),
            action: Box::new(call),
        },
    );
}

fn transpile_audit_project(yaml_path: &Path, out_dir: &Path, case: &AuditCase) -> Result<(), String> {
    let mut project = Project::load(yaml_path).map_err(|e| format!("load project: {e}"))?;
    if case.inject_rescue_callroute {
        inject_rescue_callroute(&mut project);
    }
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

fn has_forge() -> bool {
    Command::new("forge")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-evm-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ))
}

struct ForgeRun {
    forge_ok: bool,
    combined: String,
}

fn ensure_forge_std(out_dir: &Path) {
    cambrian_transpiler::codegen::evm_test_codegen::install_forge_std(out_dir)
        .unwrap_or_else(|e| panic!("{e} in {}", out_dir.display()));
}

fn run_audit_case(case: &AuditCase) -> ForgeRun {
    let out_dir = unique_out_dir(case.hypothesis);
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let yaml_path = audit_root().join("fixtures").join(case.project_yaml);
    if let Err(err) = transpile_audit_project(&yaml_path, &out_dir, case) {
        let _ = std::fs::remove_dir_all(&out_dir);
        return ForgeRun {
            forge_ok: false,
            combined: err,
        };
    }

    if !out_dir.join("foundry.toml").exists() {
        std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).expect("foundry.toml");
    }

    let test_dir = out_dir.join("test");
    std::fs::create_dir_all(&test_dir).expect("test dir");
    let src_test = audit_root().join("forge").join(case.forge_test_file);
    let dst_test = test_dir.join(case.forge_test_file);
    std::fs::copy(&src_test, &dst_test).expect("copy forge test");

    ensure_forge_std(&out_dir);

    let forge = Command::new("forge")
        .args([
            "test",
            "--match-test",
            case.match_test,
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
    ForgeRun {
        forge_ok: ok,
        combined,
    }
}

fn read_transpiled_sol(out_dir: &Path) -> String {
    let src = out_dir.join("src");
    let mut parts = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&src) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "sol") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    parts.push(format!("// {}\n{text}", path.file_name().unwrap().to_string_lossy()));
                }
            }
        }
    }
    parts.sort();
    parts.join("\n\n")
}

fn forge_build(out_dir: &Path) -> ForgeRun {
    ensure_forge_std(out_dir);
    if !out_dir.join("foundry.toml").exists() {
        std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).expect("foundry.toml");
    }
    let forge = Command::new("forge")
        .args(["build", "--root"])
        .arg(out_dir)
        .output()
        .expect("forge build");
    let combined = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );
    ForgeRun {
        forge_ok: forge.status.success(),
        combined,
    }
}

fn prepare_audit_out_dir(yaml_path: &Path, tag: &str) -> Result<(PathBuf, String), String> {
    let out_dir = unique_out_dir(tag);
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create out dir");
    let no_inject = AuditCase {
        id: "prepare",
        hypothesis: "prepare",
        project_yaml: "unused",
        forge_test_file: "unused",
        match_test: "unused",
        inject_rescue_callroute: false,
    };
    transpile_audit_project(yaml_path, &out_dir, &no_inject)?;
    let sol = read_transpiled_sol(&out_dir);
    Ok((out_dir, sol))
}

fn audit_case_wont_fix(case: &AuditCase) -> bool {
    // PN-105 / E26: rescue/recover is Acki Nacki-only; EVM rejects at validate and
    // force-codegen emits an E26 comment instead of try/catch.
    case.hypothesis == "EVM-H3"
}

fn assert_evm_rescue_not_lowered(out_dir: &Path, label: &str) {
    let sol = read_transpiled_sol(out_dir);
    assert!(
        sol.contains("E26")
            && !sol.contains("try this.")
            && !sol.contains("catch"),
        "{label}: rescue must not lower to try/catch on EVM (E26 Acki Nacki-only): {sol}"
    );
}

/// Run every audit case and return a human-readable report.
fn run_all_audit_cases() -> (String, Vec<String>) {
    let mut lines = Vec::new();
    let mut confirmed = Vec::new();
    for case in CASES {
        if audit_case_wont_fix(case) {
            lines.push(format!(
                "{} / {}: WONT FIX (E26 Acki Nacki-only — see docs/plans/pn-105-rescue-evm-reject.md)",
                case.id, case.hypothesis
            ));
            continue;
        }
        let run = run_audit_case(case);
        let status = if run.forge_ok {
            "PASS (hypothesis disproven or N/A)"
        } else {
            "FAIL (hypothesis confirmed — bug or spec mismatch)"
        };
        lines.push(format!(
            "{} / {}: {}\n{}",
            case.id, case.hypothesis, status, run.combined
        ));
        if !run.forge_ok {
            confirmed.push(format!(
                "{} {}: CONFIRMED\n{}",
                case.id, case.hypothesis, run.combined
            ));
        }
    }
    (lines.join("\n---\n"), confirmed)
}

#[test]
fn audit_evm_hypotheses_forge_report() {
    if !has_forge() {
        eprintln!(
            "skipping audit_evm_hypotheses_forge_report: forge not on PATH \
             (coverage / non-Foundry CI images; Forge execution lives in \
             test-transpiler-audit-evm)"
        );
        return;
    }
    let (report, confirmed) = run_all_audit_cases();
    eprintln!("=== EVM audit hypothesis report ===\n{report}");

    if !confirmed.is_empty() {
        panic!(
            "EVM audit: {} confirmed hypothesis(es):\n\n{}",
            confirmed.len(),
            confirmed.join("\n---\n")
        );
    }
}

#[test]
fn audit_evm_h1_deploy_value_forge() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[0];
    let run = run_audit_case(case);
    assert!(
        run.forge_ok,
        "{} {} failed (confirmed bug):\n{}",
        case.id,
        case.hypothesis,
        run.combined
    );
}

#[test]
fn audit_evm_h2_frontrun_forge() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[1];
    let run = run_audit_case(case);
    assert!(
        run.forge_ok,
        "{} {} failed (confirmed bug):\n{}",
        case.id,
        case.hypothesis,
        run.combined
    );
}

#[test]
fn audit_evm_h3_rescue_callroute_forge() {
    let case = &CASES[2];
    let out_dir = unique_out_dir(case.hypothesis);
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create out dir");
    let yaml_path = audit_root().join("fixtures").join(case.project_yaml);
    transpile_audit_project(&yaml_path, &out_dir, case).expect("transpile H3");
    assert_evm_rescue_not_lowered(&out_dir, "T-EVM-003");
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn audit_evm_h4_interface_payable_forge() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[3];
    let run = run_audit_case(case);
    assert!(
        run.forge_ok,
        "{} {} failed (confirmed bug):\n{}",
        case.id,
        case.hypothesis,
        run.combined
    );
}

#[test]
fn audit_evm_h5_det_payable_forge() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[4];
    let run = run_audit_case(case);
    assert!(
        run.forge_ok,
        "{} {} failed (confirmed bug):\n{}",
        case.id,
        case.hypothesis,
        run.combined
    );
}

#[test]
fn audit_evm_h6_phased_receive_forge() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[5];
    let run = run_audit_case(case);
    assert!(
        run.forge_ok,
        "{} {} failed (confirmed bug):\n{}",
        case.id,
        case.hypothesis,
        run.combined
    );
}

#[test]
fn audit_evm_h7_transform_let_forge() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[6];
    let run = run_audit_case(case);
    assert!(
        run.forge_ok,
        "{} {} failed (confirmed bug):\n{}",
        case.id,
        case.hypothesis,
        run.combined
    );
}

#[test]
fn audit_evm_h8_match_priority_forge() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[7];
    let run = run_audit_case(case);
    assert!(
        run.forge_ok,
        "{} {} failed (confirmed bug):\n{}",
        case.id,
        case.hypothesis,
        run.combined
    );
}

#[test]
fn audit_evm_h9_if_no_else_forge() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[8];
    let run = run_audit_case(case);
    assert!(
        run.forge_ok,
        "{} {} failed (confirmed bug):\n{}",
        case.id,
        case.hypothesis,
        run.combined
    );
}

#[test]
fn audit_evm_h12_raw_send_sig_forge() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[9];
    let run = run_audit_case(case);
    assert!(
        run.forge_ok,
        "{} {} failed (confirmed bug):\n{}",
        case.id,
        case.hypothesis,
        run.combined
    );
}

#[test]
fn audit_evm_h13_double_deploy_forge() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[10];
    let run = run_audit_case(case);
    assert!(
        run.forge_ok,
        "{} {} failed (confirmed bug):\n{}",
        case.id,
        case.hypothesis,
        run.combined
    );
}

#[test]
fn audit_evm_h11_nested_contains_forge() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[11];
    let run = run_audit_case(case);
    assert!(run.forge_ok, "{} {} failed (confirmed bug):\n{}", case.id, case.hypothesis, run.combined);
}

#[test]
fn audit_evm_h14_phased_extern_var_forge() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[12];
    let run = run_audit_case(case);
    assert!(run.forge_ok, "{} {} failed (confirmed bug):\n{}", case.id, case.hypothesis, run.combined);
}

#[test]
fn audit_evm_h15_reserved_route_forge() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[13];
    let run = run_audit_case(case);
    assert!(run.forge_ok, "{} {} failed (confirmed bug):\n{}", case.id, case.hypothesis, run.combined);
}

#[test]
fn audit_evm_h18_let_binding_leak_forge() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[14];
    let run = run_audit_case(case);
    assert!(run.forge_ok, "{} {} failed (confirmed bug):\n{}", case.id, case.hypothesis, run.combined);
}

#[test]
fn audit_evm_h10_tuple_destructure_forge() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[15];
    let run = run_audit_case(case);
    assert!(run.forge_ok, "{} {} failed (confirmed bug):\n{}", case.id, case.hypothesis, run.combined);
}

#[test]
fn audit_evm_h17_hex_address_forge() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[16];
    let run = run_audit_case(case);
    assert!(run.forge_ok, "{} {} failed (confirmed bug):\n{}", case.id, case.hypothesis, run.combined);
}

/// T-X-004 / X-H6: E23 covers route-level tuple `let` (T-VAL-002 DISPROVEN) but not
/// member-transform tuple `let` — codegen emits `split(...)[i]` and `forge build` FAIL.
#[test]
fn audit_x004_member_tuple_let_forge() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }

    const ID: &str = "T-X-004";
    const HYP: &str = "X-H6";

    // Control: route-level tuple let (T-VAL-002 fixture) must PASS forge.
    let control_case = AuditCase {
        id: "T-VAL-002-control",
        hypothesis: "X-H2",
        project_yaml: "val_e23_mixed_tuple_purefn.yaml",
        forge_test_file: "ValE23MixedTuplePureFn.t.sol",
        match_test: "test_VAL_E02_mixedTuplePureFnPreservesSlots",
        inject_rescue_callroute: false,
    };
    let control = run_audit_case(&control_case);
    assert!(
        control.forge_ok,
        "{ID} control (route-level `peek` / T-VAL-002) must PASS forge before probing member transform:\n{}",
        control.combined
    );

    let yaml_path = audit_root()
        .join("fixtures")
        .join("x_h4_member_tuple_let_evm.yaml");
    let (out_dir, sol) =
        prepare_audit_out_dir(&yaml_path, "x-h6-probe").expect("transpile probe");
    let route_typed = sol.contains("(address owner, uint256 amt, bool flag) = split");
    let member_indexed = sol.contains("split(who, seed)[0]")
        || sol.contains("split(who, seed)[1]")
        || sol.contains("uint256 o = split");
    let member_fragment = sol
        .lines()
        .find(|l| l.contains("split(who, seed)["))
        .or_else(|| sol.lines().find(|l| l.contains("uint256 o = split")))
        .unwrap_or("<member split lowering not found>")
        .trim()
        .to_string();

    let project = Project::load(&yaml_path).expect("load probe project");
    let diags = validate::validate(&project.merged);
    let e23 = diags
        .iter()
        .any(|d| d.code == "E23" && matches!(d.severity, Severity::Error));

    let build = forge_build(&out_dir);
    let _ = std::fs::remove_dir_all(&out_dir);

    let confirmed = route_typed && member_indexed && !build.forge_ok;
    let status = if confirmed {
        "CONFIRMED"
    } else if !member_indexed && build.forge_ok {
        "DISPROVEN"
    } else {
        "INCONCLUSIVE"
    };

    eprintln!(
        "{ID} {HYP}: {status}\n\
         control route (T-VAL-002): forge PASS\n\
         probe route typed destructure: {route_typed}\n\
         probe member indexed widen: {member_indexed}\n\
         validator E23 on member-transform `let`: {}\n\
         member fragment: {member_fragment}\n\
         probe forge build: {}\n{}",
        if e23 { "PRESENT" } else { "absent" },
        if build.forge_ok { "PASS" } else { "FAIL" },
        build.combined
    );

    assert!(
        !confirmed,
        "{ID} {HYP} CONFIRMED — route-level typed `let` PASS but member-transform lowers to indexed uint256 slots / forge build FAIL\n\
         member fragment: {member_fragment}\n\
         validator E23: {}\n\
         forge build:\n{}",
        if e23 { "present" } else { "absent (gap vs route-level)" },
        build.combined
    );
}

/// T-X-008 / X-H10: invariant fuzz handler assigns `uint256(bound(...))` to a
/// `uint64` route param — generated `Invariant_*.t.sol` fails `forge build`.
#[test]
fn audit_x008_invariant_bound_type_forge() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }

    const ID: &str = "T-X-008";
    const HYP: &str = "X-H10";

    let yaml_path = audit_root()
        .join("fixtures")
        .join("x_h8_invariant_bound_type_evm.yaml");
    let (out_dir, _sol) =
        prepare_audit_out_dir(&yaml_path, "x-h10-probe").expect("transpile probe");

    let test_dir = out_dir.join("test");
    let invariant_path = std::fs::read_dir(&test_dir)
        .expect("test dir")
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("Invariant_"))
        })
        .expect("generated Invariant_*.t.sol");

    let invariant_sol = std::fs::read_to_string(&invariant_path).expect("read invariant test");

    let bound_fragment = invariant_sol
        .lines()
        .find(|l| l.contains("bound("))
        .unwrap_or("<bound(...) not found>")
        .trim()
        .to_string();
    let uint64_param = invariant_sol.contains("touch(uint64 amount)");
    let uint256_bound_assign = bound_fragment.contains("uint256(bound(");

    let build = forge_build(&out_dir);
    let type_error = build.combined.contains("Type uint256 is not implicitly convertible to expected type uint64");
    let _ = std::fs::remove_dir_all(&out_dir);

    let confirmed = uint64_param
        && uint256_bound_assign
        && !build.forge_ok
        && type_error;
    let status = if confirmed {
        "CONFIRMED"
    } else if build.forge_ok {
        "DISPROVEN"
    } else {
        "INCONCLUSIVE"
    };

    eprintln!(
        "{ID} {HYP}: {status}\n\
         generated invariant: {}\n\
         handler param uint64: {uint64_param}\n\
         bound fragment: {bound_fragment}\n\
         forge build: {}\n\
         solc type error (uint256→uint64): {type_error}\n{}",
        invariant_path.file_name().unwrap().to_string_lossy(),
        if build.forge_ok { "PASS" } else { "FAIL" },
        build.combined
    );

    assert!(
        !confirmed,
        "{ID} {HYP} CONFIRMED — generated invariant fuzz `bound` assigns uint256 to uint64 param; forge build FAIL\n\
         bound fragment: {bound_fragment}\n\
         forge build:\n{}",
        build.combined
    );
}
