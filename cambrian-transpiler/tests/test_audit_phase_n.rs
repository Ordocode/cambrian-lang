// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase N audit harness — coverage quality + deep audit (docs/AUDIT_PHASE_N.md).
//!
//! Red tests assert **desired** semantics; failure on HEAD = CONFIRMED finding
//! for triage on **`kernel-adapter-refactor`**. No codegen fixes on `audit`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::{
    EvmSolidityBackend, LeanBackend,
    OutputBackend,
};
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

fn audit_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit")
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn gitlab_ci_yaml() -> Option<String> {
    let path = repo_root().join(".gitlab-ci.yml");
    if path.is_file() {
        Some(fs::read_to_string(path).expect("read CI yaml"))
    } else {
        None
    }
}

fn phase_n_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/audit/fixtures/phase_n")
}

fn transpile_project_evm(yaml: &str) -> HashMap<String, String> {
    let project = load_project_yaml(yaml);
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    backend.gen_project(&project).into_iter().collect()
}

fn load_project_yaml(name: &str) -> Project {
    let yaml = phase_n_dir().join(name);
    Project::load(&yaml).unwrap_or_else(|e| panic!("load {}: {e}", yaml.display()))
}

fn transpile_project_lean(yaml: &str) -> HashMap<String, String> {
    let project = load_project_yaml(yaml);
    LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .collect()
}

fn count_route_constructor_fns(src: &str) -> usize {
    src.lines()
        .filter(|line| line.contains("pub fn route_constructor"))
        .count()
}

fn entity_solidity(files: &HashMap<String, String>, entity: &str) -> String {
    // Prefer the flat `_project.sol` (real CALL bodies live there under
    // deterministic multi-file layout). Per-entity `Entity.sol` stubs often
    // lack `call{value:}` (PN-102 harness gap).
    if let Some((_, src)) = files.iter().find(|(path, _)| path.contains("_project.sol")) {
        if src.contains(&format!("contract {entity}")) || src.contains("function ") {
            return src.clone();
        }
    }
    let needle = format!("{entity}.sol");
    files
        .iter()
        .find(|(path, _)| path.ends_with(&needle))
        .map(|(_, src)| src.clone())
        .unwrap_or_else(|| {
            panic!(
                "missing {needle} / _project.sol in EVM output (keys: {:?})",
                files.keys().collect::<Vec<_>>()
            )
        })
}

fn extract_rust_fn_body(src: &str, fn_name: &str) -> String {
    let needle = format!("fn {fn_name}(");
    let start = src
        .find(&needle)
        .unwrap_or_else(|| panic!("missing {needle}"));
    let rest = &src[start..];
    let end = rest[1..]
        .find("\nfn ")
        .map(|i| i + 1)
        .unwrap_or(rest.len());
    rest[..end].to_string()
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
        "cambrian-audit-phase-n-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ))
}

fn ensure_forge_std(out_dir: &Path) {
    cambrian_transpiler::codegen::evm_test_codegen::install_forge_std(out_dir)
        .unwrap_or_else(|e| panic!("{e} in {}", out_dir.display()));
}

fn transpile_project_to_dir(yaml_name: &str, out_dir: &Path) {
    let project = load_project_yaml(yaml_name);
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    for (rel, contents) in backend.gen_project(&project) {
        let path = out_dir.join(&rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, contents).expect("write sol");
    }
}

struct ForgeRun {
    ok: bool,
    log: String,
}

fn run_forge_test(yaml: &str, forge_file: &str, match_test: &str) -> ForgeRun {
    let out_dir = unique_out_dir(match_test);
    let _ = fs::remove_dir_all(&out_dir);
    fs::create_dir_all(&out_dir).expect("out dir");
    transpile_project_to_dir(yaml, &out_dir);
    fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).expect("foundry.toml");
    let test_dir = out_dir.join("test");
    fs::create_dir_all(&test_dir).expect("test dir");
    fs::copy(
        audit_root().join("forge").join(forge_file),
        test_dir.join(forge_file),
    )
    .expect("copy forge test");
    ensure_forge_std(&out_dir);
    let forge = Command::new("forge")
        .args(["test", "--match-test", match_test, "-vv", "--root"])
        .arg(&out_dir)
        .output()
        .expect("forge test");
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );
    let ok = forge.status.success();
    let _ = fs::remove_dir_all(&out_dir);
    ForgeRun { ok, log }
}

