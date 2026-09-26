// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM adapter — route / action / constructor / initialize emission.
//!
//! Route bodies follow **Checks → Effects → Interactions** (CEI):
//! `where`/`from` checks, then member transforms (SSTORE), then sends/deploys/calls.
//! See [`super`] module docs for the full adapter policy.

use crate::ast::{
    Entity, Expr, Member, MemberTransform, Param, Pattern, PhaseBlock, Program, Route, RouteAction,
    RouteBody, Type, WhereClause,
};
use cambrian_core::U256;
use std::collections::HashMap as StdHashMap;
use std::collections::HashSet as StdHashSet;

use super::emitter::EvmActionEmitter;
use super::factory::*;
use super::transform::*;
use crate::analysis::{ProgramGraphs, SendTarget, route_infer_evm_view};
use crate::codegen::adapter::ActionEmitter;
use crate::codegen::dispatch_action;
use crate::codegen::solidity::core::create2::gen_create2_address_expr;
use crate::codegen::solidity::core::ctx::EmitScope;
use crate::codegen::solidity::core::ctx::EvmCtx;
use crate::codegen::solidity::core::expr::{gen_expr, gen_expr_hoisted};
use crate::codegen::solidity::core::iter::{
    gen_for_scalar_reduce, pop_iter_pattern_bindings, push_iter_pattern_bindings,
};
use crate::codegen::solidity::core::scratch::EmitScratch;
use crate::codegen::solidity::core::types::*;
use crate::codegen::types::resolve_target_entity;
use crate::codegen::{collect_transforms, order_transforms_temporally, ResolvedTransform};
use crate::ir::{self, IrStmt, IrTransform, PhaseIr, RouteIr, TypedExpr, TypedExprKind};
use std::cell::RefCell;

fn send_target_entity_name(target: &SendTarget, self_entity: &str) -> Option<String> {
    match target {
        SendTarget::SameEntity { .. } => Some(self_entity.to_string()),
        SendTarget::CrossEntity { entity, .. }
        | SendTarget::DynamicTyped { entity, .. }
        | SendTarget::ExternEntity { entity } => Some(entity.clone()),
        SendTarget::DynamicUntyped { .. } | SendTarget::Raw => None,
    }
}

pub(super) fn resolve_send_target(
    target: &SendTarget,
    dest: &Expr,
    self_entity: &str,
    entity: &Entity,
    route: &Route,
) -> Option<String> {
    send_target_entity_name(target, self_entity)
        .or_else(|| resolve_target_entity(dest, entity, route))
}

fn resolve_ir_transforms<'a>(
    entity: &'a Entity,
    route: &Route,
    phase_name: Option<&str>,
    ir_transforms: &[IrTransform],
) -> Vec<(&'a Member, &'a MemberTransform)> {
    ir_transforms
        .iter()
        .filter_map(|t| {
            let member = entity.members.iter().find(|m| m.name == t.member)?;
            let transform = member
                .transforms
                .iter()
                .find(|tr| tr.route_name == route.name && tr.phase.as_deref() == phase_name)?;
            Some((member, transform))
        })
        .collect()
}

fn gen_member_updates_from_phase_ir(
    entity: &Entity,
    route: &Route,
    phase_ir: &PhaseIr,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let (compute, commit) =
        gen_member_updates_from_phase_ir_split(entity, route, phase_ir, ctx, scratch);
    format!("{}{}", compute, commit)
}

