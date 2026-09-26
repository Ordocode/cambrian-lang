// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean declaration order and tuple projections.
//!
//! Both regressions come from the same run: a Uniswap V2 pair that imported
//! `stdlib/math.cam`, transpiled clean to EVM, passed 78 Foundry tests, and
//! could not be built by Lean at all. Twelve of the thirteen errors were
//! calls to functions defined further down the file; the last was a `.0` on
//! a tuple. The EVM target is indifferent to both, so nothing upstream of
//! `lake build` noticed.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(stem: &str) -> PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "cambrian-lean-decl-order-{}-{}-{}",
        stem,
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn transpile_lean(stem: &str, source: &str) -> String {
    let dir = tempdir(stem);
    let cam = dir.join("input.cam");
    std::fs::write(&cam, source).expect("write cam");
    let out = dir.join("out");
    let result = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_cambrian-transpiler")))
        .arg(&cam)
        .arg("-o")
        .arg(&out)
        .arg("--target")
        .arg("lean")
        .output()
        .expect("invoke transpiler");
    assert!(
        result.status.success(),
        "transpile failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );

    let mut text = String::new();
    let mut stack = vec![out];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("lean")
                && p.file_name().and_then(|f| f.to_str()) == Some("Pure.lean")
            {
                text = std::fs::read_to_string(&p).expect("read Pure.lean");
            }
        }
    }
    assert!(!text.is_empty(), "no Pure.lean emitted");
    text
}

fn line_of(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not found in:\n{text}"))
}

/// A caller declared before its callee must still be emitted after it.
#[test]
fn pure_fns_are_emitted_callee_first() {
    let source = "\
pure fn caller(a: u64) -> u64 { callee(a) + 1 }
pure fn callee(a: u64) -> u64 { a * 2 }

entity Probe {
    routes { constructor() => [] }
    m_count: u64 { in constructor() => 0 }
}
";
    let text = transpile_lean("callee-first", source);

    let callee_def = line_of(&text, "def callee");
    let caller_def = line_of(&text, "def caller");
    assert!(
        callee_def < caller_def,
        "callee must precede its caller — Lean has no forward references:\n{text}"
    );
}

/// A chain of forward references orders transitively, not just pairwise.
#[test]
fn pure_fn_order_is_transitive() {
    let source = "\
pure fn top(a: u64) -> u64 { middle(a) }
pure fn middle(a: u64) -> u64 { bottom(a) + 1 }
pure fn bottom(a: u64) -> u64 { a * 2 }

entity Probe {
    routes { constructor() => [] }
    m_count: u64 { in constructor() => 0 }
}
";
    let text = transpile_lean("transitive", source);

    let bottom = line_of(&text, "def bottom");
    let middle = line_of(&text, "def middle");
    let top = line_of(&text, "def top");
    assert!(
        bottom < middle && middle < top,
        "expected bottom < middle < top:\n{text}"
    );
}

/// Cambrian indexes tuples from zero, Lean from one.
#[test]
fn tuple_projection_is_rebased_for_lean() {
    let source = "\
pure fn pair_head(a: u64, b: u64) -> u64 { split(a, b).0 }
pure fn pair_tail(a: u64, b: u64) -> u64 { split(a, b).1 }
pure fn split(a: u64, b: u64) -> (u64, u64) { (a, b) }

entity Probe {
    routes { constructor() => [] }
    m_count: u64 { in constructor() => 0 }
}
";
    let text = transpile_lean("tuple-projection", source);

    let head = text
        .lines()
        .find(|line| line.contains("Cambrian.Generated.Pure.split") && line.contains(").1"))
        .unwrap_or_else(|| panic!("no `.1` projection emitted:\n{text}"));
    assert!(!head.contains(").0"), "`.0` is not valid Lean: {head}");
    assert!(
        text.contains(").2"),
        "second component must lower to `.2`:\n{text}"
    );
}

fn transpile_spec(stem: &str, source: &str) -> String {
    let dir = tempdir(stem);
    let cam = dir.join("input.cam");
    std::fs::write(&cam, source).expect("write cam");
    let out = dir.join("out");
    let result = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_cambrian-transpiler")))
        .arg(&cam)
        .arg("-o")
        .arg(&out)
        .arg("--target")
        .arg("lean")
        .output()
        .expect("invoke transpiler");
    assert!(
        result.status.success(),
        "transpile failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );

    let mut text = String::new();
    let mut stack = vec![out];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().and_then(|f| f.to_str()).is_some_and(|f| f.ends_with("Spec.lean")) {
                text = std::fs::read_to_string(&p).expect("read Spec.lean");
            }
        }
    }
    assert!(!text.is_empty(), "no *Spec.lean emitted");
    text
}

/// Deploy bindings lower to `Entity.address inst` in `expect return` (LG-013).
#[test]
fn test_expect_return_deploy_binding_lowers_to_address() {
    let source = "\
entity Probe {
    routes {
        constructor() => []
        view token0() -> address => [ return(m_token0) ]
    }
    m_token0: address { in constructor() => 0x0000000000000000000000000000000000000000 as address }
}

test \"wires token\" for Probe {
    deploy t0 = Probe() with { m_token0: 0x0000000000000000000000000000000000000001 as address }
    call token0()
    expect return t0
}
";
    let text = transpile_spec("test-deploy-binding", source);
    assert!(
        text.contains("__e0 := _result_0 = (Probe.address t0_inst)"),
        "expect return deploy binding must lower to CREATE2 address:\n{text}"
    );
    assert!(
        !text.contains("`expect return` dropped"),
        "declared deploy binding must not be dropped:\n{text}"
    );
}

/// Cross-contract `deploy` + `call peer.route` + `expect return` is lowered
/// when `InstallPeer` is supported (peer gets its own `_result` slot).
#[test]
fn return_expectation_is_dropped_with_its_cross_contract_call() {
    let source = "\
entity Probe {
    routes {
        constructor() => []
        view own_value() -> u64 => [ return(m_count) ]
        view peer_value() -> u64 => [ return(m_count) ]
    }
    m_count: u64 { in constructor() => 1 }
}

property \"P1 StaleResult\" for Probe {
    call own_value()
    expect return 1

    deploy peer = Probe() with { m_count: 7 }
    call peer.peer_value()
    expect return 7
}
";
    let text = transpile_spec("stale-result", source);

    assert!(
        text.contains("_result_0 = 1"),
        "self-call expectation must survive:\n{text}"
    );
    assert!(
        text.contains("_result_1 = 7"),
        "peer-call expectation must bind its own result slot:\n{text}"
    );
}