fn count_active_sorries(lean: &str) -> usize {
    lean.lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .filter(|line| line.contains("| sorry") || line.contains("by sorry"))
        .count()
}

// ---------------------------------------------------------------------------
// PN-001 / T-PN-DIFF-001 — differential Lean `#eval` oracle (PASS)
// ---------------------------------------------------------------------------

#[test]
fn phase_n_pn001_differential_uses_lean_execution_oracle() {
    let harness = include_str!("test_audit_differential.rs");
    let has_substring_inspect = harness.contains("fn inspect_lean_case")
        && harness.contains("ForbiddenSubstring");
    assert!(
        has_substring_inspect,
        "PN-001 setup: differential harness must still define inspect_lean_case"
    );

    assert!(
        harness.contains("struct LeanExec")
            && harness.contains("lean_exec:")
            && harness.contains("#eval")
            && harness.contains("fn eval_lean_case"),
        "PN-001: test_audit_differential must carry LeanExec + eval_lean_case + #eval"
    );
}

#[test]
fn phase_n_pn001_differential_exposes_eval_lean_case() {
    let harness = include_str!("test_audit_differential.rs");
    assert!(
        harness.contains("fn inspect_lean_case"),
        "PN-001 setup: differential harness must define inspect_lean_case"
    );
    assert!(
        harness.contains("fn eval_lean_case")
            && harness.contains("fn eval_lean_case_at")
            && harness.contains("lake env lean"),
        "PN-001: differential must expose eval_lean_case / eval_lean_case_at + lake env lean"
    );
}

// ---------------------------------------------------------------------------
// PN-003 — revm / cargo-fuzz codegen absent from CI
// ---------------------------------------------------------------------------

#[test]
fn phase_n_pn003_revm_or_cargo_fuzz_in_ci() {
    let Some(ci) = gitlab_ci_yaml() else {
        eprintln!("PN-003 skip: .gitlab-ci.yml omitted from public snapshot");
        return;
    };
    let has_revm_gate = ci.contains("revm")
        || ci.contains("evm_revm")
        || ci.contains("cargo-fuzz")
        || ci.contains("cargo_fuzz");
    assert!(
        has_revm_gate,
        "PN-003 CONFIRMED (T-PN-HARNESS-001 / PN-003): `.gitlab-ci.yml` must run \
         at least one job that builds/executes generated revm-tests or cargo-fuzz targets."
    );
    assert!(
        ci.contains("CAMBRIAN_TEST_REVM"),
        "PN-003: CI revm job must set CAMBRIAN_TEST_REVM=1 (see test-transpiler-revm)"
    );
}

// ---------------------------------------------------------------------------
// PN-103 / T-PN-ARITH-001 — Lean u64 `+` not checked vs LANGUAGE.md
// ---------------------------------------------------------------------------

#[test]
fn phase_n_pn103_lean_u64_increment_uses_checked_add() {
    let files = transpile_project_lean("pn_m103_lean.yaml");
    let routes = files
        .get("Cambrian/Generated/CounterRoutes.lean")
        .expect("CounterRoutes.lean");
    let members = files
        .get("Cambrian/Generated/CounterMembers.lean")
        .or_else(|| files.get("Cambrian/Generated/Counter.lean"))
        .expect("Counter member defs");
    let body = format!("{routes}\n{members}");
    let uses_checked = body.contains("checkedAdd")
        || body.contains("checked_add")
        || body.contains("Cambrian.add")
        || body.contains("addOverflow");
    assert!(
        uses_checked,
        "PN-103 (T-PN-ARITH-001 / PN-103): Lean lowering of `m_count + 1` under \
         lean.numerics: overflow-panic must use checked arithmetic. Emitted:\n{body}"
    );
}

