// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Session B / CAM-H-02 S4 — Lean canonical lowering + theorem slug snapshot.
//!
//! Fixture: `tests/fixtures/smafd_canonical/` + `project.lean.yaml`

use std::path::PathBuf;

use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/smafd_canonical")
}

fn has_lake() -> bool {
    use std::process::Command;
    Command::new("lake")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn transpile_lean_spec() -> String {
    let yaml = fixture_dir().join("project.lean.yaml");
    let project = Project::load(&yaml).unwrap_or_else(|e| panic!("load {}: {e}", yaml.display()));
    let entity = project
        .merged
        .entities
        .first()
        .map(|e| e.name.clone())
        .expect("entity");
    let backend = LeanBackend::default();
    let files = backend.gen_project(&project);
    files
        .into_iter()
        .find(|(p, _)| p.ends_with(&format!("{entity}Spec.lean")))
        .map(|(_, c)| c)
        .expect("Spec.lean")
}

/// Collect `theorem <slug> :` names from `RehearsalTokenSpec.lean` in source order.
fn collect_spec_theorem_slugs(spec: &str) -> Vec<String> {
    spec.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            line.strip_prefix("theorem ")
                .and_then(|rest| rest.split(':').next())
                .map(|s| s.trim().to_string())
        })
        .collect()
}

/// Pinned after CAM-H-02 merge: catalog `INV-RHT-001` is superseded; supplemental CP-* overlays emit instead.
const EXPECTED_SPEC_THEOREM_SLUGS: &[&str] = &[
    "init_mints_full_supply_to_deployer",
    "constructor_metadata_decimals",
    "transfer_happy_path",
    "transfer_guard_zero_recipient",
    "transfer_guard_insufficient_balance",
    "approve_sets_allowance",
    "approve_guard_zero_spender",
    "transferfrom_pulls_with_allowance",
    "transferfrom_guard_insufficient_allowance",
    "integration_distribute_from_deployer_to_three_holders",
    "integration_multi_holder_transfer_chain",
    "integration_approve_and_spend_with_second_spend_reverts",
    "integration_repeated_partial_transferfrom_exhausts_allowance",
    "integration_allowance_reset_to_zero_then_spend",
    "integration_zero_value_transfer_in_multi_holder_context",
    "integration_near_max_single_transfer",
    "integration_majority_supply_transfer",
    "integration_chain_transfer_then_partial_pull",
    "prop_rht_001_constructorinitialmint",
    "prop_rht_002_constructormetadata",
    "prop_rht_003_transferconservation",
    "prop_rht_004_transferrevertszerorecipient",
    "prop_rht_005_transferrevertsinsufficientbalance",
    "prop_rht_006_transferzeroamountpermitted",
    "prop_rht_008_approvesetsallowance",
    "prop_rht_009_approverevertszerospender",
    "prop_rht_010_transferfromconservationandallowancedecr",
    "prop_rht_010a_transferfrompartialallowancedecr",
    "prop_rht_011_transferfromrevertsinsufficientallowance",
    "prop_rht_012_transferfromrevertsinsufficientbalance",
    "prop_rht_013_transferfromrevertszeroaddress",
    "prop_rht_015_balanceofreflectsstorage",
    "prop_rht_016_allowancereflectsstorage",
    "prop_rht_017_totalsupplyreflectsstorage",
    "prop_rht_021_ownerisdeployer",
    "cp_001_stateful_fixed_total_supply",
    "cp_002_ctor_mint_only_total_supply",
];

fn prop_rht_013_fragment(spec: &str) -> &str {
    let start = spec
        .find("theorem prop_rht_013_transferfromrevertszeroaddress")
        .expect("prop_rht_013 theorem");
    let rest = &spec[start..];
    rest.split("theorem prop_rht_015")
        .next()
        .expect("prop_rht_013 body")
}

