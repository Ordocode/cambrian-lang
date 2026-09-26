// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use cambrian_transpiler::ProgramParser;
use cambrian_transpiler::pretty::{pretty_print, fmt_expr, fmt_type, fmt_pattern};

use cambrian_core::U256;
fn parser() -> ProgramParser {
    ProgramParser::new()
}

fn parse(src: &str) -> cambrian_transpiler::ast::Program {
    parser().parse(src).unwrap_or_else(|e| panic!("Parse error: {e}"))
}

/// Parse → pretty-print → re-parse: the two ASTs must be equal.
fn roundtrip(src: &str) {
    let prog1 = parse(src);
    let printed = pretty_print(&prog1);
    let prog2 = parser().parse(&printed)
        .unwrap_or_else(|e| panic!("Re-parse failed for pretty output:\n{}\nError: {}", printed, e));
    assert_eq!(prog1, prog2, "Round-trip mismatch.\nOriginal:\n{}\nPrinted:\n{}", src, printed);
}

// ===================================================================
// Round-trip: simple constructs
// ===================================================================

#[test]
fn roundtrip_minimal_entity() {
    roundtrip("entity A {\n    m_x: u64 {}\n}\n");
}

#[test]
fn roundtrip_entity_with_route() {
    roundtrip("entity E {\n    routes {\n        go(x: u64) => []\n    }\n    m_x: u64 {\n        in go(x) => x\n    }\n}\n");
}

