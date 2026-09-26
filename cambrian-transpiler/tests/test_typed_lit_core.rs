// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! TYPED-LIT-0 — core EVM emission: numeric literal default, no implicit `address`.

use cambrian_transpiler::codegen::gen_evm_solidity;
use cambrian_transpiler::ProgramParser;

fn gen_sol(src: &str) -> String {
    let mut program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    cambrian_transpiler::using_rewrite::apply_using_rewrites(&mut program);
    gen_evm_solidity(&program, true)
}

#[test]
fn typed_lit_route_decimal_arith_no_implicit_address() {
    let sol = gen_sol(
        r#"
        entity Arith {
            routes {
                sum() -> u64 => [
                    let small = 5;
                    let amount = 1000000000000000000;
                    return(small + amount)
                ]
            }
            m_n: u64 { in sum() => m_n }
        }
    "#,
    );
    assert!(
        !sol.contains("address small") && !sol.contains("address amount"),
        "implicit address let must not appear: {sol}"
    );
    assert!(
        sol.contains("uint64 small") || sol.contains("uint256 small"),
        "small must be numeric typed: {sol}"
    );
}

#[test]
fn typed_lit_cast_u256_literal_not_address_wrap() {
    let sol = gen_sol(
        r#"
        entity CastU256 {
            routes {
                go() -> U256 => [
                    return(1 as U256)
                ]
            }
            m_n: U256 { in go() => m_n }
        }
    "#,
    );
    assert!(
        !sol.contains("uint256(address(uint160"),
        "U256 cast must not wrap address(): {sol}"
    );
    assert!(
        sol.contains("uint256(1)") || sol.contains("return uint256(1)"),
        "expected plain uint256(1): {sol}"
    );
}

#[test]
fn typed_lit_entity_const_address_wraps_zero() {
    let sol = gen_sol(
        r#"
        entity Token {
            const ZERO: address = 0x0000000000000000000000000000000000000000
            routes { go() => [] }
            m_n: u64 { in go() => m_n }
        }
    "#,
    );
    assert!(
        sol.contains("address public constant ZERO = address(uint160"),
        "address entity const must wrap hex literal: {sol}"
    );
    assert!(
        !sol.contains("address public constant ZERO = 0"),
        "bare 0 is invalid for address const: {sol}"
    );
}

#[test]
fn typed_lit_v46_retired_untyped_decimal_let_no_warning() {
    let mut program = ProgramParser::new()
        .parse(
            r#"
            entity E {
                routes {
                    constructor() => []
                    go() -> u64 => [
                        let h = 5;
                        return(h)
                    ]
                }
                m_n: u64 { in constructor() => 0 in go() => m_n }
            }
        "#,
        )
        .unwrap();
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    let diags = cambrian_transpiler::validate::validate(&program);
    assert!(
        !diags.iter().any(|d| d.code == "V46"),
        "V46 retired — decimal untyped let must not warn: {diags:?}"
    );
}