fn prop_rht_001_fragment(spec: &str) -> &str {
    let start = spec
        .find("theorem prop_rht_001_constructorinitialmint")
        .expect("prop_rht_001 theorem");
    let rest = &spec[start..];
    rest.split("theorem prop_rht_002")
        .next()
        .expect("prop_rht_001 body")
}

#[test]
fn smafd_lean_canonical_theorem_slug_snapshot() {
    let spec = transpile_lean_spec();
    let slugs = collect_spec_theorem_slugs(&spec);
    assert_eq!(
        slugs.len(),
        EXPECTED_SPEC_THEOREM_SLUGS.len(),
        "theorem count drift — update EXPECTED_SPEC_THEOREM_SLUGS:\n{slugs:?}"
    );
    for (got, want) in slugs.iter().zip(EXPECTED_SPEC_THEOREM_SLUGS.iter()) {
        assert_eq!(got, *want, "theorem slug order drift in RehearsalTokenSpec.lean");
    }
    assert!(
        !spec.contains("inv_rht_001_fixedtotalsupply"),
        "superseded catalog invariant must not emit a Lean theorem"
    );
}

#[test]
fn smafd_plf16_prop_rht_013_dual_throw_error_arms() {
    let spec = transpile_lean_spec();
    let frag = prop_rht_013_fragment(&spec);
    assert!(
        frag.contains("n_throw_0 = 201"),
        "first transferFrom throw must match code 201, not bare False:\n{frag}"
    );
    assert!(
        frag.contains("n_throw_1 = 201"),
        "second transferFrom throw must match code 201:\n{frag}"
    );
    assert!(
        !frag.contains("| .error _ => False\n    | .ok w =>\n      match RehearsalToken.Routes.transferFrom"),
        "first transferFrom must not use | .error _ => False before continuation:\n{frag}"
    );
}

#[test]
fn smafd_lean_canonical_prop_deployer_ctx_before_constructor() {
    let spec = transpile_lean_spec();
    let frag = prop_rht_001_fragment(&spec);
    let ctx_line = frag
        .find("let ctx := { ctx with sender := deployer }")
        .expect("deployer ctx update");
    let ctor_line = frag
        .find("RehearsalToken.Routes.constructor")
        .expect("constructor route call");
    assert!(
        ctx_line < ctor_line,
        "canonical property must set MsgCtx.sender before constructor:\n{frag}"
    );
}

#[test]
fn smafd_lean_canonical_prop_no_ctor_under_default_sender() {
    let spec = transpile_lean_spec();
    let frag = prop_rht_001_fragment(&spec);
    assert!(
        !frag.contains("let w := RehearsalToken.Routes.constructor w inst ctx deployer\n  let ctx := { ctx with sender"),
        "must not call constructor before msg ctx update (PL-F anti-pattern):\n{frag}"
    );
}

#[test]
#[ignore = "opt-in: CAMBRIAN_TEST_LEAN_BUILD=1 — lake build smafd_canonical output"]
fn smafd_lean_canonical_lake_build_smoke() {
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").ok().as_deref() != Some("1") {
        eprintln!("skipping: set CAMBRIAN_TEST_LEAN_BUILD=1");
        return;
    }
    if !has_lake() {
        eprintln!("skipping smafd_lean_canonical_lake_build_smoke: lake not on PATH");
        return;
    }
    use std::process::Command;
    // `project.lean.yaml` pins `output_dir: build-lean/` (relative to fixture root).
    let out = fixture_dir().join("build-lean");
    let yaml = fixture_dir().join("project.lean.yaml");
    let status = Command::new(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../target/release/cambrian-transpiler"),
    )
        .args(["--project", yaml.to_str().unwrap(), "--target", "lean"])
        .status()
        .expect("transpile");
    assert!(status.success(), "lean transpile failed");
    let lake = Command::new("lake")
        .arg("build")
        .current_dir(&out)
        .status()
        .expect("lake");
    assert!(lake.success(), "lake build smafd_canonical failed");
}
