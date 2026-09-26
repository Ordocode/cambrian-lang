// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM invariant / property predicate lowering (SD-01).
//!
//! Compute-heavy `check` expressions (fold over HashMap, hoisted loops) must
//! lower into **entity-side** `view` helpers so generated code can read
//! `m_*_keys` storage. Lean already uses full `gen_expr`; this module
//! classifies each check for Foundry / revm / cargo-fuzz.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};

use crate::ast::{Entity, Expr, InvariantDecl, Program};

use super::evm_test_codegen::sanitize_test_name;
use super::solidity::core::ctx::EvmCtx;
use super::solidity::core::expr::{gen_expr, gen_expr_hoisted};
use super::solidity::core::scratch::EmitScratch;
use super::solidity::core::ctx::EmitScope;

/// How an invariant `check` should lower on the EVM/Solidity backends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CheckKind {
    /// Existing harness expression (`gen_expr_test` / `rust_bool_expr` /
    /// `lower_check_expr_multi`).
    Inline,
    /// Entity-side `public view returns (bool)` helper.
    Helper {
        entity: String,
        instance: Option<String>,
        fn_name: String,
    },
    /// Cannot lower — validation I17.
    Unsupported,
}

/// One synthesized entity helper for an invariant `check` clause.
#[derive(Debug, Clone)]
pub(crate) struct InvariantCheckHelper {
    pub fn_name: String,
    pub body_stmts: Vec<String>,
    pub return_expr: String,
}

/// Classify a single invariant check (shared by validate I17 and codegen).
pub(crate) fn classify_invariant_check(
    program: &Program,
    inv: &InvariantDecl,
    check_idx: usize,
    ctx: &EvmCtx,
) -> CheckKind {
    classify_check_plan(program, inv, check_idx, ctx).0
}

fn helper_fn_name(inv_name: &str, check_idx: usize) -> String {
    format!("_cam_inv_{}_{}", sanitize_test_name(inv_name), check_idx)
}

/// `(kind, helper body)` — helper body is `Some` iff `kind` is `Helper`.
fn classify_check_plan(
    program: &Program,
    inv: &InvariantDecl,
    check_idx: usize,
    ctx: &EvmCtx,
) -> (CheckKind, Option<InvariantCheckHelper>) {
    let Some(check) = inv.checks.get(check_idx) else {
        return (CheckKind::Unsupported, None);
    };
    let fn_name = helper_fn_name(&inv.name, check_idx);

    if inv.is_single_entity() {
        let Some(entity) = program.entities.iter().find(|e| e.name == inv.entity_name()) else {
            return (CheckKind::Unsupported, None);
        };
        return classify_on_entity(entity, None, check, &fn_name, ctx, /*prefer_inline_if_gen_expr*/ true);
    }

    let inst_names: HashSet<&str> = inv.instances.iter().map(|i| i.name.as_str()).collect();
    let mut refs = BTreeSet::new();
    collect_instance_refs(check, &inst_names, &mut refs);

    if refs.len() > 1 {
        if is_simple_multi_inline(check) {
            return (CheckKind::Inline, None);
        }
        return (CheckKind::Unsupported, None);
    }

    let (instance, entity_name) = if let Some(inst) = refs.iter().next() {
        let Some(decl) = inv.instances.iter().find(|i| i.name == *inst) else {
            return (CheckKind::Unsupported, None);
        };
        (Some(inst.clone()), decl.entity.clone())
    } else {
        let Some(first) = inv.instances.first() else {
            return (CheckKind::Unsupported, None);
        };
        (None, first.entity.clone())
    };

    let Some(entity) = program.entities.iter().find(|e| e.name == entity_name) else {
        return (CheckKind::Unsupported, None);
    };

    let rewritten = match &instance {
        Some(inst) => strip_instance_qualifier(check, inst),
        None => check.clone(),
    };

    if is_simple_multi_inline(check) {
        return (CheckKind::Inline, None);
    }

    // Non-simple multi-entity: even if `gen_expr` succeeds after the strip,
    // the harness cannot read `m_*_keys` — emit a helper on the owner entity.
    classify_on_entity(
        entity,
        instance.or_else(|| inv.instances.first().map(|i| i.name.clone())),
        &rewritten,
        &fn_name,
        ctx,
        /*prefer_inline_if_gen_expr*/ false,
    )
}

