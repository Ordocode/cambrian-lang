// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase N4 — targeted coverage slices under `src/ir/` (execution tests).

use cambrian_transpiler::analysis::ProgramGraphs;
use cambrian_transpiler::ast::{normalize_program_types, Expr, Type};
use cambrian_core::U256;
use cambrian_transpiler::ir::{
    lower_expr, lower_program, lower_route, IrStmt, LowerCtx, ResolvedType, TypedExprKind,
    resolve_type,
};
use std::collections::HashMap;

fn parse(src: &str) -> cambrian_transpiler::ast::Program {
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    normalize_program_types(&mut program);
    program
}

fn entity_route<'a>(
    program: &'a cambrian_transpiler::ast::Program,
    entity: &str,
    route: &str,
) -> (&'a cambrian_transpiler::ast::Entity, &'a cambrian_transpiler::ast::Route) {
    let ent = program
        .entities
        .iter()
        .find(|e| e.name == entity)
        .unwrap_or_else(|| panic!("entity {entity}"));
    let rt = ent
        .routes
        .iter()
        .find(|r| r.name == route)
        .unwrap_or_else(|| panic!("route {entity}.{route}"));
    (ent, rt)
}

fn alias_map(pairs: &[(&str, Type)]) -> HashMap<String, Type> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

#[test]
fn n4_ir_ty_simple_constructor_and_unaliased_resolve() {
    let empty = HashMap::new();
    assert_eq!(
        ResolvedType::simple("u64"),
        ResolvedType::Simple("u64".to_string())
    );
    assert_eq!(
        resolve_type(&Type::Simple("address".to_string()), &empty),
        ResolvedType::Simple("address".to_string())
    );
}

#[test]
fn n4_ir_ty_resolve_aliases_matches_from_ast() {
    let map = alias_map(&[("Amt", Type::Simple("u64".to_string()))]);
    let ast = Type::Simple("Amt".to_string());
    assert_eq!(
        ResolvedType::resolve_aliases(&ast, &map),
        ResolvedType::from_ast(&ast, &map)
    );
}

#[test]
fn n4_ir_ty_generic_resolves_param_aliases() {
    let map = alias_map(&[
        ("Key", Type::Simple("u64".to_string())),
        ("Val", Type::Simple("bool".to_string())),
    ]);
    let ast = Type::Generic(
        "HashMap".to_string(),
        vec![
            Type::Simple("Key".to_string()),
            Type::Simple("Val".to_string()),
        ],
    );
    assert_eq!(
        resolve_type(&ast, &map),
        ResolvedType::Generic {
            name: "HashMap".to_string(),
            params: vec![
                ResolvedType::Simple("u64".to_string()),
                ResolvedType::Simple("bool".to_string()),
            ],
        }
    );
}

#[test]
fn n4_ir_ty_tuple_resolves_element_aliases() {
    let map = alias_map(&[("Slot", Type::Simple("u128".to_string()))]);
    let ast = Type::Tuple(vec![
        Type::Simple("Slot".to_string()),
        Type::Simple("u8".to_string()),
    ]);
    assert_eq!(
        resolve_type(&ast, &map),
        ResolvedType::Tuple(vec![
            ResolvedType::Simple("u128".to_string()),
            ResolvedType::Simple("u8".to_string()),
        ])
    );
}

#[test]
fn n4_ir_ty_typed_address_passthrough() {
    let map = HashMap::new();
    let ast = Type::TypedAddress("Counter".to_string());
    assert_eq!(
        resolve_type(&ast, &map),
        ResolvedType::TypedAddress("Counter".to_string())
    );
}

