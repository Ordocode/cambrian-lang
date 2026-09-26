// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

/// Tests for Phase 6F: Private Routes + Code Upgrade
///
/// This file covers TARGET-INDEPENDENT layers only:
///   - Parser: private keyword, call action, gosh:: effects
///   - Pretty-printer: round-trip, output formatting
///   - Validation: private route rules, CallRoute in pure routes
///   - Fixture: upgrade.cam parse + validate + roundtrip
///
/// AckiNacki-specific codegen (WASM, Solidity, WasmEffect encoding)
/// is tested in test_ackinacki_upgrade.rs and test_boc.rs.
use cambrian_transpiler::ProgramParser;
use cambrian_transpiler::ast::{RouteAction, RouteBody};
use cambrian_transpiler::pretty::pretty_print;
use cambrian_transpiler::validate::{validate, Severity};

fn parser() -> ProgramParser { ProgramParser::new() }

fn parse(src: &str) -> cambrian_transpiler::ast::Program {
    parser().parse(src).unwrap_or_else(|e| panic!("Parse error: {e}"))
}

fn has_error(src: &str, code: &str) -> bool {
    let prog = parse(src);
    validate(&prog).into_iter()
        .any(|d| d.severity == Severity::Error && d.code == code)
}

fn no_errors(src: &str) {
    let prog = parse(src);
    let errs: Vec<_> = validate(&prog).into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect();
    assert!(errs.is_empty(), "Expected no errors, got: {:?}", errs);
}

fn roundtrip(src: &str) {
    let prog1 = parse(src);
    let printed = pretty_print(&prog1);
    let prog2 = parser().parse(&printed)
        .unwrap_or_else(|e| panic!("Re-parse failed for pretty output:\n{}\nError: {}", printed, e));
    assert_eq!(prog1, prog2, "Round-trip mismatch.\nOriginal:\n{}\nPrinted:\n{}", src, printed);
}

// ===================================================================
// P1: Parser — private route
// ===================================================================

#[test]
fn parse_private_route() {
    let prog = parse("use gosh\n\
        entity E {\n\
            routes {\n\
                private onUpgrade(x: u64) => []\n\
            }\n\
            m_x: u64 { in onUpgrade(x) => x }\n\
        }");
    let entity = &prog.entities[0];
    let route = &entity.routes[0];
    assert_eq!(route.name, "onUpgrade");
    assert!(route.is_private, "route should be marked private");
    assert!(!route.is_view);
    assert!(!route.is_pure);
    assert!(!route.is_init);
    assert!(!route.is_accept);
}

#[test]
fn parse_private_route_with_body() {
    let prog = parse("use gosh\n\
        entity E {\n\
            routes {\n\
                private onUpgrade(x: u64) => [\n\
                    gosh::resetStorage()\n\
                ]\n\
            }\n\
            m_x: u64 { in onUpgrade(x) => x }\n\
        }");
    let entity = &prog.entities[0];
    let route = &entity.routes[0];
    assert!(route.is_private);
    match &route.body {
        RouteBody::Unphased(actions) => {
            assert_eq!(actions.len(), 1);
            assert!(matches!(&actions[0], RouteAction::Effect { namespace, name, .. }
                if namespace == "gosh" && name == "resetStorage"));
        }
        _ => panic!("expected unphased body"),
    }
}

#[test]
fn parse_regular_route_not_private() {
    let prog = parse("entity E {\n\
        routes { go(x: u64) => [] }\n\
        m_x: u64 { in go(x) => x }\n\
    }");
    assert!(!prog.entities[0].routes[0].is_private);
}

// ===================================================================
// P2: Parser — call action
// ===================================================================