fn classify_on_entity(
    entity: &Entity,
    instance: Option<String>,
    expr: &Expr,
    fn_name: &str,
    ctx: &EvmCtx,
    prefer_inline_if_gen_expr: bool,
) -> (CheckKind, Option<InvariantCheckHelper>) {
    let scope = EmitScope::for_entity(entity);
    let scratch = RefCell::new(EmitScratch::new());
    let uses_trace = expr_uses_trace(expr);
    if let Some(inline) = gen_expr(expr, ctx, &scope, &scratch) {
        // `trace::*` counters live on the Foundry/revm *handler*, not the
        // SUT. Never bake `_traceLen` / `_traceCount_*` into an entity
        // `_cam_inv_*` helper (solc 7576 undeclared identifier).
        // HashMap aggregate pure fns need entity-side `_cam_inv_*` helpers
        // so the lowered call can pass `storage` refs to parallel `_keys`
        // arrays — public getters cannot supply those from the test harness.
        let needs_keys_helper = check_expr_needs_entity_keys_helper(expr, entity, ctx);
        if (prefer_inline_if_gen_expr || uses_trace) && !needs_keys_helper {
            return (CheckKind::Inline, None);
        }
        let helper = InvariantCheckHelper {
            fn_name: fn_name.to_string(),
            body_stmts: Vec::new(),
            return_expr: inline,
        };
        return (
            CheckKind::Helper {
                entity: entity.name.clone(),
                instance,
                fn_name: fn_name.to_string(),
            },
            Some(helper),
        );
    }
    if uses_trace {
        return (CheckKind::Unsupported, None);
    }
    let (stmts, ret) = gen_expr_hoisted(expr, entity, ctx, &scope, &scratch);
    if hoist_is_real(&stmts, &ret) {
        let helper = InvariantCheckHelper {
            fn_name: fn_name.to_string(),
            body_stmts: stmts,
            return_expr: ret,
        };
        return (
            CheckKind::Helper {
                entity: entity.name.clone(),
                instance,
                fn_name: fn_name.to_string(),
            },
            Some(helper),
        );
    }
    (CheckKind::Unsupported, None)
}

/// True when `expr` passes an entity HashMap member into a pure fn that
/// lowers with a parallel `_keys` sidecar (e.g. `sum_balances(m_balances)`).
fn check_expr_needs_entity_keys_helper(expr: &Expr, entity: &Entity, ctx: &EvmCtx) -> bool {
    fn walk(expr: &Expr, entity: &Entity, ctx: &EvmCtx) -> bool {
        match expr {
            Expr::FnCall(name, args) => {
                if let Some(key_params) = ctx.lookup_pure_fn_keys_params(name, None) {
                    for (idx, arg) in args.iter().enumerate() {
                        if let Expr::Ident(member) = arg {
                            if entity.members.iter().any(|m| m.name == *member)
                                && key_params.iter().any(|(i, _)| *i == idx)
                            {
                                return true;
                            }
                        }
                    }
                }
                args.iter().any(|a| walk(a, entity, ctx))
            }
            Expr::NamespacedCall { name, args, .. } => {
                if let Some(key_params) = ctx.lookup_pure_fn_keys_params(name, None) {
                    for (idx, arg) in args.iter().enumerate() {
                        if let Expr::Ident(member) = arg {
                            if entity.members.iter().any(|m| m.name == *member)
                                && key_params.iter().any(|(i, _)| *i == idx)
                            {
                                return true;
                            }
                        }
                    }
                }
                args.iter().any(|a| walk(a, entity, ctx))
            }
            Expr::BinOp(l, _, r) => walk(l, entity, ctx) || walk(r, entity, ctx),
            Expr::UnaryOp(_, e) => walk(e, entity, ctx),
            Expr::MethodCall(recv, _, args) => {
                walk(recv, entity, ctx) || args.iter().any(|a| walk(a, entity, ctx))
            }
            Expr::If(c, t, e) => {
                walk(c, entity, ctx)
                    || walk(t, entity, ctx)
                    || e.as_ref().is_some_and(|x| walk(x, entity, ctx))
            }
            Expr::Let(_, v, b) => walk(v, entity, ctx) || walk(b, entity, ctx),
            Expr::Block(items) => items.iter().any(|e| walk(e, entity, ctx)),
            _ => false,
        }
    }
    walk(expr, entity, ctx)
}

