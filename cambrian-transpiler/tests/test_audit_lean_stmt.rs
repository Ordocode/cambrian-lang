// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase J Wave 5 — Lean statement / action-axis audit matrix (T-LEAN-ST-*).
//!
//! Extends `test_lean_matrix.rs` with manual-review cells (LEAN-ST-H1/H2).
//!
//! See `docs/AUDIT_PHASE_J.md` and `docs/AUDIT_EVM_LEAN.md` §6 Wave 5.

use std::path::PathBuf;
use std::process::Command;

use cambrian_transpiler::validate::{check_lean_target_compat, Diagnostic, Severity};
use cambrian_transpiler::ProgramParser;

const SILENT_SENTINELS: &[&str] = &[
    "-- skipped:",
    "-- internal: effectful",
    "-- bug:",
    "-- L8:",
    "Cambrian.Unsupported",
    // Wrong state-only view fallback (not the intentional `(w, default)`
    // product fill for escaping `var` in world-threaded `if`).
    "(s, default)",
];

enum Expect {
    Faithful(&'static [&'static str]),
    /// Formerly CONFIRMED lake-red cells — now FIXED; still skip strict
    /// validator on the cell program when needed.
    ConfirmedLake(&'static [&'static str]),
}

struct StmtCell {
    id: &'static str,
    hypothesis: &'static str,
    matrix_mirror: &'static str,
    program: String,
    expect: Expect,
}

fn parse(src: &str) -> cambrian_transpiler::ast::Program {
    ProgramParser::new()
        .parse(src)
        .expect("parse test program")
}

fn program(decls: &str, route: &str) -> String {
    format!(
        "{decls}entity M {{\n\
         routes {{\n\
             init create() => []\n\
             {route}\n\
         }}\n\
         m_count: u64 {{ in create() => 0 }}\n\
         }}\n"
    )
}

fn for_bare_program() -> String {
    program(
        "",
        "fDoStuff(items: Vec<u64>) -> u64 => [\n\
             for x in items => [ let y = x + 1; if y > 0 => [ ] ]\n\
             return(0)\n\
         ]",
    )
}

fn for_phased_program() -> String {
    program(
        "",
        "fPhased(items: Vec<u64>) -> u64 => [\n\
             p1: [ for x in items => [ let y = x + 1; if y > 0 => [ ] ] return(0) ]\n\
         ]",
    )
}

fn let_escapes_if_program() -> String {
    program(
        "",
        "check(flag: bool) -> u64 => [\n\
             if flag => [\n\
                 let y = 5;\n\
                 return(y)\n\
             ]\n\
             return(0)\n\
         ]",
    )
}

fn var_escapes_if_program() -> String {
    r#"extern entity Oracle {
    view route isAlive() -> bool;
}

entity Caller {
    routes {
        constructor(oracle: Address<Oracle>) => []
        check(flag: bool) -> bool => [
            if flag => [ var alive = isAlive() ~> m_oracle; ]
            return(alive)
        ]
    }
    m_oracle: Address<Oracle> { in constructor(oracle) => oracle }
}
"#
    .to_string()
}

