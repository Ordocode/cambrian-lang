// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Audit hypothesis tests — Lean codegen inspection.
//!
//! Each case transpiles a minimal project and checks generated Lean for
//! expected lowering. A failing assertion means the hypothesis is
//! **confirmed** (code diverges from expected semantics).
//!
//! See [docs/AUDIT_EVM_LEAN.md](../../../docs/AUDIT_EVM_LEAN.md).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::ast::{RouteAction, RouteBody};
use cambrian_transpiler::codegen::{EvmSolidityBackend, LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;

enum LeanAuditCheck {
    /// `route_name` body must not contain `forbidden`.
    RouteMustNotContain {
        forbidden: &'static str,
    },
    /// After the first `terminator` in `route_name`, `forbidden` must be absent.
    NoDeadCodeAfter {
        terminator: &'static str,
        forbidden: &'static str,
    },
    /// `route_name` body must contain every substring.
    RouteMustContainAll {
        required: &'static [&'static str],
    },
}

struct LeanAuditCase {
    id: &'static str,
    hypothesis: &'static str,
    project_yaml: &'static str,
    routes_file: &'static str,
    route_name: &'static str,
    checks: &'static [LeanAuditCheck],
    /// Rewrite the first cross-entity `credit()` send to a bogus route name.
    inject_unresolved_cross_send: bool,
    /// When true, excluded from `audit_lean_hypotheses_report` (custom test only).
    skip_aggregate: bool,
}

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

const CASES: &[LeanAuditCase] = &[
    LeanAuditCase {
        id: "T-LEAN-001",
        hypothesis: "LEAN-H1",
        project_yaml: "lean_h1_return_shortcircuit.yaml",
        routes_file: "Cambrian/Generated/ShortCircuitRoutes.lean",
        route_name: "go",
        checks: &[LeanAuditCheck::NoDeadCodeAfter {
            terminator: "throw (Cambrian.ThrowCode.ofNat 5)",
            forbidden: "ShortCircuit_Logged 2",
        }],
        inject_unresolved_cross_send: false,
        skip_aggregate: false,
    },
    LeanAuditCase {
        id: "T-LEAN-002",
        hypothesis: "LEAN-H2",
        project_yaml: "lean_h2_classify_fail.yaml",
        routes_file: "Cambrian/Generated/ClassifyFailRoutes.lean",
        route_name: "classify",
        checks: &[
            LeanAuditCheck::RouteMustNotContain {
                forbidden: ", default)",
            },
            LeanAuditCheck::RouteMustContainAll {
                required: &[
                    "if (n > ((100 : BitVec 64))) then",
                    "return (s, (2 : BitVec 64))",
                    "return (s, (1 : BitVec 64))",
                    "return (s, (0 : BitVec 64))",
                ],
            },
        ],
        inject_unresolved_cross_send: false,
        skip_aggregate: false,
    },
    LeanAuditCase {
        id: "T-LEAN-003",
        hypothesis: "LEAN-H4",
        project_yaml: "lean_h4_typed_send_value.yaml",
        routes_file: "Cambrian/Generated/PayerRoutes.lean",
        route_name: "pay",
        checks: &[
            LeanAuditCheck::RouteMustContainAll {
                required: &[
                    "callParams",
                    "value := (Cambrian.castWidth 256 (amount : BitVec 128))",
                    "withValue",
                ],
            },
        ],
        inject_unresolved_cross_send: false,
        skip_aggregate: false,
    },
    LeanAuditCase {
        id: "T-LEAN-004",
        hypothesis: "LEAN-H6",
        project_yaml: "lean_h6_silent_send.yaml",
        routes_file: "Cambrian/Generated/SenderRoutes.lean",
        route_name: "ping",
        checks: &[LeanAuditCheck::RouteMustContainAll {
            // Unresolved cross-entity route must emit an L8 sentinel, not a
            // silent empty effect (LEAN-H6). Injection renames `credit` so the
            // resolver cannot find it on Treasury.
            required: &["-- L8:"],
        }],
        inject_unresolved_cross_send: true,
        skip_aggregate: false,
    },
    LeanAuditCase {
        id: "T-LEAN-005",
        hypothesis: "LEAN-H7",
        project_yaml: "lean_h7_dynamic_dispatch_balance.yaml",
        routes_file: "Cambrian/Generated/PayerRoutes.lean",
        route_name: "pay",
        checks: &[LeanAuditCheck::RouteMustContainAll {
            required: &[
                "Cambrian.Generated.Dispatch.Payee.deposit",
                "Cambrian.WorldState.transfer",
            ],
        }],
        inject_unresolved_cross_send: false,
        skip_aggregate: false,
    },
    LeanAuditCase {
        id: "T-LEAN-007",
        hypothesis: "LEAN-H9",
        project_yaml: "lean_h9_runtrace_continue.yaml",
        routes_file: "Cambrian/Generated/TraceContSpec.lean",
        route_name: "runTrace",
        checks: &[
            LeanAuditCheck::RouteMustContainAll {
                required: &[
                    "match step w₀ inst ctx a with",
                    "runTrace w' inst ctx rest",
                ],
            },
            LeanAuditCheck::RouteMustNotContain {
                forbidden: "| .error e => .error e",
            },
        ],
        inject_unresolved_cross_send: false,
        skip_aggregate: false,
    },
    LeanAuditCase {
        id: "T-LEAN-008",
        hypothesis: "LEAN-H10",
        project_yaml: "lean_h10_route_fail_mode.yaml",
        routes_file: "Cambrian/Generated/FailSurfaceRoutes.lean",
        route_name: "invoke",
        checks: &[
            LeanAuditCheck::RouteMustContainAll {
                required: &["Cambrian.RouteResult"],
            },
            LeanAuditCheck::RouteMustNotContain {
                forbidden: "Cambrian.exceptGetD",
            },
        ],
        inject_unresolved_cross_send: false,
        skip_aggregate: false,
    },
    LeanAuditCase {
        id: "T-LEAN-009",
        hypothesis: "LEAN-H11",
        project_yaml: "lean_h11_inst_address.yaml",
        routes_file: "Cambrian/Generated/PairRoutes.lean",
        route_name: "getAddr",
        checks: &[],
        inject_unresolved_cross_send: false,
        skip_aggregate: true,
    },
];