/// `trace::length` / `trace::count` / `trace::lastWas` — handler storage.
fn expr_uses_trace(expr: &Expr) -> bool {
    match expr {
        Expr::TraceField(_) | Expr::TraceCall { .. } => true,
        Expr::FieldAccess(inner, _)
        | Expr::UnaryOp(_, inner)
        | Expr::Some(inner)
        | Expr::Cast(inner, _)
        | Expr::Closure(_, inner)
        | Expr::Encode { value: inner, .. } => expr_uses_trace(inner),
        Expr::BinOp(l, _, r) | Expr::Index(l, r) | Expr::Range(l, r) | Expr::Let(_, l, r) => {
            expr_uses_trace(l) || expr_uses_trace(r)
        }
        Expr::MethodCall(recv, _, args) => {
            expr_uses_trace(recv) || args.iter().any(expr_uses_trace)
        }
        Expr::FnCall(_, args)
        | Expr::MacroRef(_, args)
        | Expr::Tuple(args)
        | Expr::ArrayLit(args)
        | Expr::EnumVariantWithData(_, _, args)
        | Expr::Block(args) => args.iter().any(expr_uses_trace),
        Expr::If(c, t, e) => {
            expr_uses_trace(c) || expr_uses_trace(t) || e.as_ref().is_some_and(|x| expr_uses_trace(x))
        }
        Expr::RecordConstruct(_, fields) => fields.iter().any(|(_, e)| expr_uses_trace(e)),
        Expr::RecordUpdate(b, fields) => {
            expr_uses_trace(b) || fields.iter().any(|(_, e)| expr_uses_trace(e))
        }
        Expr::Match(s, arms) => {
            expr_uses_trace(s) || arms.iter().any(|a| expr_uses_trace(&a.body))
        }
        Expr::For(_, it, body) => expr_uses_trace(it) || expr_uses_trace(body),
        Expr::NamespacedCall { args, .. } => args.iter().any(expr_uses_trace),
        Expr::AddressOf {
            args, with_params, ..
        } => args.iter().any(expr_uses_trace) || with_params.iter().any(|(_, e)| expr_uses_trace(e)),
        _ => false,
    }
}

fn hoist_is_real(stmts: &[String], ret: &str) -> bool {
    let sentinel = stmts.iter().any(|s| {
        let t = s.trim();
        t.starts_with("revert(\"EVM:") || t.starts_with("revert(\"EVM")
    });
    if sentinel {
        return false;
    }
    let only_comments = !stmts.is_empty() && stmts.iter().all(|s| s.trim().starts_with("//"));
    if only_comments && (ret == "0" || ret.is_empty()) {
        return false;
    }
    !(stmts.is_empty() && ret == "0")
}

