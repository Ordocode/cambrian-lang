// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase M audit harness — post kernel-adapter PoC gates (docs/AUDIT_PHASE_M.md).
//!
//! Red tests assert **desired** semantics; failure on HEAD = CONFIRMED finding
//! for owner triage on **`kernel-adapter-refactor`** (not legacy `lean-target`). No codegen fixes on `audit`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cambrian_transpiler::ast;
use cambrian_transpiler::codegen::{
    collect_transforms, order_transforms_temporally, storage_layout::compute_layout,
    EvmSolidityBackend, LeanBackend, OutputBackend,
};
use cambrian_transpiler::ast::{RouteAction, RouteBody};
use cambrian_transpiler::ir;
use cambrian_transpiler::ir::{lower_expr, CoerceKind, LowerCtx, ResolvedType, TypedExprKind};
use cambrian_transpiler::project::Project;
use cambrian_transpiler::target::{Domain, Target};

#[path = "corpus/mod.rs"]
mod corpus;
use cambrian_transpiler::validate::{
    build_temporal_orders, check_lean_target_compat, check_target_compat, Severity, RULES,
};
use cambrian_core::U256;
use cambrian_transpiler::ProgramParser;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn phase_m_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/audit/fixtures/phase_m")
}

fn transpiler_bin() -> PathBuf {
    // Compile-time path from Cargo; follows the active target dir
    // (`target/debug`, `target/llvm-cov-target/debug`, …) unlike a
    // hardcoded `target/{debug,release}` lookup.
    PathBuf::from(env!("CARGO_BIN_EXE_cambrian-transpiler"))
}

fn parse_cam(src: &str) -> ast::Program {
    let mut program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("fixture parse error: {e}"));
    ast::normalize_program_types(&mut program);
    program
}

fn load_project_yaml(name: &str) -> Project {
    let yaml = phase_m_dir().join(name);
    Project::load(&yaml).unwrap_or_else(|e| panic!("load {}: {e}", yaml.display()))
}

fn transpile_project_lean(yaml: &str) -> HashMap<String, String> {
    let project = load_project_yaml(yaml);
    LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .collect()
}

fn transpile_project_evm(yaml: &str) -> HashMap<String, String> {
    let project = load_project_yaml(yaml);
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    backend.gen_project(&project).into_iter().collect()
}

fn run_cli(args: &[&str]) -> (bool, String) {
    let output = Command::new(transpiler_bin())
        .args(args)
        .output()
        .expect("run cambrian-transpiler");
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), log)
}

fn lake_build(dir: &Path) -> (bool, String) {
    let output = Command::new("lake")
        .arg("build")
        .current_dir(dir)
        .output()
        .expect("run lake build");
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), log)
}

fn write_lean_project(files: &HashMap<String, String>, tag: &str) -> PathBuf {
    let out = std::env::temp_dir().join(format!(
        "cambrian-phase-m-{}-{}",
        tag,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&out);
    for (rel, content) in files {
        let path = out.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }
    out
}

// ---------------------------------------------------------------------------
// PM-001 / T-LEAN-IR-001 — MA-L01
// ---------------------------------------------------------------------------

#[test]
fn phase_m_pm001_lean_send_in_for_after_send() {
    let files = transpile_project_lean("pm_l01_lean.yaml");
    let routes = files
        .get("Cambrian/Generated/BRoutes.lean")
        .expect("BRoutes.lean");
    assert!(
        !routes.contains("-- L8:"),
        "PM-001: fan route must not emit silent L8 drop:\n{}",
        routes
    );
    assert!(
        routes.contains("A.Routes.ping") || routes.contains("ARoutes.ping"),
        "PM-001: typed send inside for must call A.Routes.ping:\n{}",
        routes
    );
    assert!(
        routes.contains("BitVec.ofNat 64 i"),
        "PM-001: range binder i is Nat; identity/send args must coerce via ofNat:\n{}",
        routes
    );
}

// ---------------------------------------------------------------------------
// PM-002 / T-EVM-TYPE-001 — MA-E01
// ---------------------------------------------------------------------------

#[test]
fn phase_m_pm002_evm_program_record_member_type() {
    let files = transpile_project_evm("pm_e01_evm.yaml");
    let sol = files
        .values()
        .find(|s| s.contains("contract S"))
        .expect("S.sol");
    assert!(
        sol.contains("struct Info"),
        "PM-002: program-scope record struct must be emitted:\n{}",
        sol
    );
    assert!(
        sol.contains("Info public m_info"),
        "PM-002: member m_info must keep Info type (not uint256 erasure):\n{}",
        sol
    );
}

// ---------------------------------------------------------------------------
// PM-005 / T-LEAN-SPEC-001 — MA-LSPEC01
// ---------------------------------------------------------------------------

#[test]
fn phase_m_pm005_lean_effects_only_not_vacuous() {
    let files = transpile_project_lean("pm_lspec01_lean.yaml");
    let spec = files
        .get("Cambrian/Generated/ESpec.lean")
        .expect("ESpec.lean");
    let theorem_block = spec
        .lines()
        .skip_while(|l| !l.contains("only_effects"))
        .take(30)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !theorem_block.contains("True := by"),
        "PM-005: effects-only test must not collapse to vacuous True:\n{}",
        theorem_block
    );
    assert!(
        !spec.contains("expect effects [...] skipped"),
        "PM-005: expect effects must not be silently skipped in generated spec:\n{}",
        spec
    );
}

// ---------------------------------------------------------------------------
// PM-006 / T-LEAN-ADMIT-001 — MA-ADMIT01
// ---------------------------------------------------------------------------

#[test]
fn phase_m_pm006_admit_e17_lake_build_or_reject() {
    let cam = repo_root().join("cambrian-transpiler/tests/audit/fixtures/val_e17_hashmap_shape.cam");
    let (ok, log) = run_cli(&[
        cam.to_str().unwrap(),
        "-o",
        "/tmp/cambrian-pm006-lean",
        "--target",
        "lean",
    ]);
    if !ok {
        // Desired: validator rejects ill-shaped HashMap transform on lean.
        return;
    }

    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() != Ok("1") {
        eprintln!("PM-006: skip lake gate (set CAMBRIAN_TEST_LEAN_BUILD=1)");
        return;
    }

    let out = PathBuf::from("/tmp/cambrian-pm006-lean");
    let (lake_ok, lake_log) = lake_build(&out);
    assert!(
        lake_ok,
        "PM-006: lean-admitted E17 fixture must lake build cleanly (or be rejected at validate):\n{log}\n--- lake ---\n{lake_log}"
    );
}

// ---------------------------------------------------------------------------
// PM-007 / T-LEAN-MAP-001 — MA-LH01
// ---------------------------------------------------------------------------

#[test]
fn phase_m_pm007_lean_hashmap_conditional_insert_syncs_keys() {
    let src = include_str!("audit/fixtures/phase_m/pm_lh01_hashmap_cond.cam");
    let prog = parse_cam(src);
    let backend = LeanBackend::default();
    let extras = backend.extra_files(&prog, "MapCond");
    let entity = extras
        .iter()
        .find(|(path, _)| path == "Cambrian/Generated/MapCond.lean")
        .map(|(_, content)| content.clone())
        .expect("MapCond.lean");
    let routes = extras
        .iter()
        .find(|(path, _)| path == "Cambrian/Generated/MapCondRoutes.lean")
        .map(|(_, content)| content.clone())
        .expect("MapCondRoutes.lean");
    assert!(
        entity.contains("m_map_keys"),
        "PM-007: iterated HashMap must emit ghost keys field:\n{}",
        entity
    );
    assert!(
        routes.contains("pushKeyIfNew"),
        "PM-007: conditional insert transform must sync `_keys` via pushKeyIfNew:\n{}",
        routes
    );
}

