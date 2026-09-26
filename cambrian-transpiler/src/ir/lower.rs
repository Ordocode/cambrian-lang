// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! AST → IR lowering entry points.

use std::collections::HashMap;

use crate::analysis::{classify_message_dest, fail_closure, FailPropagation, ProgramGraphs};
use crate::ast::{
    Entity, Expr, FromClause, Pattern, Program, Route, RouteAction, RouteBody, Type, WhereClause,
};

use super::expr::{
    coerce_binop_operands, coerce_kind_for_cast, infer_binop_type, infer_type, InferCtx,
    TypedExpr, TypedExprKind, IR_BIN_LITERAL_CAST, IR_HEX_LITERAL_CAST,
};
use super::route::{IrStmt, IrTransform, PhaseIr, ProgramIr, RouteIr};
use super::ty::{resolve_type, ResolvedType};

/// Lowering context for one route.
pub struct LowerCtx<'a> {
    pub program: &'a Program,
    pub entity: &'a Entity,
    pub route: &'a Route,
    pub aliases: HashMap<String, Type>,
}

impl<'a> LowerCtx<'a> {
    pub fn new(program: &'a Program, entity: &'a Entity, route: &'a Route) -> Self {
        Self {
            program,
            entity,
            route,
            aliases: build_alias_map(program, entity),
        }
    }

    pub fn infer_ctx(&self) -> InferCtx<'a, '_> {
        InferCtx {
            entity: self.entity,
            route_params: &self.route.params,
            aliases: &self.aliases,
        }
    }
}

pub fn build_alias_map(program: &Program, entity: &Entity) -> HashMap<String, Type> {
    let mut map = HashMap::new();
    for ta in &program.type_aliases {
        map.insert(ta.name.clone(), ta.ty.clone());
    }
    for ta in &entity.type_aliases {
        map.insert(ta.name.clone(), ta.ty.clone());
    }
    map
}

pub fn lower_program(program: &Program, graphs: &ProgramGraphs) -> ProgramIr {
    let mut routes = HashMap::new();
    for entity in &program.entities {
        for route in &entity.routes {
            let key = (entity.name.clone(), route.name.clone());
            routes.insert(key, lower_route(program, entity, route, graphs));
        }
    }
    ProgramIr { routes }
}

pub fn lower_route(
    program: &Program,
    entity: &Entity,
    route: &Route,
    graphs: &ProgramGraphs,
) -> RouteIr {
    let ctx = LowerCtx::new(program, entity, route);
    let facts = graphs
        .route_facts
        .get(&(entity.name.clone(), route.name.clone()));
    let can_fail = fail_closure(&graphs.route_facts, FailPropagation::EVM_LEAN);
    let fail_mode = can_fail
        .get(&(entity.name.clone(), route.name.clone()))
        .copied()
        .unwrap_or(false);
    let needs_world = facts
        .map(|f| f.has_unphased_sends || f.phased_needs_world_thread)
        .unwrap_or(false);

    let phases = match &route.body {
        RouteBody::Unphased(actions) => vec![lower_unphased_phase(
            &ctx, program, graphs, route, None, &route.from_clauses, &route.where_clauses, actions,
        )],
        RouteBody::Phased(phase_blocks) => phase_blocks
            .iter()
            .map(|phase| {
                lower_unphased_phase(
                    &ctx,
                    program,
                    graphs,
                    route,
                    Some(phase.name.clone()),
                    &[],
                    &phase.where_clauses,
                    &phase.actions,
                )
            })
            .collect(),
        RouteBody::Mixed(phase_blocks, trailing) => {
            let mut out: Vec<PhaseIr> = phase_blocks
                .iter()
                .map(|phase| {
                    lower_unphased_phase(
                        &ctx,
                        program,
                        graphs,
                        route,
                        Some(phase.name.clone()),
                        &[],
                        &phase.where_clauses,
                        &phase.actions,
                    )
                })
                .collect();
            out.push(lower_unphased_phase(
                &ctx,
                program,
                graphs,
                route,
                None,
                &route.from_clauses,
                &route.where_clauses,
                trailing,
            ));
            out
        }
    };

    RouteIr {
        entity: entity.name.clone(),
        name: route.name.clone(),
        fail_mode,
        needs_world,
        phases,
    }
}

fn lower_unphased_phase(
    ctx: &LowerCtx<'_>,
    program: &Program,
    graphs: &ProgramGraphs,
    route: &Route,
    phase_name: Option<String>,
    from_guards: &[FromClause],
    where_guards: &[WhereClause],
    actions: &[RouteAction],
) -> PhaseIr {
    PhaseIr {
        name: phase_name.clone(),
        from_guards: from_guards.to_vec(),
        where_guards: where_guards.to_vec(),
        transforms: collect_transforms(ctx, graphs, route, phase_name.as_deref()),
        stmts: lower_actions(ctx, program, route, actions, &HashMap::new()),
    }
}