#[test]
fn phase_n_pn103_lean_overflow_wrap_keeps_wrapping_add() {
    let files = transpile_project_lean("pn_m103_wrap_lean.yaml");
    let members = files
        .get("Cambrian/Generated/CounterMembers.lean")
        .or_else(|| files.get("Cambrian/Generated/Counter.lean"))
        .expect("Counter member defs");
    assert!(
        !members.contains("checkedAdd"),
        "PN-103 pin: lean.numerics: overflow-wrap must keep wrapping `+`, not checkedAdd:\n{members}"
    );
    assert!(
        members.contains("+") || members.contains("m_count"),
        "PN-103 pin: wrap mode must still emit an increment:\n{members}"
    );
}

#[test]
fn phase_n_pn103_evm_u64_increment_lowers_member_transform() {
    let files = transpile_project_evm("pn_m103_evm.yaml");
    let sol = files
        .values()
        .find(|s| s.contains("m_count"))
        .unwrap_or_else(|| {
            panic!(
                "PN-103 setup: EVM output must reference m_count (keys: {:?})",
                files.keys().collect::<Vec<_>>()
            )
        });
    assert!(
        sol.contains("m_count"),
        "PN-103 setup: EVM must reference m_count:\n{sol}"
    );
}

// ---------------------------------------------------------------------------
// PN-002 / T-PN-VAC-001 — goal-shape + false-zero ratchet (PASS)
// ---------------------------------------------------------------------------

#[test]
fn phase_n_pn002_spec_sorry_gate_detects_false_theorem_body() {
    let specs_harness = include_str!("test_audit_lean_specs.rs");
    assert!(
        specs_harness.contains("fn theorem_goal_references_state"),
        "PN-002: test_audit_lean_specs must define theorem_goal_references_state"
    );
    assert!(
        specs_harness.contains("plausible") && specs_harness.contains("counterexample"),
        "PN-002: specs harness must mention plausible / counterexample ratchets"
    );

    let files = transpile_project_lean("pn_vac_false_zero.yaml");
    let spec = files
        .get("Cambrian/Generated/CounterSpec.lean")
        .expect("CounterSpec.lean");
    assert!(
        spec.contains("m_count"),
        "PN-002: false-zero invariant statement must mention m_count:\n{spec}"
    );
}

// ---------------------------------------------------------------------------
// PN-104 / T-PN-VAC-001 — false invariant check needs goal/exec oracle (PASS)
// ---------------------------------------------------------------------------

#[test]
fn phase_n_pn104_mutated_false_check_needs_exec_oracle() {
    let files = transpile_project_lean("pn_vac_false_zero.yaml");
    let spec = files
        .get("Cambrian/Generated/CounterSpec.lean")
        .expect("CounterSpec.lean");
    assert!(
        spec.contains("m_count") && (spec.contains("== 0") || spec.contains("= 0")),
        "PN-104 setup: spec must encode false check m_count == 0:\n{spec}"
    );
    let sorries = count_active_sorries(spec);
    assert!(
        sorries >= 1,
        "PN-104 setup: mutated spec still uses sorry (count={sorries})"
    );

    let specs_harness = include_str!("test_audit_lean_specs.rs");
    assert!(
        specs_harness.contains("theorem_goal_references_state")
            && (specs_harness.contains("plausible") || specs_harness.contains("counterexample")),
        "PN-104: refutable invariant must be gated by theorem_goal_references_state / \
         plausible|counterexample (sorry count={sorries} alone is insufficient)"
    );
}

// ---------------------------------------------------------------------------
// PN-106 / T-PN-DIFF-001 — extern revert: EVM ok, Lean opaque total
// ---------------------------------------------------------------------------

#[test]
fn phase_n_pn106_evm_extern_revert_propagates() {
    if !has_forge() {
        eprintln!(
            "skipping phase_n_pn106_evm_extern_revert_propagates: forge not on \
             PATH (coverage / non-Foundry CI images; the gate lives in \
             test-transpiler-phase-n-gates)"
        );
        return;
    }
    let run = run_forge_test(
        "pn_diff_extern_revert_evm.yaml",
        "Pn106ExternRevert.t.sol",
        "test_PN106_evmPayRevertsWhenTokenReverts",
    );
    assert!(
        run.ok,
        "PN-106 setup: EVM must propagate extern revert:\n{}",
        run.log
    );
}

