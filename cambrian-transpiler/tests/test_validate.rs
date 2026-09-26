// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use cambrian_transpiler::target::Target;
use cambrian_transpiler::validate::{check_target_compat, validate, Diagnostic, Severity};

fn parse(src: &str) -> cambrian_transpiler::ast::Program {
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("Parse error: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    program
}

fn errors(src: &str) -> Vec<&'static str> {
    let prog = parse(src);
    validate(&prog)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect()
}

fn warnings(src: &str) -> Vec<&'static str> {
    let prog = parse(src);
    validate(&prog)
        .into_iter()
        .filter(|d| d.severity == Severity::Warning)
        .map(|d| d.code)
        .collect()
}

fn has_error(src: &str, code: &str) -> bool {
    errors(src).contains(&code)
}

fn has_warning(src: &str, code: &str) -> bool {
    warnings(src).contains(&code)
}

fn no_errors(src: &str) {
    let errs = errors(src);
    assert!(errs.is_empty(), "Expected no errors, got: {:?}", errs);
}

// ===================================================================
// Valid programs have no errors
// ===================================================================

#[test]
fn valid_simple_entity() {
    no_errors("entity E { routes { go() => [] } m_x: u64 { in go() => 0 } }");
}

#[test]
fn valid_with_pure_fn() {
    no_errors("pure fn f(x: u64) -> u64 { x }\nentity E { routes { go() => [] } m_x: u64 { in go() => f(1) } }");
}

#[test]
fn valid_temporal_ref() {
    no_errors(
        "entity E { routes { go() => [] } m_a: u64 { in go() => 1 } m_b: u64 { in go() => ^m_a } }",
    );
}

#[test]
fn valid_view_route() {
    no_errors("entity E { routes { view get() -> u64 => [return(0)] } m_x: u64 {} }");
}

#[test]
fn valid_pure_route() {
    no_errors("entity E { routes { pure calc(x: u64) -> u64 => [return(x)] } m_x: u64 {} }");
}

#[test]
fn valid_where_clause() {
    no_errors(
        "entity E { routes { go(x: u64) where x > 0 : throw 1 => [] } m_x: u64 { in go(x) => x } }",
    );
}

#[test]
fn valid_from_clause() {
    no_errors("entity E { routes { go() from Admin() => [] } m_x: u64 { in go() => 0 } }");
}

#[test]
fn valid_match_in_pure() {
    no_errors("pure fn f(x: u64) -> u64 { match x { 0 => 1, _ => x } }\nentity E { m_x: u64 {} }");
}

// ===================================================================
// Temporal reference errors
// ===================================================================

#[test]
fn temporal_ref_to_self_is_cycle() {
    assert!(has_error(
        "entity E { routes { go() => [] } m_x: u64 { in go() => ^m_x } }",
        "V3",
    ));
}

#[test]
fn temporal_cycle_is_error() {
    assert!(has_error(
        "entity E { routes { go() => [] } m_a: u64 { in go() => ^m_b } m_b: u64 { in go() => ^m_a } }",
        "V3",
    ));
}

#[test]
fn temporal_ref_to_undefined_member() {
    assert!(has_error(
        "entity E { routes { go() => [] } m_x: u64 { in go() => ^m_nonexistent } }",
        "V3",
    ));
}

#[test]
fn transform_for_unknown_route_is_error() {
    assert!(has_error(
        "entity E { routes { go() => [] } m_x: u64 { in nonexistent() => 0 } }",
        "V1",
    ));
}

// ===================================================================
// Purity violations (V4)
// ===================================================================

#[test]
fn pure_fn_accessing_msg_is_error() {
    assert!(has_error(
        "pure fn f() -> u64 { msg::sender }\nentity E { m_x: u64 {} }",
        "V4",
    ));
}

#[test]
fn pure_fn_using_temporal_is_error() {
    assert!(has_error(
        "pure fn f() -> u64 { ^m_x }\nentity E { m_x: u64 {} }",
        "V4",
    ));
}

#[test]
fn pure_fn_using_macro_is_error() {
    assert!(has_error(
        "pure fn f() -> u64 { @m() }\nentity E { macro m() -> u64 = { 42 } m_x: u64 {} }",
        "V4",
    ));
}

// EVM-target intrinsic purity (Batch 1):
// `evm::sha256` and `evm::ripemd160` are deterministic precompile
// hashes — they may appear inside `pure fn`. `evm::balance` reads live
// state and `evm::blockhash` reads block context — both are impure.

#[test]
fn pure_fn_using_evm_sha256_is_ok() {
    no_errors(
        "pure fn h(a: U256, b: U256) -> U256 { evm::sha256(a, b) }\n\
         entity E { routes { go() => [] } m_x: U256 { in go() => h(1, 2) } }",
    );
}

#[test]
fn pure_fn_using_evm_ripemd160_is_ok() {
    no_errors(
        "pure fn h(a: U256) -> U256 { evm::ripemd160(a) }\n\
         entity E { routes { go() => [] } m_x: U256 { in go() => h(7) } }",
    );
}

#[test]
fn pure_fn_using_evm_balance_is_error() {
    assert!(has_error(
        "pure fn h(addr: address) -> U256 { evm::balance(addr) }\n\
         entity E { m_x: U256 {} }",
        "V4",
    ));
}

#[test]
fn pure_fn_using_evm_blockhash_is_error() {
    assert!(has_error(
        "pure fn h(n: U256) -> U256 { evm::blockhash(n) }\n\
         entity E { m_x: U256 {} }",
        "V4",
    ));
}

// ===================================================================
// View route errors (V9)
// ===================================================================

#[test]
fn view_route_with_state_modification_is_error() {
    assert!(has_error(
        "entity E { routes { view get() => [] } m_x: u64 { in get() => 1 } }",
        "V9",
    ));
}

// ===================================================================
// Undefined reference errors (V8)
// ===================================================================

#[test]
fn undefined_pure_fn_call_is_error() {
    assert!(has_error(
        "entity E { routes { go() => [] } m_x: u64 { in go() => nonexistent_fn(1) } }",
        "V8",
    ));
}

#[test]
fn defined_pure_fn_call_is_ok() {
    no_errors(
        "pure fn helper(x: u64) -> u64 { x }\nentity E { routes { go() => [] } m_x: u64 { in go() => helper(1) } }",
    );
}

// ===================================================================
// Pure route errors (V010, V011)
// ===================================================================

#[test]
fn pure_route_with_member_transform_is_error() {
    assert!(has_error(
        "entity E { routes { pure calc() => [] } m_x: u64 { in calc() => 42 } }",
        "V10",
    ));
}

#[test]
fn pure_route_with_state_read_is_error() {
    assert!(has_error(
        "entity E { routes { pure calc() -> u64 => [return(m_x)] } m_x: u64 {} }",
        "V11",
    ));
}

#[test]
fn pure_route_with_msg_is_error() {
    assert!(has_error(
        "entity E { routes { pure calc() -> u64 => [return(msg::sender)] } m_x: u64 {} }",
        "V11",
    ));
}

#[test]
fn pure_route_with_sys_is_error() {
    assert!(has_error(
        "entity E { routes { pure calc() -> u64 => [return(sys::now)] } m_x: u64 {} }",
        "V11",
    ));
}

#[test]
fn pure_route_with_temporal_is_error() {
    assert!(has_error(
        "entity E { routes { pure calc() -> u64 => [return(^m_x)] } m_x: u64 {} }",
        "V11",
    ));
}

#[test]
fn pure_route_with_macro_is_error() {
    assert!(has_error(
        "entity E { macro h() -> u64 = { 42 } routes { pure calc() -> u64 => [return(@h())] } m_x: u64 {} }",
        "V11",
    ));
}

#[test]
fn pure_route_with_only_params_is_ok() {
    no_errors("entity E { routes { pure calc(x: u64) -> u64 => [return(x + 1)] } m_x: u64 {} }");
}

// ===================================================================
// Linting: unused members (W1)
// ===================================================================

#[test]
fn lint_unused_member_warns() {
    assert!(has_warning(
        "entity E { routes { go() => [] } m_x: u64 {} }",
        "W1",
    ));
}

#[test]
fn lint_used_member_no_warn() {
    assert!(!has_warning(
        "entity E { routes { go() => [] } m_x: u64 { in go() => 0 } }",
        "W1",
    ));
}

// ===================================================================
// Linting: unused pure fns (W2)
// ===================================================================

#[test]
fn lint_unused_pure_fn_warns() {
    assert!(has_warning(
        "pure fn unused(x: u64) -> u64 { x }\nentity E { m_x: u64 {} }",
        "W2",
    ));
}

#[test]
fn lint_used_pure_fn_no_warn() {
    assert!(!has_warning(
        "pure fn helper(x: u64) -> u64 { x }\nentity E { routes { go() => [] } m_x: u64 { in go() => helper(1) } }",
        "W2",
    ));
}

// ===================================================================
// Linting: unused constants (W3)
// ===================================================================

#[test]
fn lint_unused_const_warns() {
    assert!(has_warning(
        "entity E { const UNUSED: u64 = 42 m_x: u64 {} }",
        "W3",
    ));
}

#[test]
fn lint_used_const_no_warn() {
    assert!(!has_warning(
        "entity E { const K: u64 = 42 routes { go() => [] } m_x: u64 { in go() => K } }",
        "W3",
    ));
}

// ===================================================================
// Complex scenarios
// ===================================================================

#[test]
fn valid_complex_entity() {
    no_errors(
        r#"
pure fn helper(x: u64, y: u64) -> u64 { x + y }

record Data { value: u64, flag: bool }

enum Status { Active, Done }

type Balance = u128

entity Complex {
    const MAX: u64 = 100

    macro get_max() -> u64 = { MAX }

    routes {
        setup(val: u64) where val < MAX : throw 1 => []
        view get_val() -> u64 => [return(m_value)]
        pure compute(a: u64, b: u64) -> u64 => [return(helper(a, b))]
    }

    m_value: u64 {
        in setup(val) => val
    }

    m_status: Status {
        in setup(_) => Status::Active
    }
}
"#,
    );
}

#[test]
fn multiple_entities_validate_independently() {
    no_errors(
        r#"
entity A {
    routes { go() => [] }
    m_x: u64 { in go() => 1 }
}
entity B {
    routes { run() => [] }
    m_y: bool { in run() => true }
}
"#,
    );
}

#[test]
fn let_in_route_body_validates() {
    no_errors(
        r#"
pure fn double(x: u64) -> u64 { x * 2 }
entity E {
    routes {
        act(x: u64) -> u64 => [
            let result = double(x);
            return(result)
        ]
    }
    m_x: u64 { in act(x) => x }
}
"#,
    );
}

#[test]
fn conditional_route_action_validates() {
    no_errors(
        r#"
entity E {
    routes {
        act(x: u64) -> u64 => [
            if x > 10 => [
                return(x)
            ]
        ]
    }
    m_x: u64 { in act(x) => x }
}
"#,
    );
}

#[test]
fn nested_expr_in_transform_validates() {
    no_errors(
        r#"
entity E {
    routes { go(x: u64) => [] }
    m_x: u64 {
        in go(x) => {
            let a = x + 1;
            let b = a * 2;
            if b > 10 { b } else { 0 }
        }
    }
}
"#,
    );
}

#[test]
fn for_range_in_pure_fn_validates() {
    no_errors(
        r#"
pure fn sum(n: u64) -> Vec<u64> {
    for x in 0..n { x + 1 }
}
entity E { m_x: u64 {} }
"#,
    );
}

#[test]
fn option_destructure_validates() {
    no_errors(
        r#"
pure fn unwrap_or(opt: Option<u64>, default_val: u64) -> u64 {
    match opt { some(v) => v, none => default_val }
}
entity E { m_x: u64 {} }
"#,
    );
}

// ===================================================================
// Gap 5: V11 — Send action inside pure route
// ===================================================================

#[test]
fn pure_route_with_send_is_error() {
    assert!(has_error(
        "entity E { routes { pure calc(x: u64) => [go(x) ~> Target.address()] } m_x: u64 {} }",
        "V11",
    ));
}

#[test]
fn pure_route_send_dest_member_ref_is_error() {
    assert!(has_error(
        r#"entity E { routes { pure calc(x: u64) => [go(x) ~> m_target] } m_target: address {} m_x: u64 {} }"#,
        "V11",
    ));
}

#[test]
fn pure_route_conditional_with_send_is_error() {
    assert!(has_error(
        r#"entity E { routes { pure calc(x: u64) => [ if x > 0 => [go(x) ~> Target.address()] ] } m_x: u64 {} }"#,
        "V11",
    ));
}

// ===================================================================
// Effects in pure routes
// ===================================================================

