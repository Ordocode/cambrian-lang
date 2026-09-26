// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase N Wave-3 — type / nested-expr fuzz skeleton (PW3-S-005).
//!
//! Adds a **type-declaration axis** (alias chains depth 1–4, record, enum, mixed
//! program types) per PW3-O-015 / PW3-G-001 and a validate ⇒ forge ⇒ lake
//! tri-oracle (PN-007 tail).
//!
//!   cargo test -p cambrian-transpiler --test test_audit_type_fuzz -- --nocapture

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use cambrian_transpiler::codegen::{EvmSolidityBackend, LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;
use cambrian_transpiler::validate::{
    self, check_evm_target_compat_with, check_lean_target_compat, validate_project_config,
    Diagnostic, Severity,
};
use cambrian_transpiler::ProgramParser;
use proptest::prelude::*;

const FOUNDRY_TOML: &str = r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
evm_version = "prague"
optimizer = false
optimizer_runs = 200
via_ir = false
"#;

const CAM_NAME: &str = "input.cam";
const SMOKE_DEPTH_CASES: u8 = 4;
const FUZZ_CASES: u32 = 24;

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);
static STAT_RUN: AtomicU64 = AtomicU64::new(0);
static STAT_SKIP: AtomicU64 = AtomicU64::new(0);
static STAT_PASS: AtomicU64 = AtomicU64::new(0);
static STAT_NEW_FAIL: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypeAxisKind {
    TypeAliasDepth,
    RecordField,
    EnumTagged,
    MixedDecl,
}

impl TypeAxisKind {
    const ALL: [TypeAxisKind; 4] = [
        TypeAxisKind::TypeAliasDepth,
        TypeAxisKind::RecordField,
        TypeAxisKind::EnumTagged,
        TypeAxisKind::MixedDecl,
    ];

    fn slug(self) -> &'static str {
        match self {
            TypeAxisKind::TypeAliasDepth => "alias",
            TypeAxisKind::RecordField => "rec",
            TypeAxisKind::EnumTagged => "enum",
            TypeAxisKind::MixedDecl => "mixed",
        }
    }
}

#[derive(Debug, Clone)]
struct TypeFuzzCase {
    kind: TypeAxisKind,
    depth: u8,
    lit: u64,
}

impl TypeFuzzCase {
    fn slug(&self) -> String {
        format!("{}_{}d_{}", self.kind.slug(), self.depth, self.lit)
    }

    fn render(&self) -> String {
        match self.kind {
            TypeAxisKind::TypeAliasDepth => render_type_alias_depth_program(self.depth, self.lit),
            TypeAxisKind::RecordField => render_record_field_program(self.lit),
            TypeAxisKind::EnumTagged => render_enum_program(self.lit),
            TypeAxisKind::MixedDecl => render_mixed_decl_program(self.lit),
        }
    }
}

fn render_type_alias_depth_program(depth: u8, lit: u64) -> String {
    let depth = depth.clamp(1, 4);
    let mut decls = String::new();
    decls.push_str("type Alias1 = u64\n\n");
    for d in 2..=depth {
        decls.push_str(&format!("type Alias{d} = Alias{}\n\n", d - 1));
    }
    let ty = format!("Alias{depth}");
    format!(
        "{decls}entity AliasNest {{
    routes {{
        constructor() => []
        probe() -> u64 => [
            return(m_v)
        ]
    }}
    m_v: {ty} {{
        in constructor() => {lit} as {ty}
    }}
}}
"
    )
}

fn render_record_field_program(lit: u64) -> String {
    format!(
        r#"record Info {{
    a: u64,
    b: u64,
}}

entity RecField {{
    routes {{
        constructor() => []
        probe() -> u64 => [
            return(m_info.a + m_info.b)
        ]
    }}
    m_info: Info {{
        in constructor() => {{ let i = Info {{ a: {lit}, b: {lit} + 1 }}; i }}
    }}
}}
"#
    )
}

fn render_enum_program(lit: u64) -> String {
    format!(
        r#"enum Tag {{
    Unit,
    Num(u64),
}}

entity EnumNest {{
    routes {{
        constructor() => []
        probe() -> u64 => [
            let r = match m_tag {{
                Tag::Unit => 0,
                Tag::Num(x) => x,
            }};
            return(r)
        ]
    }}
    m_tag: Tag {{
        in constructor() => Tag::Num({lit})
    }}
}}
"#
    )
}