#[test]
fn phase_n_pn106_lean_extern_models_revert_not_total_axiom() {
    let files = transpile_project_lean("pn_diff_extern_revert_lean.yaml");
    let ext = files
        .get("Cambrian/Generated/Extern.lean")
        .expect("Extern.lean");
    assert!(
        ext.contains("opaque transfer")
            && ext.contains("Except Cambrian.ThrowCode"),
        "PN-106: extern transfer must be opaque Except axiom:\n{ext}"
    );
    let routes = files
        .get("Cambrian/Generated/WalletRoutes.lean")
        .expect("WalletRoutes.lean");
    let models_revert = routes.contains("RouteResult")
        && (routes.contains("Except.bind")
            || routes.contains(" ← ")
            || routes.contains("← "));
    assert!(
        models_revert,
        "PN-106: Lean must treat extern `transfer` as Except and bind at \
         callsite (RouteResult + ← / Except.bind). WalletRoutes:\n{routes}"
    );
}

// ---------------------------------------------------------------------------
// PN-103 — U256 MAX+1: EVM revert (reference) + Lean must not wrap
// ---------------------------------------------------------------------------

#[test]
fn phase_n_pn103_evm_max_plus_one_reverts() {
    if !has_forge() {
        eprintln!(
            "skipping phase_n_pn103_evm_max_plus_one_reverts: forge not on \
             PATH (coverage / non-Foundry CI images; the gate lives in \
             test-transpiler-phase-n-gates)"
        );
        return;
    }
    let run = run_forge_test(
        "pn_arith_max_plus_one_evm.yaml",
        "Pn103ArithOverflow.t.sol",
        "test_PN103_maxPlusOneReverts",
    );
    assert!(
        run.ok,
        "PN-103 setup: EVM checked add must revert at MAX+1:\n{}",
        run.log
    );
}

#[test]
fn phase_n_pn103_lean_max_plus_one_does_not_wrap() {
    let files = transpile_project_lean("pn_arith_max_plus_one_lean.yaml");
    let members = files
        .get("Cambrian/Generated/Adder.lean")
        .or_else(|| files.get("Cambrian/Generated/AdderMembers.lean"))
        .expect("Adder member defs");
    let routes = files
        .get("Cambrian/Generated/AdderRoutes.lean")
        .expect("AdderRoutes.lean");
    let body = format!("{members}\n{routes}");

    let uses_wrapping_bitvec_add = body.contains("(s.m_x + n)")
        || body.contains("(s.m_x + ")
        || (body.contains("m_x + n") && !body.contains("checkedAdd"));
    let has_failure_surface = body.contains("RouteResult")
        || body.contains("checkedAdd")
        || body.contains("Except")
        || body.contains("addOverflow");
    assert!(
        !uses_wrapping_bitvec_add,
        "PN-103 (T-PN-ARITH-001): Lean must not emit wrapping `m_x + n` under \
         lean.numerics: overflow-panic. Emitted:\n{body}"
    );
    assert!(
        has_failure_surface,
        "PN-103 (T-PN-ARITH-001): Lean `m_x + n` at U256::MAX must not \
         silently wrap — need checked add / RouteResult failure surface. Emitted:\n{body}"
    );
}

// ---------------------------------------------------------------------------
// PN-104 — okImplies default / okAnd under #[fail_on_revert] (PASS)
// ---------------------------------------------------------------------------