#[test]
fn roundtrip_pure_fn() {
    roundtrip("pure fn add(a: u64, b: u64) -> u64 {\n    a + b\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_record() {
    roundtrip("record Pos { x: u64, y: u64 }\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_enum() {
    roundtrip("enum Dir { Up, Down, Left, Right }\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_enum_with_data() {
    roundtrip("enum Msg { Transfer(u64, u64), Halt }\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_type_alias() {
    roundtrip("type Amount = u128\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_const() {
    roundtrip("entity E {\n    const MAX: u64 = 100\n    m_x: u64 {}\n}\n");
}

#[test]
fn roundtrip_macro() {
    roundtrip("entity E {\n    macro helper(x: u64) -> u64 = {\n        x\n    }\n    m_x: u64 {}\n}\n");
}

#[test]
fn roundtrip_view_route() {
    roundtrip("entity E {\n    routes {\n        view get_val() -> u64 => [\n            return(0)\n        ]\n    }\n    m_x: u64 {}\n}\n");
}

#[test]
fn roundtrip_pure_route() {
    roundtrip("entity E {\n    routes {\n        pure compute(x: u64) -> u64 => [\n            return(x)\n        ]\n    }\n    m_x: u64 {}\n}\n");
}

#[test]
fn roundtrip_where_clause() {
    roundtrip("entity E {\n    routes {\n        act(x: u64)\n            where x > 0 : throw 1\n            => []\n    }\n    m_x: u64 {\n        in act(x) => x\n    }\n}\n");
}

#[test]
fn roundtrip_from_clause() {
    roundtrip("entity E {\n    routes {\n        act(x: u64)\n            from Admin(x)\n            => []\n    }\n    m_x: u64 {\n        in act(x) => x\n    }\n}\n");
}

#[test]
fn roundtrip_send_action() {
    roundtrip("entity E {\n    routes {\n        act(addr: address) => [\n            notify(42) ~> addr\n        ]\n    }\n    m_x: u64 {}\n}\n");
}

#[test]
fn roundtrip_conditional_action() {
    roundtrip("entity E {\n    routes {\n        act(x: u64) => [\n            if x > 0 => [\n                return(x)\n            ]\n        ]\n    }\n    m_x: u64 {\n        in act(x) => x\n    }\n}\n");
}

#[test]
fn roundtrip_member_default_value() {
    roundtrip("entity E {\n    m_x: u64 = 42 {}\n}\n");
}

#[test]
fn roundtrip_multiple_members() {
    roundtrip("entity E {\n    routes {\n        go() => []\n    }\n    m_a: u64 {\n        in go() => 1\n    }\n    m_b: bool {\n        in go() => true\n    }\n}\n");
}

#[test]
fn roundtrip_entity_local_record() {
    roundtrip("entity E {\n    record Inner { x: u64 }\n    m_x: u64 {}\n}\n");
}

#[test]
fn roundtrip_entity_local_enum() {
    roundtrip("entity E {\n    enum Status { Active, Done }\n    m_x: u64 {}\n}\n");
}

#[test]
fn roundtrip_entity_local_type_alias() {
    roundtrip("entity E {\n    type Balance = u128\n    m_x: u64 {}\n}\n");
}

// ===================================================================
// Round-trip: expressions
// ===================================================================

#[test]
fn roundtrip_if_else_in_pure() {
    roundtrip("pure fn f(x: u64) -> u64 {\n    if x > 0 { x } else { 0 }\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_let_chain() {
    roundtrip("pure fn f(x: u64) -> u64 {\n    let a = x + 1; a\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_match_expr() {
    roundtrip("pure fn f(x: u64) -> u64 {\n    match x { 0 => 1, _ => x }\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_some_none() {
    roundtrip("pure fn f(x: u64) -> Option<u64> {\n    some(x)\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_option_match() {
    roundtrip("pure fn f(x: Option<u64>) -> u64 {\n    match x { some(v) => v, none => 0 }\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_temporal_ref() {
    roundtrip("entity E {\n    routes { go() => [] }\n    m_a: u64 {\n        in go() => 1\n    }\n    m_b: u64 {\n        in go() => ^m_a\n    }\n}\n");
}

#[test]
fn roundtrip_closure() {
    roundtrip("pure fn f(xs: Vec<u64>) -> Vec<u64> {\n    xs.map(|x| x + 1)\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_cast() {
    roundtrip("pure fn f(x: u32) -> u64 {\n    x as u64\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_tuple() {
    roundtrip("pure fn f(x: u64) -> (u64, u64) {\n    (x, x)\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_range_for() {
    roundtrip("pure fn f(n: u64) -> Vec<u64> {\n    for x in 0..n { x }\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_wrapping_arith() {
    roundtrip("pure fn f(a: u64, b: u64) -> u64 {\n    a +% b\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_array_literal() {
    roundtrip("pure fn f() -> Vec<u64> {\n    array(1, 2, 3)\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_empty_collection() {
    roundtrip("entity E {\n    m_x: HashMap<u64, u64> {}\n}\n");
}

#[test]
fn roundtrip_enum_variant() {
    roundtrip("enum D { A, B }\npure fn f() -> D {\n    D::A\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_method_call() {
    roundtrip("pure fn f(v: Vec<u64>) -> u64 {\n    v.len()\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_field_access() {
    roundtrip("record P { x: u64 }\npure fn f(p: P) -> u64 {\n    p.x\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_msg_field() {
    roundtrip("entity E {\n    routes {\n        act() => []\n    }\n    m_sender: address {\n        in act() => msg::sender\n    }\n}\n");
}

#[test]
fn roundtrip_macro_ref() {
    roundtrip("entity E {\n    macro m() -> u64 = {\n        42\n    }\n    routes {\n        act() => []\n    }\n    m_x: u64 {\n        in act() => @m()\n    }\n}\n");
}

// ===================================================================
// Round-trip: fixture files
// ===================================================================

fn roundtrip_fixture(name: &str) {
    let path = format!("../contracts/{}", name);
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{} not found", path));
    let prog1 = parse(&src);
    let printed = pretty_print(&prog1);
    let prog2 = parser().parse(&printed)
        .unwrap_or_else(|e| panic!("Re-parse of {} failed:\n{}\nError: {}", name, printed, e));
    assert_eq!(prog1, prog2, "Round-trip mismatch for {}", name);
}

#[test]
fn roundtrip_fixture_counter() { roundtrip_fixture("counter.cam"); }
#[test]
fn roundtrip_fixture_token() { roundtrip_fixture("token.cam"); }
#[test]
fn roundtrip_fixture_voting() { roundtrip_fixture("voting.cam"); }
#[test]
fn roundtrip_fixture_escrow() { roundtrip_fixture("escrow.cam"); }
#[test]
fn roundtrip_fixture_escrow_v2() { roundtrip_fixture("escrow_v2.cam"); }
#[test]
fn roundtrip_fixture_staking() { roundtrip_fixture("staking.cam"); }
#[test]
fn roundtrip_fixture_dex() { roundtrip_fixture("dex.cam"); }
#[test]
fn roundtrip_fixture_nft() { roundtrip_fixture("nft.cam"); }
#[test]
fn roundtrip_fixture_payment_channel() { roundtrip_fixture("payment_channel.cam"); }

// ===================================================================
// fmt_expr unit tests
// ===================================================================

#[test]
fn fmt_expr_int_literal() {
    let e = cambrian_transpiler::ast::Expr::IntLiteral(U256::from_u128(42));
    assert_eq!(fmt_expr(&e), "42");
}

#[test]
fn fmt_expr_hex_literal() {
    let e = cambrian_transpiler::ast::Expr::IntLiteral(U256::from_hex_digits("ff").unwrap());
    // TYPED-LIT: fits_u128 literals pretty-print as decimal (AST has no radix).
    assert_eq!(fmt_expr(&e), "255");
}

#[test]
fn fmt_expr_bool_literal() {
    assert_eq!(fmt_expr(&cambrian_transpiler::ast::Expr::BoolLiteral(true)), "true");
    assert_eq!(fmt_expr(&cambrian_transpiler::ast::Expr::BoolLiteral(false)), "false");
}

#[test]
fn fmt_expr_string_literal() {
    let e = cambrian_transpiler::ast::Expr::StringLiteral("hello".to_string());
    assert_eq!(fmt_expr(&e), "\"hello\"");
}

#[test]
fn fmt_expr_none() {
    assert_eq!(fmt_expr(&cambrian_transpiler::ast::Expr::None), "none");
}

#[test]
fn fmt_expr_some() {
    let e = cambrian_transpiler::ast::Expr::Some(Box::new(cambrian_transpiler::ast::Expr::IntLiteral(U256::from_u128(1))));
    assert_eq!(fmt_expr(&e), "some(1)");
}

#[test]
fn fmt_expr_empty_collection() {
    assert_eq!(fmt_expr(&cambrian_transpiler::ast::Expr::EmptyCollection), "{}");
}

#[test]
fn fmt_expr_temporal_ref() {
    let e = cambrian_transpiler::ast::Expr::TemporalRef("m_x".to_string());
    assert_eq!(fmt_expr(&e), "^m_x");
}

#[test]
fn fmt_expr_msg_field() {
    let e = cambrian_transpiler::ast::Expr::MsgField("sender".to_string());
    assert_eq!(fmt_expr(&e), "msg::sender");
}

#[test]
fn fmt_expr_sys_field() {
    let e = cambrian_transpiler::ast::Expr::SysField("now".to_string());
    assert_eq!(fmt_expr(&e), "sys::now");
}

#[test]
fn fmt_expr_binop_add() {
    use cambrian_transpiler::ast::*;
    let e = Expr::BinOp(Box::new(Expr::IntLiteral(U256::from_u128(1))), BinOp::Add, Box::new(Expr::IntLiteral(U256::from_u128(2))));
    assert_eq!(fmt_expr(&e), "(1 + 2)");
}

#[test]
fn fmt_expr_binop_wrapping() {
    use cambrian_transpiler::ast::*;
    let e = Expr::BinOp(Box::new(Expr::Ident("a".into())), BinOp::WrappingAdd, Box::new(Expr::Ident("b".into())));
    assert_eq!(fmt_expr(&e), "(a +% b)");
}

#[test]
fn fmt_expr_unary_not() {
    use cambrian_transpiler::ast::*;
    let e = Expr::UnaryOp(UnaryOp::Not, Box::new(Expr::BoolLiteral(true)));
    assert_eq!(fmt_expr(&e), "!true");
}

#[test]
fn fmt_expr_range() {
    use cambrian_transpiler::ast::*;
    let e = Expr::Range(Box::new(Expr::IntLiteral(U256::ZERO)), Box::new(Expr::IntLiteral(U256::from_u128(10))));
    assert_eq!(fmt_expr(&e), "0..10");
}

#[test]
fn fmt_expr_for() {
    use cambrian_transpiler::ast::*;
    let e = Expr::For(
        Pattern::Ident("x".into()),
        Box::new(Expr::Range(Box::new(Expr::IntLiteral(U256::ZERO)), Box::new(Expr::IntLiteral(U256::from_u128(5))))),
        Box::new(Expr::Ident("x".into())),
    );
    assert_eq!(fmt_expr(&e), "for x in 0..5 { x }");
}

// ===================================================================
// fmt_type unit tests
// ===================================================================

#[test]
fn fmt_type_simple() {
    assert_eq!(fmt_type(&cambrian_transpiler::ast::Type::Simple("u64".into())), "u64");
}

#[test]
fn fmt_type_generic() {
    use cambrian_transpiler::ast::Type;
    let t = Type::Generic("HashMap".into(), vec![Type::Simple("u64".into()), Type::Simple("String".into())]);
    assert_eq!(fmt_type(&t), "HashMap<u64, String>");
}

#[test]
fn fmt_type_tuple() {
    use cambrian_transpiler::ast::Type;
    let t = Type::Tuple(vec![Type::Simple("u64".into()), Type::Simple("bool".into())]);
    assert_eq!(fmt_type(&t), "(u64, bool)");
}

#[test]
fn fmt_type_typed_address() {
    use cambrian_transpiler::ast::Type;
    let t = Type::TypedAddress("Admin".into());
    assert_eq!(fmt_type(&t), "Address<Admin>");
}

// ===================================================================
// fmt_pattern unit tests
// ===================================================================

#[test]
fn fmt_pattern_ident() {
    assert_eq!(fmt_pattern(&cambrian_transpiler::ast::Pattern::Ident("x".into())), "x");
}

#[test]
fn fmt_pattern_wildcard() {
    assert_eq!(fmt_pattern(&cambrian_transpiler::ast::Pattern::Wildcard), "_");
}

#[test]
fn fmt_pattern_tuple() {
    use cambrian_transpiler::ast::Pattern;
    let p = Pattern::Tuple(vec![Pattern::Ident("a".into()), Pattern::Ident("b".into())]);
    assert_eq!(fmt_pattern(&p), "(a, b)");
}

#[test]
fn fmt_pattern_deref() {
    use cambrian_transpiler::ast::Pattern;
    let p = Pattern::Deref(Box::new(Pattern::Ident("x".into())));
    assert_eq!(fmt_pattern(&p), "*x");
}

#[test]
fn fmt_pattern_some() {
    use cambrian_transpiler::ast::Pattern;
    let p = Pattern::Some(Box::new(Pattern::Ident("v".into())));
    assert_eq!(fmt_pattern(&p), "some(v)");
}

#[test]
fn fmt_pattern_none() {
    assert_eq!(fmt_pattern(&cambrian_transpiler::ast::Pattern::None), "none");
}

// Gap 8: TypedAddress formatting preserves entity name
#[test]
fn fmt_type_typed_address_entity_name() {
    use cambrian_transpiler::ast::Type;
    assert_eq!(fmt_type(&Type::TypedAddress("MyEntity".into())), "Address<MyEntity>");
}

// Identity member round-trip
#[test]
fn pretty_identity_member_roundtrip() {
    let src = r#"
        entity Vault {
            routes { constructor() => [] }
            identity m_id: u64
            m_balance: u64 {
                in constructor() => 0
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let pp = pretty_print(&program);
    assert!(pp.contains("identity m_id: u64"), "pretty print must contain identity keyword: {}", pp);
    assert!(pp.contains("m_balance: u64 {"), "regular member must not have identity: {}", pp);
    // Round-trip: parse the pretty-printed output
    let reparsed = ProgramParser::new().parse(&pp).unwrap();
    assert!(reparsed.entities[0].members[0].is_identity);
    assert_eq!(reparsed.entities[0].members[0].name, "m_id");
    assert!(!reparsed.entities[0].members[1].is_identity);
    assert_eq!(reparsed.entities[0].members[1].name, "m_balance");
}

#[test]
fn pretty_multiple_identity_members_roundtrip() {
    let src = r#"
        entity MultiId {
            identity m_owner: address
            identity m_nonce: u64
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let pp = pretty_print(&program);
    assert!(pp.contains("identity m_owner: address"), "pp: {}", pp);
    assert!(pp.contains("identity m_nonce: u64"), "pp: {}", pp);
    let reparsed = ProgramParser::new().parse(&pp).unwrap();
    assert!(reparsed.entities[0].members.iter().all(|m| m.is_identity));
}

// ===================================================================
// Round-trip: regression tests for specific bugs
// ===================================================================

#[test]
fn roundtrip_deploy_with_args_and_opts() {
    roundtrip(
        "entity Factory {\n    routes {\n        create(code: CamData) => [\n            deploy Child(42) with { value: 100, stateInit: code }\n        ]\n    }\n    m_x: u64 {}\n}\n",
    );
}

#[test]
fn roundtrip_deploy_args_only() {
    roundtrip(
        "entity Factory {\n    routes {\n        create() => [\n            deploy Child(1, 2)\n        ]\n    }\n    m_x: u64 {}\n}\n",
    );
}

#[test]
fn roundtrip_deploy_opts_only() {
    roundtrip(
        "entity Factory {\n    routes {\n        create(code: CamData) => [\n            deploy Child with { value: 100, stateInit: code }\n        ]\n    }\n    m_x: u64 {}\n}\n",
    );
}

#[test]
fn roundtrip_phased_transform() {
    roundtrip(
        "entity V {\n    routes {\n        deposit(amount: u128) => [\n            save: [\n            ]\n            finish: [\n            ]\n        ]\n    }\n    m_balance: u128 {\n        in deposit(amount) => save: m_balance + amount\n    }\n}\n",
    );
}

#[test]
fn roundtrip_fixture_phased_vault() {
    roundtrip_fixture("phased_vault.cam");
}

#[test]
fn roundtrip_string_literal() {
    roundtrip(
        "pure fn f() -> String {\n    \"hello world\"\n}\nentity E { m_x: u64 {} }\n",
    );
}

#[test]
fn roundtrip_string_literal_with_backslash() {
    roundtrip(
        "pure fn f() -> String {\n    \"path\\to\\file\"\n}\nentity E { m_x: u64 {} }\n",
    );
}

#[test]
fn roundtrip_bytes_literal() {
    roundtrip(
        "pure fn f() -> Vec<u8> {\n    b\"hello\"\n}\nentity E { m_x: u64 {} }\n",
    );
}

// ===================================================================
// Round-trip: route actions (expanded coverage)
// ===================================================================

#[test]
fn roundtrip_throw_action() {
    roundtrip("entity E { routes { go() => [throw 100] } m_x: u64 {} }");
}

#[test]
fn roundtrip_call_route() {
    roundtrip("entity E { routes { private helper() => [] go() => [call helper()] } m_x: u64 { in helper() => 0 } }");
}

#[test]
fn roundtrip_route_let() {
    roundtrip("entity E { routes { go(x: u64) => [let y = x + 1;] } m_v: u64 { in go(x) => x } }");
}

#[test]
fn roundtrip_conditional_else() {
    roundtrip("entity E { routes { go(x: u64) => [if x > 0 => [return(x)] else [throw 100]] } m_x: u64 { in go(x) => x } }");
}

#[test]
fn roundtrip_private_route() {
    roundtrip("entity E { routes { private helper(x: u64) => [throw 1] go() => [call helper(42)] } m_x: u64 {} }");
}

// ===================================================================
// Round-trip: expressions (expanded coverage)
// ===================================================================

#[test]
fn roundtrip_hex_literal() {
    roundtrip("pure fn f() -> u64 {\n    0xFF\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_bin_literal() {
    roundtrip("pure fn f() -> u64 {\n    0b1010\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_unary_not() {
    roundtrip("pure fn f(x: bool) -> bool {\n    !x\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_unary_neg() {
    roundtrip("pure fn f(x: i32) -> i32 {\n    -x\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_index_expr() {
    roundtrip("entity E { routes { go() => [] } m_v: Vec<u64> { in go() => m_v } m_x: u64 { in go() => m_v[0] } }");
}

#[test]
fn roundtrip_record_construct() {
    roundtrip("record Point { x: u64, y: u64 }\npure fn f() -> Point {\n    Point { x: 1, y: 2 }\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_record_update() {
    roundtrip("record Point { x: u64, y: u64 }\npure fn f(p: Point) -> Point {\n    p { x: 99 }\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_enum_variant_with_data() {
    roundtrip("enum Msg { Transfer(u64), Approve(u64) }\npure fn f() -> Msg {\n    Msg::Transfer(100)\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_sys_field() {
    roundtrip("entity E { routes { go() => [] } m_t: u64 { in go() => sys::now } }");
}

#[test]
fn roundtrip_namespaced_call() {
    roundtrip("use gosh\nentity E { routes { go() => [gosh::rawReserve(100, 0)] } m_x: u64 {} }");
}

#[test]
fn roundtrip_match_multiple_arms() {
    roundtrip("enum Color { Red, Blue, Green }\npure fn f(c: Color) -> u64 {\n    match c { Color::Red => 1, Color::Blue => 2, Color::Green => 3 }\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_for_expression() {
    roundtrip("pure fn f(xs: Vec<u64>) -> Vec<u64> {\n    for x in xs { x + 1 }\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_range_expression() {
    roundtrip("pure fn f() -> Vec<u64> {\n    0..10\n}\nentity E { m_x: u64 {} }\n");
}

// ===================================================================
// Round-trip: other structures (expanded coverage)
// ===================================================================

#[test]
fn roundtrip_import() {
    roundtrip("use gosh\nentity E { routes { go() => [] } m_x: u64 {} }");
}

#[test]
fn roundtrip_type_alias_as_member_type() {
    roundtrip("type Amount = u64\nentity E { routes { go() => [] } m_x: Amount {} }");
}

// ===================================================================
// Round-trip: closure with multi-arg
// ===================================================================

#[test]
fn roundtrip_closure_multi_arg() {
    roundtrip("pure fn f(xs: Vec<u64>) -> u64 {\n    xs.fold(0, |acc, x| acc + x)\n}\nentity E { m_x: u64 {} }\n");
}

// ===================================================================
// Round-trip: Option some/none expressions
// ===================================================================

#[test]
fn roundtrip_some_expression() {
    roundtrip("pure fn f() -> Option<u64> {\n    some(42)\n}\nentity E { m_x: u64 {} }\n");
}

#[test]
fn roundtrip_none_expression() {
    roundtrip("pure fn f() -> Option<u64> {\n    none\n}\nentity E { m_x: u64 {} }\n");
}

// ===================================================================
// Round-trip: tuple field access
// ===================================================================

#[test]
fn roundtrip_tuple_field_access() {
    roundtrip("pure fn f(p: (u64, u64)) -> u64 {\n    p.0\n}\nentity E { m_x: u64 {} }\n");
}

// ===================================================================
// Round-trip: for over collection
// ===================================================================

#[test]
fn roundtrip_for_over_collection() {
    roundtrip("pure fn f(xs: Vec<u64>) -> Vec<u64> {\n    for x in xs { x + 1 }\n}\nentity E { m_x: u64 {} }\n");
}