fn cells() -> Vec<StmtCell> {
    vec![
        StmtCell {
            id: "T-LEAN-ST-001",
            hypothesis: "LEAN-ST-H1",
            matrix_mirror: "—",
            program: for_bare_program(),
            expect: Expect::Faithful(&["elided: effect-free for-loop"]),
        },
        StmtCell {
            id: "T-LEAN-ST-002",
            hypothesis: "LEAN-ST-H2",
            matrix_mirror: "—",
            program: var_escapes_if_program(),
            // Skip Lean L1 on `Address<Oracle>` (same as pre-fix ConfirmedLake);
            // lake + product-merge needles still assert the fix.
            expect: Expect::ConfirmedLake(&["isAlive", "(w, alive)"]),
        },
        StmtCell {
            id: "T-LEAN-ST-003",
            hypothesis: "LEAN-ST-H1",
            matrix_mirror: "—",
            program: for_phased_program(),
            expect: Expect::Faithful(&["elided: effect-free for-loop"]),
        },
        StmtCell {
            id: "T-LEAN-ST-010",
            hypothesis: "LEAN-ST-H0",
            matrix_mirror: "throw/top",
            program: program("", "fThrow() => [ throw 7 ]"),
            expect: Expect::Faithful(&["Cambrian.ThrowCode.ofNat 7"]),
        },
        StmtCell {
            id: "T-LEAN-ST-011",
            hypothesis: "LEAN-ST-H0",
            matrix_mirror: "emit/if",
            program: program(
                "    event Ev(x: u64);\n",
                "fEmitIf(n: u64) => [ if n > 0 => [ emit Ev(n); ] ]",
            ),
            expect: Expect::Faithful(&["Cambrian.WorldState.emit"]),
        },
        StmtCell {
            id: "T-LEAN-ST-012",
            hypothesis: "LEAN-ST-H0",
            matrix_mirror: "send/top",
            program: program(
                "",
                "fSend(dest: address, n: u64) => [ ~> dest with { value: n } ]",
            ),
            expect: Expect::Faithful(&["Cambrian.WorldState.transfer"]),
        },
        StmtCell {
            id: "T-LEAN-ST-013",
            hypothesis: "LEAN-ST-H0",
            matrix_mirror: "throw/if",
            program: program("", "fThrowIf(n: u64) => [ if n > 0 => [ throw 7 ] ]"),
            expect: Expect::Faithful(&["Cambrian.ThrowCode.ofNat 7"]),
        },
        StmtCell {
            id: "T-LEAN-ST-014",
            hypothesis: "LEAN-ST-H0",
            matrix_mirror: "throw/for",
            program: program(
                "",
                "fThrowFor(items: Vec<u64>) => [ for x in items => [ if x > 0 => [ throw 7 ] ] ]",
            ),
            expect: Expect::Faithful(&[".foldlM", "Cambrian.ThrowCode.ofNat 7"]),
        },
        StmtCell {
            id: "T-LEAN-ST-015",
            hypothesis: "LEAN-ST-H0",
            matrix_mirror: "throw_custom/if",
            program: program(
                "    error TooSmall(x: u64);\n",
                "fThrowCustom(n: u64) => [ if n > 0 => [ throw TooSmall(n) ] ]",
            ),
            expect: Expect::Faithful(&["-- TooSmall"]),
        },
        StmtCell {
            id: "T-LEAN-ST-016",
            hypothesis: "LEAN-ST-H0",
            matrix_mirror: "emit/for",
            program: program(
                "    event Ev(x: u64);\n",
                "fEmitFor(items: Vec<u64>) => [ for x in items => [ emit Ev(x); ] ]",
            ),
            expect: Expect::Faithful(&[".foldl", "Cambrian.WorldState.emit"]),
        },
        StmtCell {
            id: "T-LEAN-ST-017",
            hypothesis: "LEAN-ST-H0",
            matrix_mirror: "send/if",
            program: program(
                "",
                "fSendIf(dest: address, n: u64) => [ if n > 0 => [ ~> dest with { value: n } ] ]",
            ),
            expect: Expect::Faithful(&["Cambrian.WorldState.transfer"]),
        },
        StmtCell {
            id: "T-LEAN-ST-018",
            hypothesis: "LEAN-ST-H0",
            matrix_mirror: "send/for",
            program: program(
                "",
                "fSendFor(recipients: Vec<address>, n: u64) => [ for r in recipients => [ ~> r with { value: n } ] ]",
            ),
            expect: Expect::Faithful(&[".foldl", "Cambrian.WorldState.transfer"]),
        },
        StmtCell {
            id: "T-LEAN-ST-019",
            hypothesis: "LEAN-ST-H0",
            matrix_mirror: "return/if",
            program: program(
                "",
                "fRetIf(n: u64) -> u64 => [ if n > 0 => [ return(n) ] else [ return(0) ] ]",
            ),
            expect: Expect::Faithful(&["if (n > ((0 : BitVec 64))) then (n) else ((0 : BitVec 64))"]),
        },
        StmtCell {
            id: "T-STD-LEAN-STMT-001a",
            hypothesis: "STD-H-PLAC-1",
            matrix_mirror: "T-STD-STMT-001a",
            program: program(
                "",
                "clampIf() -> u64 => [ if m_count < 200 => [ let capped = std::math::clamp(m_count, 0, 100); return(capped) ] else [ return(0) ] ]",
            ),
            expect: Expect::Faithful(&["s.m_count < ((200 : BitVec 64))"]),
        },
        StmtCell {
            id: "T-STD-LEAN-STMT-001b",
            hypothesis: "STD-H-PLAC-1",
            matrix_mirror: "T-STD-STMT-001b",
            program: program(
                "",
                "clampFor(items: Vec<u64>) -> u64 => [ for i in items => [ let capped = std::math::clamp(i, 0, 50); if capped < 51 => [ ] ] return(0) ]",
            ),
            expect: Expect::Faithful(&["elided: effect-free for-loop"]),
        },
    ]
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

fn tempdir(stem: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cambrian-audit-lean-stmt-{}-{}",
        stem,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn transpile(cell: &StmtCell) -> (PathBuf, String) {
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
        "[{}] transpile failed:\n{}\nstderr:\n{}",
        cell.id,
        cell.program,
        String::from_utf8_lossy(&result.stderr),
    );

    let mut text = String::new();
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
                if p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with("Spec.lean"))
                {
                    continue;
                }
                if let Ok(t) = std::fs::read_to_string(&p) {
                    text.push_str(&t);
                    text.push('\n');
                }
            }
        }
    }
    (out, text)
}

