// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! INT-004 follow-up — bare `call route()` after `deploy sut = Entity(...)` must
//! use the deployed instance var, not the prefix `inst`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lean_sut_deploy_self_call")
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn pair_spec() -> String {
    let yaml = fixture_dir().join("project.lean.yaml");
    let project = Project::load(&yaml).unwrap_or_else(|e| panic!("load {}: {e}", yaml.display()));
    LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .find(|(p, _)| p.ends_with("PairSpec.lean"))
        .map(|(_, c)| c)
        .expect("PairSpec.lean")
}

fn theorem_fragment<'a>(spec: &'a str, slug: &str) -> &'a str {
    let needle = format!("theorem {slug}");
    let start = spec.find(&needle).unwrap_or_else(|| panic!("missing {slug}"));
    let tail = &spec[start..];
    let end = tail[needle.len()..]
        .find("\ntheorem ")
        .map(|i| needle.len() + i)
        .unwrap_or(tail.len());
    &tail[..end]
}

fn prop_fragment<'a>(spec: &'a str, slug: &str) -> &'a str {
    let needle = format!("theorem {slug}");
    let start = spec.find(&needle).unwrap_or_else(|| panic!("missing {slug}"));
    let rest = &spec[start..];
    rest.split("end Pair.Spec.Properties")
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

fn unique_out_dir(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "cambrian-{prefix}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

fn write_project_to(out_dir: &PathBuf, project: &Project) {
    let files: std::collections::HashMap<String, String> = LeanBackend::default()
        .gen_project(project)
        .into_iter()
        .collect();
    let _ = fs::remove_dir_all(out_dir);
    fs::create_dir_all(out_dir).expect("create out dir");
    for (rel, content) in &files {
        let path = out_dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&path, content).expect("write generated file");
    }
}

#[test]
fn self_call_before_sut_deploy_uses_default_inst() {
    let spec = pair_spec();
    let body = theorem_fragment(&spec, "self_call_before_sut_deploy_uses_default_inst");
    assert!(
        body.contains("Pair.Routes.token0 w inst ctx"),
        "pre-deploy self-call must use prefix inst:\n{body}"
    );
    assert!(
        !body.contains("pair_inst"),
        "must not reference deploy inst before deploy:\n{body}"
    );
}

#[test]
fn self_call_after_sut_deploy_uses_deployed_inst() {
    let spec = pair_spec();
    let body = theorem_fragment(&spec, "self_call_after_sut_deploy_uses_deployed_inst");
    assert!(
        body.contains("Pair.Routes.token0 w pair_inst ctx"),
        "post-deploy self-call must use deployed pair_inst:\n{body}"
    );
    assert!(
        body.contains("Pair.Routes.token1 w pair_inst ctx"),
        "second self-call must also use pair_inst:\n{body}"
    );
    assert!(
        !body.contains("Pair.Routes.token0 w inst ctx"),
        "must not use stale prefix inst after deploy:\n{body}"
    );
    assert!(
        !body.contains("`expect return` dropped"),
        "expect return on deploy binding must be kept:\n{body}"
    );
}

#[test]
fn redeploy_updates_active_sut_inst() {
    let spec = pair_spec();
    let body = theorem_fragment(&spec, "redeploy_updates_active_sut_inst");
    assert!(
        body.contains("Pair.Routes.token0 w pair2_inst ctx"),
        "latest SUT deploy binding must win:\n{body}"
    );
    assert!(
        !body.contains("Pair.Routes.token0 w pair_inst ctx"),
        "must not call through superseded pair_inst:\n{body}"
    );
}

#[test]
fn property_self_call_after_sut_deploy_uses_deployed_inst() {
    let spec = pair_spec();
    let body = prop_fragment(&spec, "prop_self_call_after_sut_deploy");
    assert!(
        body.contains("Pair.Routes.token0 w pair_inst ctx"),
        "property self-call must use deployed inst:\n{body}"
    );
}

#[test]
fn int004_uniswap_constructor_wires_self_call_inst() {
    let yaml = workspace_root().join("examples/uniswap-v2/project.lean.yaml");
    let project = Project::load(&yaml).unwrap_or_else(|e| panic!("load uniswap: {e}"));
    let spec = LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .find(|(p, _)| p.ends_with("UniswapV2PairSpec.lean"))
        .map(|(_, c)| c)
        .expect("UniswapV2PairSpec.lean");

    let body = theorem_fragment(&spec, "constructor_wires_both_token_identities");
    assert!(
        body.contains("UniswapV2Pair.Routes.token0 w pair_inst ctx"),
        "INT-004: token0 must use pair_inst:\n{body}"
    );
    assert!(
        body.contains("UniswapV2Pair.Routes.token1 w pair_inst ctx"),
        "INT-004: token1 must use pair_inst:\n{body}"
    );
}

#[test]
fn sut_deploy_self_call_lake_smoke() {
    if !lean_build_enabled() {
        eprintln!("skip sut_deploy_self_call_lake_smoke (set CAMBRIAN_TEST_LEAN_BUILD=1)");
        return;
    }
    if !has_lake() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }

    let yaml = fixture_dir().join("project.lean.yaml");
    let project = Project::load(&yaml).expect("load fixture project");
    let out_dir = unique_out_dir("sut-deploy");
    write_project_to(&out_dir, &project);

    let lake = Command::new("lake")
        .args(["build", "Cambrian.Generated.PairSpec"])
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
        "PairSpec lake build must pass (INT-004 follow-up):\n{log}"
    );
}