/// Collect instance names that qualify state / route access in `expr`.
fn collect_instance_refs(expr: &Expr, instance_names: &HashSet<&str>, out: &mut BTreeSet<String>) {
    match expr {
        Expr::FieldAccess(inner, _) => {
            if let Expr::Ident(n) = inner.as_ref() {
                if instance_names.contains(n.as_str()) {
                    out.insert(n.clone());
                }
            }
            collect_instance_refs(inner, instance_names, out);
        }
        Expr::MethodCall(recv, _, args) => {
            if let Expr::Ident(n) = recv.as_ref() {
                if instance_names.contains(n.as_str()) {
                    out.insert(n.clone());
                }
            }
            collect_instance_refs(recv, instance_names, out);
            for a in args {
                collect_instance_refs(a, instance_names, out);
            }
        }
        Expr::BinOp(l, _, r) => {
            collect_instance_refs(l, instance_names, out);
            collect_instance_refs(r, instance_names, out);
        }
        Expr::UnaryOp(_, e) => collect_instance_refs(e, instance_names, out),
        Expr::Index(b, k) => {
            collect_instance_refs(b, instance_names, out);
            collect_instance_refs(k, instance_names, out);
        }
        Expr::FnCall(_, args) | Expr::MacroRef(_, args) => {
            for a in args {
                collect_instance_refs(a, instance_names, out);
            }
        }
        Expr::If(c, t, e) => {
            collect_instance_refs(c, instance_names, out);
            collect_instance_refs(t, instance_names, out);
            if let Some(x) = e {
                collect_instance_refs(x, instance_names, out);
            }
        }
        Expr::Let(_, v, b) => {
            collect_instance_refs(v, instance_names, out);
            collect_instance_refs(b, instance_names, out);
        }
        Expr::Block(items) => {
            for e in items {
                collect_instance_refs(e, instance_names, out);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, e) in fields {
                collect_instance_refs(e, instance_names, out);
            }
        }
        Expr::RecordUpdate(b, fields) => {
            collect_instance_refs(b, instance_names, out);
            for (_, e) in fields {
                collect_instance_refs(e, instance_names, out);
            }
        }
        Expr::Closure(_, b) => collect_instance_refs(b, instance_names, out),
        Expr::Cast(e, _) => collect_instance_refs(e, instance_names, out),
        Expr::Tuple(es) | Expr::ArrayLit(es) => {
            for e in es {
                collect_instance_refs(e, instance_names, out);
            }
        }
        Expr::Match(s, arms) => {
            collect_instance_refs(s, instance_names, out);
            for a in arms {
                collect_instance_refs(&a.body, instance_names, out);
            }
        }
        Expr::EnumVariantWithData(_, _, args) => {
            for a in args {
                collect_instance_refs(a, instance_names, out);
            }
        }
        Expr::Some(e) => collect_instance_refs(e, instance_names, out),
        Expr::Range(s, e) => {
            collect_instance_refs(s, instance_names, out);
            collect_instance_refs(e, instance_names, out);
        }
        Expr::For(_, it, body) => {
            collect_instance_refs(it, instance_names, out);
            collect_instance_refs(body, instance_names, out);
        }
        Expr::NamespacedCall { args, .. } => {
            for a in args {
                collect_instance_refs(a, instance_names, out);
            }
        }
        Expr::AddressOf {
            args, with_params, ..
        } => {
            for a in args {
                collect_instance_refs(a, instance_names, out);
            }
            for (_, e) in with_params {
                collect_instance_refs(e, instance_names, out);
            }
        }
        Expr::Encode { value, .. } => collect_instance_refs(value, instance_names, out),
        _ => {}
    }
}