#[test]
fn n4_ir_ty_to_ast_roundtrip_preserves_shape() {
    let resolved = ResolvedType::Generic {
        name: "Vec".to_string(),
        params: vec![ResolvedType::Tuple(vec![
            ResolvedType::Simple("u64".to_string()),
            ResolvedType::TypedAddress("Vault".to_string()),
        ])],
    };
    let ast = resolved.to_ast();
    assert_eq!(
        ast,
        Type::Generic(
            "Vec".to_string(),
            vec![Type::Tuple(vec![
                Type::Simple("u64".to_string()),
                Type::TypedAddress("Vault".to_string()),
            ])],
        )
    );
    assert_eq!(resolve_type(&ast, &HashMap::new()), resolved);
}

#[test]
fn n4_ir_ty_nested_alias_through_generic_param() {
    let map = alias_map(&[
        ("Outer", Type::Generic(
            "Option".to_string(),
            vec![Type::Simple("Inner".to_string())],
        )),
        ("Inner", Type::Simple("u32".to_string())),
    ]);
    assert_eq!(
        resolve_type(&Type::Simple("Outer".to_string()), &map),
        ResolvedType::Generic {
            name: "Option".to_string(),
            params: vec![ResolvedType::Simple("u32".to_string())],
        }
    );
}

#[test]
fn n4_ir_lower_mixed_route_trailing_phase_carries_route_guards() {
    let program = parse(
        r#"
        entity E {
            routes {
                mixed() from Self(0x1) where (m_x > 0) : throw 1 => [
                    prep: [ call helper() ]
                    call helper()
                ]
                helper() => []
            }
            m_x: u64 { in mixed() => 0 }
        }
    "#,
    );
    let graphs = ProgramGraphs::build(&program);
    let (entity, route) = entity_route(&program, "E", "mixed");
    let ir = lower_route(&program, entity, route, &graphs);
    assert_eq!(ir.phases.len(), 2);
    assert_eq!(ir.phases[0].name.as_deref(), Some("prep"));
    assert_eq!(ir.phases[1].name, None);
    assert!(!ir.phases[1].from_guards.is_empty());
    assert!(!ir.phases[1].where_guards.is_empty());
    assert!(ir.phases[1]
        .stmts
        .iter()
        .any(|s| matches!(s, IrStmt::CallRoute { name, .. } if name == "helper")));
}

#[test]
fn n4_ir_lower_fail_mode_and_needs_world_from_route_facts() {
    let program = parse(
        r#"
        entity E {
            routes {
                helper() where (false) : throw 1 => []
                wrapper() => [ call helper() ]
                pay(dest: address) => [ ~> dest with { value: 1 } ]
            }
            m_x: u64 {}
        }
    "#,
    );
    let graphs = ProgramGraphs::build(&program);
    let ir = lower_program(&program, &graphs);
    let wrapper = ir
        .routes
        .get(&("E".to_string(), "wrapper".to_string()))
        .expect("wrapper");
    let helper = ir
        .routes
        .get(&("E".to_string(), "helper".to_string()))
        .expect("helper");
    let pay = ir
        .routes
        .get(&("E".to_string(), "pay".to_string()))
        .expect("pay");
    assert!(helper.fail_mode);
    assert!(wrapper.fail_mode);
    assert!(pay.needs_world);
}

#[test]
fn n4_ir_lower_phased_and_unphased_member_transforms() {
    let program = parse(
        r#"
        entity E {
            routes {
                go() => [
                    setup: []
                    run: []
                ]
            }
            m_a: u64 { in go() => setup: 0 run: m_a + 1 }
            m_plain: u64 { in go() => 99 }
        }
    "#,
    );
    let graphs = ProgramGraphs::build(&program);
    let (entity, route) = entity_route(&program, "E", "go");
    let ir = lower_route(&program, entity, route, &graphs);
    assert_eq!(ir.phases.len(), 2);
    let setup = &ir.phases[0];
    let run = &ir.phases[1];
    assert_eq!(setup.transforms.len(), 1);
    assert_eq!(setup.transforms[0].member, "m_a");
    assert!(matches!(
        setup.transforms[0].body.kind,
        TypedExprKind::IntLiteral(_)
    ));
    assert_eq!(run.transforms.len(), 1);
    assert!(matches!(
        run.transforms[0].body.kind,
        TypedExprKind::BinOp { .. }
    ));
    assert!(setup.transforms.iter().all(|t| t.member != "m_plain"));
    assert!(run.transforms.iter().all(|t| t.member != "m_plain"));
}

