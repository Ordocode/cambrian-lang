// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase I Wave 4.6 — EVM expression-axis proptest (T-EVM-EX-FUZZ / EVM-EX-H2).
//!
//! Parser → validate → `check_evm_target_compat_with` → `EvmSolidityBackend` →
//! `forge build` on validator-clean mini-entities (never direct `solc`).
//!
//! See `docs/AUDIT_EVM_LEAN.md` §6 Phase I Wave 4.6.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use cambrian_transpiler::ast;
use cambrian_transpiler::codegen::{EvmSolidityBackend, OutputBackend};
use cambrian_transpiler::project::Project;
use cambrian_transpiler::validate::{
    self, check_evm_target_compat_with, validate_project_config, Diagnostic, Severity,
};
use cambrian_transpiler::ProgramParser;
use proptest::prelude::*;

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

const CAM_NAME: &str = "input.cam";
const SMOKE_CASES: usize = 18;
const FUZZ_CASES: u32 = 64;

static STAT_RUN: AtomicU64 = AtomicU64::new(0);
static STAT_SKIP: AtomicU64 = AtomicU64::new(0);
static STAT_PASS: AtomicU64 = AtomicU64::new(0);
static STAT_KNOWN_FAIL: AtomicU64 = AtomicU64::new(0);
static STAT_NEW_FAIL: AtomicU64 = AtomicU64::new(0);

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExprKind {
    BinOpAdd,
    CastU256,
    CompareEq,
    UnaryNot,
    MsgSender,
    SysTimestamp,
    PureDouble,
    MatchInt,
    PhasedRef,
}

impl ExprKind {
    const ALL: [ExprKind; 9] = [
        ExprKind::BinOpAdd,
        ExprKind::CastU256,
        ExprKind::CompareEq,
        ExprKind::UnaryNot,
        ExprKind::MsgSender,
        ExprKind::SysTimestamp,
        ExprKind::PureDouble,
        ExprKind::MatchInt,
        ExprKind::PhasedRef,
    ];

    fn slug(self) -> &'static str {
        match self {
            ExprKind::BinOpAdd => "binop",
            ExprKind::CastU256 => "cast",
            ExprKind::CompareEq => "eq",
            ExprKind::UnaryNot => "not",
            ExprKind::MsgSender => "msg",
            ExprKind::SysTimestamp => "sys",
            ExprKind::PureDouble => "pure",
            ExprKind::MatchInt => "match",
            ExprKind::PhasedRef => "phased",
        }
    }
}

fn phased_fuzz_program() -> String {
    r#"entity PhasedExpr {
    routes {
        bump(amount: u64) => [
            inc: []
            mirror: []
        ]
    }
    m_a: u64 {
        in bump(amount) => inc: m_a + amount
    }
    m_b: u64 {
        in bump(amount) => mirror: ^m_a
    }
}
"#
    .to_string()
}

#[derive(Debug, Clone)]
struct ExprFuzzCase {
    kind: ExprKind,
    lit: u64,
}

impl ExprFuzzCase {
    fn slug(&self) -> String {
        format!("{}_{}", self.kind.slug(), self.lit)
    }

    fn render_expr(&self) -> String {
        match self.kind {
            ExprKind::BinOpAdd => format!("m_count + {}", self.lit),
            ExprKind::CastU256 => format!("{} as U256", self.lit),
            ExprKind::CompareEq => format!("m_count == {}", self.lit),
            ExprKind::UnaryNot => "!m_flag".to_string(),
            ExprKind::MsgSender => "msg::sender".to_string(),
            ExprKind::SysTimestamp => "sys::timestamp".to_string(),
            ExprKind::PureDouble => format!("double({})", self.lit),
            ExprKind::MatchInt => format!("match {} {{ 0 => 1, _ => 2 }}", self.lit % 5),
            ExprKind::PhasedRef => unreachable!("PhasedRef uses phased_fuzz_program()"),
        }
    }

    fn render(&self) -> String {
        if matches!(self.kind, ExprKind::PhasedRef) {
            return phased_fuzz_program();
        }
        let pure = if matches!(self.kind, ExprKind::PureDouble) {
            "pure fn double(x: u64) -> u64 { x + x }\n\n"
        } else {
            ""
        };
        let extra_members = if matches!(self.kind, ExprKind::UnaryNot) {
            "    m_flag: bool { in constructor() => false }\n"
        } else {
            ""
        };
        let expr = self.render_expr();
        format!(
            "{pure}entity ExprFuzz {{
    routes {{
        constructor() => []
        probe(n: u64) -> u64 => [
            let x = {expr};
            return(m_count)
        ]
    }}
    m_count: u64 {{ in constructor() => 0 }}
{extra_members}}}
"
        )
    }
}