/// Rewrite `<inst>.<member>` / `<inst>.route(args)` to bare entity-scope forms.
fn strip_instance_qualifier(expr: &Expr, inst: &str) -> Expr {
    match expr {
        Expr::FieldAccess(inner, field) => {
            if let Expr::Ident(n) = inner.as_ref() {
                if n == inst {
                    return Expr::Ident(field.clone());
                }
            }
            Expr::FieldAccess(Box::new(strip_instance_qualifier(inner, inst)), field.clone())
        }
        Expr::MethodCall(recv, method, args) => {
            let args: Vec<Expr> = args.iter().map(|a| strip_instance_qualifier(a, inst)).collect();
            if let Expr::Ident(n) = recv.as_ref() {
                if n == inst {
                    return Expr::FnCall(method.clone(), args);
                }
            }
            Expr::MethodCall(
                Box::new(strip_instance_qualifier(recv, inst)),
                method.clone(),
                args,
            )
        }
        Expr::BinOp(l, op, r) => Expr::BinOp(
            Box::new(strip_instance_qualifier(l, inst)),
            op.clone(),
            Box::new(strip_instance_qualifier(r, inst)),
        ),
        Expr::UnaryOp(op, e) => {
            Expr::UnaryOp(op.clone(), Box::new(strip_instance_qualifier(e, inst)))
        }
        Expr::Index(b, k) => Expr::Index(
            Box::new(strip_instance_qualifier(b, inst)),
            Box::new(strip_instance_qualifier(k, inst)),
        ),
        Expr::FnCall(name, args) => Expr::FnCall(
            name.clone(),
            args.iter().map(|a| strip_instance_qualifier(a, inst)).collect(),
        ),
        Expr::MacroRef(name, args) => Expr::MacroRef(
            name.clone(),
            args.iter().map(|a| strip_instance_qualifier(a, inst)).collect(),
        ),
        Expr::If(c, t, e) => Expr::If(
            Box::new(strip_instance_qualifier(c, inst)),
            Box::new(strip_instance_qualifier(t, inst)),
            e.as_ref()
                .map(|x| Box::new(strip_instance_qualifier(x, inst))),
        ),
        Expr::Let(p, v, b) => Expr::Let(
            p.clone(),
            Box::new(strip_instance_qualifier(v, inst)),
            Box::new(strip_instance_qualifier(b, inst)),
        ),
        Expr::Block(items) => {
            Expr::Block(items.iter().map(|e| strip_instance_qualifier(e, inst)).collect())
        }
        Expr::RecordConstruct(name, fields) => Expr::RecordConstruct(
            name.clone(),
            fields
                .iter()
                .map(|(k, v)| (k.clone(), strip_instance_qualifier(v, inst)))
                .collect(),
        ),
        Expr::RecordUpdate(b, fields) => Expr::RecordUpdate(
            Box::new(strip_instance_qualifier(b, inst)),
            fields
                .iter()
                .map(|(k, v)| (k.clone(), strip_instance_qualifier(v, inst)))
                .collect(),
        ),
        Expr::Closure(ps, b) => {
            Expr::Closure(ps.clone(), Box::new(strip_instance_qualifier(b, inst)))
        }
        Expr::Cast(e, ty) => Expr::Cast(Box::new(strip_instance_qualifier(e, inst)), ty.clone()),
        Expr::Tuple(es) => {
            Expr::Tuple(es.iter().map(|e| strip_instance_qualifier(e, inst)).collect())
        }
        Expr::ArrayLit(es) => {
            Expr::ArrayLit(es.iter().map(|e| strip_instance_qualifier(e, inst)).collect())
        }
        Expr::Match(s, arms) => Expr::Match(
            Box::new(strip_instance_qualifier(s, inst)),
            arms.iter()
                .map(|a| crate::ast::MatchArm {
                    pattern: a.pattern.clone(),
                    body: strip_instance_qualifier(&a.body, inst),
                })
                .collect(),
        ),
        Expr::EnumVariantWithData(en, vn, args) => Expr::EnumVariantWithData(
            en.clone(),
            vn.clone(),
            args.iter().map(|a| strip_instance_qualifier(a, inst)).collect(),
        ),
        Expr::Some(e) => Expr::Some(Box::new(strip_instance_qualifier(e, inst))),
        Expr::Range(s, e) => Expr::Range(
            Box::new(strip_instance_qualifier(s, inst)),
            Box::new(strip_instance_qualifier(e, inst)),
        ),
        Expr::For(p, it, body) => Expr::For(
            p.clone(),
            Box::new(strip_instance_qualifier(it, inst)),
            Box::new(strip_instance_qualifier(body, inst)),
        ),
        Expr::NamespacedCall {
            namespace,
            name,
            args,
            type_params,
        } => Expr::NamespacedCall {
            namespace: namespace.clone(),
            name: name.clone(),
            args: args.iter().map(|a| strip_instance_qualifier(a, inst)).collect(),
            type_params: type_params.clone(),
        },
        Expr::AddressOf {
            entity_name,
            args,
            with_params,
        } => Expr::AddressOf {
            entity_name: entity_name.clone(),
            args: args.iter().map(|a| strip_instance_qualifier(a, inst)).collect(),
            with_params: with_params
                .iter()
                .map(|(k, v)| (k.clone(), strip_instance_qualifier(v, inst)))
                .collect(),
        },
        Expr::Encode { target_type, value } => Expr::Encode {
            target_type: target_type.clone(),
            value: Box::new(strip_instance_qualifier(value, inst)),
        },
        other => other.clone(),
    }
}

/// Shapes `rust_bool_expr_multi` / `rust_u256_expr_multi` can walk without a
/// leftover `gen_expr` panic. HashMap index and tuple/fn-call leaves stay
/// Inline (keyed getters or `compile_error!` oracles). Folds / method chains
/// remain non-simple so they hoist or become I17.
fn is_simple_multi_inline(expr: &Expr) -> bool {
    match expr {
        Expr::FieldAccess(inner, _) => is_simple_multi_inline(inner),
        Expr::Index(base, key) => is_simple_multi_inline(base) && is_simple_multi_inline(key),
        Expr::BinOp(l, _, r) => is_simple_multi_inline(l) && is_simple_multi_inline(r),
        Expr::UnaryOp(_, inner) => is_simple_multi_inline(inner),
        Expr::Tuple(xs) | Expr::ArrayLit(xs) => xs.iter().all(is_simple_multi_inline),
        Expr::FnCall(_, args) => args.iter().all(is_simple_multi_inline),
        Expr::MethodCall(recv, _, args) => {
            is_simple_multi_inline(recv) && args.iter().all(is_simple_multi_inline)
        }
        Expr::Ident(_)
        | Expr::IntLiteral(_)
       
       
        | Expr::BoolLiteral(_)
        | Expr::StringLiteral(_) => true,
        Expr::TraceField(field) if field == "length" => true,
        Expr::TraceCall { name, .. } if name == "count" || name == "lastWas" => true,
        _ => false,
    }
}