#[test]
fn parse_call_action() {
    let prog = parse("use gosh\n\
        entity E {\n\
            routes {\n\
                go() => [\n\
                    call onUpgrade(42)\n\
                ]\n\
                private onUpgrade(x: u64) => []\n\
            }\n\
            m_x: u64 { in onUpgrade(x) => x }\n\
        }");
    let entity = &prog.entities[0];
    let go_route = &entity.routes[0];
    match &go_route.body {
        RouteBody::Unphased(actions) => {
            assert_eq!(actions.len(), 1);
            match &actions[0] {
                RouteAction::CallRoute { name, args } => {
                    assert_eq!(name, "onUpgrade");
                    assert_eq!(args.len(), 1);
                }
                other => panic!("expected CallRoute, got {:?}", other),
            }
        }
        _ => panic!("expected unphased body"),
    }
}

#[test]
fn parse_call_action_multiple_args() {
    let prog = parse("use gosh\n\
        entity E {\n\
            routes {\n\
                go(a: u64, b: U256) => [\n\
                    call onUpgrade(a, b)\n\
                ]\n\
                private onUpgrade(x: u64, y: U256) => []\n\
            }\n\
            m_x: u64 { in onUpgrade(x, _) => x }\n\
        }");
    let entity = &prog.entities[0];
    let go_route = &entity.routes[0];
    match &go_route.body {
        RouteBody::Unphased(actions) => {
            match &actions[0] {
                RouteAction::CallRoute { name, args } => {
                    assert_eq!(name, "onUpgrade");
                    assert_eq!(args.len(), 2);
                }
                other => panic!("expected CallRoute, got {:?}", other),
            }
        }
        _ => panic!("expected unphased body"),
    }
}

#[test]
fn parse_call_action_no_args() {
    let prog = parse("use gosh\n\
        entity E {\n\
            routes {\n\
                go() => [\n\
                    call onUpgrade()\n\
                ]\n\
                private onUpgrade() => []\n\
            }\n\
            m_x: u64 { in onUpgrade() => 0 }\n\
        }");
    let entity = &prog.entities[0];
    let go_route = &entity.routes[0];
    match &go_route.body {
        RouteBody::Unphased(actions) => {
            match &actions[0] {
                RouteAction::CallRoute { name, args } => {
                    assert_eq!(name, "onUpgrade");
                    assert!(args.is_empty());
                }
                other => panic!("expected CallRoute, got {:?}", other),
            }
        }
        _ => panic!("expected unphased body"),
    }
}

// ===================================================================
// P3: Parser — code upgrade effects (AST level only)
// ===================================================================

#[test]
fn parse_setcode_effect() {
    let prog = parse("use gosh\n\
        entity E {\n\
            routes { go(c: CamData) => [gosh::setcode(c)] }\n\
            m_x: u64 {}\n\
        }");
    let actions = prog.entities[0].routes[0].body.all_actions();
    assert!(matches!(&actions[0], RouteAction::Effect { namespace, name, .. }
        if namespace == "gosh" && name == "setcode"));
}

#[test]
fn parse_setcurrentcode_effect() {
    let prog = parse("use gosh\n\
        entity E {\n\
            routes { go(c: CamData) => [gosh::setCurrentCode(c)] }\n\
            m_x: u64 {}\n\
        }");
    let actions = prog.entities[0].routes[0].body.all_actions();
    assert!(matches!(&actions[0], RouteAction::Effect { namespace, name, .. }
        if namespace == "gosh" && name == "setCurrentCode"));
}

#[test]
fn parse_resetstorage_effect() {
    let prog = parse("use gosh\n\
        entity E {\n\
            routes { go() => [gosh::resetStorage()] }\n\
            m_x: u64 {}\n\
        }");
    let actions = prog.entities[0].routes[0].body.all_actions();
    assert!(matches!(&actions[0], RouteAction::Effect { namespace, name, .. }
        if namespace == "gosh" && name == "resetStorage"));
}

