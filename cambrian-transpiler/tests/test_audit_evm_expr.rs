// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase I Wave 4b — EVM expression-axis audit matrix (T-EVM-EX-* / EVM-EX-H1).
//!
//! Mirrors Wave 4.1 Lean placements (`test_audit_lean_expr.rs`) for the EVM
//! target: parse → validate → `check_evm_target_compat_with` →
//! `EvmSolidityBackend::gen_project` → `forge build` (never direct `solc`).
//!
//! See `docs/AUDIT_EVM_LEAN.md` §6 Phase I Wave 4b.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::{EvmSolidityBackend, OutputBackend};
use cambrian_transpiler::project::Project;
use cambrian_transpiler::validate::{
    self, check_evm_target_compat_with, validate_project_config, Diagnostic, Severity,
};

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

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

enum Expect {
    Faithful,
    /// No EVM blocking validator analogue (documented skip).
    ValidatorSkip(&'static str),
}

struct EvmExprCell {
    id: &'static str,
    lean_mirror: &'static str,
    hypothesis: &'static str,
    program: String,
    expect: Expect,
}

fn program(members: &str, routes: &str) -> String {
    format!(
        "entity ExprProbe {{\n\
         routes {{\n\
             #[factory_only]
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
             #[factory_only]
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
        #[factory_only]
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
        #[factory_only]
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
        #[factory_only]
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
        #[factory_only]
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

fn cells() -> Vec<EvmExprCell> {
    vec![
        EvmExprCell {
            id: "T-EVM-EX-001",
            lean_mirror: "T-LEAN-EX-001",
            hypothesis: "EVM-EX-H1",
            program: program("", "add(n: u64) -> u64 => [ let t = m_count + n; return(t) ]"),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-EVM-EX-002",
            lean_mirror: "T-LEAN-EX-002",
            hypothesis: "EVM-EX-H1",
            program: program(
                "m_total: u64 { in add(n) => add: m_total + n }",
                "add(n: u64) => [ add: [] ]",
            ),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-EVM-EX-003",
            lean_mirror: "T-LEAN-EX-003",
            hypothesis: "EVM-EX-H1",
            program: program(
                "",
                "guard(n: u64) where (m_count + n > 0) : throw 1 => []",
            ),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-EVM-EX-004",
            lean_mirror: "T-LEAN-EX-004",
            hypothesis: "EVM-EX-H1",
            program: program("", "view_plus() -> u64 => [ return(m_count + 1) ]"),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-EVM-EX-005",
            lean_mirror: "T-LEAN-EX-005",
            hypothesis: "EVM-EX-H1",
            program: program("", "widen(n: u64) -> U256 => [ let w = n as U256; return(w) ]"),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-EVM-EX-006",
            lean_mirror: "T-LEAN-EX-006",
            hypothesis: "EVM-EX-H1",
            program: program("", "eq(n: u64) -> bool => [ let b = m_count == n; return(b) ]"),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-EVM-EX-007",
            lean_mirror: "T-LEAN-EX-007",
            hypothesis: "EVM-EX-H1",
            program: program(
                "m_flag: bool { in constructor() => false }",
                "flip() -> bool => [ let b = !m_flag; return(b) ]",
            ),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-EVM-EX-008",
            lean_mirror: "T-LEAN-EX-008",
            hypothesis: "EVM-EX-H1",
            program: map_exists_program(),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-EVM-EX-009",
            lean_mirror: "T-LEAN-EX-009",
            hypothesis: "EVM-EX-H1",
            program: program(
                "",
                "classify(n: u64) -> u64 => [ let c = match n { 0 => 1, _ => 2 }; return(c) ]",
            ),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-EVM-EX-010",
            lean_mirror: "T-LEAN-EX-010",
            hypothesis: "EVM-EX-H1",
            program: program("", "who() -> address => [ let s = msg::sender; return(s) ]"),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-EVM-EX-011",
            lean_mirror: "T-LEAN-EX-011",
            hypothesis: "EVM-EX-H1",
            program: program("", "now() -> u64 => [ let t = sys::timestamp; return(t) ]"),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-EVM-EX-012",
            lean_mirror: "T-LEAN-EX-012",
            hypothesis: "EVM-EX-H1",
            program: program_with_pure(
                "pure fn double(x: u64) -> u64 { x + x }",
                "",
                "dbl(n: u64) -> u64 => [ let t = double(n); return(t) ]",
            ),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-EVM-EX-013",
            lean_mirror: "T-LEAN-EX-013",
            hypothesis: "EVM-EX-H1",
            program: phased_program(),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-EVM-EX-015",
            lean_mirror: "T-LEAN-EX-015",
            hypothesis: "EVM-EX-H1",
            program: record_program(),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-EVM-EX-016",
            lean_mirror: "T-LEAN-EX-016",
            hypothesis: "EVM-EX-H1",
            program: derived_program(),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-EVM-EX-017",
            lean_mirror: "T-LEAN-EX-017",
            hypothesis: "EVM-EX-H1",
            program: trace_program(),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-EVM-EX-018",
            lean_mirror: "T-LEAN-EX-018",
            hypothesis: "EVM-EX-H1",
            program: where_msg_program(),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-EVM-EX-019",
            lean_mirror: "T-LEAN-EX-019",
            hypothesis: "EVM-EX-H1",
            program: transform_let_program(),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-EVM-EX-014",
            lean_mirror: "T-LEAN-EX-014",
            hypothesis: "EVM-EX-H1",
            program: program("", "bad() -> u64 => [ let f = |x| x + 1; return(f(1)) ]"),
            expect: Expect::ValidatorSkip(
                "E07 error on closure-as-value (EVM analogue of Lean L2)",
            ),
        },
        EvmExprCell {
            id: "T-STD-EXPR-001a",
            lean_mirror: "T-STD-LEAN-EXPR-001a",
            hypothesis: "STD-H-PLAC-1",
            program: program("", "add(n: u64) -> u64 => [ let t = std::math::min(m_count, n); return(t) ]"),
            expect: Expect::Faithful,
        },
        EvmExprCell {
            id: "T-STD-EXPR-001b",
            lean_mirror: "T-STD-LEAN-EXPR-001b",
            hypothesis: "STD-H-PLAC-1",
            program: program(
                "m_total: u64 { in constructor() => 0 in add(n) => add: std::math::min(m_total, n) }",
                "add(n: u64) => [ add: [] ]",
            ),
            expect: Expect::Faithful,
        },
    ]
}

fn audit_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit")
}

fn repro_root() -> PathBuf {
    audit_root().join("evm_expr_repro")
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-evm-expr-{}-{}-{}",
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

fn has_blocking_errors(diags: &[Diagnostic]) -> bool {
    diags
        .iter()
        .any(|d| matches!(d.severity, Severity::Error))
}

fn collect_evm_diagnostics(project: &Project) -> Vec<Diagnostic> {
    let mut diags = validate::validate(&project.merged);
    diags.extend(validate_project_config(&project.config));
    let det = project.config.resolved_deterministic_addresses();
    diags.extend(check_evm_target_compat_with(&project.merged, det));
    diags
}

fn write_project_yaml(work_dir: &Path, project_name: &str) -> std::io::Result<()> {
    let yaml = format!(
        "name: {project_name}\n\
         target: evm\n\
         deterministic_addresses: true\n\
         output_dir: build/\n\
         sources:\n\
           - {CAM_NAME}\n"
    );
    std::fs::write(work_dir.join("project.yaml"), yaml)
}

fn load_project(cam_src: &str, project_name: &str) -> Result<Project, String> {
    let work_dir = unique_out_dir(project_name);
    let _ = std::fs::remove_dir_all(&work_dir);
    std::fs::create_dir_all(&work_dir).map_err(|e| format!("mkdir work: {e}"))?;
    std::fs::write(work_dir.join(CAM_NAME), cam_src).map_err(|e| format!("write cam: {e}"))?;
    write_project_yaml(&work_dir, project_name).map_err(|e| format!("write yaml: {e}"))?;
    Project::load(&work_dir.join("project.yaml")).map_err(|e| format!("load project: {e}"))
}

fn write_codegen_files(
    files: impl IntoIterator<Item = (String, String)>,
    out_dir: &Path,
) -> Result<(), String> {
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

fn transpile_evm(project: &Project, out_dir: &Path) -> Result<(), String> {
    let det = project.config.resolved_deterministic_addresses();
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    write_codegen_files(backend.gen_project(project), out_dir)
}

fn strip_generated_invariant_tests(out_dir: &Path) {
    let test_dir = out_dir.join("test");
    let Ok(entries) = std::fs::read_dir(&test_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("Invariant_") && name.ends_with(".t.sol") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn read_transpiled_sol(out_dir: &Path) -> String {
    let src = out_dir.join("src");
    let mut parts = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&src) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "sol") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    parts.push(format!(
                        "// {}\n{text}",
                        path.file_name().unwrap().to_string_lossy()
                    ));
                }
            }
        }
    }
    parts.sort();
    parts.join("\n\n")
}