fn audit_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit")
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-lean-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ))
}

fn lean_build_enabled() -> bool {
    std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() == Ok("1")
}

fn has_lake() -> bool {
    Command::new("lake")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn write_codegen_files(
    files: &HashMap<String, String>,
    out_dir: &Path,
) -> Result<(), String> {
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

fn run_lake_on_files(files: &HashMap<String, String>) -> Result<bool, String> {
    if !lean_build_enabled() {
        return Ok(false);
    }
    if !has_lake() {
        return Err("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH".into());
    }
    let out_dir = unique_out_dir("lake");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("mkdir: {e}"))?;
    write_codegen_files(files, &out_dir)?;
    let lake = Command::new("lake")
        .arg("build")
        .current_dir(&out_dir)
        .output()
        .map_err(|e| format!("invoke lake: {e}"))?;
    let ok = lake.status.success();
    let _ = std::fs::remove_dir_all(&out_dir);
    Ok(ok)
}

/// LEAN-H11: `inst.address` must lower to `<E>.address inst` (or fail-closed).
fn evaluate_inst_address_accessor(route_body: &str, lake_ran: bool, lake_ok: bool) -> (bool, String) {
    if route_body.contains("Pair.address inst") {
        let lake_line = if lake_ran {
            if lake_ok { "PASS" } else { "FAIL" }
        } else {
            "SKIPPED"
        };
        return (
            true,
            format!("FIXED: `Pair.address inst` lowering present; lake build {lake_line}"),
        );
    }
    if route_body.contains("Cambrian.Unsupported") {
        return (
            true,
            "TRACKED: explicit `Cambrian.Unsupported` fail-closed".into(),
        );
    }
    let bad_accessor = route_body.contains("(inst).address");
    if bad_accessor {
        if lake_ran && lake_ok {
            return (
                false,
                "CONFIRMED: ill-typed `(inst).address` without `Unsupported`; `lake build` PASS"
                    .into(),
            );
        }
        let lake_line = if lake_ran {
            if lake_ok { "PASS" } else { "FAIL" }
        } else {
            "SKIPPED"
        };
        return (
            false,
            format!(
                "CONFIRMED: ill-typed `(inst).address`; no `Unsupported`; lake build {lake_line}"
            ),
        );
    }
    (
        false,
        "INCONCLUSIVE: expected `inst.address` accessor lowering in route body".into(),
    )
}

fn inject_unresolved_cross_send(project: &mut Project) {
    let entity = project
        .merged
        .entities
        .iter_mut()
        .find(|e| e.name == "Sender")
        .expect("Sender entity");
    let route = entity
        .routes
        .iter_mut()
        .find(|r| r.name == "ping")
        .expect("ping route");
    let RouteBody::Unphased(actions) = &mut route.body else {
        panic!("ping must be unphased");
    };
    for action in actions.iter_mut() {
        if let RouteAction::Send {
            message: Some(msg),
            ..
        } = action
        {
            *msg = "ghostRoute".to_string();
        }
    }
}

fn transpile_audit_project(
    yaml_path: &Path,
    case: &LeanAuditCase,
) -> Result<HashMap<String, String>, String> {
    let mut project = Project::load(yaml_path).map_err(|e| format!("load project: {e}"))?;
    if case.inject_unresolved_cross_send {
        inject_unresolved_cross_send(&mut project);
    }
    let backend = LeanBackend::default();
    Ok(backend.gen_project(&project).into_iter().collect())
}

fn transpile_evm_audit_project(yaml_path: &Path) -> Result<Vec<(String, String)>, String> {
    let project = Project::load(yaml_path).map_err(|e| format!("load project: {e}"))?;
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    Ok(backend.gen_project(&project))
}

fn extract_route_body(lean: &str, route_name: &str) -> Option<String> {
    let needle = format!("def {route_name} ");
    let start = lean.find(&needle)?;
    let rest = &lean[start..];
    let end = rest.find("\n\n").unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn run_check(route_body: &str, check: &LeanAuditCheck) -> Option<String> {
    match check {
        LeanAuditCheck::RouteMustNotContain { forbidden } => {
            if route_body.contains(forbidden) {
                Some(format!("route body contains forbidden `{}`", forbidden))
            } else {
                None
            }
        }
        LeanAuditCheck::NoDeadCodeAfter {
            terminator,
            forbidden,
        } => {
            let Some((_, tail)) = route_body.split_once(terminator) else {
                return Some(format!("terminator `{}` not found in route body", terminator));
            };
            if tail.contains(forbidden) {
                Some(format!(
                    "found `{}` after terminator `{}`",
                    forbidden, terminator
                ))
            } else {
                None
            }
        }
        LeanAuditCheck::RouteMustContainAll { required } => {
            for needle in *required {
                if !route_body.contains(needle) {
                    return Some(format!("route body missing required `{}`", needle));
                }
            }
            None
        }
    }
}

struct LeanRun {
    ok: bool,
    detail: String,
}

fn run_audit_case(case: &LeanAuditCase) -> LeanRun {
    let yaml_path = audit_root().join("fixtures").join(case.project_yaml);
    let files = match transpile_audit_project(&yaml_path, case) {
        Ok(f) => f,
        Err(err) => {
            return LeanRun {
                ok: false,
                detail: err,
            };
        }
    };
    let lean = match files.get(case.routes_file) {
        Some(s) => s,
        None => {
            return LeanRun {
                ok: false,
                detail: format!("missing generated file {}", case.routes_file),
            };
        }
    };

    let route_body = match extract_route_body(lean, case.route_name) {
        Some(b) => b,
        None => {
            return LeanRun {
                ok: false,
                detail: format!("route `{}` not found in {}", case.route_name, case.routes_file),
            };
        }
    };

    let mut failures = Vec::new();
    for check in case.checks {
        if let Some(msg) = run_check(&route_body, check) {
            failures.push(msg);
        }
    }

    if failures.is_empty() {
        LeanRun {
            ok: true,
            detail: format!("route `{}` passed all checks", case.route_name),
        }
    } else {
        LeanRun {
            ok: false,
            detail: format!(
                "route `{}` failed {} check(s):\n{}\n\n{}",
                case.route_name,
                failures.len(),
                failures.join("\n"),
                route_body
            ),
        }
    }
}

fn run_all_audit_cases() -> (String, Vec<String>) {
    let mut lines = Vec::new();
    let mut confirmed = Vec::new();
    for case in CASES {
        if case.skip_aggregate {
            continue;
        }
        let run = run_audit_case(case);
        let status = if run.ok {
            "PASS (hypothesis disproven)"
        } else {
            "FAIL (hypothesis confirmed — bug or spec mismatch)"
        };
        lines.push(format!(
            "{} / {}: {}\n{}",
            case.id, case.hypothesis, status, run.detail
        ));
        if !run.ok {
            confirmed.push(format!(
                "{} {}: CONFIRMED\n{}",
                case.id, case.hypothesis, run.detail
            ));
        }
    }
    (lines.join("\n---\n"), confirmed)
}

#[test]
fn audit_lean_hypotheses_report() {
    let (report, confirmed) = run_all_audit_cases();
    eprintln!("=== Lean audit hypothesis report ===\n{report}");

    if !confirmed.is_empty() {
        panic!(
            "Lean audit: {} confirmed hypothesis(es):\n\n{}",
            confirmed.len(),
            confirmed.join("\n---\n")
        );
    }
}

#[test]
fn audit_lean_h1_return_shortcircuit() {
    let case = &CASES[0];
    let run = run_audit_case(case);
    assert!(
        run.ok,
        "{} {} failed (confirmed bug):\n{}",
        case.id, case.hypothesis, run.detail
    );
}

#[test]
fn audit_lean_h2_classify_fail_mode() {
    let case = &CASES[1];
    let run = run_audit_case(case);
    assert!(
        run.ok,
        "{} {} failed (confirmed bug):\n{}",
        case.id, case.hypothesis, run.detail
    );
}

#[test]
fn audit_lean_h4_typed_send_value() {
    let case = &CASES[2];
    let run = run_audit_case(case);
    assert!(
        run.ok,
        "{} {} failed (confirmed bug):\n{}",
        case.id, case.hypothesis, run.detail
    );
    // B-2 regression: valued typed send must also `lake build` (string
    // needles alone previously marked LEAN-H4 FIXED while Lean rejected
    // the World vs World×α wrap).
    let yaml_path = audit_root().join("fixtures").join(case.project_yaml);
    let files = transpile_audit_project(&yaml_path, case).expect("lean transpile");
    match run_lake_on_files(&files) {
        Ok(true) => {}
        Ok(false) => eprintln!("skipping lean_h4 lake build (CAMBRIAN_TEST_LEAN_BUILD unset)"),
        Err(e) => panic!("lean_h4 lake build failed: {e}"),
    }
}

#[test]
fn audit_lean_h6_silent_cross_send() {
    let case = &CASES[3];
    let run = run_audit_case(case);
    assert!(
        run.ok,
        "{} {} failed (confirmed bug):\n{}",
        case.id, case.hypothesis, run.detail
    );
}

#[test]
fn audit_lean_t_lean_005_dynamic_dispatch_balance() {
    let case = &CASES[4];
    let run = run_audit_case(case);
    assert!(
        run.ok,
        "{} {} failed (confirmed bug):\n{}",
        case.id, case.hypothesis, run.detail
    );
}

#[test]
fn audit_lean_t_lean_007_runtrace_continue() {
    let case = &CASES[5];
    let evm_yaml = audit_root()
        .join("fixtures")
        .join("lean_h9_runtrace_continue_evm.yaml");
    let evm_files = transpile_evm_audit_project(&evm_yaml).expect("evm transpile");
    let handler = evm_files
        .iter()
        .find(|(path, _)| path.contains("trace_continues_after_revert"))
        .map(|(_, content)| content.as_str())
        .expect("EVM invariant handler");
    assert!(
        handler.contains("try _traceCont.fail(amount) {} catch { return; }"),
        "EVM lowering should wrap fallible action in try/catch when fail_on_revert=false:\n{handler}"
    );

    let run = run_audit_case(case);
    assert!(
        run.ok,
        "{} {} failed (confirmed bug):\n{}",
        case.id, case.hypothesis, run.detail
    );
}

#[test]
fn audit_lean_t_lean_008_route_fail_mode_nested_call() {
    let case = &CASES[6];
    let yaml_path = audit_root().join("fixtures").join(case.project_yaml);
    let files = transpile_audit_project(&yaml_path, case).expect("lean transpile");
    let spec = files
        .get("Cambrian/Generated/FailSurfaceSpec.lean")
        .expect("FailSurfaceSpec.lean");
    assert!(
        !spec.contains(".ok (FailSurface.Routes.invoke"),
        "invariant `step` must not force `.ok` on a wrapper route whose nested callee can throw:\n{spec}"
    );

    let run = run_audit_case(case);
    assert!(
        run.ok,
        "{} {} failed (confirmed bug):\n{}",
        case.id, case.hypothesis, run.detail
    );
}

#[test]
fn audit_lean_t_lean_009_inst_address_accessor() {
    let case = CASES
        .iter()
        .find(|c| c.id == "T-LEAN-009")
        .expect("T-LEAN-009 case");
    let yaml_path = audit_root().join("fixtures").join(case.project_yaml);
    let files = transpile_audit_project(&yaml_path, case).expect("lean transpile");
    let lean = files
        .get(case.routes_file)
        .expect("generated routes file");
    let route_body = extract_route_body(lean, case.route_name)
        .expect("getAddr route body");

    let lake_ran = lean_build_enabled();
    let lake_ok = if lake_ran {
        run_lake_on_files(&files).expect("lake build")
    } else {
        false
    };

    let (ok, status) = evaluate_inst_address_accessor(&route_body, lake_ran, lake_ok);
    eprintln!(
        "{} {}: {}\n\nroute `getAddr`:\n{}",
        case.id, case.hypothesis, status, route_body
    );
    assert!(
        ok,
        "{} {} failed:\n{}\n\n{}",
        case.id, case.hypothesis, status, route_body
    );
}

/// LEAN-H12: markers that depend on per-project thread-local flags.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LeanThreadLocalMarkers {
    proof_helpers_on: bool,
    nat_numerics: bool,
    member_type_line: String,
}

fn transpile_fixture_yaml(yaml_name: &str) -> Result<HashMap<String, String>, String> {
    let yaml_path = audit_root().join("fixtures").join(yaml_name);
    let project = Project::load(&yaml_path).map_err(|e| format!("load {yaml_name}: {e}"))?;
    Ok(LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .collect())
}

fn lean_thread_local_markers(files: &HashMap<String, String>, entity: &str) -> LeanThreadLocalMarkers {
    let spec = files
        .get(&format!("Cambrian/Generated/{entity}Spec.lean"))
        .map(|s| s.as_str())
        .unwrap_or("");
    let routes = files
        .get(&format!("Cambrian/Generated/{entity}Routes.lean"))
        .map(|s| s.as_str())
        .unwrap_or("");
    let entity_src = files
        .get(&format!("Cambrian/Generated/{entity}.lean"))
        .map(|s| s.as_str())
        .unwrap_or("");
    let member_type_line = entity_src
        .lines()
        .find(|l| l.contains("m_count"))
        .unwrap_or("<missing m_count line>")
        .trim()
        .to_string();
    LeanThreadLocalMarkers {
        proof_helpers_on: spec.contains("invByCases")
            && routes.contains("attribute [cambrian_route_simp]"),
        nat_numerics: member_type_line.contains(": Nat"),
        member_type_line,
    }
}

fn format_marker_diff(baseline: &LeanThreadLocalMarkers, after: &LeanThreadLocalMarkers) -> String {
    format!(
        "baseline (B alone): {baseline:?}\nafter A→B:          {after:?}"
    )
}

#[test]
fn audit_lean_t_lean_010_thread_local_no_leak() {
    const ID: &str = "T-LEAN-010";
    const HYP: &str = "LEAN-H12";

    let b_alone = transpile_fixture_yaml("lean_h12_tl_b.yaml").expect("transpile B alone");
    let baseline = lean_thread_local_markers(&b_alone, "TlLeakB");
    assert!(
        baseline.proof_helpers_on,
        "{ID} baseline must emit proof helpers (invByCases + cambrian_route_simp); got {baseline:?}"
    );
    assert!(
        !baseline.nat_numerics,
        "{ID} baseline must use BitVec numerics, not Nat; got {baseline:?}"
    );

    let _a = transpile_fixture_yaml("lean_h12_tl_a.yaml").expect("transpile A (prime thread-locals)");
    let b_after_a = transpile_fixture_yaml("lean_h12_tl_b.yaml").expect("transpile B after A");
    let after = lean_thread_local_markers(&b_after_a, "TlLeakB");

    let diff = format_marker_diff(&baseline, &after);
    eprintln!(
        "{ID} {HYP}: sequential A(proof_helpers:false,numerics:nat) → B(defaults)\n{diff}"
    );

    if after != baseline {
        panic!(
            "{ID} {HYP} CONFIRMED — thread-local flags from A leaked into B:\n{diff}\n\n\
             B-alone member: {}\nB-after-A member: {}",
            baseline.member_type_line, after.member_type_line
        );
    }

    eprintln!("{ID} {HYP}: DISPROVEN — B-after-A matches B-alone negative control");
}
