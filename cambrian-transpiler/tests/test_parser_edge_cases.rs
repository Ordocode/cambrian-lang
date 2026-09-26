// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use cambrian_core::U256;
use cambrian_transpiler::ast::*;
use cambrian_transpiler::ProgramParser;

const MAX_U256_DECIMAL: &str =
    "115792089237316195423570985008687907853269984665640564039457584007913129639935";

fn parse(src: &str) -> Program {
    ProgramParser::new().parse(src)
        .unwrap_or_else(|e| panic!("Parse failed:\n{}\nError: {}", src, e))
}

// ===================================================================
// Edge cases: boundary values and unusual valid inputs
// ===================================================================

#[test]
fn edge_zero_literal() {
    let p = parse("pure fn f() -> u64 { 0 }");
    match &p.pure_fns[0].body {
        Expr::IntLiteral(n) => assert_eq!(*n, U256::ZERO),
        other => panic!("Expected IntLiteral(0), got {:?}", other),
    }
}

#[test]
fn edge_large_int_literal() {
    let p = parse("pure fn f() -> u128 { 340282366920938463463374607431768211455 }");
    match &p.pure_fns[0].body {
        Expr::IntLiteral(n) => assert_eq!(*n, U256::from_u128(u128::MAX)),
        other => panic!("Expected IntLiteral(u128::MAX), got {:?}", other),
    }
}

#[test]
fn edge_hex_max() {
    let p = parse("pure fn f() -> u128 { 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF }");
    match &p.pure_fns[0].body {
        Expr::IntLiteral(n) => assert_eq!(*n, U256::from_u128(u128::MAX)),
        other => panic!("Expected IntLiteral(u128::MAX), got {:?}", other),
    }
}

#[test]
fn edge_decimal_u256_max() {
    let p = parse(&format!("pure fn f() -> U256 {{ {} }}", MAX_U256_DECIMAL));
    match &p.pure_fns[0].body {
        Expr::IntLiteral(n) => assert_eq!(*n, U256::MAX),
        other => panic!("Expected IntLiteral(U256::MAX), got {:?}", other),
    }
}

#[test]
fn edge_binary_all_ones() {
    let p = parse("pure fn f() -> u8 { 0b11111111 }");
    match &p.pure_fns[0].body {
        Expr::IntLiteral(n) => assert_eq!(*n, U256::from_u128(255)),
        other => panic!("Expected IntLiteral(255), got {:?}", other),
    }
}

#[test]
fn edge_binary_zero() {
    let p = parse("pure fn f() -> u8 { 0b0 }");
    match &p.pure_fns[0].body {
        Expr::IntLiteral(n) => assert_eq!(*n, U256::ZERO),
        other => panic!("Expected IntLiteral(0), got {:?}", other),
    }
}

#[test]
fn edge_underscores_in_number() {
    let p = parse("pure fn f() -> u64 { 1_0_0_0 }");
    match &p.pure_fns[0].body {
        Expr::IntLiteral(n) => assert_eq!(*n, U256::from_u128(1000)),
        other => panic!("Expected IntLiteral(1000), got {:?}", other),
    }
}

#[test]
fn edge_u256_max_type_assoc_desugar() {
    let mut p = parse("pure fn f() -> U256 { U256::MAX }");
    normalize_program_types(&mut p);
    match &p.pure_fns[0].body {
        Expr::IntLiteral(n) => assert_eq!(*n, U256::MAX),
        other => panic!("Expected desugared IntLiteral(U256::MAX), got {:?}", other),
    }
}

