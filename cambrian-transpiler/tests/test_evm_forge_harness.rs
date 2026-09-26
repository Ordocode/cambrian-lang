// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Regression: Forge harness for lowered EVM tests (RehearsalToken fixture).
//! See `docs/TASK_EVM_FORGE_LOWERED_HARNESS.md`.

use cambrian_transpiler::codegen::evm_test_codegen::{
    generate_evm_tests, generate_evm_tests_for_project,
};
use cambrian_transpiler::project::{InvariantConfig, Project};
use cambrian_transpiler::target::Target;
use cambrian_transpiler::validate::check_target_compat;
use cambrian_transpiler::ProgramParser;
use std::path::PathBuf;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/evm_forge_harness/RehearsalToken")
}

fn transpile_project(yaml_name: &str) -> Vec<(String, String)> {
    let yaml = fixture_dir().join(yaml_name);
    let project = Project::load(&yaml).expect("load rehearsal project");
    let det = project.config.resolved_deterministic_addresses();
    generate_evm_tests_for_project(
        &project.merged,
        det,
        &project.config.resolved_invariant(),
        Some(project.name().as_str()),
        false,
    )
}

fn foundry_test_sol(files: &[(String, String)]) -> &str {
    files
        .iter()
        .find(|(p, _)| p == "test/RehearsalToken.t.sol")
        .map(|(_, c)| c.as_str())
        .expect("RehearsalToken.t.sol")
}

fn invariant_sol(files: &[(String, String)]) -> &str {
    files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("Invariant_*.t.sol")
}

#[test]
fn evm_forge_harness_emits_entity_constants_in_test_contract() {
    let files = transpile_project("project.deterministic.yaml");
    let sol = foundry_test_sol(&files);
    assert!(
        sol.contains("private constant INITIAL_SUPPLY ="),
        "entity const must be mirrored in Foundry test contract: {sol}"
    );
    assert!(
        sol.contains("private constant ZERO ="),
        "entity const ZERO must be mirrored in Foundry test contract: {sol}"
    );
}

#[test]
fn evm_forge_harness_det_hoists_factory_deploy_to_setup() {
    let files = transpile_project("project.deterministic.yaml");
    let sol = foundry_test_sol(&files);
    assert!(
        !sol.contains("skipped: `call constructor"),
        "deterministic must not skip constructor: {sol}"
    );
    let setup = sol
        .split("function setUp() public {")
        .nth(1)
        .unwrap()
        .split("function test_")
        .next()
        .unwrap();
    assert!(
        setup.contains("new CambrianFactory()"),
        "setUp must deploy CambrianFactory (BUG-U4 U4-4): {setup}"
    );
    assert!(
        setup.contains("deployRehearsalToken("),
        "setUp must call factory.deployRehearsalToken: {setup}"
    );
    assert!(
        !setup.contains("new RehearsalToken(address(this)"),
        "setUp must not fake-factory via new Entity(address(this)): {setup}"
    );
    assert!(
        !setup.contains(".initialize("),
        "factory.deploy* already initializes; setUp must not call .initialize: {setup}"
    );
}

#[test]
fn evm_forge_harness_invariant_ctor_not_vm_store_total_supply() {
    let files = transpile_project("project.deterministic.yaml");
    let inv = invariant_sol(&files);
    assert!(
        inv.contains("new CambrianFactory()"),
        "Tier A invariant setUp must deploy CambrianFactory (U4-4b): {inv}"
    );
    assert!(
        inv.contains("deployRehearsalToken("),
        "Tier A invariant setUp must call factory.deployRehearsalToken: {inv}"
    );
    assert!(
        !inv.contains(".initialize("),
        "factory.deploy* already initializes; invariant setUp must not call .initialize: {inv}"
    );
    assert!(
        !inv.contains("vm.store(address(_rehearsalToken), bytes32(uint256(0)), bytes32(uint256(1000000000000000000000000)))"),
        "m_total_supply must not be seeded via vm.store when ctor/init covers it: {inv}"
    );
}

