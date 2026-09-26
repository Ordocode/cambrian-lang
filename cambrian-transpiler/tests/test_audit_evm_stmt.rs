// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase J Wave 5 — EVM statement / action-axis audit matrix (T-EVM-ST-*).
//!
//! Mirrors `test_lean_matrix.rs` faithful cells where applicable; adds
//! action-scoping cells from manual review (EVM-ST-H1).
//!
//! See `docs/AUDIT_PHASE_J.md` and `docs/AUDIT_EVM_LEAN.md` §6 Wave 5.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::{EvmSolidityBackend, OutputBackend};
use cambrian_transpiler::project::Project;
use cambrian_transpiler::validate::{
    self, check_evm_target_compat_with, validate_project_config, Diagnostic, Severity,
};

const CAM_NAME: &str = "input.cam";
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

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

enum Expect {
    /// Must `forge build` clean (faithful lowering).
    Faithful,
}

struct StmtCell {
    id: &'static str,
    hypothesis: &'static str,
    lean_mirror: &'static str,
    program: String,
    expect: Expect,
}

fn program(decls: &str, route: &str) -> String {
    format!(
        "{decls}entity StmtProbe {{\n\
         routes {{\n\
             #[factory_only]
             constructor() => []\n\
             {route}\n\
         }}\n\
         m_count: u64 {{ in constructor() => 0 }}\n\
         }}\n"
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

fn var_escapes_phased_if_program() -> String {
    r#"extern entity Oracle {
    view route isAlive() -> bool;
}

entity Caller {
    identity m_id: u64
    routes {
        #[factory_only]
        constructor(oracle: Address<Oracle>) => []
        check(flag: bool) -> bool => [
            fetch: [
                if flag => [ var alive = isAlive() ~> m_oracle; ]
                return(alive)
            ]
        ]
    }
    m_oracle: Address<Oracle> { in constructor(oracle) => oracle }
}
"#
    .to_string()
}