#[test]
fn n4_ir_lower_mixed_trailing_phase_collects_unphased_transforms() {
    let program = parse(
        r#"
        entity E {
            routes {
                go() => [
                    p1: [ call noop() ]
                    call noop()
                ]
                noop() => []
            }
            m_phased: u64 { in go() => p1: 1 }
            m_trailing: u64 { in go() => 2 }
        }
    "#,
    );
    let graphs = ProgramGraphs::build(&program);
    let (entity, route) = entity_route(&program, "E", "go");
    let ir = lower_route(&program, entity, route, &graphs);
    assert_eq!(ir.phases.len(), 2);
    assert_eq!(ir.phases[0].transforms.len(), 1);
    assert_eq!(ir.phases[0].transforms[0].member, "m_phased");
    assert_eq!(ir.phases[1].transforms.len(), 1);
    assert_eq!(ir.phases[1].transforms[0].member, "m_trailing");
}

#[test]
fn n4_ir_lower_temporal_transform_order_follows_member_dag() {
    let program = parse(
        r#"
        entity E {
            routes { go() => [] }
            m_a: u64 { in go() => 0 }
            m_b: u64 { in go() => ^m_a + 1 }
            m_c: u64 { in go() => ^m_b + 1 }
        }
    "#,
    );
    let graphs = ProgramGraphs::build(&program);
    let (entity, route) = entity_route(&program, "E", "go");
    let ir = lower_route(&program, entity, route, &graphs);
    let names: Vec<_> = ir.phases[0]
        .transforms
        .iter()
        .map(|t| t.member.as_str())
        .collect();
    assert_eq!(names, vec!["m_a", "m_b", "m_c"]);
}

#[test]
fn n4_ir_lower_action_emit_throw_custom_deploy_rescue_for() {
    let program = parse(
        r#"
        entity Child {
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 0 }
        }
        entity E {
            event Ev(x: u64);
            error Bad(code: u64);
            routes {
                kitchen(dest: address) -> u64 => [
                    let n = 1;
                    if false => [ throw 2 ] else [ let z = 1; ]
                    for i in 0..1 => [ call noop() ]
                    gosh::commit()
                    emit Ev(n);
                    throw Bad(1)
                    deploy Child()
                    rescue bounce: ~> dest
                    call noop()
                ]
                noop() => []
            }
            m_dummy: u64 {}
        }
    "#,
    );
    let graphs = ProgramGraphs::build(&program);
    let (entity, route) = entity_route(&program, "E", "kitchen");
    let ir = lower_route(&program, entity, route, &graphs);
    let stmts = &ir.phases[0].stmts;
    assert!(stmts.iter().any(|s| matches!(s, IrStmt::Let { .. })));
    assert!(stmts.iter().any(|s| matches!(s, IrStmt::Conditional { .. })));
    assert!(stmts.iter().any(|s| matches!(s, IrStmt::For { .. })));
    assert!(stmts.iter().any(|s| matches!(
        s,
        IrStmt::Effect {
            namespace,
            name,
            ..
        } if namespace == "gosh" && name == "commit"
    )));
    assert!(stmts.iter().any(|s| matches!(
        s,
        IrStmt::Emit { event, .. } if event == "Ev"
    )));
    assert!(stmts.iter().any(|s| matches!(
        s,
        IrStmt::ThrowCustom { name, .. } if name == "Bad"
    )));
    assert!(stmts.iter().any(|s| matches!(
        s,
        IrStmt::Deploy { entity, .. } if entity == "Child"
    )));
    assert!(stmts.iter().any(|s| matches!(
        s,
        IrStmt::Rescue { tag, .. } if tag == "bounce"
    )));
}

