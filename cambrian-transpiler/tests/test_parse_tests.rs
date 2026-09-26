// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use cambrian_transpiler::ast::*;
use cambrian_transpiler::ProgramParser;

use cambrian_core::U256;
fn parse(src: &str) -> Program {
    ProgramParser::new().parse(src)
        .unwrap_or_else(|e| panic!("Parse failed:\n{}\nError: {}", src, e))
}

#[test]
fn parse_minimal_test() {
    let p = parse(r#"
        entity Counter {
            routes { increment(amount: u64) => [] }
            m_count: u64 { in increment(amount) => m_count + amount }
        }

        test "increment adds" for Counter with { m_count: 5 } {
            call increment(3)
            expect state { m_count: 8 }
        }
    "#);
    assert_eq!(p.tests.len(), 1);
    let t = &p.tests[0];
    assert_eq!(t.name, "increment adds");
    assert_eq!(t.entity_name, "Counter");
    assert_eq!(t.init_state.len(), 1);
    assert_eq!(t.init_state[0].0, "m_count");
    assert_eq!(t.body.len(), 2);
    assert!(matches!(&t.body[0], TestStep::Call { target: None, route, args } if route == "increment" && args.len() == 1));
    assert!(matches!(&t.body[1], TestStep::ExpectState { fields } if fields.len() == 1));
}

#[test]
fn parse_test_without_with() {
    let p = parse(r#"
        entity Counter {
            routes { getCount() -> u64 => [return(m_count)] }
            m_count: u64 {}
        }

        test "default count" for Counter {
            call getCount()
            expect return 0
        }
    "#);
    assert_eq!(p.tests.len(), 1);
    let t = &p.tests[0];
    assert!(t.init_state.is_empty());
    assert_eq!(t.body.len(), 2);
    assert!(matches!(&t.body[1], TestStep::ExpectReturn { .. }));
}

#[test]
fn parse_test_expect_throw() {
    let p = parse(r#"
        entity Counter {
            routes { increment(amount: u64) => [] }
            m_count: u64 { in increment(amount) => m_count + amount }
        }

        test "rejects zero" for Counter with { m_count: 0 } {
            call increment(0)
            expect throw 100
        }
    "#);
    let t = &p.tests[0];
    assert!(matches!(&t.body[1], TestStep::ExpectThrow { code: 100 }));
}

#[test]
fn parse_test_expect_effects_empty() {
    let p = parse(r#"
        entity Counter {
            routes { increment(amount: u64) => [] }
            m_count: u64 { in increment(amount) => m_count + amount }
        }

        test "no effects" for Counter with { m_count: 0 } {
            call increment(1)
            expect effects []
        }
    "#);
    let t = &p.tests[0];
    assert!(matches!(&t.body[1], TestStep::ExpectEffects { elements } if elements.is_empty()));
}

#[test]
fn parse_test_expect_effects_with_items() {
    let p = parse(r#"
        entity Vault {
            routes { withdraw(amount: u64) => [] }
            m_balance: u64 { in withdraw(amount) => m_balance - amount }
        }

        test "withdraw effects" for Vault with { m_balance: 1000 } {
            call withdraw(100)
            expect effects [rawReserve(500000, 0)]
        }
    "#);
    let t = &p.tests[0];
    match &t.body[1] {
        TestStep::ExpectEffects { elements } => {
            assert_eq!(elements.len(), 1);
            assert!(matches!(&elements[0], TestEffectElement::Effect(TestEffect::PlatformEffect { name, args }) if name == "rawReserve" && args.len() == 2));
        }
        other => panic!("expected ExpectEffects, got {:?}", other),
    }
}

#[test]
fn parse_test_expect_effects_partial() {
    let p = parse(r#"
        entity Vault {
            routes { withdraw(amount: u64) => [] }
            m_balance: u64 { in withdraw(amount) => m_balance - amount }
        }

        test "partial effects" for Vault with { m_balance: 1000 } {
            call withdraw(100)
            expect effects [rawReserve(500000, 0), ..]
        }
    "#);
    let t = &p.tests[0];
    match &t.body[1] {
        TestStep::ExpectEffects { elements } => {
            assert_eq!(elements.len(), 2);
            assert!(matches!(&elements[0], TestEffectElement::Effect(TestEffect::PlatformEffect { .. })));
            assert!(matches!(&elements[1], TestEffectElement::Wildcard));
        }
        other => panic!("expected ExpectEffects, got {:?}", other),
    }
}

#[test]
fn parse_test_with_typed_let() {
    let p = parse(r#"
        entity Token {
            routes { transfer(to: address, amount: u64) => [] }
            m_balance: u64 { in transfer(to, amount) => m_balance - amount }
        }

        test "typed let" for Token {
            let bob: address = 0x0000000000000000000000000000000000000000000000000000000000000002
            call transfer(bob, 100)
        }
    "#);
    match &p.tests[0].body[0] {
        TestStep::Let { name, ty, .. } => {
            assert_eq!(name, "bob");
            assert!(matches!(ty, Some(cambrian_transpiler::ast::Type::Simple(s)) if s == "address"));
        }
        other => panic!("expected Let, got {:?}", other),
    }
}

#[test]
fn parse_test_with_let() {
    let p = parse(r#"
        entity Token {
            routes { transfer(to: address, amount: u64) => [] }
            m_balance: u64 { in transfer(to, amount) => m_balance - amount }
        }

        test "transfer with let" for Token with { m_balance: 1000 } {
            let bob = 0x0000000000000000000000000000000000000000000000000000000000000002
            call transfer(bob, 100)
            expect state { m_balance: 900 }
        }
    "#);
    let t = &p.tests[0];
    assert_eq!(t.body.len(), 3);
    match &t.body[0] {
        TestStep::Let { name, ty: None, value } => {
            assert_eq!(name, "bob");
            assert!(matches!(value, Expr::IntLiteral(_)));
        }
        other => panic!("expected Let, got {:?}", other),
    }
}

#[test]
fn parse_test_with_msg_context() {
    let p = parse(r#"
        entity Vault {
            routes { deposit(amount: u64) => [] }
            m_balance: u64 { in deposit(amount) => m_balance + amount }
        }

        test "deposit with sender" for Vault with { m_balance: 0 } {
            msg { sender: 0x0000000000000000000000000000000000000000000000000000000000001234, value: 100 }
            call deposit(100)
            expect state { m_balance: 100 }
        }
    "#);
    let t = &p.tests[0];
    assert_eq!(t.body.len(), 3);
    match &t.body[0] {
        TestStep::SetContext { namespace, fields } => {
            assert_eq!(namespace, "msg");
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].0, "sender");
            assert_eq!(fields[1].0, "value");
        }
        other => panic!("expected SetContext, got {:?}", other),
    }
}

#[test]
fn parse_test_with_sys_context() {
    let p = parse(r#"
        entity Vault {
            routes { deposit(amount: u64) => [] }
            m_balance: u64 { in deposit(amount) => m_balance + amount }
        }

        test "deposit with sys" for Vault with { m_balance: 0 } {
            sys { now: 1000, balance: 5000000 }
            call deposit(100)
            expect state { m_balance: 100 }
        }
    "#);
    let t = &p.tests[0];
    match &t.body[0] {
        TestStep::SetContext { namespace, fields } => {
            assert_eq!(namespace, "sys");
            assert_eq!(fields.len(), 2);
        }
        other => panic!("expected SetContext for sys, got {:?}", other),
    }
}

#[test]
fn parse_test_multistep() {
    let p = parse(r#"
        entity Counter {
            routes { increment(amount: u64) => [] }
            m_count: u64 { in increment(amount) => m_count + amount }
        }

        test "multi-step" for Counter with { m_count: 0 } {
            call increment(10)
            expect state { m_count: 10 }

            call increment(5)
            expect state { m_count: 15 }
        }
    "#);
    let t = &p.tests[0];
    assert_eq!(t.body.len(), 4);
    assert!(matches!(&t.body[0], TestStep::Call { route, .. } if route == "increment"));
    assert!(matches!(&t.body[1], TestStep::ExpectState { .. }));
    assert!(matches!(&t.body[2], TestStep::Call { route, .. } if route == "increment"));
    assert!(matches!(&t.body[3], TestStep::ExpectState { .. }));
}

#[test]
fn parse_multiple_tests() {
    let p = parse(r#"
        entity Counter {
            routes {
                increment(amount: u64) => []
                reset() => []
            }
            m_count: u64 {
                in increment(amount) => m_count + amount
                in reset() => 0
            }
        }

        test "inc test" for Counter with { m_count: 0 } {
            call increment(5)
            expect state { m_count: 5 }
        }

        test "reset test" for Counter with { m_count: 42 } {
            call reset()
            expect state { m_count: 0 }
        }
    "#);
    assert_eq!(p.tests.len(), 2);
    assert_eq!(p.tests[0].name, "inc test");
    assert_eq!(p.tests[1].name, "reset test");
}

#[test]
fn parse_test_with_hashmap_init() {
    let p = parse(r#"
        entity Token {
            routes { transfer(to: address, amount: u64) => [] }
            m_balances: mapping<address, u64> {
                in transfer(to, amount) => m_balances
            }
        }

        test "transfer between addresses" for Token with {
            m_balances: { alice => 1000, bob => 0 }
        } {
            let alice = 0x0000000000000000000000000000000000000000000000000000000000000001
            let bob = 0x0000000000000000000000000000000000000000000000000000000000000002
            call transfer(bob, 100)
            expect state { m_balances: { alice => 900, bob => 100 } }
        }
    "#);
    let t = &p.tests[0];
    assert_eq!(t.init_state.len(), 1);
    assert_eq!(t.init_state[0].0, "m_balances");
    match &t.init_state[0].1 {
        Expr::RecordConstruct(name, fields) => {
            assert_eq!(name, "__HashMap");
            assert_eq!(fields.len(), 2);
        }
        other => panic!("expected HashMap literal, got {:?}", other),
    }
}

// ===== Phase 8: Lens / FieldPath tests =====

#[test]
fn parse_expect_state_simple_field_as_fieldpath() {
    let p = parse(r#"
        entity Counter {
            routes { increment(amount: u64) => [] }
            m_count: u64 { in increment(amount) => m_count + amount }
        }

        test "simple field" for Counter with { m_count: 5 } {
            call increment(3)
            expect state { m_count: 8 }
        }
    "#);
    let t = &p.tests[0];
    match &t.body[1] {
        TestStep::ExpectState { fields } => {
            assert_eq!(fields.len(), 1);
            let (path, _val) = &fields[0];
            assert_eq!(path.len(), 1);
            assert!(matches!(&path[0], PathSegment::Field(name) if name == "m_count"));
        }
        other => panic!("expected ExpectState, got {:?}", other),
    }
}

#[test]
fn parse_expect_state_dotted_field_path() {
    let p = parse(r#"
        entity Dex {
            routes { addLiquidity(amount: u64) => [] }
            m_pool: u64 { in addLiquidity(amount) => m_pool + amount }
        }

        test "dotted path" for Dex with { m_pool: 0 } {
            call addLiquidity(100)
            expect state { m_pool.reserve_a: 100 }
        }
    "#);
    let t = &p.tests[0];
    match &t.body[1] {
        TestStep::ExpectState { fields } => {
            assert_eq!(fields.len(), 1);
            let (path, _val) = &fields[0];
            assert_eq!(path.len(), 2);
            assert!(matches!(&path[0], PathSegment::Field(n) if n == "m_pool"));
            assert!(matches!(&path[1], PathSegment::Field(n) if n == "reserve_a"));
        }
        other => panic!("expected ExpectState, got {:?}", other),
    }
}

#[test]
fn parse_expect_state_indexed_field_path() {
    let p = parse(r#"
        entity Token {
            routes { transfer(to: address, amount: u64) => [] }
            m_balances: mapping<address, u64> { in transfer(to, amount) => m_balances }
        }

        test "indexed path" for Token with { m_balances: { alice => 1000 } } {
            let alice = 0x0000000000000000000000000000000000000000000000000000000000000001
            call transfer(alice, 100)
            expect state { m_balances[alice]: 900 }
        }
    "#);
    let t = &p.tests[0];
    let expect_steps: Vec<_> = t.body.iter().filter(|s| matches!(s, TestStep::ExpectState { .. })).collect();
    assert_eq!(expect_steps.len(), 1);
    match &expect_steps[0] {
        TestStep::ExpectState { fields } => {
            assert_eq!(fields.len(), 1);
            let (path, _val) = &fields[0];
            assert_eq!(path.len(), 2);
            assert!(matches!(&path[0], PathSegment::Field(n) if n == "m_balances"));
            assert!(matches!(&path[1], PathSegment::Index(Expr::Ident(n)) if n == "alice"));
        }
        _ => unreachable!(),
    }
}

#[test]
fn parse_expect_state_deep_path() {
    let p = parse(r#"
        entity Dex {
            routes { addLiquidity(pool_id: u64, amount: u64) => [] }
            m_pools: mapping<u64, u64> { in addLiquidity(pool_id, amount) => m_pools }
        }

        test "deep path" for Dex with { m_pools: { 0 => 0 } } {
            call addLiquidity(0, 1000)
            expect state { m_pools[0].reserve_a: 1000 }
        }
    "#);
    let t = &p.tests[0];
    let expect_steps: Vec<_> = t.body.iter().filter(|s| matches!(s, TestStep::ExpectState { .. })).collect();
    match &expect_steps[0] {
        TestStep::ExpectState { fields } => {
            let (path, _) = &fields[0];
            assert_eq!(path.len(), 3);
            assert!(matches!(&path[0], PathSegment::Field(n) if n == "m_pools"));
            assert!(matches!(&path[1], PathSegment::Index(Expr::IntLiteral(v)) if *v == U256::from_u128(0)));
            assert!(matches!(&path[2], PathSegment::Field(n) if n == "reserve_a"));
        }
        _ => unreachable!(),
    }
}

#[test]
fn parse_expect_state_tuple_index() {
    let p = parse(r#"
        entity Pair {
            routes { set(a: u64, b: u64) => [] }
            m_pair: u64 { in set(a, b) => a }
        }

        test "tuple index" for Pair with { m_pair: 0 } {
            call set(10, 20)
            expect state { m_pair.0: 10 }
        }
    "#);
    let t = &p.tests[0];
    match &t.body[1] {
        TestStep::ExpectState { fields } => {
            let (path, _) = &fields[0];
            assert_eq!(path.len(), 2);
            assert!(matches!(&path[0], PathSegment::Field(n) if n == "m_pair"));
            assert!(matches!(&path[1], PathSegment::TupleIndex(0)));
        }
        other => panic!("expected ExpectState, got {:?}", other),
    }
}

#[test]
fn parse_expect_return_tuple() {
    let p = parse(r#"
        entity Pair {
            routes { get() -> (u64, u64) => [return((m_a, m_b))] }
            m_a: u64 {}
            m_b: u64 {}
        }

        test "return tuple" for Pair with { m_a: 10, m_b: 20 } {
            call get()
            expect return (10, 20)
        }
    "#);
    let t = &p.tests[0];
    match &t.body[1] {
        TestStep::ExpectReturnTuple { values } => {
            assert_eq!(values.len(), 2);
            assert!(matches!(&values[0], Expr::IntLiteral(v) if *v == U256::from_u128(10)));
            assert!(matches!(&values[1], Expr::IntLiteral(v) if *v == U256::from_u128(20)));
        }
        other => panic!("expected ExpectReturnTuple, got {:?}", other),
    }
}

#[test]
fn parse_expect_return_lens() {
    let p = parse(r#"
        entity Pair {
            routes { get() -> (u64, u64) => [return((m_a, m_b))] }
            m_a: u64 {}
            m_b: u64 {}
        }

        test "return lens" for Pair with { m_a: 10, m_b: 20 } {
            call get()
            expect return.0 == 10
        }
    "#);
    let t = &p.tests[0];
    match &t.body[1] {
        TestStep::ExpectReturnLens { path, value } => {
            assert_eq!(path.len(), 1);
            assert!(matches!(&path[0], PathSegment::TupleIndex(0)));
            assert!(matches!(value, Expr::IntLiteral(v) if *v == U256::from_u128(10)));
        }
        other => panic!("expected ExpectReturnLens, got {:?}", other),
    }
}

#[test]
fn parse_expect_return_lens_deep() {
    let p = parse(r#"
        entity Dex {
            routes { getPool() -> u64 => [return(0)] }
            m_x: u64 {}
        }

        test "return lens deep" for Dex with { m_x: 0 } {
            call getPool()
            expect return.pool.reserve_a == 1000
        }
    "#);
    let t = &p.tests[0];
    match &t.body[1] {
        TestStep::ExpectReturnLens { path, value } => {
            assert_eq!(path.len(), 2);
            assert!(matches!(&path[0], PathSegment::Field(n) if n == "pool"));
            assert!(matches!(&path[1], PathSegment::Field(n) if n == "reserve_a"));
            assert!(matches!(value, Expr::IntLiteral(v) if *v == U256::from_u128(1000)));
        }
        other => panic!("expected ExpectReturnLens, got {:?}", other),
    }
}

#[test]
fn parse_expect_state_multiple_paths() {
    let p = parse(r#"
        entity Dex {
            routes { swap(amount: u64) => [] }
            m_pools: mapping<u64, u64> { in swap(amount) => m_pools }
            m_count: u64 { in swap(amount) => m_count + 1 }
        }

        test "multiple paths" for Dex with { m_count: 0 } {
            call swap(100)
            expect state {
                m_pools[0].reserve_a: 1000,
                m_pools[0].reserve_b: 500,
                m_count: 1
            }
        }
    "#);
    let t = &p.tests[0];
    match &t.body[1] {
        TestStep::ExpectState { fields } => {
            assert_eq!(fields.len(), 3);
            assert_eq!(fields[2].0.len(), 1);
            assert!(matches!(&fields[2].0[0], PathSegment::Field(n) if n == "m_count"));
        }
        other => panic!("expected ExpectState, got {:?}", other),
    }
}

#[test]
fn parse_expect_return_scalar_still_works() {
    let p = parse(r#"
        entity Counter {
            routes { getCount() -> u64 => [return(m_count)] }
            m_count: u64 {}
        }

        test "scalar return" for Counter with { m_count: 42 } {
            call getCount()
            expect return 42
        }
    "#);
    let t = &p.tests[0];
    assert!(matches!(&t.body[1], TestStep::ExpectReturn { value: Expr::IntLiteral(v) } if *v == U256::from_u128(42)));
}

// ===== Phase 8: E2E codegen tests =====

#[test]
fn parse_escrow_v2_with_tests() {
    let src = std::fs::read_to_string("../contracts/escrow_v2.cam")
        .expect("escrow_v2.cam not found");
    let test_src = std::fs::read_to_string("../contracts/escrow_v2.test.cam")
        .expect("escrow_v2.test.cam not found");
    let combined = format!("{}\n{}", src, test_src);
    let p = parse(&combined);

    assert_eq!(p.entities.len(), 1);
    assert_eq!(p.entities[0].name, "EscrowV2");
    assert!(p.tests.len() >= 4, "expected at least 4 tests, got {}", p.tests.len());

    // Check enum variant assertion in "constructor" test
    let t0 = &p.tests[0];
    assert_eq!(t0.name, "constructor sets Created state");
    let expect_steps: Vec<_> = t0.body.iter()
        .filter(|s| matches!(s, TestStep::ExpectState { .. }))
        .collect();
    assert_eq!(expect_steps.len(), 1);
    match &expect_steps[0] {
        TestStep::ExpectState { fields } => {
            assert!(fields.len() >= 2);
            // m_state: State::Created
            let (path0, val0) = &fields[0];
            assert!(matches!(&path0[0], PathSegment::Field(n) if n == "m_state"));
            assert!(matches!(val0, Expr::EnumVariant(e, v) if e == "State" && v == "Created"));
            // m_resolution: none
            let (path1, val1) = &fields[1];
            assert!(matches!(&path1[0], PathSegment::Field(n) if n == "m_resolution"));
            assert!(matches!(val1, Expr::None));
        }
        _ => unreachable!(),
    }

    // Check return lens test (find by name since ordering may change)
    let t3 = p.tests.iter().find(|t| t.name == "getStatus returns tuple")
        .expect("test 'getStatus returns tuple' not found");
    let ret_lens_steps: Vec<_> = t3.body.iter()
        .filter(|s| matches!(s, TestStep::ExpectReturnLens { .. }))
        .collect();
    assert_eq!(ret_lens_steps.len(), 1);
    match &ret_lens_steps[0] {
        TestStep::ExpectReturnLens { path, value } => {
            assert_eq!(path.len(), 1);
            assert!(matches!(&path[0], PathSegment::TupleIndex(0)));
            assert!(matches!(value, Expr::IntLiteral(v) if *v == U256::from_u128(0)));
        }
        _ => unreachable!(),
    }
}

// ===== Phase 8.9: Vec return lenses =====

#[test]
fn parse_expect_return_len() {
    let p = parse(r#"
        entity Registry {
            routes { getAll() -> u64 => [return(0)] }
            m_x: u64 {}
        }

        test "return len" for Registry with { m_x: 0 } {
            call getAll()
            expect return.len == 3
        }
    "#);
    let t = &p.tests[0];
    match &t.body[1] {
        TestStep::ExpectReturnLens { path, value } => {
            assert_eq!(path.len(), 1);
            assert!(matches!(&path[0], PathSegment::Field(n) if n == "len"));
            assert!(matches!(value, Expr::IntLiteral(v) if *v == U256::from_u128(3)));
        }
        other => panic!("expected ExpectReturnLens, got {:?}", other),
    }
}

#[test]
fn parse_expect_return_index() {
    let p = parse(r#"
        entity Registry {
            routes { getAll() -> u64 => [return(0)] }
            m_x: u64 {}
        }

        test "return index" for Registry with { m_x: 0 } {
            call getAll()
            expect return[0] == 42
        }
    "#);
    let t = &p.tests[0];
    match &t.body[1] {
        TestStep::ExpectReturnLens { path, value } => {
            assert_eq!(path.len(), 1);
            assert!(matches!(&path[0], PathSegment::Index(Expr::IntLiteral(v)) if *v == U256::from_u128(0)));
            assert!(matches!(value, Expr::IntLiteral(v) if *v == U256::from_u128(42)));
        }
        other => panic!("expected ExpectReturnLens, got {:?}", other),
    }
}

#[test]
fn parse_expect_return_index_deep() {
    let p = parse(r#"
        entity Registry {
            routes { getAll() -> u64 => [return(0)] }
            m_x: u64 {}
        }

        test "return index deep" for Registry with { m_x: 0 } {
            call getAll()
            expect return[0].name == 42
        }
    "#);
    let t = &p.tests[0];
    match &t.body[1] {
        TestStep::ExpectReturnLens { path, value } => {
            assert_eq!(path.len(), 2);
            assert!(matches!(&path[0], PathSegment::Index(Expr::IntLiteral(v)) if *v == U256::from_u128(0)));
            assert!(matches!(&path[1], PathSegment::Field(n) if n == "name"));
            assert!(matches!(value, Expr::IntLiteral(v) if *v == U256::from_u128(42)));
        }
        other => panic!("expected ExpectReturnLens, got {:?}", other),
    }
}

#[test]
fn parse_expect_return_index_ident() {
    let p = parse(r#"
        entity Registry {
            routes { getAll() -> u64 => [return(0)] }
            m_x: u64 {}
        }

        test "return index ident" for Registry with { m_x: 0 } {
            let idx = 0
            call getAll()
            expect return[idx] == 42
        }
    "#);
    let t = &p.tests[0];
    let ret_lens: Vec<_> = t.body.iter()
        .filter(|s| matches!(s, TestStep::ExpectReturnLens { .. }))
        .collect();
    assert_eq!(ret_lens.len(), 1);
    match &ret_lens[0] {
        TestStep::ExpectReturnLens { path, .. } => {
            assert_eq!(path.len(), 1);
            assert!(matches!(&path[0], PathSegment::Index(Expr::Ident(n)) if n == "idx"));
        }
        _ => unreachable!(),
    }
}

#[test]
fn parse_test_from_fixture() {
    let src = std::fs::read_to_string("../contracts/counter.cam")
        .expect("counter.cam not found");
    let test_src = std::fs::read_to_string("../contracts/counter.test.cam")
        .expect("counter.test.cam not found");
    let combined = format!("{}\n{}", src, test_src);
    let p = parse(&combined);
    assert_eq!(p.entities.len(), 1);
    assert_eq!(p.entities[0].name, "Counter");
    assert!(p.tests.len() >= 4, "expected at least 4 tests, got {}", p.tests.len());
}

#[test]
fn parse_vault_test_with_skip_from() {
    let src = std::fs::read_to_string("../contracts/guardian_vault/vault.cam")
        .expect("vault.cam not found");
    let test_src = std::fs::read_to_string("../contracts/guardian_vault/vault.test.cam")
        .expect("vault.test.cam not found");
    let combined = format!("{}\n{}", src, test_src);
    let p = parse(&combined);

    assert!(p.tests.len() >= 4, "should have at least 4 tests");
    assert!(p.tests[0].skip_from, "first test should have skip_from");
    assert!(p.tests[1].skip_from, "second test should have skip_from");
    assert!(!p.tests[2].skip_from, "third test should NOT have skip_from");
    assert!(!p.tests[3].skip_from, "fourth test should NOT have skip_from");
}

#[test]
fn parse_registry_step() {
    let src = r#"
        entity Vault {
            identity m_id: u64
            routes {
                constructor() => []
                acceptPing(sender_pubkey: pubkey) from Guardian(m_id) with {pubkey: sender_pubkey} => []
            }
            m_ping_count: u64 {
                in constructor() => 0
                in acceptPing(_) => m_ping_count + 1
            }
        }

        entity Guardian {
            identity m_id: u64
            routes { constructor() => [] }
        }

        test "with registry" for Vault with { m_id: 1 } {
            registry Guardian { code_hash: 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa, code_depth: 1, wasm_hash: 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb }
            call constructor()
            expect state { m_ping_count: 0 }
        }
    "#;
    let p = parse(src);
    assert_eq!(p.tests.len(), 1);
    assert!(!p.tests[0].skip_from);
    let registry_steps: Vec<_> = p.tests[0].body.iter()
        .filter(|s| matches!(s, TestStep::SetRegistry { .. }))
        .collect();
    assert_eq!(registry_steps.len(), 1, "should have one registry step");
    match &registry_steps[0] {
        TestStep::SetRegistry { entity_name, .. } => {
            assert_eq!(entity_name, "Guardian");
        }
        _ => unreachable!(),
    }
}

// ===========================================================================
// Fixture-based tests: parse + codegen for real .cam contracts with skip from
// ===========================================================================

fn parse_fixture_with_tests(cam: &str, test_cam: &str) -> cambrian_transpiler::ast::Program {
    let src = std::fs::read_to_string(format!("../contracts/{}", cam))
        .unwrap_or_else(|_| panic!("{} not found", cam));
    let test_src = std::fs::read_to_string(format!("../contracts/{}", test_cam))
        .unwrap_or_else(|_| panic!("{} not found", test_cam));
    let combined = format!("{}\n{}", src, test_src);
    parse(&combined)
}

fn parse_project_with_tests(dir: &str, cam_files: &[&str], test_cam: &str) -> cambrian_transpiler::ast::Program {
    let mut combined = String::new();
    for cam in cam_files {
        let src = std::fs::read_to_string(format!("{}/{}", dir, cam))
            .unwrap_or_else(|_| panic!("{}/{} not found", dir, cam));
        combined.push_str(&src);
        combined.push('\n');
    }
    let test_src = std::fs::read_to_string(format!("{}/{}", dir, test_cam))
        .unwrap_or_else(|_| panic!("{}/{} not found", dir, test_cam));
    combined.push_str(&test_src);
    parse(&combined)
}

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_broker_tests() {
    let p = parse_fixture_with_tests("marketplace/broker.cam", "marketplace/broker.test.cam");
    assert!(p.tests.len() >= 3, "broker should have at least 3 tests, got {}", p.tests.len());
    assert!(p.tests.iter().all(|t| t.skip_from), "all broker tests should have skip_from");
    assert_codegen_ok(&p, "broker_entity_wasm");
}

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_ledger_tests() {
    let p = parse_fixture_with_tests("marketplace/ledger.cam", "marketplace/ledger.test.cam");
    assert!(p.tests.len() >= 4, "ledger should have at least 4 tests, got {}", p.tests.len());
    assert!(p.tests.iter().all(|t| t.skip_from), "all ledger tests should have skip_from");
    assert_codegen_ok(&p, "ledger_entity_wasm");
}

#[test]
#[cfg(feature = "rust-targets")]
#[ignore = "marketplace.cam uses const-ref in throw (ERROR_SHOP_ZERO_AMOUNT) which grammar doesn't support yet"]
fn parse_and_codegen_marketplace_tests() {
    let p = parse_fixture_with_tests("marketplace/marketplace.cam", "marketplace/marketplace.test.cam");
    assert!(p.tests.len() >= 10, "marketplace should have at least 10 tests, got {}", p.tests.len());
    let order_tests: Vec<_> = p.tests.iter().filter(|t| t.entity_name == "Order").collect();
    assert!(order_tests.iter().all(|t| t.skip_from), "Order tests should have skip_from");
    let treasury_tests: Vec<_> = p.tests.iter().filter(|t| t.entity_name == "Treasury").collect();
    assert!(treasury_tests.iter().all(|t| !t.skip_from), "Treasury tests should NOT have skip_from");
    assert_codegen_ok(&p, "marketplace_entity_wasm");
}

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_identity_pair_tests() {
    let p = parse_fixture_with_tests("identity_pair/identity_pair.cam", "identity_pair/identity_pair.test.cam");
    assert!(p.tests.len() >= 4, "identity_pair should have at least 4 tests, got {}", p.tests.len());
    let guardian_tests: Vec<_> = p.tests.iter().filter(|t| t.entity_name == "Guardian").collect();
    assert!(guardian_tests.iter().all(|t| !t.skip_from), "Guardian tests should NOT have skip_from");
    let vault_tests: Vec<_> = p.tests.iter().filter(|t| t.entity_name == "Vault").collect();
    assert!(vault_tests.iter().all(|t| t.skip_from), "Vault tests should have skip_from");
    assert_codegen_ok(&p, "identity_pair_entity_wasm");
}

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_guardian_tests() {
    let p = parse_fixture_with_tests("guardian_vault/guardian.cam", "guardian_vault/guardian.test.cam");
    assert!(p.tests.len() >= 3, "guardian should have at least 3 tests, got {}", p.tests.len());
    assert!(p.tests.iter().all(|t| !t.skip_from), "guardian tests should NOT have skip_from (no from clauses)");
    assert_codegen_ok(&p, "guardian_entity_wasm");
}

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_shop_tests() {
    let p = parse_fixture_with_tests("marketplace/shop.cam", "marketplace/shop.test.cam");
    assert!(p.tests.len() >= 4, "shop should have at least 4 tests, got {}", p.tests.len());
    assert!(p.tests.iter().all(|t| !t.skip_from), "shop tests should NOT have skip_from (no from clauses)");
    assert_codegen_ok(&p, "shop_entity_wasm");
}

// ===========================================================================
// Real contract tests: token, nft, voting, staking, multisig
// ===========================================================================

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_token_tests() {
    let p = parse_fixture_with_tests("token.cam", "token.test.cam");
    assert!(p.tests.len() >= 11, "token should have at least 11 tests, got {}", p.tests.len());
    assert!(p.tests.iter().all(|t| t.entity_name == "Token"), "all tests for Token entity");
    assert_codegen_ok(&p, "token_entity_wasm");
}

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_nft_tests() {
    let p = parse_fixture_with_tests("nft.cam", "nft.test.cam");
    assert!(p.tests.len() >= 10, "nft should have at least 10 tests, got {}", p.tests.len());
    assert!(p.tests.iter().all(|t| t.entity_name == "NftCollection"), "all tests for NftCollection entity");
    assert_codegen_ok(&p, "nft_entity_wasm");
}

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_voting_tests() {
    let p = parse_fixture_with_tests("voting.cam", "voting.test.cam");
    assert!(p.tests.len() >= 7, "voting should have at least 7 tests, got {}", p.tests.len());
    assert!(p.tests.iter().all(|t| t.entity_name == "Voting"), "all tests for Voting entity");
    assert_codegen_ok(&p, "voting_entity_wasm");
}

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_staking_tests() {
    let p = parse_fixture_with_tests("staking.cam", "staking.test.cam");
    assert!(p.tests.len() >= 9, "staking should have at least 9 tests, got {}", p.tests.len());
    assert!(p.tests.iter().all(|t| t.entity_name == "Staking"), "all tests for Staking entity");
    assert_codegen_ok(&p, "staking_entity_wasm");
}

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_multisig_tests() {
    let p = parse_fixture_with_tests("multisig.cam", "multisig.test.cam");
    assert!(p.tests.len() >= 10, "multisig should have at least 10 tests, got {}", p.tests.len());
    assert!(p.tests.iter().all(|t| t.entity_name == "MultisigWallet"), "all tests for MultisigWallet entity");
    assert_codegen_ok(&p, "multisig_entity_wasm");
}

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_bouncer_tests() {
    let p = parse_fixture_with_tests("bouncer.cam", "bouncer.test.cam");
    assert!(p.tests.len() >= 13, "bouncer should have at least 13 tests, got {}", p.tests.len());
    assert!(p.tests.iter().all(|t| t.entity_name == "Bouncer"), "all tests for Bouncer entity");
    assert_codegen_ok(&p, "bouncer_entity_wasm");
}

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_phased_predictable_tests() {
    let p = parse_fixture_with_tests("phased_predictable.cam", "phased_predictable.test.cam");
    assert!(p.tests.len() >= 11, "phased_predictable should have at least 11 tests, got {}", p.tests.len());
    assert!(p.tests.iter().all(|t| t.entity_name == "Escrow"), "all tests for Escrow entity");
    assert_codegen_ok(&p, "escrow_entity_wasm");
}

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_phased_vault_tests() {
    let p = parse_fixture_with_tests("phased_vault.cam", "phased_vault.test.cam");
    assert!(p.tests.len() >= 6, "phased_vault should have at least 6 tests, got {}", p.tests.len());
    assert!(p.tests.iter().all(|t| t.entity_name == "PhasedVault"), "all tests for PhasedVault entity");
    assert_codegen_ok(&p, "phasedvault_entity_wasm");
}

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_upgrade_tests() {
    let p = parse_fixture_with_tests("upgrade.cam", "upgrade.test.cam");
    assert!(p.tests.len() >= 6, "upgrade should have at least 6 tests, got {}", p.tests.len());
    assert!(p.tests.iter().all(|t| t.entity_name == "Upgradeable"), "all tests for Upgradeable entity");
    assert_codegen_ok(&p, "upgradeable_entity_wasm");
}

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_payment_channel_tests() {
    let p = parse_fixture_with_tests("payment_channel.cam", "payment_channel.test.cam");
    assert!(p.tests.len() >= 12, "payment_channel should have at least 12 tests, got {}", p.tests.len());
    assert!(p.tests.iter().all(|t| t.entity_name == "PaymentChannel"), "all tests for PaymentChannel entity");
    assert_codegen_ok(&p, "paymentchannel_entity_wasm");
}

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_wallet_tests() {
    let p = parse_fixture_with_tests("wallet.cam", "wallet.test.cam");
    assert!(p.tests.len() >= 2, "wallet should have at least 2 tests, got {}", p.tests.len());
    assert!(p.tests.iter().all(|t| t.entity_name == "Wallet"), "all tests for Wallet entity");
    assert_codegen_ok(&p, "wallet_entity_wasm");
}

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_registry_tests() {
    let p = parse_fixture_with_tests("registry.cam", "registry.test.cam");
    assert!(p.tests.len() >= 7, "registry should have at least 7 tests, got {}", p.tests.len());
    assert!(p.tests.iter().all(|t| t.entity_name == "Registry"), "all tests for Registry entity");
    assert_codegen_ok(&p, "registry_entity_wasm");
}

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_sellorderlot_tests() {
    let p = parse_project_with_tests(
        "../examples/accumulator",
        &["ShellSellOrderLot.cam"],
        "ShellSellOrderLot.test.cam",
    );
    assert!(p.tests.len() >= 9, "ShellSellOrderLot should have at least 9 tests, got {}", p.tests.len());
    assert!(p.tests.iter().all(|t| t.entity_name == "ShellSellOrderLot"), "all tests for ShellSellOrderLot entity");
    assert_codegen_ok(&p, "shellsellorderlot_entity_wasm");
}

#[cfg(feature = "rust-targets")]
#[test]
fn parse_and_codegen_accumulator_tests() {
    let p = parse_project_with_tests(
        "../examples/accumulator",
        &["ShellAccumulatorRootUSDC.cam"],
        "ShellAccumulatorRootUSDC.test.cam",
    );
    assert!(p.tests.len() >= 14, "ShellAccumulatorRootUSDC should have at least 14 tests, got {}", p.tests.len());
    assert!(p.tests.iter().all(|t| t.entity_name == "ShellAccumulatorRootUSDC"), "all tests for ShellAccumulatorRootUSDC entity");
    assert_codegen_ok(&p, "shellaccumulatorrootusdc_entity_wasm");
}

// ===========================================================================
// Multi-entity (`for { ... }`) invariant grammar + validator
// ===========================================================================

#[test]
fn parse_multi_entity_invariant_basic() {
    let p = parse(r#"
        entity Vault {
            routes { deposit(amount: u64) => [] }
            m_balance: u64 { in deposit(amount) => m_balance + amount }
        }
        entity Treasury {
            routes { credit(amount: u64) => [] }
            m_total: u64 { in credit(amount) => m_total + amount }
        }

        invariant "sum bounded" for { v: Vault, t: Treasury } {
            init v { m_balance: 0 }
            init t { m_total: 0 }

            action v.deposit(amount: u64) { bound amount in 0..1000 }
            action t.credit(amount: u64) { bound amount in 0..1000 }

            check v.m_balance + t.m_total >= 0
        }
    "#);
    assert_eq!(p.invariants.len(), 1);
    let inv = &p.invariants[0];
    assert!(!inv.is_single_entity(), "should parse as multi-instance");
    assert_eq!(inv.instances.len(), 2);
    assert_eq!(inv.instances[0].name, "v");
    assert_eq!(inv.instances[0].entity, "Vault");
    assert_eq!(inv.instances[1].name, "t");
    assert_eq!(inv.instances[1].entity, "Treasury");
    assert_eq!(inv.actions.len(), 2);
    assert_eq!(inv.actions[0].instance, "v");
    assert_eq!(inv.actions[0].route, "deposit");
    assert_eq!(inv.actions[1].instance, "t");
    assert_eq!(inv.actions[1].route, "credit");
}

#[test]
fn parse_single_entity_invariant_desugars_to_self_instance() {
    let p = parse(r#"
        entity Counter {
            routes { increment(amount: u64) => [] }
            m_count: u64 { in increment(amount) => m_count + amount }
        }

        invariant "count bounded" for Counter {
            init { m_count: 0 }
            action increment(amount: u64) { bound amount in 0..100 }
            check m_count >= 0
        }
    "#);
    assert_eq!(p.invariants.len(), 1);
    let inv = &p.invariants[0];
    assert!(inv.is_single_entity(), "single-entity form should desugar to one _self instance");
    assert_eq!(inv.entity_name(), "Counter");
    assert_eq!(inv.actions[0].instance, "_self");
}

#[test]
fn validate_multi_entity_invariant_emits_I7() {
    let p = parse(r#"
        entity Vault {
            routes { deposit(amount: u64) => [] }
            m_balance: u64 { in deposit(amount) => m_balance + amount }
        }

        invariant "trivial" for { v: Vault, w: Vault } {
            action v.deposit(amount: u64) { bound amount in 0..10 }
            check v.m_balance >= 0
        }
    "#);
    // I7 is Domains([Tvm]): only the TVM backend rejects multi-entity invariants.
    let diags = cambrian_transpiler::validate::check_target_compat(
        &p,
        cambrian_transpiler::target::Target::AckiNacki,
        false,
    );
    assert!(diags.iter().any(|d| d.code == "I7"),
        "multi-entity invariant should emit I7 on the Acki Nacki target: {:?}",
        diags.iter().map(|d| (d.code.clone(), d.message.clone())).collect::<Vec<_>>());
}

#[test]
fn validate_duplicate_instance_names_emits_I8() {
    let p = parse(r#"
        entity Vault {
            routes { deposit(amount: u64) => [] }
            m_balance: u64 { in deposit(amount) => m_balance + amount }
        }

        invariant "dup" for { v: Vault, v: Vault } {
            action v.deposit(amount: u64) { bound amount in 0..10 }
            check v.m_balance >= 0
        }
    "#);
    let diags = cambrian_transpiler::validate::validate(&p);
    assert!(diags.iter().any(|d| d.code == "I8"),
        "duplicate instance names should emit I8");
}

#[test]
fn validate_unknown_instance_entity_emits_I9() {
    let p = parse(r#"
        entity Vault {
            routes { deposit(amount: u64) => [] }
            m_balance: u64 { in deposit(amount) => m_balance + amount }
        }

        invariant "bad" for { v: Vault, t: NoSuch } {
            action v.deposit(amount: u64) { bound amount in 0..10 }
            check v.m_balance >= 0
        }
    "#);
    let diags = cambrian_transpiler::validate::validate(&p);
    assert!(diags.iter().any(|d| d.code == "I9"),
        "unknown entity reference should emit I9");
}

#[test]
fn validate_init_unknown_member_emits_I10() {
    let p = parse(r#"
        entity Vault {
            routes { deposit(amount: u64) => [] }
            m_balance: u64 { in deposit(amount) => m_balance + amount }
        }

        invariant "bad init" for { v: Vault, w: Vault } {
            init v { not_a_member: 0 }
            action v.deposit(amount: u64) { bound amount in 0..10 }
            check v.m_balance >= 0
        }
    "#);
    let diags = cambrian_transpiler::validate::validate(&p);
    assert!(diags.iter().any(|d| d.code == "I10"),
        "unknown init field in multi-instance form should emit I10");
}

#[test]
fn validate_bare_member_in_multi_invariant_check_emits_I11() {
    let p = parse(r#"
        entity Vault {
            routes { deposit(amount: u64) => [] }
            m_balance: u64 { in deposit(amount) => m_balance + amount }
        }

        invariant "bare" for { v: Vault, w: Vault } {
            action v.deposit(amount: u64) { bound amount in 0..10 }
            check m_balance >= 0
        }
    "#);
    let diags = cambrian_transpiler::validate::validate(&p);
    assert!(diags.iter().any(|d| d.code == "I11"),
        "bare member reference in multi-instance check should emit I11");
}

#[test]
fn validate_trace_count_unknown_route_emits_I15() {
    let p = parse(r#"
        entity Counter {
            routes {
                increment(amount: u64) => []
                reset() => []
            }
            m_count: u64 {
                in increment(amount) => m_count + amount
                in reset() => 0
            }
        }

        invariant "bad trace route" for Counter {
            action increment(amount: u64) {
                bound amount in 0..10
                assume trace::count(nonexistent) >= 0
            }
            action reset() { }
            check m_count >= 0
        }
    "#);
    let diags = cambrian_transpiler::validate::validate(&p);
    assert!(diags.iter().any(|d| d.code == "I15"),
        "trace::count over an undeclared action route should emit I15: {:?}", diags);
}

#[test]
fn validate_trace_unknown_accessor_name_emits_I15() {
    let p = parse(r#"
        entity Counter {
            routes { increment(amount: u64) => [] reset() => [] }
            m_count: u64 {
                in increment(amount) => m_count + amount
                in reset() => 0
            }
        }

        invariant "bad accessor" for Counter {
            action increment(amount: u64) {
                bound amount in 0..10
                assume trace::bogus(increment) >= 0
            }
            action reset() { }
            check m_count >= 0
        }
    "#);
    let diags = cambrian_transpiler::validate::validate(&p);
    assert!(diags.iter().any(|d| d.code == "I15"),
        "unknown trace:: accessor name should emit I15: {:?}", diags);
}

#[test]
fn validate_trace_in_bound_emits_I16() {
    let p = parse(r#"
        entity Counter {
            routes { increment(amount: u64) => [] reset() => [] }
            m_count: u64 {
                in increment(amount) => m_count + amount
                in reset() => 0
            }
        }

        invariant "trace in bound" for Counter {
            action increment(amount: u64) {
                bound amount in 0..trace::length
            }
            action reset() { }
            check m_count >= 0
        }
    "#);
    let diags = cambrian_transpiler::validate::validate(&p);
    assert!(diags.iter().any(|d| d.code == "I16"),
        "trace:: in a bound endpoint should emit I16: {:?}", diags);
}

#[test]
fn validate_trace_in_skip_if_emits_I16() {
    let p = parse(r#"
        entity Counter {
            routes { increment(amount: u64) => [] reset() => [] }
            m_count: u64 {
                in increment(amount) => m_count + amount
                in reset() => 0
            }
        }

        invariant "trace in skip" for Counter {
            action increment(amount: u64) {
                bound amount in 0..10
                skip if trace::lastWas(increment)
            }
            action reset() { }
            check m_count >= 0
        }
    "#);
    let diags = cambrian_transpiler::validate::validate(&p);
    assert!(diags.iter().any(|d| d.code == "I16"),
        "trace:: in a skip if condition should emit I16: {:?}", diags);
}

#[test]
fn validate_trace_in_assume_and_check_is_accepted() {
    let p = parse(r#"
        entity Counter {
            routes { increment(amount: u64) => [] reset() => [] }
            m_count: u64 {
                in increment(amount) => m_count + amount
                in reset() => 0
            }
        }

        invariant "trace ok" for Counter {
            action increment(amount: u64) {
                bound amount in 1..10
                assume trace::length < 5
                assume trace::count(increment) <= trace::count(reset) + 3
            }
            action reset() {
                assume !trace::lastWas(reset)
            }
            check m_count >= 0
            check trace::count(increment) >= trace::count(reset)
        }
    "#);
    let diags = cambrian_transpiler::validate::validate(&p);
    assert!(!diags.iter().any(|d| d.code == "I15" || d.code == "I16"),
        "valid trace:: usage should not emit I15/I16: {:?}", diags);
}

#[test]
fn invariant_missing_entity_does_not_spam_action_i9() {
    let p = parse(r#"
        invariant "ghost" for Ghost {
            action ping() {}
            action pong() {}
            check true
        }
    "#);
    let diags = cambrian_transpiler::validate::validate(&p);
    let t1: Vec<_> = diags.iter().filter(|d| d.code == "T1" && d.suppressed_by.is_none()).collect();
    assert_eq!(t1.len(), 1, "expected one primary T1, got {:?}", diags);
    let actionable_i9 = diags
        .iter()
        .filter(|d| d.code == "I9" && d.suppressed_by.is_none())
        .count();
    assert_eq!(actionable_i9, 0, "dependent I9 must be suppressed, got {:?}", diags);
    let suppressed = diags.iter().filter(|d| d.suppressed_by.as_deref() == Some("T1")).count();
    assert_eq!(suppressed, 2, "two actions should yield two suppressed I9, got {:?}", diags);
}

#[test]
fn multi_entity_unknown_entity_suppresses_action_i9() {
    let p = parse(r#"
        entity Vault {
            routes { deposit(amount: u64) => [] }
            m_balance: u64 { in deposit(amount) => m_balance + amount }
        }
        invariant "bad" for { v: Vault, t: NoSuch } {
            action v.deposit(amount: u64) { bound amount in 0..10 }
            action t.deposit(amount: u64) { bound amount in 0..10 }
            check v.m_balance >= 0
        }
    "#);
    let diags = cambrian_transpiler::validate::validate(&p);
    let primary_i9 = diags
        .iter()
        .filter(|d| d.code == "I9" && d.suppressed_by.is_none())
        .count();
    assert_eq!(primary_i9, 1, "one unknown-entity I9, got {:?}", diags);
    assert!(
        diags.iter().any(|d| d.code == "I9" && d.suppressed_by.as_deref() == Some("I9")),
        "action on unbound instance must be suppressed: {:?}",
        diags
    );
}

fn is_int(expr: &Expr, n: u128) -> bool {
    matches!(expr, Expr::IntLiteral(v) if *v == U256::from_u128(n))
}

fn is_result(expr: &Expr) -> bool {
    matches!(expr, Expr::Ident(name) if name == "result")
}

#[test]
fn parse_expect_return_relop_is_predicate() {
    let p = parse(r#"
        entity Counter {
            routes { get() -> u64 => [return(m_count)] }
            m_count: u64 {}
        }
        test "rel" for Counter {
            call get()
            expect return != 0
            expect return > 1
            expect return.0 > 1
            expect return > 0 && m_count >= 1 || flag
        }
    "#);
    let body = &p.tests[0].body;
    match &body[1] {
        TestStep::ExpectPred { cond } => match cond {
            Expr::BinOp(l, BinOp::Ne, r) => {
                assert!(is_result(l));
                assert!(is_int(r, 0));
            }
            other => panic!("expected != predicate, got {other:?}"),
        },
        other => panic!("expected ExpectPred, got {other:?}"),
    }
    match &body[2] {
        TestStep::ExpectPred { cond } => match cond {
            Expr::BinOp(l, BinOp::Gt, r) => {
                assert!(is_result(l));
                assert!(is_int(r, 1));
            }
            other => panic!("expected > predicate, got {other:?}"),
        },
        other => panic!("expected ExpectPred, got {other:?}"),
    }
    match &body[3] {
        TestStep::ExpectPred { cond } => match cond {
            Expr::BinOp(l, BinOp::Gt, r) => {
                assert!(matches!(l.as_ref(), Expr::FieldAccess(base, field) if is_result(base) && field == "0"));
                assert!(is_int(r, 1));
            }
            other => panic!("expected lens predicate, got {other:?}"),
        },
        other => panic!("expected ExpectPred, got {other:?}"),
    }
    match &body[4] {
        TestStep::ExpectPred { cond } => match cond {
            Expr::BinOp(l, BinOp::Or, r) => {
                assert!(matches!(r.as_ref(), Expr::Ident(name) if name == "flag"));
                match l.as_ref() {
                    Expr::BinOp(cmp, BinOp::And, ge) => {
                        assert!(matches!(cmp.as_ref(), Expr::BinOp(lhs, BinOp::Gt, rhs) if is_result(lhs) && is_int(rhs, 0)));
                        assert!(matches!(ge.as_ref(), Expr::BinOp(lhs, BinOp::Ge, rhs) if matches!(lhs.as_ref(), Expr::Ident(n) if n == "m_count") && is_int(rhs, 1)));
                    }
                    other => panic!("expected && under ||, got {other:?}"),
                }
            }
            other => panic!("expected || predicate, got {other:?}"),
        },
        other => panic!("expected ExpectPred, got {other:?}"),
    }
}

#[test]
fn parse_expect_member_arithmetic_predicate() {
    let p = parse(r#"
        entity Counter {
            routes { get() -> u64 => [return(m_count)] }
            m_count: u64 {}
            m_spent: u64 {}
        }
        test "arith" for Counter {
            call get()
            expect m_spent + m_count == start
        }
    "#);
    match &p.tests[0].body[1] {
        TestStep::ExpectPred { cond } => match cond {
            Expr::BinOp(l, BinOp::Eq, r) => {
                assert!(matches!(l.as_ref(), Expr::BinOp(a, BinOp::Add, b)
                    if matches!(a.as_ref(), Expr::Ident(n) if n == "m_spent")
                    && matches!(b.as_ref(), Expr::Ident(n) if n == "m_count")));
                assert!(matches!(r.as_ref(), Expr::Ident(n) if n == "start"));
            }
            other => panic!("expected equality predicate, got {other:?}"),
        },
        other => panic!("expected ExpectPred, got {other:?}"),
    }
}

#[test]
fn parse_expect_equality_forms_unchanged() {
    let p = parse(r#"
        entity Counter {
            routes { get() -> u64 => [return(m_count)] }
            m_count: u64 {}
        }
        test "eq" for Counter {
            call get()
            expect return 42
            expect return == 42
            expect state { m_count: 1 }
            expect return.0 == 1
            expect return.field == 1
        }
    "#);
    let body = &p.tests[0].body;
    assert!(matches!(&body[1], TestStep::ExpectReturn { value } if is_int(value, 42)));
    assert!(matches!(&body[2], TestStep::ExpectReturn { value } if is_int(value, 42)));
    assert!(matches!(&body[3], TestStep::ExpectState { .. }));
    assert!(matches!(&body[4], TestStep::ExpectReturnLens { .. }));
    assert!(matches!(&body[5], TestStep::ExpectReturnLens { .. }));
}

#[cfg(feature = "rust-targets")]
fn assert_codegen_ok(p: &cambrian_transpiler::ast::Program, crate_name: &str) {
    use cambrian_transpiler::codegen::test_codegen::generate_tests;
    let code = generate_tests(p, crate_name, &cambrian_transpiler::project::FuzzConfig::default(), &cambrian_transpiler::project::InvariantConfig::default());
    assert!(code.is_some(), "should generate test code for {}", crate_name);
    let code = code.unwrap();
    assert!(code.contains("#[test]"), "generated code must have #[test] for {}", crate_name);
    for test in &p.tests {
        let fn_name: String = test.name.chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect::<String>().trim_matches('_').to_lowercase();
        let fn_name = format!("test_{}", fn_name);
        assert!(code.contains(&fn_name),
            "generated code must contain fn '{}' for test '{}' in {}", fn_name, test.name, crate_name);
    }
}