fn cells() -> Vec<StmtCell> {
    vec![
        // ---- CONFIRMED (EVM-ST-H1) ----------------------------------------
        StmtCell {
            id: "T-EVM-ST-001",
            hypothesis: "EVM-ST-H1",
            lean_mirror: "—",
            program: let_escapes_if_program(),
            expect: Expect::Faithful,
        },
        StmtCell {
            id: "T-EVM-ST-002",
            hypothesis: "EVM-ST-H1",
            lean_mirror: "—",
            program: var_escapes_phased_if_program(),
            expect: Expect::Faithful,
        },
        // ---- Lean matrix mirrors (GAP-001 parity) ---------------------------
        StmtCell {
            id: "T-EVM-ST-010",
            hypothesis: "EVM-ST-H0",
            lean_mirror: "throw/top",
            program: program("", "fThrow() => [ throw 7 ]"),
            expect: Expect::Faithful,
        },
        StmtCell {
            id: "T-EVM-ST-011",
            hypothesis: "EVM-ST-H0",
            lean_mirror: "emit/if",
            program: program(
                "    event Ev(x: u64);\n",
                "fEmitIf(n: u64) => [ if n > 0 => [ emit Ev(n); ] ]",
            ),
            expect: Expect::Faithful,
        },
        StmtCell {
            id: "T-EVM-ST-012",
            hypothesis: "EVM-ST-H0",
            lean_mirror: "send/top",
            program: program(
                "",
                "fSend(dest: address, n: u64) => [ ~> dest with { value: n } ]",
            ),
            expect: Expect::Faithful,
        },
        StmtCell {
            id: "T-EVM-ST-013",
            hypothesis: "EVM-ST-H0",
            lean_mirror: "throw/if",
            program: program("", "fThrowIf(n: u64) => [ if n > 0 => [ throw 7 ] ]"),
            expect: Expect::Faithful,
        },
        StmtCell {
            id: "T-EVM-ST-014",
            hypothesis: "EVM-ST-H0",
            lean_mirror: "throw/for",
            program: program(
                "",
                "fThrowFor(items: Vec<u64>) => [ for x in items => [ if x > 0 => [ throw 7 ] ] ]",
            ),
            expect: Expect::Faithful,
        },
        StmtCell {
            id: "T-EVM-ST-015",
            hypothesis: "EVM-ST-H0",
            lean_mirror: "throw_custom/if",
            program: program(
                "    error TooSmall(x: u64);\n",
                "fThrowCustom(n: u64) => [ if n > 0 => [ throw TooSmall(n) ] ]",
            ),
            expect: Expect::Faithful,
        },
        StmtCell {
            id: "T-EVM-ST-016",
            hypothesis: "EVM-ST-H0",
            lean_mirror: "emit/for",
            program: program(
                "    event Ev(x: u64);\n",
                "fEmitFor(items: Vec<u64>) => [ for x in items => [ emit Ev(x); ] ]",
            ),
            expect: Expect::Faithful,
        },
        StmtCell {
            id: "T-EVM-ST-017",
            hypothesis: "EVM-ST-H0",
            lean_mirror: "send/if",
            program: program(
                "",
                "fSendIf(dest: address, n: u64) => [ if n > 0 => [ ~> dest with { value: n } ] ]",
            ),
            expect: Expect::Faithful,
        },
        StmtCell {
            id: "T-EVM-ST-018",
            hypothesis: "EVM-ST-H0",
            lean_mirror: "send/for",
            program: program(
                "",
                "fSendFor(recipients: Vec<address>, n: u64) => [ for r in recipients => [ ~> r with { value: n } ] ]",
            ),
            expect: Expect::Faithful,
        },
        StmtCell {
            id: "T-EVM-ST-019",
            hypothesis: "EVM-ST-H0",
            lean_mirror: "return/if",
            program: program(
                "",
                "fRetIf(n: u64) -> u64 => [ if n > 0 => [ return(n) ] else [ return(0) ] ]",
            ),
            expect: Expect::Faithful,
        },
        StmtCell {
            id: "T-STD-STMT-001a",
            hypothesis: "STD-H-PLAC-1",
            lean_mirror: "T-STD-LEAN-STMT-001a",
            program: program(
                "",
                "clampIf() -> u64 => [ if m_count < 200 => [ let capped = std::math::clamp(m_count, 0, 100); return(capped) ] else [ return(0) ] ]",
            ),
            expect: Expect::Faithful,
        },
        StmtCell {
            id: "T-STD-STMT-001b",
            hypothesis: "STD-H-PLAC-1",
            lean_mirror: "T-STD-LEAN-STMT-001b",
            program: program(
                "",
                "clampFor(items: Vec<u64>) -> u64 => [ for i in items => [ let capped = std::math::clamp(i, 0, 50); if capped < 51 => [ ] ] return(0) ]",
            ),
            expect: Expect::Faithful,
        },
    ]
}

fn repro_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/audit/evm_stmt_repro")
}

fn unique_out_dir(stem: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-evm-stmt-{stem}-{n}-{}",
        std::process::id()
    ))
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

fn collect_evm_diagnostics(project: &Project) -> Vec<Diagnostic> {
    let mut diags = validate::validate(&project.merged);
    diags.extend(validate_project_config(&project.config));
    let det = project.config.resolved_deterministic_addresses();
    diags.extend(check_evm_target_compat_with(&project.merged, det));
    diags
}

fn has_blocking_errors(diags: &[Diagnostic]) -> bool {
    diags
        .iter()
        .any(|d| matches!(d.severity, Severity::Error))
}

fn transpile_evm(project: &Project, out_dir: &Path) -> Result<(), String> {
    let det = project.config.resolved_deterministic_addresses();
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    for (rel, contents) in backend.gen_project(project) {
        let path = out_dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, contents).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

fn read_transpiled_sol(out_dir: &Path) -> String {
    let src = out_dir.join("src");
    let mut parts = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&src) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "sol") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    parts.push(text);
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

fn save_repro(cell: &StmtCell, cam_src: &str, sol: &str, forge_output: &str) -> PathBuf {
    let dir = repro_root().join(cell.id);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create repro dir");
    std::fs::write(dir.join("repro.cam"), cam_src).expect("write repro.cam");
    write_project_yaml(&dir, &format!("evm-stmt-{}", cell.id.to_lowercase()))
        .expect("write repro yaml");
    std::fs::write(dir.join("generated.sol"), sol).expect("write generated.sol");
    let note = format!(
        "# {} / {} repro\n\n\
         Expected faithful `forge build`; got solc error.\n\n\
         --- forge output ---\n{forge_output}\n",
        cell.id, cell.hypothesis
    );
    std::fs::write(dir.join("NOTE.md"), note).expect("write NOTE.md");
    dir
}