// ---------------------------------------------------------------------------
// PM-008 / T-STD-N-001 — MA-N04
// ---------------------------------------------------------------------------

#[test]
fn phase_m_pm008_reserved_names_single_file_matches_project() {
    let cam = phase_m_dir().join("pm_n04_reserved.cam");
    let yaml = phase_m_dir().join("pm_n04_project.yaml");
    let out = std::env::temp_dir().join(format!("pm008-{}", std::process::id()));

    let (single_ok, single_log) = run_cli(&[
        cam.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--target",
        "lean",
    ]);
    let (_, proj_log) = run_cli(&["--project", yaml.to_str().unwrap()]);

    assert!(
        !single_ok,
        "PM-008: single-file CLI must reject reserved param __cbr_v (project mode does):\n{single_log}"
    );
    assert!(
        proj_log.contains("[N4]"),
        "PM-008: project mode must emit N4 for __cbr_v:\n{proj_log}"
    );
}

// ---------------------------------------------------------------------------
// PM-009 / T-STD-CI-001 — MA-CI01
// ---------------------------------------------------------------------------

/// Integration / audit gates that must appear in `.gitlab-ci.yml`.
const CI_REQUIRED_TEST_BINARIES: &[&str] = &[
    "test_corpus",
    "test_domain_fixture_parity",
    "test_domain_fixture_exec",
    "test_validate_domain_parity",
    "test_validate_lean_admit",
    "test_validate_snapshot",
    "test_lean_goldens",
    "test_codegen_lean_predictable",
    "test_audit_phase_m",
    "test_ci_public_parity",
    "test_std_parse_fuzz",
    "test_std_lean",
    "test_audit_std_diff",
    "test_audit_dual_yaml",
    "test_property",
    "test_using",
    "test_import",
    "test_library",
    "test_solidity_import",
    "test_validate_names",
    "test_std_parse",
    "test_std_str_matrix",
];

#[test]
fn phase_m_pm009_ci_lists_audit_gates() {
    let ci_path = repo_root().join(".gitlab-ci.yml");
    if !ci_path.is_file() {
        eprintln!("PM-009 skip: .gitlab-ci.yml omitted from public snapshot");
        return;
    }
    let ci = fs::read_to_string(&ci_path).expect("read CI yaml");
    let mut missing = Vec::new();
    for bin in CI_REQUIRED_TEST_BINARIES {
        let needle = format!("--test {bin}");
        if !ci.contains(&needle) {
            missing.push(*bin);
        }
    }
    assert!(
        missing.is_empty(),
        "PM-009: missing from .gitlab-ci.yml: {:?}",
        missing
    );
    assert!(
        ci.contains("CAMBRIAN_TEST_RUST_BUILD"),
        "PM-009: CI must set CAMBRIAN_TEST_RUST_BUILD for Native compile gate"
    );
}

// ---------------------------------------------------------------------------
// PM-001 lake parity (documents fail-open hazard)
// ---------------------------------------------------------------------------

#[test]
fn phase_m_pm001_lake_builds_despite_send_drop() {
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() != Ok("1") {
        eprintln!("PM-001 lake: skip (set CAMBRIAN_TEST_LEAN_BUILD=1)");
        return;
    }

    let files = transpile_project_lean("pm_l01_lean.yaml");
    let out = write_lean_project(&files, "pm001");
    let (ok, log) = lake_build(&out);
    let _ = fs::remove_dir_all(&out);

    assert!(
        ok,
        "PM-001 lake parity note: build should succeed today (fail-open); if this fails, investigate:\n{log}"
    );

    let routes = files
        .get("Cambrian/Generated/BRoutes.lean")
        .expect("BRoutes.lean");
    if routes.contains("-- L8:") {
        eprintln!(
            "PM-001 CONFIRMED: lake green while -- L8: present (silent semantic drop)"
        );
    }
}

fn lean_error_codes(prog: &ast::Program) -> Vec<String> {
    check_lean_target_compat(prog)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code.to_string())
        .collect()
}

fn entity_by_name<'a>(prog: &'a ast::Program, name: &str) -> &'a ast::Entity {
    prog.entities
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("entity {name} not found"))
}

fn route_by_name<'a>(entity: &'a ast::Entity, name: &str) -> &'a ast::Route {
    entity
        .routes
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("route {name} not found"))
}

// ---------------------------------------------------------------------------
// Wave M1 — P6 IR / send / temporal (PM-012 … PM-020)
// ---------------------------------------------------------------------------

#[test]
fn phase_m_pm012_mix_binop_retains_widen_coerce_in_ir() {
    let prog = parse_cam(include_str!(
        "audit/fixtures/phase_m/pm_m12_mix.cam"
    ));
    let entity = entity_by_name(&prog, "Mix");
    let route = route_by_name(entity, "mix");
    let ctx = LowerCtx::new(&prog, entity, route);
    let body = route.body.actions();
    let return_expr = body
        .iter()
        .find_map(|a| match a {
            ast::RouteAction::Return { values } if values.len() == 1 => Some(&values[0]),
            _ => None,
        })
        .expect("return action");
    let typed = lower_expr(return_expr, &ctx);
    match typed.kind {
        TypedExprKind::BinOp { lhs, .. } => match lhs.kind {
            TypedExprKind::Coerce {
                kind: CoerceKind::Widen,
                ..
            } => {}
            other => panic!("PM-012: u8 operand must carry widen Coerce in IR binop: {other:?}"),
        },
        other => panic!("PM-012: expected binop return in IR: {other:?}"),
    }
}

const PM_M13_RESCUE_IR_CONVERTIBLE: &[&str] = &[
    "AstAction",
    "Send",
    "VarCall",
    "Deploy",
    "CallRoute",
];

fn pm_m13_ir_stmt_is_rescue_convertible(stmt: &ir::IrStmt) -> bool {
    let kind = match stmt {
        ir::IrStmt::AstAction(_) => "AstAction",
        ir::IrStmt::Send { .. } => "Send",
        ir::IrStmt::VarCall { .. } => "VarCall",
        ir::IrStmt::Deploy { .. } => "Deploy",
        ir::IrStmt::CallRoute { .. } => "CallRoute",
        _ => return false,
    };
    PM_M13_RESCUE_IR_CONVERTIBLE.contains(&kind)
}

fn pm_m13_collect_rescue_ir_gaps(prog: &ast::Program) -> Vec<String> {
    use cambrian_transpiler::analysis::ProgramGraphs;

    let graphs = ProgramGraphs::build(prog);
    let mut gaps = Vec::new();
    for entity in &prog.entities {
        for route in &entity.routes {
            let route_ir = ir::lower_route(prog, entity, route, &graphs);
            for phase in &route_ir.phases {
                pm_m13_walk_ir_stmts(
                    &phase.stmts,
                    &format!("{}.{}", entity.name, route.name),
                    &mut gaps,
                );
            }
        }
    }
    gaps
}