#[derive(Debug)]
enum RunOutcome {
    Skip,
    Pass,
    /// Matrix §8 finding reproduced (eq/not kinds); not a new finding ID.
    KnownFail(&'static str),
    NewFail(String),
}

struct FuzzStatsSummary;

impl Drop for FuzzStatsSummary {
    fn drop(&mut self) {
        eprintln!(
            "T-EVM-EX-FUZZ audit_evm_expr_fuzz: run={} skip={} pass={} known_fail={} new_fail={}",
            STAT_RUN.load(Ordering::Relaxed),
            STAT_SKIP.load(Ordering::Relaxed),
            STAT_PASS.load(Ordering::Relaxed),
            STAT_KNOWN_FAIL.load(Ordering::Relaxed),
            STAT_NEW_FAIL.load(Ordering::Relaxed),
        );
    }
}

fn audit_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit"))
}

fn repro_root() -> PathBuf {
    audit_root().join("evm_expr_fuzz_repro")
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-evm-expr-fuzz-{}-{}-{}",
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

fn parser() -> ProgramParser {
    ProgramParser::new()
}

fn has_blocking_errors(diags: &[Diagnostic]) -> bool {
    diags
        .iter()
        .any(|d| matches!(d.severity, Severity::Error))
}

fn collect_evm_diagnostics(project: &Project) -> Vec<Diagnostic> {
    let mut diags = validate::validate(&project.merged);
    diags.extend(validate_project_config(&project.config));
    let det = project.config.resolved_deterministic_addresses();
    diags.extend(check_evm_target_compat_with(&project.merged, det));
    diags
}

fn write_project_yaml(work_dir: &Path, project_name: &str) -> std::io::Result<()> {
    let yaml = format!(
        "name: {project_name}\n\
         target: evm\n\
         deterministic_addresses: true\n\
         output_dir: build/\n\
         sources:\n\
           - {CAM_NAME}\n"
    );
    std::fs::write(work_dir.join("project.yaml"), yaml)
}

fn load_project(cam_src: &str, project_name: &str) -> Result<Project, String> {
    let work_dir = unique_out_dir(project_name);
    let _ = std::fs::remove_dir_all(&work_dir);
    std::fs::create_dir_all(&work_dir).map_err(|e| format!("mkdir work: {e}"))?;
    std::fs::write(work_dir.join(CAM_NAME), cam_src).map_err(|e| format!("write cam: {e}"))?;
    write_project_yaml(&work_dir, project_name).map_err(|e| format!("write yaml: {e}"))?;
    Project::load(&work_dir.join("project.yaml")).map_err(|e| format!("load project: {e}"))
}

fn write_codegen_files(
    files: impl IntoIterator<Item = (String, String)>,
    out_dir: &Path,
) -> Result<(), String> {
    for (rel, contents) in files {
        let path = out_dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, contents).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

fn transpile_evm(project: &Project, out_dir: &Path) -> Result<(), String> {
    let det = project.config.resolved_deterministic_addresses();
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    write_codegen_files(backend.gen_project(project), out_dir)
}

fn strip_generated_invariant_tests(out_dir: &Path) {
    let test_dir = out_dir.join("test");
    let Ok(entries) = std::fs::read_dir(&test_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("Invariant_") && name.ends_with(".t.sol") {
            let _ = std::fs::remove_file(entry.path());
        }
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
                    parts.push(format!(
                        "// {}\n{text}",
                        path.file_name().unwrap().to_string_lossy()
                    ));
                }
            }
        }
    }
    parts.sort();
    parts.join("\n\n")
}

fn forge_build(out_dir: &Path) -> (bool, String) {
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
    (forge.status.success(), combined)
}

/// Map forge stderr to matrix findings already tracked in AUDIT_AUTHOR_REPORT §8.
fn classify_known_forge_failure(forge_output: &str) -> Option<&'static str> {
    if forge_output.contains("Explicit type conversion not allowed from \"bool\" to \"uint64\"") {
        Some("T-EVM-EX-006")
    } else if forge_output
        .contains("Type bool is not implicitly convertible to expected type uint256")
    {
        Some("T-EVM-EX-007")
    } else if forge_output.contains("next_m_a") {
        Some("T-EVM-EX-013")
    } else {
        None
    }
}

fn save_repro(case: &ExprFuzzCase, cam: &str, sol: &str, forge_output: &str, tag: &str) -> PathBuf {
    let dir = repro_root().join(case.slug());
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create repro dir");
    std::fs::write(dir.join("repro.cam"), cam).expect("write repro.cam");
    write_project_yaml(&dir, &format!("evm-expr-fuzz-{}", case.slug())).expect("write yaml");
    std::fs::write(dir.join("generated.sol"), sol).expect("write generated.sol");
    let note = format!(
        "# T-EVM-EX-FUZZ / EVM-EX-H2 repro ({tag})\n\n\
         Kind: {:?}\n\
         Literal: {}\n\n\
         --- forge output ---\n\
         {forge_output}\n",
        case.kind, case.lit
    );
    std::fs::write(dir.join("NOTE.md"), note).expect("write NOTE.md");
    dir
}