fn check_faithful_sentinels(cell: &StmtCell, text: &str, needles: &[&str]) -> Vec<String> {
    let mut failures = Vec::new();
    for s in SILENT_SENTINELS {
        if text.contains(s) {
            failures.push(format!(
                "[{} / {}] leaked sentinel {:?}",
                cell.id, cell.hypothesis, s
            ));
        }
    }
    for needle in needles {
        if !text.contains(needle) {
            failures.push(format!(
                "[{} / {}] missing needle {:?}",
                cell.id, cell.hypothesis, needle
            ));
        }
    }
    failures
}

fn check_cell(cell: &StmtCell) -> Vec<String> {
    let (needles, check_validator) = match &cell.expect {
        Expect::Faithful(needles) => (*needles, true),
        Expect::ConfirmedLake(needles) => (*needles, false),
    };
    if check_validator {
        let prog = parse(&cell.program);
        let diags = check_lean_target_compat(&prog);
        let blocking: Vec<&Diagnostic> = diags
            .iter()
            .filter(|d| d.severity == cambrian_transpiler::validate::Severity::Error)
            .collect();
        if !blocking.is_empty() {
            return vec![format!(
                "[{} / {}] unexpected validator errors: {:?}",
                cell.id, cell.hypothesis, blocking
            )];
        }
    }
    let (_out, text) = transpile(cell);
    check_faithful_sentinels(cell, &text, needles)
}

#[test]
fn audit_lean_stmt_matrix_faithful() {
    let mut all_failures = Vec::new();
    for cell in cells() {
        let failures = check_cell(&cell);
        if failures.is_empty() {
            eprintln!("{} / {}: PASS — sentinel scan", cell.id, cell.hypothesis);
        } else {
            for f in &failures {
                eprintln!("{f}");
            }
            all_failures.extend(failures);
        }
    }
    assert!(
        all_failures.is_empty(),
        "Wave 5 Lean stmt faithful scan failures:\n{}",
        all_failures.join("\n")
    );
}

#[test]
fn audit_lean_stmt_matrix_smoke() {
    let mut failures = Vec::new();
    for cell in cells().into_iter().filter(|c| {
        matches!(c.id, "T-LEAN-ST-010" | "T-LEAN-ST-011" | "T-LEAN-ST-012")
    }) {
        failures.extend(check_cell(&cell));
    }
    assert!(
        failures.is_empty(),
        "Wave 5 Lean stmt smoke failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn audit_lean_stmt_std_smoke() {
    let ids = ["T-STD-LEAN-STMT-001a", "T-STD-LEAN-STMT-001b"];
    let mut failures = Vec::new();
    for cell in cells().iter().filter(|c| ids.contains(&c.id)) {
        failures.extend(check_cell(cell));
    }
    assert!(
        failures.is_empty(),
        "STD-H-PLAC-1 Lean stmt-axis std::math smoke failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn audit_lean_stmt_matrix_lake_build() {
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() != Ok("1") {
        eprintln!("skipping audit_lean_stmt_matrix_lake_build (set CAMBRIAN_TEST_LEAN_BUILD=1)");
        return;
    }
    let lake_ok = Command::new("lake")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(lake_ok, "CAMBRIAN_TEST_LEAN_BUILD=1 but lake not on PATH");

    let mut failures = Vec::new();
    for cell in &cells() {
        let (out, _text) = transpile(cell);
        let build = Command::new("lake")
            .arg("build")
            .current_dir(&out)
            .output()
            .expect("lake build");
        if !build.status.success() {
            let stdout = String::from_utf8_lossy(&build.stdout);
            let stderr = String::from_utf8_lossy(&build.stderr);
            failures.push(format!(
                "[{} / {}] lake build failed:\nstdout:\n{stdout}\nstderr:\n{stderr}",
                cell.id, cell.hypothesis,
            ));
        } else {
            eprintln!("{} / {}: lake PASS", cell.id, cell.hypothesis);
        }
    }
    assert!(
        failures.is_empty(),
        "Wave 5 Lean statement-axis lake-build failures:\n{}",
        failures.join("\n")
    );
}

/// LEAN-ST-H3: validator does not yet reject `let` escape from `if`
/// (failure is at `lake build` — same shape as EVM-ST-H1 / T-EVM-ST-001).
#[test]
fn audit_lean_stmt_if_escape_not_validator_blocked() {
    let prog = parse(&let_escapes_if_program());
    let diags = check_lean_target_compat(&prog);
    assert!(
        !diags.iter().any(|d| d.code == "V44"),
        "V44 should not exist yet"
    );
    let blocking: Vec<_> = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        blocking.is_empty(),
        "LEAN-ST-H3 expects validator pass (lake gate catches bug): {blocking:?}"
    );
}
