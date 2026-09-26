// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase N4 — targeted coverage slices under `src/analysis/` (execution tests).

use cambrian_transpiler::analysis::graphs::ProgramGraphs;
use cambrian_transpiler::analysis::{
    compute_route_facts, entity_evm_lean_can_fail, evm_lean_can_fail, fail_closure,
    route_can_fail_evm_lean, route_has_unphased_sends, route_is_view,
    route_phased_needs_world_thread, route_uses_msg_value, FailPropagation, RouteFacts,
};
use cambrian_transpiler::ast::normalize_program_types;
use std::collections::HashMap;

fn parse(src: &str) -> cambrian_transpiler::ast::Program {
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    normalize_program_types(&mut program);
    program
}

fn fact<'a>(
    facts: &'a HashMap<(String, String), RouteFacts>,
    entity: &str,
    route: &str,
) -> &'a RouteFacts {
    facts
        .get(&(entity.to_string(), route.to_string()))
        .unwrap_or_else(|| panic!("missing RouteFacts for {entity}.{route}"))
}

#[test]
fn n4_route_facts_fail_closure_skips_when_same_entity_calls_disabled() {
    let program = parse(
        r#"
        entity E {
            routes {
                helper() where (false) : throw 1 => []
                wrapper() => [ call helper() ]
            }
            m_x: u64 {}
        }
    "#,
    );
    let facts = compute_route_facts(&program);
    let policy = FailPropagation {
        same_entity_calls: false,
        cross_entity_sends: false,
        rescue_cuts: true,
    };
    let can = fail_closure(&facts, policy);
    assert!(can[&(String::from("E"), String::from("helper"))]);
    assert!(!can[&(String::from("E"), String::from("wrapper"))]);
}

#[test]
fn n4_route_facts_local_fail_surface_from_where_and_phased_where() {
    let program = parse(
        r#"
        entity E {
            routes {
                fromGuard() from Self(0x1) => []
                whereGuard() where (m_x > 0) : throw 1 => []
                phasedGuard() => [
                    step where (m_x > 0) : throw 2: []
                ]
            }
            m_x: u64 { in fromGuard() => 0 }
        }
    "#,
    );
    let facts = compute_route_facts(&program);
    assert!(fact(&facts, "E", "fromGuard").local_fail_surface);
    assert!(fact(&facts, "E", "whereGuard").local_fail_surface);
    assert!(fact(&facts, "E", "phasedGuard").local_fail_surface);
}

#[test]
fn n4_route_facts_local_fail_surface_deploy_transfer_and_nested_throw() {
    let program = parse(
        r#"
        entity Child {
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 0 }
        }
        entity E {
            routes {
                pay(dest: address) => [
                    if dest != 0x0 => [
                        throw 1
                    ] else [
                        ~> dest with { value: 1 }
                    ]
                ]
                spawn() => [ deploy Child() ]
            }
            m_y: u64 {}
        }
    "#,
    );
    let facts = compute_route_facts(&program);
    assert!(fact(&facts, "E", "pay").local_fail_surface);
    assert!(fact(&facts, "E", "spawn").local_fail_surface);
}

#[test]
fn n4_route_facts_uses_msg_value_in_where_and_member_transform() {
    let program = parse(
        r#"
        entity E {
            routes {
                deposit() where (msg::value > 0) : throw 1 => []
            }
            m_bal: u64 { in deposit() => m_bal + msg::value }
        }
    "#,
    );
    let entity = &program.entities[0];
    let deposit = entity.routes.iter().find(|r| r.name == "deposit").unwrap();
    assert!(route_uses_msg_value(deposit, entity));
    assert!(fact(&compute_route_facts(&program), "E", "deposit").uses_msg_value);
}

