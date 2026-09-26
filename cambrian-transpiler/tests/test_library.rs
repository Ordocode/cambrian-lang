// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase Library-3: `library` declaration end-to-end tests.
//!
//! Coverage:
//!   - Parser accepts `library Name { pure fn ... }`.
//!   - EVM codegen emits `library Name { function ... internal pure ... }`.
//!   - EVM call site lowers to `LibName.fn(args)`.
//!   - A/N codegen emits library pure_fns as free Rust fns.
//!   - V47 (duplicate library fn) and V48 (impurity) fire.
//!   - solc round-trip for a small SafeMath-style library.

use std::process::Command;

use cambrian_transpiler::codegen::gen_evm_solidity;
use cambrian_transpiler::using_rewrite::apply_using_rewrites;
use cambrian_transpiler::validate;
use cambrian_transpiler::ProgramParser;

fn parse_and_rewrite(src: &str) -> cambrian_transpiler::ast::Program {
    let mut program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse error: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    apply_using_rewrites(&mut program);
    program
}

/// The backend emits `pragma solidity ^0.8.24`, so a `solc` older than that
/// rejects every file it is handed regardless of what the generated code
/// says. Presence alone is therefore the wrong question: a distro `solc` at
/// 0.8.19 turns this round-trip into a permanent red that reports a pragma
/// mismatch rather than anything about libraries. Ask for the version and
/// skip below the floor, the way the Lean tests skip without `lake`.
const SOLC_FLOOR: (u32, u32, u32) = (0, 8, 24);

fn solc_version() -> Option<(u32, u32, u32)> {
    let out = Command::new("solc").arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.trim_start().starts_with("Version:"))?;
    let raw = line.split(':').nth(1)?.trim();
    let numeric = raw.split(['+', '-']).next()?;
    let mut parts = numeric.split('.').map(|p| p.trim().parse::<u32>());
    match (parts.next(), parts.next(), parts.next()) {
        (Some(Ok(a)), Some(Ok(b)), Some(Ok(c))) => Some((a, b, c)),
        _ => None,
    }
}

fn has_solc() -> bool {
    match solc_version() {
        Some(v) if v >= SOLC_FLOOR => true,
        Some(v) => {
            eprintln!(
                "solc {}.{}.{} is below the {}.{}.{} the backend's pragma requires — skipping",
                v.0, v.1, v.2, SOLC_FLOOR.0, SOLC_FLOOR.1, SOLC_FLOOR.2
            );
            false
        }
        None => false,
    }
}

// -------------------------------------------------------------------------
// Parser / AST
// -------------------------------------------------------------------------

#[test]
fn parser_accepts_library_decl() {
    let src = r#"
        library SafeMath {
            pure fn add(a: u64, b: u64) -> u64 { a + b }
            pure fn sub(a: u64, b: u64) -> u64 { a - b }
        }

        entity Foo {
            routes { noop() => [] }
            m_v: u64 { in noop() => 0 }
        }
    "#;
    let program = parse_and_rewrite(src);
    assert_eq!(program.libraries.len(), 1);
    let lib = &program.libraries[0];
    assert_eq!(lib.name, "SafeMath");
    assert_eq!(lib.pure_fns.len(), 2);
    let fn_names: Vec<_> = lib.pure_fns.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(fn_names, vec!["add", "sub"]);
}

#[test]
fn parser_library_with_const_and_type_alias() {
    let src = r#"
        library Helpers {
            type Amount = u64
            const MAX: u64 = 1000
            pure fn cap(a: u64) -> u64 { a }
        }

        entity Foo {
            routes { noop() => [] }
            m_v: u64 { in noop() => 0 }
        }
    "#;
    let program = parse_and_rewrite(src);
    let lib = &program.libraries[0];
    assert_eq!(lib.type_aliases.len(), 1);
    assert_eq!(lib.type_aliases[0].name, "Amount");
    assert_eq!(lib.constants.len(), 1);
    assert_eq!(lib.constants[0].name, "MAX");
    assert_eq!(lib.pure_fns.len(), 1);
}

// -------------------------------------------------------------------------
// EVM codegen
// -------------------------------------------------------------------------