fn forge_build(out_dir: &Path) -> (bool, String) {
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

fn save_repro(cell: &EvmExprCell, cam_src: &str, sol: &str, forge_output: &str) -> PathBuf {
    let dir = repro_root().join(cell.id);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create repro dir");
    std::fs::write(dir.join("repro.cam"), cam_src).expect("write repro.cam");
    write_project_yaml(&dir, &format!("evm-expr-{}", cell.id.to_lowercase()))
        .expect("write repro yaml");
    std::fs::write(dir.join("generated.sol"), sol).expect("write generated.sol");
    let note = format!(
        "# {} / {} repro (mirror {})\n\n\
         Forge build failed on validator-clean expression cell.\n\n\
         --- forge output ---\n\
         {forge_output}\n",
        cell.id, cell.hypothesis, cell.lean_mirror
    );
    std::fs::write(dir.join("NOTE.md"), note).expect("write NOTE.md");
    dir
}

struct CellRun {
    status: &'static str,
    detail: String,
}

fn check_validator_skip(cell: &EvmExprCell, reason: &str) -> CellRun {
    let project = match load_project(&cell.program, cell.id) {
        Ok(p) => p,
        Err(e) => {
            return CellRun {
                status: "ERROR",
                detail: format!("project load failed: {e}"),
            };
        }
    };
    let diags = collect_evm_diagnostics(&project);
    let has_e07 = diags.iter().any(|d| d.code == "E07");
    let blocking = has_blocking_errors(&diags);
    if blocking && has_e07 {
        CellRun {
            status: "SKIP",
            detail: format!("documented skip ({reason}); E07 error present as expected"),
        }
    } else if blocking {
        CellRun {
            status: "SKIP",
            detail: format!("documented skip ({reason}); unexpected blocking validator errors: {diags:?}"),
        }
    } else if has_e07 {
        CellRun {
            status: "SKIP",
            detail: format!("documented skip ({reason}); E07 warning present as expected"),
        }
    } else {
        CellRun {
            status: "SKIP",
            detail: format!("documented skip ({reason}); no E07 — still no blocking EVM analogue to L2"),
        }
    }
}

fn check_faithful_cell(cell: &EvmExprCell) -> CellRun {
    let project = match load_project(&cell.program, cell.id) {
        Ok(p) => p,
        Err(e) => {
            return CellRun {
                status: "ERROR",
                detail: format!("project load failed: {e}"),
            };
        }
    };

    let diags = collect_evm_diagnostics(&project);
    if has_blocking_errors(&diags) {
        return CellRun {
            status: "ERROR",
            detail: format!("unexpected validator errors before codegen: {diags:?}"),
        };
    }

    let out_dir = unique_out_dir(cell.id);
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    if let Err(e) = transpile_evm(&project, &out_dir) {
        let _ = std::fs::remove_dir_all(&out_dir);
        return CellRun {
            status: "ERROR",
            detail: format!("codegen failed: {e}"),
        };
    }

    strip_generated_invariant_tests(&out_dir);
    if !out_dir.join("foundry.toml").exists() {
        std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).expect("foundry.toml");
    }

    let sol = read_transpiled_sol(&out_dir);
    let (ok, forge_output) = forge_build(&out_dir);
    let _ = std::fs::remove_dir_all(&out_dir);

    if ok {
        CellRun {
            status: "PASS",
            detail: "forge build clean".into(),
        }
    } else {
        let repro_dir = save_repro(cell, &cell.program, &sol, &forge_output);
        CellRun {
            status: "FAIL",
            detail: format!(
                "forge build FAIL; repro saved to {}\n{}",
                repro_dir.display(),
                &forge_output.chars().take(2048).collect::<String>()
            ),
        }
    }
}