fn render_mixed_decl_program(lit: u64) -> String {
    format!(
        r#"type Widget = u64

enum Mode {{
    Off,
    On(u64),
}}

record Pair {{
    lo: Widget,
    hi: Widget,
}}

entity Mixed {{
    routes {{
        constructor() => []
        probe() -> u64 => [
            let m = match m_mode {{
                Mode::Off => 0,
                Mode::On(x) => x,
            }};
            return(m + m_pair.lo + m_pair.hi)
        ]
    }}
    m_mode: Mode {{
        in constructor() => Mode::On({lit})
    }}
    m_pair: Pair {{
        in constructor() => {{ let p = Pair {{ lo: {lit} as Widget, hi: ({lit} + 2) as Widget }}; p }}
    }}
}}
"#
    )
}

#[derive(Debug)]
enum TriOutcome {
    Skip,
    Pass,
    NewFail(String),
}

struct StatsSummary;

impl Drop for StatsSummary {
    fn drop(&mut self) {
        eprintln!(
            "PW3-S-005 type_fuzz tri-oracle: run={} skip={} pass={} new_fail={}",
            STAT_RUN.load(Ordering::Relaxed),
            STAT_SKIP.load(Ordering::Relaxed),
            STAT_PASS.load(Ordering::Relaxed),
            STAT_NEW_FAIL.load(Ordering::Relaxed),
        );
    }
}

fn audit_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit"))
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-type-fuzz-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ))
}

fn has_forge() -> bool {
    Command::new("forge")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn lean_build_enabled() -> bool {
    std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() == Ok("1")
}

fn has_lake() -> bool {
    Command::new("lake")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn has_blocking_errors(diags: &[Diagnostic]) -> bool {
    diags.iter().any(|d| matches!(d.severity, Severity::Error))
}

fn collect_evm_diagnostics(project: &Project) -> Vec<Diagnostic> {
    let mut diags = validate::validate(&project.merged);
    diags.extend(validate_project_config(&project.config));
    let det = project.config.resolved_deterministic_addresses();
    diags.extend(check_evm_target_compat_with(&project.merged, det));
    diags
}

fn collect_lean_diagnostics(project: &Project) -> Vec<Diagnostic> {
    let mut diags = validate::validate(&project.merged);
    diags.extend(validate_project_config(&project.config));
    diags.extend(check_lean_target_compat(&project.merged));
    diags
}

fn write_project_yaml(work_dir: &Path, project_name: &str, target: &str) -> std::io::Result<()> {
    let lean = if target == "lean" {
        "lean:\n  numerics: overflow-wrap\n"
    } else {
        ""
    };
    let yaml = format!(
        "name: {project_name}\n\
         target: {target}\n\
         deterministic_addresses: true\n\
         output_dir: build/\n\
         {lean}\
         sources:\n\
           - {CAM_NAME}\n"
    );
    std::fs::write(work_dir.join("project.yaml"), yaml)
}

fn load_project(cam_src: &str, project_name: &str, target: &str) -> Result<Project, String> {
    let work_dir = unique_out_dir(project_name);
    let _ = std::fs::remove_dir_all(&work_dir);
    std::fs::create_dir_all(&work_dir).map_err(|e| format!("mkdir work: {e}"))?;
    std::fs::write(work_dir.join(CAM_NAME), cam_src).map_err(|e| format!("write cam: {e}"))?;
    write_project_yaml(&work_dir, project_name, target).map_err(|e| format!("write yaml: {e}"))?;
    Project::load(&work_dir.join("project.yaml")).map_err(|e| format!("load project: {e}"))
}

fn write_files(files: impl IntoIterator<Item = (String, String)>, out_dir: &Path) -> Result<(), String> {
    for (rel, contents) in files {
        let path = out_dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, contents).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

fn ensure_forge_std(out_dir: &Path) {
    cambrian_transpiler::codegen::evm_test_codegen::install_forge_std(out_dir)
        .unwrap_or_else(|e| panic!("{e} in {}", out_dir.display()));
}

fn forge_build(out_dir: &Path) -> (bool, String) {
    if !out_dir.join("foundry.toml").exists() {
        let _ = std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML);
    }
    ensure_forge_std(out_dir);
    let forge = Command::new("forge")
        .args(["build", "--root"])
        .arg(out_dir)
        .output()
        .expect("forge build");
    let combined = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );
    (forge.status.success(), combined)
}

fn lake_build(project: &Project) -> (bool, String) {
    if !lean_build_enabled() {
        return (true, "skip lake (set CAMBRIAN_TEST_LEAN_BUILD=1)".into());
    }
    if !has_lake() {
        return (false, "CAMBRIAN_TEST_LEAN_BUILD=1 but lake not on PATH".into());
    }
    let out_dir = unique_out_dir("lake");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("lake out dir");
    let backend = LeanBackend::default();
    let files: HashMap<String, String> = backend.gen_project(project).into_iter().collect();
    write_files(files, &out_dir).expect("write lean project");
    let lake = Command::new("lake")
        .arg("build")
        .current_dir(&out_dir)
        .output()
        .expect("lake build");
    let combined = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&lake.stdout),
        String::from_utf8_lossy(&lake.stderr)
    );
    let ok = lake.status.success();
    let _ = std::fs::remove_dir_all(&out_dir);
    (ok, combined)
}