/// Governor-shaped 5-arg init route is filled from `init { ... }` via
/// `factory.deploy*`. Unmatched address slots use the handler (not
/// `address(0)`). Handler is constructed before deploy; `cam_wire` runs
/// after leftover / forall seeding (U4-4c).
#[test]
fn evm_forge_harness_invariant_multi_arg_ctor_uses_initialize() {
    let src = r#"
entity Gov {
    routes {
        constructor(token: address, timelock: address, admin: address,
                    period: u64, quorum: U256) => []
        propose() => []
    }
    m_token: address { in constructor(token, _, _, _, _) => token }
    m_timelock: address { in constructor(_, timelock, _, _, _) => timelock }
    m_admin: address { in constructor(_, _, admin, _, _) => admin }
    m_period: u64 { in constructor(_, _, _, period, _) => period }
    m_quorum: U256 { in constructor(_, _, _, _, quorum) => quorum }
}

invariant "quorum is immutable" for Gov {
    init { m_quorum: 1000 }
    action propose() { }
    check m_quorum == 1000
}
"#;
    let program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    let files = generate_evm_tests(&program, true, &InvariantConfig::default());
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("Invariant_*.t.sol");
    assert!(
        inv.contains("new CambrianFactory()"),
        "single-entity deterministic invariant must deploy via factory: {inv}"
    );
    let handler_at = inv
        .find("_handler = new")
        .expect("handler must be constructed");
    let deploy_at = inv
        .find("deployGov(")
        .expect("multi-arg constructor must emit factory.deployGov");
    assert!(
        handler_at < deploy_at,
        "handler must exist before factory.deploy* so init args can use address(_handler):\n{inv}"
    );
    assert!(
        inv.contains("deployGov(address(_handler), address(_handler), address(_handler), 0, 1000)"),
        "unmatched address slots must be the handler, init pin fills quorum: {inv}"
    );
    assert!(
        !inv.contains(".initialize("),
        "factory.deploy* already initializes; setUp must not call .initialize: {inv}"
    );
    assert!(
        inv.contains("cam_wire("),
        "handler-first setUp must wire the SUT after deploy: {inv}"
    );
    assert!(
        !inv.contains("vm.store("),
        "ctor-owned init field must not fall back to vm.store: {inv}"
    );
    assert!(
        !inv.contains("new Gov("),
        "deterministic invariant must not use hybrid new+initialize: {inv}"
    );
}

#[test]
fn evm_forge_harness_invariant_multi_entity_initialize() {
    let src = r#"
entity Gov {
    routes {
        constructor(token: address, timelock: address, admin: address,
                    period: u64, quorum: U256) => []
        propose() => []
    }
    m_token: address { in constructor(token, _, _, _, _) => token }
    m_timelock: address { in constructor(_, timelock, _, _, _) => timelock }
    m_admin: address { in constructor(_, _, admin, _, _) => admin }
    m_period: u64 { in constructor(_, _, _, period, _) => period }
    m_quorum: U256 { in constructor(_, _, _, _, quorum) => quorum }
}
entity Tok {
    routes {
        constructor(holder: address, supply: U256) => []
        transfer(to: address, amount: U256) => []
    }
    m_total_supply: U256 { in constructor(_, supply) => supply }
}

invariant "config and supply" for { g: Gov, tok: Tok } {
    init g { m_quorum: 1000, m_period: 50 }
    init tok { m_total_supply: 1000 }
    action g.propose() {}
    action tok.transfer(to: address, amount: U256) {}
    check g.m_quorum == 1000
    check tok.m_total_supply == 1000
}
"#;
    let program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    let files_det = generate_evm_tests(&program, true, &InvariantConfig::default());
    let inv_det = files_det
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("det Invariant_*.t.sol");
    assert!(
        inv_det.contains("new CambrianFactory()"),
        "det multi-entity invariant must deploy CambrianFactory: {inv_det}"
    );
    let handler_at = inv_det
        .find("_handler = new")
        .expect("handler must be constructed");
    let deploy_g_at = inv_det
        .find("deployGov(")
        .expect("multi-entity Gov must use factory.deployGov");
    assert!(
        handler_at < deploy_g_at,
        "handler must exist before factory.deploy*: {inv_det}"
    );
    assert!(
        inv_det.contains("deployGov(address(_handler), address(_handler), address(_handler), 50, 1000)"),
        "Gov unmatched address ctor slots use handler; pinned period/quorum from init: {inv_det}"
    );
    assert!(
        inv_det.contains("deployTok(address(_handler), 1000)"),
        "Tok deploy must mint to handler with pinned supply: {inv_det}"
    );
    assert!(
        !inv_det.contains(".initialize("),
        "factory.deploy* already initializes; setUp must not call .initialize: {inv_det}"
    );
    assert!(
        inv_det.contains("cam_wire("),
        "multi-entity handler-first setUp must call cam_wire: {inv_det}"
    );
}