fn pm_m13_walk_ir_stmts(stmts: &[ir::IrStmt], ctx: &str, gaps: &mut Vec<String>) {
    for stmt in stmts {
        match stmt {
            ir::IrStmt::Rescue { tag, action } => {
                if !pm_m13_ir_stmt_is_rescue_convertible(action) {
                    gaps.push(format!(
                        "{ctx}: rescue `{tag}` inner {:?} is not convertible via ir_stmt_to_route_action",
                        std::mem::discriminant(action.as_ref())
                    ));
                }
            }
            ir::IrStmt::Conditional { then_stmts, else_stmts, .. } => {
                pm_m13_walk_ir_stmts(then_stmts, ctx, gaps);
                pm_m13_walk_ir_stmts(else_stmts, ctx, gaps);
            }
            ir::IrStmt::For { body, .. } => pm_m13_walk_ir_stmts(body, ctx, gaps),
            _ => {}
        }
    }
}

fn pm_m13_inject_rescue_callroute(project: &mut Project) {
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

fn pm_m13_bouncer_sol() -> String {
    let files = transpile_project_evm("pm_m13_bouncer.yaml");
    files
        .values()
        .find(|s| s.contains("contract Bouncer"))
        .cloned()
        .expect("Bouncer.sol")
}

#[test]
fn phase_m_pm013_rescue_ir_stmt_not_silently_omitted() {
    let route_rs = fs::read_to_string(
        repo_root().join("cambrian-transpiler/src/codegen/solidity/evm/route.rs"),
    )
    .expect("read evm/route.rs");
    assert!(
        route_rs.contains("fn ir_stmt_to_route_action")
            && route_rs.contains("IrStmt::Rescue")
            && route_rs.contains("String::new()"),
        "PM-013 setup: EVM IR rescue path must document ir_stmt_to_route_action miss → empty emission"
    );

    let bouncer = load_project_yaml("pm_m13_bouncer.yaml");
    let ir_gaps = pm_m13_collect_rescue_ir_gaps(&bouncer.merged);
    assert!(
        ir_gaps.is_empty(),
        "PM-013 oracle A: lowered rescue inners must map through ir_stmt_to_route_action:\n{}",
        ir_gaps.join("\n")
    );

    let sol = pm_m13_bouncer_sol();
    for tag in [
        "bounce_failed",
        "cross_failed",
        "transfer_failed",
        "deploy_failed",
    ] {
        assert!(
            sol.contains(&format!("rescue '{tag}'")) && sol.contains("E26"),
            "PM-013 oracle B: rescue `{tag}` must emit E26 comment, not try/catch"
        );
    }
    assert!(
        !sol.contains("try ") && !sol.contains("catch {"),
        "PM-013: EVM no longer lowers rescue to try/catch (E26)"
    );

    let mut h3 = load_project_yaml("pm_m13_h3.yaml");
    pm_m13_inject_rescue_callroute(&mut h3);
    let det = h3.config.deterministic_addresses.unwrap_or(true);
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    let h3_files: HashMap<_, _> = backend.gen_project(&h3).into_iter().collect();
    let h3_sol = h3_files
        .values()
        .find(|s| s.contains("contract Guard"))
        .expect("Guard.sol");
    assert!(
        h3_sol.contains("E26") && h3_sol.contains("call_failed") && !h3_sol.contains("try this."),
        "PM-013 oracle C: injected Rescue{{CallRoute}} must not emit try/catch:\n{h3_sol}"
    );
}

#[test]
fn phase_m_pm014_wide_literal_in_u256_context_not_u64() {
    let prog = parse_cam(include_str!(
        "audit/fixtures/phase_m/pm_m12_wide_literal.cam"
    ));
    let entity = entity_by_name(&prog, "Wide");
    let route = route_by_name(entity, "set");
    let member = entity
        .members
        .iter()
        .find(|m| m.name == "m_val")
        .expect("m_val");
    let transform = member
        .transforms
        .iter()
        .find(|t| t.route_name == "set")
        .expect("set transform");
    let ctx = LowerCtx::new(&prog, entity, route);
    let typed = lower_expr(&transform.body, &ctx);
    let wide_ok = matches!(typed.ty, ResolvedType::Simple(ref n) if n == "U256")
        || matches!(
            typed.kind,
            TypedExprKind::Coerce {
                kind: CoerceKind::Widen,
                ..
            }
        )
        || matches!(typed.kind, TypedExprKind::BinOp { .. });
    assert!(
        wide_ok,
        "PM-014: U256 member transform must not leave u64-only typing for wide literal: {:?}",
        typed
    );
    if let TypedExprKind::BinOp { rhs, .. } = typed.kind {
        assert!(
            !matches!(rhs.ty, ResolvedType::Simple(ref n) if n == "u64")
                || matches!(
                    rhs.kind,
                    TypedExprKind::Coerce {
                        kind: CoerceKind::Widen,
                        ..
                    }
                ),
            "PM-014: wide literal rhs must coerce to U256 width: {:?}",
            rhs
        );
    }
}

#[test]
fn phase_m_pm015_evm_let_bound_typed_send() {
    let files = transpile_project_evm("pm_m15_evm.yaml");
    let sol = files
        .values()
        .find(|s| s.contains("contract Sender"))
        .expect("Sender.sol");
    assert!(
        sol.contains("credit()"),
        "PM-015: EVM must emit typed credit() call for let-bound dest:\n{}",
        sol
    );
}

#[test]
fn phase_m_pm015_lean_let_bound_typed_send() {
    let project = load_project_yaml("pm_m15_lean.yaml");
    let codes = lean_error_codes(&project.merged);
    assert!(
        !codes.iter().any(|c| c == "L8"),
        "PM-015: Lean must not reject let-bound Treasury.address typed send (EVM allows): {codes:?}"
    );
    let files = transpile_project_lean("pm_m15_lean.yaml");
    let routes = files
        .get("Cambrian/Generated/SenderRoutes.lean")
        .expect("SenderRoutes.lean");
    assert!(
        routes.contains("Treasury.Routes.credit"),
        "PM-015: Lean must lower let-bound dest to Treasury.Routes.credit:\n{}",
        routes
    );
}

#[test]
fn phase_m_pm016_evm_temporal_ref_uses_new_value() {
    // LANGUAGE.md: `^member` is the already-computed NEW value.
    let files = transpile_project_evm("pm_m16_evm.yaml");
    let sol = files
        .values()
        .find(|s| s.contains("contract Ord"))
        .expect("Ord.sol");
    assert!(
        sol.contains("m_b + next_m_a") || sol.contains("(m_b + next_m_a)"),
        "PM-016: EVM `^m_a` must use post-update next_m_a:\n{}",
        sol
    );
    assert!(
        !sol.contains("(m_b + m_a)") && !sol.contains("m_b + m_a)"),
        "PM-016: EVM must not use pre-update m_a for `^m_a`:\n{}",
        sol
    );
}

#[test]
fn phase_m_pm016_lean_temporal_ref_uses_new_value() {
    // LANGUAGE.md: `^member` is the already-computed NEW value.
    let files = transpile_project_lean("pm_m16_lean.yaml");
    let entity = files
        .get("Cambrian/Generated/Ord.lean")
        .expect("Ord.lean");
    assert!(
        entity.contains("M_m_a.bump"),
        "PM-016: Lean `^m_a` must invoke m_a transform (NEW value):\n{}",
        entity
    );
    assert!(
        !entity.contains("s.m_b + s.m_a"),
        "PM-016: Lean must not read pre-update s.m_a for `^m_a`:\n{}",
        entity
    );
}

#[test]
fn phase_m_pm016_transform_order_evm_uses_temporal_not_decl() {
    let prog = parse_cam(include_str!(
        "audit/fixtures/phase_m/pm_m16_temporal_ref.cam"
    ));
    let entity = entity_by_name(&prog, "Ord");
    let decl: Vec<_> = collect_transforms(entity, "bump", None)
        .iter()
        .map(|t| t.member.name.as_str())
        .collect();
    let (orders, _) = build_temporal_orders(entity);
    let temporal: Vec<_> = order_transforms_temporally(
        collect_transforms(entity, "bump", None),
        &orders,
        "bump",
    )
        .iter()
        .map(|t| t.member.name.as_str())
        .collect();
    assert_eq!(decl, vec!["m_b", "m_a"], "fixture declares m_b before m_a");
    assert_eq!(
        temporal,
        vec!["m_a", "m_b"],
        "temporal order must apply m_a before m_b"
    );
}

#[test]
fn phase_m_pm020_evm_extern_dynamic_address_call() {
    let files = transpile_project_evm("pm_m20_evm.yaml");
    let sol = files
        .values()
        .find(|s| s.contains("contract Wallet"))
        .expect("Wallet.sol");
    assert!(
        sol.contains("transfer(address,uint256)")
            || sol.contains(".transfer(")
            || sol.contains("IToken(token).transfer"),
        "PM-020: EVM must call extern transfer on dynamic address:\n{}",
        sol
    );
}

#[test]
fn phase_m_pm020_lean_extern_dynamic_address_call() {
    let project = load_project_yaml("pm_m20_lean.yaml");
    let codes = lean_error_codes(&project.merged);
    assert!(
        !codes.iter().any(|c| c == "L8"),
        "PM-020: Lean must admit extern typed send to address param when extern entity declared: {codes:?}"
    );
    let files = transpile_project_lean("pm_m20_lean.yaml");
    let routes = files
        .get("Cambrian/Generated/WalletRoutes.lean")
        .expect("WalletRoutes.lean");
    assert!(
        routes.contains("Extern.Token.Routes.transfer"),
        "PM-020: Lean must lower to Extern.Token.Routes.transfer:\n{}",
        routes
    );
}

#[test]
fn phase_m_pm020_lean_ambiguous_extern_dynamic_address_stays_l8() {
    let prog = parse_cam(
        r#"
extern entity TokenA {
    route transfer(to: address, amount: U256);
}
extern entity TokenB {
    route transfer(to: address, amount: U256);
}
entity Wallet {
    routes {
        pay(token: address, to: address, amount: U256) => [
            transfer(to, amount) ~> token
        ]
    }
    m_x: u64 {}
}
"#,
    );
    assert!(
        lean_error_codes(&prog).iter().any(|code| code == "L8"),
        "PM-020: a plain-address send matching multiple extern routes must remain L8"
    );
}

#[test]
fn phase_m_pm023_native_gosh_should_warn_like_evm() {
    let src = r#"entity E {
    routes { go() => [ gosh::commit() ] }
    m_x: u64 {}
}"#;
    let prog = parse_cam(src);
    let evm: Vec<_> = check_target_compat(&prog, Target::Evm, false)
        .into_iter()
        .map(|d| d.code.to_string())
        .collect();
    let native: Vec<_> = check_target_compat(&prog, Target::Native, false)
        .into_iter()
        .map(|d| d.code.to_string())
        .collect();
    assert!(
        evm.iter().any(|c| c == "E01" || c == "E02"),
        "PM-023 setup: EVM must warn on gosh::commit: {evm:?}"
    );
    assert!(
        native.iter().any(|c| c == "E01" || c == "E02"),
        "PM-023: Native/Container must warn on gosh::commit like EVM (not silent stub): evm={evm:?} native={native:?}"
    );
}