#[test]
fn pure_route_with_effect_is_error() {
    assert!(has_error(
        r#"use gosh entity E { routes { pure calc(x: u64) => [ gosh::rawReserve(x, 0) ] } m_x: u64 {} }"#,
        "V11",
    ));
}

#[test]
fn pure_route_conditional_with_effect_is_error() {
    assert!(has_error(
        r#"use gosh entity E { routes { pure calc(x: u64) => [ if x > 0 => [gosh::rawReserve(x, 0)] ] } m_x: u64 {} }"#,
        "V11",
    ));
}

#[test]
fn non_pure_route_with_effect_is_ok() {
    no_errors(
        r#"use gosh entity E { routes { withdraw(amount: u64) => [ gosh::rawReserve(amount, 0) ] } m_x: u64 {} }"#,
    );
}

// ===================================================================
// Gap 6: is_const_expr — hex, binary, string, bytes, bool literals
// ===================================================================

#[test]
fn const_hex_literal_is_valid() {
    no_errors("entity E { const X: u64 = 0xFF m_x: u64 {} }");
}

#[test]
fn const_bin_literal_is_valid() {
    no_errors("entity E { const X: u64 = 0b1010 m_x: u64 {} }");
}

#[test]
fn const_string_literal_is_valid() {
    no_errors(r#"entity E { const X: String = "hello" m_x: u64 {} }"#);
}

#[test]
fn const_bytes_literal_is_valid() {
    no_errors(r#"entity E { const X: Vec<u8> = 0x48656C6C6F m_x: u64 {} }"#);
}

#[test]
fn const_bool_literal_is_valid() {
    no_errors("entity E { const X: bool = true m_x: u64 {} }");
}

#[test]
fn const_non_literal_is_error() {
    assert!(has_error(
        "entity E { const X: u64 = m_x m_x: u64 {} }",
        "V7",
    ));
}

#[test]
fn const_fn_call_is_error() {
    assert!(has_error(
        "pure fn f() -> u64 { 1 }\nentity E { const X: u64 = f() m_x: u64 {} }",
        "V7",
    ));
}

// ===================================================================
// Gap 10: Expr::For scope extension in purity check
// ===================================================================

#[test]
fn for_loop_var_is_in_scope_in_pure_fn() {
    no_errors(
        r#"
pure fn process(items: Vec<u64>) -> Vec<u64> {
    for item in items { item + 1 }
}
entity E { m_x: u64 {} }
"#,
    );
}

#[test]
fn for_loop_var_shadows_outer_in_pure_fn() {
    no_errors(
        r#"
pure fn process(items: Vec<u64>) -> Vec<u64> {
    let offset = 0;
    for item in items { item + offset }
}
entity E { m_x: u64 {} }
"#,
    );
}

#[test]
fn for_range_body_can_use_range_var_in_pure() {
    no_errors(
        r#"
pure fn seq(n: u64) -> Vec<u64> {
    for i in 0..n { i * 2 }
}
entity E { m_x: u64 {} }
"#,
    );
}

#[test]
fn for_body_using_msg_in_pure_fn_is_error() {
    assert!(has_error(
        r#"
pure fn bad(items: Vec<u64>) -> Vec<u64> {
    for item in items { msg::sender }
}
entity E { m_x: u64 {} }
"#,
        "V4"
    ));
}

#[test]
fn for_body_using_temporal_in_pure_fn_is_error() {
    assert!(has_error(
        r#"
pure fn bad(items: Vec<u64>) -> Vec<u64> {
    for item in items { ^m_x }
}
entity E { m_x: u64 {} }
"#,
        "V4"
    ));
}

#[test]
fn pure_route_with_let_and_return_is_ok() {
    no_errors(
        r#"
entity E {
    routes {
        pure double(x: u64) -> u64 => [
            let result = x + x;
            return(result)
        ]
    }
    m_x: u64 {}
}
"#,
    );
}

// ===================================================================
// V16-V17: Identity member constraints
// ===================================================================

#[test]
fn identity_member_valid() {
    no_errors(
        r#"
entity Vault {
    routes { constructor() => [] }
    identity m_id: u64
    m_balance: u64 {
        in constructor() => 0
    }
}
"#,
    );
}

#[test]
fn identity_member_no_w1_warning() {
    let src = r#"
entity Vault {
    identity m_id: u64
}
"#;
    assert!(
        !has_warning(src, "W1"),
        "identity member should not trigger W1 unused warning"
    );
}

// V16 is defense-in-depth: grammar already prevents transforms on identity members.
// Verify that identity + regular members coexist without V16 errors.
#[test]
fn identity_member_coexists_with_regular_transforms() {
    no_errors(
        r#"
entity Vault {
    routes { setId(new_id: u64) => [] }
    identity m_id: u64
    m_count: u64 {
        in setId(_) => 0
    }
}
"#,
    );
}

#[test]
fn identity_member_multiple_valid() {
    no_errors(
        r#"
entity MultiId {
    identity m_owner: address
    identity m_nonce: u64
}
"#,
    );
}

#[test]
fn rescue_deploy_is_valid() {
    assert!(
        !has_error(
            r#"
entity Factory {
    routes {
        create(id: u64) => [
            rescue vault_deploy: deploy Vault with { value: 1000000, stateInit: Vault.state(code, id) }
        ]
        recover vault_deploy(body: CamData) => []
    }
}
"#,
            "V25"
        ),
        "rescue deploy must be allowed (V25 removed)"
    );
}

#[test]
fn rescue_plain_transfer_is_valid() {
    assert!(
        !has_error(
            r#"
entity Wallet {
    routes {
        send(dest: address, amount: u128) => [
            rescue send_failed: ~> dest with { value: amount }
        ]
        recover send_failed(body: CamData) => []
    }
}
"#,
            "V25"
        ),
        "rescue plain transfer must be allowed (V25 removed)"
    );
}

#[test]
fn rescue_with_bounce_false_is_error() {
    assert!(
        has_error(
            r#"
entity Wallet {
    routes {
        send(dest: address, amount: u128) => [
            rescue send_failed: transfer(amount) ~> dest with { bounce: false }
        ]
        transfer(amount: u128) => []
        recover send_failed(body: CamData) => []
    }
}
"#,
            "V26"
        ),
        "rescue with bounce: false must be rejected"
    );
}

#[test]
fn rescue_with_bounce_true_is_valid() {
    assert!(
        !has_error(
            r#"
entity Wallet {
    routes {
        send(dest: address, amount: u128) => [
            rescue send_failed: transfer(amount) ~> dest with { bounce: true }
        ]
        transfer(amount: u128) => []
        recover send_failed(body: CamData) => []
    }
}
"#,
            "V26"
        ),
        "rescue with bounce: true must be accepted"
    );
}

#[test]
fn rescue_without_bounce_option_is_valid() {
    assert!(
        !has_error(
            r#"
entity Wallet {
    routes {
        send(dest: address, amount: u128) => [
            rescue send_failed: transfer(amount) ~> dest
        ]
        transfer(amount: u128) => []
        recover send_failed(body: CamData) => []
    }
}
"#,
            "V26"
        ),
        "rescue without explicit bounce option must be accepted (default is true)"
    );
}

// ===== V27: Mixed phased and unphased items in route body =====

#[test]
fn validate_mixed_phases_and_actions_produces_error() {
    assert!(
        has_error(
            r#"
entity E {
    routes {
        foo() => [
            step: [ gosh::commit() ]
            gosh::exit(0)
        ]
    }
}
"#,
            "V54"
        ),
        "Mixing phases and bare actions should produce V54 error"
    );
}

#[test]
fn validate_all_phases_no_error() {
    assert!(
        !has_error(
            r#"
entity E {
    routes {
        foo() => [
            a: [ gosh::commit() ]
            b: [ gosh::exit(0) ]
        ]
    }
}
"#,
            "V27"
        ),
        "All phases should not produce V27 error"
    );
}

#[test]
fn validate_all_actions_no_error() {
    assert!(
        !has_error(
            r#"
entity E {
    routes {
        foo() => [
            gosh::commit()
            gosh::exit(0)
        ]
    }
}
"#,
            "V27"
        ),
        "All bare actions should not produce V27 error"
    );
}

// ===================================================================
// Bug-fix regression tests
// ===================================================================

// Bug: RecordUpdate base expression skipped by or-pattern in walkers.
// `undefined_var` in the base must produce V8.
#[test]
fn record_update_base_checked_for_undefined_refs() {
    assert!(has_error(
        r#"
record R { x: u64 }
entity E {
    routes { go(r: R) => [] }
    m_data: R { in go(r) => { let u = undefined_var { x: 1 }; u } }
}
"#,
        "V8"
    ));
}

// Bug: Match arm pattern bindings not added to scope in check_entity_expr_refs.
// `y` bound by pattern must not trigger V8.
#[test]
fn match_arm_binding_in_scope() {
    no_errors(
        r#"
entity E {
    routes { go() => [] }
    m_x: u64 { in go() => { match m_x { 0 => 0, y => y + 1 } } }
}
"#,
    );
}

#[test]
fn match_arm_some_binding_in_scope() {
    no_errors(
        r#"
entity E {
    routes { go(opt: Option<u64>) => [] }
    m_x: u64 { in go(opt) => { match opt { some(v) => v, none => 0 } } }
}
"#,
    );
}

// Bug: check_empty_string_as_address panics on entity.routes[0] when routes is empty.
#[test]
fn empty_routes_no_panic() {
    let src = "entity E { m_addr: address { in nonexistent() => msg::sender } }";
    let prog = parse(src);
    let _ = validate(&prog);
}

// Bug: For-loop pattern not added to route_params in check_pure_route_expr.
// `m_x` from for-loop should shadow the member, not trigger V11.
#[test]
fn for_loop_var_in_pure_route_shadows_member() {
    no_errors(
        r#"
entity E {
    routes {
        pure calc(items: Vec<u64>) -> Vec<u64> => [
            let result = { for m_x in items { m_x + 1 } };
            return(result)
        ]
    }
    m_x: u64 {}
}
"#,
    );
}

// Bug: Match arm bindings not added to scope in check_pure_route_expr.
// `m_x` from match pattern should shadow the member, not trigger V11.
#[test]
fn match_arm_binding_shadows_member_in_pure_route() {
    no_errors(
        r#"
entity E {
    routes {
        pure calc(x: u64) -> u64 => [
            let result = match x { 0 => 0, m_x => m_x + 1 };
            return(result)
        ]
    }
    m_x: u64 {}
}
"#,
    );
}

// Bug: CallRoute target route never validated for existence.
#[test]
fn call_nonexistent_route_is_error() {
    assert!(has_error(
        r#"
entity E {
    routes {
        go() => [call nonexistent()]
    }
    m_x: u64 { in go() => 0 }
}
"#,
        "V8"
    ));
}

#[test]
fn call_existing_route_is_ok() {
    no_errors(
        r#"
entity E {
    routes {
        go() => [call helper()]
        private helper() => []
    }
    m_x: u64 { in helper() => 0 }
}
"#,
    );
}

// Bug: Deploy entity name never validated against known entities.
#[test]
fn deploy_unknown_entity_is_warning() {
    assert!(has_warning(
        r#"
entity Factory {
    routes {
        create() => [deploy Nonexistent with { value: 1000000 }]
    }
    m_x: u64 {}
}
"#,
        "W5"
    ));
}

#[test]
fn deploy_known_entity_no_warning() {
    assert!(!has_warning(
        r#"
entity Vault {
    routes { go() => [] }
    m_x: u64 { in go() => 0 }
}
entity Factory {
    routes {
        create() => [deploy Vault with { value: 1000000 }]
    }
    m_x: u64 {}
}
"#,
        "W5"
    ));
}

// ===================================================================
// V12: init route constraints
// ===================================================================

#[test]
fn two_init_routes_is_error() {
    assert!(has_error(
        "entity E { routes { init a() => [] init b() => [] } m_x: u64 {} }",
        "V12",
    ));
}

#[test]
fn single_init_route_no_error() {
    no_errors("entity E { routes { init go() => [] } m_x: u64 { in go() => 0 } }");
}

// ===================================================================
// V42: return(value) requires a declared return type
// ===================================================================

#[test]
fn return_value_without_return_type_is_error() {
    assert!(has_error(
        "entity E { routes { init create() => [] a(n: u8) => [ return(m_x) ] } m_x: u64 { in create() => 0 } }",
        "V42",
    ));
}

#[test]
fn return_value_with_return_type_ok() {
    no_errors(
        "entity E { routes { init create() => [] a(n: u8) -> u64 => [ return(m_x) ] } m_x: u64 { in create() => 0 } }",
    );
}

#[test]
fn bare_return_without_return_type_ok() {
    // A bare `return` / `return()` is a valid early exit and does not
    // require a declared return type.
    no_errors(
        "entity E { routes { init create() => [] early() => [ return() ] } m_x: u64 { in create() => 0 } }",
    );
}

// ===================================================================
// V45: Multiple deploy of the same entity in one route
// ===================================================================

#[test]
fn double_deploy_different_args_is_warning() {
    assert!(has_warning(
        r#"
        entity Vault {
            identity m_id: u64
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 0 }
        }
        entity Deployer {
            routes {
                constructor() => []
                spawn(a: u64, b: u64) => [
                    deploy Vault(a)
                    deploy Vault(b)
                ]
            }
            m_n: u64 { in constructor() => 0 in spawn(_, _) => m_n + 1 }
        }
        "#,
        "V45",
    ));
    assert!(!has_error(
        r#"
        entity Vault {
            identity m_id: u64
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 0 }
        }
        entity Deployer {
            routes {
                constructor() => []
                spawn(a: u64, b: u64) => [
                    deploy Vault(a)
                    deploy Vault(b)
                ]
            }
            m_n: u64 { in constructor() => 0 in spawn(_, _) => m_n + 1 }
        }
        "#,
        "V45",
    ));
}