#[test]
fn edge_empty_string() {
    let p = parse(r#"pure fn f() -> String { "" }"#);
    match &p.pure_fns[0].body {
        Expr::StringLiteral(s) => assert_eq!(s, ""),
        other => panic!("Expected empty StringLiteral, got {:?}", other),
    }
}

#[test]
fn edge_string_with_escapes() {
    let p = parse(r#"pure fn f() -> String { "hello\nworld" }"#);
    match &p.pure_fns[0].body {
        Expr::StringLiteral(s) => assert_eq!(s, "hello\nworld"),
        other => panic!("Expected StringLiteral with escape, got {:?}", other),
    }
}

#[test]
fn edge_empty_bytes() {
    let p = parse(r#"pure fn f() -> bytes { b"" }"#);
    match &p.pure_fns[0].body {
        Expr::BytesLiteral(b) => assert!(b.is_empty()),
        other => panic!("Expected empty BytesLiteral, got {:?}", other),
    }
}

#[test]
fn edge_empty_array() {
    let p = parse("pure fn f() -> u64 { array() }");
    match &p.pure_fns[0].body {
        Expr::ArrayLit(v) => assert!(v.is_empty()),
        other => panic!("Expected empty ArrayLit, got {:?}", other),
    }
}

#[test]
fn edge_single_element_array() {
    let p = parse("pure fn f() -> u64 { array(42) }");
    match &p.pure_fns[0].body {
        Expr::ArrayLit(v) => assert_eq!(v.len(), 1),
        other => panic!("Expected ArrayLit with 1 element, got {:?}", other),
    }
}

#[test]
fn edge_nested_if() {
    let src = r#"
        pure fn f(a: bool, b: bool, c: bool) -> u64 {
            if a { if b { if c { 1 } else { 2 } } else { 3 } } else { 4 }
        }
    "#;
    let p = parse(src);
    assert_eq!(p.pure_fns.len(), 1);
}

#[test]
fn edge_deeply_nested_method_chain() {
    let src = r#"
        pure fn f(x: u64) -> u64 {
            x.to_string().len().to_string().len()
        }
    "#;
    let p = parse(src);
    assert_eq!(p.pure_fns.len(), 1);
}

#[test]
fn edge_deeply_nested_field_access() {
    let src = "pure fn f(x: u64) -> u64 { x.a.b.c.d.e.f }";
    let p = parse(src);
    assert_eq!(p.pure_fns.len(), 1);
}

#[test]
fn edge_many_params() {
    let params: Vec<String> = (0..20)
        .map(|i| format!("p{}: u64", i))
        .collect();
    let src = format!("pure fn f({}) -> u64 {{ p0 }}", params.join(", "));
    let p = parse(&src);
    assert_eq!(p.pure_fns[0].params.len(), 20);
}

#[test]
fn edge_empty_entity() {
    let p = parse("entity Empty {}");
    assert_eq!(p.entities.len(), 1);
    assert!(p.entities[0].routes.is_empty());
    assert!(p.entities[0].members.is_empty());
}

#[test]
fn edge_entity_only_routes_no_members() {
    let p = parse("entity E { routes { go() => [] } }");
    assert_eq!(p.entities[0].routes.len(), 1);
    assert!(p.entities[0].members.is_empty());
}

#[test]
fn edge_member_no_transforms() {
    let p = parse("entity E { m_x: u64 {} }");
    assert!(p.entities[0].members[0].transforms.is_empty());
}

#[test]
fn edge_multiple_where_clauses() {
    let src = r#"
        entity E {
            routes {
                go(x: u64)
                where x > 0 : throw 100
                && x < 1000 : throw 101
                && x != 42 : throw 102
                && x != 13 : throw 103
                => []
            }
        }
    "#;
    let p = parse(src);
    assert_eq!(p.entities[0].routes[0].where_clauses.len(), 4);
}

#[test]
fn edge_route_many_sends() {
    let sends: Vec<String> = (0..10)
        .map(|i| format!("msg{}(0) ~> target", i))
        .collect();
    let src = format!(
        "entity E {{ routes {{ go() => [ {} ] }} }}",
        sends.join("\n")
    );
    let p = parse(&src);
    assert_eq!(p.entities[0].routes[0].body.actions().len(), 10);
}

#[test]
fn edge_route_nested_conditionals() {
    let src = r#"
        entity E {
            routes {
                go(x: u64) => [
                    if x > 100 => [
                        if x > 200 => [
                            if x > 300 => [
                                deep(x) ~> target
                            ]
                        ]
                    ]
                ]
            }
        }
    "#;
    let p = parse(src);
    match &p.entities[0].routes[0].body.actions()[0] {
        RouteAction::Conditional { then_actions, .. } => {
            match &then_actions[0] {
                RouteAction::Conditional { then_actions: inner, .. } => {
                    match &inner[0] {
                        RouteAction::Conditional { .. } => {}
                        other => panic!("Expected nested Conditional, got {:?}", other),
                    }
                }
                other => panic!("Expected Conditional, got {:?}", other),
            }
        }
        other => panic!("Expected Conditional, got {:?}", other),
    }
}

#[test]
fn edge_many_type_aliases() {
    let aliases: Vec<String> = (0..10)
        .map(|i| format!("type T{} = u64", i))
        .collect();
    let src = aliases.join("\n");
    let p = parse(&src);
    assert_eq!(p.type_aliases.len(), 10);
}

#[test]
fn edge_many_pure_fns() {
    let fns: Vec<String> = (0..10)
        .map(|i| format!("pure fn f{}(x: u64) -> u64 {{ x + {} }}", i, i))
        .collect();
    let src = fns.join("\n");
    let p = parse(&src);
    assert_eq!(p.pure_fns.len(), 10);
}

#[test]
fn edge_many_entities() {
    let entities: Vec<String> = (0..10)
        .map(|i| format!("entity E{} {{ routes {{ setup() => [] }} m_x: u64 {{ in setup() => {} }} }}", i, i))
        .collect();
    let src = entities.join("\n");
    let p = parse(&src);
    assert_eq!(p.entities.len(), 10);
}

#[test]
fn edge_record_many_fields() {
    let fields: Vec<String> = (0..20)
        .map(|i| format!("field_{}: u64", i))
        .collect();
    let src = format!("record Big {{ {} }}", fields.join(", "));
    let p = parse(&src);
    assert_eq!(p.records[0].fields.len(), 20);
}

#[test]
fn edge_enum_many_variants() {
    let variants: Vec<String> = (0..20)
        .map(|i| format!("V{}", i))
        .collect();
    let src = format!("enum Big {{ {} }}", variants.join(", "));
    let p = parse(&src);
    assert_eq!(p.enums[0].variants.len(), 20);
}

#[test]
fn edge_enum_variant_with_many_fields() {
    let src = "enum Msg { Complex(u8, u16, u32, u64, u128, String, bool, address) }";
    let p = parse(src);
    assert_eq!(p.enums[0].variants[0].fields.len(), 8);
}

#[test]
fn edge_match_many_arms() {
    let arms: Vec<String> = (0..10)
        .map(|i| format!("{} => {}", i, i * 10))
        .collect();
    let src = format!(
        "pure fn f(x: u64) -> u64 {{ match x {{ {}, _ => 0 }} }}",
        arms.join(", ")
    );
    let p = parse(&src);
    match &p.pure_fns[0].body {
        Expr::Match(_, arms) => assert_eq!(arms.len(), 11),
        other => panic!("Expected Match, got {:?}", other),
    }
}

#[test]
fn edge_complex_expression_precedence() {
    let src = "pure fn f(a: u64, b: u64, c: u64) -> bool { a + b * c > a - b / c && a != 0 || b == 1 }";
    let p = parse(src);
    assert_eq!(p.pure_fns.len(), 1);
}

#[test]
fn edge_let_chain() {
    let src = r#"
        pure fn f(x: u64) -> u64 {
            let a = x + 1;
            let b = a + 1;
            let c = b + 1;
            let d = c + 1;
            let e = d + 1;
            e
        }
    "#;
    let p = parse(src);
    assert_eq!(p.pure_fns.len(), 1);
}

#[test]
fn edge_closure_in_method_chain() {
    let src = r#"
        pure fn f(items: Vec<u64>) -> u64 {
            items.iter().filter(|x| x > 0).map(|x| x * 2).fold(0, |acc, x| acc + x)
        }
    "#;
    let p = parse(src);
    assert_eq!(p.pure_fns.len(), 1);
}

#[test]
fn edge_all_operators() {
    let src = r#"
        pure fn f(a: u64, b: u64) -> u64 {
            let sum = a + b;
            let diff = a - b;
            let prod = a * b;
            let quot = a / b;
            let rem = a % b;
            let band = a & b;
            let bor = a | b;
            let bxor = a ^ b;
            let shl = a << b;
            let shr = a >> b;
            sum
        }
    "#;
    let p = parse(src);
    assert_eq!(p.pure_fns.len(), 1);
}

#[test]
fn edge_all_comparison_operators() {
    let src = r#"
        pure fn f(a: u64, b: u64) -> bool {
            a == b || a != b || a < b || a <= b || a > b || a >= b
        }
    "#;
    let p = parse(src);
    assert_eq!(p.pure_fns.len(), 1);
}

#[test]
fn edge_comments_everywhere() {
    let src = r#"
        // Before type alias
        type X = u64 // after type alias
        // Before record
        record R { // after record open
            x: u64 // after field
        } // after record close
        // Before entity
        entity E { // after entity open
            // Before routes
            routes { // after routes open
                // Before route
                go() // after route name
                => [] // after body
            } // after routes close
            // Before member
            m_x: u64 { // after member open
                in go() => 0 // after transform
            } // after member close
        } // after entity close
        // At end
    "#;
    let p = parse(src);
    assert_eq!(p.type_aliases.len(), 1);
    assert_eq!(p.records.len(), 1);
    assert_eq!(p.entities.len(), 1);
}

#[test]
fn edge_unicode_in_string() {
    let src = r#"pure fn f() -> String { "привет мир 🌍" }"#;
    let p = parse(src);
    match &p.pure_fns[0].body {
        Expr::StringLiteral(s) => assert!(s.contains("привет")),
        other => panic!("Expected StringLiteral with unicode, got {:?}", other),
    }
}

#[test]
fn edge_some_none_in_expressions() {
    let src = r#"
        pure fn f(x: u64) -> u64 {
            match some(x) {
                _ => 0
            }
        }
    "#;
    let p = parse(src);
    assert_eq!(p.pure_fns.len(), 1);
}

#[test]
fn edge_block_expr_in_let() {
    let src = r#"
        pure fn f(x: u64) -> u64 {
            let a = { let b = x + 1; b };
            a
        }
    "#;
    let p = parse(src);
    assert_eq!(p.pure_fns.len(), 1);
}

#[test]
fn edge_nested_block_exprs() {
    let src = r#"
        pure fn f(x: u64) -> u64 {
            let a = {
                let b = {
                    let c = x + 1;
                    c * 2
                };
                b + 10
            };
            a
        }
    "#;
    let p = parse(src);
    assert_eq!(p.pure_fns.len(), 1);
}

#[test]
fn edge_block_as_final_expr() {
    let src = r#"
        pure fn f(x: u64) -> u64 {
            let a = x + 1;
            { let b = a * 2; b }
        }
    "#;
    let p = parse(src);
    assert_eq!(p.pure_fns.len(), 1);
}

#[test]
fn edge_block_in_member_transform() {
    let src = r#"
        entity E {
            routes { go(x: u64) => [] }
            m_val: u64 {
                in go(x) => {
                    let intermediate = { let step = x * 2; step + 1 };
                    intermediate
                }
            }
        }
    "#;
    let p = parse(src);
    assert_eq!(p.entities[0].members[0].transforms.len(), 1);
}

#[test]
fn edge_nested_generic_types() {
    let src = r#"
        entity E {
            m_data: HashMap<String, Vec<u64>> {}
        }
    "#;
    let p = parse(src);
    match &p.entities[0].members[0].ty {
        Type::Generic(name, args) => {
            assert_eq!(name, "HashMap");
            assert_eq!(args.len(), 2);
            match &args[1] {
                Type::Generic(inner_name, _) => assert_eq!(inner_name, "Vec"),
                other => panic!("Expected Generic(Vec), got {:?}", other),
            }
        }
        other => panic!("Expected Generic(HashMap), got {:?}", other),
    }
}

#[test]
fn edge_tuple_type_in_params() {
    let src = "pure fn f(x: (u64, u64, bool)) -> u64 { 0 }";
    let p = parse(src);
    match &p.pure_fns[0].params[0].ty {
        Type::Tuple(ts) => assert_eq!(ts.len(), 3),
        other => panic!("Expected Tuple type, got {:?}", other),
    }
}

// ===================================================================
// Phased routes: edge cases
// ===================================================================

#[test]
fn edge_phased_route_single_all_empty() {
    let p = parse(r#"
        entity E {
            routes { go() => [ only: [] ] }
        }
    "#);
    let phases = p.entities[0].routes[0].body.phases().unwrap();
    assert_eq!(phases.len(), 1);
    assert!(phases[0].actions.is_empty());
}

#[test]
fn edge_phased_route_many_empty_phases() {
    let p = parse(r#"
        entity E {
            routes { go() => [ a: [] b: [] c: [] d: [] e: [] f: [] g: [] h: [] ] }
        }
    "#);
    let phases = p.entities[0].routes[0].body.phases().unwrap();
    assert_eq!(phases.len(), 8);
    for ph in phases {
        assert!(ph.actions.is_empty());
    }
}

#[test]
fn edge_phased_and_unphased_routes_same_entity() {
    let p = parse(r#"
        entity E {
            routes {
                foo() => [ step: [ gosh::commit() ] ]
                bar() => [ gosh::commit() ]
                baz() => []
            }
        }
    "#);
    let routes = &p.entities[0].routes;
    assert!(routes[0].body.is_phased(), "foo is phased");
    assert!(!routes[1].body.is_phased(), "bar is unphased");
    assert!(!routes[2].body.is_phased(), "baz is unphased (empty)");
}

#[test]
fn edge_phase_name_long() {
    let p = parse(r#"
        entity E {
            routes {
                go() => [ very_long_phase_name_that_describes_what_happens: [] ]
            }
        }
    "#);
    let phases = p.entities[0].routes[0].body.phases().unwrap();
    assert_eq!(phases[0].name, "very_long_phase_name_that_describes_what_happens");
}

#[test]
fn edge_phase_name_single_char() {
    let p = parse(r#"
        entity E {
            routes { go() => [ x: [] ] }
        }
    "#);
    let phases = p.entities[0].routes[0].body.phases().unwrap();
    assert_eq!(phases[0].name, "x");
}

#[test]
fn edge_phase_name_with_underscore() {
    let p = parse(r#"
        entity E {
            routes { go() => [ _private: [] step_2: [] __double: [] ] }
        }
    "#);
    let phases = p.entities[0].routes[0].body.phases().unwrap();
    assert_eq!(phases[0].name, "_private");
    assert_eq!(phases[1].name, "step_2");
    assert_eq!(phases[2].name, "__double");
}

#[test]
fn edge_phased_transform_refers_to_other_member() {
    let p = parse(r#"
        entity E {
            routes { go(x: u64) => [ step: [] ] }
            m_a: u64 { in go(x) => step: m_b + x }
            m_b: u64 { in go(x) => step: m_a + x }
        }
    "#);
    let members = &p.entities[0].members;
    assert_eq!(members[0].transforms[0].phase, Some("step".to_string()));
    assert_eq!(members[1].transforms[0].phase, Some("step".to_string()));
}

#[test]
fn edge_phased_route_conditional_else_branch() {
    let p = parse(r#"
        entity E {
            routes {
                foo(x: u64) => [
                    check: [
                        if x > 0 => [
                            gosh::commit()
                        ] else [
                            gosh::exit(1)
                        ]
                    ]
                ]
            }
        }
    "#);
    let phases = p.entities[0].routes[0].body.phases().unwrap();
    match &phases[0].actions[0] {
        RouteAction::Conditional { then_actions, else_actions, .. } => {
            assert_eq!(then_actions.len(), 1);
            assert_eq!(else_actions.len(), 1);
        }
        _ => panic!("expected conditional"),
    }
}

#[test]
fn edge_phased_route_multiple_sends_in_phase() {
    let p = parse(r#"
        entity E {
            routes {
                multi_send(a: address, b: address) => [
                    sends: [
                        msg1(1) ~> a
                        msg2(2) ~> b
                        ~> a
                    ]
                ]
            }
        }
    "#);
    let phases = p.entities[0].routes[0].body.phases().unwrap();
    assert_eq!(phases[0].actions.len(), 3);
    assert!(matches!(&phases[0].actions[0], RouteAction::Send { message: Some(m), .. } if m == "msg1"));
    assert!(matches!(&phases[0].actions[1], RouteAction::Send { message: Some(m), .. } if m == "msg2"));
    assert!(matches!(&phases[0].actions[2], RouteAction::Send { message: None, .. }));
}

#[test]
fn edge_phased_member_same_phase_different_routes() {
    let p = parse(r#"
        entity E {
            routes {
                foo(x: u64) => [ step: [] ]
                bar(y: u64) => [ step: [] ]
            }
            m_val: u64 {
                in foo(x) => step: x
                in bar(y) => step: y
            }
        }
    "#);
    let transforms = &p.entities[0].members[0].transforms;
    assert_eq!(transforms.len(), 2);
    assert_eq!(transforms[0].route_name, "foo");
    assert_eq!(transforms[0].phase, Some("step".to_string()));
    assert_eq!(transforms[1].route_name, "bar");
    assert_eq!(transforms[1].phase, Some("step".to_string()));
}

// ===== Negative parser tests: phased routes =====

#[test]
fn mixed_phases_and_actions_parses_without_panic() {
    let src = r#"
        entity E {
            routes {
                foo() => [
                    step: [ gosh::commit() ]
                    gosh::exit(0)
                ]
            }
        }
    "#;
    let result = ProgramParser::new().parse(src);
    assert!(result.is_ok(), "Mixed phases and bare actions should parse without panic (validation catches it)");
}

// ===== CRITICAL 2: Integer/binary literal overflow =====

#[test]
fn int_literal_overflow_returns_parse_error() {
    let over = format!("{}0", MAX_U256_DECIMAL);
    let src = format!("pure fn f() -> U256 {{ {} }}", over);
    let err = ProgramParser::new()
        .parse(&src)
        .expect_err("overflow must fail parse");
    let msg = err.to_string();
    assert!(
        msg.contains(U256_LITERAL_OVERFLOW),
        "expected overflow message, got: {msg}"
    );
}

#[test]
fn bin_literal_overflow_returns_parse_error() {
    let bits = "1".repeat(257);
    let src = format!("pure fn f() -> U256 {{ 0b{} }}", bits);
    let err = ProgramParser::new()
        .parse(&src)
        .expect_err("overflow must fail parse");
    let msg = err.to_string();
    assert!(
        msg.contains(U256_LITERAL_OVERFLOW),
        "expected overflow message, got: {msg}"
    );
}

// ===== HIGH 1: Block-body closures =====

#[test]
fn closure_with_block_body() {
    let src = r#"
        pure fn f(items: Vec<u64>) -> u64 {
            items.map(|x| {
                let y = x * 2;
                y + 1
            })
        }
    "#;
    let p = parse(src);
    assert_eq!(p.pure_fns.len(), 1);
}

#[test]
fn closure_block_body_with_match() {
    let src = r#"
        pure fn f(items: Vec<u64>) -> u64 {
            items.map(|x| {
                match x {
                    0 => 1,
                    _ => x * 2
                }
            })
        }
    "#;
    let p = parse(src);
    assert_eq!(p.pure_fns.len(), 1);
}

#[test]
fn closure_block_body_with_for() {
    let src = r#"
        pure fn f(items: Vec<u64>) -> u64 {
            items.map(|x| {
                for i in 0..x { i }
            })
        }
    "#;
    let p = parse(src);
    assert_eq!(p.pure_fns.len(), 1);
}

// ===== MEDIUM: Trailing comma support =====

#[test]
fn trailing_comma_in_fn_params() {
    let src = "pure fn f(a: u64, b: u64,) -> u64 { a + b }";
    let p = parse(src);
    assert_eq!(p.pure_fns[0].params.len(), 2);
}

#[test]
fn trailing_comma_in_record_fields() {
    let src = "record R { x: u64, y: u64, }";
    let p = parse(&src);
    assert_eq!(p.records[0].fields.len(), 2);
}

#[test]
fn trailing_comma_in_enum_variants() {
    let src = "enum E { A, B, C, }";
    let p = parse(&src);
    assert_eq!(p.enums[0].variants.len(), 3);
}

#[test]
fn trailing_comma_in_fn_call_args() {
    let src = "pure fn f(a: u64, b: u64) -> u64 { f(1, 2,) }";
    let p = parse(src);
    assert_eq!(p.pure_fns.len(), 1);
}

#[test]
fn trailing_comma_in_array_literal() {
    let src = "pure fn f() -> u64 { array(1, 2, 3,) }";
    let p = parse(src);
    match &p.pure_fns[0].body {
        Expr::ArrayLit(v) => assert_eq!(v.len(), 3),
        other => panic!("Expected ArrayLit, got {:?}", other),
    }
}

#[test]
fn negative_phase_keyword_init_fails() {
    let src = r#"
        entity E {
            routes { foo() => [ init: [] ] }
        }
    "#;
    let result = ProgramParser::new().parse(src);
    assert!(result.is_err(), "'init' is a keyword, cannot be used as phase name");
}

#[test]
fn negative_phase_keyword_return_fails() {
    let src = r#"
        entity E {
            routes { foo() => [ return: [] ] }
        }
    "#;
    let result = ProgramParser::new().parse(src);
    assert!(result.is_err(), "'return' is a keyword, cannot be used as phase name");
}

#[test]
fn negative_phase_keyword_let_fails() {
    let src = r#"
        entity E {
            routes { foo() => [ let: [] ] }
        }
    "#;
    let result = ProgramParser::new().parse(src);
    assert!(result.is_err(), "'let' is a keyword, cannot be used as phase name");
}

#[test]
fn negative_phase_keyword_if_fails() {
    let src = r#"
        entity E {
            routes { foo() => [ if: [] ] }
        }
    "#;
    let result = ProgramParser::new().parse(src);
    assert!(result.is_err(), "'if' is a keyword, cannot be used as phase name");
}

// ===== Pretty-printer: phased routes =====

#[test]
fn pretty_phased_route_roundtrip() {
    let src = r#"
        entity E {
            routes {
                go(x: u64) => [
                    save: [ gosh::commit() ]
                    done: [ gosh::exit(0) ]
                ]
            }
            m_val: u64 {
                in go(x) => save: x
            }
        }
    "#;
    let prog = parse(src);
    let pp = cambrian_transpiler::pretty::pretty_print(&prog);
    assert!(pp.contains("save:"), "Pretty print should contain phase name 'save': {}", pp);
    assert!(pp.contains("done:"), "Pretty print should contain phase name 'done': {}", pp);
}

#[test]
fn pretty_unphased_route_no_phase_names() {
    let src = r#"
        entity E {
            routes {
                go() => [ gosh::commit() ]
            }
        }
    "#;
    let prog = parse(src);
    let pp = cambrian_transpiler::pretty::pretty_print(&prog);
    assert!(!pp.contains("save:"), "Unphased should have no phase names: {}", pp);
    assert!(pp.contains("gosh::commit"), "Should contain effect: {}", pp);
}

#[test]
fn pretty_phased_empty_phases() {
    let src = r#"
        entity E {
            routes {
                go() => [
                    a: []
                    b: []
                ]
            }
        }
    "#;
    let prog = parse(src);
    let pp = cambrian_transpiler::pretty::pretty_print(&prog);
    assert!(pp.contains("a:"), "Phase 'a': {}", pp);
    assert!(pp.contains("b:"), "Phase 'b': {}", pp);
}

// ===== MEDIUM: normalize_program_types covers top-level records/enums/type_aliases =====

#[test]
fn normalize_top_level_record_typed_address() {
    use cambrian_transpiler::ast::{normalize_program_types, Type};
    let src = r#"
        record Peer { addr: Address<Wallet> }
        entity Wallet {}
    "#;
    let mut p = parse(src);
    normalize_program_types(&mut p);
    match &p.records[0].fields[0].ty {
        Type::TypedAddress(name) => assert_eq!(name, "Wallet"),
        other => panic!("Expected TypedAddress after normalization, got {:?}", other),
    }
}

#[test]
fn normalize_top_level_enum_typed_address() {
    use cambrian_transpiler::ast::{normalize_program_types, Type};
    let src = r#"
        enum Dest { Single(Address<Vault>) }
        entity Vault {}
    "#;
    let mut p = parse(src);
    normalize_program_types(&mut p);
    match &p.enums[0].variants[0].fields[0] {
        Type::TypedAddress(name) => assert_eq!(name, "Vault"),
        other => panic!("Expected TypedAddress after normalization, got {:?}", other),
    }
}

#[test]
fn normalize_top_level_type_alias_typed_address() {
    use cambrian_transpiler::ast::{normalize_program_types, Type};
    let src = r#"
        type WalletAddr = Address<Wallet>
        entity Wallet {}
    "#;
    let mut p = parse(src);
    normalize_program_types(&mut p);
    match &p.type_aliases[0].ty {
        Type::TypedAddress(name) => assert_eq!(name, "Wallet"),
        other => panic!("Expected TypedAddress after normalization, got {:?}", other),
    }
}

// ===================================================================
// Route action types: deploy, throw, call, private, onBounce/recover
// ===================================================================

#[test]
fn parse_deploy_action() {
    let src = "entity Child { routes { init create() => [] } m_x: u64 { in create() => 0 } }\nentity Parent { routes { go() => [deploy Child()] } m_x: u64 {} }";
    let prog = parse(src);
    let parent = prog.entities.iter().find(|e| e.name == "Parent").unwrap();
    let route = &parent.routes[0];
    let actions = route.body.actions();
    assert!(matches!(&actions[0], RouteAction::Deploy { entity, .. } if entity == "Child"));
}

#[test]
fn parse_throw_action() {
    let src = "entity E { routes { go() => [throw 100] } m_x: u64 {} }";
    let prog = parse(src);
    let route = &prog.entities[0].routes[0];
    let actions = route.body.actions();
    assert!(matches!(&actions[0], RouteAction::Throw { error_code: 100 }));
}

#[test]
fn parse_call_route_action() {
    let src = "entity E { routes { private helper() => [] go() => [call helper()] } m_x: u64 { in helper() => 0 } }";
    let prog = parse(src);
    let entity = &prog.entities[0];
    let route = entity.routes.iter().find(|r| r.name == "go").unwrap();
    let actions = route.body.actions();
    assert!(matches!(&actions[0], RouteAction::CallRoute { name, .. } if name == "helper"));
}

#[test]
fn parse_private_route() {
    let src = "entity E { routes { private helper() => [] } m_x: u64 { in helper() => 0 } }";
    let prog = parse(src);
    let route = &prog.entities[0].routes[0];
    assert!(route.is_private);
    assert_eq!(route.name, "helper");
}

#[test]
fn parse_onbounce_route() {
    let src = "entity E { routes { onBounce tag(body: CamData) => [] } m_x: u64 {} }";
    let prog = parse(src);
    let route = &prog.entities[0].routes[0];
    assert_eq!(route.name, "onBounce_tag");
    assert_eq!(route.recover_tag, Some("tag".to_string()));
    assert_eq!(route.params.len(), 1);
    assert_eq!(route.params[0].name, "body");
}

#[test]
fn parse_recover_route() {
    let src = "entity E { routes { recover my_tag(body: CamData) => [] } m_x: u64 {} }";
    let prog = parse(src);
    let route = &prog.entities[0].routes[0];
    assert_eq!(route.name, "recover_my_tag");
    assert_eq!(route.recover_tag, Some("my_tag".to_string()));
}

// ===================================================================
// Import declarations and from clauses
// ===================================================================

#[test]
fn parse_import_declaration() {
    let src = "use gosh\nentity E { routes { go() => [] } m_x: u64 {} }";
    let prog = parse(src);
    assert_eq!(prog.imports.len(), 1);
    assert_eq!(prog.imports[0].namespace, "gosh");
}

#[test]
fn parse_multiple_from_clauses() {
    let src = "entity A { routes { go() => [] } m_x: u64 {} }\nentity B { routes { go() => [] } m_x: u64 {} }\nentity E { routes { handle() from A() | B() => [] } m_x: u64 {} }";
    let prog = parse(src);
    let entity = prog.entities.iter().find(|e| e.name == "E").unwrap();
    let route = &entity.routes[0];
    assert_eq!(route.from_clauses.len(), 2);
    assert_eq!(route.from_clauses[0].entity_name, "A");
    assert_eq!(route.from_clauses[1].entity_name, "B");
}

// ===================================================================
// Namespaced call with turbofish syntax
// ===================================================================

#[test]
fn parse_namespaced_call_turbofish() {
    let src = "use gosh\nentity E { routes { go(c: CamData) => [] } m_s: String { in go(c) => gosh::decode::<String>(c) } }";
    let prog = parse(src);
    assert!(!prog.entities.is_empty());
    let member = &prog.entities[0].members[0];
    match &member.transforms[0].body {
        Expr::NamespacedCall { namespace, name, type_params, .. } => {
            assert_eq!(namespace, "gosh");
            assert_eq!(name, "decode");
            assert_eq!(type_params.len(), 1);
        }
        other => panic!("Expected NamespacedCall, got {:?}", other),
    }
}

// ===================================================================
// normalize_program_types for pure fn params
// ===================================================================

#[test]
fn normalize_pure_fn_params() {
    use cambrian_transpiler::ast::{normalize_program_types, Type};
    let src = "entity Target { routes { go() => [] } m_x: u64 {} }\npure fn helper(a: Address<Target>) -> address { a }";
    let mut prog = parse(src);
    normalize_program_types(&mut prog);
    let f = &prog.pure_fns[0];
    match &f.params[0].ty {
        Type::TypedAddress(name) => assert_eq!(name, "Target"),
        other => panic!("Expected TypedAddress(Target) after normalization, got {:?}", other),
    }
}

// ===================================================================
// Index expression parsing
// ===================================================================

#[test]
fn parse_index_expression() {
    let src = "entity E { routes { go() => [] } m_v: Vec<u64> {} m_x: u64 { in go() => m_v[0] } }";
    let prog = parse(src);
    assert!(!prog.entities.is_empty());
    let member = prog.entities[0].members.iter().find(|m| m.name == "m_x").unwrap();
    match &member.transforms[0].body {
        Expr::Index(_, _) => {}
        other => panic!("Expected Index expression, got {:?}", other),
    }
}

// ===================================================================
// Tuple field access (e.0)
// ===================================================================

#[test]
fn parse_tuple_field_access() {
    let src = "pure fn f(p: (u64, u64)) -> u64 { p.0 }";
    let prog = parse(src);
    assert!(!prog.pure_fns.is_empty());
    match &prog.pure_fns[0].body {
        Expr::FieldAccess(_, field) => assert_eq!(field, "0"),
        other => panic!("Expected FieldAccess(.0), got {:?}", other),
    }
}

// ===================================================================
// Unary operations
// ===================================================================

#[test]
fn parse_unary_neg() {
    let src = "pure fn f(x: i32) -> i32 { -x }";
    let prog = parse(src);
    assert!(!prog.pure_fns.is_empty());
    match &prog.pure_fns[0].body {
        Expr::UnaryOp(UnaryOp::Neg, _) => {}
        other => panic!("Expected UnaryOp(Neg), got {:?}", other),
    }
}

#[test]
fn parse_unary_not() {
    let src = "pure fn f(x: bool) -> bool { !x }";
    let prog = parse(src);
    assert!(!prog.pure_fns.is_empty());
    match &prog.pure_fns[0].body {
        Expr::UnaryOp(UnaryOp::Not, _) => {}
        other => panic!("Expected UnaryOp(Not), got {:?}", other),
    }
}

// ===================================================================
// Route let with if-value
// ===================================================================

#[test]
fn parse_route_let_with_if() {
    let src = "entity E { routes { go(x: u64) => [let y = if x > 0 { x } else { 1 };] } m_v: u64 { in go(x) => x } }";
    let prog = parse(src);
    let route = &prog.entities[0].routes[0];
    assert!(route.body.all_actions().iter().any(|a| matches!(a, RouteAction::Let { .. })));
}

// ===================================================================
// Route let with match-value
// ===================================================================

#[test]
fn parse_route_let_with_match() {
    let src = "entity E { routes { go(x: Option<u64>) => [let y = match x { some(v) => v, none => 0 };] } m_v: u64 { in go(x) => 0 } }";
    let prog = parse(src);
    let route = &prog.entities[0].routes[0];
    assert!(route.body.all_actions().iter().any(|a| matches!(a, RouteAction::Let { .. })));
}
