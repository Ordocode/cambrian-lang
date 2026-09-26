// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Typed lowering for invariant `check` expressions (INV-TYPED).
//! Forge inline path: single-entity + multi-instance `require(...)` conditions.

use crate::ast::{Entity, Expr, Program, Type, UnaryOp};
use crate::codegen::invariant_predicate_types::{
    hashmap_key_type, member_field_type_multi, member_field_type_single, route_return_type,
};
use crate::codegen::predicate_expr;

use super::{gen_expr_test_typed, is_address_type};
use crate::codegen::solidity::core::ctx::{gen_expr_test, EvmCtx};
use crate::codegen::solidity::core::expr::binop_str;
use crate::codegen::solidity::core::types::hex_literal_to_sol_for_ty;

/// Fail-loud: inline checks must not silently become `require(true, ...)`.
fn invariant_check_gen_expr(entity: &Entity, expr: &Expr) -> String {
    gen_expr_test(expr, entity).unwrap_or_else(|| {
        panic!(
            "INVARIANT_CHECK_UNLOWERED: {:?} — extend invariant_predicate_lower or classify as I17",
            expr
        )
    })
}

fn binop_cmp_op_str(op: &crate::ast::BinOp) -> &'static str {
    use crate::ast::BinOp;
    match op {
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        _ => "==",
    }
}

fn lower_address_literal_for_compare(expr: &Expr) -> Option<String> {
    if let Expr::IntLiteral(v) = expr {
        return Some(hex_literal_to_sol_for_ty(
            &crate::ast::u256_hex_digits(v),
            Some("address"),
        ));
    }
    None
}

fn try_lower_address_literal_compare(
    op: &crate::ast::BinOp,
    lhs_expr: &Expr,
    rhs_expr: &Expr,
    lhs_sol: &str,
    rhs_sol: &str,
) -> Option<String> {
    use crate::ast::BinOp;
    if !matches!(op, BinOp::Eq | BinOp::Ne) {
        return None;
    }
    let op_str = binop_cmp_op_str(op);
    if let Some(rs) = lower_address_literal_for_compare(rhs_expr) {
        return Some(format!("({} {} {})", lhs_sol, op_str, rs));
    }
    if let Some(ls) = lower_address_literal_for_compare(lhs_expr) {
        return Some(format!("({} {} {})", ls, op_str, rhs_sol));
    }
    None
}

fn lower_hashmap_index_single(
    entity: &Entity,
    member: &str,
    key: &Expr,
    sut_var: &str,
) -> Option<String> {
    let m = entity.members.iter().find(|mem| mem.name == member)?;
    let key_ty = hashmap_key_type(&m.ty)?;
    let k = gen_expr_test_typed(key, entity, &key_ty, None);
    Some(format!("{}.{member}({k})", sut_var))
}

fn route_call_needs_sut_qualifier(route: &crate::ast::Route) -> bool {
    !route.is_init && route.name != "constructor"
}

fn member_getter_solidity(sut_var: &str, member: &str) -> String {
    format!("{}.{member}()", sut_var)
}

fn lower_hashmap_index_multi(
    entity: &Entity,
    inst: &str,
    member: &str,
    key: &Expr,
) -> Option<String> {
    let m = entity.members.iter().find(|mem| mem.name == member)?;
    let key_ty = hashmap_key_type(&m.ty)?;
    Some(gen_expr_test_typed(key, entity, &key_ty, None))
        .map(|k| format!("_{}.{}({})", inst, member, k))
}

