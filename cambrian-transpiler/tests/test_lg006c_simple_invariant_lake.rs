// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! LG-006c — transfer-class CTE invariant `lake build` (invByCases macro + ladder).
//!
//! See `docs/TASK_LG006C_INVBYCASES_MACRO_SYNTAX.md`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;

fn audit_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/audit")
}

fn fixture_project_yaml() -> PathBuf {
    audit_root().join("fixtures/smafd_lg006_simple/project.yaml")
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

fn unique_out_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "cambrian-lg006c-lake-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

#[test]
fn lg006c_simple_invariant_lake_build_spec_module() {
    if !lean_build_enabled() {
        eprintln!("skip lg006c_simple_invariant_lake_build_spec_module (set CAMBRIAN_TEST_LEAN_BUILD=1)");
        return;
    }
    if !has_lake() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }

    let yaml = fixture_project_yaml();
    let project = Project::load(&yaml).expect("load smafd_lg006_simple project");
    let files: std::collections::HashMap<String, String> = LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .collect();

    let spec = files
        .get("Cambrian/Generated/TokenSpec.lean")
        .expect("TokenSpec.lean generated");
    assert!(
        spec.contains("scoped macro \"invByCases\""),
        "transfer-class invariant must emit invByCases (LG-006c):\n{spec}",
    );
    assert!(
        spec.contains("(try omega))))"),
        "invByCases macro must close tactic quasiquote (LG-006c):\n{spec}",
    );
    assert!(
        spec.contains("| (set_option maxHeartbeats 80000 in (invByCases; done))"),
        "ladder must not try-wrap invByCases rung (LG-006c):\n{spec}",
    );

    let out_dir = unique_out_dir();
    let _ = fs::remove_dir_all(&out_dir);
    fs::create_dir_all(&out_dir).expect("create out dir");
    for (rel, content) in &files {
        let path = out_dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&path, content).expect("write generated file");
    }

    let lake = Command::new("lake")
        .args(["build", "Cambrian.Generated.TokenSpec"])
        .current_dir(&out_dir)
        .output()
        .expect("invoke lake");

    let log = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&lake.stdout),
        String::from_utf8_lossy(&lake.stderr)
    );

    let _ = fs::remove_dir_all(&out_dir);

    assert!(
        lake.status.success(),
        "LG-006c: TokenSpec lake build must pass for transfer-class invByCases invariant\n{log}",
    );
}
