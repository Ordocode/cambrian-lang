// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use cambrian_transpiler::ProgramParser;

fn parser() -> ProgramParser {
    ProgramParser::new()
}

fn must_fail(src: &str) {
    assert!(parser().parse(src).is_err(),
        "Expected parse error but succeeded for:\n{}", src);
}

fn must_succeed(src: &str) {
    assert!(parser().parse(src).is_ok(),
        "Expected parse success but failed for:\n{}", src);
}

// ===================================================================
// Negative tests: invalid syntax must produce errors, not panics
// ===================================================================

#[test]
fn neg_empty_input() {
    must_succeed("");
}

#[test]
fn neg_garbage_input() {
    must_fail("@#$%^&*()_+{}|:<>?~`");
}

#[test]
fn neg_entity_no_braces() {
    must_fail("entity Foo");
}

#[test]
fn neg_entity_unclosed() {
    must_fail("entity Foo {");
}

#[test]
fn neg_entity_double_close() {
    must_fail("entity Foo { } }");
}

#[test]
fn neg_pure_fn_missing_return_type() {
    must_fail("pure fn f() { 42 }");
}

#[test]
fn neg_pure_fn_missing_body() {
    must_fail("pure fn f() -> u64");
}

#[test]
fn neg_pure_fn_missing_parens() {
    must_fail("pure fn f -> u64 { 42 }");
}

#[test]
fn neg_route_outside_routes_block() {
    must_fail("entity Foo { increment() => [] }");
}

#[test]
fn neg_route_missing_arrow() {
    must_fail("entity Foo { routes { go() [] } }");
}

#[test]
fn neg_route_missing_brackets() {
    must_fail("entity Foo { routes { go() => } }");
}

#[test]
fn neg_member_missing_type() {
    must_fail("entity Foo { m_x { in go() => 0 } }");
}

#[test]
fn neg_member_missing_colon() {
    must_fail("entity Foo { m_x u64 { in go() => 0 } }");
}

#[test]
fn neg_const_missing_equals() {
    must_fail("entity Foo { const X: u8 42 }");
}

#[test]
fn neg_const_missing_type() {
    must_fail("entity Foo { const X = 42 }");
}

#[test]
fn neg_record_missing_field_type() {
    must_fail("record R { field }");
}

#[test]
fn neg_record_missing_comma() {
    must_fail("record R { a: u8 b: u16 }");
}

#[test]
fn neg_enum_empty() {
    must_fail("enum E {}");
}

#[test]
fn neg_type_alias_missing_equals() {
    must_fail("type X u64");
}

#[test]
fn neg_type_alias_missing_type() {
    must_fail("type X =");
}

#[test]
fn neg_where_missing_throw() {
    must_fail("entity E { routes { go() where x > 0 => [] } }");
}

#[test]
fn neg_where_missing_error_code() {
    must_fail("entity E { routes { go() where x > 0 : throw => [] } }");
}

#[test]
fn neg_send_missing_dest() {
    must_fail("entity E { routes { go() => [ send(1) ~> ] } }");
}

#[test]
fn neg_send_missing_arrow() {
    must_fail("entity E { routes { go() => [ send(1) target ] } }");
}

#[test]
fn neg_return_without_parens() {
    must_fail("entity E { routes { view go() -> u64 => [ return 42 ] } }");
}

#[test]
fn neg_from_without_entity() {
    must_fail("entity E { routes { go() from => [] } }");
}

#[test]
fn neg_let_without_value() {
    must_fail("pure fn f() -> u64 { let x; x }");
}

#[test]
fn neg_unclosed_string() {
    must_fail("pure fn f() -> String { \"hello }");
}

#[test]
fn neg_match_no_arms() {
    must_fail("pure fn f(x: u8) -> u8 { match x {} }");
}

#[test]
fn neg_keyword_as_param_name_view() {
    must_fail("entity E { routes { go(view: u8) => [] } }");
}

#[test]
fn neg_keyword_as_param_name_from() {
    must_fail("entity E { routes { go(from: u8) => [] } }");
}

#[test]
fn neg_keyword_as_param_name_pure() {
    must_fail("entity E { routes { go(pure: u8) => [] } }");
}

#[test]
fn neg_keyword_as_entity_name() {
    must_fail("entity routes {}");
}

#[test]
fn neg_double_view() {
    must_fail("entity E { routes { view view go() => [] } }");
}