fn collect_transforms(
    ctx: &LowerCtx<'_>,
    graphs: &ProgramGraphs,
    route: &Route,
    phase_name: Option<&str>,
) -> Vec<IrTransform> {
    let temporal = graphs
        .temporal
        .get(&ctx.entity.name)
        .and_then(|orders| orders.iter().find(|o| o.route_name == route.name));

    let mut transforms: Vec<IrTransform> = Vec::new();
    for member in &ctx.entity.members {
        if member.is_identity {
            continue;
        }
        for transform in &member.transforms {
            if transform.route_name != route.name {
                continue;
            }
            let matches_phase = match (phase_name, transform.phase.as_deref()) {
                (None, None) => true,
                (Some(p), Some(tp)) => p == tp,
                _ => false,
            };
            if !matches_phase {
                continue;
            }
            transforms.push(IrTransform {
                member: member.name.clone(),
                phase: transform.phase.clone(),
                body: lower_expr(&transform.body, ctx),
            });
        }
    }

    if let Some(order) = temporal {
        transforms.sort_by_key(|t| {
            order
                .order
                .iter()
                .position(|n| n == &t.member)
                .unwrap_or(usize::MAX)
        });
    }

    transforms
}

fn type_to_resolved(ty: &Type) -> ResolvedType {
    match ty {
        Type::Simple(name) => ResolvedType::Simple(name.clone()),
        Type::Generic(name, params) => ResolvedType::Generic {
            name: name.clone(),
            params: params.iter().map(type_to_resolved).collect(),
        },
        Type::Tuple(elems) => {
            ResolvedType::Tuple(elems.iter().map(type_to_resolved).collect())
        }
        Type::TypedAddress(entity) => ResolvedType::TypedAddress(entity.clone()),
    }
}

pub fn lower_expr(expr: &Expr, ctx: &LowerCtx<'_>) -> TypedExpr {
    let infer = || infer_type(expr, &ctx.infer_ctx());

    match expr {
        Expr::Ident(name) => {
            let ty = infer().unwrap_or(ResolvedType::simple("unknown"));
            TypedExpr {
                ty,
                kind: TypedExprKind::Ident(name.clone()),
            }
        }
        Expr::IntLiteral(v) => TypedExpr {
            ty: infer().unwrap_or(ResolvedType::simple("u64")),
            kind: TypedExprKind::IntLiteral(v.to_display_decimal()),
        },
        Expr::BoolLiteral(b) => TypedExpr {
            ty: ResolvedType::simple("bool"),
            kind: TypedExprKind::BoolLiteral(*b),
        },
        Expr::StringLiteral(s) => TypedExpr {
            ty: ResolvedType::simple("String"),
            kind: TypedExprKind::StringLiteral(s.clone()),
        },
        Expr::Cast(inner, ty) => {
            let to = type_to_resolved(ty);
            if let Type::Simple(name) = ty {
                if name == IR_HEX_LITERAL_CAST {
                    if let Expr::StringLiteral(digits) = inner.as_ref() {
                        return TypedExpr {
                            ty: to,
                            kind: TypedExprKind::HexLiteral(digits.clone()),
                        };
                    }
                }
                if name == IR_BIN_LITERAL_CAST {
                    if let Expr::StringLiteral(digits) = inner.as_ref() {
                        return TypedExpr {
                            ty: to,
                            kind: TypedExprKind::BinLiteral(digits.clone()),
                        };
                    }
                }
            }
            let inner_expr = lower_expr(inner, ctx);
            let resolved = resolve_type(ty, &ctx.aliases);
            if let Some(kind) = coerce_kind_for_cast(&inner_expr.ty, &resolved) {
                TypedExpr {
                    ty: resolved.clone(),
                    kind: TypedExprKind::Coerce {
                        kind,
                        expr: Box::new(inner_expr),
                        to: resolved,
                    },
                }
            } else {
                TypedExpr {
                    ty: resolved,
                    kind: TypedExprKind::Cast {
                        expr: Box::new(inner_expr),
                        to,
                    },
                }
            }
        }
        Expr::MsgField(field) => TypedExpr {
            ty: infer().unwrap_or(ResolvedType::simple("unknown")),
            kind: TypedExprKind::MsgField(field.clone()),
        },
        Expr::SysField(field) => TypedExpr {
            ty: infer().unwrap_or(ResolvedType::simple("unknown")),
            kind: TypedExprKind::SysField(field.clone()),
        },
        Expr::TemporalRef(name) => TypedExpr {
            ty: infer().unwrap_or(ResolvedType::simple("unknown")),
            kind: TypedExprKind::TemporalRef(name.clone()),
        },
        Expr::FieldAccess(base, field) => {
            let base_expr = lower_expr(base, ctx);
            TypedExpr {
                ty: infer().unwrap_or(base_expr.ty.clone()),
                kind: TypedExprKind::FieldAccess {
                    base: Box::new(base_expr),
                    field: field.clone(),
                },
            }
        }
        Expr::BinOp(lhs, op, rhs) => {
            let l = lower_expr(lhs, ctx);
            let r = lower_expr(rhs, ctx);
            let (l, r) = coerce_binop_operands(l, r, op, lhs, rhs);
            let ty = infer_binop_type(&l.ty, &r.ty);
            TypedExpr {
                ty,
                kind: TypedExprKind::BinOp {
                    lhs: Box::new(l),
                    op: op.clone(),
                    rhs: Box::new(r),
                },
            }
        }
        other => TypedExpr {
            ty: infer().unwrap_or(ResolvedType::simple("unknown")),
            kind: TypedExprKind::AstPassthrough(Box::new(other.clone())),
        },
    }
}