/// Lower a single-entity invariant `check` for Foundry `require(...)`.
pub(crate) fn lower_single_entity_check(
    program: &Program,
    entity: &Entity,
    expr: &Expr,
    _ctx: &EvmCtx,
    sut_var: &str,
    handler_state_idents: &[String],
) -> String {
    let lower_arg = |ty: &Type, a: &Expr| -> String {
        if matches!(a, Expr::FnCall(..)) {
            lower_single_entity_check(program, entity, a, _ctx, sut_var, handler_state_idents)
        } else if let Expr::Ident(name) = a {
            if entity
                .members
                .iter()
                .any(|m| m.name == *name && !m.is_identity)
            {
                member_getter_solidity(sut_var, name)
            } else {
                gen_expr_test_typed(a, entity, ty, None)
            }
        } else {
            gen_expr_test_typed(a, entity, ty, None)
        }
    };

    match expr {
        Expr::FnCall(name, args) => {
            if let Some(route) = entity.routes.iter().find(|r| r.name == *name) {
                let arg_strs: Vec<String> = route
                    .params
                    .iter()
                    .zip(args.iter())
                    .map(|(p, a)| lower_arg(&p.ty, a))
                    .collect();
                if route_call_needs_sut_qualifier(route) {
                    return format!(
                        "{}.{}({})",
                        sut_var,
                        name,
                        arg_strs.join(", ")
                    );
                }
                return format!("{}({})", name, arg_strs.join(", "));
            }
            if let Some(pf) = program.pure_fns.iter().find(|f| f.name == *name) {
                let arg_strs: Vec<String> = pf
                    .params
                    .iter()
                    .zip(args.iter())
                    .map(|(p, a)| lower_arg(&p.ty, a))
                    .collect();
                return format!("{}({})", name, arg_strs.join(", "));
            }
            invariant_check_gen_expr(entity, expr)
        }
        Expr::MethodCall(recv, method, args) => {
            if (method == "len" || method == "length") && args.is_empty() {
                if let Expr::Ident(m) = recv.as_ref() {
                    if entity.members.iter().any(|mem| mem.name == *m && !mem.is_identity) {
                        return format!("{}.length()", member_getter_solidity(sut_var, m));
                    }
                    return format!("{}.length()", m);
                }
            }
            invariant_check_gen_expr(entity, expr)
        }
        Expr::Ident(name) => {
            if handler_state_idents.iter().any(|s| s == name) {
                return format!("_handler.{}()", name);
            }
            if let Some(m) = entity.members.iter().find(|mem| mem.name == *name) {
                if !m.is_identity {
                    return member_getter_solidity(sut_var, name);
                }
            }
            invariant_check_gen_expr(entity, expr)
        }
        Expr::Index(base, key) => {
            if let Expr::Ident(member) = base.as_ref() {
                if let Some(s) = lower_hashmap_index_single(entity, member, key, sut_var) {
                    return s;
                }
            }
            invariant_check_gen_expr(entity, expr)
        }
        Expr::BinOp(l, op, r) => {
            use crate::ast::BinOp;
            if matches!(op, BinOp::Eq | BinOp::Ne) {
                if let Some(ty) = member_field_type_single(l.as_ref(), entity)
                    .or_else(|| route_return_type(l.as_ref(), entity))
                {
                    if is_address_type(&ty) {
                        let ls = lower_single_entity_check(program, entity, l, _ctx, sut_var, handler_state_idents);
                        if let Some(cond) = try_lower_address_literal_compare(op, l, r, &ls, "") {
                            return cond;
                        }
                    }
                }
                if let Some(ty) = member_field_type_single(r.as_ref(), entity)
                    .or_else(|| route_return_type(r.as_ref(), entity))
                {
                    if is_address_type(&ty) {
                        let rs = lower_single_entity_check(program, entity, r, _ctx, sut_var, handler_state_idents);
                        if let Some(cond) = try_lower_address_literal_compare(op, l, r, "", &rs) {
                            return cond;
                        }
                    }
                }
            }
            let ls = lower_single_entity_check(program, entity, l, _ctx, sut_var, handler_state_idents);
            let rs = lower_single_entity_check(program, entity, r, _ctx, sut_var, handler_state_idents);
            format!("({} {} {})", ls, binop_str(op), rs)
        }
        Expr::UnaryOp(op, inner) => {
            let s = lower_single_entity_check(program, entity, inner, _ctx, sut_var, handler_state_idents);
            match op {
                UnaryOp::Not => format!("!({})", s),
                UnaryOp::Neg => format!("-({})", s),
                UnaryOp::Deref => s,
            }
        }
        _ => invariant_check_gen_expr(entity, expr),
    }
}

