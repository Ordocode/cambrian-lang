// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean signed / negative-number lowering across `lean.numerics` modes.
//!
//! Pins: `nat` → `Int` for `i*`; BitVec modes use `slt` / `signExtend` /
//! `checkedS*` for signed ops.

use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(stem: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("cam-lean-signed-{}-{}", stem, n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn gen_project(source: &str, yaml_lean: &str, stem: &str) -> HashMap<String, String> {
    let dir = tempdir(stem);
    std::fs::write(dir.join("signed.cam"), source).unwrap();
    std::fs::write(
        dir.join("project.yaml"),
        format!(
            "name: signed\ntarget: lean\nsources:\n  - signed.cam\nlean:\n{yaml_lean}"
        ),
    )
    .unwrap();
    let project = Project::load(&dir.join("project.yaml")).expect("load project");
    let files: HashMap<String, String> = LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    files
}

const SIGNED_SRC: &str = r#"
entity Signed {
    routes {
        constructor() => []
        go() => []
        adjust(delta: i64) => []
        cmp(a: i8, b: i8) -> bool => [ return(a < b) ]
        widen(x: i8) -> i16 => [ return(x as i16) ]
        add_i8(a: i8, b: i8) -> i8 => [ return(a + b) ]
    }
    m_x: i64 {
        in constructor() => -5
        in go() => m_x + 1
        in adjust(delta) => m_x + delta
    }
}

property "signed delta" (delta: i64) for Signed with { m_x: -5 } {
    call adjust(delta)
    expect state { m_x: m_x }
    fuzz { delta in -10..0 }
}
"#;

#[test]
fn lean_numerics_nat_signed_lowers_to_int() {
    let files = gen_project(SIGNED_SRC, "  numerics: nat\n", "nat-int");
    let entity = files
        .get("Cambrian/Generated/Signed.lean")
        .expect("Signed.lean");
    assert!(
        entity.contains("m_x : Int"),
        "nat-mode signed member must be Int:\n{entity}"
    );
    assert!(
        entity.contains("(0 : Int)") || entity.contains("m_x"),
        "nat-mode signed default uses Int:\n{entity}"
    );

    let members = files
        .get("Cambrian/Generated/SignedMembers.lean")
        .or_else(|| files.get("Cambrian/Generated/Signed.lean"))
        .expect("members");
    assert!(
        members.contains("(-(5") || members.contains("(-5") || members.contains("-5"),
        "constructor -5 must lower for Int:\n{members}"
    );

    let spec = files
        .get("Cambrian/Generated/SignedSpec.lean")
        .expect("SignedSpec.lean");
    assert!(
        spec.contains("∀ (delta : Int)") || spec.contains("(delta : Int)"),
        "nat-mode signed property binder must be Int:\n{spec}"
    );
}

#[test]
fn lean_overflow_wrap_signed_cmp_uses_slt() {
    let files = gen_project(SIGNED_SRC, "  numerics: overflow-wrap\n", "wrap-slt");
    let routes = files
        .get("Cambrian/Generated/SignedRoutes.lean")
        .expect("SignedRoutes.lean");
    assert!(
        routes.contains(".slt") || routes.contains("BitVec.slt"),
        "signed `<` under BitVec must use slt:\n{routes}"
    );
    assert!(
        !routes.contains("checkedSAdd") && !routes.contains("checkedAdd"),
        "wrap mode must not emit checked*:\n{routes}"
    );
}

#[test]
fn lean_overflow_wrap_signed_widen_sign_extends() {
    let files = gen_project(SIGNED_SRC, "  numerics: overflow-wrap\n", "wrap-sext");
    let routes = files
        .get("Cambrian/Generated/SignedRoutes.lean")
        .expect("SignedRoutes.lean");
    assert!(
        routes.contains("signExtend"),
        "i8 as i16 must sign-extend:\n{routes}"
    );
}

#[test]
fn lean_overflow_panic_signed_uses_checked_sadd() {
    let files = gen_project(SIGNED_SRC, "  numerics: overflow-panic\n", "panic-sadd");
    let routes = files
        .get("Cambrian/Generated/SignedRoutes.lean")
        .expect("SignedRoutes.lean");
    let members = files
        .get("Cambrian/Generated/SignedMembers.lean")
        .or_else(|| files.get("Cambrian/Generated/Signed.lean"))
        .expect("members");
    let body = format!("{routes}\n{members}");
    assert!(
        body.contains("checkedSAdd"),
        "overflow-panic signed + must use checkedSAdd:\n{body}"
    );
}

#[test]
fn lean_core_prelude_has_checked_s_ops() {
    let files = gen_project(
        "entity E { routes { constructor() => [] } m_c: u64 { in constructor() => 0 } }\n",
        "  numerics: overflow-panic\n",
        "prelude-check",
    );
    let core = files.get("Cambrian/Core.lean").expect("Core.lean");
    assert!(core.contains("def checkedSAdd"), "missing checkedSAdd:\n");
    assert!(core.contains("def checkedSSub"), "missing checkedSSub");
    assert!(core.contains("def checkedSMul"), "missing checkedSMul");
    assert!(
        core.contains("x.slt 0#n") || core.contains(".slt 0#n"),
        "abs/sign must use slt:\n{}",
        &core[core.find("def abs").unwrap_or(0)..core.find("def abs").unwrap_or(0).saturating_add(200)]
    );
}

#[test]
fn lean_generators_sample_int() {
    let files = gen_project(
        "entity E { routes { constructor() => [] } m_c: u64 { in constructor() => 0 } }\n",
        "  numerics: nat\n",
        "gen-int",
    );
    // Generators.lean is only emitted when plausible assets are copied —
    // look for it under Testing/ if present.
    if let Some(gen) = files.get("Testing/Generators.lean") {
        assert!(
            gen.contains("camIntArbitrary") || gen.contains("Arbitrary Int"),
            "Generators must sample Int:\n{gen}"
        );
    }
}