#[test]
fn evm_forge_harness_typed_address_ctor_uses_sibling() {
    let src = r#"
entity Tok {
    routes {
        constructor(holder: address, supply: U256) => []
        transfer(to: address, amount: U256) => []
    }
    m_total_supply: U256 { in constructor(_, supply) => supply }
}
entity Gov {
    routes {
        constructor(token: Address<Tok>, admin: address, quorum: U256) => []
        propose() => []
    }
    m_token: Address<Tok> { in constructor(token, _, _) => token }
    m_admin: address { in constructor(_, admin, _) => admin }
    m_quorum: U256 { in constructor(_, _, quorum) => quorum }
}

invariant "config and supply" for { g: Gov, tok: Tok } {
    init g { m_quorum: 1000 }
    init tok { m_total_supply: 1000 }
    action g.propose() {}
    action tok.transfer(to: address, amount: U256) {}
    check g.m_quorum == 1000
    check tok.m_total_supply == 1000
}
"#;
    let program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    let files = generate_evm_tests(&program, true, &InvariantConfig::default());
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("Invariant_*.t.sol");
    assert!(
        inv.contains("deployGov(address(_tok), address(_handler), 1000)"),
        "typed Address<Tok> must come from the sibling instance in factory.deploy*: {inv}"
    );
    assert!(
        inv.contains("deployTok(address(_handler), 1000)"),
        "token holder must be the handler in factory.deploy*: {inv}"
    );
    assert!(
        !inv.contains(".initialize("),
        "factory.deploy* already initializes; setUp must not call .initialize: {inv}"
    );
}

/// Vault shape: `identity m_asset: Address<VaultAsset>` is pinned at a
/// sibling instance (`init vlt { m_asset: tok }`). Type-based deploy
/// order cannot see this edge — the identity is typed against the
/// `extern` interface, the instance is a concrete peer — so the pin
/// must both render `address(_tok)` and force `tok` to deploy first.
#[test]
fn evm_forge_harness_identity_pin_uses_sibling_and_deploys_it_first() {
    let src = r#"
extern entity VaultAsset {
    view route balanceOf(who: address) -> U256;
}

entity Tok {
    routes {
        constructor() => []
        view balanceOf(who: address) -> U256 => [ return(0) ]
    }
}

entity Vlt {
    routes {
        constructor() => []
        poke() => [
            var b = balanceOf(sys::address) ~> m_asset;
        ]
    }
    identity m_asset: Address<VaultAsset>
}

invariant "count only grows" for { vlt: Vlt, tok: Tok } {
    init vlt { m_asset: tok }
    action vlt.poke() {}
    check true
}
"#;
    let program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    let files = generate_evm_tests(&program, true, &InvariantConfig::default());
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("Invariant_*.t.sol");

    assert!(
        inv.contains("address(_tok)") && inv.contains("deployVlt(") && !inv.contains(", tok)"),
        "identity pin must render the sibling address in factory.deploy*, not the bare name:\n{inv}"
    );
    assert!(
        !inv.contains("new Vlt("),
        "multi-entity deterministic invariant must not use hybrid new Vlt:\n{inv}"
    );

    let tok_at = inv
        .find("deployTok(")
        .expect("Tok must be deployed via factory");
    let vlt_at = inv
        .find("deployVlt(")
        .expect("Vlt must be deployed via factory");
    assert!(
        tok_at < vlt_at,
        "tok must deploy before vlt even though vlt is declared first:\n{inv}"
    );
}