#[test]
fn n4_route_facts_world_thread_flags_and_view_shape() {
    let program = parse(
        r#"
        entity E {
            routes {
                unphased(dest: address) => [
                    ~> dest with { value: 1 }
                ]
                phased(dest: address) => [
                    step: [
                        ~> dest with { value: 1 }
                    ]
                ]
                balanceView() -> u64 => [ return(m_x) ]
                helper() => []
                caller() => [ call helper() ]
            }
            m_x: u64 { in balanceView() => 0 }
        }
    "#,
    );
    let entity = &program.entities[0];
    let unphased = entity.routes.iter().find(|r| r.name == "unphased").unwrap();
    let phased = entity.routes.iter().find(|r| r.name == "phased").unwrap();
    let balance_view = entity.routes.iter().find(|r| r.name == "balanceView").unwrap();
    let caller_route = entity.routes.iter().find(|r| r.name == "caller").unwrap();

    assert!(route_has_unphased_sends(unphased));
    assert!(route_phased_needs_world_thread(phased));
    assert!(route_is_view(balance_view));
    assert!(!route_is_view(unphased));

    let facts = compute_route_facts(&program);
    assert!(fact(&facts, "E", "unphased").has_unphased_sends);
    assert!(fact(&facts, "E", "phased").phased_needs_world_thread);
    assert!(fact(&facts, "E", "balanceView").is_view);
    assert_eq!(
        fact(&facts, "E", "caller").call_callees,
        vec!["helper".to_string()]
    );
    assert!(!route_can_fail_evm_lean(entity, caller_route));
}

#[test]
fn n4_route_facts_sys_balance_in_return_needs_world_thread() {
    let program = parse(
        r#"
        entity E {
            routes {
                bal() -> u128 => [ return(sys::balance) ]
            }
            m_x: u128 {}
        }
    "#,
    );
    let entity = &program.entities[0];
    let bal = entity.routes.iter().find(|r| r.name == "bal").unwrap();
    assert!(route_has_unphased_sends(bal));
}

#[test]
fn n4_route_facts_mixed_body_fail_surface_and_msg_value_in_var_call() {
    let program = parse(
        r#"
        entity Token {
            routes {
                constructor() => []
                transfer(to: address, amount: u64) => []
            }
            m_supply: u64 { in constructor() => 0 }
        }
        entity E {
            routes {
                constructor(t: Address<Token>) => []
                mixed(dest: address) => [
                    prep: []
                    ~> m_token with { value: 1 }
                ]
                capture(dest: address, amount: u64) => [
                    var ok = transfer(dest, amount) ~> m_token with { value: msg::value };
                ]
            }
            m_token: Address<Token> {
                in constructor(t) => t
            }
        }
    "#,
    );
    let facts = compute_route_facts(&program);
    assert!(fact(&facts, "E", "mixed").local_fail_surface);
    assert!(fact(&facts, "E", "capture").uses_msg_value);
}

#[test]
fn n4_route_facts_entity_and_single_route_can_fail_evm_lean() {
    let program = parse(
        r#"
        entity E {
            routes {
                risky() where (false) : throw 1 => []
                safe() => []
            }
            m_x: u64 {}
        }
    "#,
    );
    let entity = &program.entities[0];
    let risky = entity.routes.iter().find(|r| r.name == "risky").unwrap();
    let safe = entity.routes.iter().find(|r| r.name == "safe").unwrap();

    let per_entity = entity_evm_lean_can_fail(entity);
    assert_eq!(per_entity.get("risky"), Some(&true));
    assert_eq!(per_entity.get("safe"), Some(&false));

    assert!(route_can_fail_evm_lean(entity, risky));
    assert!(!route_can_fail_evm_lean(entity, safe));

    let program_wide = evm_lean_can_fail(&program);
    assert_eq!(program_wide.get(&("E".into(), "risky".into())), Some(&true));
}

#[test]
fn n4_route_facts_program_graphs_attaches_same_facts() {
    let program = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
    "#,
    );
    let direct = compute_route_facts(&program);
    let graphs = ProgramGraphs::build(&program);
    assert_eq!(graphs.route_facts, direct);
}
