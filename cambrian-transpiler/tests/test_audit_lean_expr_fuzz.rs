// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase I Wave 4.3 — Lean expression-axis proptest (T-LEAN-EX-FUZZ / LEAN-EX-H4).
//!
//! Parser → validate → `check_lean_target_compat` → `LeanBackend::gen_project` →
//! sentinel scan on generated Lean (no `lake build`).
//!
//! See `docs/AUDIT_EVM_LEAN.md` §6 Phase I Wave 4.3.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use cambrian_transpiler::ast;
use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::validate::{self, check_lean_target_compat, Diagnostic, Severity};
use cambrian_transpiler::ProgramParser;
use proptest::prelude::*;

const SILENT_SENTINELS: &[&str] = &[
    "-- skipped:",
    "-- internal: effectful",
    "-- bug:",
    "-- L8:",
    "Cambrian.Unsupported",
    ", default)",
];

const SMOKE_CASES: usize = 18;
const FUZZ_CASES: u32 = 64;

static STAT_RUN: AtomicU64 = AtomicU64::new(0);
static STAT_SKIP: AtomicU64 = AtomicU64::new(0);
static STAT_PASS: AtomicU64 = AtomicU64::new(0);
static STAT_FAIL: AtomicU64 = AtomicU64::new(0);

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
    /// Phased member transform `^m_a` (matrix T-LEAN/EVM-EX-013).
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

struct FuzzStatsSummary;

impl Drop for FuzzStatsSummary {
    fn drop(&mut self) {
        eprintln!(
            "T-LEAN-EX-FUZZ audit_lean_expr_fuzz: run={} skip={} pass={} fail={}",
            STAT_RUN.load(Ordering::Relaxed),
            STAT_SKIP.load(Ordering::Relaxed),
            STAT_PASS.load(Ordering::Relaxed),
            STAT_FAIL.load(Ordering::Relaxed),
        );
    }
}

fn audit_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit"))
}

fn repro_root() -> PathBuf {
    audit_root().join("lean_expr_fuzz_repro")
}

fn parser() -> ProgramParser {
    ProgramParser::new()
}

fn has_blocking_errors(diags: &[Diagnostic]) -> bool {
    diags
        .iter()
        .any(|d| matches!(d.severity, Severity::Error))
}

fn concat_generated_lean(program: &ast::Program) -> String {
    let backend = LeanBackend::default();
    let entity_name = program
        .entities
        .first()
        .map(|e| e.name.as_str())
        .unwrap_or("ExprFuzz");
    let mut text = backend.gen_program(program);
    for (path, content) in backend.extra_files(program, entity_name) {
        if path.ends_with(".lean") && !path.ends_with("Spec.lean") {
            text.push_str(&content);
            text.push('\n');
        }
    }
    text
}

fn find_sentinel(lean_text: &str) -> Option<&'static str> {
    SILENT_SENTINELS.iter().copied().find(|s| lean_text.contains(s))
}

fn save_repro(case: &ExprFuzzCase, cam: &str, sentinel: &str, lean_text: &str) -> PathBuf {
    let dir = repro_root().join(case.slug());
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create repro dir");
    std::fs::write(dir.join("repro.cam"), cam).expect("write repro.cam");
    std::fs::write(dir.join("generated.lean"), lean_text).expect("write generated.lean");
    let note = format!(
        "# T-LEAN-EX-FUZZ / LEAN-EX-H4 repro\n\n\
         Sentinel: `{sentinel}`\n\
         Kind: {:?}\n\
         Literal: {}\n",
        case.kind, case.lit
    );
    std::fs::write(dir.join("NOTE.md"), note).expect("write NOTE.md");
    dir
}

fn run_expr_fuzz_case(case: &ExprFuzzCase) -> Result<(), String> {
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
            return Ok(());
        }
    };

    let diags = validate::validate(&program);
    if has_blocking_errors(&diags) {
        STAT_SKIP.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }

    let lean_diags = check_lean_target_compat(&program);
    if has_blocking_errors(&lean_diags) {
        STAT_SKIP.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }

    let lean_text = concat_generated_lean(&program);

    if let Some(sentinel) = find_sentinel(&lean_text) {
        STAT_FAIL.fetch_add(1, Ordering::Relaxed);
        let repro_dir = save_repro(case, &cam_src, sentinel, &lean_text);
        return Err(format!(
            "T-LEAN-EX-FUZZ CONFIRMED — sentinel `{sentinel}` in Lean output; repro saved to {}\n\
             (first 2k chars of generated lean)\n{}",
            repro_dir.display(),
            &lean_text.chars().take(2048).collect::<String>()
        ));
    }

    STAT_PASS.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

fn reset_stats() {
    STAT_RUN.store(0, Ordering::Relaxed);
    STAT_SKIP.store(0, Ordering::Relaxed);
    STAT_PASS.store(0, Ordering::Relaxed);
    STAT_FAIL.store(0, Ordering::Relaxed);
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

proptest! {
    #![proptest_config(ProptestConfig {
        cases: FUZZ_CASES,
        .. ProptestConfig::default()
    })]

    #[test]
    fn audit_lean_expr_fuzz(case in arb_expr_fuzz_case()) {
        let _summary = FuzzStatsSummary;
        if let Err(detail) = run_expr_fuzz_case(&case) {
            panic!("{detail}");
        }
    }
}

#[test]
fn audit_lean_expr_fuzz_smoke() {
    reset_stats();
    let _summary = FuzzStatsSummary;
    for case in smoke_cases() {
        run_expr_fuzz_case(&case).unwrap_or_else(|e| panic!("smoke case {:?}: {e}", case.kind));
    }
}