#[test]
fn parse_setwasm_hash_effect() {
    let prog = parse("use gosh\n\
        entity E {\n\
            routes { go(h: U256) => [gosh::setWasmHash(h)] }\n\
            m_x: u64 {}\n\
        }");
    let actions = prog.entities[0].routes[0].body.all_actions();
    assert!(matches!(&actions[0], RouteAction::Effect { namespace, name, .. }
        if namespace == "gosh" && name == "setWasmHash"));
}

// ===================================================================
// PP1: Pretty-printer — round-trip
// ===================================================================

#[test]
fn roundtrip_private_route() {
    roundtrip("use gosh\n\
        entity E {\n\
            routes {\n\
                private onUpgrade(x: u64) => []\n\
            }\n\
            m_x: u64 {\n\
                in onUpgrade(x) => x\n\
            }\n\
        }\n");
}

#[test]
fn roundtrip_call_action() {
    roundtrip("use gosh\n\
        entity E {\n\
            routes {\n\
                go(x: u64) => [\n\
                    call onUpgrade(x)\n\
                ]\n\
                private onUpgrade(x: u64) => []\n\
            }\n\
            m_x: u64 {\n\
                in onUpgrade(x) => x\n\
            }\n\
        }\n");
}

#[test]
fn roundtrip_effects_setcode() {
    roundtrip("use gosh\n\
        entity E {\n\
            routes {\n\
                go(c: CamData) => [\n\
                    gosh::setcode(c)\n\
                    gosh::setCurrentCode(c)\n\
                ]\n\
            }\n\
            m_x: u64 {}\n\
        }\n");
}

#[test]
fn roundtrip_full_upgrade_pattern() {
    roundtrip("use gosh\n\
        entity E {\n\
            routes {\n\
                go(c: CamData, h: U256) => [\n\
                    gosh::commit()\n\
                    gosh::setcode(c)\n\
                    gosh::setCurrentCode(c)\n\
                    gosh::setWasmHash(h)\n\
                    call onUpgrade()\n\
                ]\n\
                private onUpgrade() => [\n\
                    gosh::resetStorage()\n\
                ]\n\
            }\n\
            m_x: u64 {\n\
                in onUpgrade() => 0\n\
            }\n\
        }\n");
}

// ===================================================================
// PP2: Pretty-printer — output formatting
// ===================================================================

#[test]
fn pretty_private_route_prefix() {
    let prog = parse("use gosh\n\
        entity E {\n\
            routes { private onUpgrade(x: u64) => [] }\n\
            m_x: u64 { in onUpgrade(x) => x }\n\
        }");
    let out = pretty_print(&prog);
    assert!(out.contains("private onUpgrade"),
        "Expected 'private' prefix in:\n{}", out);
}

#[test]
fn pretty_call_action_output() {
    let prog = parse("use gosh\n\
        entity E {\n\
            routes {\n\
                go(x: u64) => [call onUpgrade(x)]\n\
                private onUpgrade(x: u64) => []\n\
            }\n\
            m_x: u64 { in onUpgrade(x) => x }\n\
        }");
    let out = pretty_print(&prog);
    assert!(out.contains("call onUpgrade(x)"),
        "Expected 'call onUpgrade(x)' in:\n{}", out);
}

#[test]
fn pretty_use_import_preserved() {
    let prog = parse("use gosh\nentity E { m_x: u64 {} }");
    let out = pretty_print(&prog);
    assert!(out.contains("use gosh"),
        "Expected 'use gosh' in:\n{}", out);
}

// ===================================================================
// V1: Validation — private routes
// ===================================================================

#[test]
fn valid_private_route_with_transform() {
    no_errors("use gosh\n\
        entity E {\n\
            routes {\n\
                go() => [call onUpgrade()]\n\
                private onUpgrade() => [gosh::resetStorage()]\n\
            }\n\
            m_x: u64 { in onUpgrade() => 0 }\n\
        }");
}