#[test]
fn evm_library_emits_internal_pure_function() {
    let src = r#"
        library Math {
            pure fn add_one(a: u64) -> u64 { a + 1 }
        }

        using Math for u64;

        entity Foo {
            routes { set(v: u64) => [] }
            m_v: u64 { in set(v) => v.add_one() }
        }
    "#;
    let program = parse_and_rewrite(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(sol.contains("library Math {"), "missing library block: {sol}");
    assert!(
        sol.contains("function add_one(uint64 a) internal pure returns (uint64)"),
        "missing internal pure fn: {sol}"
    );
    // Call site must dispatch via library: `Math.add_one(...)`, not the
    // bare `add_one(...)` shape used by program-scope pure fns.
    assert!(
        sol.contains("Math.add_one("),
        "missing library-qualified call: {sol}"
    );
    // And of course no method-call syntax — the using_rewrite pass
    // should have eliminated `v.add_one()`.
    assert!(
        !sol.contains(".add_one()"),
        "stray method-call form: {sol}"
    );
}

#[test]
fn evm_library_solc_roundtrip_safe_math() {
    if !has_solc() {
        eprintln!("solc not installed — skipping round-trip test");
        return;
    }

    // SafeMath-shaped fixture: a small library used by an entity, with
    // an actual library call from within a route.
    let src = r#"
        library SafeMath {
            pure fn add(a: u64, b: u64) -> u64 { a + b }
            pure fn sub(a: u64, b: u64) -> u64 { a - b }
        }

        using SafeMath for u64;

        entity Counter {
            routes {
                inc(by: u64) => []
                dec(by: u64) => []
            }
            m_total: u64 {
                in inc(by) => m_total.add(by)
                in dec(by) => m_total.sub(by)
            }
        }
    "#;
    let program = parse_and_rewrite(src);
    let sol = gen_evm_solidity(&program, true);

    let out_dir = std::env::temp_dir().join(format!(
        "cambrian-library-solc-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).unwrap();
    let sol_path = out_dir.join("Counter.sol");
    std::fs::write(&sol_path, &sol).unwrap();

    let output = Command::new("solc")
        .arg("--bin")
        .arg("--abi")
        .arg(&sol_path)
        .output()
        .expect("solc invocation failed");
    let _ = std::fs::remove_dir_all(&out_dir);

    assert!(
        output.status.success(),
        "solc rejected library output:\nsolc stderr:\n{}\nsolc stdout:\n{}\nsource:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
        sol
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("SafeMath"),
        "solc did not emit the SafeMath library: {}",
        stdout
    );
}

// -------------------------------------------------------------------------
// Validator
// -------------------------------------------------------------------------

#[test]
fn v47_duplicate_library_pure_fn() {
    let src = r#"
        library Math {
            pure fn add(a: u64, b: u64) -> u64 { a + b }
            pure fn add(a: u64, b: u64) -> u64 { a + b }
        }

        entity Foo {
            routes { noop() => [] }
            m_v: u64 { in noop() => 0 }
        }
    "#;
    let program = parse_and_rewrite(src);
    let diags = validate::validate(&program);
    let v47 = diags.iter().find(|d| d.code == "V47");
    assert!(v47.is_some(), "expected V47, got: {:?}", diags);
}

#[test]
fn v47_duplicate_library_decl() {
    let src = r#"
        library Math {
            pure fn add(a: u64, b: u64) -> u64 { a + b }
        }
        library Math {
            pure fn sub(a: u64, b: u64) -> u64 { a - b }
        }

        entity Foo {
            routes { noop() => [] }
            m_v: u64 { in noop() => 0 }
        }
    "#;
    let program = parse_and_rewrite(src);
    let diags = validate::validate(&program);
    let v47 = diags.iter().find(|d| d.code == "V47");
    assert!(v47.is_some(), "expected V47 for duplicate library, got: {:?}", diags);
}

#[test]
fn library_fn_referencing_state_emits_v4() {
    // Library pure_fn referencing `sys::now` is impure. V48 plan
    // language → reuses existing V4 ("Pure function ... references
    // sys::... (impure)") infrastructure.
    let src = r#"
        library Math {
            pure fn now_plus(a: u64) -> u64 { a + sys::now }
        }

        entity Foo {
            routes { noop() => [] }
            m_v: u64 { in noop() => 0 }
        }
    "#;
    let program = parse_and_rewrite(src);
    let diags = validate::validate(&program);
    let v4 = diags.iter().find(|d| d.code == "V4");
    assert!(v4.is_some(), "expected V4 (impurity) for library fn, got: {:?}", diags);
}
