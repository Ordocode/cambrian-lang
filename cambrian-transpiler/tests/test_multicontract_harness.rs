// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Multi-contract test bodies (G-U9).
//!
//! Before `deploy <b> = <Entity>(...)`, a generated harness held exactly one
//! instance and seeded its identity members with the type default. Every
//! route that reaches a second contract — the whole of `UniswapV2Pair.mint`,
//! every ERC-4626 deposit — was therefore unreachable, and the suite stayed
//! green by never running those paths. These tests pin the shape of the
//! generated Solidity so that regression is loud rather than silent.

use cambrian_transpiler::ProgramParser;
use cambrian_transpiler::ast::TestStep;
use cambrian_transpiler::project::InvariantConfig;
use cambrian_transpiler::validate;

fn parse(src: &str) -> cambrian_transpiler::ast::Program {
    let mut program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("Parse error: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    program
}

fn gen_tests(src: &str, deterministic: bool) -> String {
    let program = parse(src);
    let cfg = InvariantConfig::default();
    cambrian_transpiler::codegen::evm_test_codegen::generate_evm_tests(&program, deterministic, &cfg)
        .into_iter()
        .map(|(_, body)| body)
        .collect::<Vec<_>>()
        .join("\n")
}

fn diag_codes(src: &str) -> Vec<String> {
    validate::validate(&parse(src))
        .into_iter()
        .map(|d| d.code.to_string())
        .collect()
}

/// A vault holding an `identity` reference to a token, plus the token —
/// the smallest shape that reproduces the ERC-4626 / UniswapV2Pair problem.
const VAULT_AND_TOKEN: &str = r#"
entity Token {
    routes {
        constructor(holder: address, supply: u64) => []
        transfer(to: address, amount: u64) => []
        view balanceOf(who: address) -> u64 => [ return(m_balances[who]) ]
    }
    identity m_token_id: u8
    m_balances: HashMap<address, u64> {
        in constructor(holder, supply) => { {}.insert(holder, supply) }
        in transfer(to, amount) => {
            let bal_from = m_balances[msg::sender];
            let bal_to   = m_balances[to];
            if to == msg::sender { m_balances } else {
                m_balances
                    .update(msg::sender, bal_from - amount)
                    .update(to,          bal_to   + amount)
            }
        }
    }
}

entity Vault {
    routes {
        constructor() => []
        view asset() -> address => [ return(m_asset) ]
        note(amount: u64) => []
    }
    identity m_asset: address
    m_noted: u64 {
        in note(amount) => amount
    }
}
"#;

#[test]
fn deploy_step_parses_with_and_without_a_seed_clause() {
    let program = parse(&format!(
        r#"{VAULT_AND_TOKEN}
test "peers" for Vault {{
    deploy t = Token(0x1, 100) with {{ m_token_id: 7 }}
    deploy v = Vault() with {{ m_asset: t }}
    call v.note(3)
}}
"#
    ));
    let t = &program.tests[0];
    assert!(
        matches!(&t.body[0], TestStep::DeployPeer { binding, entity, args, init_state }
            if binding == "t" && entity == "Token" && args.len() == 2 && init_state.len() == 1),
        "unexpected first step: {:?}",
        t.body[0]
    );
    assert!(
        matches!(&t.body[2], TestStep::Call { target: Some(b), route, .. }
            if b == "v" && route == "note"),
        "qualified call did not carry its target: {:?}",
        t.body[2]
    );
}

#[test]
fn an_unqualified_call_still_means_the_setup_instance() {
    // The single-contract form is what the whole corpus is written in;
    // parsing it must not change shape.
    let program = parse(&format!(
        r#"{VAULT_AND_TOKEN}
test "plain" for Vault {{
    call note(3)
}}
"#
    ));
    assert!(
        matches!(&program.tests[0].body[0], TestStep::Call { target: None, .. }),
        "plain call grew a target"
    );
}

#[test]
fn identity_seeds_become_constructor_arguments() {
    let sol = gen_tests(
        &format!(
            r#"{VAULT_AND_TOKEN}
test "wired" for Vault {{
    deploy t = Token(0x1, 100) with {{ m_token_id: 7 }}
    deploy v = Vault() with {{ m_asset: t }}
    call v.asset()
    expect return t
}}
"#
        ),
        true,
    );

    // Identity seeds and init-route args must reach factory.deploy* (no setter).
    assert!(
        sol.contains("deployToken(7") && sol.contains("100"),
        "identity seed and init-route args must reach factory.deployToken: {sol}"
    );
    assert!(
        sol.contains("deployVault(t)") || sol.contains("deployVault( t)"),
        "peer address must reach factory.deployVault: {sol}"
    );
}

#[test]
fn deploying_the_entity_under_test_rebinds_unqualified_steps() {
    // Otherwise `expect state` would read the default-seeded setUp()
    // instance and pass for the wrong reason.
    let sol = gen_tests(
        &format!(
            r#"{VAULT_AND_TOKEN}
test "rebind" for Vault {{
    deploy t = Token(0x1, 100) with {{ m_token_id: 7 }}
    deploy v = Vault() with {{ m_asset: t }}
    call note(3)
    expect state {{ m_noted: 3 }}
}}
"#
        ),
        true,
    );
    assert!(
        sol.contains("_peer_v.note(3)"),
        "unqualified call did not follow the deployed subject: {sol}"
    );
    assert!(
        sol.contains("_peer_v.m_noted()"),
        "expect state did not follow the deployed subject: {sol}"
    );
}

#[test]
fn a_peer_entity_gets_imported() {
    let sol = gen_tests(
        &format!(
            r#"{VAULT_AND_TOKEN}
test "import" for Vault {{
    deploy t = Token(0x1, 100)
    call t.transfer(0x2, 1)
}}
"#
        ),
        true,
    );
    assert!(
        sol.contains("import \"../src/Token.sol\";"),
        "peer entity was not imported: {sol}"
    );
}

#[test]
fn an_unreferenced_peer_address_is_not_declared() {
    // Generated code carrying compiler warnings teaches readers to ignore
    // compiler warnings.
    let sol = gen_tests(
        &format!(
            r#"{VAULT_AND_TOKEN}
test "unused" for Vault {{
    deploy t = Token(0x1, 100)
    call t.transfer(0x2, 1)
}}
"#
        ),
        true,
    );
    assert!(
        !sol.contains("address t = address(_peer_t);"),
        "declared an unused local: {sol}"
    );

    let used = gen_tests(
        &format!(
            r#"{VAULT_AND_TOKEN}
test "used" for Vault {{
    deploy t = Token(0x1, 100)
    deploy v = Vault() with {{ m_asset: t }}
    call v.note(1)
}}
"#
        ),
        true,
    );
    assert!(
        used.contains("address t = address(_peer_t);"),
        "dropped a local that is referenced: {used}"
    );
}

#[test]
fn the_validator_rejects_an_unknown_peer_entity() {
    let codes = diag_codes(&format!(
        r#"{VAULT_AND_TOKEN}
test "bad" for Vault {{
    deploy t = Nonexistent(1)
    call t.transfer(0x2, 1)
}}
"#
    ));
    assert!(codes.contains(&"T24".to_string()), "expected T24, got {codes:?}");
}

#[test]
fn the_validator_rejects_a_rebound_name() {
    let codes = diag_codes(&format!(
        r#"{VAULT_AND_TOKEN}
test "bad" for Vault {{
    deploy t = Token(0x1, 100)
    deploy t = Token(0x2, 100)
    call t.transfer(0x2, 1)
}}
"#
    ));
    assert!(codes.contains(&"T25".to_string()), "expected T25, got {codes:?}");
}

#[test]
fn the_validator_checks_constructor_arity_and_seed_names() {
    let codes = diag_codes(&format!(
        r#"{VAULT_AND_TOKEN}
test "bad" for Vault {{
    deploy t = Token(0x1) with {{ m_nope: 1 }}
    call t.transfer(0x2, 1)
}}
"#
    ));
    assert!(codes.contains(&"T26".to_string()), "expected T26, got {codes:?}");
    assert!(codes.contains(&"T27".to_string()), "expected T27, got {codes:?}");
}

#[test]
fn the_validator_rejects_a_call_on_an_unbound_name() {
    let codes = diag_codes(&format!(
        r#"{VAULT_AND_TOKEN}
test "bad" for Vault {{
    call ghost.transfer(0x2, 1)
}}
"#
    ));
    assert!(codes.contains(&"T28".to_string()), "expected T28, got {codes:?}");
}

#[test]
fn the_validator_checks_the_route_on_the_peer_not_on_the_subject() {
    // `note` exists on Vault, not on Token: resolving the qualified call
    // against the subject would let this through.
    let codes = diag_codes(&format!(
        r#"{VAULT_AND_TOKEN}
test "bad" for Vault {{
    deploy t = Token(0x1, 100)
    call t.note(1)
}}
"#
    ));
    assert!(codes.contains(&"T29".to_string()), "expected T29, got {codes:?}");

    let arity = diag_codes(&format!(
        r#"{VAULT_AND_TOKEN}
test "bad" for Vault {{
    deploy t = Token(0x1, 100)
    call t.transfer(0x2)
}}
"#
    ));
    assert!(arity.contains(&"T29".to_string()), "expected T29 for arity, got {arity:?}");
}

#[test]
fn a_peer_call_satisfies_the_expect_ordering_check() {
    // T5 fires on `expect` before any `call`; a qualified call is a call.
    let codes = diag_codes(&format!(
        r#"{VAULT_AND_TOKEN}
test "ordering" for Vault {{
    deploy t = Token(0x1, 100)
    call t.balanceOf(0x1)
    expect return 100
}}
"#
    ));
    assert!(!codes.contains(&"T5".to_string()), "peer call was not counted as a call: {codes:?}");
}
