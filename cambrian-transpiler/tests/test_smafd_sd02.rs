// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! SD-02 S5a/S5b — wide U256 literals in property surface (SMAFD PROP-CORE-008 shaped).
//!
//! Fixture: `tests/audit/fixtures/smafd_sd02/`

use std::path::PathBuf;

use cambrian_transpiler::codegen::{EvmSolidityBackend, LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;

const U256_MAX_HEX_NIBBLES: &str =
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/audit/fixtures/smafd_sd02")
}

fn sd02_token_test_sol(files: &[(String, String)]) -> &str {
    files
        .iter()
        .find(|(p, _)| p.contains("Sd02Token.t.sol"))
        .map(|(_, c)| c.as_str())
        .expect("Sd02Token forge test harness")
}

fn load_project(yaml_name: &str) -> Project {
    let yaml = fixture_dir().join(yaml_name);
    Project::load(&yaml).unwrap_or_else(|e| panic!("load {}: {e}", yaml.display()))
}

#[test]
fn smafd_sd02_hex_max_property_transpiles_evm() {
    let project = load_project("project.evm.yaml");
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let files = EvmSolidityBackend {
        deterministic_addresses: det,
    }
    .gen_project(&project);
    let project_sol = files
        .iter()
        .find(|(p, _)| p.contains("_project.sol"))
        .map(|(_, c)| c.as_str())
        .expect("combined project .sol");
    assert!(
        project_sol.contains(U256_MAX_HEX_NIBBLES),
        "entity route must lower 64-nibble U256 max:\n{project_sol}"
    );
    let test_sol = sd02_token_test_sol(&files);
    assert!(
        test_sol.contains(U256_MAX_HEX_NIBBLES),
        "property expect return must lower wide hex in harness:\n{test_sol}"
    );
}

#[test]
fn smafd_sd02_hex_max_property_transpiles_lean() {
    let project = load_project("project.lean.yaml");
    let files = LeanBackend::default().gen_project(&project);
    let spec = files
        .iter()
        .find(|(p, _)| p.ends_with("Sd02TokenSpec.lean"))
        .map(|(_, c)| c.as_str())
        .expect("Sd02TokenSpec.lean");
    assert!(
        spec.to_ascii_lowercase().contains(U256_MAX_HEX_NIBBLES),
        "Lean spec must retain 64-nibble U256 max literal:\n{spec}"
    );
}

#[test]
fn smafd_sd02_decimal_max_property_transpiles_evm() {
    let project = load_project("project.evm.yaml");
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let files = EvmSolidityBackend {
        deterministic_addresses: det,
    }
    .gen_project(&project);
    let test_sol = sd02_token_test_sol(&files);
    let body = test_sol
        .split("function test_decimal_max_roundtrip()")
        .nth(1)
        .expect("decimal max property harness");
    assert!(
        body.contains(U256_MAX_HEX_NIBBLES),
        "wide decimal + U256::MAX expect returns must lower full-width:\n{body}"
    );
    let wide_hex_count = body.matches(U256_MAX_HEX_NIBBLES).count();
    assert!(
        wide_hex_count >= 2,
        "both expect return lines must lower to full-width max (got {wide_hex_count}):\n{body}"
    );
}