#[test]
fn evm_forge_harness_named_init_route_remaps_to_initialize() {
    let src = r#"
entity Vault {
    routes {
        init create(x: u64) => []
        ping() => []
    }
    m_x: u64 { in create(x) => x }
}

test "constructs" for Vault {
    call create(7)
    expect state { m_x: 7 }
}
"#;
    let program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    let files = generate_evm_tests_for_project(
        &program,
        true,
        &InvariantConfig::default(),
        Some("project"),
        true,
    );
    let sol = files
        .iter()
        .find(|(p, _)| p.ends_with("Vault.t.sol"))
        .map(|(_, c)| c.as_str())
        .expect("Vault.t.sol");
    assert!(
        sol.contains("deployVault(7)") || sol.contains(".deployVault(7)"),
        "named init create(...) must redeploy via factory.deployVault: {sol}"
    );
}

/// The Governor example that failed CI with solc 6160 must not pad
/// unmatched ctor slots with `address(0)`. Typed `Address<E>` without a
/// sibling instance (single-entity invariants) uses the handler.
#[test]
fn evm_forge_harness_governor_example_initialize_not_zero() {
    let yaml = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples/governor/project.yaml");
    let project = Project::load(&yaml).expect("load governor example");
    let files = generate_evm_tests(
        &project.merged,
        project.config.resolved_deterministic_addresses(),
        &project.config.resolved_invariant(),
    );
    let invs: Vec<&(String, String)> = files
        .iter()
        .filter(|(p, _)| p.starts_with("test/Invariant_"))
        .collect();
    assert!(
        !invs.is_empty(),
        "governor project must emit invariant harnesses: {:?}",
        files.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
    for (path, sol) in &invs {
        assert!(
            !sol.contains("initialize(address(0)") && !sol.contains("initialize(address(0x0"),
            "{path} still pads initialize with the zero address:\n{sol}"
        );
        if sol.contains("deployGovernor(") || sol.contains(".deployGovernor(") {
            assert!(
                sol.contains("address(_handler)"),
                "{path} factory.deploy* must fill unmatched slots with the handler:\n{sol}"
            );
            assert!(
                !sol.contains(".initialize("),
                "{path} factory.deploy* must not be followed by .initialize:\n{sol}"
            );
            if let (Some(handler_at), Some(deploy_at)) =
                (sol.find("_handler = new"), sol.find("deployGovernor("))
            {
                assert!(
                    handler_at < deploy_at,
                    "{path} handler must exist before factory.deploy*:\n{sol}"
                );
            }
            assert!(
                sol.contains("cam_wire("),
                "{path} handler-first setUp must call cam_wire:\n{sol}"
            );
        } else if let (Some(handler_at), Some(init_at)) =
            (sol.find("_handler = new"), sol.find(".initialize("))
        {
            assert!(
                init_at > handler_at,
                "{path} multi-entity hybrid must initialize after the handler exists:\n{sol}"
            );
        }
    }
    let gov = invs
        .iter()
        .find(|(p, _)| p.contains("Governor") || p.contains("quorum"))
        .map(|(_, c)| c.as_str())
        .unwrap_or_else(|| {
            panic!(
                "Governor invariant missing: {:?}",
                invs.iter().map(|(p, _)| p).collect::<Vec<_>>()
            )
        });
    let gov_init_ok = gov.contains(
        "deployGovernor(address(_handler), address(_handler), address(_handler), 0, 1000)",
    ) || gov.contains(
        "initialize(address(_handler), address(_handler), address(_handler), 0, 1000)",
    );
    assert!(
        gov_init_ok,
        "Governor unmatched Address<E> / admin slots must be the handler, quorum from init:\n{gov}"
    );
}

#[test]
fn evm_forge_harness_init_boot_keeps_expect_state() {
    let src = r#"
entity Ledger {
    routes { init boot() => [] }
    m_scores: HashMap<u64, u64> {
        in boot() => m_scores
    }
}

test "after boot" for Ledger {
    call boot()
    expect state { m_scores[7]: 99 }
}
"#;
    let program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    let files = generate_evm_tests(&program, true, &InvariantConfig::default());
    let test_sol = files
        .iter()
        .find(|(p, _)| p.ends_with(".t.sol") && !p.contains("Invariant"))
        .map(|(_, c)| c.as_str())
        .unwrap_or_else(|| {
            panic!(
                "Foundry unit test missing: {:?}",
                files.iter().map(|(p, _)| p).collect::<Vec<_>>()
            )
        });
    assert!(
        test_sol.contains("m_scores") && (test_sol.contains("assertEq") || test_sol.contains("[7]")),
        "skipping empty init-route call must still emit expect state:\n{test_sol}"
    );
}

fn evm_diag_codes(src: &str) -> Vec<String> {
    let mut program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    cambrian_transpiler::desugar::desugar_properties(&mut program);
    let mut diags = cambrian_transpiler::validate::validate(&program);
    diags.extend(check_target_compat(&program, Target::Evm, true));
    diags.into_iter().map(|d| d.code.to_string()).collect()
}

/// U4-4c Step 1c — factory deploy emits `predict*` assert after each instance.
#[test]
fn evm_forge_harness_multi_entity_predict_assert() {
    let src = r#"
entity Tok {
    routes { constructor(holder: address, supply: U256) => [] transfer(to: address, amount: U256) => [] }
    m_total_supply: U256 { in constructor(_, supply) => supply }
}
entity Gov {
    routes {
        constructor(token: address, timelock: address, admin: address, period: u64, quorum: U256) => []
        propose() => []
    }
    m_token: address { in constructor(token, _, _, _, _) => token }
    m_quorum: U256 { in constructor(_, _, _, _, quorum) => quorum }
}

invariant "predict assert" for { g: Gov, tok: Tok } {
    init g { m_quorum: 1 }
    init tok { m_total_supply: 1 }
    action g.propose() {}
    action tok.transfer(to: address, amount: U256) {}
    check g.m_quorum == 1
}
"#;
    let program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    let files = generate_evm_tests(&program, true, &InvariantConfig::default());
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("Invariant_*.t.sol");
    assert!(
        inv.contains(".predictTok()") && inv.contains(".predictGov()"),
        "each factory.deploy* must be followed by predict assert: {inv}"
    );
}

/// U4-4c T3 — duplicate resolved identity tuple on same entity.
#[test]
fn validator_duplicate_identity_errors() {
    let src = r#"
entity Vault {
    routes { deposit(amount: u64) => [] }
    identity m_vault_id: u64
    m_balance: u64 { in deposit(amount) => m_balance + amount }
}

invariant "dup identity" for { v: Vault, a: Vault } {
    init v { m_vault_id: 1, m_balance: 0 }
    init a { m_vault_id: 1, m_balance: 0 }
    action v.deposit(amount: u64) { bound amount in 0..10 }
    check v.m_balance >= 0
}
"#;
    let codes = evm_diag_codes(src);
    assert!(
        codes.iter().filter(|c| *c == "I18").count() >= 1,
        "duplicate identity tuple must emit I18: {codes:?}"
    );
}

/// U4-4c T5 — cyclic sibling deploy-order graph → I18.
#[test]
fn deploy_order_cycle_errors() {
    let src = r#"
entity A {
    routes { constructor(peer: Address<B>) => [] }
    identity m_id: u64
    m_peer: Address<B> { in constructor(peer) => peer }
}
entity B {
    routes { constructor(peer: Address<A>) => [] }
    identity m_id: u64
    m_peer: Address<A> { in constructor(peer) => peer }
}

invariant "cycle" for { a: A, b: B } {
    init a { m_id: 1, m_peer: b }
    init b { m_id: 1, m_peer: a }
    action a.constructor(peer: address) { }
    check true
}
"#;
    let codes = evm_diag_codes(src);
    assert!(
        codes.iter().any(|c| c == "I18"),
        "cyclic instance deploy-order must emit I18: {codes:?}"
    );
}

/// U4-4c T4 — two instances of an entity with no identity members.
#[test]
fn validator_identity_less_duplicate_errors() {
    let src = r#"
entity Vault {
    routes { deposit(amount: u64) => [] }
    m_balance: u64 { in deposit(amount) => m_balance + amount }
}

invariant "no identity" for { v: Vault, a: Vault } {
    init v { m_balance: 0 }
    init a { m_balance: 0 }
    action v.deposit(amount: u64) { bound amount in 0..10 }
    check v.m_balance >= 0
}
"#;
    let codes = evm_diag_codes(src);
    assert!(
        codes.iter().any(|c| c == "I18"),
        "identity-less duplicate entity instances must emit I18: {codes:?}"
    );
}

/// PW3-G-012 / U4-4c: unary address ctor must use handler, not `m_marked: 0` pin.
#[test]
fn evm_forge_harness_phased_value_unary_address_uses_handler() {
    let yaml = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/audit/fixtures/pw3_invariant/phased_value_trace_evm.yaml");
    let project = Project::load(&yaml).expect("load phased value pw3 fixture");
    let files = generate_evm_tests_for_project(
        &project.merged,
        true,
        &project.config.resolved_invariant(),
        Some(project.name().as_str()),
        false,
    );
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, s)| s.as_str())
        .expect("Invariant_*.t.sol");
    assert!(
        inv.contains("deployValuePhase(address(_handler))")
            || inv.contains(".deployValuePhase(address(_handler))"),
        "unmatched unary address ctor must use handler, not init pin literal 0:\n{inv}"
    );
    assert!(
        !inv.contains("deployValuePhase(0)"),
        "must not pass m_marked init pin as recipient address:\n{inv}"
    );
}