/// Lower a multi-instance invariant `check` expression.
pub(crate) fn lower_multi_entity_check(
    expr: &Expr,
    entity_for_inst: &[(String, &Entity)],
) -> String {
    use crate::ast::{BinOp, UnaryOp};
    fn is_inst(name: &str, ents: &[(String, &Entity)]) -> bool {
        ents.iter().any(|(n, _)| n == name)
    }
    match expr {
        Expr::FieldAccess(inner, member) => {
            if let Expr::Ident(name) = inner.as_ref() {
                if is_inst(name, entity_for_inst) {
                    return format!("_{}.{}()", name, member);
                }
            }
            let recv = lower_multi_entity_check(inner, entity_for_inst);
            format!("{}.{}", recv, member)
        }
        Expr::MethodCall(recv, method, args) => {
            if (method == "len" || method == "length") && args.is_empty() {
                if let Expr::FieldAccess(inner, member) = recv.as_ref() {
                    if let Expr::Ident(inst) = inner.as_ref() {
                        if is_inst(inst, entity_for_inst) {
                            return format!("_{}.{}().length()", inst, member);
                        }
                    }
                }
            }
            if let Expr::Ident(inst) = recv.as_ref() {
                if is_inst(inst, entity_for_inst) {
                    if let Some((_, entity)) = entity_for_inst.iter().find(|(n, _)| n == inst) {
                        if let Some(route) = entity.routes.iter().find(|r| r.name == *method) {
                            let arg_strs: Vec<String> = route
                                .params
                                .iter()
                                .zip(args.iter())
                                .map(|(p, a)| gen_expr_test_typed(a, entity, &p.ty, None))
                                .collect();
                            return format!("_{}.{}({})", inst, method, arg_strs.join(", "));
                        }
                    }
                }
            }
            if (method == "len" || method == "length") && args.is_empty() {
                let recv_s = lower_multi_entity_check(recv, entity_for_inst);
                return format!("{}.length()", recv_s);
            }
            format!(
                "{}.{}({})",
                lower_multi_entity_check(recv, entity_for_inst),
                method,
                args
                    .iter()
                    .map(|a| lower_multi_entity_check(a, entity_for_inst))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Expr::BinOp(l, op, r) => {
            if matches!(op, BinOp::Eq | BinOp::Ne) {
                if let Some(ty) = member_field_type_multi(l, entity_for_inst) {
                    if is_address_type(&ty) {
                        let ls = lower_multi_entity_check(l, entity_for_inst);
                        if let Some(cond) = try_lower_address_literal_compare(op, l, r, &ls, "") {
                            return cond;
                        }
                    }
                }
                if let Some(ty) = member_field_type_multi(r, entity_for_inst) {
                    if is_address_type(&ty) {
                        let rs = lower_multi_entity_check(r, entity_for_inst);
                        if let Some(cond) = try_lower_address_literal_compare(op, l, r, "", &rs) {
                            return cond;
                        }
                    }
                }
            }
            let ls = lower_multi_entity_check(l, entity_for_inst);
            let rs = lower_multi_entity_check(r, entity_for_inst);
            let op_str = match op {
                BinOp::Add | BinOp::WrappingAdd => "+",
                BinOp::Sub | BinOp::WrappingSub => "-",
                BinOp::Mul | BinOp::WrappingMul => "*",
                BinOp::Div => "/",
                BinOp::Mod => "%",
                BinOp::Eq => "==",
                BinOp::Ne => "!=",
                BinOp::Lt => "<",
                BinOp::Le => "<=",
                BinOp::Gt => ">",
                BinOp::Ge => ">=",
                BinOp::And => "&&",
                BinOp::Or => "||",
                BinOp::BitAnd => "&",
                BinOp::BitOr => "|",
                BinOp::BitXor => "^",
                BinOp::Shl => "<<",
                BinOp::Shr => ">>",
            };
            format!("({} {} {})", ls, op_str, rs)
        }
        Expr::Ident(name) if is_inst(name, entity_for_inst) => {
            format!("address(_{}) != address(0)", name)
        }
        Expr::UnaryOp(op, inner) => {
            let s = lower_multi_entity_check(inner, entity_for_inst);
            match op {
                UnaryOp::Not => format!("!({})", s),
                UnaryOp::Neg => format!("-({})", s),
                UnaryOp::Deref => s,
            }
        }
        Expr::Index(base, key) => {
            if let Expr::FieldAccess(inner, member) = base.as_ref() {
                if let Expr::Ident(name) = inner.as_ref() {
                    if is_inst(name, entity_for_inst) {
                        if let Some((_, entity)) =
                            entity_for_inst.iter().find(|(n, _)| n == name)
                        {
                            if let Some(s) =
                                lower_hashmap_index_multi(entity, name, member, key)
                            {
                                return s;
                            }
                        }
                        let k = lower_multi_entity_check(key, entity_for_inst);
                        return format!("_{}.{}({})", name, member, k);
                    }
                }
            }
            if let Expr::Ident(member) = base.as_ref() {
                if let Some((inst, entity)) = entity_for_inst.first() {
                    if let Some(s) =
                        lower_hashmap_index_single(entity, member, key, &format!("_{}", inst))
                    {
                        return s;
                    }
                }
            }
            format!(
                "{}[{}]",
                lower_multi_entity_check(base, entity_for_inst),
                lower_multi_entity_check(key, entity_for_inst)
            )
        }
        Expr::TraceField(field) if field == "length" => "_traceLen".to_string(),
        Expr::TraceCall { name, route } if name == "count" => format!("_traceCount_{}", route),
        Expr::TraceCall { name, route } if name == "lastWas" => format!("_traceLast_{}", route),
        _ => entity_for_inst
            .first()
            .map(|(_, e)| invariant_check_gen_expr(e, expr))
            .unwrap_or_else(|| {
                unreachable!(
                    "I17 skipped (bug): {}",
                    predicate_expr::i17_message("multi-entity", 0)
                )
            }),
    }
}