fn has_forge() -> bool {
    Command::new("forge")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

struct CellRun {
    status: &'static str,
    detail: String,
}

fn check_faithful_cell(cell: &StmtCell) -> CellRun {
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
            detail: format!("unexpected validator errors: {diags:?}"),
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
                "forge build FAIL; repro {}\n{}",
                repro_dir.display(),
                &forge_output.chars().take(1500).collect::<String>()
            ),
        }
    }
}

#[test]
fn audit_evm_stmt_matrix_smoke() {
    if !has_forge() {
        eprintln!("skipping audit_evm_stmt_matrix_smoke: forge not on PATH");
        return;
    }
    let ids = [
        "T-EVM-ST-010",
        "T-EVM-ST-011",
        "T-EVM-ST-012",
        "T-EVM-ST-017",
    ];
    let mut failures = Vec::new();
    for cell in cells().into_iter().filter(|c| ids.contains(&c.id)) {
        let run = check_faithful_cell(&cell);
        eprintln!("{}: {} — {}", cell.id, run.status, run.detail);
        if run.status != "PASS" {
            failures.push(format!("{}: {}", cell.id, run.detail));
        }
    }
    assert!(
        failures.is_empty(),
        "Wave 5 EVM stmt smoke failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn audit_evm_stmt_matrix_forge_build() {
    if !has_forge() {
        eprintln!("skipping audit_evm_stmt_matrix_forge_build: forge not on PATH");
        return;
    }
    let mut failures = Vec::new();
    eprintln!("=== Wave 5 EVM statement-axis forge-build report ===");
    for cell in cells() {
        let run = check_faithful_cell(&cell);
        eprintln!(
            "{} ({}) / {}: {} — {}",
            cell.id, cell.lean_mirror, cell.hypothesis, run.status, run.detail
        );
        if matches!(cell.expect, Expect::Faithful) && run.status != "PASS" {
            failures.push(format!(
                "{} ({}): {} — {}",
                cell.id, cell.lean_mirror, run.status, run.detail
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "Wave 5 EVM statement-axis forge-build failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn audit_evm_stmt_std_smoke() {
    if !has_forge() {
        eprintln!("skipping audit_evm_stmt_std_smoke: forge not on PATH");
        return;
    }

    let ids = ["T-STD-STMT-001a", "T-STD-STMT-001b"];
    let mut failures = Vec::new();
    for cell in cells().into_iter().filter(|c| ids.contains(&c.id)) {
        let run = check_faithful_cell(&cell);
        eprintln!("{}: {} — {}", cell.id, run.status, run.detail);
        if run.status != "PASS" {
            failures.push(format!("{}: {}", cell.id, run.detail));
        }
    }
    assert!(
        failures.is_empty(),
        "STD-H-PLAC-1 EVM stmt-axis std::math smoke failures:\n{}",
        failures.join("\n")
    );
}

/// E26: `rescue`/`recover` is Acki Nacki bounce; EVM domain rejects it (no try/catch).
#[test]
fn audit_evm_stmt_rescue_rejected_e26() {
    let cam = include_str!("../../contracts/bouncer.cam");
    let project = load_project(cam, "T-EVM-ST-020").expect("load bouncer");
    let diags = collect_evm_diagnostics(&project);
    assert!(
        diags.iter().any(|d| d.code == "E26"),
        "bouncer rescue/recover must be E26 on EVM: {diags:?}"
    );
    let out_dir = unique_out_dir("T-EVM-ST-020");
    std::fs::create_dir_all(&out_dir).unwrap();
    transpile_evm(&project, &out_dir).expect("force-codegen bouncer");
    let sol = read_transpiled_sol(&out_dir);
    let _ = std::fs::remove_dir_all(&out_dir);
    assert!(
        sol.contains("E26"),
        "force-codegen must comment rescue (E26), not try/catch"
    );
    assert!(
        !sol.contains("try "),
        "EVM must not lower rescue to try/catch: {sol}"
    );
}
