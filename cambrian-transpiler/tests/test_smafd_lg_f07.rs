// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! LG-012 / SMAFD LG-F07 — invariant constructor bootstrap typing.

use std::path::PathBuf;

use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;

fn plf_inv04_spec() -> String {
    let yaml = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/smafd_plf_inv04/project.lean.yaml");
    let project = Project::load(&yaml).unwrap_or_else(|e| panic!("load {}: {e}", yaml.display()));
    LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .find(|(p, _)| p.ends_with("TokenCoreSpec.lean"))
        .map(|(_, c)| c)
        .expect("TokenCoreSpec.lean")
}

fn cp_004_fragment(spec: &str) -> &str {
    let start = spec
        .find("theorem cp_004_ctor_mint_only_supply_cap")
        .expect("cp_004 theorem");
    let rest = &spec[start..];
    rest.split("end cp_004_ctor_mint_only_supply_cap")
        .next()
        .unwrap_or(rest)
}

#[test]
fn lg_f07_cp004_total_ctor_bootstrap_assigns_world() {
    let spec = plf_inv04_spec();
    let frag = cp_004_fragment(&spec);
    assert!(
        frag.contains("let w : Cambrian.Generated.World"),
        "cp_004 must pin initial world type:\n{frag}"
    );
    assert!(
        frag.contains("let w := TokenCore.Routes.constructor"),
        "total init route must assign World directly:\n{frag}"
    );
    assert!(
        !frag.contains("exceptGetD") && !frag.contains("match TokenCore.Routes.constructor"),
        "total init route must not unwrap RouteResult:\n{frag}"
    );
    assert!(
        frag.contains("Cambrian.RouteResult.okImplies (runTrace w inst ctx trace)"),
        "theorem shape unchanged:\n{frag}"
    );
}