#[test]
fn n4_ir_lower_var_call_send_and_update_code() {
    let program = parse(
        r#"
        use gosh
        entity E {
            routes {
                caller(dest: address) => [
                    var ok = ping() ~> dest with { value: 1 };
                    gosh::updateCode(0x01, 0x02) with onUpgrade()
                ]
                ping() => []
                onUpgrade() => []
            }
            m_x: u64 {}
        }
    "#,
    );
    let graphs = ProgramGraphs::build(&program);
    let (entity, route) = entity_route(&program, "E", "caller");
    let ir = lower_route(&program, entity, route, &graphs);
    let stmts = &ir.phases[0].stmts;
    assert!(stmts.iter().any(|s| matches!(
        s,
        IrStmt::VarCall { message, .. } if message == "ping"
    )));
    assert!(stmts.iter().any(|s| matches!(
        s,
        IrStmt::UpdateCode {
            callback_route,
            ..
        } if callback_route == "onUpgrade"
    )));
}

#[test]
fn n4_ir_lower_expr_literals_cast_field_temporal_and_passthrough() {
    let program = parse(
        r#"
        type Amt = u64
        entity E {
            routes { go(x: u64) => [] }
            m_amt: Amt { in go(x) => x as Amt }
            m_next: u64 { in go(x) => ^m_amt + x }
        }
    "#,
    );
    let (entity, route) = entity_route(&program, "E", "go");
    let ctx = LowerCtx::new(&program, entity, route);

    let hex = lower_expr(&Expr::IntLiteral(U256::from_hex_digits("10").unwrap()), &ctx);
    assert!(matches!(hex.kind, TypedExprKind::IntLiteral(_)));

    let bin = lower_expr(&Expr::IntLiteral(U256::from_u128(5)), &ctx);
    assert!(matches!(bin.kind, TypedExprKind::IntLiteral(_)));

    let bl = lower_expr(&Expr::BoolLiteral(true), &ctx);
    assert!(matches!(bl.kind, TypedExprKind::BoolLiteral(true)));

    let sl = lower_expr(&Expr::StringLiteral("hi".to_string()), &ctx);
    assert!(matches!(sl.kind, TypedExprKind::StringLiteral(_)));

    let cast = lower_expr(
        &Expr::Cast(
            Box::new(Expr::Ident("x".to_string())),
            Type::Simple("Amt".to_string()),
        ),
        &ctx,
    );
    assert!(matches!(cast.kind, TypedExprKind::Cast { .. }));
    assert_eq!(cast.ty, ResolvedType::Simple("u64".to_string()));

    let msg = lower_expr(&Expr::MsgField("sender".to_string()), &ctx);
    assert!(matches!(msg.kind, TypedExprKind::MsgField(_)));

    let sys = lower_expr(&Expr::SysField("now".to_string()), &ctx);
    assert!(matches!(sys.kind, TypedExprKind::SysField(_)));

    let temporal = lower_expr(&Expr::TemporalRef("m_amt".to_string()), &ctx);
    assert!(matches!(temporal.kind, TypedExprKind::TemporalRef(_)));

    let field = lower_expr(
        &Expr::FieldAccess(
            Box::new(Expr::Ident("m_amt".to_string())),
            "clone".to_string(),
        ),
        &ctx,
    );
    assert!(matches!(field.kind, TypedExprKind::FieldAccess { .. }));

    let passthrough = lower_expr(
        &Expr::FnCall(
            "min".to_string(),
            vec![Expr::IntLiteral(U256::from_u128(1)), Expr::IntLiteral(U256::from_u128(2))],
        ),
        &ctx,
    );
    assert!(matches!(passthrough.kind, TypedExprKind::AstPassthrough(_)));
}
