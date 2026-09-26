// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! TYPED-LIT-1 — Foundry harness: numeric default `let`, call-site address coercion.

use cambrian_transpiler::ast::Program;
#[cfg(feature = "revm")]
use cambrian_transpiler::codegen::evm_revm_test_codegen::generate_revm_tests;
use cambrian_transpiler::codegen::evm_test_codegen::generate_evm_tests;
use cambrian_transpiler::project::InvariantConfig;
#[cfg(feature = "revm")]
use cambrian_transpiler::project::{FuzzConfig, RevmTestConfig};
use cambrian_transpiler::ProgramParser;

fn program_with_test(test_body: &str) -> Program {
    let src = format!(
        r#"
        entity Token {{
            routes {{
                transfer(to: address, amount: u64) => []
            }}
            m_balance: u64 {{ in transfer(to, amount) => m_balance }}
        }}

        test "harness" for Token {{
            {body}
        }}
    "#,
        body = test_body
    );
    let mut program = ProgramParser::new()
        .parse(&src)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    program
}

fn harness_sol(program: &Program) -> String {
    let files = generate_evm_tests(program, true, &InvariantConfig::default());
    let paths = files.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>();
    files
        .into_iter()
        .find(|(path, _)| path.contains("Test") || path.ends_with(".t.sol"))
        .map(|(_, s)| s)
        .unwrap_or_else(|| panic!("no test sol in {:?}", paths))
}

#[test]
fn typed_lit_harness_let_wei_amount_is_uint256() {
    let program = program_with_test(
        r#"
            let amount = 1000000000000000000
            call transfer(address(1), amount)
        "#,
    );
    let sol = harness_sol(&program);
    assert!(
        !sol.contains("address amount"),
        "harness must not declare implicit address let: {sol}"
    );
    assert!(
        sol.contains("uint256 amount")
            || sol.contains("uint128 amount")
            || sol.contains("uint64 amount"),
        "harness let must be numeric: {sol}"
    );
}

#[test]
fn typed_lit_harness_call_coerces_numeric_let_to_address_param() {
    let program = program_with_test(
        r#"
            let to = 0x0000000000000000000000000000000000000000000000000000000000000d01
            call transfer(to, 1)
        "#,
    );
    let sol = harness_sol(&program);
    assert!(
        sol.contains("address to = address(uint160(") && sol.contains(".transfer(to, 1)"),
        "a let used only as an address param is declared `address` (#57): {sol}"
    );
}

#[test]
fn typed_lit_harness_det_init_and_prank_coerce_numeric_deployer() {
    let src = r#"
        entity Token {
            routes {
                constructor(holder: address) => []
            }
            m_n: u64 { in constructor(_) => m_n }
        }
        test "init" for Token {
            let deployer = 0x0000000000000000000000000000000000000000000000000000000000000d01
            msg { sender: deployer }
            call constructor(deployer)
        }
    "#;
    let mut program = ProgramParser::new().parse(src).unwrap();
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    let files = generate_evm_tests(&program, true, &InvariantConfig::default());
    let sol = files
        .into_iter()
        .find(|(path, _)| path.ends_with(".t.sol"))
        .map(|(_, s)| s)
        .expect("test sol");
    assert!(
        sol.contains("deployToken(address(uint160(uint256(deployer)))")
            || sol.contains("deployToken(address(uint160")
            || sol.contains("initialize(address(uint160(uint256(deployer)))")
            || sol.contains("initialize(address(uint160"),
        "factory deploy / init must coerce deployer to address: {sol}"
    );
    assert!(
        sol.contains("startPrank(address(uint160(uint256(deployer)))")
            || sol.contains("startPrank(address(uint160"),
        "msg sender prank must coerce numeric deployer: {sol}"
    );
}

#[test]
fn typed_lit_harness_expect_return_coerces_numeric_let_to_address() {
    let src = r#"
        entity Token {
            routes {
                constructor(holder: address) => []
                owner() -> address => []
            }
            m_n: u64 { in constructor(_) => m_n in owner() => m_n }
        }
        test "owner" for Token {
            let deployer = 0x0000000000000000000000000000000000000000000000000000000000000d01
            msg { sender: deployer }
            call constructor(deployer)
            call owner()
            expect return deployer
        }
    "#;
    let mut program = ProgramParser::new().parse(src).unwrap();
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    let files = generate_evm_tests(&program, true, &InvariantConfig::default());
    let sol = files
        .into_iter()
        .find(|(path, _)| path.ends_with(".t.sol"))
        .map(|(_, s)| s)
        .expect("test sol");
    assert!(
        sol.contains("address deployer = address(uint160(") && sol.contains("assertEq(_ret_1, deployer"),
        "a let used as sender / address arg is declared `address` (#57): {sol}"
    );
}

#[test]
fn typed_lit_harness_entity_const_u256_not_address() {
    let src = r#"
        entity T {
            const ONE: U256 = 1000000000000000000
            routes { go() => [] }
            m_n: U256 { in go() => m_n }
        }
        test "c" for T {
            call go()
        }
    "#;
    let mut program = ProgramParser::new().parse(src).unwrap();
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    let sol = harness_sol(&program);
    assert!(
        !sol.contains("address(uint160(uint256(1000000000000000000)))"),
        "entity const U256 must not use address wrap: {sol}"
    );
}

#[cfg(feature = "revm")]
#[test]
fn typed_lit_revm_harness_typed_address_let() {
    let src = r#"
        entity Token {
            routes { transfer(to: address, amount: u64) => [] }
            m_balance: u64 { in transfer(to, amount) => m_balance }
        }
        test "revm typed let" for Token {
            let deployer: address = 0x0000000000000000000000000000000000000000000000000000000000000d01
            call transfer(deployer, 1)
        }
    "#;
    let mut program = ProgramParser::new().parse(src).unwrap();
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    let cfg = RevmTestConfig {
        enabled: true,
        ..Default::default()
    };
    let files = generate_revm_tests(
        &program,
        "typed-lit",
        false,
        &cfg,
        &FuzzConfig::default(),
        &InvariantConfig::default(),
    );
    let rust = files
        .into_iter()
        .find(|(p, _)| p.starts_with("revm-tests/tests/"))
        .map(|(_, c)| c)
        .expect("revm test module");
    assert!(
        rust.contains("let deployer: Address ="),
        "revm harness must honor typed address let: {rust}"
    );
}
