// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! A `let` bound in one phase and read in a later one.
//!
//! A phase body lowers to a Solidity `{ }` block, so a `let` declared in one
//! phase used to fall out of scope at the phase boundary — while the validator
//! accepted the forward reference, because a route body is one scope at the
//! Cambrian level. The result was Solidity that `solc` rejected with
//! `Undeclared identifier`, and it made the "snapshot the pre-state into a
//! local" idiom unusable in exactly the routes that need it: a route that
//! mutates a member and must answer a value derived from the member's
//! pre-state has nowhere else to put the snapshot (member transforms cannot
//! see route-body `let`s, and `var` only binds a cross-contract call).
//!
//! Bindings read only inside their own phase keep their block scope.

use cambrian_transpiler::ProgramParser;
use cambrian_transpiler::codegen::evm::gen_evm_solidity;

fn transpile(src: &str) -> String {
    let mut program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("Parse error: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    gen_evm_solidity(&program, true)
}

fn route_body<'a>(sol: &'a str, signature: &str) -> &'a str {
    let start = sol
        .find(signature)
        .unwrap_or_else(|| panic!("no `{signature}` in:\n{sol}"));
    let rest = &sol[start..];
    let end = rest.find("\n    }\n").unwrap_or(rest.len());
    &rest[..end]
}

const VAULT: &str = r#"
extern entity Asset {
    view route balanceOf(owner: address) -> U256;
    route transferFrom(from_addr: address, to: address, amount: U256) -> bool;
}

entity Vault {
    identity m_asset: Address<Asset>

    routes {
        constructor() => []

        deposit(assets: U256) -> U256 => [
            read: [
                var bal0 = balanceOf(sys::address) ~> m_asset;
                let supply0 = m_total_supply;
            ]
            pull: [
                transferFrom(msg::sender, sys::address, assets) ~> m_asset
            ]
            issue: [
                return(supply0 + assets)
            ]
        ]
    }

    m_total_supply: U256 {
        in constructor() => 0
        in deposit(assets) => issue: { m_total_supply + assets }
    }
}
"#;

#[test]
fn a_let_read_by_a_later_phase_is_declared_at_function_scope() {
    let sol = transpile(VAULT);
    let body = route_body(&sol, "function deposit(");

    // Declared once, up top — outside every phase block.
    let decl = body.find("uint256 supply0;").expect("hoisted declaration");
    let first_phase = body.find("// Phase: read").expect("first phase");
    assert!(
        decl < first_phase,
        "declaration must precede the first phase block:\n{body}"
    );

    // Assigned, not re-declared, at the binding site.
    assert!(
        body.contains("supply0 = m_total_supply;"),
        "expected a plain assignment in the binding phase:\n{body}"
    );
    assert_eq!(
        body.matches("uint256 supply0").count(),
        1,
        "the binding must not be declared twice:\n{body}"
    );
}

#[test]
fn the_later_phase_reads_the_value_the_earlier_phase_bound() {
    let sol = transpile(VAULT);
    let body = route_body(&sol, "function deposit(");

    // The commit to m_total_supply happens in the same phase as the return,
    // so reading the member directly would report the post-commit value —
    // the whole reason the route snapshots it in an earlier phase.
    assert!(
        body.contains("return (supply0 + assets);")
            || body.contains("return supply0 + assets;")
            || body.contains("return (supply0 + assets)"),
        "the return must read the snapshot, not the member:\n{body}"
    );
}

#[test]
fn a_let_confined_to_one_phase_keeps_its_block_scope() {
    let sol = transpile(
        r#"
entity Counter {
    routes {
        constructor() => []

        bump(n: U256) -> U256 => [
            compute: [
                let doubled = n * 2;
                return(doubled)
            ]
        ]
    }

    m_count: U256 {
        in constructor() => 0
        in bump(n) => compute: { m_count + n }
    }
}
"#,
    );
    let body = route_body(&sol, "function bump(");

    assert!(
        body.contains("uint256 doubled = "),
        "a phase-local binding must still be declared where it is bound:\n{body}"
    );
    assert!(
        !body.contains("uint256 doubled;"),
        "a phase-local binding must not be hoisted:\n{body}"
    );
}

#[test]
fn a_let_read_by_the_trailing_actions_of_a_mixed_body_is_hoisted() {
    let sol = transpile(
        r#"
entity Mixed {
    routes {
        constructor() => []

        run(n: U256) -> U256 => [
            head: [
                let seed = n + 1;
            ]
            return(seed * 2)
        ]
    }

    m_seen: U256 {
        in constructor() => 0
        in run(n) => { m_seen + n }
    }
}
"#,
    );
    let body = route_body(&sol, "function run(");

    assert!(
        body.contains("uint256 seed;"),
        "a binding read after the phases must be hoisted:\n{body}"
    );
    assert!(
        body.contains("seed = "),
        "expected an assignment at the binding site:\n{body}"
    );
}