fn tri_oracle(case: &TypeFuzzCase) -> TriOutcome {
    STAT_RUN.fetch_add(1, Ordering::Relaxed);
    let cam = case.render();
    let slug = case.slug();
    let evm_project = match load_project(&cam, &format!("pw3-type-{slug}"), "evm") {
        Ok(p) => p,
        Err(err) => {
            STAT_NEW_FAIL.fetch_add(1, Ordering::Relaxed);
            return TriOutcome::NewFail(format!("load evm project: {err}"));
        }
    };
    let evm_diags = collect_evm_diagnostics(&evm_project);
    if has_blocking_errors(&evm_diags) {
        STAT_SKIP.fetch_add(1, Ordering::Relaxed);
        return TriOutcome::Skip;
    }
    if !has_forge() {
        STAT_SKIP.fetch_add(1, Ordering::Relaxed);
        return TriOutcome::Skip;
    }
    let out_dir = unique_out_dir(&slug);
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("forge out");
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    if let Err(err) = write_files(backend.gen_project(&evm_project), &out_dir) {
        STAT_NEW_FAIL.fetch_add(1, Ordering::Relaxed);
        return TriOutcome::NewFail(err);
    }
    let (forge_ok, forge_log) = forge_build(&out_dir);
    let _ = std::fs::remove_dir_all(&out_dir);
    if !forge_ok {
        STAT_NEW_FAIL.fetch_add(1, Ordering::Relaxed);
        return TriOutcome::NewFail(format!("validate OK but forge build failed:\n{forge_log}"));
    }
    let lean_project = match load_project(&cam, &format!("pw3-type-lean-{slug}"), "lean") {
        Ok(p) => p,
        Err(err) => {
            STAT_NEW_FAIL.fetch_add(1, Ordering::Relaxed);
            return TriOutcome::NewFail(format!("load lean project: {err}"));
        }
    };
    let lean_diags = collect_lean_diagnostics(&lean_project);
    if has_blocking_errors(&lean_diags) {
        STAT_NEW_FAIL.fetch_add(1, Ordering::Relaxed);
        return TriOutcome::NewFail(format!(
            "validate OK for EVM but Lean compat errors:\n{lean_diags:?}"
        ));
    }
    let (lake_ok, lake_log) = lake_build(&lean_project);
    if !lake_ok {
        STAT_NEW_FAIL.fetch_add(1, Ordering::Relaxed);
        return TriOutcome::NewFail(format!("validate OK but lake build failed:\n{lake_log}"));
    }
    STAT_PASS.fetch_add(1, Ordering::Relaxed);
    TriOutcome::Pass
}

// ---------------------------------------------------------------------------
// Smoke / gap documentation (green)
// ---------------------------------------------------------------------------

#[test]
fn pw3_s005_smoke_type_alias_depths_1_to_4() {
    for depth in 1..=SMOKE_DEPTH_CASES {
        let src = render_type_alias_depth_program(depth, 7);
        let mut program = ProgramParser::new()
            .parse(&src)
            .unwrap_or_else(|e| panic!("depth {depth} parse: {e}"));
        cambrian_transpiler::ast::normalize_program_types(&mut program);
        cambrian_transpiler::desugar::desugar_properties(&mut program);
        let diags = validate::validate(&program);
        assert!(
            !has_blocking_errors(&diags),
            "depth {depth} type alias chain must validate:\n{diags:?}\n{src}"
        );
    }
}

