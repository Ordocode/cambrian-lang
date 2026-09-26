// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase I Wave 4 — Lean expression-axis audit matrix.
//!
//! Complements `test_lean_matrix.rs` (statement / `RouteAction` axis) with a
//! generative grid over **expression forms × placement** (route `let`,
//! member transform, `where`, view `return`, pure-fn body). Faithful cells
//! must not leak silent sentinels; optional `lake build` catches ill-typed
//! lowerings string scans miss.
//!
//! See `docs/AUDIT_EVM_LEAN.md` §6 Phase I Wave 4.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::validate::check_lean_target_compat;
use cambrian_transpiler::ProgramParser;

fn parse(src: &str) -> cambrian_transpiler::ast::Program {
    ProgramParser::new()
        .parse(src)
        .expect("parse test program")
}

const SILENT_SENTINELS: &[&str] = &[
    "-- skipped:",
    "-- internal: effectful",
    "-- bug:",
    "-- L8:",
    "Cambrian.Unsupported",
    ", default)",
];

enum Expect {
    Faithful(&'static [&'static str]),
    /// Faithful lowering with needles expected in `*Spec.lean` (invariants).
    FaithfulWithSpec {
        code: &'static [&'static str],
        spec: &'static [&'static str],
    },
    /// Validator must reject before codegen (no silent Unsupported in output).
    ValidatorError(&'static str),
}

struct ExprCell {
    id: &'static str,
    hypothesis: &'static str,
    program: String,
    expect: Expect,
}

fn program(members: &str, routes: &str) -> String {
    format!(
        "entity ExprProbe {{\n\
         routes {{\n\
             constructor() => []\n\
             {routes}\n\
         }}\n\
         m_count: u64 {{ in constructor() => 0 }}\n\
         {members}\n\
         }}\n"
    )
}

fn program_with_pure(pure_decls: &str, members: &str, routes: &str) -> String {
    format!(
        "{pure_decls}\n\
         entity ExprProbe {{\n\
         routes {{\n\
             constructor() => []\n\
             {routes}\n\
         }}\n\
         m_count: u64 {{ in constructor() => 0 }}\n\
         {members}\n\
         }}\n"
    )
}

fn phased_program() -> String {
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

fn map_exists_program() -> String {
    r#"entity MapExpr {
    routes {
        constructor() => []
        ping(k: u64) => []
    }
    m_map: HashMap<u64, u64> {
        in ping(k) => m_map
    }
    m_hit: bool {
        in constructor() => false
        in ping(k) => m_map.exists(k)
    }
}
"#
    .to_string()
}

fn record_program() -> String {
    r#"entity ExprProbe {
    record Pair {
        a: u64,
        b: u64
    }
    routes {
        constructor() => []
        setup(a: u64, b: u64) => []
        probe() -> u64 => [ let x = m_pair.a; return(x) ]
    }
    m_pair: Pair {
        in setup(a, b) => {
            Pair { a: a, b: b }
        }
    }
}
"#
    .to_string()
}

fn derived_program() -> String {
    r#"entity ExprProbe {
    routes {
        constructor() => []
        routeA() => []
    }
    m_count: u64 { in constructor() => 0 }
    m_extra: u64 { in constructor() => 1 }
}

invariant "derived total" for ExprProbe {
    init { m_count: 0, m_extra: 1 }

    derived total() -> u64 {
        return m_count + m_extra
    }

    action routeA() {
        assume total() <= 1000
    }

    check total() >= m_count
}
"#
    .to_string()
}

fn trace_program() -> String {
    r#"entity ExprProbe {
    routes {
        constructor() => []
        routeA() => []
        routeB() => []
    }
    m_count: u64 {
        in constructor() => 0
        in routeA() => m_count + 1
    }
}

invariant "trace bounded" for ExprProbe {
    init { m_count: 0 }

    action routeA() {
        assume trace::count(routeA) <= 5
    }
    action routeB() {}

    check m_count >= 0
}
"#
    .to_string()
}

fn where_msg_program() -> String {
    program(
        "",
        "auth(a: address) where (msg::sender == a) : throw 1 => []",
    )
}

fn transform_let_program() -> String {
    program(
        "m_total: u64 { in add(n) => add: { let t = m_count + n; t } }",
        "add(n: u64) => [ add: [] ]",
    )
}