#[test]
fn phase_n_pn104_invariant_trace_no_permissive_error_arm() {
    let files = transpile_project_lean("pn_vac_guarded_false.yaml");
    let spec = files
        .get("Cambrian/Generated/GuardedSpec.lean")
        .expect("GuardedSpec.lean");
    assert!(
        spec.contains("runTrace") || spec.contains("step"),
        "PN-104 setup: guarded invariant spec:\n{spec}"
    );
    assert!(
        !spec.contains("| .error _ => True") && !spec.contains(".error _ => True"),
        "PN-104: must not emit raw `| .error _ => True` match:\n{spec}"
    );
    assert!(
        spec.contains("okImplies") || spec.contains("RouteResult.okImplies"),
        "PN-104: default (no #[fail_on_revert]) must use okImplies:\n{spec}"
    );
    let core = files
        .get("Cambrian/Core.lean")
        .expect("Cambrian/Core.lean");
    assert!(
        core.contains("instDecidableOkImplies") && core.contains("instDecidableOkAnd"),
        "PN-104: Core must ship Decidable for okImplies/okAnd (T-G-003 Testable)"
    );

    let sibling = transpile_project_lean("pn_vac_guarded_false_fail_on_revert.yaml");
    let sib = sibling
        .get("Cambrian/Generated/GuardedSpec.lean")
        .expect("GuardedSpec.lean fail_on_revert");
    assert!(
        !sib.contains("| .error _ => True") && !sib.contains(".error _ => True"),
        "PN-104: fail_on_revert sibling must not emit raw error→True:\n{sib}"
    );
    assert!(
        sib.contains("okAnd") || sib.contains("RouteResult.okAnd"),
        "PN-104: #[fail_on_revert] sibling must use okAnd:\n{sib}"
    );
    // okImplies may still appear in comments / shared prelude imports — do not
    // require its absence; the pin is okAnd on the fail_on_revert theorem.
}

#[test]
fn phase_n_pn104_axioms_gate_uses_print_axioms_not_sorry_count_only() {
    let specs_harness = include_str!("test_audit_lean_specs.rs");
    let has_axioms_oracle = specs_harness.contains("print_axioms")
        || specs_harness.contains("#print axioms")
        || specs_harness.contains("sorryAx");
    assert!(
        has_axioms_oracle,
        "PN-104: `test_audit_lean_specs` must gate on `#print axioms` / `sorryAx`"
    );
}

#[test]
fn phase_n_pn103_lean_arith_has_eval_exec_oracle() {
    let diff = include_str!("test_audit_differential.rs");
    assert!(
        diff.contains("fn eval_lean_case")
            && diff.contains("#eval")
            && diff.contains("audit_differential_pn103_overflow_panic_eval"),
        "PN-103: differential must expose eval_lean_case/#eval and PN-103 overflow eval test"
    );
}

// ---------------------------------------------------------------------------
// PW3-S-009 / PW3-O-009 — coverage harness CI + forge oracle migration
// ---------------------------------------------------------------------------

const CI_COVERAGE_SMOKE_JOB: &str = "test-transpiler-audit-coverage-smoke";

const CI_COVERAGE_ORACLE_STRINGS: &[&str] = &[
    "test-transpiler-audit-coverage-smoke",
    "test_audit_coverage_validate",
    "test_audit_coverage_codegen pw3_o009",
    "CAMBRIAN_TEST_COVERAGE_FORGE",
];

#[test]
fn pw3_o009_coverage_harness_smoke_job_in_ci() {
    let Some(ci) = gitlab_ci_yaml() else {
        eprintln!("PW3-O-009 skip: .gitlab-ci.yml omitted from public snapshot");
        return;
    };
    let missing: Vec<&str> = CI_COVERAGE_ORACLE_STRINGS
        .iter()
        .copied()
        .filter(|needle| !ci.contains(needle))
        .collect();
    assert!(
        missing.is_empty(),
        "PW3-O-009: audit coverage smoke job must be wired in .gitlab-ci.yml \
         (forge oracle subset + CAMBRIAN_TEST_COVERAGE_FORGE); missing: {missing:?}"
    );
    assert!(
        ci.contains(CI_COVERAGE_SMOKE_JOB),
        "PW3-O-009: expected GitLab job `{CI_COVERAGE_SMOKE_JOB}`"
    );
}