#[test]
fn double_deploy_identical_args_is_error() {
    assert!(has_error(
        r#"
        entity Vault {
            identity m_id: u64
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 0 }
        }
        entity Deployer {
            routes {
                constructor() => []
                spawn(a: u64) => [
                    deploy Vault(a)
                    deploy Vault(a)
                ]
            }
            m_n: u64 { in constructor() => 0 in spawn(_) => m_n + 1 }
        }
        "#,
        "V45",
    ));
}

#[test]
fn double_deploy_const_fold_equal_args_is_error() {
    assert!(has_error(
        r#"
        entity Vault {
            identity m_id: u64
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 0 }
        }
        entity Deployer {
            routes {
                constructor() => []
                spawn() => [
                    deploy Vault(0)
                    deploy Vault(0 + 0)
                ]
            }
            m_n: u64 { in constructor() => 0 in spawn() => m_n + 1 }
        }
        "#,
        "V45",
    ));
}

#[test]
fn double_deploy_wide_u256_max_equal_is_error() {
    assert!(has_error(
        r#"
        entity Vault {
            identity m_id: U256
            routes { constructor() => [] }
            m_x: U256 { in constructor() => 0 }
        }
        entity Deployer {
            routes {
                constructor() => []
                spawn() => [
                    deploy Vault(115792089237316195423570985008687907853269984665640564039457584007913129639935)
                    deploy Vault(0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF)
                ]
            }
            m_n: u64 { in constructor() => 0 in spawn() => m_n + 1 }
        }
        "#,
        "V45",
    ));
}

#[test]
fn funded_deploy_is_not_v45() {
    let src = r#"
        entity Vault {
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 0 }
        }
        entity E {
            routes {
                constructor() => []
                go() => [ deploy Vault() with { value: 1 } ]
            }
            m_n: u64 { in constructor() => 0 in go() => m_n + 1 }
        }
    "#;
    assert!(!has_error(src, "V45"));
    assert!(!has_warning(src, "V45"));
}

// ===================================================================
// V51: Unused `let` bindings
// ===================================================================

#[test]
fn unused_let_in_route_body_is_error() {
    assert!(has_error(
        r#"
        entity E {
            routes {
                constructor() => []
                go() => [
                    let x = 1;
                ]
            }
            m_n: u64 { in constructor() => 0 in go() => m_n }
        }
        "#,
        "V51",
    ));
}

#[test]
fn used_let_in_route_body_ok() {
    assert!(!has_error(
        r#"
        entity E {
            routes {
                constructor() => []
                go() -> u64 => [
                    let x = 1;
                    return(x)
                ]
            }
            m_n: u64 { in constructor() => 0 in go() => m_n }
        }
        "#,
        "V51",
    ));
}

#[test]
fn let_used_in_later_phase_is_not_unused() {
    // Cross-phase lets are hoisted to function scope by codegen; a binding
    // read by a later phase's actions is used (the ERC-4626 deposit shape).
    assert!(!has_error(
        r#"
        entity E {
            routes {
                constructor() => []
                go(amount: u64) -> u64 => [
                    read: [
                        let issued = amount + m_n;
                    ]
                    issue: [
                        return(issued)
                    ]
                ]
            }
            m_n: u64 { in constructor() => 0 in go(_) => m_n }
        }
        "#,
        "V51",
    ));
}

#[test]
fn let_used_in_later_phase_where_is_not_unused() {
    // A later phase's `where` clause is a usage site (V28 direction).
    assert!(!has_error(
        r#"
        entity E {
            routes {
                constructor() => []
                go(amount: u64) => [
                    read: [
                        let limit = m_n + 1;
                    ]
                    commit where (amount < limit) : throw 9 : [
                        ~> msg::sender
                    ]
                ]
            }
            m_n: u64 { in constructor() => 0 in go(_) => m_n }
        }
        "#,
        "V51",
    ));
}

#[test]
fn unused_let_in_phased_route_is_error() {
    // The cross-phase widening must not swallow genuinely dead bindings.
    assert!(has_error(
        r#"
        entity E {
            routes {
                constructor() => []
                go(amount: u64) -> u64 => [
                    read: [
                        let dead = amount + m_n;
                    ]
                    issue: [
                        return(amount)
                    ]
                ]
            }
            m_n: u64 { in constructor() => 0 in go(_) => m_n }
        }
        "#,
        "V51",
    ));
}

#[test]
fn unused_let_in_if_branch_is_error() {
    assert!(has_error(
        r#"
        entity E {
            routes {
                constructor() => []
                go(flag: bool) => [
                    if flag => [
                        let capped = std::math::clamp(m_n, 0, 10);
                    ]
                ]
            }
            m_n: u64 { in constructor() => 0 in go(_) => m_n }
        }
        "#,
        "V51",
    ));
}

// ===================================================================
// V52: Recursive / cyclic records
// ===================================================================

#[test]
fn recursive_record_is_error() {
    assert!(has_error(
        r#"
        record Node {
            v: u64,
            next: Node
        }
        entity E {
            routes { constructor() => [] }
            m_n: Node { }
        }
        "#,
        "V52",
    ));
}

#[test]
fn option_wrapped_recursive_record_ok() {
    assert!(!has_error(
        r#"
        record Node {
            v: u64,
            next: Option<Node>
        }
        entity E {
            routes { constructor() => [] }
            m_n: Option<Node> { in constructor() => none }
        }
        "#,
        "V52",
    ));
}

#[test]
fn mutual_recursive_records_are_error() {
    assert!(has_error(
        r#"
        record A { b: B }
        record B { a: A }
        entity E {
            routes { constructor() => [] }
            m_a: A { }
        }
        "#,
        "V52",
    ));
}

#[test]
fn cyclic_alias_pair_is_error() {
    assert!(has_error(
        r#"
        type A = B
        type B = A
        entity E {
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 0 }
        }
        "#,
        "V52",
    ));
}

#[test]
fn self_alias_is_error() {
    assert!(has_error(
        r#"
        type A = A
        entity E {
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 0 }
        }
        "#,
        "V52",
    ));
}

#[test]
fn option_wrapped_cyclic_alias_is_error() {
    assert!(has_error(
        r#"
        type A = Option<A>
        entity E {
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 0 }
        }
        "#,
        "V52",
    ));
}

#[test]
fn alias_to_cyclic_record_is_error() {
    assert!(has_error(
        r#"
        type A = Node
        record Node { next: A }
        entity E {
            routes { constructor() => [] }
            m_n: Node { }
        }
        "#,
        "V52",
    ));
}

// ===================================================================
// V53: Nested pattern under `let some(...)`
// ===================================================================

#[test]
fn let_some_nested_pattern_is_error() {
    assert!(has_error(
        r#"
        entity E {
            routes {
                constructor() => []
                go(x: Option<u64>) => [
                    let some(none) = x;
                ]
            }
            m_n: u64 { in constructor() => 0 in go(_) => m_n }
        }
        "#,
        "V53",
    ));
}