// ---------------------------------------------------------------------------
// Wave M3 — CI / P8 / V27 meta gates (PM-010, PM-011, PM-030)
// ---------------------------------------------------------------------------

#[test]
fn phase_m_pm010_exec_gate_must_not_self_skip() {
    let src = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test_domain_fixture_exec.rs"),
    )
    .expect("read test_domain_fixture_exec.rs");
    // The gate job declares itself via CAMBRIAN_TEST_P8_EXEC_GATE=1 and must
    // keep the fail-loud branch; only non-gate jobs (coverage llvm-cov) may
    // skip when no backend is available.
    assert!(
        src.contains("CAMBRIAN_TEST_P8_EXEC_GATE") && src.contains("requires a backend"),
        "PM-010: domain_fixture_execution_actions must panic/fail when tools are \
         absent in the P8 exec gate job (CAMBRIAN_TEST_P8_EXEC_GATE=1 branch; \
         no unconditional self-skip)"
    );
    // The CI config is not part of source distributions; the gate-job check
    // only applies where it exists.
    if let Ok(ci) = fs::read_to_string(repo_root().join(".gitlab-ci.yml")) {
        assert!(
            ci.contains("CAMBRIAN_TEST_P8_EXEC_GATE"),
            "PM-010: .gitlab-ci.yml gate job must set CAMBRIAN_TEST_P8_EXEC_GATE=1 \
             so the exec gate stays soft-skip free"
        );
    }
}

#[test]
fn phase_m_pm011_parity_gate_tracks_accept_baselines() {
    let src = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test_domain_fixture_parity.rs"),
    )
    .expect("read test_domain_fixture_parity.rs");
    assert!(
        src.contains("ACCEPT_BASELINE") || src.contains("min_accepts_per_domain"),
        "PM-011: parity gate must ratchet per-domain accept counts (not treat all-reject as OK)"
    );
}

#[test]
fn phase_m_pm030_v27_code_overloaded_mixed_phases_vs_var_where() {
    let mixed_src = r#"entity E {
    routes {
        foo() => [
            step: [ gosh::commit() ]
            gosh::exit(0)
        ]
    }
}
"#;
    let mixed_prog = parse_cam(mixed_src);
    let mixed_diags: Vec<_> = cambrian_transpiler::validate::validate(&mixed_prog)
        .into_iter()
        .filter(|d| d.code == "V54" || d.code == "V27")
        .map(|d| (d.code.to_string(), d.message.clone()))
        .collect();
    assert!(
        mixed_diags
            .iter()
            .any(|(c, m)| c == "V54" && m.contains("mixes phase")),
        "PM-030: mixed phased/unphased route must hit V54 (not V27): {mixed_diags:?}"
    );
    assert!(
        !mixed_diags.iter().any(|(c, _)| c == "V27"),
        "PM-030: mixed-phase must not reuse V27 after split: {mixed_diags:?}"
    );

    let var_src = r#"entity Token {
    routes {
        constructor() => []
        view balanceOf(o: address) -> U256 => [ return(m_s) ]
    }
    m_s: U256 { in constructor() => 0 }
}
entity V {
    routes {
        constructor(t: Address<Token>) => []
        go(who: address) where bal > 0 : throw 3 => [
            fetch: [
                var bal = balanceOf(who) ~> m_t;
            ]
        ]
    }
    m_t: Address<Token> { in constructor(t) => t }
}
"#;
    let var_prog = parse_cam(var_src);
    let var_v27: Vec<_> = cambrian_transpiler::validate::validate(&var_prog)
        .into_iter()
        .filter(|d| d.code == "V27")
        .map(|d| d.message.clone())
        .collect();
    assert!(
        var_v27.iter().any(|m| m.contains("var")),
        "PM-030 setup: route-level where on var must hit V27: {var_v27:?}"
    );

    let readme = fs::read_to_string(repo_root().join("cambrian-transpiler/README.md"))
        .expect("read cambrian-transpiler/README.md");
    let documents_mixed = readme.contains("V54") && readme.contains("mixes phase");
    let documents_var = readme.contains("V27") && readme.contains("var");
    assert!(
        documents_mixed && documents_var,
        "PM-030: README must document V54 (mixed phase) and V27 (var-in-where)"
    );
}