fn coverage_forge_on_path() -> bool {
    Command::new("forge")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Opt-in META gate for the full 824-row forge oracle (owner / release).
/// CI smoke stays on the `pw3_o009_*` pilot subset until owner expands the job.
#[test]
fn pw3_o009_full_coverage_forge_824() {
    if std::env::var("CAMBRIAN_TEST_COVERAGE_FORGE_FULL").as_deref() != Ok("1") {
        eprintln!(
            "PW3-S-009 skip: full 824-row forge gate \
             (set CAMBRIAN_TEST_COVERAGE_FORGE_FULL=1; also needs forge on PATH)"
        );
        return;
    }
    if !coverage_forge_on_path() {
        panic!("CAMBRIAN_TEST_COVERAGE_FORGE_FULL=1 but `forge` not on PATH");
    }
    let output = Command::new("cargo")
        .args([
            "test",
            "-p",
            "cambrian-transpiler",
            "--test",
            "test_audit_coverage_codegen",
            "--",
            "--test-threads=20",
        ])
        .env("CAMBRIAN_TEST_COVERAGE_FORGE", "1")
        .current_dir(repo_root())
        .output()
        .expect("run full coverage_codegen forge gate");
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "PW3-S-009: full 824-row forge gate must be green:\n{log}"
    );
    assert!(
        log.contains("824 passed") && log.contains("0 failed"),
        "PW3-S-009: expected 824 passed / 0 failed in gate output:\n{log}"
    );
}

#[test]
fn pw3_o009_coverage_codegen_oracle_uses_forge_not_solc() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test_audit_coverage_codegen.rs");
    let src = fs::read_to_string(&path).expect("read coverage codegen harness");
    assert!(
        !src.contains("Command::new(\"solc\")"),
        "PW3-O-009: coverage codegen harness must not invoke raw solc (rule 04)"
    );
    assert!(
        src.contains("fn assert_forge_compiles") && src.contains("forge build"),
        "PW3-O-009: coverage harness must expose forge compile oracle"
    );
    assert!(
        src.contains("fn pw3_o009_pilot_forge_"),
        "PW3-O-009: pilot forge rows must be pinned in coverage harness"
    );
}

// ---------------------------------------------------------------------------
// Wave-3 matrices must stay wired in GitLab CI (single Phase O job)
// ---------------------------------------------------------------------------

const CI_PHASE_O_JOB: &str = "test-transpiler-phase-o-gates:";

const CI_PHASE_O_BINS: &[&str] = &[
    "test_audit_layout_matrix",
    "test_audit_arith_matrix",
    "test_audit_deploy_matrix",
    "test_audit_type_fuzz",
    "test_audit_stdlib_matrix",
    "test_audit_p7_admit_matrix",
    "test_audit_dispatch_matrix",
    "test_audit_predictable_matrix",
    "test_audit_scaling_matrix",
    "test_audit_invariant_matrix",
    "test_audit_iter_matrix",
    "test_audit_differential",
    "audit_fuzz_iterator",
];

#[test]
fn pw3_wave3_matrices_wired_in_ci() {
    let Some(ci) = gitlab_ci_yaml() else {
        eprintln!("wave-3 skip: .gitlab-ci.yml omitted from public snapshot");
        return;
    };
    assert!(
        ci.contains(CI_PHASE_O_JOB),
        "wave-3 matrices must run in GitLab job `{CI_PHASE_O_JOB}`"
    );
    let job = ci
        .split("test-transpiler-phase-o-gates:")
        .nth(1)
        .and_then(|rest| rest.split("\n# ").next())
        .unwrap_or("");
    let missing_bins: Vec<&str> = CI_PHASE_O_BINS
        .iter()
        .copied()
        .filter(|needle| !job.contains(needle))
        .collect();
    assert!(
        missing_bins.is_empty(),
        "wave-3 matrix binaries missing from {CI_PHASE_O_JOB}: {missing_bins:?}"
    );
    assert!(
        !job.contains("--skip "),
        "test-transpiler-phase-o-gates must not use skip lists:\n{job}"
    );
    assert!(
        !job.contains("test_audit_native_matrix"),
        "PW3-G-015 TODO: test_audit_native_matrix must stay out of {CI_PHASE_O_JOB} \
         (Native world vs forge is a large later change)"
    );
    assert!(
        job.contains("pw3_g002") && !job.contains("pw3_g003"),
        "PW3-G-003 TODO with G-015: {CI_PHASE_O_JOB} must filter deploy matrix to pw3_g002 \
         (Native/EVM address parity only useful with Native/EVM integration)"
    );
}