#[test]
fn valid_private_route_full_upgrade() {
    no_errors("use gosh\n\
        entity E {\n\
            routes {\n\
                init constructor(owner: U256) => []\n\
                updateCode(c: CamData, h: U256, x: u64) => [\n\
                    gosh::commit()\n\
                    gosh::setcode(c)\n\
                    gosh::setCurrentCode(c)\n\
                    gosh::setWasmHash(h)\n\
                    call onUpgrade(x)\n\
                ]\n\
                private onUpgrade(x: u64) => [\n\
                    gosh::resetStorage()\n\
                ]\n\
            }\n\
            m_owner: U256 { in constructor(owner) => owner in onUpgrade(_) => m_owner }\n\
            m_count: u64 { in constructor(_) => 0 in onUpgrade(x) => x }\n\
        }");
}

#[test]
fn pure_route_cannot_call_private() {
    assert!(has_error(
        "use gosh\n\
        entity E {\n\
            routes {\n\
                pure calc() -> u64 => [call onUpgrade()]\n\
                private onUpgrade() => []\n\
            }\n\
            m_x: u64 { in onUpgrade() => 0 }\n\
        }",
        "V11"
    ));
}

// ===================================================================
// N1: Negative parser tests — invalid syntax
// ===================================================================

#[test]
fn parse_call_without_parens_fails() {
    assert!(parser().parse("entity E { routes { go() => [call foo] } m_x: u64 {} }").is_err());
}

#[test]
fn parse_private_without_route_name_fails() {
    assert!(parser().parse("entity E { routes { private => [] } m_x: u64 {} }").is_err());
}

#[test]
fn parse_call_as_top_level_keyword_fails() {
    assert!(parser().parse("call foo()").is_err());
}

// ===================================================================
// Upgrade fixture — parse, validate, roundtrip (target-independent)
// ===================================================================

#[test]
fn upgrade_fixture_parses() {
    let src = include_str!("../../contracts/upgrade.cam");
    let prog = parse(src);
    assert_eq!(prog.entities.len(), 1);
    assert_eq!(prog.entities[0].name, "Upgradeable");

    let routes = &prog.entities[0].routes;
    assert_eq!(routes.len(), 6);

    let update_code = routes.iter().find(|r| r.name == "updateCode").unwrap();
    assert!(!update_code.is_private);
    assert_eq!(update_code.params.len(), 3);

    let on_upgrade = routes.iter().find(|r| r.name == "onCodeUpgrade").unwrap();
    assert!(on_upgrade.is_private);
    assert_eq!(on_upgrade.params.len(), 1);
}

#[test]
fn upgrade_fixture_validates() {
    let src = include_str!("../../contracts/upgrade.cam");
    let prog = parse(src);
    let diags = validate(&prog);
    let errs: Vec<_> = diags.iter().filter(|d| d.severity == Severity::Error).collect();
    assert!(errs.is_empty(), "Unexpected errors: {:?}", errs);
}

#[test]
fn upgrade_fixture_roundtrip() {
    let src = include_str!("../../contracts/upgrade.cam");
    roundtrip(src);
}

// ===========================================================================
// Phase 8: gosh::updateCode(...) with callback(...) — parsing
// ===========================================================================

#[test]
fn update_code_parses_to_update_code_variant() {
    let src = "use gosh\n\
        entity E {\n\
            routes {\n\
                doUpgrade(code: CamData, hash: U256) => [\n\
                    gosh::updateCode(code, hash) with onUpgrade()\n\
                ]\n\
                private onUpgrade() => []\n\
            }\n\
            m_x: u64 { in onUpgrade() => 0 }\n\
        }";
    let prog = parse(src);
    let route = prog.entities[0].routes.iter().find(|r| r.name == "doUpgrade").unwrap();
    let actions = route.body.actions();
    assert!(actions.iter().any(|a| matches!(a, RouteAction::UpdateCode { .. })),
        "doUpgrade route must contain UpdateCode action");
}