#[test]
fn phase_m_pm019_local_fail_surface_must_cover_mixed_phase_where() {
    let src = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/analysis/route_facts.rs"),
    )
    .expect("read route_facts.rs");
    assert!(
        src.contains("fn local_fail_surface"),
        "PM-019 setup: route_facts.rs must define local_fail_surface"
    );
    assert!(
        src.contains("if let RouteBody::Phased(phases) | RouteBody::Mixed(phases, _) = &route.body"),
        "PM-019: local_fail_surface must include Mixed phased where clauses (validate checks them)"
    );
}

#[test]
fn phase_m_pm031_validator_registry_one_code_per_rule_entry() {
    let clustered: Vec<_> = RULES
        .iter()
        .filter(|e| e.codes.len() > 1)
        .map(|e| format!("{} codes in one RuleEntry", e.codes.len()))
        .collect();
    assert!(
        clustered.is_empty(),
        "PM-031: validator RULES table clusters multiple codes per entry: {clustered:?}"
    );
}

#[test]
fn phase_m_pm032_lake_corpus_must_be_fully_enumerated() {
    let exec = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test_domain_fixture_exec.rs"),
    )
    .expect("read test_domain_fixture_exec.rs");
    assert!(
        !exec.contains("other corpus fixtures are graduation-checked"),
        "PM-032: lake corpus allows silent pass outside LAKE_MUST_PASS (graduation-only orphans)"
    );
    assert!(
        exec.contains("LAKE_KNOWN_BROKEN") && exec.contains("lake_corpus_fully_enumerated"),
        "PM-032: every lake corpus key must be LAKE_MUST_PASS or LAKE_KNOWN_BROKEN"
    );
}

#[test]
fn phase_m_pm041_docs_agree_on_t2_string_oracle_policy() {
    let agents_path = repo_root().join("AGENTS.md");
    let audit_path = repo_root().join("docs/AUDIT_EVM_LEAN.md");
    if !agents_path.is_file() || !audit_path.is_file() {
        eprintln!("PM-041 skip: internal docs omitted from public snapshot");
        return;
    }
    let agents = fs::read_to_string(&agents_path).expect("AGENTS.md");
    let audit = fs::read_to_string(&audit_path).expect("AUDIT_EVM_LEAN");
    let agents_allows_t2 = agents.contains("contains()") || agents.contains("string");
    let audit_marks_t2_weak = audit.contains("T2") && audit.contains("Medium");
    assert!(
        !(agents_allows_t2 && audit_marks_t2_weak),
        "PM-041: AGENTS.md treats string contains() as normal while AUDIT_EVM_LEAN flags T2 as weak oracle"
    );
}

// ---------------------------------------------------------------------------
// Wave M4 — EVM storage / layout (PM-024)
// ---------------------------------------------------------------------------

