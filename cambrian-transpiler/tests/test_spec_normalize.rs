// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! W11 lint for ctor/msg ordering in spec bodies.

use cambrian_transpiler::ast;
use cambrian_transpiler::spec_normalize;
use cambrian_transpiler::validate;
use cambrian_transpiler::ProgramParser;

const MINT_ENTITY: &str = r#"
entity MintToken {
    const ZERO: address = 0x0000000000000000000000000000000000000000
    routes {
        constructor()
            where msg::sender != ZERO : throw 201
        => []
    }
    m_balance: u64 {
        in constructor() => 1000
    }
}
"#;

fn parse_program(source: &str) -> ast::Program {
    let mut p = ProgramParser::new().parse(source).expect("parse");
    ast::normalize_program_types(&mut p);
    p
}

#[test]
fn w11_warns_on_ctor_before_msg() {
    let src = format!(
        r#"{MINT_ENTITY}
property "p" () for MintToken {{
    call constructor()
    msg {{ sender: 0x0000000000000000000000000000000000000000000000000000000000000d01 }}
}}
"#
    );
    let program = parse_program(&src);
    let diags = validate::validate(&program);
    assert!(
        diags.iter().any(|d| d.code == "W11"),
        "expected W11, got: {diags:?}"
    );
}

#[test]
fn w11_silent_when_msg_precedes_ctor() {
    let src = format!(
        r#"{MINT_ENTITY}
property "p" () for MintToken {{
    msg {{ sender: 0x0000000000000000000000000000000000000000000000000000000000000d01 }}
    call constructor()
}}
"#
    );
    let program = parse_program(&src);
    let entity = &program.entities[0];
    let body = &program.properties[0].body;
    assert!(!spec_normalize::has_ctor_before_msg_pattern(entity, body));
    let diags = validate::validate(&program);
    assert!(
        !diags.iter().any(|d| d.code == "W11"),
        "unexpected W11: {diags:?}"
    );
}