fn lower_action(
    ctx: &LowerCtx<'_>,
    program: &Program,
    route: &Route,
    action: &RouteAction,
    let_env: &HashMap<String, Expr>,
) -> IrStmt {
    match action {
        RouteAction::Let { pattern, value } => IrStmt::Let {
            pattern: pattern.clone(),
            value: lower_expr(value, ctx),
        },
        RouteAction::Return { values } => IrStmt::Return {
            values: values.iter().map(|v| lower_expr(v, ctx)).collect(),
        },
        RouteAction::Throw { error_code } => IrStmt::Throw {
            code: *error_code,
        },
        RouteAction::ThrowCustom { name, args } => IrStmt::ThrowCustom {
            name: name.clone(),
            args: args.iter().map(|a| lower_expr(a, ctx)).collect(),
        },
        RouteAction::Emit {
            event_name,
            args,
        } => IrStmt::Emit {
            event: event_name.clone(),
            args: args.iter().map(|a| lower_expr(a, ctx)).collect(),
        },
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => IrStmt::Conditional {
            cond: lower_expr(condition, ctx),
            then_stmts: lower_actions(ctx, program, route, then_actions, let_env),
            else_stmts: lower_actions(ctx, program, route, else_actions, let_env),
        },
        RouteAction::For { pattern, iter, body } => IrStmt::For {
            pattern: pattern.clone(),
            iter: lower_expr(iter, ctx),
            body: lower_actions(ctx, program, route, body, let_env),
        },
        RouteAction::Effect {
            namespace,
            name,
            args,
        } => IrStmt::Effect {
            namespace: namespace.clone(),
            name: name.clone(),
            args: args.iter().map(|a| lower_expr(a, ctx)).collect(),
        },
        RouteAction::Send {
            message,
            args,
            dest,
            send_options,
        } => {
            let target = classify_message_dest(
                dest,
                message.as_deref().unwrap_or(""),
                ctx.entity,
                route,
                program,
                let_env,
            );
            IrStmt::Send {
                message: message.clone(),
                args: args.iter().map(|a| lower_expr(a, ctx)).collect(),
                dest: lower_expr(dest, ctx),
                target,
                send_options: send_options.clone(),
            }
        }
        RouteAction::VarCall {
            name,
            message,
            args,
            dest,
            send_options,
        } => {
            let target =
                classify_message_dest(dest, message, ctx.entity, route, program, let_env);
            IrStmt::VarCall {
                name: name.clone(),
                message: message.clone(),
                args: args.iter().map(|a| lower_expr(a, ctx)).collect(),
                dest: lower_expr(dest, ctx),
                target,
                send_options: send_options.clone(),
            }
        }
        RouteAction::Deploy {
            entity,
            send_options,
            constructor_args,
        } => IrStmt::Deploy {
            entity: entity.clone(),
            constructor_args: constructor_args
                .iter()
                .map(|a| lower_expr(a, ctx))
                .collect(),
            send_options: send_options.clone(),
        },
        RouteAction::CallRoute { name, args } => IrStmt::CallRoute {
            name: name.clone(),
            args: args.iter().map(|a| lower_expr(a, ctx)).collect(),
        },
        RouteAction::Rescue { tag, action } => IrStmt::Rescue {
            tag: tag.clone(),
            action: Box::new(lower_action(ctx, program, route, action, let_env)),
        },
        RouteAction::UpdateCode {
            update_args,
            callback_route,
            callback_args,
        } => IrStmt::UpdateCode {
            update_args: update_args
                .iter()
                .map(|a| lower_expr(a, ctx))
                .collect(),
            callback_route: callback_route.clone(),
            callback_args: callback_args
                .iter()
                .map(|a| lower_expr(a, ctx))
                .collect(),
        },
    }
}