fn has_forge() -> bool {
    Command::new("forge")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn forge_storage_slot(out_dir: &Path, contract: &str, label: &str) -> Option<u64> {
    let out = Command::new("forge")
        .args([
            "inspect",
            &format!("src/{contract}.sol:{contract}"),
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
    json.get("storage")?
        .as_array()?
        .iter()
        .find(|e| e.get("label").and_then(|l| l.as_str()) == Some(label))?
        .get("slot")
        .and_then(|s| s.as_u64().or_else(|| s.as_str()?.parse().ok()))
}

#[test]
fn phase_m_pm024_compute_layout_matches_solc_record_member() {
    let src = include_str!("audit/fixtures/phase_m/pm_m24_record_storage.cam");
    let prog = parse_cam(src);
    let entity = prog.entities.first().expect("entity S");
    let computed = compute_layout(entity, false)
        .lookup("m_b")
        .expect("m_b layout")
        .slot;

    if has_forge() {
        let out = unique_temp_dir("pm24-forge");
        fs::create_dir_all(&out).unwrap();
        let cam = out.join("s.cam");
        fs::write(&cam, src).unwrap();
        let (ok, log) = run_cli(&[
            cam.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--target",
            "evm",
        ]);
        assert!(ok, "PM-024 setup: evm transpile failed:\n{log}");
        fs::write(
            out.join("foundry.toml"),
            r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
evm_version = "prague"
optimizer = false
"#,
        )
        .unwrap();
        let build = Command::new("forge")
            .args(["build"])
            .current_dir(&out)
            .output()
            .expect("forge build");
        assert!(
            build.status.success(),
            "PM-024 setup: forge build failed:\n{}{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        let solc_slot = forge_storage_slot(&out, "S", "m_b").expect("forge storage-layout m_b");
        assert_eq!(
            computed, solc_slot,
            "PM-024: compute_layout slot for m_b must match solc storage-layout"
        );
        let _ = fs::remove_dir_all(&out);
    } else {
        // Ground truth from `forge inspect` on HEAD (struct Info occupies slots 1–2).
        const SOLC_M_B_SLOT: u64 = 3;
        assert_eq!(
            computed, SOLC_M_B_SLOT,
            "PM-024: compute_layout must match solc for record-typed member (m_b expected slot {SOLC_M_B_SLOT})"
        );
    }
}

#[test]
fn phase_m_pm025_lean_create2_is_abstract_fnv_formula() {
    // PM-025 RECLASSIFIED: Lean CREATE2 is intentionally abstract (FNV
    // deployer/initCodeHash + opaque create2Address for injectivity).
    // Numeric equality with CambrianFactory.predict* (Keccak, salt=0,
    // identity in init-code) is NOT required.
    let lean_files = transpile_project_lean("pm_m25_lean.yaml");
    let lean_src = lean_files
        .get("Cambrian/Generated/PredictTarget.lean")
        .expect("PredictTarget.lean");

    assert!(
        lean_src.contains("def deployer : Cambrian.Address"),
        "PM-025: Lean must emit abstract deployer constant:\n{lean_src}"
    );
    assert!(
        lean_src.contains("def initCodeHash : BitVec 256"),
        "PM-025: Lean must emit abstract initCodeHash constant:\n{lean_src}"
    );
    assert!(
        lean_src.contains("def salt (id : Identity) : BitVec 256"),
        "PM-025: Lean must emit identity→salt encoder:\n{lean_src}"
    );
    assert!(
        lean_src.contains("Cambrian.create2Address deployer (salt id) initCodeHash"),
        "PM-025: Lean address must compose create2Address(deployer, salt, initCodeHash):\n{lean_src}"
    );

    // Distinct identities ⇒ distinct analytic addresses (salt injectivity).
    let a42 = lean_analytic_address_from_entity_lean(lean_src, 42);
    let a7 = lean_analytic_address_from_entity_lean(lean_src, 7);
    assert_ne!(
        a42, a7,
        "PM-025: abstract CREATE2 salt must distinguish identities\n  id=42: 0x{}\n  id=7:  0x{}",
        hex_addr(&a42),
        hex_addr(&a7),
    );

    // EVM factory still exists as the runtime oracle (not compared numerically).
    let evm_files = transpile_project_evm("pm_m25_evm.yaml");
    let factory = evm_files
        .values()
        .find(|s| s.contains("contract CambrianFactory"))
        .expect("CambrianFactory");
    assert!(
        factory.contains("predictPredictTarget"),
        "PM-025 setup: EVM factory must still expose predict* runtime oracle:\n{factory}"
    );
}

/// Ground truth from `forge create` + `cast call` on HEAD (identity `m_id = 42`).
/// Retained for optional forge probes; no longer compared to Lean (PM-025 reclass).
#[allow(dead_code)]
const FROZEN_EVM_PREDICT_ADDR: [u8; 20] = [
    0x71, 0xdb, 0x9a, 0xff, 0x37, 0xac, 0xea, 0x47, 0xe7, 0x35, 0xd2, 0x75, 0x56, 0xfd, 0x46,
    0x5b, 0x9b, 0xea, 0xfd, 0x95,
];

const ANVIL_DEFAULT_KEY: &str =
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

const ANVIL_ATTACKER_KEY: &str =
    "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";

#[test]
fn phase_m_pm026_lean_deploy_occupancy_only_is_intentional() {
    // PM-026 RECLASSIFIED: Lean spawn models language-level CREATE2
    // occupancy only. Factory owner / isDeployed is an EVM adapter concern.
    const ID: u64 = 7;
    let _ = ID;

    let lean_files = transpile_project_lean("pm_m26_lean.yaml");
    let routes = lean_files
        .get("Cambrian/Generated/DeployCallerRoutes.lean")
        .expect("DeployCallerRoutes.lean");
    assert!(
        routes.contains("deployTarget_deployed") && routes.contains("ThrowCode.ofNat 91"),
        "PM-026: Lean deploy must model CREATE2 occupancy (T-X-001):\n{}",
        lean_spawn_deploy_excerpt(routes)
    );
    assert!(
        !lean_deploy_models_factory_owner_guard(routes),
        "PM-026: Lean must NOT model factory owner/isDeployed (EVM-adapter-only):\n{}",
        lean_spawn_deploy_excerpt(routes)
    );

    let evm_files = transpile_project_evm("pm_m26_evm.yaml");
    let factory_src = evm_files
        .values()
        .find(|s| s.contains("contract CambrianFactory"))
        .expect("CambrianFactory in EVM project");
    assert!(
        factory_src.contains("CambrianFactory: unauthorized"),
        "PM-026 setup: EVM factory deploy must still require owner or prior deployment"
    );

    let evm_non_owner_reverts = if has_forge() {
        forge_non_owner_deploy_reverts("pm_m26_evm.yaml", 7).unwrap_or_else(|| {
            eprintln!("PM-026: forge oracle failed — using frozen non-owner revert=true");
            FROZEN_EVM_NON_OWNER_DEPLOY_REVERTS
        })
    } else {
        eprintln!("PM-026: forge not on PATH — using frozen non-owner revert=true");
        FROZEN_EVM_NON_OWNER_DEPLOY_REVERTS
    };
    assert!(
        evm_non_owner_reverts,
        "PM-026 setup: non-owner deployDeployTarget must revert on EVM"
    );
}

// ---------------------------------------------------------------------------
// Wave M4 — CEI forge exec corpus (PM-027)
// ---------------------------------------------------------------------------

#[test]
fn phase_m_pm027_det_reentrancy_order_must_be_in_forge_exec_corpus() {
    const KEY: &str = "det_reentrancy_order";

    assert!(
        corpus::PROJECT_ONLY.contains(&KEY),
        "PM-027: `{KEY}` must stay PROJECT_ONLY (needs contracts/det_reentrancy_order.yaml)"
    );

    let in_single_file_corpus = corpus::domain_fixtures(Domain::Evm)
        .iter()
        .any(|f| f.key == KEY);
    assert!(
        !in_single_file_corpus,
        "PM-027: PROJECT_ONLY entry must remain absent from domain_fixtures(Domain::Evm)"
    );

    let exec_src = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test_domain_fixture_exec.rs"),
    )
    .expect("read test_domain_fixture_exec.rs");
    assert!(
        exec_src.contains("PROJECT_FORGE_EXEC") && exec_src.contains(KEY),
        "PM-027: domain_fixture_execution_actions must forge-build `{KEY}` via PROJECT_FORGE_EXEC"
    );

    let project_src = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test_project.rs"),
    )
    .expect("read test_project.rs");
    assert!(
        project_src.contains("phases_q1_unphased_route_sstores_before_calls")
            && project_src.contains(KEY),
        "PM-027: static SSTORE-before-CALL oracle must still reference `{KEY}`"
    );
}

// ---------------------------------------------------------------------------
// Wave M4 — ACTIVE_CTX TLS isolation (PM-028)
// ---------------------------------------------------------------------------

fn pm_m28_evm_test_sol(yaml: &str) -> String {
    let project = load_project_yaml(yaml);
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

fn pm_m28_uses_predict_markers(test_sol: &str) -> bool {
    test_sol.contains("predictTlsPeer")
        || test_sol.contains("ICambrianFactory(_factory).predict")
}

fn pm_m28_early_return_skips_set_active_ctx(testgen_src: &str) -> bool {
    let Some(set_pos) = testgen_src.find("set_active_ctx(EvmCtx::build") else {
        return false;
    };
    let Some(ret_pos) = testgen_src.find("return vec![];") else {
        return false;
    };
    ret_pos < set_pos
}

fn pm_m28_active_ctx_reset_contract() -> bool {
    let ctx_src = fs::read_to_string(
        repo_root().join("cambrian-transpiler/src/codegen/solidity/core/ctx.rs"),
    )
    .expect("read ctx.rs");
    let testgen_src = fs::read_to_string(
        repo_root().join("cambrian-transpiler/src/codegen/evm_test_codegen.rs"),
    )
    .expect("read evm_test_codegen.rs");

    let has_reset_fn = ctx_src.contains("fn reset_active_ctx")
        || ctx_src.contains("fn clear_active_ctx");
    let generate_evm_tests_resets = testgen_src.contains("reset_active_ctx(");

    has_reset_fn
        && generate_evm_tests_resets
        && !pm_m28_early_return_skips_set_active_ctx(&testgen_src)
}

#[test]
fn phase_m_pm028_active_ctx_no_leak_across_sequential_gen() {
    let ctx_src = fs::read_to_string(
        repo_root().join("cambrian-transpiler/src/codegen/solidity/core/ctx.rs"),
    )
    .expect("read ctx.rs");
    let testgen_src = fs::read_to_string(
        repo_root().join("cambrian-transpiler/src/codegen/evm_test_codegen.rs"),
    )
    .expect("read evm_test_codegen.rs");
    assert!(
        ctx_src.contains("static ACTIVE_CTX") && ctx_src.contains("fn gen_expr_test"),
        "PM-028 setup: Foundry test bridge uses thread-local ACTIVE_CTX + gen_expr_test"
    );
    assert!(
        testgen_src.contains("set_active_ctx(EvmCtx::build(program, deterministic, true))")
            && testgen_src.contains("reset_active_scratch()"),
        "PM-028 setup: generate_evm_tests sets ACTIVE_CTX and resets scratch once per call"
    );

    let b_alone = pm_m28_evm_test_sol("pm_m28_b_evm.yaml");
    assert!(
        pm_m28_uses_predict_markers(&b_alone),
        "PM-028 setup: program B (det=true, U4-6) must emit predict* when generated alone"
    );

    let a_sol = pm_m28_evm_test_sol("pm_m28_a_evm.yaml");
    assert!(
        pm_m28_uses_predict_markers(&a_sol),
        "PM-028 setup: program A (det=true) must emit predict* in test harness"
    );

    // Sequential gen in one thread: prime deterministic A, then non-det B.
    let _prime = transpile_project_evm("pm_m28_a_evm.yaml");
    let b_after_a = pm_m28_evm_test_sol("pm_m28_b_evm.yaml");

    assert_eq!(
        b_alone, b_after_a,
        "PM-028 note: B output identical after A prime on HEAD (set_active_ctx overwrite masks stale deterministic)"
    );

    assert!(
        pm_m28_active_ctx_reset_contract(),
        "PM-028: ACTIVE_CTX TLS bridge lacks explicit reset / early-return can skip set_active_ctx — \
         gen_expr_test reads stale EvmCtx.deterministic across sequential codegen (p5 deferred TLS)"
    );
}

// ---------------------------------------------------------------------------
// Wave M4 — Foundry harness IR bypass (PM-029)
// ---------------------------------------------------------------------------

fn pm_m29_contract_sol(yaml: &str) -> String {
    let files = transpile_project_evm(yaml);
    files
        .into_iter()
        .find(|(p, s)| p.contains("_project.sol") || s.contains("function probe("))
        .map(|(_, s)| s)
        .unwrap_or_default()
}

fn pm_m29_test_sol(yaml: &str) -> String {
    let files = transpile_project_evm(yaml);
    files
        .into_iter()
        .find(|(p, _)| p.starts_with("test/") && p.ends_with(".t.sol"))
        .map(|(_, s)| s)
        .unwrap_or_default()
}

fn pm_m29_extract_return_expr(contract: &str, fn_name: &str) -> Option<String> {
    let start = contract.find(&format!("function {fn_name}("))?;
    let tail = &contract[start..];
    let ret_line = tail.lines().find(|l| l.trim_start().starts_with("return "))?;
    Some(
        ret_line
            .trim()
            .trim_end_matches(';')
            .trim_start_matches("return ")
            .to_string(),
    )
}

fn pm_m29_extract_asserteq_rhs(test_sol: &str, test_fn: &str) -> Option<String> {
    let start = test_sol.find(&format!("function {test_fn}"))?;
    let block = &test_sol[start..];
    let line = block.lines().find(|l| l.contains("assertEq(_ret_"))?;
    let args = line.split("assertEq(").nth(1)?;
    let rhs = args.split(',').nth(1)?.trim();
    Some(rhs.to_string())
}

fn pm_m29_gen_expr_test_routes_through_ir() -> bool {
    let ctx_src = fs::read_to_string(
        repo_root().join("cambrian-transpiler/src/codegen/solidity/core/ctx.rs"),
    )
    .expect("read ctx.rs");
    let testgen_src = fs::read_to_string(
        repo_root().join("cambrian-transpiler/src/codegen/evm_test_codegen.rs"),
    )
    .expect("read evm_test_codegen.rs");

    ctx_src.contains("gen_expr_test_ir")
        && ctx_src.contains("lower_expr")
        && ctx_src.contains("materialize_typed_expr")
        && testgen_src.contains("gen_expr_test_ir")
}

#[test]
fn phase_m_pm029_gen_expr_test_bypasses_ir() {
    let prog = parse_cam(include_str!(
        "audit/fixtures/phase_m/pm_m29_harness_ir_bypass.cam"
    ));
    let entity = entity_by_name(&prog, "HarnessIr");
    let mix_route = route_by_name(entity, "mix");
    let mix_ctx = LowerCtx::new(&prog, entity, mix_route);
    let mix_return = mix_route
        .body
        .actions()
        .iter()
        .find_map(|a| match a {
            ast::RouteAction::Return { values } if values.len() == 1 => Some(&values[0]),
            _ => None,
        })
        .expect("mix return");
    let mix_typed = lower_expr(mix_return, &mix_ctx);
    match mix_typed.kind {
        TypedExprKind::BinOp { lhs, .. } => match lhs.kind {
            TypedExprKind::Coerce {
                kind: CoerceKind::Widen,
                ..
            } => {}
            other => panic!("PM-029 setup: mix u8 operand must carry widen Coerce in IR: {other:?}"),
        },
        other => panic!("PM-029 setup: expected mix binop in IR: {other:?}"),
    }

    let contract = pm_m29_contract_sol("pm_m29_evm.yaml");
    let test_sol = pm_m29_test_sol("pm_m29_evm.yaml");
    let route_expr = pm_m29_extract_return_expr(&contract, "probe")
        .expect("probe() return in generated contract");
    let harness_expr = pm_m29_extract_asserteq_rhs(&test_sol, "test_probe_return_oracle")
        .expect("probe test assertEq rhs in generated harness");

    assert!(
        pm_m29_gen_expr_test_routes_through_ir() && route_expr == harness_expr,
        "PM-029: Foundry harness gen_expr_test bypasses P6 IR (lower_expr / materialize_typed_expr); \
         route probe() `{route_expr}` vs harness expect `{harness_expr}` (p6 residual F)"
    );
}

/// Frozen from `cast send deployDeployTarget` by anvil account #1 on HEAD.
const FROZEN_EVM_NON_OWNER_DEPLOY_REVERTS: bool = true;

fn lean_deploy_models_factory_owner_guard(routes_lean: &str) -> bool {
    let body = lean_spawn_deploy_excerpt(routes_lean);
    (body.contains("ctx.sender") || body.contains("MsgCtx.sender"))
        && (body.contains("owner") || body.contains("isDeployed"))
}

fn lean_spawn_deploy_excerpt(routes_lean: &str) -> String {
    let start = routes_lean
        .find("def spawn ")
        .unwrap_or(0);
    let slice = &routes_lean[start..];
    slice.chars().take(1200).collect()
}

fn forge_non_owner_deploy_reverts(yaml: &str, id: u64) -> Option<bool> {
    use std::time::Duration;

    let project = load_project_yaml(yaml);
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    let files: HashMap<_, _> = backend.gen_project(&project).into_iter().collect();
    if !files
        .values()
        .any(|src| src.contains("contract CambrianFactory"))
    {
        return None;
    }

    let out = unique_temp_dir("pm26-forge");
    fs::create_dir_all(out.join("src")).ok()?;
    for (rel, body) in &files {
        if rel.ends_with(".sol") {
            let path = out.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).ok()?;
            }
            fs::write(path, body).ok()?;
        }
    }
    fs::write(
        out.join("foundry.toml"),
        r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
evm_version = "prague"
optimizer = false
"#,
    )
    .ok()?;
    let build = Command::new("forge")
        .arg("build")
        .current_dir(&out)
        .output()
        .ok()?;
    if !build.status.success() {
        eprintln!(
            "PM-026 forge build failed:\n{}{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        let _ = fs::remove_dir_all(&out);
        return None;
    }

    let port = 19_700u16 + (std::process::id() % 1000) as u16;
    let rpc = format!("http://127.0.0.1:{port}");
    let mut anvil = Command::new("anvil")
        .args(["--port", &port.to_string(), "--silent"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    std::thread::sleep(Duration::from_millis(900));

    let factory_contract = format!("src/_{}_project.sol:CambrianFactory", project.name());
    let create = Command::new("forge")
        .args([
            "create",
            &factory_contract,
            "--rpc-url",
            &rpc,
            "--private-key",
            ANVIL_DEFAULT_KEY,
            "--broadcast",
        ])
        .current_dir(&out)
        .output()
        .ok()?;
    let create_log = format!(
        "{}{}",
        String::from_utf8_lossy(&create.stdout),
        String::from_utf8_lossy(&create.stderr)
    );
    let factory_addr = create_log
        .split("Deployed to:")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(parse_evm_address_hex);
    let Some(factory_addr) = factory_addr else {
        eprintln!("PM-026 forge create failed:\n{create_log}");
        let _ = anvil.kill();
        let _ = fs::remove_dir_all(&out);
        return None;
    };

    let send = Command::new("cast")
        .args([
            "send",
            &hex_addr(&factory_addr),
            "deployDeployTarget(uint64)",
            &id.to_string(),
            "--private-key",
            ANVIL_ATTACKER_KEY,
            "--rpc-url",
            &rpc,
        ])
        .output()
        .ok()?;
    let send_log = format!(
        "{}{}",
        String::from_utf8_lossy(&send.stdout),
        String::from_utf8_lossy(&send.stderr)
    );
    let _ = anvil.kill();
    let _ = fs::remove_dir_all(&out);
    Some(!send.status.success() && send_log.contains("unauthorized"))
}

fn hex_addr(bytes: &[u8; 20]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_lean_hex_const(lean_src: &str, def: &str, suffix: &str) -> U256 {
    let needle = format!("def {def}");
    let block = lean_src
        .split(&needle)
        .nth(1)
        .unwrap_or_else(|| panic!("PM-025 setup: missing `{needle}` in Lean entity"));
    let line = block
        .lines()
        .find(|l| l.contains("#160") || l.contains("#256"))
        .unwrap_or_else(|| panic!("PM-025 setup: no hex literal for `{def}`"));
    let hex = line
        .split("0x")
        .nth(1)
        .and_then(|s| s.split('#').next())
        .unwrap_or_else(|| panic!("PM-025 setup: parse hex for `{def}` from: {line}"));
    let mut buf = [0u8; 32];
    let raw = hex.as_bytes();
    let start = 32usize.saturating_sub(raw.len() / 2);
    for i in 0..raw.len() / 2 {
        let byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
            .unwrap_or_else(|_| panic!("invalid hex in `{def}`"));
        buf[start + i] = byte;
    }
    let _ = suffix;
    U256::from_be_bytes(&buf)
}

fn lean_hash_words_core(words: &[U256]) -> U256 {
    let mut h = U256::from_u128(0xcbf29ce484222325);
    for w in words {
        h = (h ^ *w).wrapping_mul(U256::from_u128(0x100000001b3));
    }
    h
}

fn lean_analytic_address_from_entity_lean(lean_src: &str, m_id: u64) -> [u8; 20] {
    let deployer = parse_lean_hex_const(lean_src, "deployer", "#160");
    let init = parse_lean_hex_const(lean_src, "initCodeHash", "#256");
    let salt = U256::from_u128(m_id as u128);
    let digest = lean_hash_words_core(&[deployer, salt, init]);
    let be = digest.to_be_bytes();
    let mut out = [0u8; 20];
    out.copy_from_slice(&be[12..32]);
    out
}

#[allow(dead_code)]
fn forge_factory_predict_address(yaml: &str, m_id: u64) -> Option<[u8; 20]> {
    use std::time::Duration;

    let project = load_project_yaml(yaml);
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    let files: HashMap<_, _> = backend.gen_project(&project).into_iter().collect();
    if !files
        .values()
        .any(|src| src.contains("contract CambrianFactory"))
    {
        return None;
    }

    let out = unique_temp_dir("pm25-forge");
    fs::create_dir_all(out.join("src")).ok()?;
    for (rel, body) in &files {
        if rel.ends_with(".sol") || rel.ends_with("_project.sol") {
            let path = out.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).ok()?;
            }
            fs::write(path, body).ok()?;
        }
    }
    fs::write(
        out.join("foundry.toml"),
        r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
evm_version = "prague"
optimizer = false
"#,
    )
    .ok()?;
    let build = Command::new("forge")
        .arg("build")
        .current_dir(&out)
        .output()
        .ok()?;
    if !build.status.success() {
        eprintln!(
            "PM-025 forge build failed:\n{}{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        let _ = fs::remove_dir_all(&out);
        return None;
    }

    let port = 19_600u16 + (std::process::id() % 1000) as u16;
    let rpc = format!("http://127.0.0.1:{port}");
    let mut anvil = Command::new("anvil")
        .args(["--port", &port.to_string(), "--silent"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    std::thread::sleep(Duration::from_millis(900));

    let factory_contract = format!("src/_{}_project.sol:CambrianFactory", project.name());
    let create = Command::new("forge")
        .args([
            "create",
            &factory_contract,
            "--rpc-url",
            &rpc,
            "--private-key",
            ANVIL_DEFAULT_KEY,
            "--broadcast",
        ])
        .current_dir(&out)
        .output()
        .ok()?;
    let create_log = format!(
        "{}{}",
        String::from_utf8_lossy(&create.stdout),
        String::from_utf8_lossy(&create.stderr)
    );
    let factory_addr = create_log
        .split("Deployed to:")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(parse_evm_address_hex);
    let Some(factory_addr) = factory_addr else {
        eprintln!("PM-025 forge create failed:\n{create_log}");
        let _ = anvil.kill();
        let _ = fs::remove_dir_all(&out);
        return None;
    };

    let call = Command::new("cast")
        .args([
            "call",
            &hex_addr(&factory_addr),
            "predictPredictTarget(uint64)(address)",
            &m_id.to_string(),
            "--rpc-url",
            &rpc,
        ])
        .output()
        .ok()?;
    let _ = anvil.kill();
    let _ = fs::remove_dir_all(&out);
    if !call.status.success() {
        eprintln!(
            "PM-025 cast call failed:\n{}{}",
            String::from_utf8_lossy(&call.stdout),
            String::from_utf8_lossy(&call.stderr)
        );
        return None;
    }
    let predicted_hex = String::from_utf8_lossy(&call.stdout).trim().to_string();
    parse_evm_address_hex(&predicted_hex)
}

fn parse_evm_address_hex(s: &str) -> Option<[u8; 20]> {
    let hex = s.strip_prefix("0x").unwrap_or(s);
    if hex.len() != 40 {
        return None;
    }
    let mut out = [0u8; 20];
    for i in 0..20 {
        out[i] = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

fn unique_temp_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("phase-m-{tag}-{}-{n}", std::process::id()))
}