#[test]
fn neg_view_and_pure() {
    must_fail("entity E { routes { view pure go() => [] } }");
}

#[test]
fn neg_macro_outside_entity() {
    must_fail("macro m() -> u8 = { 0 }");
}

#[test]
fn neg_binary_literal_invalid_digits() {
    must_fail("pure fn f() -> u8 { 0b123 }");
}

#[test]
fn neg_dangling_operator() {
    must_fail("pure fn f() -> u64 { 1 + }");
}

#[test]
fn neg_double_operator() {
    must_fail("pure fn f() -> u64 { 1 + + 2 }");
}

#[test]
fn neg_unmatched_parens() {
    must_fail("pure fn f() -> u64 { (1 + 2 }");
}

#[test]
fn neg_unmatched_bracket() {
    must_fail("entity E { routes { go() => [ send(1) ~> x } }");
}

// ===================================================================
// Ensure valid constructs still work (sanity checks for negatives)
// ===================================================================

#[test]
fn pos_empty_entity() {
    must_succeed("entity Foo {}");
}

#[test]
fn pos_entity_with_empty_routes() {
    must_succeed("entity Foo { routes {} }");
}

#[test]
fn pos_empty_member() {
    must_succeed("entity Foo { m_x: u8 {} }");
}

#[test]
fn pos_record_single_field() {
    must_succeed("record R { x: u8 }");
}

#[test]
fn pos_enum_single_variant() {
    must_succeed("enum E { A }");
}

#[test]
fn pos_view_route() {
    must_succeed("entity E { routes { view get() -> u8 => [ return(0) ] } }");
}

#[test]
fn pos_pure_route() {
    must_succeed("entity E { routes { pure add(a: u8, b: u8) -> u8 => [ return(a + b) ] } }");
}

#[test]
fn pos_from_clause() {
    must_succeed("entity E { routes { go() from Other(0) => [] } }");
}

#[test]
fn pos_from_or_clause() {
    must_succeed("entity E { routes { go() from A(0) | B(1) => [] } }");
}

// ===========================================================================
// Phase 6F: private routes + call action
// ===========================================================================

#[test]
fn pos_private_route() {
    must_succeed("use gosh\n\
        entity E { routes { private onUpgrade(x: u64) => [] } m_x: u64 { in onUpgrade(x) => x } }");
}

#[test]
fn pos_call_action() {
    must_succeed("use gosh\n\
        entity E { routes { go() => [call onUpgrade()] private onUpgrade() => [] } m_x: u64 { in onUpgrade() => 0 } }");
}

#[test]
fn pos_call_action_with_args() {
    must_succeed("use gosh\n\
        entity E { routes { go(x: u64) => [call onUpgrade(x, 42)] private onUpgrade(a: u64, b: u64) => [] } m_x: u64 { in onUpgrade(a, _) => a } }");
}

#[test]
fn neg_private_view_route() {
    must_fail("entity E { routes { private view get() -> u64 => [return(0)] } }");
}

#[test]
fn neg_private_pure_route() {
    must_fail("entity E { routes { private pure calc() -> u64 => [return(0)] } }");
}

#[test]
fn neg_call_missing_parens() {
    must_fail("entity E { routes { go() => [call foo] } }");
}

#[test]
fn neg_call_missing_name() {
    must_fail("entity E { routes { go() => [call ()] } }");
}

#[test]
fn neg_private_as_identifier() {
    must_fail("entity E { routes { go(private: u64) => [] } }");
}

#[test]
fn neg_call_as_identifier() {
    must_fail("entity E { routes { go(call: u64) => [] } }");
}

#[test]
fn pos_gosh_setcode_effect() {
    must_succeed("use gosh\nentity E { routes { go(c: CamData) => [gosh::setcode(c)] } m_x: u64 {} }");
}

#[test]
fn pos_gosh_setcurrentcode_effect() {
    must_succeed("use gosh\nentity E { routes { go(c: CamData) => [gosh::setCurrentCode(c)] } m_x: u64 {} }");
}

#[test]
fn pos_gosh_resetstorage_effect() {
    must_succeed("use gosh\nentity E { routes { go() => [gosh::resetStorage()] } m_x: u64 {} }");
}

#[test]
fn pos_gosh_setwasm_hash_effect() {
    must_succeed("use gosh\nentity E { routes { go(h: U256) => [gosh::setWasmHash(h)] } m_x: u64 {} }");
}