fn lower_actions(
    ctx: &LowerCtx<'_>,
    program: &Program,
    route: &Route,
    actions: &[RouteAction],
    outer_let_env: &HashMap<String, Expr>,
) -> Vec<IrStmt> {
    let mut let_env = outer_let_env.clone();
    let mut stmts = Vec::with_capacity(actions.len());
    for action in actions {
        stmts.push(lower_action(ctx, program, route, action, &let_env));
        if let RouteAction::Let {
            pattern: Pattern::Ident(name),
            value,
        } = action
        {
            let_env.insert(name.clone(), value.clone());
        }
    }
    stmts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{ProgramGraphs, SendTarget};
    use crate::ast::BinOp;
    use crate::ir::expr::{CoerceKind, TypedExprKind};

    fn parse(src: &str) -> Program {
        crate::ProgramParser::new()
            .parse(src)
            .unwrap_or_else(|e| panic!("parse failed: {e}"))
    }

    #[test]
    fn lower_counter_routes_have_phases_and_content() {
        let src = include_str!("../../../contracts/counter.cam");
        let program = parse(src);
        let graphs = ProgramGraphs::build(&program);
        let ir = lower_program(&program, &graphs);

        let increment = ir
            .routes
            .get(&("Counter".to_string(), "increment".to_string()))
            .expect("increment route");
        assert!(!increment.phases.is_empty());
        assert!(!increment.phases[0].transforms.is_empty());

        let get_count = ir
            .routes
            .get(&("Counter".to_string(), "getCount".to_string()))
            .expect("getCount route");
        assert!(!get_count.phases.is_empty());
        assert!(!get_count.phases[0].stmts.is_empty());
    }

    #[test]
    fn lower_send_records_send_target() {
        let program = parse(
            r#"
            entity A {
                identity id: u64
                routes {
                    poke() => [ bump() ~> A.address(id) ]
                    bump() => []
                }
            }
            "#,
        );
        let graphs = ProgramGraphs::build(&program);
        let ir = lower_program(&program, &graphs);
        let poke = ir.routes.get(&("A".to_string(), "poke".to_string())).unwrap();
        let send = poke.phases[0]
            .stmts
            .iter()
            .find_map(|s| match s {
                IrStmt::Send { target, .. } => Some(target),
                _ => None,
            })
            .expect("send stmt");
        assert!(matches!(send, SendTarget::SameEntity { .. }));
    }

    #[test]
    fn lower_phased_route_has_named_phases() {
        let program = parse(
            r#"
            entity E {
                routes {
                    go() => [
                        p1: [ call helper() ]
                        p2: [ call helper() ]
                    ]
                    helper() => []
                }
            }
            "#,
        );
        let graphs = ProgramGraphs::build(&program);
        let entity = &program.entities[0];
        let route = entity.routes.iter().find(|r| r.name == "go").unwrap();
        let ir = lower_route(&program, entity, route, &graphs);
        assert_eq!(ir.phases.len(), 2);
        assert_eq!(ir.phases[0].name.as_deref(), Some("p1"));
        assert_eq!(ir.phases[1].name.as_deref(), Some("p2"));
    }

    #[test]
    fn lower_expr_binop_inserts_widen_coerce() {
        let program = parse(
            r#"
            entity E {
                routes {
                    add(a: u8, b: u64) => []
                }
            }
            "#,
        );
        let entity = &program.entities[0];
        let route = &entity.routes[0];
        let ctx = LowerCtx::new(&program, entity, route);
        let expr = Expr::BinOp(
            Box::new(Expr::Ident("a".to_string())),
            BinOp::Add,
            Box::new(Expr::Ident("b".to_string())),
        );
        let typed = lower_expr(&expr, &ctx);
        match typed.kind {
            TypedExprKind::BinOp { lhs, .. } => match lhs.kind {
                TypedExprKind::Coerce {
                    kind: CoerceKind::Widen,
                    ..
                } => {}
                other => panic!("expected widen coerce on lhs, got {other:?}"),
            },
            other => panic!("expected binop, got {other:?}"),
        }
    }
}