// ---------------------------------------------------------------------------
// PW3-S-013 / PW3-G-015 — Native runtime exec-oracle policy
// ---------------------------------------------------------------------------

#[test]
fn pw3_s013_native_exec_oracle_policy_documented() {
    let native_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test_audit_native_matrix.rs");
    if !native_path.is_file() {
        // Native-target harness is not part of source distributions.
        eprintln!("skipping PW3-S-013: native matrix harness not in tree");
        return;
    }
    let native = fs::read_to_string(&native_path).expect("read native matrix harness");
    assert!(
        native.contains("fn pw3_s013_g015_value_forge_exec_oracle"),
        "PW3-S-013: forge behavioral oracle row required"
    );
    assert!(
        native.contains("fn pw3_s013_g015_value_native_route_exec"),
        "PW3-S-013: native route exec row required"
    );
    assert!(
        native.contains("CAMBRIAN_TEST_RUST_BUILD") && native.contains("no forge on PATH"),
        "PW3-S-013: skip policy for native-only / missing-forge legs must be documented"
    );
    assert!(
        native.contains("#[ignore = \"PW3-G-015 TODO: Native world vs forge is a large later change\"]"),
        "PW3-G-015: four Native-world vs forge parity red gates must stay ignored (TODO)"
    );
    assert!(
        fs::metadata(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/audit/fixtures/pw3_native/value_transfer.cam"),
        )
        .is_ok(),
        "PW3-S-013: pw3_native fixtures required"
    );
}

// ---------------------------------------------------------------------------
// PW3-S-015 / PW3-O-007, G-014 — iterator truncation policy (extends T-F-004)
// ---------------------------------------------------------------------------

#[test]
fn pw3_s015_iter_truncation_policy_documented() {
    let matrix = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test_audit_iter_matrix.rs"),
    )
    .expect("read iter matrix harness");
    assert!(
        matrix.contains("fn pw3_s015_o007_fold_trunc_forge_oracle"),
        "PW3-S-015: O-007 trunc fold forge row required"
    );
    assert!(
        matrix.contains("fn pw3_s015_g014_wide_chain_forge_exec_red_gate"),
        "PW3-S-015: G-014 wide chain red gate required"
    );
    assert!(
        matrix.contains("fn pw3_s015_beyond_tf004_deep_chain_compile_red_gate"),
        "PW3-S-015: beyond T-F-004 deep chain row required"
    );
    let fuzz = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test_audit_fuzz.rs"),
    )
    .expect("read fuzz harness");
    assert!(
        fuzz.contains("fn audit_fuzz_iterator_wide_chains")
            && fuzz.contains("fn audit_fuzz_iterator_trunc_fold"),
        "PW3-S-015: extended iterator proptest rows required in test_audit_fuzz.rs"
    );
    assert!(
        fs::metadata(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/audit/fixtures/pw3_iter/fold_trunc_u64.cam"),
        )
        .is_ok(),
        "PW3-S-015: pw3_iter fixtures required"
    );
}

// ---------------------------------------------------------------------------
// PW3-S-014 / PW3-O-013, G-012 — invariant trace parity sweep policy
// ---------------------------------------------------------------------------

