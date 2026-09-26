// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean emission gate for `std::` (STD-LEAN-1, docs/STDLIB.md §7).

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use cambrian_transpiler::ast;
use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;
use cambrian_transpiler::validate::{check_lean_target_compat, validate, Severity};

const STD_LEAN_CAM: &str = include_str!("fixtures/std_lean.cam");

fn parse(src: &str) -> ast::Program {
    let mut p = cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .expect("fixture must parse");
    ast::normalize_program_types(&mut p);
    p
}

fn blocking_codes(prog: &ast::Program) -> Vec<&'static str> {
    let mut codes: Vec<&'static str> = validate(prog)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect();
    codes.extend(
        check_lean_target_compat(prog)
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.code),
    );
    codes
}

fn lean_project_files(source: &str) -> HashMap<String, String> {
    let dir = std::env::temp_dir().join(format!("cambrian-std-lean-gen-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("std_lean.cam"), source).unwrap();
    std::fs::write(
        dir.join("project.yaml"),
        "target: lean\nsources:\n  - std_lean.cam\n",
    )
    .unwrap();
    let project = Project::load(&dir.join("project.yaml")).expect("project loads");
    let files = LeanBackend::default().gen_project(&project);
    let _ = std::fs::remove_dir_all(&dir);
    files.into_iter().collect()
}

fn write_project_to(out_dir: &Path, files: &HashMap<String, String>) {
    for (rel, content) in files {
        let path = out_dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }
}

fn lake_available() -> bool {
    Command::new("lake")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn std_lean_codegen_contains_prelude_calls() {
    let prog = parse(STD_LEAN_CAM);
    assert!(
        blocking_codes(&prog).is_empty(),
        "validator blocked std fixture: {:?}",
        blocking_codes(&prog)
    );

    let files = lean_project_files(STD_LEAN_CAM);
    let routes = files
        .get("Cambrian/Generated/StdLeanRoutes.lean")
        .expect("StdLeanRoutes.lean");
    let all_lean = files.values().map(|s| s.as_str()).collect::<Vec<_>>().join("\n");

    assert!(
        routes.contains("Cambrian.format") || routes.contains("Cambrian.formatNat"),
        "missing format lowering:\n{}",
        routes
    );
    assert!(
        all_lean.contains("Cambrian.pow") || all_lean.contains("Cambrian.powNat"),
        "missing pow lowering:\n{}",
        all_lean
    );
    assert!(routes.contains("Cambrian.parseRadixNat?"), "missing parseRadixNat? lowering:\n{}", routes);
    assert!(
        routes.contains("parseRadixSignedNat?"),
        "missing parseRadixSignedNat? lowering:\n{}",
        routes
    );
    assert!(
        routes.contains("8") && routes.contains("parseRadixNat?"),
        "expected u8 bit-width in parse lowering:\n{}",
        routes
    );
    // Radix is `Nat` on parseRadix*; BitVec ascription on the radix literal
    // (from ambient return-width expectation) must not leak into the call.
    assert!(
        !routes.contains("parseRadixNat? s_p (10 : BitVec")
            && !routes.contains("parseRadixSignedNat? s_p (10 : BitVec"),
        "radix must be Nat, not BitVec-ascribed:\n{}",
        routes
    );
    // Outer `match parse… { some/none }` must not ascribe the Option
    // scrutinee with the route return width (`: BitVec 64`).
    assert!(
        !routes.contains(") : BitVec 64) with | (Option.some")
            && !routes.contains(") : BitVec 8) with | (Option.some"),
        "Option match scrutinee must not inherit result BitVec width:\n{}",
        routes
    );
    assert!(routes.contains("Cambrian.clamp"), "missing clamp lowering:\n{}", routes);
    assert!(
        routes.contains("Cambrian.checkedMuldiv") || routes.contains("Cambrian.muldiv"),
        "missing muldiv lowering:\n{}",
        routes
    );
    assert!(all_lean.contains("Cambrian.modpow2"), "missing modpow2 lowering");
    assert!(
        !all_lean.contains("Cambrian.Unsupported"),
        "std fixture must not emit Unsupported"
    );
}

#[test]
fn std_lean_l13_rejects_crypto() {
    let src = r#"
entity E {
    routes {
        constructor() => []
        hash() -> u64 => [ return(std::crypto::sha256("x")) ]
    }
    m_x: u64 { in constructor() => 0 }
}
"#;
    let prog = parse(src);
    let codes = check_lean_target_compat(&prog);
    assert!(
        codes.iter().any(|d| d.code == "L13"),
        "expected L13 for std::crypto on Lean, got {:?}",
        codes
    );
}

#[test]
fn std_lean_lake_build_gate() {
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() != Ok("1") {
        eprintln!("skipping std_lean_lake_build_gate (set CAMBRIAN_TEST_LEAN_BUILD=1)");
        return;
    }
    if !lake_available() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }

    let files = lean_project_files(STD_LEAN_CAM);
    let out_dir = std::env::temp_dir().join(format!(
        "cambrian-std-lean-lake-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).unwrap();
    write_project_to(&out_dir, &files);

    let lake_out = Command::new("lake")
        .arg("build")
        .current_dir(&out_dir)
        .output()
        .expect("invoke lake");
    let _ = std::fs::remove_dir_all(&out_dir);
    assert!(
        lake_out.status.success(),
        "lake build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&lake_out.stdout),
        String::from_utf8_lossy(&lake_out.stderr),
    );
}
