// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! W10 — mutating ledger routes should `emit` when the entity declares events (SD-03).

use cambrian_transpiler::ast;
use cambrian_transpiler::target::Target;
use cambrian_transpiler::validate::{check_target_compat, validate};
use cambrian_transpiler::ProgramParser;

fn parse_program(source: &str) -> ast::Program {
    let mut p = ProgramParser::new().parse(source).expect("parse");
    ast::normalize_program_types(&mut p);
    p
}

fn evm_diags(program: &ast::Program) -> Vec<cambrian_transpiler::validate::Diagnostic> {
    check_target_compat(program, Target::Evm, true)
}

#[test]
fn w10_warns_transfer_without_emit() {
    let src = r#"
entity BadToken {
    event Transfer(indexed src: address, indexed dst: address, value: U256);

    routes {
        constructor(initial: U256) => []
        transfer(to: address, amount: U256) => []
    }

    m_balances: HashMap<address, U256> {
        in constructor(initial) => {
            {}.insert(msg::sender, initial)
        }
        in transfer(to, amount) => {
            let sender = msg::sender;
            m_balances.update(sender, 0).update(to, amount)
        }
    }
}
"#;
    let program = parse_program(src);
    let diags = evm_diags(&program);
    assert!(
        diags.iter().any(|d| d.code == "W10" && d.message.contains("transfer")),
        "expected W10 on transfer, got: {diags:?}"
    );
}

#[test]
fn w10_silent_when_emit_present() {
    let src = r#"
entity GoodToken {
    event Transfer(indexed src: address, indexed dst: address, value: U256);

    routes {
        transfer(to: address, amount: U256) => [
            emit Transfer(msg::sender, to, amount);
        ]
    }

    m_balances: HashMap<address, U256> {
        in transfer(to, amount) => m_balances
    }
}
"#;
    let program = parse_program(src);
    let diags = evm_diags(&program);
    assert!(
        !diags.iter().any(|d| d.code == "W10"),
        "unexpected W10: {diags:?}"
    );
}

#[test]
fn w10_not_in_targetless_validate() {
    let src = r#"
entity BadToken {
    event Transfer(indexed src: address, indexed dst: address, value: U256);
    routes {
        transfer(to: address, amount: U256) => []
    }
    m_balances: HashMap<address, U256> {
        in transfer(to, amount) => m_balances
    }
}
"#;
    let program = parse_program(src);
    let diags = validate(&program);
    assert!(
        !diags.iter().any(|d| d.code == "W10"),
        "W10 is EVM-domain only; target-less validate must not emit it: {diags:?}"
    );
}

#[test]
fn w10_rehearsal_token_events_reference_has_no_false_positives() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../contracts/rehearsal_token_events_evm.cam");
    let src = std::fs::read_to_string(&path).expect("read rehearsal_token_events_evm.cam");
    let program = parse_program(&src);
    let diags = evm_diags(&program);
    let w10 = diags.iter().filter(|d| d.code == "W10").collect::<Vec<_>>();
    assert!(
        w10.is_empty(),
        "SD-03 reference corpus must not trigger W10: {w10:?}"
    );
}

#[test]
fn w10_erc20_events_reference_has_no_false_positives() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../contracts/erc20_events_evm.cam");
    let src = std::fs::read_to_string(&path).expect("read erc20_events_evm.cam");
    let program = parse_program(&src);
    let diags = evm_diags(&program);
    assert!(
        !diags.iter().any(|d| d.code == "W10"),
        "erc20_events reference must not trigger W10: {diags:?}"
    );
}

#[test]
fn w10_contracts_with_events_noise_below_threshold() {
    let contracts = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../contracts");
    let mut total = 0usize;
    for entry in std::fs::read_dir(&contracts).expect("read contracts/").flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("cam") {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap_or_default();
        if !src.contains("event ") {
            continue;
        }
        let program = parse_program(&src);
        total += evm_diags(&program)
            .iter()
            .filter(|d| d.code == "W10")
            .count();
    }
    assert!(
        total <= 5,
        "W10 noise on contracts/ with events should stay low (got {total}); narrow heuristic if needed"
    );
}