#[test]
fn let_some_plain_ident_ok() {
    assert!(!has_error(
        r#"
        entity E {
            routes {
                constructor() => []
                go(x: Option<u64>) -> u64 => [
                    let some(v) = x;
                    return(v)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
        "#,
        "V53",
    ));
}

// ===================================================================
// V46: retired (TYPED-LIT-0) — no implicit-address lint
// ===================================================================

#[test]
fn v46_retired_untyped_address_shaped_hex_no_warning() {
    assert!(!has_warning(
        r#"
        entity E {
            routes {
                constructor() => []
                go() -> u64 => [
                    let h = 0x1111111111111111111111111111111111111111;
                    return(h as u64)
                ]
            }
            m_n: u64 { in constructor() => 0 in go() => m_n }
        }
        "#,
        "V46",
    ));
}

#[test]
fn v46_retired_explicit_cast_still_ok() {
    assert!(!has_warning(
        r#"
        entity E {
            routes {
                constructor() => []
                go() -> address => [
                    let h = 0x1111111111111111111111111111111111111111 as address;
                    return(h)
                ]
            }
            m_n: u64 { in constructor() => 0 in go() => m_n }
        }
        "#,
        "V46",
    ));
}

// ===================================================================
// V47: Unreachable / overlapping match arms
// ===================================================================

#[test]
fn match_arm_after_wildcard_is_warning() {
    assert!(has_warning(
        r#"
        entity E {
            routes {
                constructor() => []
                go(x: u64) => []
            }
            m_n: u64 {
                in constructor() => 0
                in go(x) => {
                    match x { _ => 0, 1 => 1 }
                }
            }
        }
        "#,
        "V47",
    ));
}

#[test]
fn match_duplicate_int_literal_is_warning() {
    assert!(has_warning(
        r#"
        entity E {
            routes {
                constructor() => []
                go(x: u64) => []
            }
            m_n: u64 {
                in constructor() => 0
                in go(x) => {
                    match x { 1 => 1, 1 => 2, _ => 0 }
                }
            }
        }
        "#,
        "V47",
    ));
}

// ===================================================================
// V48: if-without-else in value contexts
// ===================================================================

#[test]
fn if_without_else_in_transform_is_error() {
    assert!(has_error(
        r#"
        entity E {
            routes {
                constructor() => []
                pick(on: bool) => []
            }
            m_flag: bool {
                in constructor() => false
                in pick(on) => {
                    if on { true }
                }
            }
        }
        "#,
        "V48",
    ));
}

#[test]
fn statement_if_without_else_ok() {
    assert!(!has_error(
        r#"
        entity E {
            routes {
                constructor() => []
                go(c: bool) => [
                    if c => [ throw 1 ]
                ]
            }
            m_n: u64 { in constructor() => 0 in go(_) => m_n }
        }
        "#,
        "V48",
    ));
}

#[test]
fn if_with_else_in_transform_ok() {
    assert!(!has_error(
        r#"
        entity E {
            routes {
                constructor() => []
                pick(on: bool) => []
            }
            m_flag: bool {
                in constructor() => false
                in pick(on) => {
                    if on { true } else { false }
                }
            }
        }
        "#,
        "V48",
    ));
}

// ===================================================================
// V13: duplicate phase names
// ===================================================================

#[test]
fn duplicate_phase_names_is_error() {
    assert!(has_error(
        "entity E { routes { go() => [a: [] a: []] } m_x: u64 { in go() => a: 1 } }",
        "V13",
    ));
}

#[test]
fn unique_phase_names_no_error() {
    no_errors("entity E { routes { go() => [a: [] b: []] } m_x: u64 { in go() => a: 1 b: 2 } }");
}

// ===================================================================
// V14: phased/unphased mismatch
// ===================================================================

#[test]
fn phased_route_with_unphased_member_is_error() {
    assert!(has_error(
        "entity E { routes { go() => [a: []] } m_x: u64 { in go() => 1 } }",
        "V14",
    ));
}

// ===================================================================
// V15: unknown phase tag
// ===================================================================

#[test]
fn unknown_phase_tag_is_error() {
    assert!(has_error(
        "entity E { routes { go() => [a: [] b: []] } m_x: u64 { in go() => c: 1 } }",
        "V15",
    ));
}

// ===================================================================
// V20: empty string as address
// ===================================================================

#[test]
fn empty_string_compared_to_address_member_is_error() {
    assert!(has_error(
        r#"entity E { routes { go() => [] } m_addr: address { in go() => { if m_addr == "" { m_addr } else { msg::sender } } } }"#,
        "V20",
    ));
}

// ===================================================================
// V21/V22: namespace imports
// ===================================================================

#[test]
fn unknown_namespace_import_is_error() {
    assert!(has_error(
        "use unknown\nentity E { routes { go() => [] } m_x: u64 {} }",
        "V21",
    ));
}

#[test]
fn namespace_without_import_is_error() {
    assert!(has_error(
        "entity E { routes { go() => [gosh::rawReserve(100, 0)] } m_x: u64 {} }",
        "V22",
    ));
}

// ===================================================================
// Purity: sys:: ref in pure fn is V4 error
// ===================================================================

#[test]
fn sys_ref_in_pure_fn_is_error() {
    assert!(has_error(
        "pure fn f() -> u64 { sys::now }\nentity E { routes { go() => [] } m_x: u64 {} }",
        "V4",
    ));
}

// ===================================================================
// Temporal reference chain: 3 members
// ===================================================================

#[test]
fn temporal_chain_three_members_valid() {
    no_errors(
        "entity E { routes { go() => [] } m_a: u64 { in go() => 0 } m_b: u64 { in go() => ^m_a + 1 } m_c: u64 { in go() => ^m_b + 1 } }",
    );
}

// ===================================================================
// Const expression validation: valid constant used in transform
// ===================================================================

#[test]
fn const_expression_used_in_transform_valid() {
    no_errors(
        "entity E { const MAX_VAL: u64 = 100 routes { go() => [] } m_x: u64 { in go() => MAX_VAL } }",
    );
}

// ===================================================================
// T9: registry references unknown entity
// ===================================================================

#[test]
fn registry_unknown_entity_is_warning() {
    assert!(has_warning(
        r#"
        entity Vault {
            identity m_id: u64
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 0 }
        }
        test "t" for Vault with { m_id: 1 } {
            registry UnknownEntity { code_hash: 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa, code_depth: 1, wasm_hash: 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb }
            call constructor()
            expect state { m_x: 0 }
        }
    "#,
        "T9"
    ));
}

#[test]
fn registry_known_entity_no_warning() {
    let ws = warnings(
        r#"
        entity Vault {
            identity m_id: u64
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 0 }
        }
        test "t" for Vault with { m_id: 1 } {
            registry Vault { code_hash: 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa, code_depth: 1, wasm_hash: 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb }
            call constructor()
            expect state { m_x: 0 }
        }
    "#,
    );
    assert!(
        !ws.contains(&"T9"),
        "known entity should not produce T9 warning: {:?}",
        ws
    );
}

// ===================================================================
// V16/V17: Identity member constraints — grammar prevents triggering
// these from source, so they are defense-in-depth only. Skipped.
// ===================================================================

// ===================================================================
// V23: Named send to untyped address
// ===================================================================

#[test]
fn v23_named_send_to_untyped_address() {
    assert!(has_error(
        r#"
entity Foo {
    routes { route1(n: u64) => [ping(n) ~> m_target] }
    m_target: address {}
}
"#,
        "V23"
    ));
}

// ===================================================================
// V25: gosh::updateCode wrong argument count
// ===================================================================

#[test]
fn v25_update_code_wrong_arity() {
    assert!(has_error(
        r#"
use gosh
entity Foo {
    routes {
        up() => [gosh::updateCode() with onUpgrade()]
        onUpgrade() => []
    }
    m_x: u64 { in onUpgrade() => 0 }
}
"#,
        "V25"
    ));
}

// ===================================================================
// W4: Duplicate use gosh
// ===================================================================

#[test]
fn w4_duplicate_use_gosh() {
    assert!(has_warning(
        r#"
use gosh
use gosh
entity Foo {
    routes { ping() => [] }
    m_x: u64 { in ping() => 0 }
}
"#,
        "W4"
    ));
}

// ===================================================================
// W6: Deprecated standalone gosh::setcode
// ===================================================================

// ===================================================================
// W9: Nested record / enum / type alias duplicates program-scope name
// ===================================================================

#[test]
fn w9_nested_record_duplicates_program_scope() {
    assert!(has_warning(
        r#"
record Packet { v: u64 }
entity Holder {
    record Packet { v: u64 }
    routes { constructor() => [] }
    m_p: HashMap<u64, Packet> { in constructor() => {} }
}
"#,
        "W9"
    ));
}

#[test]
fn w9_nested_enum_duplicates_program_scope() {
    assert!(has_warning(
        r#"
enum Status { On, Off }
entity Gate {
    enum Status { On, Off }
    routes { constructor() => [] }
    m_s: Status { in constructor() => Status::On }
}
"#,
        "W9"
    ));
}

#[test]
fn w9_nested_alias_duplicates_program_scope() {
    assert!(has_warning(
        r#"
type Amount = u64
entity Vault {
    type Amount = u64
    routes { constructor() => [] }
    m_n: Amount { in constructor() => 0 }
}
"#,
        "W9"
    ));
}

#[test]
fn w9_distinct_names_are_silent() {
    assert!(!has_warning(
        r#"
record Packet { v: u64 }
entity Holder {
    record Envelope { v: u64 }
    routes { constructor() => [] }
    m_p: HashMap<u64, Envelope> { in constructor() => {} }
}
"#,
        "W9"
    ));
}

#[test]
fn w6_deprecated_standalone_setcode() {
    assert!(has_warning(
        r#"
use gosh
entity Foo {
    routes {
        up(code: CamData) => [gosh::setcode(code)]
    }
    m_x: u64 {}
}
"#,
        "W6"
    ));
}

// ===================================================================
// T1: Test references nonexistent entity
// ===================================================================

#[test]
fn t1_entity_not_found_in_test() {
    assert!(has_error(
        r#"
entity Foo {
    routes { ping() => [] }
    m_x: u64 { in ping() => 0 }
}
test "bad" for NonExistent {
    call ping()
}
"#,
        "T1"
    ));
}

// ===================================================================
// T2: Test calls nonexistent route
// ===================================================================

#[test]
fn t2_route_not_found_in_test() {
    assert!(has_error(
        r#"
entity Foo {
    routes { ping() => [] }
    m_x: u64 { in ping() => 0 }
}
test "bad" for Foo {
    call nonexistent()
    expect state { m_x: 0 }
}
"#,
        "T2"
    ));
}

// ===================================================================
// T3: Test call with wrong number of arguments
// ===================================================================

#[test]
fn t3_wrong_arg_count_in_test() {
    assert!(has_error(
        r#"
entity Foo {
    routes { ping(n: u64) => [] }
    m_x: u64 { in ping(n) => n }
}
test "bad" for Foo {
    call ping()
    expect state { m_x: 0 }
}
"#,
        "T3"
    ));
}

// ===================================================================
// T4: Test expect references nonexistent member
// ===================================================================

#[test]
fn t4_unknown_field_in_expect() {
    assert!(has_error(
        r#"
entity Foo {
    routes { ping() => [] }
    m_x: u64 { in ping() => 0 }
}
test "bad" for Foo {
    call ping()
    expect state { m_nonexistent: 0 }
}
"#,
        "T4"
    ));
}

// ===================================================================
// T5: expect state before any call
// (Source: T5 = "expect before call", T6 = "no call at all")
// ===================================================================

#[test]
fn t5_expect_state_before_call() {
    assert!(has_error(
        r#"
entity Foo {
    routes { ping() => [] }
    m_x: u64 { in ping() => 0 }
}
test "bad" for Foo {
    expect state { m_x: 0 }
    call ping()
}
"#,
        "T5"
    ));
}

// ===================================================================
// T6: Test without any call (related to user's T5 request)
// ===================================================================

#[test]
fn t6_test_without_call() {
    assert!(has_error(
        r#"
entity Foo {
    routes { ping() => [] }
    m_x: u64 { in ping() => 0 }
}
test "bad" for Foo {
    expect state { m_x: 0 }
}
"#,
        "T6"
    ));
}

// ===================================================================
// T8: Unknown namespace in test set context (warning)
// ===================================================================

#[test]
fn t8_unknown_namespace_in_set_context() {
    assert!(has_warning(
        r#"
entity Foo {
    routes { ping() => [] }
    m_x: u64 { in ping() => 0 }
}
test "bad" for Foo {
    badns { some_field: 0 }
    call ping()
    expect state { m_x: 0 }
}
"#,
        "T8"
    ));
}

// ===================================================================
// VarCall validation
// ===================================================================

#[test]
fn var_call_valid_with_member_transform() {
    no_errors(
        r#"
entity Token {
    routes {
        constructor() => []
        view balanceOf(owner: address) -> U256 => [
            return(m_supply)
        ]
    }
    m_supply: U256 {
        in constructor() => 0
    }
}

entity Tracker {
    routes {
        constructor(token_addr: Address<Token>) => []
        refresh(who: address) => [
            fetch: [
                var bal = balanceOf(who) ~> m_token;
            ]
            apply: []
        ]
    }
    m_token: Address<Token> {
        in constructor(token_addr) => token_addr
    }
    m_cached: U256 {
        in constructor() => 0
        in refresh(who) => apply: bal
    }
}
"#,
    );
}

#[test]
fn var_call_v23_nonexistent_route() {
    assert!(
        has_error(
            r#"
entity Token {
    routes {
        constructor() => []
    }
}

entity Vault {
    routes {
        constructor(t: Address<Token>) => []
        query(who: address) => [
            var bal = nonExistent(who) ~> m_token;
        ]
    }
    m_token: Address<Token> {
        in constructor(t) => t
    }
}
"#,
            "V23"
        ),
        "var call to non-existent route should emit V23"
    );
}

#[test]
fn var_call_v24_void_route() {
    assert!(
        has_error(
            r#"
entity Token {
    routes {
        constructor() => []
        doSomething(x: U256) => []
    }
}

entity Vault {
    routes {
        constructor(t: Address<Token>) => []
        act(x: U256) => [
            var result = doSomething(x) ~> m_token;
        ]
    }
    m_token: Address<Token> {
        in constructor(t) => t
    }
}
"#,
            "V24"
        ),
        "var call to void route should emit V24"
    );
}

#[test]
fn var_call_v25_shadows_route_param() {
    assert!(
        has_error(
            r#"
entity Token {
    routes {
        constructor() => []
        view balanceOf(owner: address) -> U256 => [
            return(m_supply)
        ]
    }
    m_supply: U256 {
        in constructor() => 0
    }
}

entity Vault {
    routes {
        constructor(t: Address<Token>) => []
        query(bal: address) => [
            var bal = balanceOf(bal) ~> m_token;
        ]
    }
    m_token: Address<Token> {
        in constructor(t) => t
    }
}
"#,
            "V25"
        ),
        "var shadowing route param should emit V25"
    );
}

#[test]
fn var_call_v25_shadows_state_member() {
    assert!(
        has_error(
            r#"
entity Token {
    routes {
        constructor() => []
        view balanceOf(owner: address) -> U256 => [
            return(m_supply)
        ]
    }
    m_supply: U256 {
        in constructor() => 0
    }
}

entity Vault {
    routes {
        constructor(t: Address<Token>) => []
        query(who: address) => [
            var m_token = balanceOf(who) ~> m_token;
        ]
    }
    m_token: Address<Token> {
        in constructor(t) => t
    }
}
"#,
            "V25"
        ),
        "var shadowing state member should emit V25"
    );
}

#[test]
fn var_call_v25_duplicate_var_name() {
    assert!(
        has_error(
            r#"
entity Token {
    routes {
        constructor() => []
        view balanceOf(owner: address) -> U256 => [
            return(m_supply)
        ]
        view totalSupply() -> U256 => [
            return(m_supply)
        ]
    }
    m_supply: U256 {
        in constructor() => 0
    }
}

entity Vault {
    routes {
        constructor(t: Address<Token>) => []
        query(who: address) => [
            var x = balanceOf(who) ~> m_token;
            var x = totalSupply() ~> m_token;
        ]
    }
    m_token: Address<Token> {
        in constructor(t) => t
    }
}
"#,
            "V25"
        ),
        "duplicate var name in same route should emit V25"
    );
}

#[test]
fn var_call_v11_in_pure_route() {
    assert!(
        has_error(
            r#"
entity Token {
    routes {
        constructor() => []
        view balanceOf(owner: address) -> U256 => [
            return(m_supply)
        ]
    }
    m_supply: U256 {
        in constructor() => 0
    }
}

entity Vault {
    routes {
        constructor(t: Address<Token>) => []
        pure pureQuery(who: address) -> U256 => [
            var bal = balanceOf(who) ~> m_token;
            return(bal)
        ]
    }
    m_token: Address<Token> {
        in constructor(t) => t
    }
}
"#,
            "V11"
        ),
        "var call in pure route should emit V11"
    );
}

#[test]
fn var_call_v23_untyped_address() {
    assert!(
        has_error(
            r#"
entity Vault {
    routes {
        constructor() => []
        query(dest: address) => [
            var bal = balanceOf(dest) ~> dest;
        ]
    }
}
"#,
            "V23"
        ),
        "var call to untyped address should emit V23"
    );
}

#[test]
fn var_call_v26_unphased_route_with_var_in_transform() {
    assert!(
        has_error(
            r#"
entity Token {
    routes {
        constructor() => []
        view balanceOf(owner: address) -> U256 => [
            return(m_supply)
        ]
    }
    m_supply: U256 {
        in constructor() => 0
    }
}

entity Tracker {
    routes {
        constructor(token_addr: Address<Token>) => []
        refresh(who: address) => [
            var bal = balanceOf(who) ~> m_token;
        ]
    }
    m_token: Address<Token> {
        in constructor(token_addr) => token_addr
    }
    m_cached: U256 {
        in constructor() => 0
        in refresh(who) => bal
    }
}
"#,
            "V26"
        ),
        "var used in unphased member transform should emit V26"
    );
}

#[test]
fn var_call_v26_same_phase_var_in_transform() {
    assert!(
        has_error(
            r#"
entity Token {
    routes {
        constructor() => []
        view balanceOf(owner: address) -> U256 => [
            return(m_supply)
        ]
    }
    m_supply: U256 {
        in constructor() => 0
    }
}

entity Tracker {
    routes {
        constructor(token_addr: Address<Token>) => []
        refresh(who: address) => [
            fetch: [
                var bal = balanceOf(who) ~> m_token;
            ]
        ]
    }
    m_token: Address<Token> {
        in constructor(token_addr) => token_addr
    }
    m_cached: U256 {
        in constructor() => 0
        in refresh(who) => fetch: bal
    }
}
"#,
            "V26"
        ),
        "var used in same phase transform should emit V26"
    );
}

#[test]
fn var_call_v26_earlier_phase_var_in_transform_is_ok() {
    no_errors(
        r#"
entity Token {
    routes {
        constructor() => []
        view balanceOf(owner: address) -> U256 => [
            return(m_supply)
        ]
    }
    m_supply: U256 {
        in constructor() => 0
    }
}

entity Tracker {
    routes {
        constructor(token_addr: Address<Token>) => []
        refresh(who: address) => [
            fetch: [
                var bal = balanceOf(who) ~> m_token;
            ]
            update: []
        ]
    }
    m_token: Address<Token> {
        in constructor(token_addr) => token_addr
    }
    m_cached: U256 {
        in constructor() => 0
        in refresh(who) => update: bal
    }
}
"#,
    );
}

#[test]
fn var_call_v26_in_arithmetic_transform() {
    assert!(
        has_error(
            r#"
entity Token {
    routes {
        constructor() => []
        view totalSupply() -> U256 => [
            return(m_supply)
        ]
    }
    m_supply: U256 {
        in constructor() => 0
    }
}

entity Stats {
    routes {
        constructor(token_addr: Address<Token>) => []
        snapshot() => [
            var supply = totalSupply() ~> m_token;
        ]
    }
    m_token: Address<Token> {
        in constructor(token_addr) => token_addr
    }
    m_double: U256 {
        in constructor() => 0
        in snapshot() => supply * 2
    }
}
"#,
            "V26"
        ),
        "var used in arithmetic in unphased transform should emit V26"
    );
}

// ===================================================================
// Phases Q2A: per-phase where clause var-scoping diagnostics
// ===================================================================

/// Per-phase where on phase P0 may NOT reference a var defined in the same
/// phase P0. The var is not yet bound when the per-phase where is evaluated.
#[test]
fn phase_where_var_in_same_phase_emits_v28() {
    assert!(
        has_error(
            r#"
entity Token {
    routes {
        constructor() => []
        view balanceOf(o: address) -> U256 => [ return(m_s) ]
    }
    m_s: U256 { in constructor() => 0 }
}
entity V {
    routes {
        constructor(t: Address<Token>) => []
        go(who: address) => [
            fetch where bal > 0 : throw 1: [
                var bal = balanceOf(who) ~> m_t;
            ]
        ]
    }
    m_t: Address<Token> { in constructor(t) => t }
}
"#,
            "V28"
        ),
        "per-phase where referencing same-phase var should emit V28"
    );
}

/// Per-phase where on phase P0 may NOT reference a var defined in a LATER
/// phase. Phases run sequentially and earlier phases cannot see later vars.
#[test]
fn phase_where_var_in_later_phase_emits_v28() {
    assert!(
        has_error(
            r#"
entity Token {
    routes {
        constructor() => []
        view balanceOf(o: address) -> U256 => [ return(m_s) ]
    }
    m_s: U256 { in constructor() => 0 }
}
entity V {
    routes {
        constructor(t: Address<Token>) => []
        go(who: address) => [
            early where bal > 0 : throw 2: [
            ]
            late: [
                var bal = balanceOf(who) ~> m_t;
            ]
        ]
    }
    m_t: Address<Token> { in constructor(t) => t }
}
"#,
            "V28"
        ),
        "per-phase where referencing later-phase var should emit V28"
    );
}

/// Route-level where clauses are evaluated BEFORE any phase runs, so they
/// cannot reference vars (which only exist after their defining VarCall).
#[test]
fn route_where_referencing_var_emits_v27() {
    assert!(
        has_error(
            r#"
entity Token {
    routes {
        constructor() => []
        view balanceOf(o: address) -> U256 => [ return(m_s) ]
    }
    m_s: U256 { in constructor() => 0 }
}
entity V {
    routes {
        constructor(t: Address<Token>) => []
        go(who: address) where bal > 0 : throw 3 => [
            fetch: [
                var bal = balanceOf(who) ~> m_t;
            ]
        ]
    }
    m_t: Address<Token> { in constructor(t) => t }
}
"#,
            "V27"
        ),
        "route-level where referencing a var should emit V27"
    );
}

/// Positive control: per-phase where on a LATER phase referencing a var
/// from an EARLIER phase must validate cleanly (no V27/V28).
#[test]
fn phase_where_var_from_earlier_phase_is_ok() {
    let src = r#"
entity Token {
    routes {
        constructor() => []
        view balanceOf(o: address) -> U256 => [ return(m_s) ]
    }
    m_s: U256 { in constructor() => 0 }
}
entity V {
    routes {
        constructor(t: Address<Token>) => []
        go(who: address) => [
            fetch: [
                var bal = balanceOf(who) ~> m_t;
            ]
            act where bal > 0 : throw 4: [
            ]
        ]
    }
    m_t: Address<Token> { in constructor(t) => t }
}
"#;
    let errs = errors(src);
    assert!(
        !errs.contains(&"V27") && !errs.contains(&"V28"),
        "earlier-phase var should be visible in later per-phase where, got: {:?}",
        errs
    );
}

// ===========================================================================
// V29: action-level `for` loop body restrictions.
// ===========================================================================

#[test]
fn v29_action_for_var_call_in_body_rejected() {
    let src = r#"
entity B {
    routes {
        view ping(x: U256) -> U256 => [ return(x) ]
    }
}
entity A {
    routes {
        constructor(b: Address<B>) => []
        broadcast(targets: Vec<Address<B>>) => [
            for t in targets => [
                var r = ping(0) ~> t;
            ]
        ]
    }
    m_b: Address<B> { in constructor(b) => b }
}
"#;
    assert!(
        has_error(src, "V29"),
        "var call inside action-level for must be rejected with V29"
    );
}

#[test]
fn v29_action_for_return_in_body_rejected() {
    let src = r#"
entity A {
    routes {
        first(xs: Vec<U256>) -> U256 => [
            for x in xs => [
                return(x)
            ]
        ]
    }
}
"#;
    assert!(
        has_error(src, "V29"),
        "`return(...)` inside action-level for must be rejected with V29"
    );
}

#[test]
fn v29_action_for_pure_send_body_ok() {
    let src = r#"
entity A {
    routes {
        airdrop(amount: uint256, recipients: Vec<address>) => [
            for r in recipients => [
                ~> r with {value: amount}
            ]
        ]
    }
}
"#;
    let errs = errors(src);
    assert!(
        !errs.contains(&"V29"),
        "send-only body inside action-level for must validate cleanly, got: {:?}",
        errs
    );
}

#[test]
fn v29_action_for_nested_for_inherits_restriction() {
    let src = r#"
entity A {
    routes {
        nest(xs: Vec<U256>) -> U256 => [
            for x in xs => [
                for y in xs => [
                    return(y)
                ]
            ]
        ]
    }
}
"#;
    assert!(
        has_error(src, "V29"),
        "nested action-level for must inherit V29 restrictions"
    );
}

// ===========================================================================
// Phase EVM-2 K1/K2 (Cluster K) — silent-miscompile rejection (E15-E21)
// ===========================================================================
//
// These run against `check_evm_target_compat` (the EVM-target-specific
// pass), not the generic `validate()`. Helper below.

fn evm_compat_codes(src: &str) -> Vec<&'static str> {
    let prog = parse(src);
    cambrian_transpiler::validate::check_evm_target_compat(&prog)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect()
}

