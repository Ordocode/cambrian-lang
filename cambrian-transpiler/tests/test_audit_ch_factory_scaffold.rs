// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! CH factory scaffolding — deterministic-mode constructor guards (WP-B/C).

use std::fs;
use std::path::PathBuf;

use cambrian_transpiler::codegen::{EvmSolidityBackend, OutputBackend, gen_evm_solidity_opts};
use cambrian_transpiler::project::Project;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn governor_project_sol() -> String {
    let yaml = repo_root().join("examples/governor/project.yaml");
    let project = Project::load(&yaml).unwrap_or_else(|e| panic!("load {}: {e}", yaml.display()));
    let backend = EvmSolidityBackend {
        deterministic_addresses: project.config.resolved_deterministic_addresses(),
    };
    let files: Vec<(String, String)> = backend.gen_project(&project);
    files
        .into_iter()
        .find(|(p, _): &(String, String)| p.contains("_project.sol"))
        .map(|(_, c)| c)
        .expect("combined project sol")
}

#[test]
fn ch_factory_zero_check_emitted() {
    let combined = governor_project_sol();
    assert!(
        combined.contains("require(factory_ != address(0), \"zero factory\")"),
        "deterministic ctor must reject zero factory address:\n{combined}"
    );
}

#[test]
fn ch_constructor_payable_by_default() {
    let combined = governor_project_sol();
    assert!(
        combined.contains("constructor(address factory_) payable"),
        "default project must keep payable constructor for deploy-with-value:\n{combined}"
    );
}

#[test]
fn ch_constructor_non_payable_when_flag_false() {
    let work = std::env::temp_dir().join(format!(
        "cambrian-ch-wpc-{}-{}",
        std::process::id(),
        "ctor"
    ));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).expect("mkdir work");
    fs::write(
        work.join("token.cam"),
        r"entity RehearsalToken {
    routes {
        constructor() => []
    }
    m_supply: U256 {
        in constructor() => 0
    }
}
",
    )
    .expect("write cam");
    fs::write(
        work.join("project.yaml"),
        r"name: ch-wpc
target: evm
output_dir: build/
deterministic_addresses: true
evm:
  allow_constructor_payable: false
sources:
  - token.cam
",
    )
    .expect("write yaml");

    let project =
        Project::load(&work.join("project.yaml")).expect("load wp-c project");
    assert!(!project.config.resolved_evm().allow_constructor_payable());

    let sol = gen_evm_solidity_opts(&project.merged, true, false);
    assert!(
        !sol.contains("constructor(address factory_) payable"),
        "flag false must omit payable on deterministic constructor:\n{sol}"
    );
    assert!(
        sol.contains("constructor(address factory_) {"),
        "constructor must still emit:\n{sol}"
    );
    assert!(
        sol.contains("new RehearsalToken{salt: bytes32(0), value: 0}"),
        "factory CREATE2 must not forward msg.value when ctor is non-payable:\n{sol}"
    );

    let _ = fs::remove_dir_all(&work);
}
