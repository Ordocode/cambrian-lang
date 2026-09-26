// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean-target completeness *prover* — the (action × placement × mode)
//! matrix harness.
//!
//! Where `test_lean_completeness.rs` is a regression net over real
//! contracts (it only catches gaps a fixture happens to exercise), this
//! file is the *generative* counterpart: it synthesizes a minimal program
//! for every interesting `(RouteAction, placement)` cell and asserts the
//! lowering is faithful. Every gap we hit reactively (conditional-return
//! payload, `throw` in a conditional, `throw`/effect in a `for`-loop) was
//! one cell of this grid that was silently mishandled; enumerating the
//! grid makes "is anything missing?" a decidable, enforced check.
//!
//! Each [`Cell`] declares:
//!   * a complete `.cam` program exercising the action in a placement;
//!   * an [`Expect`]ation: `Faithful` (must contain semantic needles and
//!     NO silent sentinel) or `Unsupported` (must contain the documented
//!     "not supported on Lean" marker — intentionally-unsupported,
//!     validator-guarded effects).
//!
//! The cheap part (transpile + scan + needle check) always runs. The
//! `lake build` of every `Faithful` cell is gated behind
//! `CAMBRIAN_TEST_LEAN_BUILD=1`, like the other Lean smoke suites.

use std::path::PathBuf;
use std::process::Command;

// ---------------------------------------------------------------------------
// Sentinels that must never appear in a `Faithful` cell's route/expr output.
// (`*Spec.lean` is excluded — proof/test codegen has its own intentional
// markers. Mirrors `test_lean_completeness.rs`.)
// ---------------------------------------------------------------------------

const SILENT_SENTINELS: &[&str] = &[
    "-- skipped:",
    "-- internal: effectful",
    "-- bug:",
    "-- L8:",
    "Cambrian.Unsupported",
    ", default)",
    "sorry",
];

enum Expect {
    /// Faithful lowering: output must contain every needle and no sentinel.
    Faithful(&'static [&'static str]),
    /// Intentionally unsupported on Lean: output must contain this marker
    /// substring (and the action is therefore NOT expected to build).
    Unsupported(&'static str),
}

struct Cell {
    name: &'static str,
    program: String,
    expect: Expect,
}

// ---------------------------------------------------------------------------
// Program templates
// ---------------------------------------------------------------------------

/// Wrap a focus route plus optional top-level declarations in a complete
/// single-entity program. `decls` go inside the entity (events/errors),
/// `route` is spliced into the `routes { … }` block.
fn program(decls: &str, route: &str) -> String {
    format!(
        "entity M {{\n\
         {decls}\n\
             routes {{\n\
                 init create() => []\n\
                 {route}\n\
             }}\n\
             m_count: u64 {{ in create() => 0 }}\n\
         }}\n"
    )
}

fn cells() -> Vec<Cell> {
    let mut v = Vec::new();

    // ---- Throw × {top, if, for} (failing routes) --------------------------
    v.push(Cell {
        name: "throw/top",
        program: program("", "fThrow() => [ throw 7 ]"),
        expect: Expect::Faithful(&["Cambrian.ThrowCode.ofNat 7"]),
    });
    v.push(Cell {
        name: "throw/if",
        program: program("", "fThrowIf(n: u64) => [ if n > 0 => [ throw 7 ] ]"),
        expect: Expect::Faithful(&["Cambrian.ThrowCode.ofNat 7"]),
    });
    v.push(Cell {
        name: "throw/for",
        program: program(
            "",
            "fThrowFor(items: Vec<u64>) => [ for x in items => [ if x > 0 => [ throw 7 ] ] ]",
        ),
        expect: Expect::Faithful(&[".foldlM", "Cambrian.ThrowCode.ofNat 7"]),
    });

    // ---- ThrowCustom × if -------------------------------------------------
    v.push(Cell {
        name: "throw_custom/if",
        program: program(
            "    error TooSmall(x: u64);",
            "fThrowCustom(n: u64) => [ if n > 0 => [ throw TooSmall(n) ] ]",
        ),
        // Stable, non-zero name-derived code + the `-- TooSmall` annotation,
        // never the `ofNat 0` placeholder.
        expect: Expect::Faithful(&["-- TooSmall"]),
    });

    // ---- Emit × {top, if, for} (world-threaded) --------------------------
    let ev = "    event Ev(x: u64);";
    v.push(Cell {
        name: "emit/top",
        program: program(ev, "fEmit(n: u64) => [ emit Ev(n); ]"),
        expect: Expect::Faithful(&["Cambrian.WorldState.emit"]),
    });
    v.push(Cell {
        name: "emit/if",
        program: program(ev, "fEmitIf(n: u64) => [ if n > 0 => [ emit Ev(n); ] ]"),
        expect: Expect::Faithful(&["Cambrian.WorldState.emit"]),
    });
    v.push(Cell {
        name: "emit/for",
        program: program(
            ev,
            "fEmitFor(items: Vec<u64>) => [ for x in items => [ emit Ev(x); ] ]",
        ),
        expect: Expect::Faithful(&[".foldl", "Cambrian.WorldState.emit"]),
    });

    // ---- Raw transfer × {top, if, for} (world-threaded) ------------------
    v.push(Cell {
        name: "send/top",
        program: program("", "fSend(dest: address, n: u64) => [ ~> dest with { value: n } ]"),
        expect: Expect::Faithful(&["Cambrian.WorldState.transfer"]),
    });
    v.push(Cell {
        name: "send/if",
        program: program(
            "",
            "fSendIf(dest: address, n: u64) => [ if n > 0 => [ ~> dest with { value: n } ] ]",
        ),
        expect: Expect::Faithful(&["Cambrian.WorldState.transfer"]),
    });
    v.push(Cell {
        name: "send/for",
        program: program(
            "",
            "fSendFor(recipients: Vec<address>, n: u64) => [ for r in recipients => [ ~> r with { value: n } ] ]",
        ),
        expect: Expect::Faithful(&[".foldl", "Cambrian.WorldState.transfer"]),
    });

    // ---- Return × {top, if} (view) ---------------------------------------
    v.push(Cell {
        name: "return/top",
        program: program("", "fRet(n: u64) -> u64 => [ return(n) ]"),
        expect: Expect::Faithful(&["(s, n)"]),
    });
    v.push(Cell {
        name: "return/if",
        program: program(
            "",
            "fRetIf(n: u64) -> u64 => [ if n > 0 => [ return(n) ] else [ return(0) ] ]",
        ),
        // The conditional-return must be lifted into a value-producing term.
        expect: Expect::Faithful(&["if (n > ((0 : BitVec 64))) then (n) else ((0 : BitVec 64))"]),
    });

    // ---- Intentionally unsupported: gosh effect --------------------------
    v.push(Cell {
        name: "gosh_effect/top",
        program: format!(
            "use gosh\n{}",
            program("", "fGosh() => [ gosh::rawReserve(0, 0) ]")
        ),
        expect: Expect::Unsupported("not supported on Lean"),
    });

    v
}