#[test]
fn pw3_o015_expr_fuzz_lacks_type_decl_axis() {
    let expr_fuzz = include_str!("test_audit_evm_expr_fuzz.rs");
    assert!(
        !expr_fuzz.contains("TypeAxisKind") && !expr_fuzz.contains("TypeAliasDepth"),
        "PW3-O-015 setup: legacy expr fuzz must not yet cover type-decl axis"
    );
    let type_fuzz = include_str!("test_audit_type_fuzz.rs");
    assert!(
        type_fuzz.contains("TypeAxisKind") && type_fuzz.contains("TypeAliasDepth"),
        "PW3-S-005 must add dedicated type-decl fuzz harness"
    );
}

#[test]
fn pw3_s005_v52_rejects_cyclic_record() {
    let src = r#"
record Node { next: Node }

entity Bad {
    routes { constructor() => [] }
    m_n: u64 { in constructor() => 0 }
}
"#;
    let mut program = ProgramParser::new().parse(src).expect("parse cyclic");
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    let diags = validate::validate(&program);
    assert!(
        diags.iter().any(|d| d.code == "V52"),
        "cyclic record must be rejected by V52:\n{diags:?}"
    );
}

#[test]
fn pw3_s005_tri_oracle_baseline_matrix() {
    let _stats = StatsSummary;
    if !has_forge() {
        eprintln!("skip PW3-S-005 tri-oracle baseline (no forge)");
        return;
    }
    let cases = [
        TypeFuzzCase {
            kind: TypeAxisKind::TypeAliasDepth,
            depth: 4,
            lit: 11,
        },
        TypeFuzzCase {
            kind: TypeAxisKind::RecordField,
            depth: 1,
            lit: 5,
        },
        TypeFuzzCase {
            kind: TypeAxisKind::EnumTagged,
            depth: 1,
            lit: 5,
        },
        TypeFuzzCase {
            kind: TypeAxisKind::MixedDecl,
            depth: 1,
            lit: 3,
        },
    ];
    for case in cases {
        match tri_oracle(&case) {
            TriOutcome::Pass | TriOutcome::Skip => {}
            TriOutcome::NewFail(detail) => {
                panic!("PW3-S-005 baseline {} failed:\n{detail}", case.slug());
            }
        }
    }
}

fn arb_type_fuzz_case() -> impl Strategy<Value = TypeFuzzCase> {
    (
        prop::sample::select(&TypeAxisKind::ALL[..]),
        1u8..=4u8,
        0u64..64u64,
    )
        .prop_map(|(kind, depth, lit)| TypeFuzzCase { kind, depth, lit })
}

fn handle_tri_outcome(outcome: TriOutcome, slug: &str) {
    match outcome {
        TriOutcome::Pass | TriOutcome::Skip => {}
        TriOutcome::NewFail(detail) => {
            panic!("PW3-S-005 tri-oracle new_fail {slug}:\n{detail}");
        }
    }
}

#[test]
fn pw3_s005_tri_oracle_proptest() {
    if !has_forge() {
        eprintln!(
            "skipping pw3_s005_tri_oracle_proptest: forge not on PATH \
             (coverage / non-Foundry CI images; Forge execution lives in \
             test-transpiler-phase-o-gates)"
        );
        return;
    }
    let config = ProptestConfig {
        cases: FUZZ_CASES,
        ..ProptestConfig::default()
    };
    proptest!(config, |(case in arb_type_fuzz_case())| {
        let _stats = StatsSummary;
        let slug = case.slug();
        let outcome = tri_oracle(&case);
        if let TriOutcome::NewFail(ref detail) = outcome {
            let repro = audit_root()
                .join("type_fuzz_repro")
                .join(format!("{slug}.cam"));
            let _ = std::fs::create_dir_all(repro.parent().unwrap());
            let _ = std::fs::write(&repro, case.render());
            eprintln!("repro written to {}", repro.display());
            eprintln!("{detail}");
        }
        handle_tri_outcome(outcome, &slug);
    });
}