/// Collect entity-side invariant check helpers for a single entity.
pub(crate) fn invariant_check_helpers_for_entity(
    program: &Program,
    entity: &Entity,
    ctx: &EvmCtx,
) -> Vec<InvariantCheckHelper> {
    let mut out = Vec::new();
    for inv in &program.invariants {
        for idx in 0..inv.checks.len() {
            let (kind, helper) = classify_check_plan(program, inv, idx, ctx);
            if let CheckKind::Helper { entity: en, .. } = kind {
                if en == entity.name {
                    if let Some(h) = helper {
                        out.push(h);
                    }
                }
            }
        }
    }
    out
}

/// Map entity name → helpers (for invariant test emission).
#[allow(dead_code)]
pub(crate) fn invariant_check_helpers_by_entity(
    program: &Program,
    ctx: &EvmCtx,
) -> HashMap<String, Vec<InvariantCheckHelper>> {
    let mut map = HashMap::new();
    for entity in &program.entities {
        let helpers = invariant_check_helpers_for_entity(program, entity, ctx);
        if !helpers.is_empty() {
            map.insert(entity.name.clone(), helpers);
        }
    }
    map
}

/// Lookup helper for a specific invariant check index.
#[allow(dead_code)]
pub(crate) fn find_invariant_check_helper<'a>(
    helpers: &'a [InvariantCheckHelper],
    inv_name: &str,
    check_idx: usize,
) -> Option<&'a InvariantCheckHelper> {
    let expected = helper_fn_name(inv_name, check_idx);
    helpers.iter().find(|h| h.fn_name == expected)
}

/// Render Solidity helper functions to append inside an entity contract.
pub(crate) fn emit_invariant_check_helpers(helpers: &[InvariantCheckHelper]) -> String {
    let mut out = String::new();
    for h in helpers {
        out.push_str(&format!(
            "    function {}() public view returns (bool) {{\n",
            h.fn_name
        ));
        for line in &h.body_stmts {
            out.push_str("        ");
            out.push_str(line);
            if !line.trim_end().ends_with(';') && !line.trim_end().ends_with('}') {
                out.push(';');
            }
            out.push('\n');
        }
        out.push_str(&format!("        return ({});\n", h.return_expr));
        out.push_str("    }\n\n");
    }
    out
}

pub(crate) fn i17_message(inv_name: &str, check_idx: usize) -> String {
    format!(
        "invariant \"{}\": check #{} uses an EVM predicate form that cannot be lowered \
         (unsupported library/`keys().fold` shape, or a check that spans multiple instances). \
         Rewrite the check as a `view` route if you cannot express it yet",
        inv_name, check_idx
    )
}

/// Lower a check for Foundry invariant emission: helper call, inline expr, or bug.
pub(crate) fn lower_invariant_check_require_expr(
    program: &Program,
    entity: &Entity,
    inv: &InvariantDecl,
    check_idx: usize,
    check: &Expr,
    sut_var: &str,
    ctx: &EvmCtx,
    handler_state_idents: &[String],
) -> String {
    match classify_invariant_check(program, inv, check_idx, ctx) {
        CheckKind::Helper { fn_name, instance, .. } => match instance {
            Some(inst) => format!("_{}.{}()", inst, fn_name),
            None => format!("{}.{}()", sut_var, fn_name),
        },
        CheckKind::Inline => super::evm_test_codegen::lower_invariant_check_inline(
            program,
            entity,
            check,
            ctx,
            sut_var,
            handler_state_idents,
        ),
        CheckKind::Unsupported => unreachable!(
            "I17 skipped (bug): {}",
            i17_message(&inv.name, check_idx)
        ),
    }
}

/// Foundry / revm helper invocation for a classified `Helper` check.
pub(crate) fn helper_call_foundry(instance: Option<&str>, sut_var: &str, fn_name: &str) -> String {
    match instance {
        Some(inst) => format!("_{}.{}()", inst, fn_name),
        None => format!("{}.{}()", sut_var, fn_name),
    }
}