// ---------------------------------------------------------------------------
// Scaffolding
// ---------------------------------------------------------------------------

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
        "cambrian-lean-matrix-{}-{}",
        stem,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Transpile a program string to Lean; return (output dir, concatenated
/// non-Spec generated Lean).
fn transpile(name: &str, program: &str) -> (PathBuf, String) {
    let dir = tempdir(&name.replace('/', "_"));
    let cam = dir.join("input.cam");
    std::fs::write(&cam, program).expect("write cam");
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
        "[{name}] transpile failed:\nprogram:\n{program}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );

    // Concatenate non-Spec generated Lean (route/expr codegen).
    let mut text = String::new();
    let mut stack = vec![out.clone()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("lean")
                && !p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with("Spec.lean"))
            {
                if let Ok(t) = std::fs::read_to_string(&p) {
                    text.push_str(&t);
                    text.push('\n');
                }
            }
        }
    }
    (out, text)
}

// ---------------------------------------------------------------------------
// The grid test
// ---------------------------------------------------------------------------

#[test]
fn action_context_matrix_is_faithful() {
    let mut failures = Vec::new();

    for cell in cells() {
        let (_out, text) = transpile(cell.name, &cell.program);
        match cell.expect {
            Expect::Faithful(needles) => {
                for s in SILENT_SENTINELS {
                    if text.contains(s) {
                        failures.push(format!(
                            "[{}] FAITHFUL cell leaked sentinel {:?}",
                            cell.name, s
                        ));
                    }
                }
                for needle in needles {
                    if !text.contains(needle) {
                        failures.push(format!(
                            "[{}] missing expected lowering {:?}",
                            cell.name, needle
                        ));
                    }
                }
            }
            Expect::Unsupported(marker) => {
                if !text.contains(marker) {
                    failures.push(format!(
                        "[{}] expected unsupported marker {:?} not found",
                        cell.name, marker
                    ));
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "Lean action×context matrix gaps:\n{}",
        failures.join("\n")
    );
}

/// Gated: every `Faithful` cell must `lake build` clean (catches ill-typed
/// lowerings — e.g. a `throw` inside a pure `Id.run do`). Mirrors the
/// `CAMBRIAN_TEST_LEAN_BUILD=1` gate of the other Lean smoke suites.
#[test]
fn action_context_matrix_lake_builds() {
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() != Ok("1") {
        eprintln!("skipping matrix lake-build (set CAMBRIAN_TEST_LEAN_BUILD=1 to enable)");
        return;
    }
    let lake_ok = Command::new("lake")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(lake_ok, "CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");

    let mut failures = Vec::new();
    for cell in cells() {
        if !matches!(cell.expect, Expect::Faithful(_)) {
            continue;
        }
        let (out, _text) = transpile(cell.name, &cell.program);
        let build = Command::new("lake")
            .arg("build")
            .current_dir(&out)
            .output()
            .expect("invoke lake");
        if !build.status.success() {
            failures.push(format!(
                "[{}] lake build failed:\n{}",
                cell.name,
                String::from_utf8_lossy(&build.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "matrix lake-build failures:\n{}", failures.join("\n"));
}