/// `(compute, commit)` for one IR phase, so the caller can splice
/// pre-commit snapshots between the two halves.
fn gen_member_updates_from_phase_ir_split(
    entity: &Entity,
    route: &Route,
    phase_ir: &PhaseIr,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> (String, String) {
    let ordered = resolve_ir_transforms(
        entity,
        route,
        phase_ir.name.as_deref(),
        &phase_ir.transforms,
    );
    let resolved: Vec<ResolvedTransform> = ordered
        .iter()
        .map(|(member, transform)| ResolvedTransform {
            member,
            transform,
            phase: phase_ir.name.as_deref(),
        })
        .collect();
    let scope = EmitScope::for_transform(entity, &route.name, phase_ir.name.as_deref());
    emit_transforms_sol_split(&resolved, entity, route, ctx, &scope, scratch)
}

/// Member updates for one IR phase, with the state its effects read
/// captured into locals ahead of the commit.
///
/// Returns the Solidity to emit before the statements, and the
/// statements rewritten to read those locals instead of storage. The IR
/// counterpart of [`gen_updates_with_snapshots`], which still serves the
/// constructor / `initialize` paths that emit from the AST.
fn gen_updates_with_snapshots_from_phase_ir(
    entity: &Entity,
    route: &Route,
    phase_ir: &PhaseIr,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> (String, Vec<IrStmt>) {
    let (compute, commit) =
        gen_member_updates_from_phase_ir_split(entity, route, phase_ir, ctx, scratch);

    let committed: StdHashSet<String> = phase_ir
        .transforms
        .iter()
        .map(|t| t.member.clone())
        .collect();
    let mut outbound = Vec::new();
    for stmt in &phase_ir.stmts {
        ir_stmt_outbound_exprs(stmt, &mut outbound);
    }
    let snapshots = snapshots_from_reads(
        entity,
        committed_reads_in(entity, &committed, &outbound),
        ctx,
    );
    if snapshots.is_empty() {
        return (format!("{}{}", compute, commit), phase_ir.stmts.clone());
    }

    let scope = EmitScope::for_transform(entity, &route.name, None);
    let decls = emit_snapshot_decls(&snapshots, entity, ctx, &scope, scratch);
    let stmts = phase_ir
        .stmts
        .iter()
        .map(|s| rewrite_ir_stmt_snapshots(s, &snapshots))
        .collect();

    (format!("{}{}{}", compute, decls, commit), stmts)
}

pub fn materialize_typed_expr(te: &TypedExpr) -> Expr {
    match &te.kind {
        TypedExprKind::AstPassthrough(e) => (**e).clone(),
        TypedExprKind::Ident(name) => Expr::Ident(name.clone()),
        TypedExprKind::IntLiteral(s) => {
            Expr::IntLiteral(U256::from_decimal_str(s).unwrap_or(U256::ZERO))
        }
        TypedExprKind::HexLiteral(s) => Expr::Cast(
            Box::new(Expr::StringLiteral(s.clone())),
            Type::Simple(crate::ir::IR_HEX_LITERAL_CAST.to_string()),
        ),
        TypedExprKind::BinLiteral(s) => Expr::Cast(
            Box::new(Expr::StringLiteral(s.clone())),
            Type::Simple(crate::ir::IR_BIN_LITERAL_CAST.to_string()),
        ),
        TypedExprKind::BoolLiteral(b) => Expr::BoolLiteral(*b),
        TypedExprKind::StringLiteral(s) => Expr::StringLiteral(s.clone()),
        TypedExprKind::MsgField(field) => Expr::MsgField(field.clone()),
        TypedExprKind::SysField(field) => Expr::SysField(field.clone()),
        TypedExprKind::TemporalRef(name) => Expr::TemporalRef(name.clone()),
        TypedExprKind::FieldAccess { base, field } => {
            Expr::FieldAccess(Box::new(materialize_typed_expr(base)), field.clone())
        }
        TypedExprKind::BinOp { lhs, op, rhs } => Expr::BinOp(
            Box::new(materialize_typed_expr(lhs)),
            op.clone(),
            Box::new(materialize_typed_expr(rhs)),
        ),
        TypedExprKind::Cast { expr, to } => {
            Expr::Cast(Box::new(materialize_typed_expr(expr)), to.to_ast())
        }
        TypedExprKind::Coerce { expr, to, .. } => {
            let inner = materialize_typed_expr(expr);
            Expr::Cast(Box::new(inner), to.to_ast())
        }
    }
}

fn materialize_typed_exprs(values: &[TypedExpr]) -> Vec<Expr> {
    values.iter().map(materialize_typed_expr).collect()
}

fn ir_stmt_to_route_action(stmt: &IrStmt) -> Option<RouteAction> {
    match stmt {
        IrStmt::AstAction(action) => Some(action.clone()),
        IrStmt::Send {
            message,
            args,
            dest,
            send_options,
            ..
        } => Some(RouteAction::Send {
            message: message.clone(),
            args: materialize_typed_exprs(args),
            dest: materialize_typed_expr(dest),
            send_options: send_options.clone(),
        }),
        IrStmt::VarCall {
            name,
            message,
            args,
            dest,
            send_options,
            ..
        } => Some(RouteAction::VarCall {
            name: name.clone(),
            message: message.clone(),
            args: materialize_typed_exprs(args),
            dest: materialize_typed_expr(dest),
            send_options: send_options.clone(),
        }),
        IrStmt::Deploy {
            entity,
            constructor_args,
            send_options,
        } => Some(RouteAction::Deploy {
            entity: entity.clone(),
            constructor_args: materialize_typed_exprs(constructor_args),
            send_options: send_options.clone(),
        }),
        // Rescue inners still round-trip through RouteAction so the
        // E26 comment in `format_rescue` can name the tag. No try/catch.
        IrStmt::CallRoute { name, args } => Some(RouteAction::CallRoute {
            name: name.clone(),
            args: materialize_typed_exprs(args),
        }),
        _ => None,
    }
}

struct RouteEmitMode {
    use_det_action: bool,
    var_call_assignments: bool,
    reindent_actions: bool,
}

fn gen_ir_stmt_evm(
    stmt: &IrStmt,
    entity: &Entity,
    route: &Route,
    program: &Program,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
    indent: &str,
    mode: &RouteEmitMode,
) -> String {
    let scope = EmitScope::for_entity(entity);
    let emitter = EvmActionEmitter::new(ctx, scope, scratch);
    match stmt {
        IrStmt::AstAction(action) => {
            if mode.use_det_action {
                gen_action_det(action, entity, route, program, ctx, scratch)
            } else {
                gen_action(action, entity, route, program, ctx, scratch)
            }
        }
        IrStmt::Let { pattern, value } => {
            let value_expr = materialize_typed_expr(value);
            emitter.emit_let(pattern, &value_expr, entity, route, program, indent)
        }
        IrStmt::Return { values } => {
            let values = materialize_typed_exprs(values);
            emitter.emit_return(&values, entity, route, program, indent)
        }
        IrStmt::Throw { code } => emitter.emit_throw(*code, indent),
        IrStmt::ThrowCustom { name, args } => {
            let args = materialize_typed_exprs(args);
            emitter.emit_throw_custom(name, &args, entity, route, program, indent)
        }
        IrStmt::Emit { event, args } => {
            let args = materialize_typed_exprs(args);
            emitter.emit_emit(event, &args, entity, route, program, indent)
        }
        IrStmt::Send {
            message,
            args,
            dest,
            target,
            send_options,
        } => {
            let dest_expr = materialize_typed_expr(dest);
            let args = materialize_typed_exprs(args);
            emitter.emit_send_with_target(
                message,
                &args,
                &dest_expr,
                send_options.as_ref(),
                target,
                entity,
                route,
                program,
                indent,
            )
        }
        IrStmt::VarCall {
            name,
            message,
            args,
            dest,
            target,
            send_options,
        } => {
            let dest_expr = materialize_typed_expr(dest);
            let args = materialize_typed_exprs(args);
            if mode.var_call_assignments {
                gen_var_call_assignment_with_target(
                    name,
                    message,
                    &args,
                    &dest_expr,
                    send_options.as_ref(),
                    target,
                    entity,
                    route,
                    program,
                    indent,
                    ctx,
                    scratch,
                )
            } else {
                emitter.emit_var_call_with_target(
                    name,
                    message,
                    &args,
                    &dest_expr,
                    send_options.as_ref(),
                    target,
                    entity,
                    route,
                    program,
                    indent,
                )
            }
        }
        IrStmt::Deploy {
            entity: target_entity,
            constructor_args,
            send_options,
        } => {
            let constructor_args = materialize_typed_exprs(constructor_args);
            if mode.use_det_action {
                gen_deploy_via_factory(
                    target_entity,
                    send_options.as_ref(),
                    &constructor_args,
                    entity,
                    program,
                    ctx,
                    scratch,
                )
            } else {
                emitter.emit_deploy(
                    target_entity,
                    send_options.as_ref(),
                    &constructor_args,
                    entity,
                    route,
                    program,
                    indent,
                )
            }
        }
        IrStmt::CallRoute { name, args } => {
            let args = materialize_typed_exprs(args);
            emitter.emit_call_route(name, &args, entity, route, indent)
        }
        IrStmt::Effect {
            namespace,
            name,
            args,
        } => {
            let args = materialize_typed_exprs(args);
            emitter.emit_effect(namespace, name, &args, entity, route, indent)
        }
        IrStmt::Conditional {
            cond,
            then_stmts,
            else_stmts,
        } => {
            let condition = materialize_typed_expr(cond);
            let inner_indent = format!("{}    ", indent);
            let then_code: String = then_stmts
                .iter()
                .map(|s| {
                    gen_ir_stmt_evm(s, entity, route, program, ctx, scratch, &inner_indent, mode)
                })
                .collect();
            let else_code: String = else_stmts
                .iter()
                .map(|s| {
                    gen_ir_stmt_evm(s, entity, route, program, ctx, scratch, &inner_indent, mode)
                })
                .collect();
            emitter.format_conditional(&condition, &then_code, &else_code, entity, route, indent)
        }
        IrStmt::For {
            pattern,
            iter,
            body,
        } => {
            let iter_expr = materialize_typed_expr(iter);
            let inner_indent = format!("{}    ", indent);
            let idx = scratch.borrow_mut().next_tmp();
            let (_, _, elem) =
                emitter.action_for_iter_elem(&iter_expr, route, entity, &idx);
            let pushed = push_iter_pattern_bindings(pattern, &elem, scratch);
            let body_code: String = body
                .iter()
                .map(|s| {
                    gen_ir_stmt_evm(s, entity, route, program, ctx, scratch, &inner_indent, mode)
                })
                .collect();
            pop_iter_pattern_bindings(&pushed, scratch);
            emitter.format_for(pattern, &iter_expr, &body_code, entity, route, indent)
        }
        IrStmt::Rescue { tag, action } => {
            if let Some(inner_action) = ir_stmt_to_route_action(action) {
                let inner_code = if mode.use_det_action {
                    gen_action_det(&inner_action, entity, route, program, ctx, scratch)
                } else {
                    gen_action(&inner_action, entity, route, program, ctx, scratch)
                };
                emitter.format_rescue(
                    tag,
                    &inner_code,
                    &inner_action,
                    entity,
                    route,
                    program,
                    indent,
                )
            } else {
                String::new()
            }
        }
        IrStmt::UpdateCode {
            update_args,
            callback_route,
            callback_args,
        } => {
            let update_args = materialize_typed_exprs(update_args);
            let callback_args = materialize_typed_exprs(callback_args);
            emitter.emit_update_code(
                &update_args,
                callback_route,
                &callback_args,
                entity,
                route,
                program,
                indent,
            )
        }
    }
}

fn emit_hoisted_var_call_decls_from_phase(
    out: &mut String,
    phase_ir: &PhaseIr,
    entity: &Entity,
    route: &Route,
    program: &Program,
    ctx: &EvmCtx,
    nested_hoist: &StdHashMap<String, String>,
) {
    for stmt in &phase_ir.stmts {
        if let IrStmt::VarCall {
            name,
            message,
            dest,
            target,
            ..
        } = stmt
        {
            if nested_hoist.contains_key(name) {
                continue;
            }
            let dest_expr = materialize_typed_expr(dest);
            let target_name = resolve_send_target(target, &dest_expr, &entity.name, entity, route);
            let ret_ty = resolve_var_call_type_with_target(
                message,
                &dest_expr,
                target_name.as_deref(),
                entity,
                route,
                program,
                ctx,
            );
            // Reads lower through `sol_sanitize_ident` (`var after = ...`
            // is read back as `_after`), so the hoisted declaration must
            // carry the same escaped name.
            out.push_str(&format!(
                "        {} {};\n",
                ret_ty,
                sol_sanitize_ident(name)
            ));
        }
    }
}

fn emit_named_phase_block_from_ir(
    out: &mut String,
    phase_ir: &PhaseIr,
    entity: &Entity,
    route: &Route,
    program: &Program,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
    _nested_hoist: &StdHashMap<String, String>,
    mode: &RouteEmitMode,
) {
    let phase_name = phase_ir.name.as_deref().unwrap_or("");
    out.push_str(&format!("        // Phase: {}\n        {{\n", phase_name));
    out.push_str(&gen_phase_where_checks(
        &phase_ir.where_guards,
        phase_name,
        ctx,
        entity,
        scratch,
    ));
    let (updates, stmts) =
        gen_updates_with_snapshots_from_phase_ir(entity, route, phase_ir, ctx, scratch);
    for line in updates.lines() {
        out.push_str(&format!("    {}\n", line));
    }
    for stmt in &stmts {
        let action_str = gen_ir_stmt_evm(
            stmt,
            entity,
            route,
            program,
            ctx,
            scratch,
            "            ",
            mode,
        );
        if mode.reindent_actions && !mode.var_call_assignments {
            for line in action_str.lines() {
                out.push_str(&format!("    {}\n", line));
            }
        } else {
            out.push_str(&action_str);
        }
    }
    out.push_str("        }\n");
}

fn emit_route_body_from_ir(
    out: &mut String,
    route_ir: &RouteIr,
    entity: &Entity,
    route: &Route,
    program: &Program,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
    nested_hoist: &StdHashMap<String, String>,
    mode: &RouteEmitMode,
) {
    let named: Vec<&PhaseIr> = route_ir
        .phases
        .iter()
        .filter(|p| p.name.is_some())
        .collect();
    let trailing = route_ir.phases.iter().find(|p| p.name.is_none());

    if named.is_empty() {
        if let Some(phase_ir) = trailing {
            emit_trailing_phase_from_ir(out, phase_ir, entity, route, program, ctx, scratch, mode);
        }
        return;
    }

    if mode.var_call_assignments {
        for phase_ir in &named {
            emit_hoisted_var_call_decls_from_phase(
                out,
                phase_ir,
                entity,
                route,
                program,
                ctx,
                nested_hoist,
            );
        }
    }

    for phase_ir in &named {
        emit_named_phase_block_from_ir(
            out,
            phase_ir,
            entity,
            route,
            program,
            ctx,
            scratch,
            nested_hoist,
            mode,
        );
    }

    if let Some(phase_ir) = trailing {
        emit_trailing_phase_from_ir(out, phase_ir, entity, route, program, ctx, scratch, mode);
    }
}

/// Unphased (trailing) body: commit transforms, then run the statements.
/// Effect arguments read pre-commit snapshots, like a named phase.
#[allow(clippy::too_many_arguments)]
fn emit_trailing_phase_from_ir(
    out: &mut String,
    phase_ir: &PhaseIr,
    entity: &Entity,
    route: &Route,
    program: &Program,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
    mode: &RouteEmitMode,
) {
    let (updates, stmts) =
        gen_updates_with_snapshots_from_phase_ir(entity, route, phase_ir, ctx, scratch);
    out.push_str(&updates);
    for stmt in &stmts {
        out.push_str(&gen_ir_stmt_evm(
            stmt, entity, route, program, ctx, scratch, "        ", mode,
        ));
    }
}

// ---------------------------------------------------------------------------
// Route precondition checks
// ---------------------------------------------------------------------------

pub(super) fn gen_where_checks(
    where_clauses: &[WhereClause],
    ctx: &EvmCtx,
    entity: &Entity,
    scratch: &RefCell<EmitScratch>,
) -> String {
    gen_where_checks_with_indent(where_clauses, "        ", ctx, entity, scratch)
}

fn gen_where_checks_with_indent(
    where_clauses: &[WhereClause],
    indent: &str,
    ctx: &EvmCtx,
    entity: &Entity,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let scope = EmitScope::for_entity(entity);
    let mut out = String::new();
    for wc in where_clauses {
        let cond =
            gen_expr(&wc.condition, ctx, &scope, scratch).unwrap_or_else(|| "false".to_string());
        if let Some(err_name) = &wc.error_name {
            // Phase EVM-P0-D: custom error revert.
            let args: Vec<String> = wc
                .error_args
                .iter()
                .map(|a| gen_expr(a, ctx, &scope, scratch).unwrap_or_else(|| "0".to_string()))
                .collect();
            out.push_str(&format!(
                "{indent}if (!({cond})) revert {name}({a});\n",
                indent = indent,
                cond = cond,
                name = err_name,
                a = args.join(", "),
            ));
        } else {
            // Legacy numeric throw.
            // Match the revert-message format expected by the test
            // harness (`expect throw(N)` lowers to `vm.expectRevert(bytes("throw(N)"))`).
            out.push_str(&format!(
                "{}require({}, \"throw({})\");\n",
                indent, cond, wc.error_code
            ));
        }
    }
    out
}

/// Per-phase where checks. Emitted with 12-space indent (inside the phase
/// `{ }` block), using a label that includes the phase name so a runtime
/// failure points back to the right place in the source.
pub(super) fn gen_phase_where_checks(
    where_clauses: &[WhereClause],
    phase_name: &str,
    ctx: &EvmCtx,
    entity: &Entity,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let _ = phase_name;
    gen_where_checks_with_indent(where_clauses, "            ", ctx, entity, scratch)
}

pub(super) fn gen_from_checks(
    route: &Route,
    ctx: &EvmCtx,
    entity: &Entity,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let scope = EmitScope::for_entity(entity);
    use crate::ast::FromClauseKind;

    if route.from_clauses.is_empty() {
        return String::new();
    }
    let mut checks: Vec<String> = Vec::new();
    for clause in &route.from_clauses {
        match clause.kind {
            FromClauseKind::Member => {
                // Address-typed member of the current entity. Lower as a
                // direct equality check against the storage slot. No
                // CREATE2 salt computation needed.
                checks.push(format!("(msg.sender == {})", clause.entity_name));
                continue;
            }
            FromClauseKind::Entity => {
                if clause.args.len() == 1 {
                    if let Some(addr_expr) = gen_expr(&clause.args[0], ctx, &scope, scratch) {
                        let addr = crate::codegen::solidity::core::types::coerce_to_address_sol(
                            &clause.args[0],
                            &addr_expr,
                            entity,
                            ctx,
                            scratch,
                        );
                        checks.push(format!("(msg.sender == {})", addr));
                        continue;
                    }
                }
                // Phase EVM-6 M2: V33 should reject any non-single-arg
                // `from Entity(args)` in non-deterministic mode before
                // codegen runs. The literal-`false` fallback is kept as
                // defence in depth so codegen always produces compilable
                // Solidity even on an unvalidated program.
                debug_assert!(
                    false,
                    "EVM-6 M2: from-clause arity {} should have been rejected by validator V33 in non-deterministic mode",
                    clause.args.len(),
                );
                checks.push("false".to_string());
            }
        }
    }
    // Phase EVM-P0-D: prefer a custom-error revert when every from-clause
    // shares the same `: throw CustomErr(args)` annotation. Falls back to
    // the legacy numeric `throw(N)` revert string otherwise.
    let names: Vec<&String> = route
        .from_clauses
        .iter()
        .filter_map(|c| c.error_name.as_ref())
        .collect();
    if names.len() == route.from_clauses.len() && names.windows(2).all(|w| w[0] == w[1]) {
        if let Some(first) = route.from_clauses.first() {
            if let Some(err_name) = &first.error_name {
                let args: Vec<String> = first
                    .error_args
                    .iter()
                    .map(|a| gen_expr(a, ctx, &scope, scratch).unwrap_or_else(|| "0".to_string()))
                    .collect();
                return format!(
                    "        if (!({cond})) revert {name}({a});\n",
                    cond = checks.join(" || "),
                    name = err_name,
                    a = args.join(", "),
                );
            }
        }
    }

    let codes: Vec<u32> = route
        .from_clauses
        .iter()
        .filter_map(|c| c.error_code)
        .collect();
    let msg = if codes.len() == route.from_clauses.len() && codes.windows(2).all(|w| w[0] == w[1]) {
        match codes.first() {
            Some(c) => format!("throw({})", c),
            None => "from clause failed".to_string(),
        }
    } else {
        "from clause failed".to_string()
    };
    format!("        require({}, \"{}\");\n", checks.join(" || "), msg,)
}

pub(super) fn gen_params_entity(entity: &Entity, params: &[Param], ctx: &EvmCtx) -> String {
    let mut parts: Vec<String> = Vec::new();
    for p in params {
        let p_name = sol_sanitize_ident(&p.name);
        if is_mapping_type(&p.ty) {
            parts.push(format!(
                "{} storage {}",
                sol_type_entity(entity, &p.ty, false, ctx),
                p_name
            ));
            if let crate::ast::Type::Generic(_, tps) = &p.ty {
                if let Some(k_ty) = tps.first() {
                    let k_sol = sol_type(k_ty, false);
                    parts.push(format!(
                        "mapping({} => bool) storage {}_exists",
                        k_sol,
                        p_name
                    ));
                    parts.push(format!("{}[] storage {}_keys", k_sol, p_name));
                }
            }
        } else {
            parts.push(format!(
                "{} {}",
                sol_type_entity(entity, &p.ty, true, ctx),
                p_name
            ));
        }
    }
    parts.join(", ")
}

fn route_has_mapping_param(route: &Route) -> bool {
    route.params.iter().any(|p| is_mapping_type(&p.ty))
}

/// `call name(...)` is an in-contract invocation that keeps the caller's
/// `msg.sender`, so it lowers to a direct (internal) Solidity call. Private
/// routes are `internal`; a non-private `call` target must be `public`
/// because `external` functions cannot be called internally.
fn route_function_visibility(route: &Route, entity: &Entity) -> (&'static str, &'static str) {
    if route.is_private {
        ("internal", "_")
    } else if route_has_mapping_param(route) {
        // Solidity rejects mapping parameters on external/public entry
        // points — only `internal` may take `storage` mapping refs.
        ("internal", "")
    } else if entity_calls_route(entity, &route.name) {
        ("public", "")
    } else {
        ("external", "")
    }
}

fn entity_calls_route(entity: &Entity, target: &str) -> bool {
    let mut found = false;
    for r in &entity.routes {
        for action in r.body.all_actions() {
            crate::analysis::walk::for_each_action(action, &mut |a| {
                if let RouteAction::CallRoute { name, .. } = a {
                    if name == target {
                        found = true;
                    }
                }
            });
        }
    }
    found
}

fn push_route_param_bindings(route: &Route, entity: &Entity, ctx: &EvmCtx, scratch: &RefCell<EmitScratch>) {
    for p in &route.params {
        if is_mapping_type(&p.ty) {
            let pty = format!(
                "{} storage",
                sol_type_entity(entity, &p.ty, false, ctx)
            );
            scratch.borrow_mut().push_let_binding(&p.name, &pty);
            if let crate::ast::Type::Generic(_, tps) = &p.ty {
                if let Some(k_ty) = tps.first() {
                    let k_sol = sol_type(k_ty, false);
                    let exists_ty = format!("mapping({} => bool) storage", k_sol);
                    scratch
                        .borrow_mut()
                        .push_let_binding(&format!("{}_exists", p.name), &exists_ty);
                    scratch
                        .borrow_mut()
                        .push_let_binding(&format!("{}_keys", p.name), &format!("{}[] storage", k_sol));
                }
            }
        } else {
            let pty = sol_type_entity(entity, &p.ty, true, ctx);
            scratch.borrow_mut().push_let_binding(&p.name, &pty);
        }
    }
}

fn pop_route_param_bindings(route: &Route, scratch: &RefCell<EmitScratch>) {
    for p in &route.params {
        if is_mapping_type(&p.ty) {
            scratch
                .borrow_mut()
                .pop_let_binding(&format!("{}_keys", p.name));
            scratch
                .borrow_mut()
                .pop_let_binding(&format!("{}_exists", p.name));
        }
        scratch.borrow_mut().pop_let_binding(&p.name);
    }
}

// ---------------------------------------------------------------------------
// Send option extraction
// ---------------------------------------------------------------------------

pub(super) fn extract_send_option<'a>(opts: Option<&'a Expr>, key: &str) -> Option<&'a Expr> {
    match opts {
        Some(Expr::RecordConstruct(_, fields)) | Some(Expr::RecordUpdate(_, fields)) => fields
            .iter()
            .find_map(|(k, v)| if k == key { Some(v) } else { None }),
        _ => None,
    }
}

