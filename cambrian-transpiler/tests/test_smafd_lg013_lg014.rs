// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! LG-013 / LG-014 — cross-entity property lowering (SMAFD Run #12 class).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/smafd_lg013_lg014")
}

fn spender_spec() -> String {
    let yaml = fixture_dir().join("project.lean.yaml");
    let project = Project::load(&yaml).unwrap_or_else(|e| panic!("load {}: {e}", yaml.display()));
    LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .find(|(p, _)| p.ends_with("SpenderSpec.lean"))
        .map(|(_, c)| c)
        .expect("SpenderSpec.lean")
}

fn prop_fragment<'a>(spec: &'a str, slug: &str) -> &'a str {
    let needle = format!("theorem {slug}");
    let start = spec.find(&needle).unwrap_or_else(|| panic!("missing {slug}"));
    let rest = &spec[start..];
    rest.split("end Spender.Spec.Properties")
        .next()
        .unwrap_or(rest)
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
        "cambrian-lg013-lake-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

#[test]
fn lg014_foreign_const_in_assume() {
    let spec = spender_spec();
    let cp = prop_fragment(&spec, "prop_sp_002_foreign_const_only");
    assert!(
        cp.contains("Token.INITIAL_SUPPLY"),
        "foreign entity const must be qualified:\n{cp}"
    );
    assert!(
        !cp.contains("(Token).INITIAL_SUPPLY"),
        "must not use parenthesized entity var:\n{cp}"
    );

    let prop = prop_fragment(&spec, "prop_sp_001_cross_entity_deploy");
    assert!(
        prop.contains("Token.INITIAL_SUPPLY"),
        "main property assume must reference Token const:\n{prop}"
    );
}

#[test]
fn lg013_deploy_positional_init_without_explicit_ctor() {
    let spec = spender_spec();
    let prop = prop_fragment(&spec, "prop_sp_003_deploy_positional_init_only");
    assert!(
        prop.contains("let w := Spender.Routes.constructor w sp_inst ctx (Token.address tok_inst)"),
        "deploy Spender(tok) must run init from positional args:\n{prop}"
    );
    assert!(
        !prop.contains("folded into deploy positional init"),
        "PROP-SP-003 has no redundant explicit ctor:\n{prop}"
    );
}

#[test]
fn lg013_deploy_binding_lowers_to_address() {
    let spec = spender_spec();
    let prop = prop_fragment(&spec, "prop_sp_001_cross_entity_deploy");
    assert!(
        prop.contains("(Token.address tok_inst)"),
        "deploy binding in ctor arg must lower to CREATE2 address:\n{prop}"
    );
    assert!(
        prop.contains("Spender.Routes.constructor w sp_inst ctx (Token.address tok_inst)"),
        "chained deploy ctor must pass peer address:\n{prop}"
    );
    assert!(
        prop.contains("folded into deploy positional init")
            || prop.matches("Spender.Routes.constructor").count() >= 2,
        "explicit ctor call must fold into deploy or run init:\n{prop}"
    );
    assert!(
        !prop.contains("`expect return` dropped"),
        "expect return tok must not be dropped:\n{prop}"
    );
}

#[test]
fn lg013_lg014_lake_smoke() {
    if !lean_build_enabled() {
        eprintln!("skip lg013_lg014_lake_smoke (set CAMBRIAN_TEST_LEAN_BUILD=1)");
        return;
    }
    if !has_lake() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }

    let yaml = fixture_dir().join("project.lean.yaml");
    let project = Project::load(&yaml).expect("load fixture project");
    let files: std::collections::HashMap<String, String> = LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .collect();

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
        .args(["build", "Cambrian.Generated.SpenderSpec"])
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
        "SpenderSpec lake build must pass (LG-013/LG-014):\n{log}"
    );
}
