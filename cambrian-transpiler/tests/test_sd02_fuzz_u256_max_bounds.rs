// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! SD-02 S5b — `fuzz { amount in 0..U256::MAX }` → plausible-bounds + Foundry `bound()`.

use std::path::PathBuf;

#[cfg(feature = "plausible")]
use cambrian_transpiler::codegen::LeanBackend;
use cambrian_transpiler::codegen::{EvmSolidityBackend, OutputBackend};
use cambrian_transpiler::project::Project;

const U256_MAX_DECIMAL: &str =
    "115792089237316195423570985008687907853269984665640564039457584007913129639935";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/audit/fixtures/smafd_sd02")
}

fn load_project(yaml_name: &str) -> Project {
    let yaml = fixture_dir().join(yaml_name);
    Project::load(&yaml).unwrap_or_else(|e| panic!("load {}: {e}", yaml.display()))
}

#[cfg(feature = "plausible")]
#[test]
fn sd02_fuzz_u256_max_plausible_bounds_json() {
    let project = load_project("project.lean.yaml");
    let files = LeanBackend::default().gen_project(&project);
    let bounds = files
        .iter()
        .find(|(p, _)| p == "plausible-bounds.json")
        .map(|(_, c)| c.as_str())
        .expect("plausible-bounds.json sidecar");
    let doc: serde_json::Value = serde_json::from_str(bounds).expect("bounds JSON must parse");
    let props = doc["properties"].as_array().expect("properties array");
    let amount = props
        .iter()
        .flat_map(|p| p["binders"].as_array())
        .flatten()
        .find(|b| b["name"] == "amount" && b.get("bound").is_some())
        .expect("amount binder with fuzz bound");
    let bound = amount["bound"].as_object().expect("amount bound");
    assert_eq!(bound["lo"], serde_json::json!(0));
    assert_eq!(
        bound["hi"].as_str(),
        Some(U256_MAX_DECIMAL),
        "wide hi must be decimal string:\n{bounds}"
    );
    assert_eq!(bound["inclusive"], serde_json::json!(false));
}

#[test]
fn sd02_fuzz_u256_max_foundry_bound_harness() {
    let project = load_project("project.evm.yaml");
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let files = EvmSolidityBackend {
        deterministic_addresses: det,
    }
    .gen_project(&project);
    let test_sol = files
        .iter()
        .find(|(p, c)| {
            p.contains("test/") && p.ends_with(".t.sol") && c.contains("bound(amount, 0, ")
        })
        .map(|(_, c)| c.as_str())
        .expect("Sd02Token fuzz harness with wide bound");
    assert!(
        test_sol.contains("bound(amount, 0, "),
        "fuzz harness must bound amount from 0:\n{test_sol}"
    );
    let max_hi = test_sol.contains(U256_MAX_DECIMAL)
        || test_sol.contains("0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");
    assert!(
        max_hi && test_sol.contains(") - 1"),
        "exclusive hi must lower full-width U256::MAX for (MAX)-1:\n{test_sol}"
    );
}
