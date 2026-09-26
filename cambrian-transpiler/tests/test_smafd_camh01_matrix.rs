// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Session C — CAM-H-01 closure matrix (canonical catalog, no parity clones).
//!
//! Rows C2/C3/C4: evidence via `smafd_canonical` 38/38 + `test_sd02_fuzz_u256_max_bounds`.
//! Rows C1/C5: pinned here.

use std::path::PathBuf;

use cambrian_transpiler::codegen::{EvmSolidityBackend, OutputBackend};
use cambrian_transpiler::project::Project;

fn canonical_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/smafd_canonical")
}

fn sd02_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/audit/fixtures/smafd_sd02")
}

fn transpile_canonical() -> String {
    let yaml = canonical_dir().join("project.evm.yaml");
    let project = Project::load(&yaml).unwrap();
    let det = project.config.deterministic_addresses.unwrap_or(true);
    EvmSolidityBackend {
        deterministic_addresses: det,
    }
    .gen_project(&project)
    .into_iter()
    .find(|(p, _)| p == "test/RehearsalToken.t.sol")
    .map(|(_, c)| c)
    .expect("RehearsalToken.t.sol")
}

/// C1 — catalog `property` with wide `U256` literal (layer A).
#[test]
fn camh01_c1_property_wide_u256_decimal() {
    let yaml = sd02_dir().join("project.evm.yaml");
    let project = Project::load(&yaml).unwrap();
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let files = EvmSolidityBackend {
        deterministic_addresses: det,
    }
    .gen_project(&project);
    let sol = files
        .iter()
        .find(|(p, _)| p.contains("Sd02Token.t.sol") && !p.contains("Allowance"))
        .map(|(_, c)| c.as_str())
        .expect("Sd02Token harness");
    let body = sol
        .split("function test_decimal_max_roundtrip()")
        .nth(1)
        .expect("decimal max property harness");
    const U256_MAX_HEX: &str =
        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    let wide_hex_count = body.matches(U256_MAX_HEX).count();
    assert!(
        wide_hex_count >= 2,
        "wide decimal property must lower full-width max (got {wide_hex_count}): {body}"
    );
}

/// C5 — `test` vs `property` same ctor-mint scenario: hoisted factory deploy in `setUp`.
#[test]
fn camh01_c5_test_property_ctor_semantics_match() {
    let sol = transpile_canonical();
    let setup = sol
        .split("function setUp() public {")
        .nth(1)
        .and_then(|s| s.split("\n    function ").next())
        .expect("setUp fragment");
    assert!(
        setup.contains("deployRehearsalToken(") && setup.contains("vm.startPrank("),
        "BUG-U4 ctor hoist must deploy+prank in setUp: {setup}"
    );
    let test_frag = sol
        .split("function test_init_mints_full_supply_to_deployer()")
        .nth(1)
        .and_then(|s| s.split("function test_").next())
        .expect("unit test fragment");
    let prop_frag = sol
        .split("function test_PROP_RHT_001_ConstructorInitialMint()")
        .nth(1)
        .and_then(|s| s.split("function ").next())
        .expect("property fragment");
    for frag in [test_frag, prop_frag] {
        assert!(
            !frag.contains("deployRehearsalToken("),
            "hoisted ctor suites must not redeploy in test body: {frag}"
        );
    }
}

/// C4 — merged `CP-001` (catalog `INV-RHT-001` + init delta) seeds ctor-owned state via `initialize`, not `vm.store`.
#[test]
fn camh01_c4_catalog_invariant_ctor_init() {
    let yaml = canonical_dir().join("project.evm.yaml");
    let project = Project::load(&yaml).unwrap();
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let files = EvmSolidityBackend {
        deterministic_addresses: det,
    }
    .gen_project(&project);
    let inv = files
        .iter()
        .find(|(p, _)| p.contains("CP_001_stateful_fixed_total_supply"))
        .map(|(_, c)| c.as_str())
        .expect("CP-001 merged invariant harness");
    assert!(
        inv.contains("deployRehearsalToken(") || inv.contains(".initialize("),
        "catalog invariant setUp must seed via factory deploy or initialize: {inv}"
    );
    assert!(
        !inv.contains("vm.store(address(_rehearsalToken), bytes32(uint256(0)), bytes32(uint256(1000000000000000000000000)))"),
        "total supply must not be vm.store-seeded when ctor covers it: {inv}"
    );
}

/// C2 — multi-`msg { sender }` mid-body (CTI: PROP-RHT-010a integration test).
#[test]
fn camh01_c2_multi_msg_prank_catalog() {
    let sol = transpile_canonical();
    let frag = sol
        .split("function test_integration_repeated_partial_transferFrom_exhausts_allowance()")
        .nth(1)
        .and_then(|s| s.split("function ").next())
        .expect("PROP-RHT-010a integration harness");
    let prank_count = frag.matches("vm.startPrank(").count();
    assert!(
        prank_count >= 2,
        "deployer + spender msg blocks need startPrank (got {prank_count}): {frag}"
    );
}
