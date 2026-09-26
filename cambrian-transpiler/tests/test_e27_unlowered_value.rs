// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! E27 — a substituted value must fail the build, not the review.
//!
//! Expression codegen falls back to the literal `0` for shapes it cannot lower,
//! behind validator E15/E16. But E15/E16 enumerate known-bad `Expr` shapes while
//! the fallback catches everything unhandled, so the gap between the two sets is
//! a silent miscompile. A `fold` over a tuple accumulator sat in that gap: a
//! whole `pure fn` became `return 0;`, the CLI exited 0, `forge test` was green
//! on paths that never reached the branch, and a security review caught it three
//! stages downstream in a generated Uniswap V2 pair.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

fn transpiler_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cambrian-transpiler"))
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(stem: &str) -> PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("cam-e27-{stem}-{n}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Transpiled {
    status: i32,
    stderr: String,
    solidity: Option<String>,
}

fn transpile_evm(stem: &str, source: &str) -> Transpiled {
    let dir = tempdir(stem);
    let src = dir.join("t.cam");
    std::fs::write(&src, source).unwrap();
    let out = dir.join("out");
    let result = Command::new(transpiler_bin())
        .arg(&src)
        .args(["--target", "evm", "-o"])
        .arg(&out)
        .output()
        .expect("spawn transpiler");
    let solidity = std::fs::read_dir(out.join("src"))
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(Result::ok)
                .find(|e| e.path().extension().is_some_and(|x| x == "sol"))
                .map(|e| std::fs::read_to_string(e.path()).unwrap())
        });
    Transpiled {
        status: result.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&result.stderr).to_string(),
        solidity,
    }
}

/// The shape that produced the finding: a `fold` whose accumulator is a tuple,
/// projected with `.0`. Reduced from the binary search in `sqrt_product`.
const TUPLE_FOLD: &str = r#"
pure fn wide(a: U256) -> U256 {
    (0..8).fold((0, a), |(lo, hi), _| {
        let mid = (lo + hi + 1) / 2;
        if mid < a { (mid, hi) } else { (lo, mid - 1) }
    }).0
}

entity F {
    routes {
        constructor() => []
        poke(a: U256) => []
    }
    m_v: U256 {
        in constructor() => 0
        in poke(a) => wide(a)
    }
}
"#;

#[test]
fn an_unlowered_value_fails_the_transpile() {
    let run = transpile_evm("tuple-fold", TUPLE_FOLD);
    assert_ne!(
        run.status, 0,
        "a substituted value must not exit 0; stderr:\n{}",
        run.stderr
    );
    assert!(
        run.stderr.contains("E27"),
        "the failure must carry the E27 code so a gate can key on it; stderr:\n{}",
        run.stderr
    );
}

#[test]
fn the_failure_names_the_generated_line() {
    let run = transpile_evm("tuple-fold-report", TUPLE_FOLD);
    assert!(
        run.stderr.contains(".sol:"),
        "E27 must point at file:line in the generated output, otherwise there is\n\
         nothing to look at; stderr:\n{}",
        run.stderr
    );
    // The artifact stays on disk: it is the evidence a fix needs, and the exit
    // code is what keeps it from being treated as a build.
    let sol = run.solidity.expect("generated Solidity is written before the check");
    assert!(
        sol.contains("[unlowered-value]"),
        "the marker must survive into the artifact the report cites"
    );
}

#[test]
fn a_scalar_accumulator_fold_still_lowers() {
    // Pins the blast radius: only the tuple-accumulator shape is unsupported.
    // Failing this means the E27 boundary started rejecting working programs.
    let run = transpile_evm(
        "scalar-fold",
        r#"
pure fn total(a: U256) -> U256 { (0..8).fold(0, |acc, _| acc + a) }

entity S {
    routes {
        constructor() => []
        poke(a: U256) => []
    }
    m_v: U256 {
        in constructor() => 0
        in poke(a) => total(a)
    }
}
"#,
    );
    assert_eq!(run.status, 0, "scalar fold must still transpile; stderr:\n{}", run.stderr);
    let sol = run.solidity.expect("scalar fold emits Solidity");
    assert!(!sol.contains("[unlowered-value]"));
}