fn run_expr_fuzz_case(case: &ExprFuzzCase) -> RunOutcome {
    STAT_RUN.fetch_add(1, Ordering::Relaxed);
    let cam_src = case.render();

    let program = match parser().parse(&cam_src) {
        Ok(mut p) => {
            ast::normalize_program_types(&mut p);
            p
        }
        Err(e) => {
            STAT_SKIP.fetch_add(1, Ordering::Relaxed);
            eprintln!("skip parse: {e}");
            return RunOutcome::Skip;
        }
    };

    let diags = validate::validate(&program);
    if has_blocking_errors(&diags) {
        STAT_SKIP.fetch_add(1, Ordering::Relaxed);
        return RunOutcome::Skip;
    }

    let project = match load_project(&cam_src, &case.slug()) {
        Ok(p) => p,
        Err(e) => {
            STAT_SKIP.fetch_add(1, Ordering::Relaxed);
            eprintln!("skip project load: {e}");
            return RunOutcome::Skip;
        }
    };

    let evm_diags = collect_evm_diagnostics(&project);
    if has_blocking_errors(&evm_diags) {
        STAT_SKIP.fetch_add(1, Ordering::Relaxed);
        return RunOutcome::Skip;
    }

    let out_dir = unique_out_dir(&case.slug());
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    if let Err(e) = transpile_evm(&project, &out_dir) {
        let _ = std::fs::remove_dir_all(&out_dir);
        return RunOutcome::NewFail(format!("codegen failed: {e}"));
    }

    strip_generated_invariant_tests(&out_dir);
    if !out_dir.join("foundry.toml").exists() {
        std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).expect("foundry.toml");
    }

    let sol = read_transpiled_sol(&out_dir);
    let (ok, forge_output) = forge_build(&out_dir);
    let _ = std::fs::remove_dir_all(&out_dir);

    if ok {
        STAT_PASS.fetch_add(1, Ordering::Relaxed);
        return RunOutcome::Pass;
    }

    if let Some(known_id) = classify_known_forge_failure(&forge_output) {
        STAT_KNOWN_FAIL.fetch_add(1, Ordering::Relaxed);
        let repro_dir = save_repro(case, &cam_src, &sol, &forge_output, known_id);
        eprintln!(
            "known forge failure {known_id} (matrix §8): repro {}",
            repro_dir.display()
        );
        return RunOutcome::KnownFail(known_id);
    }

    STAT_NEW_FAIL.fetch_add(1, Ordering::Relaxed);
    let repro_dir = save_repro(case, &cam_src, &sol, &forge_output, "NEW");
    RunOutcome::NewFail(format!(
        "T-EVM-EX-FUZZ CONFIRMED — new forge failure; repro saved to {}\n{}",
        repro_dir.display(),
        &forge_output.chars().take(2048).collect::<String>()
    ))
}

fn reset_stats() {
    STAT_RUN.store(0, Ordering::Relaxed);
    STAT_SKIP.store(0, Ordering::Relaxed);
    STAT_PASS.store(0, Ordering::Relaxed);
    STAT_KNOWN_FAIL.store(0, Ordering::Relaxed);
    STAT_NEW_FAIL.store(0, Ordering::Relaxed);
}

fn arb_expr_fuzz_case() -> impl Strategy<Value = ExprFuzzCase> {
    (
        prop::sample::select(ExprKind::ALL.as_slice()),
        0u64..=999u64,
    )
        .prop_map(|(kind, lit)| ExprFuzzCase { kind, lit })
}

fn smoke_cases() -> Vec<ExprFuzzCase> {
    let mut cases = Vec::with_capacity(SMOKE_CASES);
    for (i, kind) in ExprKind::ALL.iter().enumerate() {
        for lit in [0u64, 7u64] {
            cases.push(ExprFuzzCase {
                kind: *kind,
                lit: lit + i as u64,
            });
        }
    }
    debug_assert_eq!(cases.len(), SMOKE_CASES);
    cases
}

fn handle_outcome(outcome: RunOutcome) {
    if let RunOutcome::NewFail(detail) = outcome {
        panic!("{detail}");
    }
}

#[test]
fn audit_evm_expr_fuzz() {
    if !has_forge() {
        eprintln!(
            "skipping audit_evm_expr_fuzz: forge not on PATH \
             (coverage / non-Foundry CI images; Forge execution lives in \
             test-transpiler-audit-evm-expr)"
        );
        return;
    }
    let config = ProptestConfig {
        cases: FUZZ_CASES,
        ..ProptestConfig::default()
    };
    proptest!(config, |(case in arb_expr_fuzz_case())| {
        let _summary = FuzzStatsSummary;
        handle_outcome(run_expr_fuzz_case(&case));
    });
}

#[test]
fn audit_evm_expr_fuzz_smoke() {
    if !has_forge() {
        eprintln!("skipping audit_evm_expr_fuzz_smoke: forge not on PATH");
        return;
    }
    reset_stats();
    let _summary = FuzzStatsSummary;
    for case in smoke_cases() {
        let outcome = run_expr_fuzz_case(&case);
        eprintln!("smoke {:?} lit={}: {:?}", case.kind, case.lit, outcome);
        handle_outcome(outcome);
    }
    assert_eq!(
        STAT_NEW_FAIL.load(Ordering::Relaxed),
        0,
        "T-EVM-EX-FUZZ smoke: unexpected new forge failures"
    );
}