fn cells() -> Vec<ExprCell> {
    let mut v = Vec::new();

    // ---- Binary ops -------------------------------------------------------
    v.push(ExprCell {
        id: "T-LEAN-EX-001",
        hypothesis: "LEAN-EX-H1",
        program: program("", "add(n: u64) -> u64 => [ let t = m_count + n; return(t) ]"),
        expect: Expect::Faithful(&["m_count +"]),
    });
    v.push(ExprCell {
        id: "T-LEAN-EX-002",
        hypothesis: "LEAN-EX-H1",
        program: program(
            "m_total: u64 { in add(n) => add: m_total + n }",
            "add(n: u64) => [ add: [] ]",
        ),
        expect: Expect::Faithful(&["m_total +"]),
    });
    v.push(ExprCell {
        id: "T-LEAN-EX-003",
        hypothesis: "LEAN-EX-H1",
        program: program(
            "",
            "guard(n: u64) where (m_count + n > 0) : throw 1 => []",
        ),
        expect: Expect::Faithful(&["m_count +"]),
    });
    v.push(ExprCell {
        id: "T-LEAN-EX-004",
        hypothesis: "LEAN-EX-H1",
        program: program("", "view_plus() -> u64 => [ return(m_count + 1) ]"),
        expect: Expect::Faithful(&["m_count +"]),
    });

    // ---- Casts / compares -------------------------------------------------
    v.push(ExprCell {
        id: "T-LEAN-EX-005",
        hypothesis: "LEAN-EX-H1",
        program: program("", "widen(n: u64) -> U256 => [ let w = n as U256; return(w) ]"),
        expect: Expect::Faithful(&["Cambrian.castWidth"]),
    });
    v.push(ExprCell {
        id: "T-LEAN-EX-006",
        hypothesis: "LEAN-EX-H1",
        program: program("", "eq(n: u64) -> bool => [ let b = m_count == n; return(b) ]"),
        expect: Expect::Faithful(&["m_count =="]),
    });

    // ---- Bool / bitwise ---------------------------------------------------
    v.push(ExprCell {
        id: "T-LEAN-EX-007",
        hypothesis: "LEAN-EX-H1",
        program: program(
            "m_flag: bool { in constructor() => false }",
            "flip() -> bool => [ let b = !m_flag; return(b) ]",
        ),
        expect: Expect::Faithful(&["!"]),
    });

    // ---- HashMap .exists in member transform ---------------------------
    v.push(ExprCell {
        id: "T-LEAN-EX-008",
        hypothesis: "LEAN-EX-H1",
        program: map_exists_program(),
        expect: Expect::Faithful(&["AddressMap.contains"]),
    });

    // ---- Match in let -----------------------------------------------------
    v.push(ExprCell {
        id: "T-LEAN-EX-009",
        hypothesis: "LEAN-EX-H1",
        program: program(
            "",
            "classify(n: u64) -> u64 => [ let c = match n { 0 => 1, _ => 2 }; return(c) ]",
        ),
        expect: Expect::Faithful(&["match"]),
    });

    // ---- Context fields ---------------------------------------------------
    v.push(ExprCell {
        id: "T-LEAN-EX-010",
        hypothesis: "LEAN-EX-H1",
        program: program("", "who() -> address => [ let s = msg::sender; return(s) ]"),
        expect: Expect::Faithful(&["ctx.sender"]),
    });
    v.push(ExprCell {
        id: "T-LEAN-EX-011",
        hypothesis: "LEAN-EX-H1",
        program: program("", "now() -> u64 => [ let t = sys::timestamp; return(t) ]"),
        expect: Expect::Faithful(&["block.timestamp"]),
    });

    // ---- Pure fn ----------------------------------------------------------
    v.push(ExprCell {
        id: "T-LEAN-EX-012",
        hypothesis: "LEAN-EX-H1",
        program: program_with_pure(
            "pure fn double(x: u64) -> u64 { x + x }",
            "",
            "dbl(n: u64) -> u64 => [ let t = double(n); return(t) ]",
        ),
        expect: Expect::Faithful(&["Cambrian.Generated.Pure.double"]),
    });

    // ---- Phased temporal ref (^m_a) ---------------------------------------
    v.push(ExprCell {
        id: "T-LEAN-EX-013",
        hypothesis: "LEAN-EX-H1",
        program: phased_program(),
        expect: Expect::Faithful(&["s.m_a"]),
    });

    // ---- Wave 4.5 grid expansion ------------------------------------------
    v.push(ExprCell {
        id: "T-LEAN-EX-015",
        hypothesis: "LEAN-EX-H1",
        program: record_program(),
        expect: Expect::Faithful(&["(s.m_pair).a"]),
    });
    v.push(ExprCell {
        id: "T-LEAN-EX-016",
        hypothesis: "LEAN-EX-H1",
        program: derived_program(),
        expect: Expect::FaithfulWithSpec {
            code: &[],
            spec: &["def total", "≤ 1000"],
        },
    });
    v.push(ExprCell {
        id: "T-LEAN-EX-017",
        hypothesis: "LEAN-EX-H1",
        program: trace_program(),
        expect: Expect::FaithfulWithSpec {
            code: &[],
            spec: &["TraceAcc", "count_routeA"],
        },
    });

    // ---- Wave 4.7 micro-regions (where / transform placements) ------------
    v.push(ExprCell {
        id: "T-LEAN-EX-018",
        hypothesis: "LEAN-EX-H1",
        program: where_msg_program(),
        expect: Expect::Faithful(&["ctx.sender"]),
    });
    v.push(ExprCell {
        id: "T-LEAN-EX-019",
        hypothesis: "LEAN-EX-H1",
        program: transform_let_program(),
        expect: Expect::Faithful(&["let t := (s.m_count + n)"]),
    });

    // ---- Intentionally unsupported (documented marker) --------------------
    v.push(ExprCell {
        id: "T-LEAN-EX-014",
        hypothesis: "LEAN-EX-H3",
        program: program("", "bad() -> u64 => [ let f = |x| x + 1; return(f(1)) ]"),
        expect: Expect::ValidatorError("L2"),
    });

    // ---- Phase L std::math placements (STD-H-PLAC-1 expr axis) ------------
    v.push(ExprCell {
        id: "T-STD-LEAN-EXPR-001a",
        hypothesis: "STD-H-PLAC-1",
        program: program("", "add(n: u64) -> u64 => [ let t = std::math::min(m_count, n); return(t) ]"),
        expect: Expect::Faithful(&["s.m_count < n"]),
    });
    v.push(ExprCell {
        id: "T-STD-LEAN-EXPR-001b",
        hypothesis: "STD-H-PLAC-1",
        program: program(
            "m_total: u64 { in constructor() => 0 in add(n) => add: std::math::min(m_total, n) }",
            "add(n: u64) => [ add: [] ]",
        ),
        expect: Expect::Faithful(&["s.m_total < n"]),
    });

    v
}