fn evm_compat_codes_det(src: &str) -> Vec<&'static str> {
    let prog = parse(src);
    cambrian_transpiler::validate::check_evm_target_compat_with(&prog, true)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect()
}

#[test]
fn evm2_e15_gosh_namespaced_call_in_expr_position_errors() {
    // `gosh::sha256(...)` in expression position previously lowered
    // to literal `0` (silent miscompile). E15 promotes that to a
    // hard error.
    let src = r#"
use gosh

entity X {
    routes {
        view get() -> u64 => [
            return(gosh::sha256(0))
        ]
    }
    m_dummy: u64 {}
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        codes.contains(&"E15"),
        "expected E15 for gosh:: in expr position, got {:?}",
        codes
    );
}

#[test]
fn evm2_e15_does_not_fire_for_evm_namespace() {
    // `evm::*` calls have a real lowering table (`gen_evm_ns`); E15
    // must only fire for `gosh::*`.
    let src = r#"
entity X {
    routes {
        view get() -> u64 => [
            return(evm::keccak256(0))
        ]
    }
    m_dummy: u64 {}
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        !codes.contains(&"E15"),
        "E15 must not fire for `evm::` namespace, got {:?}",
        codes
    );
}

#[test]
fn evm2_e26_rescue_recover_errors_on_evm_domain_only() {
    let src = r#"
entity Bouncer {
    routes {
        sendBounce(dest: Address<Bouncer>, value: u64) => [
            rescue bounce_failed: acceptValue(value) ~> dest
        ]
        acceptValue(value: u64) => []
        recover bounce_failed(body: CamData) => []
    }
    m_x: u64 {}
}
"#;
    let prog = parse(src);
    let evm = check_target_compat(&prog, Target::Evm, false);
    assert!(
        evm.iter().any(|d| d.code == "E26" && d.severity == Severity::Error),
        "E26 must reject rescue/recover on --target evm: {:?}",
        evm
    );
    let lean = check_target_compat(&prog, Target::Lean, false);
    assert!(
        lean.iter().any(|d| d.code == "E26" && d.severity == Severity::Error),
        "E26 must reject rescue/recover on --target lean: {:?}",
        lean
    );
    assert!(
        !lean.iter().any(|d| d.code == "L12"),
        "L12 is superseded by E26: {:?}",
        lean
    );
    let acki = check_target_compat(&prog, Target::AckiNacki, false);
    assert!(
        !acki.iter().any(|d| d.code == "E26"),
        "E26 must not fire on Acki Nacki: {:?}",
        acki
    );
    let native = check_target_compat(&prog, Target::Native, false);
    assert!(
        !native.iter().any(|d| d.code == "E26"),
        "E26 must not fire on Native (TVM-model testbed): {:?}",
        native
    );
}

#[test]
fn evm2_e16_address_of_outside_deterministic_mode_errors() {
    // `address_of E(args)` is only valid when `deterministic_addresses
    // = true`. Without it, the codegen returns None and we silently
    // emit `0`. E16 rejects the bare form.
    let src = r#"
entity Other {
    routes {
        view get() -> u64 => [ return(0) ]
    }
    m_id: u64 {}
}

entity X {
    routes {
        view who() -> address => [
            return(address_of Other(1))
        ]
    }
    m_dummy: u64 {}
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        codes.contains(&"E16"),
        "expected E16 for address_of in non-deterministic mode, got {:?}",
        codes
    );
}

