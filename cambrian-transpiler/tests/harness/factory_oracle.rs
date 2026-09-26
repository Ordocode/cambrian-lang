// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Shared assertions for CambrianFactory harness emission (BUG-U4 / U4-6).

/// Body of `function setUp()` up to the next `function` keyword.
pub fn extract_setup_sol(full_sol: &str) -> &str {
    full_sol
        .split("function setUp()")
        .nth(1)
        .and_then(|s| s.split("function ").next())
        .unwrap_or("")
}

/// Deterministic lowered Foundry `setUp()` must deploy via factory, not legacy `new Entity`.
pub fn assert_factory_setup(setup_sol: &str, entity_name: &str) {
    assert!(
        setup_sol.contains("new CambrianFactory()"),
        "setUp must deploy CambrianFactory (U4-4): {setup_sol}"
    );
    let deploy_fn = format!("deploy{entity_name}(");
    assert!(
        setup_sol.contains(&deploy_fn),
        "setUp must call factory.{deploy_fn}: {setup_sol}"
    );
}

/// After U4-6 Phase 4, deterministic harness must not use bare `new {Entity}(` for SUT.
pub fn assert_no_sut_legacy_new(test_sol: &str, entity_name: &str) {
    let needle = format!("new {entity_name}(");
    assert!(
        !test_sol.contains(&needle),
        "deterministic harness must not use legacy {needle}: {test_sol}"
    );
}