fn transpiler_bin() -> PathBuf {
    // `CARGO_BIN_EXE_*` rather than a walk up from `current_exe()`: the two
    // are the same path under the classic `target/debug/deps` layout, but a
    // cargo configured with a split build directory puts the test executable
    // somewhere else entirely, and the walk then points at a file that does
    // not exist. The failure reads as "No such file or directory" from the
    // spawn, which names neither the binary nor the layout.
    PathBuf::from(env!("CARGO_BIN_EXE_cambrian-transpiler"))
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(stem: &str) -> PathBuf {
    // Parallel tests share a pid and several cells share an id (smoke + faithful),
    // so the directory must be unique per call.
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "cambrian-audit-lean-expr-{}-{}-{}",
        stem.replace('/', "_"),
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn transpile(cell: &ExprCell) -> (PathBuf, String, String) {
    let dir = tempdir(cell.id);
    let cam = dir.join("input.cam");
    std::fs::write(&cam, &cell.program).expect("write cam");
    let out = dir.join("out");
    let result = Command::new(transpiler_bin())
        .arg(&cam)
        .arg("-o")
        .arg(&out)
        .arg("--target")
        .arg("lean")
        .output()
        .expect("invoke transpiler");
    assert!(
        result.status.success(),
        "[{}] transpile failed:\nprogram:\n{}\nstdout:\n{}\nstderr:\n{}",
        cell.id,
        cell.program,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );

    let mut text = String::new();
    let mut spec_text = String::new();
    let mut stack = vec![out.clone()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("lean") {
                if let Ok(t) = std::fs::read_to_string(&p) {
                    if p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with("Spec.lean"))
                    {
                        spec_text.push_str(&t);
                        spec_text.push('\n');
                    } else {
                        text.push_str(&t);
                        text.push('\n');
                    }
                }
            }
        }
    }
    (out, text, spec_text)
}

