// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use cambrian_transpiler::ast::*;
use cambrian_transpiler::ProgramParser;

use cambrian_core::U256;
// ===== 1.3.8: Pure function declarations =====

#[test]
fn parse_pure_fn_simple() {
    let src = r#"
        pure fn min(a: uint256, b: uint256) -> uint256 {
            if a <= b { a } else { b }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    assert_eq!(program.pure_fns.len(), 1);
    let f = &program.pure_fns[0];
    assert_eq!(f.name, "min");
    assert_eq!(f.params.len(), 2);
    assert_eq!(f.params[0].name, "a");
    assert_eq!(f.params[0].ty, Type::Simple("uint256".into()));
    assert_eq!(f.return_type, Type::Simple("uint256".into()));
    match &f.body {
        Expr::If(_, _, Some(_)) => {}
        other => panic!("Expected If, got {:?}", other),
    }
}

#[test]
fn parse_pure_fn_no_params() {
    let src = "pure fn zero() -> uint256 { 0 }";
    let program = ProgramParser::new().parse(src).unwrap();
    assert_eq!(program.pure_fns.len(), 1);
    assert_eq!(program.pure_fns[0].params.len(), 0);
    assert_eq!(program.pure_fns[0].body, Expr::IntLiteral(U256::ZERO));
}

#[test]
fn parse_multiple_pure_fns() {
    let src = r#"
        pure fn a() -> bool { true }
        pure fn b(x: uint8) -> uint8 { x }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    assert_eq!(program.pure_fns.len(), 2);
    assert_eq!(program.pure_fns[0].name, "a");
    assert_eq!(program.pure_fns[1].name, "b");
}

// ===== 1.3.9: Entity structure =====

#[test]
fn parse_empty_entity() {
    let src = "entity Wallet {}";
    let program = ProgramParser::new().parse(src).unwrap();
    assert_eq!(program.entities.len(), 1);
    assert_eq!(program.entities[0].name, "Wallet");
}

#[test]
fn parse_entity_with_record() {
    let src = r#"
        entity Wallet {
            record Transaction {
                id: uint64,
                value: uint256,
                dest: address
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let e = &program.entities[0];
    assert_eq!(e.records.len(), 1);
    assert_eq!(e.records[0].name, "Transaction");
    assert_eq!(e.records[0].fields.len(), 3);
    assert_eq!(e.records[0].fields[0].name, "id");
    assert_eq!(e.records[0].fields[2].ty, Type::Simple("address".into()));
}

#[test]
fn parse_entity_with_const() {
    let src = r#"
        entity Wallet {
            const MAX_OWNERS: uint8 = 32
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let e = &program.entities[0];
    assert_eq!(e.constants.len(), 1);
    assert_eq!(e.constants[0].name, "MAX_OWNERS");
    assert_eq!(e.constants[0].value, Expr::IntLiteral(U256::from_u128(32)));
}

#[test]
fn parse_entity_with_macro() {
    let src = r#"
        entity Wallet {
            macro is_confirmed(mask: uint256, index: uint8) -> bool = {
                get_mask_value(mask, index) != 0
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let e = &program.entities[0];
    assert_eq!(e.macros.len(), 1);
    assert_eq!(e.macros[0].name, "is_confirmed");
    assert_eq!(e.macros[0].params.len(), 2);
    assert_eq!(e.macros[0].return_type, Type::Simple("bool".into()));
}

// ===== 1.3.10: Routes =====

#[test]
fn parse_entity_with_simple_route() {
    let src = r#"
        entity Counter {
            routes {
                increment() => []
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let e = &program.entities[0];
    assert_eq!(e.routes.len(), 1);
    assert_eq!(e.routes[0].name, "increment");
    assert_eq!(e.routes[0].params.len(), 0);
    assert!(e.routes[0].body.is_empty());
}

#[test]
fn parse_route_with_where_clause() {
    let src = r#"
        entity Wallet {
            routes {
                withdraw(amount: uint256)
                    where amount > 0 : throw 100
                    && m_balance >= amount : throw 101
                    => []
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert_eq!(r.name, "withdraw");
    assert_eq!(r.where_clauses.len(), 2);
    assert_eq!(r.where_clauses[0].error_code, 100);
    assert_eq!(r.where_clauses[1].error_code, 101);
}

#[test]
fn parse_route_with_where_block() {
    let src = r#"
        entity Wallet {
            routes {
                withdraw(amount: uint256)
                    where {
                        amount > 0 : throw 100,
                        m_balance >= amount : throw 101,
                        m_active == true : throw 102
                    }
                    => []
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert_eq!(r.where_clauses.len(), 3);
    assert_eq!(r.where_clauses[0].error_code, 100);
    assert_eq!(r.where_clauses[1].error_code, 101);
    assert_eq!(r.where_clauses[2].error_code, 102);
}

#[test]
fn parse_route_with_return_type() {
    let src = r#"
        entity Wallet {
            routes {
                getBalance() -> uint256 => [
                    return(m_balance)
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert_eq!(r.return_type, Some(Type::Simple("uint256".into())));
    assert_eq!(r.body.actions().len(), 1);
    match &r.body.actions()[0] {
        RouteAction::Return { values } => {
            assert_eq!(values.len(), 1);
        }
        other => panic!("Expected Return, got {:?}", other),
    }
}

#[test]
fn parse_route_with_send() {
    let src = r#"
        entity Wallet {
            routes {
                transfer(to: address, amount: uint256) => [
                    send(amount) ~> to
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert_eq!(r.body.actions().len(), 1);
    match &r.body.actions()[0] {
        RouteAction::Send { message, args, dest, send_options } => {
            assert_eq!(message, &Some("send".to_string()));
            assert_eq!(args.len(), 1);
            assert_eq!(*dest, Expr::Ident("to".into()));
            assert!(send_options.is_none());
        }
        other => panic!("Expected Send, got {:?}", other),
    }
}

#[test]
fn parse_route_with_send_struct_literal() {
    let src = r#"
        entity Wallet {
            routes {
                transfer(to: address, amount: U256) => [
                    PaymentReleased(amount) ~> to with Opts { value: amount, bounce: true }
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert_eq!(r.body.actions().len(), 1);
    match &r.body.actions()[0] {
        RouteAction::Send { message, send_options, .. } => {
            assert_eq!(message, &Some("PaymentReleased".to_string()));
            match send_options.as_ref().unwrap() {
                Expr::RecordConstruct(name, fields) => {
                    assert_eq!(name, "Opts");
                    assert_eq!(fields.len(), 2);
                    assert_eq!(fields[0].0, "value");
                    assert_eq!(fields[1].0, "bounce");
                }
                other => panic!("Expected RecordConstruct, got {:?}", other),
            }
        }
        other => panic!("Expected Send, got {:?}", other),
    }
}

#[test]
fn parse_nameless_send_plain_transfer() {
    let src = r#"
        entity Wallet {
            routes {
                withdraw(to: address, amount: u64) => [
                    ~> to with {value: amount}
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert_eq!(r.body.actions().len(), 1);
    match &r.body.actions()[0] {
        RouteAction::Send { message, args, dest, send_options } => {
            assert_eq!(message, &None);
            assert!(args.is_empty());
            assert_eq!(*dest, Expr::Ident("to".into()));
            assert!(send_options.is_some());
        }
        other => panic!("Expected Send, got {:?}", other),
    }
}

#[test]
fn parse_nameless_send_no_options() {
    let src = r#"
        entity Wallet {
            routes {
                withdraw(to: address) => [
                    ~> to
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert_eq!(r.body.actions().len(), 1);
    match &r.body.actions()[0] {
        RouteAction::Send { message, args, dest, send_options } => {
            assert_eq!(message, &None);
            assert!(args.is_empty());
            assert_eq!(*dest, Expr::Ident("to".into()));
            assert!(send_options.is_none());
        }
        other => panic!("Expected Send, got {:?}", other),
    }
}

#[test]
fn parse_effect_tvm_raw_reserve() {
    let src = r#"
        entity Wallet {
            routes {
                withdraw(to: address, amount: u64) => [
                    gosh::rawReserve(sys::balance - amount, 0)
                    ~> to with {value: 0, flags: 128}
                ]
            }
            m_balance: u64 {
                in withdraw(_, amount) => m_balance - amount
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert_eq!(r.body.actions().len(), 2);
    match &r.body.actions()[0] {
        RouteAction::Effect { namespace, name, args } => {
            assert_eq!(namespace, "gosh");
            assert_eq!(name, "rawReserve");
            assert_eq!(args.len(), 2);
        }
        other => panic!("Expected Effect, got {:?}", other),
    }
}

#[test]
fn parse_effect_conditional() {
    let src = r#"
        entity W {
            routes {
                withdraw(to: address, amount: u64) => [
                    if amount > 0 => [
                        gosh::rawReserve(amount, 0)
                    ]
                    ~> to with {value: amount}
                ]
            }
            m_x: u64 { in withdraw(_, amount) => m_x - amount }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert_eq!(r.body.actions().len(), 2);
    match &r.body.actions()[0] {
        RouteAction::Conditional { then_actions, .. } => {
            assert_eq!(then_actions.len(), 1);
            match &then_actions[0] {
                RouteAction::Effect { namespace, name, .. } => {
                    assert_eq!(namespace, "gosh");
                    assert_eq!(name, "rawReserve");
                }
                other => panic!("Expected Effect inside conditional, got {:?}", other),
            }
        }
        other => panic!("Expected Conditional, got {:?}", other),
    }
}

#[test]
fn parse_multiple_effects() {
    let src = r#"
        entity W {
            routes {
                act(to: address) => [
                    gosh::rawReserve(100, 0)
                    gosh::rawReserve(200, 2)
                    ~> to with {value: 0, flags: 128}
                ]
            }
            m_x: u64 {}
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert_eq!(r.body.actions().len(), 3, "should have 2 effects + 1 send");
    assert!(matches!(&r.body.actions()[0], RouteAction::Effect { .. }));
    assert!(matches!(&r.body.actions()[1], RouteAction::Effect { .. }));
    assert!(matches!(&r.body.actions()[2], RouteAction::Send { .. }));
}

#[test]
fn parse_effect_custom_namespace() {
    let src = r#"
        entity W {
            routes {
                act() => [
                    custom::doSomething(1, 2, 3)
                ]
            }
            m_x: u64 {}
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    match &r.body.actions()[0] {
        RouteAction::Effect { namespace, name, args } => {
            assert_eq!(namespace, "custom");
            assert_eq!(name, "doSomething");
            assert_eq!(args.len(), 3);
        }
        other => panic!("Expected Effect, got {:?}", other),
    }
}

#[test]
fn parse_route_with_send_variable() {
    let src = r#"
        entity Escrow {
            routes {
                release() => [
                    let opts = 42;
                    PaymentReleased(100) ~> m_seller with opts
                ]
            }
            m_seller: address = 0 {}
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    match &r.body.actions()[1] {
        RouteAction::Send { send_options, .. } => {
            assert_eq!(*send_options, Some(Expr::Ident("opts".into())));
        }
        other => panic!("Expected Send, got {:?}", other),
    }
}

#[test]
fn parse_route_with_send_record_update() {
    let src = r#"
        entity Escrow {
            routes {
                release() => [
                    PaymentReleased(100) ~> m_seller with defaults { bounce: false }
                ]
            }
            m_seller: address = 0 {}
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    match &r.body.actions()[0] {
        RouteAction::Send { send_options, .. } => {
            match send_options.as_ref().unwrap() {
                Expr::RecordUpdate(_, fields) => {
                    assert_eq!(fields.len(), 1);
                    assert_eq!(fields[0].0, "bounce");
                    assert_eq!(fields[0].1, Expr::BoolLiteral(false));
                }
                other => panic!("Expected RecordUpdate, got {:?}", other),
            }
        }
        other => panic!("Expected Send, got {:?}", other),
    }
}

#[test]
fn parse_route_with_conditional() {
    let src = r#"
        entity Wallet {
            routes {
                process() => [
                    if m_ready => [
                        execute() ~> m_target
                    ] else [
                        queue() ~> m_fallback
                    ]
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert_eq!(r.body.actions().len(), 1);
    match &r.body.actions()[0] {
        RouteAction::Conditional { then_actions, else_actions, .. } => {
            assert_eq!(then_actions.len(), 1);
            assert_eq!(else_actions.len(), 1);
        }
        other => panic!("Expected Conditional, got {:?}", other),
    }
}

// ===== 1.3.11: Member transformations =====

#[test]
fn parse_member_with_transform() {
    let src = r#"
        entity Counter {
            m_count: uint256 {
                in increment() => m_count + 1
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let e = &program.entities[0];
    assert_eq!(e.members.len(), 1);
    assert_eq!(e.members[0].name, "m_count");
    assert_eq!(e.members[0].ty, Type::Simple("uint256".into()));
    assert_eq!(e.members[0].transforms.len(), 1);
    let t = &e.members[0].transforms[0];
    assert_eq!(t.route_name, "increment");
    assert_eq!(t.body, Expr::BinOp(
        Box::new(Expr::Ident("m_count".into())),
        BinOp::Add,
        Box::new(Expr::IntLiteral(U256::from_u128(1))),
    ));
}

#[test]
fn parse_member_multiple_transforms() {
    let src = r#"
        entity Counter {
            m_count: uint256 {
                in increment() => m_count + 1
                in reset() => 0
                in add(amount) => m_count + amount
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let m = &program.entities[0].members[0];
    assert_eq!(m.transforms.len(), 3);
    assert_eq!(m.transforms[0].route_name, "increment");
    assert_eq!(m.transforms[1].route_name, "reset");
    assert_eq!(m.transforms[2].route_name, "add");
}

#[test]
fn parse_member_with_default_value() {
    let src = r#"
        entity Counter {
            routes { increment() => [] }
            m_count: u64 = 100 {
                in increment() => m_count + 1
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let m = &program.entities[0].members[0];
    assert_eq!(m.name, "m_count");
    assert_eq!(m.default_value, Some(Expr::IntLiteral(U256::from_u128(100))));
    assert_eq!(m.transforms.len(), 1);
}

#[test]
fn parse_member_without_default_value() {
    let src = r#"
        entity Counter {
            routes { increment() => [] }
            m_count: u64 {
                in increment() => m_count + 1
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let m = &program.entities[0].members[0];
    assert_eq!(m.default_value, None);
}

#[test]
fn parse_member_default_string() {
    let src = r#"
        entity E {
            m_name: String = "default" {}
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let m = &program.entities[0].members[0];
    assert_eq!(m.default_value, Some(Expr::StringLiteral("default".into())));
}

#[test]
fn parse_member_default_bool() {
    let src = r#"
        entity E {
            m_active: bool = true {}
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    assert_eq!(program.entities[0].members[0].default_value, Some(Expr::BoolLiteral(true)));
}

#[test]
fn parse_member_with_complex_body() {
    let src = r#"
        entity Wallet {
            m_balance: uint256 {
                in constructor(initial) => initial
                in deposit(amount) => m_balance + amount
                in withdraw(amount) => m_balance - amount
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let m = &program.entities[0].members[0];
    assert_eq!(m.transforms.len(), 3);
}

// ===== 1.3.12: Full program =====

#[test]
fn parse_full_program_fns_and_entities() {
    let src = r#"
        pure fn max(a: uint256, b: uint256) -> uint256 {
            if a >= b { a } else { b }
        }

        entity Counter {
            routes {
                increment() => []
                reset() => []
                getCount() -> uint256 => [
                    return(m_count)
                ]
            }

            m_count: uint256 {
                in increment() => m_count + 1
                in reset() => 0
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    assert_eq!(program.pure_fns.len(), 1);
    assert_eq!(program.entities.len(), 1);
    let e = &program.entities[0];
    assert_eq!(e.routes.len(), 3);
    assert_eq!(e.members.len(), 1);
}

#[test]
fn parse_program_with_comments() {
    let src = r#"
        // A simple counter
        pure fn id(x: uint256) -> uint256 { x }

        // The counter entity
        entity Counter {
            // Routes section
            routes {
                increment() => [] // increments
            }
            // State
            m_count: uint256 {
                in increment() => m_count + 1
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    assert_eq!(program.pure_fns.len(), 1);
    assert_eq!(program.entities.len(), 1);
}

#[test]
fn parse_multi_entity_file() {
    let src = r#"
        entity A {
            routes { setup() => [] }
            m_x: u64 { in setup() => 0 }
        }
        entity B {
            routes { start() => [] }
            m_y: u64 { in start() => 1 }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    assert_eq!(program.entities.len(), 2);
    assert_eq!(program.entities[0].name, "A");
    assert_eq!(program.entities[1].name, "B");
}

#[test]
fn parse_pure_route() {
    let src = r#"
        entity Calculator {
            routes {
                pure add(a: u64, b: u64) -> u64 => [
                    return(a + b)
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert_eq!(r.name, "add");
    assert!(r.is_pure);
    assert!(!r.is_view);
    assert!(r.from_clauses.is_empty());
    assert!(r.where_clauses.is_empty());
    assert_eq!(r.return_type, Some(Type::Simple("u64".into())));
    assert_eq!(r.body.actions().len(), 1);
}

#[test]
fn parse_pure_route_no_return() {
    let src = r#"
        entity Logger {
            routes {
                pure compute(x: u64) => []
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert!(r.is_pure);
    assert!(r.return_type.is_none());
}

#[test]
fn parse_mixed_view_pure_routes() {
    let src = r#"
        entity Calc {
            routes {
                view getState() -> u64 => [
                    return(m_val)
                ]
                pure add(a: u64, b: u64) -> u64 => [
                    return(a + b)
                ]
                set(val: u64) => []
            }
            m_val: u64 {
                in set(val) => val
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let routes = &program.entities[0].routes;
    assert_eq!(routes.len(), 3);
    assert!(routes[0].is_view);
    assert!(!routes[0].is_pure);
    assert!(!routes[1].is_view);
    assert!(routes[1].is_pure);
    assert!(!routes[2].is_view);
    assert!(!routes[2].is_pure);
}

#[test]
fn parse_top_level_record() {
    let src = r#"
        record Token {
            name: String,
            symbol: String,
            decimals: u8
        }
        entity Registry {
            routes { register() => [] }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    assert_eq!(program.records.len(), 1);
    assert_eq!(program.records[0].name, "Token");
    assert_eq!(program.records[0].fields.len(), 3);
    assert_eq!(program.entities.len(), 1);
}

#[test]
fn parse_top_level_enum() {
    let src = r#"
        enum Status {
            Active,
            Paused,
            Stopped
        }
        entity Controller {
            routes { setup() => [] }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    assert_eq!(program.enums.len(), 1);
    assert_eq!(program.enums[0].name, "Status");
    assert_eq!(program.enums[0].variants.len(), 3);
    assert_eq!(program.entities.len(), 1);
}

#[test]
fn parse_top_level_record_and_enum_with_entity() {
    let src = r#"
        type Amount = U256
        
        record Pool {
            token_a: address,
            token_b: address,
            reserve: Amount
        }
        
        enum PoolStatus {
            Active,
            Frozen
        }
        
        entity Dex {
            routes { setup() => [] }
            m_status: u8 { in setup() => 0 }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    assert_eq!(program.type_aliases.len(), 1);
    assert_eq!(program.records.len(), 1);
    assert_eq!(program.records[0].name, "Pool");
    assert_eq!(program.enums.len(), 1);
    assert_eq!(program.enums[0].name, "PoolStatus");
    assert_eq!(program.entities.len(), 1);
}

#[test]
fn parse_entity_local_type_alias() {
    let src = r#"
        entity Token {
            type Amount = U256
            type TokenId = u64
            routes { setup() => [] }
            m_supply: U256 { in setup() => 0 }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let e = &program.entities[0];
    assert_eq!(e.type_aliases.len(), 2);
    assert_eq!(e.type_aliases[0].name, "Amount");
    assert_eq!(e.type_aliases[0].ty, Type::Simple("U256".into()));
    assert_eq!(e.type_aliases[1].name, "TokenId");
}

// ===== Phased routes =====

#[test]
fn parse_phased_route_body_single_phase() {
    let src = r#"
        entity E {
            routes {
                foo(b: u64) => [
                    commit: [
                        gosh::commit()
                    ]
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert!(r.body.is_phased());
    let phases = r.body.phases().unwrap();
    assert_eq!(phases.len(), 1);
    assert_eq!(phases[0].name, "commit");
    assert_eq!(phases[0].actions.len(), 1);
    assert!(matches!(&phases[0].actions[0], RouteAction::Effect { namespace, name, .. }
        if namespace == "gosh" && name == "commit"));
}

#[test]
fn parse_phased_route_body_multiple_phases() {
    let src = r#"
        entity E {
            routes {
                foo(b: u64) => [
                    save: [
                        gosh::commit()
                    ]
                    finish: [
                        gosh::exit(0)
                    ]
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert!(r.body.is_phased());
    let phases = r.body.phases().unwrap();
    assert_eq!(phases.len(), 2);
    assert_eq!(phases[0].name, "save");
    assert_eq!(phases[1].name, "finish");
    assert_eq!(phases[0].actions.len(), 1);
    assert_eq!(phases[1].actions.len(), 1);
}

#[test]
fn parse_phased_route_body_empty_phase() {
    let src = r#"
        entity E {
            routes {
                foo() => [
                    setup: []
                    teardown: [
                        gosh::commit()
                    ]
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert!(r.body.is_phased());
    let phases = r.body.phases().unwrap();
    assert_eq!(phases.len(), 2);
    assert_eq!(phases[0].name, "setup");
    assert_eq!(phases[0].actions.len(), 0);
    assert_eq!(phases[1].name, "teardown");
    assert_eq!(phases[1].actions.len(), 1);
}

#[test]
fn parse_phased_route_body_with_sends() {
    let src = r#"
        entity E {
            routes {
                pay(to: address, amount: u128) => [
                    reserve: [
                        gosh::rawReserve(100, 0)
                    ]
                    send: [
                        transfer(amount) ~> to
                    ]
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert!(r.body.is_phased());
    let phases = r.body.phases().unwrap();
    assert_eq!(phases.len(), 2);
    assert_eq!(phases[0].name, "reserve");
    assert!(matches!(&phases[0].actions[0], RouteAction::Effect { .. }));
    assert_eq!(phases[1].name, "send");
    assert!(matches!(&phases[1].actions[0], RouteAction::Send { .. }));
}

#[test]
fn parse_unphased_route_still_works() {
    let src = r#"
        entity E {
            routes {
                foo(b: u64) => [
                    gosh::commit()
                    gosh::exit(0)
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert!(!r.body.is_phased());
    assert_eq!(r.body.actions().len(), 2);
}

#[test]
fn parse_phased_member_transform() {
    let src = r#"
        entity E {
            routes { foo(b: u64) => [] }
            m_a: u64 {
                in foo(b) => setup: b
                in foo(b) => done: b + 1
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let m = &program.entities[0].members[0];
    assert_eq!(m.transforms.len(), 2);
    assert_eq!(m.transforms[0].phase, Some("setup".to_string()));
    assert_eq!(m.transforms[1].phase, Some("done".to_string()));
}

#[test]
fn parse_phased_member_transform_block_body() {
    let src = r#"
        entity E {
            routes { foo(b: u64) => [] }
            m_a: u64 {
                in foo(b) => setup: { b + 1 }
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let m = &program.entities[0].members[0];
    assert_eq!(m.transforms.len(), 1);
    assert_eq!(m.transforms[0].phase, Some("setup".to_string()));
}

#[test]
fn parse_multi_phase_member_transform_single_clause() {
    let src = r#"
        entity E {
            routes { foo(x: u64) => [ a: [] b: [] ] }
            m_val: u64 {
                in foo(x) =>
                    a: x
                    b: x + 1
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let m = &program.entities[0].members[0];
    assert_eq!(m.transforms.len(), 2);
    assert_eq!(m.transforms[0].phase, Some("a".to_string()));
    assert_eq!(m.transforms[0].route_name, "foo");
    assert_eq!(m.transforms[1].phase, Some("b".to_string()));
    assert_eq!(m.transforms[1].route_name, "foo");
}

#[test]
fn parse_multi_phase_member_transform_block_bodies() {
    let src = r#"
        entity E {
            routes { foo(x: u64) => [ cleanup: [] execute: [] ] }
            m_val: u64 {
                in foo(x) =>
                    cleanup: { x * 2 }
                    execute: { m_val + 1 }
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let m = &program.entities[0].members[0];
    assert_eq!(m.transforms.len(), 2);
    assert_eq!(m.transforms[0].phase, Some("cleanup".to_string()));
    assert_eq!(m.transforms[1].phase, Some("execute".to_string()));
}

#[test]
fn parse_multi_phase_member_transform_three_tags() {
    let src = r#"
        entity E {
            routes { foo(x: u64) => [ a: [] b: [] c: [] ] }
            m_val: u64 {
                in foo(x) =>
                    a: x
                    b: { x + 1 }
                    c: m_val
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let m = &program.entities[0].members[0];
    assert_eq!(m.transforms.len(), 3);
    assert_eq!(m.transforms[0].phase, Some("a".to_string()));
    assert_eq!(m.transforms[1].phase, Some("b".to_string()));
    assert_eq!(m.transforms[2].phase, Some("c".to_string()));
}

#[test]
fn parse_multi_phase_mixed_with_unphased() {
    let src = r#"
        entity E {
            routes {
                foo(x: u64) => [ a: [] b: [] ]
                bar(y: u64) => []
            }
            m_val: u64 {
                in foo(x) =>
                    a: x
                    b: x + 1
                in bar(y) => y
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let m = &program.entities[0].members[0];
    assert_eq!(m.transforms.len(), 3);
    assert_eq!(m.transforms[0].phase, Some("a".to_string()));
    assert_eq!(m.transforms[1].phase, Some("b".to_string()));
    assert_eq!(m.transforms[2].phase, None);
    assert_eq!(m.transforms[2].route_name, "bar");
}

#[test]
fn parse_multi_phase_params_shared() {
    let src = r#"
        entity E {
            routes { foo(x: u64, y: u64) => [ first: [] second: [] ] }
            m_val: u64 {
                in foo(x, y) =>
                    first: x + y
                    second: m_val * 2
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let m = &program.entities[0].members[0];
    assert_eq!(m.transforms.len(), 2);
    assert_eq!(m.transforms[0].params.len(), 2);
    assert_eq!(m.transforms[1].params.len(), 2);
}

#[test]
fn parse_unphased_member_transform_still_works() {
    let src = r#"
        entity E {
            routes { foo(b: u64) => [] }
            m_a: u64 {
                in foo(b) => b + 1
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let m = &program.entities[0].members[0];
    assert_eq!(m.transforms.len(), 1);
    assert_eq!(m.transforms[0].phase, None);
}

#[test]
fn parse_phased_route_three_phases() {
    let src = r#"
        entity E {
            routes {
                process() => [
                    begin: []
                    work: [
                        gosh::commit()
                    ]
                    cleanup: [
                        gosh::exit(0)
                    ]
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert!(r.body.is_phased());
    let phases = r.body.phases().unwrap();
    assert_eq!(phases.len(), 3);
    assert_eq!(phases[0].name, "begin");
    assert_eq!(phases[1].name, "work");
    assert_eq!(phases[2].name, "cleanup");
}

#[test]
fn parse_phased_route_with_conditional() {
    let src = r#"
        entity E {
            routes {
                foo(x: u64) => [
                    check: [
                        if x > 0 => [
                            gosh::commit()
                        ]
                    ]
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert!(r.body.is_phased());
    let phases = r.body.phases().unwrap();
    assert_eq!(phases[0].actions.len(), 1);
    assert!(matches!(&phases[0].actions[0], RouteAction::Conditional { .. }));
}

#[test]
fn parse_phased_route_five_phases() {
    let src = r#"
        entity E {
            routes {
                pipeline() => [
                    p1: [ gosh::rawReserve(100, 0) ]
                    p2: []
                    p3: [ gosh::commit() ]
                    p4: []
                    p5: [ gosh::exit(0) ]
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let phases = program.entities[0].routes[0].body.phases().unwrap();
    assert_eq!(phases.len(), 5);
    assert_eq!(phases.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        vec!["p1", "p2", "p3", "p4", "p5"]);
}

#[test]
fn parse_phased_route_phase_with_let_binding() {
    let src = r#"
        entity E {
            routes {
                foo(x: u64) => [
                    calc: [
                        let y = x + 1;
                        gosh::commit()
                    ]
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let phases = program.entities[0].routes[0].body.phases().unwrap();
    assert_eq!(phases[0].actions.len(), 2);
    assert!(matches!(&phases[0].actions[0], RouteAction::Let { .. }));
    assert!(matches!(&phases[0].actions[1], RouteAction::Effect { .. }));
}

#[test]
fn parse_phased_route_phase_with_multiple_effects() {
    let src = r#"
        entity E {
            routes {
                foo() => [
                    multi: [
                        gosh::rawReserve(100, 0)
                        gosh::commit()
                        gosh::exit(0)
                    ]
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let phases = program.entities[0].routes[0].body.phases().unwrap();
    assert_eq!(phases[0].actions.len(), 3);
}

#[test]
fn parse_phased_route_with_where_clause() {
    let src = r#"
        entity E {
            routes {
                withdraw(amt: u128)
                    where m_balance >= amt : throw 100
                => [
                    deduct: []
                    send: [ ~> m_owner ]
                ]
            }
            m_balance: u128 {}
            m_owner: address {}
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert_eq!(r.where_clauses.len(), 1);
    assert!(r.body.is_phased());
    assert_eq!(r.body.phases().unwrap().len(), 2);
}

#[test]
fn parse_phased_init_route() {
    let src = r#"
        entity E {
            routes {
                init setup(owner: address) => [
                    configure: []
                ]
            }
            m_owner: address {
                in setup(owner) => configure: owner
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert!(r.is_init);
    assert!(r.body.is_phased());
}

#[test]
fn parse_phased_view_route() {
    let src = r#"
        entity E {
            routes {
                view balance() -> u128 => [
                    compute: [
                        return(m_balance)
                    ]
                ]
            }
            m_balance: u128 {}
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert!(r.is_view);
    assert!(r.body.is_phased());
    let phases = r.body.phases().unwrap();
    assert_eq!(phases[0].name, "compute");
}

#[test]
fn parse_phased_member_transform_ident_expr() {
    let src = r#"
        entity E {
            routes { foo(b: u64) => [ step: [] ] }
            m_a: u64 {
                in foo(b) => step: m_a
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let t = &program.entities[0].members[0].transforms[0];
    assert_eq!(t.phase, Some("step".to_string()));
    assert!(matches!(&t.body, Expr::Ident(name) if name == "m_a"));
}

#[test]
fn parse_phased_member_transform_complex_expr() {
    let src = r#"
        entity E {
            routes { foo(x: u64, y: u64) => [ calc: [] ] }
            m_val: u64 {
                in foo(x, y) => calc: { if x > y { x - y } else { y - x } }
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let t = &program.entities[0].members[0].transforms[0];
    assert_eq!(t.phase, Some("calc".to_string()));
    assert!(matches!(&t.body, Expr::If(_, _, _)));
}

#[test]
fn parse_phased_member_multiple_routes_mixed() {
    let src = r#"
        entity E {
            routes {
                foo(x: u64) => [ step: [] ]
                bar(y: u64) => []
            }
            m_val: u64 {
                in foo(x) => step: x
                in bar(y) => y
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let transforms = &program.entities[0].members[0].transforms;
    assert_eq!(transforms[0].phase, Some("step".to_string()));
    assert_eq!(transforms[1].phase, None);
}

#[test]
fn parse_phased_route_empty_body() {
    let src = r#"
        entity E {
            routes { foo() => [] }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    assert!(!program.entities[0].routes[0].body.is_phased());
    assert!(program.entities[0].routes[0].body.is_empty());
}

#[test]
fn parse_phased_route_nested_conditional_in_phase() {
    let src = r#"
        entity E {
            routes {
                foo(x: u64, y: u64) => [
                    check: [
                        if x > 0 => [
                            if y > 0 => [
                                gosh::commit()
                            ]
                        ]
                    ]
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let phases = program.entities[0].routes[0].body.phases().unwrap();
    assert_eq!(phases[0].actions.len(), 1);
    match &phases[0].actions[0] {
        RouteAction::Conditional { then_actions, .. } => {
            assert!(matches!(&then_actions[0], RouteAction::Conditional { .. }));
        }
        _ => panic!("expected nested conditional"),
    }
}

#[test]
fn parse_phased_accept_route() {
    let src = r#"
        entity E {
            routes {
                accept pay(amount: u128) => [
                    receive: [ gosh::commit() ]
                ]
            }
            m_bal: u128 {
                in pay(amount) => receive: m_bal + amount
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let r = &program.entities[0].routes[0];
    assert!(r.is_accept);
    assert!(r.body.is_phased());
}

#[test]
fn parse_phased_route_phase_with_send_with_options() {
    let src = r#"
        entity E {
            routes {
                pay(to: address, amt: u128) => [
                    transfer: [
                        transfer(amt) ~> to with {value: amt, bounce: true}
                    ]
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let phases = program.entities[0].routes[0].body.phases().unwrap();
    assert_eq!(phases[0].actions.len(), 1);
    match &phases[0].actions[0] {
        RouteAction::Send { send_options, .. } => assert!(send_options.is_some()),
        _ => panic!("expected send with options"),
    }
}

#[test]
fn parse_phased_member_transform_with_block_body_arithmetic() {
    let src = r#"
        entity E {
            routes { go(x: u64, y: u64) => [ step: [] ] }
            m_val: u64 {
                in go(x, y) => step: {
                    let z = x + y;
                    z * 2
                }
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let t = &program.entities[0].members[0].transforms[0];
    assert_eq!(t.phase, Some("step".to_string()));
    assert!(matches!(&t.body, Expr::Let(_, _, _)), "Expected Let expr, got {:?}", t.body);
}

// ===== Identity members =====

#[test]
fn parse_identity_member_simple() {
    let src = r#"
        entity Vault {
            routes { constructor() => [] }
            identity m_id: u64
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let entity = &program.entities[0];
    assert_eq!(entity.members.len(), 1);
    let m = &entity.members[0];
    assert_eq!(m.name, "m_id");
    assert_eq!(m.ty, Type::Simple("u64".into()));
    assert!(m.is_identity);
    assert_eq!(m.default_value, None);
    assert!(m.transforms.is_empty());
}

#[test]
fn parse_identity_member_with_regular_members() {
    let src = r#"
        entity Vault {
            routes {
                constructor(initial_balance: u64) => []
            }
            identity m_id: u64
            m_balance: u64 {
                in constructor(initial_balance) => initial_balance
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let entity = &program.entities[0];
    assert_eq!(entity.members.len(), 2);
    assert!(entity.members[0].is_identity);
    assert_eq!(entity.members[0].name, "m_id");
    assert!(!entity.members[1].is_identity);
    assert_eq!(entity.members[1].name, "m_balance");
    assert_eq!(entity.members[1].transforms.len(), 1);
}

#[test]
fn parse_multiple_identity_members() {
    let src = r#"
        entity MultiId {
            routes { constructor() => [] }
            identity m_owner: address
            identity m_nonce: u64
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let entity = &program.entities[0];
    assert_eq!(entity.members.len(), 2);
    assert!(entity.members[0].is_identity);
    assert_eq!(entity.members[0].name, "m_owner");
    assert_eq!(entity.members[0].ty, Type::Simple("address".into()));
    assert!(entity.members[1].is_identity);
    assert_eq!(entity.members[1].name, "m_nonce");
}

#[test]
fn parse_identity_member_complex_type() {
    let src = r#"
        entity E {
            identity m_key: pubkey
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let m = &program.entities[0].members[0];
    assert!(m.is_identity);
    assert_eq!(m.ty, Type::Simple("pubkey".into()));
}

#[test]
fn parse_regular_member_not_identity() {
    let src = r#"
        entity Counter {
            routes { increment() => [] }
            m_count: u64 {
                in increment() => m_count + 1
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    assert!(!program.entities[0].members[0].is_identity);
}

// ===========================================================================
// EVM-12 Batch E: action-level `for` grammar
// ===========================================================================

#[test]
fn parse_action_for_send_round_trips() {
    let src = r#"
        entity Airdrop {
            routes {
                airdrop(amount: uint256, recipients: Vec<address>) => [
                    for r in recipients => [
                        ~> r with {value: amount}
                    ]
                ]
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let actions = program.entities[0].routes[0].body.actions();
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        RouteAction::For { pattern, iter, body } => {
            match pattern {
                Pattern::Ident(n) => assert_eq!(n, "r"),
                other => panic!("Expected Pattern::Ident(\"r\"), got {:?}", other),
            }
            assert_eq!(*iter, Expr::Ident("recipients".into()));
            assert_eq!(body.len(), 1);
            match &body[0] {
                RouteAction::Send { dest, send_options, .. } => {
                    assert_eq!(*dest, Expr::Ident("r".into()));
                    assert!(send_options.is_some());
                }
                other => panic!("Expected nameless Send inside loop, got {:?}", other),
            }
        }
        other => panic!("Expected RouteAction::For, got {:?}", other),
    }
}

#[test]
fn parse_action_for_tuple_pattern_over_iter() {
    let src = r#"
        entity Reg {
            routes {
                broadcast(payload: uint256) => [
                    for (k, v) in m_data.iter() => [
                        notify(v) ~> k
                    ]
                ]
            }
            m_data: HashMap<address, uint256> {}
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let actions = program.entities[0].routes[0].body.actions();
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        RouteAction::For { pattern, body, .. } => {
            match pattern {
                Pattern::Tuple(parts) => {
                    assert_eq!(parts.len(), 2);
                    match (&parts[0], &parts[1]) {
                        (Pattern::Ident(a), Pattern::Ident(b)) => {
                            assert_eq!(a, "k");
                            assert_eq!(b, "v");
                        }
                        other => panic!("Expected (Ident, Ident), got {:?}", other),
                    }
                }
                other => panic!("Expected Pattern::Tuple, got {:?}", other),
            }
            assert_eq!(body.len(), 1);
        }
        other => panic!("Expected RouteAction::For, got {:?}", other),
    }
}

#[test]
fn parse_action_for_range_with_state_writes() {
    let src = r#"
        entity Counter {
            routes {
                bump_n(n: uint256) => [
                    for _ in 0..n => [
                    ]
                ]
            }
            m_count: uint256 {
                in bump_n(n) => m_count + n
            }
        }
    "#;
    let program = ProgramParser::new().parse(src).unwrap();
    let actions = program.entities[0].routes[0].body.actions();
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        RouteAction::For { pattern, iter, body } => {
            assert!(matches!(pattern, Pattern::Wildcard));
            assert!(matches!(iter, Expr::Range(_, _)));
            assert!(body.is_empty());
        }
        other => panic!("Expected RouteAction::For, got {:?}", other),
    }
}