fn check_cell(cell: &EvmExprCell) -> CellRun {
    match &cell.expect {
        Expect::Faithful => check_faithful_cell(cell),
        Expect::ValidatorSkip(reason) => check_validator_skip(cell, reason),
    }
}

fn run_cells(ids: &[&str]) -> Vec<(String, CellRun)> {
    cells()
        .into_iter()
        .filter(|c| ids.is_empty() || ids.contains(&c.id))
        .map(|cell| {
            let run = check_cell(&cell);
            (cell.id.to_string(), run)
        })
        .collect()
}

#[test]
fn audit_evm_expr_matrix_smoke() {
    if !has_forge() {
        eprintln!("skipping audit_evm_expr_matrix_smoke: forge not on PATH");
        return;
    }

    let ids = ["T-EVM-EX-001", "T-EVM-EX-005", "T-EVM-EX-015"];
    let mut failures = Vec::new();
    for (id, run) in run_cells(&ids) {
        eprintln!("{id}: {} — {}", run.status, run.detail);
        if run.status != "PASS" {
            failures.push(format!("{id}: {} — {}", run.status, run.detail));
        }
    }
    assert!(
        failures.is_empty(),
        "Wave 4b EVM expression-axis smoke failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn audit_evm_expr_matrix_forge_build() {
    if !has_forge() {
        eprintln!("skipping audit_evm_expr_matrix_forge_build: forge not on PATH");
        return;
    }

    let mut failures = Vec::new();
    eprintln!("=== Wave 4b EVM expression-axis forge-build report ===");
    for cell in cells() {
        let run = check_cell(&cell);
        eprintln!(
            "{} (mirror {}) / {}: {} — {}",
            cell.id, cell.lean_mirror, cell.hypothesis, run.status, run.detail
        );
        if matches!(cell.expect, Expect::Faithful) && run.status != "PASS" {
            failures.push(format!(
                "{} (mirror {}): {} — {}",
                cell.id, cell.lean_mirror, run.status, run.detail
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "Wave 4b EVM expression-axis forge-build failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn audit_evm_expr_std_math_smoke() {
    if !has_forge() {
        eprintln!("skipping audit_evm_expr_std_math_smoke: forge not on PATH");
        return;
    }

    let ids = ["T-STD-EXPR-001a", "T-STD-EXPR-001b"];
    let mut failures = Vec::new();
    for (id, run) in run_cells(&ids) {
        eprintln!("{id}: {} — {}", run.status, run.detail);
        if run.status != "PASS" {
            failures.push(format!("{id}: {} — {}", run.status, run.detail));
        }
    }
    assert!(
        failures.is_empty(),
        "STD-H-PLAC-1 EVM expr-axis std::math smoke failures:\n{}",
        failures.join("\n")
    );
}