fn check_faithful_output(
    cell: &ExprCell,
    text: &str,
    spec_text: &str,
    code_needles: &[&str],
    spec_needles: &[&str],
) -> Vec<String> {
    let mut failures = Vec::new();
    for s in SILENT_SENTINELS {
        if text.contains(s) || spec_text.contains(s) {
            failures.push(format!(
                "[{} / {}] FAITHFUL cell leaked sentinel {:?}",
                cell.id, cell.hypothesis, s
            ));
        }
    }
    for needle in code_needles {
        if !text.contains(needle) {
            failures.push(format!(
                "[{} / {}] missing expected lowering {:?} in generated Lean (non-Spec)",
                cell.id, cell.hypothesis, needle
            ));
        }
    }
    for needle in spec_needles {
        if !spec_text.contains(needle) {
            failures.push(format!(
                "[{} / {}] missing expected lowering {:?} in *Spec.lean",
                cell.id, cell.hypothesis, needle
            ));
        }
    }
    failures
}

fn check_cell(cell: &ExprCell) -> Vec<String> {
    let mut failures = Vec::new();
    match &cell.expect {
        Expect::ValidatorError(code) => {
            let prog = parse(&cell.program);
            let diags = check_lean_target_compat(&prog);
            if !diags.iter().any(|d| d.code == *code) {
                failures.push(format!(
                    "[{} / {}] expected validator [{}], got {:?}",
                    cell.id,
                    cell.hypothesis,
                    code,
                    diags
                ));
            }
            return failures;
        }
        Expect::Faithful(needles) => {
            let (_out, text, spec_text) = transpile(cell);
            failures.extend(check_faithful_output(
                cell, &text, &spec_text, needles, &[],
            ));
        }
        Expect::FaithfulWithSpec { code, spec } => {
            let (_out, text, spec_text) = transpile(cell);
            failures.extend(check_faithful_output(
                cell, &text, &spec_text, code, spec,
            ));
        }
    }
    failures
}

#[test]
fn audit_lean_expr_matrix_smoke() {
    // CI-003 smoke: faithful binop + record + unsupported cells.
    let ids = ["T-LEAN-EX-001", "T-LEAN-EX-015", "T-LEAN-EX-014"];
    let mut failures = Vec::new();
    for cell in cells().iter().filter(|c| ids.contains(&c.id)) {
        failures.extend(check_cell(cell));
    }
    assert!(
        failures.is_empty(),
        "Wave 4 expression-axis smoke failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn audit_lean_expr_matrix_faithful() {
    let mut failures = Vec::new();
    for cell in cells() {
        failures.extend(check_cell(&cell));
    }
    if !failures.is_empty() {
        eprintln!("=== Lean expression-axis audit report ===");
        for cell in cells() {
            eprintln!("{} / {}: {}", cell.id, cell.hypothesis, cell.id);
        }
    }
    assert!(
        failures.is_empty(),
        "Wave 4 expression-axis matrix failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn audit_lean_expr_std_math_smoke() {
    let ids = ["T-STD-LEAN-EXPR-001a", "T-STD-LEAN-EXPR-001b"];
    let mut failures = Vec::new();
    for cell in cells().iter().filter(|c| ids.contains(&c.id)) {
        failures.extend(check_cell(cell));
    }
    assert!(
        failures.is_empty(),
        "STD-H-PLAC-1 Lean expr-axis std::math smoke failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn audit_lean_expr_matrix_lake_build() {
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() != Ok("1") {
        eprintln!("skipping audit_lean_expr_matrix_lake_build (set CAMBRIAN_TEST_LEAN_BUILD=1)");
        return;
    }
    let lake_ok = Command::new("lake")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(
        lake_ok,
        "CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH"
    );

    let mut failures = Vec::new();
    for cell in cells() {
        if !matches!(
            cell.expect,
            Expect::Faithful(_) | Expect::FaithfulWithSpec { .. }
        ) {
            continue;
        }
        let (out, _text, _spec) = transpile(&cell);
        let build = Command::new("lake")
            .arg("build")
            .current_dir(&out)
            .output()
            .expect("invoke lake");
        if !build.status.success() {
            let stdout = String::from_utf8_lossy(&build.stdout);
            let stderr = String::from_utf8_lossy(&build.stderr);
            failures.push(format!(
                "[{} / {}] lake build failed:\nstdout:\n{stdout}\nstderr:\n{stderr}",
                cell.id, cell.hypothesis,
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "Wave 4 expression-axis lake-build failures:\n{}",
        failures.join("\n")
    );
}