#[test]
fn evm2_e16_address_of_in_deterministic_mode_ok() {
    let src = r#"
entity Other {
    routes {
        view get() -> u64 => [ return(0) ]
    }
    m_id: u64 {}
}

entity X {
    routes {
        view who() -> address => [
            return(address_of Other(1))
        ]
    }
    m_dummy: u64 {}
}
"#;
    let codes = evm_compat_codes_det(src);
    assert!(
        !codes.contains(&"E16"),
        "address_of must be accepted in deterministic mode, got {:?}",
        codes
    );
}

#[test]
fn evm2_e17_unrecognized_mapping_transform_shape_errors() {
    // A HashMap transform body that is neither insert/update/remove
    // (nor an if/block/let-wrapped variant) silently emits a no-op
    // comment. E17 catches that.
    let src = r#"
entity X {
    routes {
        push(k: address) => []
    }
    m_dummy: u64 {}
    m_map: HashMap<address, u64> {
        in push(k) => k as u64
    }
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        codes.contains(&"E17"),
        "expected E17 for unrecognised HashMap transform shape, got {:?}",
        codes
    );
}

#[test]
fn evm2_e17_recognised_insert_transform_ok() {
    let src = r#"
entity X {
    routes {
        push(k: address, v: u64) => []
    }
    m_dummy: u64 {}
    m_map: HashMap<address, u64> {
        in push(k, v) => m_map.insert(k, v)
    }
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        !codes.contains(&"E17"),
        "insert/update/remove must be accepted, got {:?}",
        codes
    );
}

#[test]
fn evm_p0_a_e23_let_tuple_destructure_unrecognised_rhs_errors() {
    // Phase EVM-P0-A (closes EVM_GAPS § 1.12). When the RHS shape
    // can't be tuple-typed (e.g. an arbitrary method call), the
    // codegen would silently widen every slot to `uint256`. E23
    // promotes that to a hard error.
    let src = r#"
entity X {
    routes {
        run(a: U256) => [
            let (q, r) = m_dummy.someUnknownCall(a);
            return(q + r)
        ]
        view dummy() -> U256 => [ return(0) ]
    }
    m_dummy: U256 {}
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        codes.contains(&"E23"),
        "expected E23 for unrecognised tuple-let RHS, got {:?}",
        codes
    );
}

#[test]
fn evm_p0_a_e23_does_not_fire_for_divmod() {
    let src = r#"
entity X {
    routes {
        run(a: U256, b: U256) => [
            let (q, r) = divmod(a, b);
            return(q + r)
        ]
        view dummy() -> U256 => [ return(0) ]
    }
    m_dummy: U256 {}
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        !codes.contains(&"E23"),
        "E23 must not fire for divmod, got {:?}",
        codes
    );
}

#[test]
fn evm_p0_a_e23_does_not_fire_for_known_pure_fn() {
    let src = r#"
pure fn split(a: U256) -> (U256, U256) {
    (a, a)
}

entity X {
    routes {
        run(a: U256) => [
            let (q, r) = split(a);
            return(q + r)
        ]
        view dummy() -> U256 => [ return(0) ]
    }
    m_dummy: U256 {}
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        !codes.contains(&"E23"),
        "E23 must not fire for known pure-fn returning tuple, got {:?}",
        codes
    );
}

#[test]
fn evm_p0_a_e23_does_not_fire_for_tuple_literal() {
    let src = r#"
entity X {
    routes {
        run(a: U256, b: U256) => [
            let (q, r) = (a, b);
            return(q + r)
        ]
        view dummy() -> U256 => [ return(0) ]
    }
    m_dummy: U256 {}
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        !codes.contains(&"E23"),
        "E23 must not fire for tuple literal RHS, got {:?}",
        codes
    );
}

#[test]
fn evm2_e18_tuple_storage_member_errors() {
    // Tuple-typed storage / param positions silently lower to `bytes`.
    let src = r#"
entity X {
    routes {
        view get() -> u64 => [ return(0) ]
    }
    m_pair: (u64, u64) {}
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        codes.contains(&"E18"),
        "expected E18 for tuple storage, got {:?}",
        codes
    );
}

#[test]
fn evm2_e18_tuple_in_return_type_is_allowed() {
    // Multi-return is the legitimate Solidity lowering of route /
    // pure-fn tuple returns; E18 must NOT fire there.
    let src = r#"
entity X {
    routes {
        view get_pair() -> (u64, u64) => [ return(0, 0) ]
    }
    m_dummy: u64 {}
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        !codes.contains(&"E18"),
        "E18 must not fire for tuple in return position (multi-return), got {:?}",
        codes
    );
}

#[test]
fn evm2_e19_unknown_generic_errors() {
    let src = r#"
entity X {
    routes {
        view get() -> u64 => [ return(0) ]
    }
    m_box: Box<u64> {}
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        codes.contains(&"E19"),
        "expected E19 for unknown generic Box<...>, got {:?}",
        codes
    );
}

#[test]
fn evm2_e19_known_generics_allowed() {
    let src = r#"
entity X {
    routes {
        view get() -> u64 => [ return(0) ]
    }
    m_xs: Vec<u64> {}
    m_map: HashMap<address, u64> {}
    m_opt: Option<u64> {}
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        !codes.contains(&"E19"),
        "Vec/HashMap/Option must be accepted, got {:?}",
        codes
    );
}

#[test]
fn evm2_e20_unknown_simple_type_errors() {
    let src = r#"
entity X {
    routes {
        view get() -> u64 => [ return(0) ]
    }
    m_unknown: TotallyMadeUpType {}
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        codes.contains(&"E20"),
        "expected E20 for unknown Simple type, got {:?}",
        codes
    );
}

#[test]
fn evm2_e20_type_alias_resolves_through_program() {
    let src = r#"
type Amount = U256

entity X {
    routes {
        view get() -> u64 => [ return(0) ]
    }
    m_total: Amount {}
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        !codes.contains(&"E20"),
        "type aliases must be accepted, got {:?}",
        codes
    );
}

#[test]
fn lean_l15_unrecognized_hashmap_transform_shape_errors() {
    let src = r#"
entity X {
    routes {
        push(k: address) => []
    }
    m_dummy: u64 {}
    m_map: HashMap<address, u64> {
        in push(k) => k as u64
    }
}
"#;
    let prog = parse(src);
    let codes: Vec<_> = cambrian_transpiler::validate::check_lean_target_compat(&prog)
        .into_iter()
        .map(|d| d.code)
        .collect();
    assert!(
        codes.contains(&"L15"),
        "expected L15 for unrecognised HashMap transform shape on Lean, got {:?}",
        codes
    );
}

#[test]
fn lean_l15_recognised_insert_transform_ok() {
    let src = r#"
entity X {
    routes {
        push(k: address, v: u64) => []
    }
    m_dummy: u64 {}
    m_map: HashMap<address, u64> {
        in push(k, v) => m_map.insert(k, v)
    }
}
"#;
    let prog = parse(src);
    let codes: Vec<_> = cambrian_transpiler::validate::check_lean_target_compat(&prog)
        .into_iter()
        .map(|d| d.code)
        .collect();
    assert!(
        !codes.contains(&"L15"),
        "insert/update/remove must be accepted on Lean, got {:?}",
        codes
    );
}

#[test]
fn lean_l14_unknown_simple_type_errors() {
    let src = r#"
entity X {
    routes {
        view get() -> u64 => [ return(0) ]
    }
    m_unknown: TotallyMadeUpType {}
}
"#;
    let prog = parse(src);
    let codes: Vec<_> = cambrian_transpiler::validate::check_lean_target_compat(&prog)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect();
    assert!(
        codes.contains(&"L14"),
        "expected L14 for unknown Simple type on Lean, got {:?}",
        codes
    );
    // Solidity still rejects via E20; Lean must not fire E20.
    let evm = evm_compat_codes(src);
    assert!(evm.contains(&"E20"));
    assert!(!evm.contains(&"L14"));
}

#[test]
fn lean_l14_type_alias_and_string_ok() {
    let src = r#"
type Amount = U256

entity X {
    routes {
        view get() -> u64 => [ return(0) ]
    }
    m_total: Amount {}
    m_label: string {}
}
"#;
    let prog = parse(src);
    let codes: Vec<_> = cambrian_transpiler::validate::check_lean_target_compat(&prog)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect();
    assert!(
        !codes.contains(&"L14"),
        "aliases and lowercase string must be accepted, got {:?}",
        codes
    );
}

#[test]
fn evm2_e21_non_primitive_cast_errors() {
    let src = r#"
entity X {
    routes {
        view get() -> u64 => [
            let s = 0 as String;
            return(0)
        ]
    }
    m_dummy: u64 {}
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        codes.contains(&"E21"),
        "expected E21 for `as String` cast, got {:?}",
        codes
    );
}

#[test]
fn evm2_e21_alias_to_primitive_cast_ok() {
    // `as Amount` where `type Amount = U256` resolves to a real
    // primitive cast — must not flag.
    let src = r#"
type Amount = U256

entity X {
    routes {
        view get() -> u64 => [
            let a = 1 as Amount;
            return(0)
        ]
    }
    m_dummy: u64 {}
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        !codes.contains(&"E21"),
        "alias-to-primitive cast must be accepted, got {:?}",
        codes
    );
}

// ===========================================================================
// Phase EVM-6 M1 — extern entity well-formedness (V30 / V31 / E22)
// ===========================================================================

#[test]
fn evm6_v30_duplicate_extern_entity_name_errors() {
    let src = r#"
extern entity Token {
    route transfer(amount: U256);
}

extern entity Token {
    route mint(amount: U256);
}

entity Caller {
    routes { constructor() => [] }
    m_dummy: u64 {}
}
"#;
    assert!(
        has_error(src, "V30"),
        "expected V30 for duplicate extern entity, got {:?}",
        errors(src)
    );
}

#[test]
fn evm6_v30_extern_collides_with_real_entity_errors() {
    let src = r#"
extern entity Token {
    route transfer(amount: U256);
}

entity Token {
    routes { constructor() => [] }
    m_supply: U256 {}
}
"#;
    assert!(
        has_error(src, "V30"),
        "expected V30 for extern/real collision, got {:?}",
        errors(src)
    );
}

#[test]
fn evm6_v31_duplicate_route_in_extern_entity_errors() {
    let src = r#"
extern entity Token {
    route transfer(amount: U256);
    route transfer(to: address);
}

entity Caller {
    routes { constructor() => [] }
    m_dummy: u64 {}
}
"#;
    assert!(
        has_error(src, "V31"),
        "expected V31 for duplicate extern route, got {:?}",
        errors(src)
    );
}

#[test]
fn evm6_e22_typed_send_to_undeclared_external_entity_errors() {
    // Without an `extern entity Token { ... }` declaration, the typed
    // send `transfer(amount) ~> m_token` can't lower to a working
    // Solidity call (the empty interface stub omits `transfer`). E22
    // promotes this to a hard validator error.
    let src = r#"
entity Caller {
    routes {
        constructor(t: Address<Token>) => []
        ping(amount: U256) => [
            transfer(amount) ~> m_token
        ]
    }
    m_token: Address<Token> {
        in constructor(t) => t
    }
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        codes.contains(&"E22"),
        "expected E22 for undeclared external entity, got {:?}",
        codes
    );
}

#[test]
fn evm6_e22_does_not_fire_when_extern_entity_declared() {
    let src = r#"
extern entity Token {
    route transfer(amount: U256);
}

entity Caller {
    routes {
        constructor(t: Address<Token>) => []
        ping(amount: U256) => [
            transfer(amount) ~> m_token
        ]
    }
    m_token: Address<Token> {
        in constructor(t) => t
    }
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        !codes.contains(&"E22"),
        "E22 must not fire when extern entity is declared, got {:?}",
        codes
    );
}

#[test]
fn evm6_e22_does_not_fire_for_in_program_entity() {
    let src = r#"
entity Token {
    routes {
        constructor() => []
        transfer(amount: U256) => []
    }
    m_supply: U256 {}
}

entity Caller {
    routes {
        constructor(t: Address<Token>) => []
        ping(amount: U256) => [
            transfer(amount) ~> m_token
        ]
    }
    m_token: Address<Token> {
        in constructor(t) => t
    }
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        !codes.contains(&"E22"),
        "E22 must not fire for in-program entity targets, got {:?}",
        codes
    );
}

// ===========================================================================
// Phase EVM-6 M2 — V32 (deploy arity) and V33 (from-clause arity)
// ===========================================================================

#[test]
fn evm6_v32_deploy_too_few_args_errors() {
    // Child has 1 identity member + constructor takes 1 param → expected
    // arity = 2. Deploying with 1 arg must fail.
    let src = r#"
entity Child {
    identity m_id: u64
    routes {
        constructor(n: u64) => []
    }
    m_n: u64 {
        in constructor(n) => n
    }
}

entity Factory {
    routes {
        spawn(id: u64) => [
            deploy Child (id)
        ]
    }
    m_dummy: u64 {}
}
"#;
    assert!(
        has_error(src, "V32"),
        "expected V32 for too-few deploy args, got {:?}",
        errors(src)
    );
}