/// U4-4c Step 3 — grep gate §5.7: invariant setUp uses factory deploy only.
#[test]
fn evm_forge_harness_invariant_setup_no_initialize_or_new_sut() {
    let yaml = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples/governor/project.yaml");
    let project = Project::load(&yaml).expect("load governor example");
    let files = generate_evm_tests(
        &project.merged,
        project.config.resolved_deterministic_addresses(),
        &project.config.resolved_invariant(),
    );
    let invs: Vec<&(String, String)> = files
        .iter()
        .filter(|(p, _)| p.starts_with("test/Invariant_"))
        .collect();
    assert!(!invs.is_empty(), "governor must emit invariant harnesses");
    for (path, sol) in &invs {
        let setup = sol
            .split("function setUp() public {")
            .nth(1)
            .unwrap_or_else(|| panic!("missing setUp in {path}"));
        let setup = setup.split("targetContract").next().unwrap_or(setup);
        assert!(
            !setup.contains(".initialize("),
            "invariant setUp must not call .initialize on SUT ({path}): {setup}"
        );
        assert!(
            !setup.contains("new Governor(") && !setup.contains("new Tok("),
            "invariant setUp must not `new` SUT entities ({path}): {setup}"
        );
        assert!(
            setup.contains("new CambrianFactory()"),
            "invariant setUp must deploy factory ({path})"
        );
        assert!(
            setup.contains("deploy"),
            "invariant setUp must factory.deploy* SUT ({path})"
        );
    }
}
