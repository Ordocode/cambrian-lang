// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase J Wave 5.4 — Lean statement-axis proptest (T-LEAN-ST-FUZZ / LEAN-ST-H4).
//!
//! Faithful action placements → validate → codegen sentinel scan.
//! Known ill-typed patterns (LEAN-ST-H1) are skipped (covered by matrix lake gate).
//!
//! See `docs/AUDIT_EVM_LEAN.md` §6 Wave 5.4.

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

const SMOKE_CASES: usize = 14;
const FUZZ_CASES: u32 = 48;

static STAT_RUN: AtomicU64 = AtomicU64::new(0);
static STAT_SKIP: AtomicU64 = AtomicU64::new(0);
static STAT_PASS: AtomicU64 = AtomicU64::new(0);
static STAT_FAIL: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StmtKind {
    ThrowTop,
    ThrowIf,
    EmitTop,
    EmitIf,
    SendTop,
    ReturnIf,
    ForThrowIf,
}

impl StmtKind {
    const ALL: [StmtKind; 7] = [
        StmtKind::ThrowTop,
        StmtKind::ThrowIf,
        StmtKind::EmitTop,
        StmtKind::EmitIf,
        StmtKind::SendTop,
        StmtKind::ReturnIf,
        StmtKind::ForThrowIf,
    ];

    fn slug(self) -> &'static str {
        match self {
            StmtKind::ThrowTop => "throw_top",
            StmtKind::ThrowIf => "throw_if",
            StmtKind::EmitTop => "emit_top",
            StmtKind::EmitIf => "emit_if",
            StmtKind::SendTop => "send_top",
            StmtKind::ReturnIf => "return_if",
            StmtKind::ForThrowIf => "for_throw_if",
        }
    }
}

#[derive(Debug, Clone)]
struct StmtFuzzCase {
    kind: StmtKind,
    lit: u64,
}

impl StmtFuzzCase {
    fn slug(&self) -> String {
        format!("{}_{}", self.kind.slug(), self.lit)
    }

    fn render(&self) -> String {
        let n = self.lit % 100;
        let decls = if matches!(self.kind, StmtKind::EmitTop | StmtKind::EmitIf) {
            "event Ev(x: u64);\n"
        } else {
            ""
        };
        let route = match self.kind {
            StmtKind::ThrowTop => "fThrow() => [ throw 7 ]".to_string(),
            StmtKind::ThrowIf => format!("fThrowIf(n: u64) => [ if n > {n} => [ throw 7 ] ]"),
            StmtKind::EmitTop => "fEmit(n: u64) => [ emit Ev(n); ]".to_string(),
            StmtKind::EmitIf => format!("fEmitIf(n: u64) => [ if n > {n} => [ emit Ev(n); ] ]"),
            StmtKind::SendTop => {
                format!("fSend(dest: address) => [ ~> dest with {{ value: {n} }} ]")
            }
            StmtKind::ReturnIf => {
                format!("fRetIf(n: u64) -> u64 => [ if n > {n} => [ return(n) ] else [ return(0) ] ]")
            }
            StmtKind::ForThrowIf => format!(
                "fThrowFor(items: Vec<u64>) => [ for x in items => [ if x > {n} => [ throw 7 ] ] ]"
            ),
        };
        format!(
            "{decls}entity StmtFuzz {{
    routes {{
        init create() => []
        {route}
    }}
    m_count: u64 {{ in create() => 0 }}
}}
"
        )
    }
}

struct FuzzStatsSummary;

impl Drop for FuzzStatsSummary {
    fn drop(&mut self) {
        eprintln!(
            "T-LEAN-ST-FUZZ audit_lean_stmt_fuzz: run={} skip={} pass={} fail={}",
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
    audit_root().join("lean_stmt_fuzz_repro")
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
        .unwrap_or("StmtFuzz");
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

fn save_repro(case: &StmtFuzzCase, cam: &str, sentinel: &str, lean_text: &str) -> PathBuf {
    let dir = repro_root().join(case.slug());
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create repro dir");
    std::fs::write(dir.join("repro.cam"), cam).expect("write repro.cam");
    std::fs::write(dir.join("generated.lean"), lean_text).expect("write generated.lean");
    let note = format!(
        "# T-LEAN-ST-FUZZ repro\n\n\
         Sentinel: `{sentinel}`\n\
         Kind: {:?}\n\
         Literal: {}\n",
        case.kind, case.lit
    );
    std::fs::write(dir.join("NOTE.md"), note).expect("write NOTE.md");
    dir
}

fn run_stmt_fuzz_case(case: &StmtFuzzCase) -> Result<(), String> {
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

    if has_blocking_errors(&validate::validate(&program)) {
        STAT_SKIP.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }

    if has_blocking_errors(&check_lean_target_compat(&program)) {
        STAT_SKIP.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }

    let lean_text = concat_generated_lean(&program);

    if let Some(sentinel) = find_sentinel(&lean_text) {
        STAT_FAIL.fetch_add(1, Ordering::Relaxed);
        let repro_dir = save_repro(case, &cam_src, sentinel, &lean_text);
        return Err(format!(
            "T-LEAN-ST-FUZZ CONFIRMED — sentinel `{sentinel}`; repro {}\n{}",
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

fn arb_stmt_fuzz_case() -> impl Strategy<Value = StmtFuzzCase> {
    (
        prop::sample::select(StmtKind::ALL.as_slice()),
        0u64..=999u64,
    )
        .prop_map(|(kind, lit)| StmtFuzzCase { kind, lit })
}

fn smoke_cases() -> Vec<StmtFuzzCase> {
    let mut cases = Vec::with_capacity(SMOKE_CASES);
    for (i, kind) in StmtKind::ALL.iter().enumerate() {
        for lit in [0u64, 5u64] {
            cases.push(StmtFuzzCase {
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
    fn audit_lean_stmt_fuzz(case in arb_stmt_fuzz_case()) {
        let _summary = FuzzStatsSummary;
        if let Err(detail) = run_stmt_fuzz_case(&case) {
            panic!("{detail}");
        }
    }
}

#[test]
fn audit_lean_stmt_fuzz_smoke() {
    reset_stats();
    let _summary = FuzzStatsSummary;
    for case in smoke_cases() {
        run_stmt_fuzz_case(&case).unwrap_or_else(|e| panic!("smoke {:?}: {e}", case.kind));
    }
}