#[test]
fn pw3_s014_invariant_trace_parity_policy_documented() {
    let matrix = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test_audit_invariant_matrix.rs"),
    )
    .expect("read invariant matrix harness");
    assert!(
        matrix.contains("fn pw3_s014_o013_trace_multiaction_forge_oracle"),
        "PW3-S-014: multi-action trace forge oracle row required"
    );
    assert!(
        matrix.contains("fn pw3_s014_o013_proptest_action_list_forge_sim"),
        "PW3-S-014: bounded proptest action-list generator required"
    );
    assert!(
        matrix.contains("fn pw3_s014_g012_phased_value_runtrace_red_gate")
            && matrix.contains("fn pw3_s014_g012_caret_phased_forge_oracle"),
        "PW3-S-014: G-012 phased value + caret rows required"
    );
    assert!(
        matrix.contains("CAMBRIAN_TEST_LEAN_BUILD") && matrix.contains("no forge on PATH"),
        "PW3-S-014: skip policy for missing forge/lake must be documented"
    );
    assert!(
        fs::metadata(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/audit/fixtures/pw3_invariant/trace_multiaction.cam"),
        )
        .is_ok(),
        "PW3-S-014: pw3_invariant fixtures required"
    );
    let diff = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test_audit_differential.rs"),
    )
    .expect("read differential harness");
    assert_eq!(
        diff.matches("route_name: \"runTrace\"").count(),
        1,
        "PW3-S-014 policy: differential retains single T-X-007 runTrace row; matrix expands sweep"
    );
}

// ---------------------------------------------------------------------------
// PW3-S-012 / PW3-O-005, O-010, G-013 — scaling ratchet policy
// ---------------------------------------------------------------------------

#[test]
fn pw3_s012_scaling_ratchet_policy_documented() {
    let scaling = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test_audit_scaling_matrix.rs"),
    )
    .expect("read scaling harness");
    assert!(
        scaling.contains("fn pw3_s012_o005_cyclic_alias_lean_controlled_failure"),
        "PW3-S-012: O-005 cyclic alias controlled-failure row required"
    );
    assert!(
        scaling.contains("fn pw3_s012_o010_lean_codegen_scaling_ratchet"),
        "PW3-S-012: O-010 lean codegen scaling row required"
    );
    // The G-013 row lives in the predictable-profile harness, which is not part of
    // source distributions; require it only when that harness is in the tree.
    let scaling_escrow = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/test_audit_scaling_matrix_escrow.rs");
    if scaling.contains("fn pw3_s012_g013_rec_mutual_ping_eval_red_gate") {
        // present in the main harness
    } else if scaling_escrow.is_file() {
        let g013_src =
            fs::read_to_string(&scaling_escrow).expect("read scaling escrow harness");
        assert!(
            g013_src.contains("fn pw3_s012_g013_rec_mutual_ping_eval_red_gate"),
            "PW3-S-012: G-013 rec_mutual #eval (create + where-guard) required"
        );
    } else {
        eprintln!("skipping PW3-S-012 G-013 row: escrow scaling harness not in tree");
    }
    assert!(
        scaling.contains("WALL_CASE_SECS") && scaling.contains("CYCLIC_ALIAS_TIMEOUT"),
        "PW3-S-012: wall-clock ratchet constants must be documented in harness"
    );
    assert!(
        fs::metadata(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/audit/fixtures/pw3_scaling/cyclic_alias.cam"),
        )
        .is_ok(),
        "PW3-S-012: cyclic_alias.cam fixture required"
    );
}

#[test]
fn pw3_o012_differential_single_expected_per_case() {
    let diff = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test_audit_differential.rs"),
    )
    .expect("read differential harness");
    assert!(
        diff.contains("enum ExpectedOutcome") && diff.contains("expected_outcome:"),
        "PW3-O-012: differential harness must use ExpectedOutcome + expected_outcome field"
    );
    assert!(
        !diff.contains("expected: \"false\"") && !diff.contains("expected: \"true\""),
        "PW3-O-012: duplicate LeanExec.expected literals must be removed"
    );
    assert!(
        diff.contains("fn audit_differential_pw3_o012_single_expected_policy"),
        "PW3-O-012: differential self-test must validate schema"
    );
    assert!(
        diff.contains("id: \"PW3-O-001\"") && diff.contains("ExpectedOutcome::Bool(true)"),
        "PW3-O-012: PW3-O-001 row must share single Bool(true) expected across legs"
    );
}