#[test]
fn update_code_with_args_parses() {
    let src = "use gosh\n\
        entity E {\n\
            routes {\n\
                doUpgrade(code: CamData, hash: U256, newVal: u64) => [\n\
                    gosh::updateCode(code, hash) with onUpgrade(newVal)\n\
                ]\n\
                private onUpgrade(val: u64) => []\n\
            }\n\
            m_x: u64 { in onUpgrade(val) => val }\n\
        }";
    let prog = parse(src);
    let route = prog.entities[0].routes.iter().find(|r| r.name == "doUpgrade").unwrap();
    let actions = route.body.actions();
    let uc = actions.iter().find_map(|a| match a {
        RouteAction::UpdateCode { callback_route, callback_args, update_args } =>
            Some((update_args, callback_route, callback_args)),
        _ => None,
    }).expect("must find UpdateCode action");
    assert_eq!(uc.0.len(), 2, "update_args must have 2 elements");
    assert_eq!(uc.1, "onUpgrade");
    assert_eq!(uc.2.len(), 1, "callback must have 1 arg");
}

#[test]
fn update_code_pretty_prints() {
    let src = "use gosh\n\
        entity E {\n\
            routes {\n\
                doUpgrade(code: CamData, hash: U256) => [\n\
                    gosh::updateCode(code, hash) with onUpgrade()\n\
                ]\n\
                private onUpgrade() => []\n\
            }\n\
            m_x: u64 { in onUpgrade() => 0 }\n\
        }";
    let prog = parse(src);
    let pp = pretty_print(&prog);
    assert!(pp.contains("gosh::updateCode(code, hash) with onUpgrade()"),
        "pretty print must reproduce updateCode syntax:\n{}", pp);
}

#[test]
fn update_code_validates_callback_route_exists() {
    let src = "use gosh\n\
        entity E {\n\
            routes {\n\
                doUpgrade(code: CamData, hash: U256) => [\n\
                    gosh::updateCode(code, hash) with nonExistent()\n\
                ]\n\
            }\n\
            m_x: u64 { in doUpgrade() => 0 }\n\
        }";
    let prog = parse(src);
    let diags = validate(&prog);
    let errs: Vec<_> = diags.iter().filter(|d| d.severity == Severity::Error).collect();
    assert!(errs.iter().any(|d| d.message.contains("nonExistent")),
        "must report error for undefined callback route: {:?}", errs);
}

#[test]
fn update_code_must_be_last_action() {
    let src = "use gosh\n\
        entity E {\n\
            routes {\n\
                doUpgrade(code: CamData, hash: U256) => [\n\
                    gosh::updateCode(code, hash) with onUpgrade()\n\
                    gosh::commit()\n\
                ]\n\
                private onUpgrade() => []\n\
            }\n\
            m_x: u64 { in onUpgrade() => 0 }\n\
        }";
    let prog = parse(src);
    let diags = validate(&prog);
    let errs: Vec<_> = diags.iter().filter(|d| d.severity == Severity::Error).collect();
    assert!(errs.iter().any(|d| d.code == "V26"),
        "must report V26 error for actions after updateCode: {:?}", errs);
}

#[test]
#[should_panic(expected = "only allowed for")]
fn with_syntax_rejected_for_non_update_code() {
    let src = "use gosh\n\
        entity E {\n\
            routes {\n\
                go() => [\n\
                    gosh::commit() with foo()\n\
                ]\n\
            }\n\
            m_x: u64 { in go() => 0 }\n\
        }";
    parse(src);
}

#[test]
fn regular_effect_still_works_without_with() {
    let src = "use gosh\n\
        entity E {\n\
            routes {\n\
                go() => [gosh::commit()]\n\
            }\n\
            m_x: u64 { in go() => 0 }\n\
        }";
    let prog = parse(src);
    let route = prog.entities[0].routes.iter().find(|r| r.name == "go").unwrap();
    let actions = route.body.actions();
    assert!(actions.iter().any(|a| matches!(a, RouteAction::Effect { .. })),
        "regular effect without 'with' must parse as Effect");
}