#[test]
fn evm6_v32_deploy_too_many_args_errors() {
    let src = r#"
entity Child {
    routes {
        constructor(owner: address) => []
    }
    m_owner: address {
        in constructor(owner) => owner
    }
}

entity Factory {
    routes {
        spawn(owner: address) => [
            deploy Child (owner, 42)
        ]
    }
    m_dummy: u64 {}
}
"#;
    assert!(
        has_error(src, "V32"),
        "expected V32 for too-many deploy args, got {:?}",
        errors(src)
    );
}

#[test]
fn evm6_v32_deploy_correct_arity_no_error() {
    let src = r#"
entity Child {
    identity m_id: u64
    routes {
        constructor(n: u64) => []
    }
    m_n: u64 {
        in constructor(n) => n
    }
}

entity Factory {
    routes {
        spawn(id: u64, n: u64) => [
            deploy Child (id, n)
        ]
    }
    m_dummy: u64 {}
}
"#;
    let codes = errors(src);
    assert!(
        !codes.contains(&"V32"),
        "V32 must not fire for correct arity, got {:?}",
        codes
    );
}

#[test]
fn evm6_v32_deploy_to_extern_entity_skipped() {
    // Extern entity signatures are unknown — V32 must not fire.
    let src = r#"
extern entity Child {
    route initialize(x: U256);
}

entity Factory {
    routes {
        spawn() => [
            deploy Child (1, 2, 3, 4, 5)
        ]
    }
    m_dummy: u64 {}
}
"#;
    let codes = errors(src);
    assert!(
        !codes.contains(&"V32"),
        "V32 must not fire for extern entity deploy, got {:?}",
        codes
    );
}

#[test]
fn evm6_v33_from_entity_multi_arg_non_det_errors() {
    let src = r#"
entity Owner {
    identity m_id: u64
    routes { constructor() => [] }
}

entity Vault {
    routes {
        withdraw(amount: u64)
            from Owner(1, 2) => []
    }
    m_balance: u64 {}
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        codes.contains(&"V33"),
        "expected V33 for multi-arg from-clause in non-deterministic mode, got {:?}",
        codes
    );
}

#[test]
fn evm6_v33_from_entity_single_arg_non_det_ok() {
    let src = r#"
entity Owner {
    routes { constructor() => [] }
    m_dummy: u64 {}
}

entity Vault {
    routes {
        withdraw(addr: address)
            from Owner(addr) => []
    }
    m_balance: u64 {}
}
"#;
    let codes = evm_compat_codes(src);
    assert!(
        !codes.contains(&"V33"),
        "V33 must not fire for single-arg from-clause in non-deterministic mode, got {:?}",
        codes
    );
}

#[test]
fn evm6_v33_from_entity_wrong_arity_det_errors() {
    let src = r#"
entity Owner {
    identity m_id: u64
    identity m_role: u8
    routes { constructor() => [] }
}

entity Vault {
    routes {
        withdraw(amount: u64)
            from Owner(1) => []
    }
    m_balance: u64 {}
}
"#;
    let codes = evm_compat_codes_det(src);
    assert!(
        codes.contains(&"V33"),
        "expected V33 in deterministic mode when args.len() != identity_count, got {:?}",
        codes
    );
}

#[test]
fn evm6_v33_from_entity_correct_arity_det_ok() {
    let src = r#"
entity Owner {
    identity m_id: u64
    identity m_role: u8
    routes { constructor() => [] }
}

entity Vault {
    routes {
        withdraw(amount: u64)
            from Owner(1, 2) => []
    }
    m_balance: u64 {}
}
"#;
    let codes = evm_compat_codes_det(src);
    assert!(
        !codes.contains(&"V33"),
        "V33 must not fire when args match identity count in deterministic mode, got {:?}",
        codes
    );
}

// ===========================================================================
// BUG-U4 — V62/V63/V64 (factory deploy init-route rules)
// ===========================================================================

fn det_src_with_factory_only(body: &str) -> String {
    format!(
        r#"entity Token {{
    routes {{
        #[factory_only]
        {body}
    }}
    m_x: u64 {{ in constructor() => 0 }}
}}"#,
        body = body
    )
}

#[test]
fn bug_u4_v63_missing_factory_only_det_errors() {
    let src = r#"
entity Token {
    routes {
        constructor() => []
    }
    m_x: u64 { in constructor() => 0 }
}
"#;
    let codes = evm_compat_codes_det(src);
    assert!(
        codes.contains(&"V63"),
        "expected V63 when init route lacks #[factory_only] under deterministic mode, got {:?}",
        codes
    );
    assert!(
        !evm_compat_codes(src).contains(&"V63"),
        "V63 must not fire without deterministic_addresses, got {:?}",
        evm_compat_codes(src)
    );
}

#[test]
fn bug_u4_v63_factory_only_on_init_ok() {
    let src = det_src_with_factory_only("constructor() => []");
    let codes = evm_compat_codes_det(&src);
    assert!(
        !codes.contains(&"V63"),
        "V63 must not fire when #[factory_only] is present, got {:?}",
        codes
    );
}

#[test]
fn bug_u4_v64_factory_only_on_non_init_errors() {
    let src = r#"
entity Token {
    routes {
        constructor() => []
        #[factory_only]
        transfer(to: address) => []
    }
    m_x: u64 {}
}
"#;
    assert!(
        has_error(src, "V64"),
        "expected V64 for #[factory_only] on non-init route, got {:?}",
        errors(src)
    );
}

// ===================================================================
// V65: Option arithmetic is undefined
// ===================================================================

#[test]
fn option_arithmetic_inline_some_is_error() {
    assert!(has_error(
        r#"
        entity E {
            routes {
                constructor() => []
                go() -> u64 => [
                    return(some(1) + 2)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
        "#,
        "V65",
    ));
}

#[test]
fn option_arithmetic_let_binding_is_error() {
    assert!(has_error(
        r#"
        entity E {
            routes {
                constructor() => []
                go(n: u64) -> u64 => [
                    let tagged = some(n);
                    return(tagged + n)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
        "#,
        "V65",
    ));
}

#[test]
fn option_arithmetic_pure_fn_is_error() {
    assert!(has_error(
        r#"
        pure fn add_opt(x: Option<u64>) -> u64 { x + 1 }
        entity E {
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }
        "#,
        "V65",
    ));
}

#[test]
fn unwrapped_option_arithmetic_ok() {
    assert!(!has_error(
        r#"
        entity E {
            routes {
                constructor() => []
                go(n: u64) -> u64 => [
                    let tagged = some(n);
                    let picked = n + 3;
                    return(picked + n)
                ]
            }
            m_n: u64 { in constructor() => 0 in go(_) => m_n }
        }
        "#,
        "V65",
    ));
}

#[test]
fn bug_u4_v62_init_transform_msg_sender_det_errors() {
    let src = r#"
entity Token {
    routes {
        #[factory_only]
        constructor() => []
    }
    m_owner: address {
        in constructor() => msg::sender
    }
}
"#;
    let codes = evm_compat_codes_det(src);
    assert!(
        codes.contains(&"V62"),
        "expected V62 for msg::sender in constructor transform under deterministic mode, got {:?}",
        codes
    );
    assert!(
        !evm_compat_codes(src).contains(&"V62"),
        "V62 must not fire without deterministic_addresses, got {:?}",
        evm_compat_codes(src)
    );
}

#[test]
fn bug_u4_v62_init_route_body_msg_sender_det_errors() {
    let src = r#"
entity Vault {
    routes {
        #[factory_only]
        constructor(seed: U256) => [
            ~> msg::sender
        ]
    }
    m_seed: U256 {
        in constructor(seed) => seed
    }
}
"#;
    let codes = evm_compat_codes_det(src);
    assert!(
        codes.contains(&"V62"),
        "expected V62 for msg::sender in constructor body under deterministic mode, got {:?}",
        codes
    );
}

#[test]
fn bug_u4_v62_explicit_holder_param_ok() {
    let src = r#"
entity Token {
    routes {
        #[factory_only]
        constructor(holder: address) => []
    }
    m_owner: address {
        in constructor(holder) => holder
    }
}
"#;
    let codes = evm_compat_codes_det(src);
    assert!(
        !codes.contains(&"V62"),
        "V62 must not fire when init route uses explicit param, got {:?}",
        codes
    );
}

#[test]
fn bug_u4_v62_non_init_route_msg_sender_ok() {
    let src = r#"
entity Token {
    routes {
        #[factory_only]
        constructor() => []
        transfer(to: address, amount: U256) => []
    }
    m_balances: HashMap<address, U256> {}
}
"#;
    let codes = evm_compat_codes_det(src);
    assert!(
        !codes.contains(&"V62"),
        "V62 must not fire on non-init routes, got {:?}",
        codes
    );
}

/// Recursively collect project yaml files under a fixture tree.
fn collect_fixture_yamls(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let entries = std::fs::read_dir(dir).expect("read_dir fixtures");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_fixture_yamls(&path, out);
        } else if path.extension().is_some_and(|e| e == "yaml" || e == "yml") {
            out.push(path);
        }
    }
}

fn assert_det_evm_fixtures_have_factory_only(
    yamls: &[std::path::PathBuf],
    v62_allowlist: &[&str],
    label: &str,
) {
    assert!(!yamls.is_empty(), "expected at least one deterministic EVM {label} yaml");
    let mut v63_failures = Vec::new();
    let mut v62_failures = Vec::new();
    for yaml in yamls {
        let name = yaml.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let diags = evm_compat_diags_det_from_yaml(yaml);
        if diags.iter().any(|d| d.code == "V63") {
            v63_failures.push(yaml.display().to_string());
        }
        if diags.iter().any(|d| d.code == "V62")
            && !v62_allowlist.iter().any(|a| name.ends_with(a))
        {
            v62_failures.push(yaml.display().to_string());
        }
    }
    assert!(
        v63_failures.is_empty(),
        "V63 on {label} det EVM fixtures (add #[factory_only] on init routes): {:?}",
        v63_failures
    );
    assert!(
        v62_failures.is_empty(),
        "unexpected V62 on {label} det EVM fixtures (fix or allowlist): {:?}",
        v62_failures
    );
}

fn yaml_is_det_evm_project(path: &std::path::Path) -> bool {
    let text = std::fs::read_to_string(path).expect("read project yaml");
    text.contains("target: evm")
        && (text.contains("deterministic_addresses: true")
            || text.contains("deterministic_addresses:true"))
}

fn evm_compat_diags_det_from_yaml(path: &std::path::Path) -> Vec<Diagnostic> {
    let merged = cambrian_transpiler::project::Project::load_merged_pre_catalog(path)
        .expect("load project yaml for evm compat");
    cambrian_transpiler::validate::check_evm_target_compat_with(&merged, true)
}

/// Intentional `msg::sender` in init routes for harness-semantics probes (T-ARCH-015/029).
/// Pending redesign under factory deploy — V63 satisfied via `#[factory_only]`.
const V62_AUDIT_FIXTURE_ALLOWLIST: &[&str] = &[
    "ax01_04_deploy_sender_evm.yaml",
    "ax04_07_ctor_fusion_owner_evm.yaml",
];

#[test]
fn bug_u4_v63_audit_det_evm_fixtures_have_factory_only() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit/fixtures");
    let mut yamls = Vec::new();
    collect_fixture_yamls(&root, &mut yamls);
    yamls.retain(|p| yaml_is_det_evm_project(p));
    assert_det_evm_fixtures_have_factory_only(&yamls, V62_AUDIT_FIXTURE_ALLOWLIST, "audit");
}

#[test]
fn bug_u4_v63_harness_det_evm_fixtures_have_factory_only() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let yamls = [
        manifest.join("tests/fixtures/evm_forge_harness/RehearsalToken/project.deterministic.yaml"),
        manifest.join("tests/fixtures/smafd_canonical/project.evm.yaml"),
        manifest.join("tests/fixtures/factory_deploy_mint/project.yaml"),
        manifest.join("tests/fixtures/invariant_deploy_parity/project.evm.yaml"),
    ];
    for yaml in &yamls {
        assert!(yaml.is_file(), "missing harness fixture yaml: {}", yaml.display());
        assert!(
            yaml_is_det_evm_project(yaml),
            "harness corpus yaml must be det EVM: {}",
            yaml.display()
        );
    }
    assert_det_evm_fixtures_have_factory_only(&yamls, &[], "harness");
}

#[test]
fn bug_u4_v63_product_det_evm_fixtures_have_factory_only() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest.join("..");
    let mut yamls: Vec<std::path::PathBuf> = std::fs::read_dir(repo.join("contracts"))
        .expect("read contracts")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "yaml")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("det_"))
        })
        .collect();
    yamls.sort();
    yamls.push(repo.join("examples/governor/project.yaml"));
    yamls.push(repo.join("examples/uniswap-v2/project.yaml"));
    for yaml in &yamls {
        assert!(yaml.is_file(), "missing product det yaml: {}", yaml.display());
        assert!(
            yaml_is_det_evm_project(yaml),
            "product yaml must be det EVM: {}",
            yaml.display()
        );
    }
    assert_det_evm_fixtures_have_factory_only(&yamls, &[], "product");
}