pub(super) fn infer_sig_type(expr: &Expr, scratch: &RefCell<EmitScratch>) -> &'static str {
    match expr {
        Expr::BoolLiteral(_) => "bool",
        Expr::StringLiteral(_) => "string",
        Expr::IntLiteral(v) if !v.fits_u128() => "uint256",
        Expr::MsgField(name) if name == "sender" => "address",
        Expr::Ident(name) => {
            // EVM-H12: use bound param / let types so address args encode
            // as `address`, not `uint256`.
            if let Some(ty) = scratch.borrow_mut().lookup_let_binding(name) {
                return sig_type_from_sol_ty(&ty);
            }
            "uint256"
        }
        _ => "uint256",
    }
}

fn sig_type_from_sol_ty(ty: &str) -> &'static str {
    let base = ty.split_whitespace().next().unwrap_or(ty);
    match base {
        "address" => "address",
        "bool" => "bool",
        "string" => "string",
        "bytes" => "bytes",
        "bytes32" => "bytes32",
        "uint8" => "uint8",
        "uint16" => "uint16",
        "uint32" => "uint32",
        "uint64" => "uint64",
        "uint128" => "uint128",
        "uint256" => "uint256",
        "int8" => "int8",
        "int16" => "int16",
        "int32" => "int32",
        "int64" => "int64",
        "int128" => "int128",
        "int256" => "int256",
        _ => "uint256",
    }
}

// ---------------------------------------------------------------------------
// Route action lowering
// ---------------------------------------------------------------------------

pub(super) fn gen_action(
    action: &RouteAction,
    entity: &Entity,
    route: &Route,
    program: &Program,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let scope = EmitScope::for_entity(entity);
    let emitter = EvmActionEmitter::new(ctx, scope, scratch);
    dispatch_action(&emitter, action, entity, route, program, "        ")
}

pub(super) fn resolve_var_call_type(
    message: &str,
    dest: &Expr,
    entity: &Entity,
    route: &Route,
    program: &Program,
    ctx: &EvmCtx,
) -> String {
    resolve_var_call_type_with_target(message, dest, None, entity, route, program, ctx)
}

fn resolve_var_call_type_with_target(
    message: &str,
    dest: &Expr,
    target_name: Option<&str>,
    entity: &Entity,
    route: &Route,
    program: &Program,
    ctx: &EvmCtx,
) -> String {
    let tn = target_name
        .map(|s| s.to_string())
        .or_else(|| resolve_target_entity(dest, entity, route));
    let Some(tn) = tn else {
        return "uint256".to_string();
    };

    if let Some(te) = program.entities.iter().find(|e| e.name == tn) {
        if let Some(tr) = te
            .routes
            .iter()
            .find(|r| r.name == message && !r.is_private)
        {
            if let Some(ty) = &tr.return_type {
                return sol_return_type(te, ty, ctx);
            }
        }
    }

    if let Some(ext) = program.extern_entities.iter().find(|e| e.name == tn) {
        if let Some(tr) = ext.routes.iter().find(|r| r.name == message) {
            if let Some(ty) = &tr.return_type {
                return sol_type(ty, true);
            }
        }
    }

    "uint256".to_string()
}

/// Assignment-only form of a capturing var-call for phased routes.
///
/// Declarations are emitted separately (`emit_hoisted_var_call_decls_from_phase`
/// / function-scope hoist). Re-declaring here shadows the outer binder
/// (EVM-H14 regression when this delegated to `emit_var_call_with_target`).
fn gen_var_call_assignment_with_target(
    name: &str,
    message: &str,
    args: &[Expr],
    dest: &Expr,
    send_options: Option<&Expr>,
    target: &SendTarget,
    entity: &Entity,
    route: &Route,
    _program: &Program,
    indent: &str,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let scope = EmitScope::for_entity(entity);
    let dest_expr =
        gen_expr(dest, ctx, &scope, scratch).unwrap_or_else(|| "address(0)".to_string());
    let arg_values: Vec<String> = args
        .iter()
        .map(|a| gen_expr(a, ctx, &scope, scratch).unwrap_or_else(|| "0".to_string()))
        .collect();
    let call_args = arg_values.join(", ");
    let target_name = resolve_send_target(target, dest, &entity.name, entity, route);
    let value_expr =
        extract_send_option(send_options, "value").and_then(|e| gen_expr(e, ctx, &scope, scratch));

    if let Some(ref tn) = target_name {
        let iface = ctx.interface_cast_name(tn);
        let call = if let Some(ref val) = value_expr {
            format!(
                "{}({}).{}{{value: {}}}({})",
                iface, dest_expr, message, val, call_args
            )
        } else {
            format!("{}({}).{}({})", iface, dest_expr, message, call_args)
        };
        // Match the escaped name the hoisted declaration and all reads use.
        format!("{}{} = {};\n", indent, sol_sanitize_ident(name), call)
    } else {
        crate::codegen::types::unresolved_var_call(name, &dest_expr)
    }
}

/// Collect `let` / `var` binders nested under `if` / `for` / `rescue` so they
/// can be declared at function scope (T-EVM-ST-001 / T-EVM-ST-002).
fn collect_nested_hoist_locals(
    actions: &[RouteAction],
    entity: &Entity,
    route: &Route,
    program: &Program,
    depth: usize,
    out: &mut StdHashMap<String, String>,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) {
    for action in actions {
        match action {
            RouteAction::Let {
                pattern: Pattern::Ident(name),
                value,
            } if depth > 0 => {
                let let_ty = match value {
                    Expr::StringLiteral(_) => "string memory".to_string(),
                    Expr::BoolLiteral(_) => "bool".to_string(),
                    Expr::RecordConstruct(rec, _) => format!("{} memory", rec),
                    _ => infer_let_type_entity(value, entity, ctx, scratch),
                };
                out.entry(name.clone()).or_insert(let_ty);
            }
            RouteAction::VarCall {
                name,
                message,
                dest,
                ..
            } if depth > 0 => {
                let ret_ty = resolve_var_call_type(message, dest, entity, route, program, ctx);
                out.entry(name.clone()).or_insert(ret_ty);
            }
            RouteAction::Conditional {
                then_actions,
                else_actions,
                ..
            } => {
                collect_nested_hoist_locals(
                    then_actions,
                    entity,
                    route,
                    program,
                    depth + 1,
                    out,
                    ctx,
                    scratch,
                );
                collect_nested_hoist_locals(
                    else_actions,
                    entity,
                    route,
                    program,
                    depth + 1,
                    out,
                    ctx,
                    scratch,
                );
            }
            RouteAction::For { body, .. } => {
                collect_nested_hoist_locals(
                    body,
                    entity,
                    route,
                    program,
                    depth + 1,
                    out,
                    ctx,
                    scratch,
                );
            }
            RouteAction::Rescue { action: inner, .. } => {
                collect_nested_hoist_locals(
                    std::slice::from_ref(inner.as_ref()),
                    entity,
                    route,
                    program,
                    depth + 1,
                    out,
                    ctx,
                    scratch,
                );
            }
            _ => {}
        }
    }
}

