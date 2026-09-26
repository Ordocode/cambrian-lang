// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! TYPED-LIT-3 — typed `let x: T =` in spec bodies + T25 validation.

use cambrian_transpiler::ast::{Program, TestStep, Type};
#[cfg(feature = "revm")]
use cambrian_transpiler::codegen::evm_revm_test_codegen::generate_revm_tests;
use cambrian_transpiler::codegen::evm_test_codegen::generate_evm_tests;
#[cfg(feature = "rust-targets")]
use cambrian_transpiler::codegen::test_codegen::generate_tests;
use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
#[cfg(any(feature = "revm", feature = "rust-targets"))]
use cambrian_transpiler::project::FuzzConfig;
use cambrian_transpiler::project::InvariantConfig;
#[cfg(feature = "revm")]
use cambrian_transpiler::project::RevmTestConfig;
use cambrian_transpiler::ProgramParser;

fn parse(src: &str) -> Program {
    let mut program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    program
}

#[test]
fn typed_lit_parse_spec_let_with_type() {
    let program = parse(
        r#"
        entity Token {
            routes { go() => [] }
            m_n: u64 { in go() => m_n }
        }
        test "typed let" for Token {
            let deployer: address = 0x0000000000000000000000000000000000000000000000000000000000000d01
            call go()
        }
    "#,
    );
    let step = &program.tests[0].body[0];
    assert!(matches!(
        step,
        TestStep::Let {
            name,
            ty: Some(Type::Simple(s)),
            ..
        } if name == "deployer" && s == "address"
    ));
}

#[test]
fn typed_lit_t25_rejects_incompatible_spec_let() {
    let program = parse(
        r#"
        entity Token {
            routes { go() => [] }
            m_n: u64 { in go() => m_n }
        }
        test "bad typed let" for Token {
            let flag: bool = 1
            call go()
        }
    "#,
    );
    let diags = cambrian_transpiler::validate::validate(&program);
    assert!(
        diags.iter().any(|d| d.code == "T30"),
        "T30 must reject bool let from numeric literal: {diags:?}"
    );
}

#[test]
fn typed_lit_harness_emits_typed_address_let() {
    let program = parse(
        r#"
        entity Token {
            routes {
                transfer(to: address, amount: u64) => []
            }
            m_balance: u64 { in transfer(to, amount) => m_balance }
        }
        test "typed addr let" for Token {
            let to: address = 0x0000000000000000000000000000000000000000000000000000000000000d01
            call transfer(to, 1)
        }
    "#,
    );
    let files = generate_evm_tests(&program, true, &InvariantConfig::default());
    let sol = files
        .into_iter()
        .find(|(path, _)| path.ends_with(".t.sol"))
        .map(|(_, s)| s)
        .expect("test sol");
    assert!(
        sol.contains("address to = address(uint160"),
        "typed let must declare address and wrap literal: {sol}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn typed_lit_native_harness_typed_address_let() {
    let program = parse(
        r#"
        entity Token {
            routes { go() => [] }
            m_n: u64 { in go() => m_n }
        }
        test "native typed addr let" for Token {
            let deployer: address = 0x0000000000000000000000000000000000000000000000000000000000000d01
            call go()
        }
    "#,
    );
    let rust = generate_tests(
        &program,
        "token",
        &FuzzConfig::default(),
        &InvariantConfig::default(),
    )
    .expect("native tests");
    assert!(
        rust.contains("\"0x0000000000000000000000000000000000000d01\".to_string()"),
        "typed address let must lower to hex string on native harness: {rust}"
    );
}

#[cfg(feature = "revm")]
#[test]
fn typed_lit_revm_harness_typed_address_let() {
    let program = parse(
        r#"
        entity Token {
            routes { go() => [] }
            m_n: u64 { in go() => m_n }
        }
        test "revm typed addr let" for Token {
            let deployer: address = 0x0000000000000000000000000000000000000000000000000000000000000d01
            call go()
        }
    "#,
    );
    let cfg = RevmTestConfig {
        enabled: true,
        ..Default::default()
    };
    let files = generate_revm_tests(
        &program,
        "token",
        false,
        &cfg,
        &FuzzConfig::default(),
        &InvariantConfig::default(),
    );
    let rust = files
        .iter()
        .find(|(p, _)| p == "revm-tests/tests/token.rs")
        .map(|(_, c)| c.as_str())
        .expect("token revm test");
    assert!(
        rust.contains("let deployer: Address = address!("),
        "typed address let must use Address type on revm harness: {rust}"
    );
}

#[test]
fn typed_lit_lean_spec_let_address_width() {
    let program = parse(
        r#"
        entity Token {
            routes { go() => [] }
            m_n: u64 { in go() => m_n }
        }
        test "lean typed addr let" for Token {
            let deployer: address = 0x0000000000000000000000000000000000000000000000000000000000000d01
            call go()
        }
    "#,
    );
    let backend = LeanBackend::default();
    let spec = backend
        .extra_files(&program, "Token")
        .into_iter()
        .find(|(p, _)| p.ends_with("TokenSpec.lean"))
        .map(|(_, c)| c)
        .expect("TokenSpec.lean");
    assert!(
        spec.contains("let deployer := (") && spec.contains(": BitVec 160)"),
        "typed address let must honor decl ty as BitVec 160 in Lean spec: {spec}"
    );
}