#[test]
fn bug_u4_v63_stdlib_det_evm_fixtures_have_factory_only() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest.join("..");
    let yamls = [
        repo.join("stdlib/token/project.yaml"),
        repo.join("stdlib/vault/project.yaml"),
    ];
    for yaml in &yamls {
        assert!(yaml.is_file(), "missing stdlib det yaml: {}", yaml.display());
        assert!(
            yaml_is_det_evm_project(yaml),
            "stdlib yaml must be det EVM: {}",
            yaml.display()
        );
    }
    assert_det_evm_fixtures_have_factory_only(&yamls, &[], "stdlib");
}

#[test]
fn evm6_v33_from_entity_extern_skipped() {
    // From-clauses to extern entities are skipped (signature unknown).
    let src = r#"
extern entity Owner {
    route ping();
}

entity Vault {
    routes {
        withdraw(amount: u64)
            from Owner(1, 2, 3) => []
    }
    m_balance: u64 {}
}
"#;
    let codes_nd = evm_compat_codes(src);
    let codes_det = evm_compat_codes_det(src);
    assert!(
        !codes_nd.contains(&"V33") && !codes_det.contains(&"V33"),
        "V33 must not fire for from-clauses against extern entities, non-det={:?} det={:?}",
        codes_nd,
        codes_det
    );
}

#[test]
fn evm6_v23_extern_route_lookup_succeeds() {
    // V23 normally reports "route X not found on entity Y" when the
    // typed-send target's route doesn't exist. With the extern entity
    // fallback in place, V23 should look up the route in the extern
    // entity's declared signatures too.
    let src = r#"
extern entity Token {
    route mint(to: address, amount: U256);
}

entity Caller {
    routes {
        constructor(t: Address<Token>) => []
        ping(amount: U256) => [
            transfer(amount) ~> m_token
        ]
    }
    m_token: Address<Token> {
        in constructor(t) => t
    }
}
"#;
    // `transfer` is not in the extern entity's routes, so V23 should fire.
    let errs = errors(src);
    assert!(
        errs.contains(&"V23"),
        "expected V23 when extern route is missing, got {:?}",
        errs
    );
}

// ---------------------------------------------------------------------------
// Phase EVM-P0-C: events + emit + indexed parameters
// ---------------------------------------------------------------------------

#[test]
fn v34_emit_undeclared_event() {
    let src = r#"
entity Token {
    routes {
        ping() => [ emit Missing(0); ]
    }
    m_x: U256 {}
}
"#;
    let errs = errors(src);
    assert!(
        errs.contains(&"V34"),
        "expected V34 for undeclared event, got {:?}",
        errs
    );
}

#[test]
fn v35_emit_arity_mismatch() {
    let src = r#"
entity Token {
    event Ping(a: U256, b: U256);
    routes {
        bump() => [ emit Ping(1); ]
    }
    m_x: U256 {}
}
"#;
    let errs = errors(src);
    assert!(
        errs.contains(&"V35"),
        "expected V35 for arity mismatch, got {:?}",
        errs
    );
}

#[test]
fn v36_too_many_indexed_params() {
    // V36 is the EVM ABI-log topic limit — Domains([Evm]), so it fires via
    // check_target_compat on EVM-domain targets, not in target-less validate.
    let src = r#"
entity Token {
    event Bad(indexed a: address, indexed b: address, indexed c: address, indexed d: address);
    routes { ping() => [] }
    m_x: U256 {}
}
"#;
    let prog = parse(src);
    assert!(
        !errors(src).contains(&"V36"),
        "V36 must not fire in target-less validate"
    );
    for target in [Target::Evm, Target::Lean] {
        let errs: Vec<_> = check_target_compat(&prog, target, false)
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.code)
            .collect();
        assert!(
            errs.contains(&"V36"),
            "expected V36 for >3 indexed params on {target:?}, got {errs:?}"
        );
    }
    let ack: Vec<_> = check_target_compat(&prog, Target::AckiNacki, false)
        .into_iter()
        .map(|d| d.code)
        .collect();
    assert!(
        !ack.contains(&"V36"),
        "V36 must not fire on the TVM domain, got {ack:?}"
    );
}

#[test]
fn evm_p0_c_valid_event_emit_passes() {
    let src = r#"
entity Token {
    event Transfer(indexed src: address, indexed dst: address, value: U256);
    routes {
        send(dst: address, amount: U256) => [
            emit Transfer(msg::sender, dst, amount);
        ]
    }
    m_x: U256 {}
}
"#;
    let errs = errors(src);
    let unrelated: Vec<_> = errs
        .iter()
        .filter(|c| matches!(**c, "V34" | "V35"))
        .collect();
    assert!(
        unrelated.is_empty(),
        "well-formed event/emit unexpectedly errored: {:?}",
        errs
    );
    let evm: Vec<_> = check_target_compat(&parse(src), Target::Evm, false)
        .into_iter()
        .map(|d| d.code)
        .collect();
    assert!(
        !evm.contains(&"V36"),
        "3 indexed params must pass V36: {evm:?}"
    );
}

// ---------------------------------------------------------------------------
// Phase EVM-P0-D: custom errors + throw
// ---------------------------------------------------------------------------

#[test]
fn v38_throw_undeclared_error() {
    let src = r#"
entity Vault {
    routes {
        ping() => [ throw Missing() ]
    }
    m_x: U256 {}
}
"#;
    let errs = errors(src);
    assert!(
        errs.contains(&"V38"),
        "expected V38 for undeclared error, got {:?}",
        errs
    );
}

#[test]
fn v39_throw_arity_mismatch() {
    let src = r#"
entity Vault {
    error Insufficient(have: U256, need: U256);
    routes {
        bump() => [ throw Insufficient(1) ]
    }
    m_x: U256 {}
}
"#;
    let errs = errors(src);
    assert!(
        errs.contains(&"V39"),
        "expected V39 for arity mismatch, got {:?}",
        errs
    );
}

#[test]
fn evm_p0_d_valid_custom_error_passes() {
    let src = r#"
entity Vault {
    error Insufficient(have: U256, need: U256);
    error Unauthorized();
    routes {
        withdraw(amount: U256) where amount > 0 : throw Unauthorized() => [
            throw Insufficient(amount, m_x)
        ]
    }
    m_x: U256 {}
}
"#;
    let errs = errors(src);
    let unrelated: Vec<_> = errs
        .iter()
        .filter(|c| matches!(**c, "V38" | "V39"))
        .collect();
    assert!(
        unrelated.is_empty(),
        "well-formed custom error unexpectedly errored: {:?}",
        errs
    );
}

// ---------------------------------------------------------------------------
// Phase EVM-P0-E: receive / fallback validation (EVM target only)
// ---------------------------------------------------------------------------

#[test]
fn v40_receive_must_be_accept() {
    let src = r#"
entity Vault {
    routes {
        receive() => []
    }
    m_x: U256 {}
}
"#;
    let errs = evm_compat_codes(src);
    assert!(
        errs.contains(&"V40"),
        "expected V40 when receive is not accept, got {:?}",
        errs
    );
}

#[test]
fn v40_receive_with_params_rejected() {
    let src = r#"
entity Vault {
    routes {
        accept receive(amount: U256) => []
    }
    m_x: U256 {}
}
"#;
    let errs = evm_compat_codes(src);
    assert!(
        errs.contains(&"V40"),
        "expected V40 for receive with params, got {:?}",
        errs
    );
}

#[test]
fn v41_duplicate_receive() {
    let src = r#"
entity Vault {
    routes {
        accept receive() => []
        accept receive() => []
    }
    m_x: U256 {}
}
"#;
    let errs = evm_compat_codes(src);
    assert!(
        errs.contains(&"V41"),
        "expected V41 for duplicate receive, got {:?}",
        errs
    );
}

#[test]
fn v41_duplicate_fallback() {
    let src = r#"
entity Vault {
    routes {
        fallback() => []
        fallback() => []
    }
    m_x: U256 {}
}
"#;
    let errs = evm_compat_codes(src);
    assert!(
        errs.contains(&"V41"),
        "expected V41 for duplicate fallback, got {:?}",
        errs
    );
}

#[test]
fn evm_p0_e_valid_receive_passes() {
    let src = r#"
entity Vault {
    routes {
        accept receive() => []
        fallback() => []
    }
    m_x: U256 {}
}
"#;
    let errs = evm_compat_codes(src);
    let unrelated: Vec<_> = errs
        .iter()
        .filter(|c| matches!(**c, "V40" | "V41"))
        .collect();
    assert!(
        unrelated.is_empty(),
        "well-formed receive/fallback unexpectedly errored: {:?}",
        errs
    );
}

#[test]
fn evm_p0_e_non_evm_target_does_not_enforce_v40() {
    // `receive()` and `fallback()` are normal route names on TVM
    // (Acki Nacki) target. The base `validate()` pass must not flag
    // V40/V41 — these checks belong to the EVM-target compat pass.
    let src = r#"
entity Vault {
    routes {
        receive() => []
        fallback() => []
    }
    m_x: U256 {}
}
"#;
    let errs = errors(src);
    let related: Vec<_> = errs
        .iter()
        .filter(|c| matches!(**c, "V40" | "V41"))
        .collect();
    assert!(
        related.is_empty(),
        "V40/V41 must not fire from base validate() — they're EVM-only: {:?}",
        errs
    );
}

// ===================================================================
// L16: namespaced call with no Lean lowering (UPSTREAM B-33)
// ===================================================================

fn lean_compat_codes(src: &str) -> Vec<&'static str> {
    let prog = parse(src);
    check_target_compat(&prog, Target::Lean, false)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect()
}

#[test]
fn l16_muldivmod_rejected_on_lean() {
    let src = r#"
entity E {
    routes {
        constructor() => []
        go(a: u64, b: u64, m: u64) => [
            let q = std::math::muldivmod(a, b, m);
            return()
        ]
        view peek() -> u64 => [ return(m_x) ]
    }
    m_x: u64 { in constructor() => 0 in go(a, b, m) => a + b + m }
}
"#;
    let errs = lean_compat_codes(src);
    assert!(
        errs.contains(&"L16"),
        "expected L16 for std::math::muldivmod on Lean, got {errs:?}"
    );
    assert!(
        !errs.contains(&"L13"),
        "muldivmod must not be L13 (crypto-only): {errs:?}"
    );
}

#[test]
fn l16_format_wrong_arity_rejected_on_lean() {
    let src = r#"
entity E {
    routes {
        constructor() => []
        go(a: u64, b: u64, c: u64) => [
            let s = std::str::format(a, b, c);
            return()
        ]
    }
    m_x: u64 { in constructor() => 0 in go(_, _, _) => m_x + 1 }
}
"#;
    let errs = lean_compat_codes(src);
    assert!(
        errs.contains(&"L16"),
        "expected L16 for std::str::format/3 on Lean, got {errs:?}"
    );
}

#[test]
fn l16_supported_std_math_min_is_silent() {
    let src = r#"
entity E {
    routes {
        constructor() => []
        go(a: u64, b: u64) => []
    }
    m_x: u64 { in constructor() => 0 in go(a, b) => std::math::min(a, b) }
}
"#;
    let errs = lean_compat_codes(src);
    assert!(
        !errs.contains(&"L16") && !errs.contains(&"L13"),
        "std::math::min/2 must be accepted on Lean, got {errs:?}"
    );
}

#[test]
fn l13_crypto_still_not_l16() {
    let src = r#"
entity E {
    routes {
        constructor() => []
        go(data: bytes) => []
    }
    m_x: u64 { in constructor() => 0 in go(data) => std::crypto::sha256(data) }
}
"#;
    let errs = lean_compat_codes(src);
    assert!(
        errs.contains(&"L13"),
        "expected L13 for std::crypto::sha256, got {errs:?}"
    );
    assert!(
        !errs.contains(&"L16"),
        "crypto must stay L13, not L16: {errs:?}"
    );
}