fn collect_route_body_hoist_locals(
    body: &RouteBody,
    entity: &Entity,
    route: &Route,
    program: &Program,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> StdHashMap<String, String> {
    let mut out = StdHashMap::new();
    match body {
        RouteBody::Unphased(actions) => {
            collect_nested_hoist_locals(actions, entity, route, program, 0, &mut out, ctx, scratch);
        }
        RouteBody::Phased(phases) => {
            for phase in phases {
                collect_nested_hoist_locals(
                    &phase.actions,
                    entity,
                    route,
                    program,
                    0,
                    &mut out,
                    ctx,
                    scratch,
                );
            }
            collect_cross_phase_lets(phases, &[], entity, ctx, scratch, &mut out);
        }
        RouteBody::Mixed(phases, actions) => {
            for phase in phases {
                collect_nested_hoist_locals(
                    &phase.actions,
                    entity,
                    route,
                    program,
                    0,
                    &mut out,
                    ctx,
                    scratch,
                );
            }
            collect_nested_hoist_locals(actions, entity, route, program, 0, &mut out, ctx, scratch);
            collect_cross_phase_lets(phases, actions, entity, ctx, scratch, &mut out);
        }
    }
    collect_rebound_top_level_lets(body, entity, ctx, scratch, &mut out);
    out
}

/// Solidity rejects a second declaration of a name in one function scope,
/// so a top-level `let x` that rebinds `x` is declared once at function
/// scope and every binding becomes an assignment.
fn collect_rebound_top_level_lets(
    body: &RouteBody,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
    out: &mut StdHashMap<String, String>,
) {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for action in body.all_actions() {
        let RouteAction::Let {
            pattern: Pattern::Ident(name),
            value,
        } = action
        else {
            continue;
        };
        if !seen.insert(name.as_str()) || out.contains_key(name) {
            if !out.contains_key(name) {
                let first = body.all_actions().into_iter().find_map(|a| match a {
                    RouteAction::Let {
                        pattern: Pattern::Ident(n),
                        value,
                    } if n == name => Some(value),
                    _ => None,
                });
                let ty = infer_let_type_entity(first.unwrap_or(value), entity, ctx, scratch);
                out.insert(name.clone(), ty);
            }
        }
    }
}

fn emit_hoisted_local_decls(
    locals: &StdHashMap<String, String>,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let mut out = String::new();
    let mut names: Vec<&String> = locals.keys().collect();
    names.sort();
    for name in names {
        let ty = &locals[name];
        // Emit the escaped spelling (reads sanitize through `gen_expr`);
        // the scratch table stays keyed by the raw Cambrian name.
        out.push_str(&format!(
            "        {} {};\n",
            ty,
            sol_sanitize_ident(name)
        ));
        scratch.borrow_mut().push_let_binding(name, ty);
    }
    out
}

fn begin_hoisted_locals(
    locals: StdHashMap<String, String>,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let decls = emit_hoisted_local_decls(&locals, scratch);
    scratch.borrow_mut().set_hoisted_locals(locals);
    decls
}

fn end_hoisted_locals(locals: &StdHashMap<String, String>, scratch: &RefCell<EmitScratch>) {
    for name in locals.keys() {
        scratch.borrow_mut().pop_let_binding(name);
    }
    scratch.borrow_mut().clear_hoisted_locals();
}

// ---------------------------------------------------------------------------
// Cross-contract call helper (EVM-specific: typed-address method call with return value)
// ---------------------------------------------------------------------------

/// If `base.method(args)` where `base: Address<SomeEntity>`, emit `ISomeEntity(base).method(args)`
/// and infer the return type from the target entity's route definition.
/// Returns `Some((sol_return_type, sol_expression))` or `None` if not a typed-addr call.
// ---------------------------------------------------------------------------
// Member update generation
// ---------------------------------------------------------------------------

pub(super) fn gen_member_updates(
    entity: &Entity,
    route: &Route,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let (compute, commit) = gen_member_updates_split(entity, route, None, ctx, scratch);
    format!("{}{}", compute, commit)
}

/// Unphased member updates of an init route plus its unphased actions,
/// rewritten so effect arguments read pre-commit snapshots. Phased init
/// bodies get their updates per phase; only the unphased half is returned.
fn init_updates(
    entity: &Entity,
    route: &Route,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> (String, Vec<RouteAction>) {
    match &route.body {
        RouteBody::Unphased(actions) => {
            gen_updates_with_snapshots(entity, route, None, actions, ctx, scratch)
        }
        _ => (gen_member_updates(entity, route, ctx, scratch), Vec::new()),
    }
}

/// `(compute, commit)` for a route body or one of its phases.
///
/// The two halves are handed back separately so the caller can insert
/// pre-commit snapshots of the state its effects read (see
/// `collect_precommit_snapshots`).
pub(super) fn gen_member_updates_split(
    entity: &Entity,
    route: &Route,
    phase_name: Option<&str>,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> (String, String) {
    let transforms = collect_transforms(entity, &route.name, phase_name);
    let (orders, _) = crate::analysis::build_temporal_orders(entity);
    let ordered = order_transforms_temporally(transforms, &orders, &route.name);

    let scope = EmitScope::for_transform(entity, &route.name, phase_name);
    emit_transforms_sol_split(&ordered, entity, route, ctx, &scope, scratch)
}

/// Apply `f` to every expression an action holds, rebuilding the action.
/// Recurses into the nested action lists (`if`, `for`, `rescue`).
///
/// `f` receives the owning action alongside the expression so a caller
/// can scope its rewrite to particular kinds of action.
pub(crate) fn map_action_exprs(
    action: &RouteAction,
    f: &dyn Fn(&RouteAction, &Expr) -> Expr,
) -> RouteAction {
    let m = |e: &Expr| f(action, e);
    let ms = |es: &Vec<Expr>| es.iter().map(|e| f(action, e)).collect::<Vec<_>>();
    let mas = |as_: &Vec<RouteAction>| {
        as_.iter()
            .map(|a| map_action_exprs(a, f))
            .collect::<Vec<_>>()
    };
    match action {
        RouteAction::Send {
            message,
            args,
            dest,
            send_options,
        } => RouteAction::Send {
            message: message.clone(),
            args: ms(args),
            dest: m(dest),
            send_options: send_options.as_ref().map(&m),
        },
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => RouteAction::Conditional {
            condition: m(condition),
            then_actions: mas(then_actions),
            else_actions: mas(else_actions),
        },
        RouteAction::Return { values } => RouteAction::Return { values: ms(values) },
        RouteAction::Let { pattern, value } => RouteAction::Let {
            pattern: pattern.clone(),
            value: m(value),
        },
        RouteAction::Effect {
            namespace,
            name,
            args,
        } => RouteAction::Effect {
            namespace: namespace.clone(),
            name: name.clone(),
            args: ms(args),
        },
        RouteAction::Deploy {
            entity,
            send_options,
            constructor_args,
        } => RouteAction::Deploy {
            entity: entity.clone(),
            send_options: send_options.as_ref().map(&m),
            constructor_args: ms(constructor_args),
        },
        RouteAction::Rescue { tag, action } => RouteAction::Rescue {
            tag: tag.clone(),
            action: Box::new(map_action_exprs(action, f)),
        },
        RouteAction::Throw { error_code } => RouteAction::Throw {
            error_code: *error_code,
        },
        RouteAction::ThrowCustom { name, args } => RouteAction::ThrowCustom {
            name: name.clone(),
            args: ms(args),
        },
        RouteAction::Emit { event_name, args } => RouteAction::Emit {
            event_name: event_name.clone(),
            args: ms(args),
        },
        RouteAction::CallRoute { name, args } => RouteAction::CallRoute {
            name: name.clone(),
            args: ms(args),
        },
        RouteAction::VarCall {
            name,
            message,
            args,
            dest,
            send_options,
        } => RouteAction::VarCall {
            name: name.clone(),
            message: message.clone(),
            args: ms(args),
            dest: m(dest),
            send_options: send_options.as_ref().map(&m),
        },
        RouteAction::UpdateCode {
            update_args,
            callback_route,
            callback_args,
        } => RouteAction::UpdateCode {
            update_args: ms(update_args),
            callback_route: callback_route.clone(),
            callback_args: ms(callback_args),
        },
        RouteAction::For {
            pattern,
            iter,
            body,
        } => RouteAction::For {
            pattern: pattern.clone(),
            iter: m(iter),
            body: mas(body),
        },
    }
}

/// Does this action leave the contract?
///
/// Only outbound actions get their member reads snapshotted. The
/// distinction matters: `return(m_result)` is the route's *output* and
/// has always reported the value the route just committed — a route
/// that computes a member and hands it back would otherwise return the
/// pre-call value. Emitted events read the same way. What genuinely
/// needs the pre-commit view is the argument to a call the contract
/// makes to someone else, because on EVM those are ordered after the
/// storage writes to keep checks-effects-interactions.
pub(crate) fn action_leaves_contract(action: &RouteAction) -> bool {
    matches!(
        action,
        RouteAction::Send { .. }
            | RouteAction::VarCall { .. }
            | RouteAction::Deploy { .. }
            | RouteAction::Effect { .. }
            | RouteAction::CallRoute { .. }
            | RouteAction::UpdateCode { .. }
    )
}

/// A member read that an effect performs against state the same phase
/// is about to overwrite.
struct PreCommitSnapshot {
    /// The expression as written in the source (`m_total_supply`, or
    /// `m_balances[sys::address]`).
    read: Expr,
    /// Name of the local the read is captured into.
    local: String,
    /// Solidity type of that local.
    ty: String,
}

/// The value type of a `HashMap<K, V>` member, or the member's own type
/// when it is not a mapping.
fn member_read_type(member_ty: &Type) -> &Type {
    match member_ty {
        Type::Generic(name, params) if name == "HashMap" && params.len() == 2 => &params[1],
        other => other,
    }
}

/// Snapshot the state an effect reads before the phase commits over it.
///
/// A phase is atomic: its member transforms and its effects all observe
/// the state as it stood when the phase began. Transforms already get
/// that — `emit_transforms_sol_split` computes every `next_*` off the
/// pre-phase snapshot before writing any of them back. Effects did not,
/// because they are emitted after the commit block and read storage
/// directly.
///
/// The gap is not academic. `UniswapV2Pair.burn` pays out
/// `mul_div(m_balances[this], bal0, m_total_supply)` in the same phase
/// that zeroes `m_balances[this]` and decrements `m_total_supply` to
/// zero, so the payout divided by zero and the route reverted for every
/// caller — the LP was unredeemable.
///
/// Rather than move the effects before the commit (which would put an
/// external call ahead of the state write and give up
/// checks-effects-interactions), capture each such read into a local
/// ahead of the commit and rewrite the effect to use it. Same values,
/// same ordering of the calls themselves.
fn collect_precommit_snapshots(
    entity: &Entity,
    committed: &StdHashSet<String>,
    actions: &[RouteAction],
    ctx: &EvmCtx,
) -> Vec<PreCommitSnapshot> {
    let outbound: RefCell<Vec<Expr>> = RefCell::new(Vec::new());
    for action in actions {
        map_action_exprs(action, &|owner, e| {
            if action_leaves_contract(owner) {
                outbound.borrow_mut().push(e.clone());
            }
            e.clone()
        });
    }
    let reads = committed_reads_in(entity, committed, &outbound.into_inner());
    snapshots_from_reads(entity, reads, ctx)
}

/// Reads of `committed` members anywhere in `exprs`.
///
/// A scalar is captured whole; a mapping only where it is indexed
/// (`m_balances[k]`) — the mapping itself is not a value on EVM.
pub(crate) fn committed_reads_in(
    entity: &Entity,
    committed: &StdHashSet<String>,
    exprs: &[Expr],
) -> Vec<Expr> {
    let found: RefCell<Vec<Expr>> = RefCell::new(Vec::new());
    let note = |e: &Expr| {
        let mut f = found.borrow_mut();
        if !f.contains(e) {
            f.push(e.clone());
        }
    };

    let is_committed_member = |name: &str| -> Option<&Member> {
        if !committed.contains(name) {
            return None;
        }
        entity.members.iter().find(|m| m.name == name)
    };

    for e in exprs {
        // `subst_expr_where` short-circuits on a match, so this pass only
        // records and never rewrites, keeping the walk going.
        subst_expr_where(e, &|inner| {
            match inner {
                Expr::Index(base, _) => {
                    if let Expr::Ident(name) = base.as_ref() {
                        if let Some(member) = is_committed_member(name) {
                            if is_mapping_type(&member.ty) {
                                note(inner);
                            }
                        }
                    }
                }
                Expr::Ident(name) => {
                    if let Some(member) = is_committed_member(name) {
                        if !is_mapping_type(&member.ty) && !is_vec_type(&member.ty) {
                            note(inner);
                        }
                    }
                }
                _ => {}
            }
            None
        });
    }

    found.into_inner()
}

/// Name the local each recorded read is captured into.
fn snapshots_from_reads(
    entity: &Entity,
    reads: Vec<Expr>,
    ctx: &EvmCtx,
) -> Vec<PreCommitSnapshot> {
    reads
        .into_iter()
        .enumerate()
        .map(|(i, read)| {
            let member_name = match &read {
                Expr::Index(base, _) => match base.as_ref() {
                    Expr::Ident(n) => n.clone(),
                    _ => unreachable!("only ident-based indexes are recorded"),
                },
                Expr::Ident(n) => n.clone(),
                _ => unreachable!("only idents and indexes are recorded"),
            };
            let member = entity
                .members
                .iter()
                .find(|m| m.name == member_name)
                .expect("recorded reads are entity members");
            let ty = sol_type_entity(entity, member_read_type(&member.ty), true, ctx);
            let local = format!("_pre_{}_{}", sol_sanitize_ident(&member_name), i);
            PreCommitSnapshot { read, local, ty }
        })
        .collect()
}

/// Emit the snapshot locals and rewrite `actions` to read them.
fn apply_precommit_snapshots(
    snapshots: &[PreCommitSnapshot],
    entity: &Entity,
    actions: &[RouteAction],
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> (String, Vec<RouteAction>) {
    if snapshots.is_empty() {
        return (String::new(), actions.to_vec());
    }

    let decls = emit_snapshot_decls(snapshots, entity, ctx, scope, scratch);

    let rewritten = actions
        .iter()
        .map(|a| {
            map_action_exprs(a, &|owner, e| {
                if !action_leaves_contract(owner) {
                    return e.clone();
                }
                subst_expr_where(e, &|inner| {
                    snapshots
                        .iter()
                        .find(|s| &s.read == inner)
                        .map(|s| Expr::Ident(s.local.clone()))
                })
            })
        })
        .collect();

    (decls, rewritten)
}

/// Declare one local per snapshot, hoisting whatever the read itself needs.
fn emit_snapshot_decls(
    snapshots: &[PreCommitSnapshot],
    entity: &Entity,
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let mut decls = String::new();
    for snap in snapshots {
        let (stmts, rhs) = gen_expr_hoisted(&snap.read, entity, ctx, scope, scratch);
        for s in &stmts {
            decls.push_str(&format!("        {}\n", s));
        }
        decls.push_str(&format!("        {} {} = {};\n", snap.ty, snap.local, rhs));
    }
    decls
}

/// Exprs an IR statement hands to the world outside this contract.
///
/// The IR mirror of `action_leaves_contract` + `map_action_exprs`: only
/// the statements that actually leave (sends, calls, deploys, effects)
/// contribute, and the nesting constructs are walked for them.
fn ir_stmt_outbound_exprs(stmt: &IrStmt, out: &mut Vec<Expr>) {
    let push_all = |tes: &[TypedExpr], out: &mut Vec<Expr>| {
        out.extend(tes.iter().map(materialize_typed_expr));
    };
    match stmt {
        IrStmt::Send {
            args,
            dest,
            send_options,
            ..
        } => {
            push_all(args, out);
            out.push(materialize_typed_expr(dest));
            out.extend(send_options.iter().cloned());
        }
        IrStmt::VarCall {
            args,
            dest,
            send_options,
            ..
        } => {
            push_all(args, out);
            out.push(materialize_typed_expr(dest));
            out.extend(send_options.iter().cloned());
        }
        IrStmt::Deploy {
            constructor_args,
            send_options,
            ..
        } => {
            push_all(constructor_args, out);
            out.extend(send_options.iter().cloned());
        }
        IrStmt::Effect { args, .. } | IrStmt::CallRoute { args, .. } => push_all(args, out),
        IrStmt::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            push_all(update_args, out);
            push_all(callback_args, out);
        }
        IrStmt::Conditional {
            then_stmts,
            else_stmts,
            ..
        } => {
            for s in then_stmts.iter().chain(else_stmts) {
                ir_stmt_outbound_exprs(s, out);
            }
        }
        IrStmt::For { body, .. } => {
            for s in body {
                ir_stmt_outbound_exprs(s, out);
            }
        }
        IrStmt::Rescue { action, .. } => ir_stmt_outbound_exprs(action, out),
        IrStmt::AstAction(action) => {
            let collected: RefCell<Vec<Expr>> = RefCell::new(Vec::new());
            map_action_exprs(action, &|owner, e| {
                if action_leaves_contract(owner) {
                    collected.borrow_mut().push(e.clone());
                }
                e.clone()
            });
            out.extend(collected.into_inner());
        }
        IrStmt::Let { .. }
        | IrStmt::Return { .. }
        | IrStmt::Throw { .. }
        | IrStmt::ThrowCustom { .. }
        | IrStmt::Emit { .. } => {}
    }
}

/// Point every snapshotted read inside `te` at its local.
///
/// A read can arrive either as its own typed node (`m_total`) or buried
/// in an AST passthrough (`mul_div(m_shares[k], ...)`, which has no
/// typed counterpart), so both levels are rewritten. The node's resolved
/// type carries over — a snapshot local holds exactly what the read did.
fn subst_typed_expr_snapshots(te: &TypedExpr, snapshots: &[PreCommitSnapshot]) -> TypedExpr {
    if let Some(snap) = snapshots
        .iter()
        .find(|s| s.read == materialize_typed_expr(te))
    {
        return TypedExpr {
            ty: te.ty.clone(),
            kind: TypedExprKind::Ident(snap.local.clone()),
        };
    }

    let kind = match &te.kind {
        TypedExprKind::AstPassthrough(e) => {
            TypedExprKind::AstPassthrough(Box::new(subst_expr_where(e, &|inner| {
                snapshots
                    .iter()
                    .find(|s| &s.read == inner)
                    .map(|s| Expr::Ident(s.local.clone()))
            })))
        }
        TypedExprKind::BinOp { lhs, op, rhs } => TypedExprKind::BinOp {
            lhs: Box::new(subst_typed_expr_snapshots(lhs, snapshots)),
            op: op.clone(),
            rhs: Box::new(subst_typed_expr_snapshots(rhs, snapshots)),
        },
        TypedExprKind::Coerce { kind, expr, to } => TypedExprKind::Coerce {
            kind: *kind,
            expr: Box::new(subst_typed_expr_snapshots(expr, snapshots)),
            to: to.clone(),
        },
        TypedExprKind::Cast { expr, to } => TypedExprKind::Cast {
            expr: Box::new(subst_typed_expr_snapshots(expr, snapshots)),
            to: to.clone(),
        },
        TypedExprKind::FieldAccess { base, field } => TypedExprKind::FieldAccess {
            base: Box::new(subst_typed_expr_snapshots(base, snapshots)),
            field: field.clone(),
        },
        other => other.clone(),
    };

    TypedExpr {
        ty: te.ty.clone(),
        kind,
    }
}

/// Rewrite the outbound parts of an IR statement to read the snapshots.
fn rewrite_ir_stmt_snapshots(stmt: &IrStmt, snapshots: &[PreCommitSnapshot]) -> IrStmt {
    let sub = |te: &TypedExpr| subst_typed_expr_snapshots(te, snapshots);
    let subs = |tes: &Vec<TypedExpr>| tes.iter().map(&sub).collect::<Vec<_>>();
    let sub_opt = |e: &Option<Expr>| {
        e.as_ref().map(|e| {
            subst_expr_where(e, &|inner| {
                snapshots
                    .iter()
                    .find(|s| &s.read == inner)
                    .map(|s| Expr::Ident(s.local.clone()))
            })
        })
    };
    let subs_stmts = |ss: &Vec<IrStmt>| {
        ss.iter()
            .map(|s| rewrite_ir_stmt_snapshots(s, snapshots))
            .collect::<Vec<_>>()
    };

    match stmt {
        IrStmt::Send {
            message,
            args,
            dest,
            target,
            send_options,
        } => IrStmt::Send {
            message: message.clone(),
            args: subs(args),
            dest: sub(dest),
            target: target.clone(),
            send_options: sub_opt(send_options),
        },
        IrStmt::VarCall {
            name,
            message,
            args,
            dest,
            target,
            send_options,
        } => IrStmt::VarCall {
            name: name.clone(),
            message: message.clone(),
            args: subs(args),
            dest: sub(dest),
            target: target.clone(),
            send_options: sub_opt(send_options),
        },
        IrStmt::Deploy {
            entity,
            constructor_args,
            send_options,
        } => IrStmt::Deploy {
            entity: entity.clone(),
            constructor_args: subs(constructor_args),
            send_options: sub_opt(send_options),
        },
        IrStmt::Effect {
            namespace,
            name,
            args,
        } => IrStmt::Effect {
            namespace: namespace.clone(),
            name: name.clone(),
            args: subs(args),
        },
        IrStmt::CallRoute { name, args } => IrStmt::CallRoute {
            name: name.clone(),
            args: subs(args),
        },
        IrStmt::UpdateCode {
            update_args,
            callback_route,
            callback_args,
        } => IrStmt::UpdateCode {
            update_args: subs(update_args),
            callback_route: callback_route.clone(),
            callback_args: subs(callback_args),
        },
        IrStmt::Conditional {
            cond,
            then_stmts,
            else_stmts,
        } => IrStmt::Conditional {
            cond: cond.clone(),
            then_stmts: subs_stmts(then_stmts),
            else_stmts: subs_stmts(else_stmts),
        },
        IrStmt::For {
            pattern,
            iter,
            body,
        } => IrStmt::For {
            pattern: pattern.clone(),
            iter: iter.clone(),
            body: subs_stmts(body),
        },
        IrStmt::Rescue { tag, action } => IrStmt::Rescue {
            tag: tag.clone(),
            action: Box::new(rewrite_ir_stmt_snapshots(action, snapshots)),
        },
        IrStmt::AstAction(action) => IrStmt::AstAction(map_action_exprs(action, &|owner, e| {
            if !action_leaves_contract(owner) {
                return e.clone();
            }
            subst_expr_where(e, &|inner| {
                snapshots
                    .iter()
                    .find(|s| &s.read == inner)
                    .map(|s| Expr::Ident(s.local.clone()))
            })
        })),
        other => other.clone(),
    }
}

/// Names of the members a route (optionally: one phase of it) commits.
fn committed_member_names(
    entity: &Entity,
    route: &Route,
    phase_name: Option<&str>,
) -> StdHashSet<String> {
    collect_transforms(entity, &route.name, phase_name)
        .iter()
        .map(|rt| rt.member.name.clone())
        .collect()
}

/// Member updates for a route body or phase, with the state its effects
/// read snapshotted ahead of the commit.
///
/// Returns the Solidity to emit before the actions, and the actions
/// rewritten to read the snapshot locals instead of storage.
pub(super) fn gen_updates_with_snapshots(
    entity: &Entity,
    route: &Route,
    phase_name: Option<&str>,
    actions: &[RouteAction],
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> (String, Vec<RouteAction>) {
    let (compute, commit) = gen_member_updates_split(entity, route, phase_name, ctx, scratch);
    let committed = committed_member_names(entity, route, phase_name);
    let snapshots = collect_precommit_snapshots(entity, &committed, actions, ctx);
    let scope = EmitScope::for_transform(entity, &route.name, None);
    let (decls, rewritten) =
        apply_precommit_snapshots(&snapshots, entity, actions, ctx, &scope, scratch);
    (format!("{}{}{}", compute, decls, commit), rewritten)
}

/// Idents any of `actions` mentions, at any depth.
fn idents_mentioned(actions: &[RouteAction]) -> StdHashSet<String> {
    let seen = RefCell::new(StdHashSet::new());
    let record = |e: &Expr| -> Expr {
        subst_expr_where(e, &|sub| {
            if let Expr::Ident(name) = sub {
                seen.borrow_mut().insert(name.clone());
            }
            None
        })
    };
    for action in actions {
        // `map_action_exprs` walks nested actions for us; the rewritten copy
        // it builds is thrown away — the closure is here for its side effect.
        let _ = map_action_exprs(action, &|_owner, e| record(e));
    }
    seen.into_inner()
}

/// Phase-level `let` bindings a later phase reads.
///
/// A phase body lowers to a Solidity `{ }` block, so a `let` declared inside
/// one is out of scope by the next phase — while the validator accepts the
/// forward reference, because at the Cambrian level a route body is one scope.
/// The mismatch produced Solidity that `solc` rejected with `Undeclared
/// identifier`, which is loud but only after the fact, and it made the
/// documented "bind the pre-state you need into a local" idiom unusable in
/// exactly the routes that need it: a route that mutates a member and must
/// answer a value derived from its pre-state has nowhere else to put the
/// snapshot, because member transforms cannot see route-body `let`s (V8) and
/// `var` only binds a cross-contract call.
///
/// Only bindings actually read later are hoisted. A `let` confined to one
/// phase keeps its block scope, so a route that does not cross a phase
/// boundary lowers byte-for-byte as before.
fn collect_cross_phase_lets(
    phases: &[PhaseBlock],
    trailing: &[RouteAction],
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
    out: &mut StdHashMap<String, String>,
) {
    for (i, phase) in phases.iter().enumerate() {
        let mut later = idents_mentioned(trailing);
        for follower in &phases[i + 1..] {
            later.extend(idents_mentioned(&follower.actions));
        }
        if later.is_empty() {
            continue;
        }
        for action in &phase.actions {
            if let RouteAction::Let {
                pattern: Pattern::Ident(name),
                value,
            } = action
            {
                if !later.contains(name.as_str()) {
                    continue;
                }
                let let_ty = match value {
                    Expr::StringLiteral(_) => "string memory".to_string(),
                    Expr::BoolLiteral(_) => "bool".to_string(),
                    Expr::RecordConstruct(rec, _) => format!("{} memory", rec),
                    _ => infer_let_type_entity(value, entity, ctx, scratch),
                };
                out.entry(name.clone()).or_insert(let_ty);
            }
        }
    }
}

/// For one transform `in route(p0, p1, ...)`, emit Solidity local aliases
/// that bind each non-wildcard pattern name to the corresponding route
/// parameter. Wildcards (`_`) and patterns whose name already matches the
/// route parameter are skipped (no alias needed).
///
/// Tuple / Deref / Option-destructuring patterns are not yet supported
/// here; those would require positional decoding logic that doesn't have
/// a natural Solidity-local equivalent. They're rare in practice — none
/// of the in-tree EVM examples use them — and the right place to extend
/// support is here.
pub(super) fn gen_transform_param_aliases(
    transform: &MemberTransform,
    route: &Route,
    entity: &Entity,
    ctx: &EvmCtx,
) -> String {
    let mut out = String::new();
    for (i, pat) in transform.params.iter().enumerate() {
        let route_param = match route.params.get(i) {
            Some(p) => p,
            None => continue,
        };
        match pat {
            Pattern::Ident(name) if name != &route_param.name => {
                out.push_str(&format!(
                    "        {} {} = {};\n",
                    sol_type_entity(entity, &route_param.ty, false, ctx),
                    name,
                    route_param.name,
                ));
            }
            _ => {}
        }
    }
    out
}

/// Emit the member-transform stage, keeping its two substages apart so a
/// caller can splice pre-commit snapshots between them.
/// Mapping members referenced as `^m` inside `body` (other than `owner`).
fn temporal_mapping_refs(body: &Expr, entity: &Entity, owner: &str) -> StdHashSet<String> {
    let found = RefCell::new(StdHashSet::new());
    let _ = subst_expr_where(body, &|e| {
        if let Expr::TemporalRef(name) = e {
            if name != owner
                && entity
                    .members
                    .iter()
                    .any(|m| m.name == *name && is_mapping_type(&m.ty))
            {
                found.borrow_mut().insert(name.clone());
            }
        }
        None
    });
    found.into_inner()
}

pub(super) fn emit_transforms_sol_split(
    transforms: &[ResolvedTransform],
    entity: &Entity,
    route: &Route,
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> (String, String) {
    // The field-mutation stage is a two-substage pipeline:
    //
    //   1. read pre-state, compute every transform's next-value into locals;
    //   2. commit every next-value to storage.
    //
    // Substage 1 is collected into `out` (in source order). Substage 2 is
    // collected into `pending` and drained at the very end, so every
    // transform — scalar or mapping — observes the same pre-route snapshot
    // of every member regardless of the order in which they appear.
    //
    // Aliased / `let`-bearing transforms get a `{ ... }` scope so locals
    // cannot collide across transforms. Scalar `next_*` temps are declared
    // *outside* that scope and committed via `pending` (EVM-H7).
    let mut out = String::new();
    let mut pending: Vec<(String, String)> = Vec::new();
    let mut declared_next: std::collections::HashSet<String> = std::collections::HashSet::new();

    for rt in transforms {
        // `^m_map[k]` must observe this route's writes to `m_map`. Mapping
        // writes only reference setup temps, so they can be flushed ahead of
        // the reader (temporal ordering places `m_map` first).
        let temporal_maps = temporal_mapping_refs(&rt.transform.body, entity, &rt.member.name);
        if !temporal_maps.is_empty() {
            let mut kept = Vec::new();
            for (member, w) in pending.drain(..) {
                if temporal_maps.contains(&member) {
                    out.push_str(&w);
                } else {
                    kept.push((member, w));
                }
            }
            pending = kept;
        }
        scratch
            .borrow_mut()
            .push_transform_member(&rt.member.name, &rt.member.ty);
        let aliases = gen_transform_param_aliases(rt.transform, route, entity, ctx);
        let needs_scope = !aliases.is_empty() || transform_has_let(&rt.transform.body);

        // G-U3: a transform whose body is exactly `m_xyz.push(arg)`
        // (against a `Vec<T>`-typed member) is a *statement-shaped*
        // transform — there is no `next_<member>` value to compute,
        // we just append in place. Detected here so the generic
        // expression path below doesn't try to coerce it into a
        // `next_m_xyz = m_xyz.push(arg)` assignment, which Solidity
        // would reject (`array.push(...)` returns nothing in 0.6+).
        if is_vec_type(&rt.member.ty) {
            if let Some(stmts) = lower_foreign_vec_push_body(
                &rt.transform.body,
                &rt.member.name,
                entity,
                ctx,
                scope,
                scratch,
            ) {
                let next_var = format!("next_{}", rt.member.name);
                let local_ty = sol_type_entity(entity, &rt.member.ty, true, ctx);
                let first_decl = declared_next.insert(next_var.clone());
                if needs_scope {
                    out.push_str("        {\n");
                    for line in aliases.lines() {
                        out.push_str(&format!("    {}\n", line));
                    }
                }
                for s in &stmts {
                    out.push_str(&format!(
                        "        {}{}\n",
                        if needs_scope { "    " } else { "" },
                        s
                    ));
                }
                if first_decl {
                    out.push_str(&format!(
                        "        {}{} {} = {};\n",
                        if needs_scope { "    " } else { "" },
                        local_ty,
                        next_var,
                        rt.member.name
                    ));
                } else {
                    out.push_str(&format!(
                        "        {}{} = {};\n",
                        if needs_scope { "    " } else { "" },
                        next_var,
                        rt.member.name
                    ));
                }
                if needs_scope {
                    out.push_str("        }\n");
                }
                pending.push((rt.member.name.clone(), format!("        {} = {};\n", rt.member.name, next_var)));
                scratch.borrow_mut().pop_transform_member();
                continue;
            }
            if let Some((stmts, rhs)) = lower_member_push_body(
                &rt.transform.body,
                &rt.member.name,
                entity,
                ctx,
                scope,
                scratch,
            ) {
                if needs_scope {
                    out.push_str("        {\n");
                    for line in aliases.lines() {
                        out.push_str(&format!("    {}\n", line));
                    }
                }
                for s in &stmts {
                    out.push_str(&format!(
                        "        {}{}\n",
                        if needs_scope { "    " } else { "" },
                        s
                    ));
                }
                out.push_str(&format!(
                    "        {}{}.push({});\n",
                    if needs_scope { "    " } else { "" },
                    rt.member.name,
                    rhs
                ));
                if needs_scope {
                    out.push_str("        }\n");
                }
                scratch.borrow_mut().pop_transform_member();
                continue;
            }
        }

        if is_mapping_type(&rt.member.ty) {
            let mut bindings = StdHashMap::new();
            let (setup, writes) = gen_mapping_transform_split(
                &rt.member.name,
                &rt.transform.body,
                entity,
                &mut bindings,
                ctx,
                scope,
                scratch,
            );
            if needs_scope {
                out.push_str("        {\n");
                for line in aliases.lines() {
                    out.push_str(&format!("    {}\n", line));
                }
            }
            for s in &setup {
                out.push_str(&format!(
                    "        {}{}\n",
                    if needs_scope { "    " } else { "" },
                    s
                ));
            }
            if needs_scope {
                // Writes consume locals declared in `setup` above (which
                // live inside the alias scope), so they have to fire
                // before we close the brace. They still see only the
                // pre-state because everything they reference was
                // captured into the temps already.
                for w in &writes {
                    out.push_str(&format!("            {}\n", w));
                }
                out.push_str("        }\n");
            } else {
                for w in &writes {
                    pending.push((rt.member.name.clone(), format!("        {}\n", w)));
                }
            }
            scratch.borrow_mut().pop_transform_member();
            continue;
        }

        // Scalar member transform.
        let transform_expr = try_wrap_record_member_field_transform(
            &rt.member.name,
            &rt.member.ty,
            &rt.transform.body,
            entity,
            ctx,
        )
        .unwrap_or_else(|| rt.transform.body.clone());
        if matches!(&transform_expr, Expr::ArrayLit(items) if items.is_empty())
            && is_vec_type(&rt.member.ty)
            && !vec_empty_transform_needs_write(entity, &rt.member.ty, ctx)
        {
            // `Vec<record|enum>` storage members default to empty; solc
            // cannot copy `T[] memory` into storage for composite `T`.
            scratch.borrow_mut().pop_transform_member();
            continue;
        }
        let (stmts, rhs) =
            if matches!(&transform_expr, Expr::ArrayLit(items) if items.is_empty())
                && is_vec_type(&rt.member.ty)
            {
                // `array()` on `Vec<T>` must lower to `new T[](0)`, not the
                // untyped `new uint256[](0)` that bare `ArrayLit` emits.
                (vec![], solidity_default_for_member_ty(entity, &rt.member.ty, ctx))
            } else if let Expr::For(pat, iter, body) = &transform_expr {
                if is_scalar_numeric_type(&rt.member.ty) {
                    gen_for_scalar_reduce(
                        pat,
                        iter,
                        body,
                        entity,
                        ctx,
                        scope,
                        scratch,
                        Some(&rt.member.ty),
                    )
                } else {
                    gen_expr_hoisted(&transform_expr, entity, ctx, scope, scratch)
                }
            } else if matches!(&rt.member.ty, Type::Generic(g, _) if g == "Option") {
                let scope = scope.with_expected_ty(Some(&rt.member.ty));
                gen_expr_hoisted(&transform_expr, entity, ctx, &scope, scratch)
            } else {
                gen_expr_hoisted(&transform_expr, entity, ctx, scope, scratch)
            };
        let is_vec_u8 = matches!(&rt.member.ty, Type::Generic(g, ps)
            if g == "Vec" && matches!(ps.as_slice(), [Type::Simple(e)] if e == "u8"));
        let is_std_sha256 = matches!(&transform_expr, Expr::NamespacedCall { namespace, name, .. }
            if namespace == "std::crypto" && name == "sha256");
        let rhs = if is_vec_u8 && is_std_sha256 {
            format!("_cam_bytes_to_u8s({rhs})")
        } else {
            rhs
        };
        let next_var = format!("next_{}", rt.member.name);
        // Phase EVM-15 H2 (Cluster A): use `in_param=true` so
        // reference types (string / bytes / struct / array)
        // carry the `memory` data-location annotation that
        // Solidity requires on locals. Substitute `Expr::None` /
        // `Expr::EmptyCollection` sentinels (which `gen_expr`
        // always lowers to the literal `"0"`) with a
        // type-appropriate default — `0` is invalid for
        // string/bytes/struct/array locals.
        let local_ty = sol_type_entity(entity, &rt.member.ty, true, ctx);
        let body_ty = infer_let_type_entity(&transform_expr, entity, ctx, scratch);
        let init = if rhs == "0" {
            solidity_default_for_member_ty(entity, &rt.member.ty, ctx)
        } else if body_ty != local_ty
            && matches!(&rt.member.ty, Type::Simple(n) if is_entity_record(entity, n) || ctx.is_program_record(n))
        {
            // e.g. `m_bucket.ids.fold(...)` over a record member — the
            // fold runs for iteration coverage but the stored member
            // value stays unchanged when the body's type ≠ the member.
            rt.member.name.clone()
        } else {
            maybe_narrow_cast(&local_ty, &rhs, &transform_expr, entity, ctx, scratch)
        };
        let assign = format!("        {} = {};\n", rt.member.name, next_var);
        let first_decl = declared_next.insert(next_var.clone());

        if needs_scope {
            // EVM-H7: declare `next_*` outside the let/alias scope and
            // defer storage writes to `pending` so later transforms still
            // read the pre-route snapshot.
            // T-EVM-LCC-002: repeat transforms on the same member only
            // declare `next_*` once; subsequent ones assign.
            if first_decl {
                out.push_str(&format!("        {} {};\n", local_ty, next_var));
            }
            out.push_str("        {\n");
            for line in aliases.lines() {
                out.push_str(&format!("    {}\n", line));
            }
            for s in &stmts {
                out.push_str(&format!("            {}\n", s));
            }
            out.push_str(&format!("            {} = {};\n", next_var, init));
            out.push_str("        }\n");
        } else {
            for s in &stmts {
                out.push_str(&format!("        {}\n", s));
            }
            if first_decl {
                out.push_str(&format!("        {} {} = {};\n", local_ty, next_var, init));
            } else {
                out.push_str(&format!("        {} = {};\n", next_var, init));
            }
        }
        pending.push((rt.member.name.clone(), assign));
        scratch.borrow_mut().pop_transform_member();
    }

    let mut commit = String::new();
    if !pending.is_empty() {
        commit.push('\n');
        for (_, assign) in pending {
            commit.push_str(&assign);
        }
    }
    (out, commit)
}

// ---------------------------------------------------------------------------
// Route implementation
// ---------------------------------------------------------------------------

/// True if the route should be emitted as a regular Solidity function.
///
/// A route named `constructor` is treated as an init route on EVM even if it
/// was not declared with the explicit `init` keyword: Solidity reserves
/// `constructor` and emitting `function constructor(...)` would produce
/// invalid syntax. The body of such a route is folded into the synthesized
/// Solidity constructor (or `initialize()` in deterministic mode) by
/// `find_init_route` / `gen_constructor_impl`.
pub(super) fn is_emittable_route(route: &Route) -> bool {
    !route.is_init && route.name != "constructor" && route.recover_tag.is_none()
}

fn push_route_mutability_modifiers(modifiers: &mut Vec<&str>, route: &Route, entity: &Entity) {
    if route_has_mapping_param(route) {
        modifiers.push("view");
    } else if route.is_pure {
        modifiers.push("pure");
    } else if route.is_view || route_infer_evm_view(entity, route) {
        modifiers.push("view");
    }
}

/// Phase EVM-P0-E: lower `receive() => [...]` / `fallback() => [...]`
/// routes to Solidity's special functions. `receive` is always
/// `payable` (it only fires on plain ETH transfers); `fallback` is
/// `payable` only when its body actually reads `msg::value` so the
/// contract continues to reject ETH on accidental calls into
/// non-receiving routes.
fn route_emit_mode(route: &Route, use_det_action: bool) -> RouteEmitMode {
    let (var_call_assignments, reindent_actions) = match &route.body {
        RouteBody::Phased(_) => (true, true),
        RouteBody::Mixed(_, _) => (false, true),
        RouteBody::Unphased(_) => (false, false),
    };
    RouteEmitMode {
        use_det_action,
        var_call_assignments,
        reindent_actions,
    }
}

fn gen_receive_or_fallback(
    entity: &Entity,
    route: &Route,
    program: &Program,
    graphs: &ProgramGraphs,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let is_receive = route.name == "receive";
    let payable = is_receive || route_uses_msg_value(route, entity);
    let modifiers = if payable { " payable" } else { "" };

    let mut out = format!("    {}() external{} {{\n", route.name, modifiers,);

    out.push_str(&gen_where_checks(
        &route.where_clauses,
        ctx,
        entity,
        scratch,
    ));

    let nested_hoist =
        collect_route_body_hoist_locals(&route.body, entity, route, program, ctx, scratch);
    out.push_str(&begin_hoisted_locals(nested_hoist.clone(), scratch));

    let route_ir = ir::lower_route(program, entity, route, graphs);
    let mode = route_emit_mode(route, false);
    emit_route_body_from_ir(
        &mut out,
        &route_ir,
        entity,
        route,
        program,
        ctx,
        scratch,
        &nested_hoist,
        &mode,
    );

    end_hoisted_locals(&nested_hoist, scratch);
    out.push_str("    }\n\n");
    out
}

pub(super) fn gen_route_impl(
    entity: &Entity,
    route: &Route,
    program: &Program,
    graphs: &ProgramGraphs,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let _scope = EmitScope::for_entity(entity);
    if !is_emittable_route(route) {
        return String::new();
    }

    // Phase EVM-P0-E: `receive` / `fallback` routes lower to Solidity's
    // special functions of the same name. They take no params, no
    // returns, and use a fixed signature.
    if route.name == "receive" || route.name == "fallback" {
        return gen_receive_or_fallback(entity, route, program, graphs, ctx, scratch);
    }

    let ret = match &route.return_type {
        Some(ty) => format!(" returns ({})", sol_return_type(entity, ty, ctx)),
        None => String::new(),
    };

    let mut modifiers: Vec<&str> = Vec::new();
    push_route_mutability_modifiers(&mut modifiers, route, entity);
    // Phase EVM-15 H5 (Cluster D): a route that reads `msg::value`
    // anywhere in its body (incl. transforms hit by this route)
    // must be `payable` — Solidity rejects `msg.value` in non-
    // payable functions. Detection is purely syntactic: any
    // `Expr::MsgField("value")` in the route's reachable AST.
    if route_uses_msg_value(route, entity) {
        modifiers.push("payable");
    }
    let modifiers_str = if modifiers.is_empty() {
        String::new()
    } else {
        format!(" {}", modifiers.join(" "))
    };

    let (visibility, fn_name_prefix) = route_function_visibility(route, entity);

    scratch.borrow_mut().clear_route_unreachable();
    let mut out = format!(
        "    function {}{}({}) {}{}{} {{\n",
        fn_name_prefix,
        sol_sanitize_ident(&route.name),
        gen_params_entity(entity, &route.params, ctx),
        visibility,
        modifiers_str,
        ret
    );

    // Phase EVM-P0-B follow-up: register route parameter types on the
    // shared let-binding stack so `actual_sol_type` (used by the
    // narrow-cast suppressor) can resolve a bare parameter Ident
    // back to its declared narrow type instead of falling through to
    // `uint256`.
    push_route_param_bindings(route, entity, ctx, scratch);

    out.push_str(&gen_from_checks(route, ctx, entity, scratch));
    out.push_str(&gen_where_checks(
        &route.where_clauses,
        ctx,
        entity,
        scratch,
    ));
    let nested_hoist =
        collect_route_body_hoist_locals(&route.body, entity, route, program, ctx, scratch);
    out.push_str(&begin_hoisted_locals(nested_hoist.clone(), scratch));

    let route_ir = ir::lower_route(program, entity, route, graphs);
    let mode = route_emit_mode(route, false);
    emit_route_body_from_ir(
        &mut out,
        &route_ir,
        entity,
        route,
        program,
        ctx,
        scratch,
        &nested_hoist,
        &mode,
    );
    end_hoisted_locals(&nested_hoist, scratch);

    pop_route_param_bindings(route, scratch);

    out.push_str("    }\n");
    out
}

pub(super) fn gen_constructor_impl(
    entity: &Entity,
    program: &Program,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let scope = EmitScope::for_entity(entity);
    // Find init route if any: explicit `init` keyword OR a route literally
    // named `constructor` (Solidity reserves that name; the route's body
    // becomes the synthesized Solidity constructor).
    let init_route = entity
        .routes
        .iter()
        .find(|r| r.is_init || r.name == "constructor");

    // Identity members → immutable, passed as constructor params
    let identity_params: Vec<String> = entity
        .members
        .iter()
        .filter(|m| m.is_identity)
        .map(|m| format!("{} {}_", sol_type_entity(entity, &m.ty, true, ctx), m.name))
        .collect();

    // Init route params
    let init_params: Vec<String> = init_route
        .map(|r| {
            r.params
                .iter()
                .map(|p| {
                    format!(
                        "{} {}",
                        sol_type_entity(entity, &p.ty, true, ctx),
                        sol_sanitize_ident(&p.name)
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    let all_params: Vec<&str> = identity_params
        .iter()
        .map(|s| s.as_str())
        .chain(init_params.iter().map(|s| s.as_str()))
        .collect();

    // Phase EVM-15 H4 (Cluster C): by default emit `payable` so deploy
    // sites can pass `value:`. Projects may opt out via
    // `evm.allow_constructor_payable: false` (CH WP-C).
    let payable = if ctx.allow_constructor_payable { " payable" } else { "" };
    let mut out = format!(
        "    constructor({}){} {{\n",
        all_params.join(", "),
        payable
    );

    // Assign identity members
    for member in entity.members.iter().filter(|m| m.is_identity) {
        out.push_str(&format!("        {} = {}_;\n", member.name, member.name));
    }

    // Default-initialize non-identity, non-mapping members (for routes with no init)
    if init_route.is_none() {
        for member in &entity.members {
            if member.is_identity {
                continue;
            }
            if is_mapping_type(&member.ty) {
                continue;
            }
            match &member.default_value {
                Some(expr) => {
                    let value = if matches!(
                        expr,
                        crate::ast::Expr::None | crate::ast::Expr::EmptyCollection
                    ) {
                        default_value_entity(entity, &member.ty, ctx)
                    } else {
                        gen_expr(expr, ctx, &scope, scratch)
                            .unwrap_or_else(|| default_value_entity(entity, &member.ty, ctx))
                    };
                    if !value.is_empty() {
                        out.push_str(&format!("        {} = {};\n", member.name, value));
                    }
                }
                None => {
                    let value = default_value_entity(entity, &member.ty, ctx);
                    if !value.is_empty() {
                        out.push_str(&format!("        {} = {};\n", member.name, value));
                    }
                }
            }
        }
    } else {
        // Apply member transforms from init route
        let route = init_route.unwrap();
        // Register init-route + identity-member parameter types for
        // narrow-cast suppression (mirrors `gen_route_impl`).
        for member in entity.members.iter().filter(|m| m.is_identity) {
            let pty = sol_type_entity(entity, &member.ty, true, ctx);
            scratch
                .borrow_mut()
                .push_let_binding(&format!("{}_", member.name), &pty);
        }
        for p in &route.params {
            let pty = sol_type_entity(entity, &p.ty, true, ctx);
            scratch.borrow_mut().push_let_binding(&p.name, &pty);
        }
        // where checks for init route
        out.push_str(&gen_where_checks(
            &route.where_clauses,
            ctx,
            entity,
            scratch,
        ));
        // member updates from init route transforms
        let (updates, unphased_actions) = init_updates(entity, route, ctx, scratch);
        out.push_str(&updates);
        // actions from init route body
        match &route.body {
            RouteBody::Unphased(_) => {
                for action in &unphased_actions {
                    out.push_str(&gen_action(action, entity, route, program, ctx, scratch));
                }
            }
            RouteBody::Phased(phases) => {
                for phase in phases {
                    out.push_str(&format!("        // Phase: {}\n        {{\n", phase.name));
                    let (updates, phase_actions) = gen_updates_with_snapshots(
                        entity,
                        route,
                        Some(&phase.name),
                        &phase.actions,
                        ctx,
                        scratch,
                    );
                    for line in updates.lines() {
                        out.push_str(&format!("    {}\n", line));
                    }
                    for action in &phase_actions {
                        let action_str = gen_action(action, entity, route, program, ctx, scratch);
                        for line in action_str.lines() {
                            out.push_str(&format!("    {}\n", line));
                        }
                    }
                    out.push_str("        }\n");
                }
            }
            RouteBody::Mixed(phases, actions) => {
                for phase in phases {
                    out.push_str(&format!("        // Phase: {}\n        {{\n", phase.name));
                    let (updates, phase_actions) = gen_updates_with_snapshots(
                        entity,
                        route,
                        Some(&phase.name),
                        &phase.actions,
                        ctx,
                        scratch,
                    );
                    for line in updates.lines() {
                        out.push_str(&format!("    {}\n", line));
                    }
                    for action in &phase_actions {
                        let action_str = gen_action(action, entity, route, program, ctx, scratch);
                        for line in action_str.lines() {
                            out.push_str(&format!("    {}\n", line));
                        }
                    }
                    out.push_str("        }\n");
                }
                for action in actions {
                    out.push_str(&gen_action(action, entity, route, program, ctx, scratch));
                }
            }
        }
        // For any non-identity, non-mapping members without an init-route transform, initialize defaults
        for member in &entity.members {
            if member.is_identity {
                continue;
            }
            if is_mapping_type(&member.ty) {
                continue;
            }
            let has_transform = member.transforms.iter().any(|t| t.route_name == route.name);
            if !has_transform {
                match &member.default_value {
                    Some(expr) => {
                        let value = if matches!(
                            expr,
                            crate::ast::Expr::None | crate::ast::Expr::EmptyCollection
                        ) {
                            default_value_entity(entity, &member.ty, ctx)
                        } else {
                            gen_expr(expr, ctx, &scope, scratch)
                                .unwrap_or_else(|| default_value_entity(entity, &member.ty, ctx))
                        };
                        if !value.is_empty() {
                            out.push_str(&format!("        {} = {};\n", member.name, value));
                        }
                    }
                    None => {
                        let value = default_value_entity(entity, &member.ty, ctx);
                        if !value.is_empty() {
                            out.push_str(&format!("        {} = {};\n", member.name, value));
                        }
                    }
                }
            }
        }
        // Pop init-route + identity-member registrations.
        for p in &route.params {
            scratch.borrow_mut().pop_let_binding(&p.name);
        }
        for member in entity.members.iter().filter(|m| m.is_identity) {
            scratch
                .borrow_mut()
                .pop_let_binding(&format!("{}_", member.name));
        }
    }

    out.push_str("    }\n");
    out
}

// ---------------------------------------------------------------------------
// Deterministic addresses: helpers
// ---------------------------------------------------------------------------

/// Find the "init" route: either one marked `is_init`, or the one named "constructor".
pub(super) fn find_init_route(entity: &Entity) -> Option<&Route> {
    entity
        .routes
        .iter()
        .find(|r| r.is_init)
        .or_else(|| entity.routes.iter().find(|r| r.name == "constructor"))
}

/// Whether the entity's init route has parameters beyond identity members.
pub(crate) fn has_non_identity_init_params(entity: &Entity) -> bool {
    find_init_route(entity)
        .map(|r| !r.params.is_empty())
        .unwrap_or(false)
}

/// Constructor for deterministic mode: `constructor(address factory_, identity_params...) { ... }`
/// Only sets _factory and identity immutables. Non-identity init-route logic goes to initialize().
pub(super) fn gen_constructor_deterministic(
    entity: &Entity,
    program: &Program,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let scope = EmitScope::for_entity(entity);
    let init_route = find_init_route(entity);

    let identity_params: Vec<String> = entity
        .members
        .iter()
        .filter(|m| m.is_identity)
        .map(|m| format!("{} {}_", sol_type_entity(entity, &m.ty, true, ctx), m.name))
        .collect();

    let mut all_params = vec!["address factory_".to_string()];
    all_params.extend(identity_params);

    // Phase EVM-15 H4 (Cluster C): by default emit `payable` so deploy
    // sites can pass `value:`. Projects may opt out via
    // `evm.allow_constructor_payable: false` (CH WP-C).
    let payable = if ctx.allow_constructor_payable { " payable" } else { "" };
    let mut out = format!(
        "    constructor({}){} {{\n",
        all_params.join(", "),
        payable
    );
    out.push_str("        require(factory_ != address(0), \"zero factory\");\n");
    out.push_str("        _factory = factory_;\n");

    for member in entity.members.iter().filter(|m| m.is_identity) {
        out.push_str(&format!("        {} = {}_;\n", member.name, member.name));
    }

    // If there's no init route or init route has no params, initialize non-identity
    // members with defaults directly in the constructor (no initialize() needed).
    if init_route.is_none() || !has_non_identity_init_params(entity) {
        if let Some(route) = init_route {
            out.push_str(&gen_where_checks(
                &route.where_clauses,
                ctx,
                entity,
                scratch,
            ));
            let (updates, unphased_actions) = init_updates(entity, route, ctx, scratch);
            out.push_str(&updates);
            match &route.body {
                RouteBody::Unphased(_) => {
                    for action in &unphased_actions {
                        out.push_str(&gen_action_det(
                            action, entity, route, program, ctx, scratch,
                        ));
                    }
                }
                _ => {}
            }
        }
        for member in &entity.members {
            if member.is_identity || is_mapping_type(&member.ty) {
                continue;
            }
            let has_transform = init_route
                .map(|r| member.transforms.iter().any(|t| t.route_name == r.name))
                .unwrap_or(false);
            if !has_transform {
                match &member.default_value {
                    Some(expr) => {
                        let value = if matches!(
                            expr,
                            crate::ast::Expr::None | crate::ast::Expr::EmptyCollection
                        ) {
                            default_value_entity(entity, &member.ty, ctx)
                        } else {
                            gen_expr(expr, ctx, &scope, scratch)
                                .unwrap_or_else(|| default_value_entity(entity, &member.ty, ctx))
                        };
                        if !value.is_empty() {
                            out.push_str(&format!("        {} = {};\n", member.name, value));
                        }
                    }
                    None => {
                        let value = default_value_entity(entity, &member.ty, ctx);
                        if !value.is_empty() {
                            out.push_str(&format!("        {} = {};\n", member.name, value));
                        }
                    }
                }
            }
        }
    }

    out.push_str("    }\n");
    out
}

/// Generate initialize() for deterministic mode. Only emitted when init route has non-identity params.
pub(super) fn gen_initialize_fn(
    entity: &Entity,
    program: &Program,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let scope = EmitScope::for_entity(entity);
    let init_route = match find_init_route(entity) {
        Some(r) => r,
        None => return String::new(),
    };

    let init_params: Vec<String> = init_route
        .params
        .iter()
        .map(|p| {
            format!(
                "{} {}",
                sol_type_entity(entity, &p.ty, true, ctx),
                sol_sanitize_ident(&p.name)
            )
        })
        .collect();

    let mut out = format!(
        "    function initialize({}) external {{\n",
        init_params.join(", ")
    );
    out.push_str("        require(msg.sender == _factory, \"only factory\");\n");
    out.push_str("        require(!_initialized, \"already initialized\");\n");
    out.push_str("        _initialized = true;\n");

    // Register init-route + identity-member parameter types, exactly as
    // `gen_constructor_impl` does. Without this the deterministic-address
    // build infers every parameter as `uint256`, so a `String` argument
    // assigned to a `String` member is coerced through `_cam_itoa` and the
    // output does not compile — a divergence between the two builds of the
    // same source.
    for member in entity.members.iter().filter(|m| m.is_identity) {
        let pty = sol_type_entity(entity, &member.ty, true, ctx);
        scratch
            .borrow_mut()
            .push_let_binding(&format!("{}_", member.name), &pty);
    }
    for p in &init_route.params {
        let pty = sol_type_entity(entity, &p.ty, true, ctx);
        scratch.borrow_mut().push_let_binding(&p.name, &pty);
    }

    out.push_str(&gen_where_checks(
        &init_route.where_clauses,
        ctx,
        entity,
        scratch,
    ));
    let (updates, unphased_actions) = init_updates(entity, init_route, ctx, scratch);
    out.push_str(&updates);

    match &init_route.body {
        RouteBody::Unphased(_) => {
            for action in &unphased_actions {
                out.push_str(&gen_action_det(
                    action, entity, init_route, program, ctx, scratch,
                ));
            }
        }
        RouteBody::Phased(phases) => {
            for phase in phases {
                out.push_str(&format!("        // Phase: {}\n        {{\n", phase.name));
                let (updates, phase_actions) = gen_updates_with_snapshots(
                    entity,
                    init_route,
                    Some(&phase.name),
                    &phase.actions,
                    ctx,
                    scratch,
                );
                for line in updates.lines() {
                    out.push_str(&format!("    {}\n", line));
                }
                for action in &phase_actions {
                    let action_str =
                        gen_action_det(action, entity, init_route, program, ctx, scratch);
                    for line in action_str.lines() {
                        out.push_str(&format!("    {}\n", line));
                    }
                }
                out.push_str("        }\n");
            }
        }
        RouteBody::Mixed(phases, actions) => {
            for phase in phases {
                out.push_str(&format!("        // Phase: {}\n        {{\n", phase.name));
                let (updates, phase_actions) = gen_updates_with_snapshots(
                    entity,
                    init_route,
                    Some(&phase.name),
                    &phase.actions,
                    ctx,
                    scratch,
                );
                for line in updates.lines() {
                    out.push_str(&format!("    {}\n", line));
                }
                for action in &phase_actions {
                    let action_str =
                        gen_action_det(action, entity, init_route, program, ctx, scratch);
                    for line in action_str.lines() {
                        out.push_str(&format!("    {}\n", line));
                    }
                }
                out.push_str("        }\n");
            }
            for action in actions {
                out.push_str(&gen_action_det(
                    action, entity, init_route, program, ctx, scratch,
                ));
            }
        }
    }

    // Default-initialize remaining non-identity, non-mapping members without a transform
    for member in &entity.members {
        if member.is_identity || is_mapping_type(&member.ty) {
            continue;
        }
        let has_transform = member
            .transforms
            .iter()
            .any(|t| t.route_name == init_route.name);
        if !has_transform {
            match &member.default_value {
                Some(expr) => {
                    let value = if matches!(
                        expr,
                        crate::ast::Expr::None | crate::ast::Expr::EmptyCollection
                    ) {
                        default_value_entity(entity, &member.ty, ctx)
                    } else {
                        gen_expr(expr, ctx, &scope, scratch)
                            .unwrap_or_else(|| default_value_entity(entity, &member.ty, ctx))
                    };
                    let value = if matches!(expr, crate::ast::Expr::None)
                        && matches!(&member.ty, crate::ast::Type::Generic(n, _) if n == "Option")
                    {
                        default_value_entity(entity, &member.ty, ctx)
                    } else {
                        value
                    };
                    if !value.is_empty() {
                        out.push_str(&format!("        {} = {};\n", member.name, value));
                    }
                }
                None => {
                    let value = default_value_entity(entity, &member.ty, ctx);
                    if !value.is_empty() {
                        out.push_str(&format!("        {} = {};\n", member.name, value));
                    }
                }
            }
        }
    }

    for p in &init_route.params {
        scratch.borrow_mut().pop_let_binding(&p.name);
    }
    for member in entity.members.iter().filter(|m| m.is_identity) {
        scratch
            .borrow_mut()
            .pop_let_binding(&format!("{}_", member.name));
    }

    out.push_str("    }\n");
    out
}

/// When a `from Target(id)` argument names the target entity's identity
/// member but that identifier is not in scope on the caller, substitute
/// the caller's identity member at the same index (e.g. Vault's `slot`
/// for Admin's `id`).
fn resolve_from_identity_arg(
    arg: &Expr,
    index: usize,
    target_entity: &Entity,
    current_entity: &Entity,
) -> Expr {
    if let Expr::Ident(name) = arg {
        let target_ids: Vec<_> = target_entity
            .members
            .iter()
            .filter(|m| m.is_identity)
            .collect();
        if index < target_ids.len() && target_ids[index].name == *name {
            let current_ids: Vec<_> = current_entity
                .members
                .iter()
                .filter(|m| m.is_identity)
                .collect();
            if index < current_ids.len() {
                return Expr::Ident(current_ids[index].name.clone());
            }
        }
    }
    arg.clone()
}

/// Extended gen_from_checks that supports deterministic address computation.
pub(super) fn gen_from_checks_det(
    route: &Route,
    program: &Program,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let scope = EmitScope::for_entity(entity);
    use crate::ast::FromClauseKind;

    if route.from_clauses.is_empty() {
        return String::new();
    }
    let defined: Vec<&str> = program.entities.iter().map(|e| e.name.as_str()).collect();
    let mut checks: Vec<String> = Vec::new();
    for clause in &route.from_clauses {
        if let FromClauseKind::Member = clause.kind {
            // Address-typed member of the current entity: lower as plain
            // SLOAD comparison. Not factory-aware because the member
            // is just an address, not a derivable identity.
            checks.push(format!("(msg.sender == {})", clause.entity_name));
            continue;
        }
        if defined.contains(&clause.entity_name.as_str()) {
            let target_entity = program
                .entities
                .iter()
                .find(|e| e.name == clause.entity_name)
                .unwrap();
            let identity_count = target_entity
                .members
                .iter()
                .filter(|m| m.is_identity)
                .count();
            if clause.args.len() == identity_count {
                let identity_exprs: Vec<String> = clause
                    .args
                    .iter()
                    .enumerate()
                    .filter_map(|(i, e)| {
                        let resolved = resolve_from_identity_arg(e, i, target_entity, entity);
                        gen_expr(&resolved, ctx, &scope, scratch)
                    })
                    .collect();
                if identity_exprs.len() == identity_count {
                    let addr = gen_create2_address_expr(&clause.entity_name, &identity_exprs);
                    checks.push(format!("(msg.sender == {})", addr));
                    continue;
                }
            }
        }
        // Fallback: explicit address check (single arg)
        if clause.args.len() == 1 {
            if let Some(addr_expr) = gen_expr(&clause.args[0], ctx, &scope, scratch) {
                let addr = crate::codegen::solidity::core::types::coerce_to_address_sol(
                    &clause.args[0],
                    &addr_expr,
                    entity,
                    ctx,
                    scratch,
                );
                checks.push(format!("(msg.sender == {})", addr));
                continue;
            }
        }
        // Phase EVM-6 M2: V33 should reject any from-clause whose
        // arity doesn't match the target entity's identity count
        // (deterministic mode) before codegen runs. Defence in depth.
        debug_assert!(
            false,
            "EVM-6 M2: from-clause `from {}({} args)` should have been rejected by validator V33 in deterministic mode",
            clause.entity_name,
            clause.args.len(),
        );
        checks.push("false".to_string());
    }
    // Phase EVM-P0-D: prefer a custom-error revert when every from-clause
    // shares the same `: throw CustomErr(args)` annotation.
    let names: Vec<&String> = route
        .from_clauses
        .iter()
        .filter_map(|c| c.error_name.as_ref())
        .collect();
    if names.len() == route.from_clauses.len() && names.windows(2).all(|w| w[0] == w[1]) {
        if let Some(first) = route.from_clauses.first() {
            if let Some(err_name) = &first.error_name {
                let args: Vec<String> = first
                    .error_args
                    .iter()
                    .map(|a| gen_expr(a, ctx, &scope, scratch).unwrap_or_else(|| "0".to_string()))
                    .collect();
                return format!(
                    "        if (!({cond})) revert {name}({a});\n",
                    cond = checks.join(" || "),
                    name = err_name,
                    a = args.join(", "),
                );
            }
        }
    }
    let codes: Vec<u32> = route
        .from_clauses
        .iter()
        .filter_map(|c| c.error_code)
        .collect();
    let msg = if codes.len() == route.from_clauses.len() && codes.windows(2).all(|w| w[0] == w[1]) {
        match codes.first() {
            Some(c) => format!("throw({})", c),
            None => "from clause failed".to_string(),
        }
    } else {
        "from clause failed".to_string()
    };
    format!("        require({}, \"{}\");\n", checks.join(" || "), msg,)
}

/// gen_route_impl with deterministic flag support.
pub(super) fn gen_route_impl_ext(
    entity: &Entity,
    route: &Route,
    program: &Program,
    graphs: &ProgramGraphs,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    if !ctx.is_deterministic_mode() {
        return gen_route_impl(entity, route, program, graphs, ctx, scratch);
    }
    if !is_emittable_route(route) {
        return String::new();
    }

    if route.name == "receive" || route.name == "fallback" {
        return gen_receive_or_fallback(entity, route, program, graphs, ctx, scratch);
    }

    let ret = match &route.return_type {
        Some(ty) => format!(" returns ({})", sol_return_type(entity, ty, ctx)),
        None => String::new(),
    };

    let mut modifiers: Vec<&str> = Vec::new();
    push_route_mutability_modifiers(&mut modifiers, route, entity);
    if route_uses_msg_value(route, entity) {
        modifiers.push("payable");
    }
    let modifiers_str = if modifiers.is_empty() {
        String::new()
    } else {
        format!(" {}", modifiers.join(" "))
    };

    let (visibility, fn_name_prefix) = route_function_visibility(route, entity);

    scratch.borrow_mut().clear_route_unreachable();
    let mut out = format!(
        "    function {}{}({}) {}{}{} {{\n",
        fn_name_prefix,
        sol_sanitize_ident(&route.name),
        gen_params_entity(entity, &route.params, ctx),
        visibility,
        modifiers_str,
        ret
    );

    push_route_param_bindings(route, entity, ctx, scratch);

    out.push_str(&gen_from_checks_det(route, program, entity, ctx, scratch));
    out.push_str(&gen_where_checks(
        &route.where_clauses,
        ctx,
        entity,
        scratch,
    ));
    let nested_hoist =
        collect_route_body_hoist_locals(&route.body, entity, route, program, ctx, scratch);
    out.push_str(&begin_hoisted_locals(nested_hoist.clone(), scratch));

    let route_ir = ir::lower_route(program, entity, route, graphs);
    let mode = route_emit_mode(route, true);
    emit_route_body_from_ir(
        &mut out,
        &route_ir,
        entity,
        route,
        program,
        ctx,
        scratch,
        &nested_hoist,
        &mode,
    );
    end_hoisted_locals(&nested_hoist, scratch);

    pop_route_param_bindings(route, scratch);

    out.push_str("    }\n");
    out
}

/// Deterministic-aware gen_action: delegates deploy to factory.
pub(super) fn gen_action_det(
    action: &RouteAction,
    entity: &Entity,
    route: &Route,
    program: &Program,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    match action {
        RouteAction::Deploy {
            entity: target_entity,
            send_options,
            constructor_args,
        } => gen_deploy_via_factory(
            target_entity,
            send_options.as_ref(),
            constructor_args,
            entity,
            program,
            ctx,
            scratch,
        ),
        _ => gen_action(action, entity, route, program, ctx, scratch),
    }
}
