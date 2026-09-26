// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase Library-2: `using ... for T;` method-call sugar rewrite.
//!
//! Walks every expression in the program (entity routes, member
//! transforms, where clauses, from clauses, route from-clauses, macros,
//! tests, fuzz tests, invariants) and rewrites `recv.method(args)` to
//! `method(recv, args)` whenever:
//!
//!   1. A `using { ..., method, ... } for T;` (or `using LibName for T;`)
//!      directive is in scope, and
//!   2. The receiver's type can be inferred to match `T`.
//!
//! The rewrite happens *after* `normalize_program_types` and *after*
//! the loader's merge step, so both single-file and project modes
//! benefit uniformly. Codegen sees only the rewritten form, so neither
//! the EVM nor the Acki Nacki backend needs to know about `using`.

use crate::ast::*;
use crate::codegen::infer_expr_type;
use std::collections::HashMap;

/// Apply `using ... for T;` rewrites in place across the merged program.
///
/// Library-2 only rewrites — never reports diagnostics. The validator
/// (V55/V56/V57) is responsible for catching unresolved or
/// type-mismatched `using` decls.
pub fn apply_using_rewrites(program: &mut Program) {
    if program.using_decls.is_empty() {
        return;
    }

    // Build: method_name -> [(target_type, source) ...]
    // The source field records whether the entry came from a
    // free-function `using { ... } for T;` or a library `using Lib for T;`
    // so future phases can choose between free-fn and `Lib.fn` lowering.
    let mut by_method: HashMap<String, Vec<UsingEntry>> = HashMap::new();

    // Free-function pool: every visible `pure fn` at program scope.
    let free_fn_names: std::collections::HashSet<&str> =
        program.pure_fns.iter().map(|pf| pf.name.as_str()).collect();

    // Library-scoped pure fn pool, indexed by library name.
    let mut lib_fns: HashMap<&str, std::collections::HashSet<&str>> = HashMap::new();
    for lib in &program.libraries {
        lib_fns.insert(
            lib.name.as_str(),
            lib.pure_fns.iter().map(|pf| pf.name.as_str()).collect(),
        );
    }

    for ud in &program.using_decls {
        match &ud.items {
            UsingItems::Functions(names) => {
                for name in names {
                    if free_fn_names.contains(name.as_str()) {
                        by_method
                            .entry(name.clone())
                            .or_default()
                            .push(UsingEntry {
                                target_type: ud.target_type.clone(),
                                lib_name: None,
                            });
                    }
                }
            }
            UsingItems::Library(lib_name) => {
                if let Some(fns) = lib_fns.get(lib_name.as_str()) {
                    for name in fns {
                        by_method
                            .entry((*name).to_string())
                            .or_default()
                            .push(UsingEntry {
                                target_type: ud.target_type.clone(),
                                lib_name: Some(lib_name.clone()),
                            });
                    }
                }
            }
        }
    }

    if by_method.is_empty() {
        return;
    }

    // Build a pure-fn return-type map so `infer_expr_type` (which is
    // entity-scoped) can also resolve `FnCall(name, ...)` results when
    // rewriting chained calls like `v.add(1).mul(2)`. Includes
    // library-scoped pure fns too — they're reachable via `using Lib
    // for T;`.
    let mut pure_fn_returns: HashMap<String, Type> = HashMap::new();
    for pf in &program.pure_fns {
        pure_fn_returns.insert(pf.name.clone(), pf.return_type.clone());
    }
    for lib in &program.libraries {
        for pf in &lib.pure_fns {
            pure_fn_returns.insert(pf.name.clone(), pf.return_type.clone());
            pure_fn_returns.insert(format!("{}.{}", lib.name, pf.name), pf.return_type.clone());
        }
    }

    // Apply rewrites per-entity (needs entity members and route params
    // for type inference). Iterate without holding an immutable borrow
    // on `program.entities` while mutating.
    let entities = std::mem::take(&mut program.entities);
    let mut new_entities = Vec::with_capacity(entities.len());
    for mut entity in entities {
        rewrite_entity(&mut entity, &by_method, &pure_fn_returns);
        new_entities.push(entity);
    }
    program.entities = new_entities;
}

#[derive(Debug, Clone)]
struct UsingEntry {
    target_type: Type,
    /// `None` → free-function form (codegen emits `name(recv, args)`).
    /// `Some(lib)` → library form. Reserved for Phase 3 (codegen will
    /// emit `Lib.name(recv, args)`); Library-2 still rewrites the AST
    /// identically.
    #[allow(dead_code)]
    lib_name: Option<String>,
}

fn rewrite_entity(
    entity: &mut Entity,
    by_method: &HashMap<String, Vec<UsingEntry>>,
    pure_fn_returns: &HashMap<String, Type>,
) {
    let entity_snapshot = entity.clone();

    for route in &mut entity.routes {
        let route_params = route.params.clone();
        for wc in &mut route.where_clauses {
            rewrite_expr(&mut wc.condition, &entity_snapshot, &route_params, by_method, pure_fn_returns);
        }
        for fc in &mut route.from_clauses {
            for a in &mut fc.args {
                rewrite_expr(a, &entity_snapshot, &route_params, by_method, pure_fn_returns);
            }
        }
        rewrite_route_body(&mut route.body, &entity_snapshot, &route_params, by_method, pure_fn_returns);
    }
    for member in &mut entity.members {
        for t in &mut member.transforms {
            // Resolve the matching route to recover named parameter
            // types; fall back to an empty list if the transform
            // references an unknown route (validator will reject this
            // independently).
            let params: Vec<Param> = entity_snapshot
                .routes
                .iter()
                .find(|r| r.name == t.route_name)
                .map(|r| {
                    r.params
                        .iter()
                        .zip(t.params.iter())
                        .filter_map(|(rp, pat)| {
                            if let Pattern::Ident(name) = pat {
                                Some(Param {
                                    name: name.clone(),
                                    ty: rp.ty.clone(),
                                })
                            } else {
                                None
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            rewrite_expr(&mut t.body, &entity_snapshot, &params, by_method, pure_fn_returns);
        }
        if let Some(dv) = &mut member.default_value {
            rewrite_expr(dv, &entity_snapshot, &[], by_method, pure_fn_returns);
        }
    }
    for mac in &mut entity.macros {
        let params = mac.params.clone();
        rewrite_expr(&mut mac.body, &entity_snapshot, &params, by_method, pure_fn_returns);
    }
    for c in &mut entity.constants {
        rewrite_expr(&mut c.value, &entity_snapshot, &[], by_method, pure_fn_returns);
    }
}

fn rewrite_route_body(
    body: &mut RouteBody,
    entity: &Entity,
    route_params: &[Param],
    by_method: &HashMap<String, Vec<UsingEntry>>,
    pure_fn_returns: &HashMap<String, Type>,
) {
    match body {
        RouteBody::Unphased(actions) => {
            for a in actions {
                rewrite_action(a, entity, route_params, by_method, pure_fn_returns);
            }
        }
        RouteBody::Phased(phases) => {
            for ph in phases {
                for wc in &mut ph.where_clauses {
                    rewrite_expr(&mut wc.condition, entity, route_params, by_method, pure_fn_returns);
                }
                for a in &mut ph.actions {
                    rewrite_action(a, entity, route_params, by_method, pure_fn_returns);
                }
            }
        }
        RouteBody::Mixed(phases, actions) => {
            for ph in phases {
                for wc in &mut ph.where_clauses {
                    rewrite_expr(&mut wc.condition, entity, route_params, by_method, pure_fn_returns);
                }
                for a in &mut ph.actions {
                    rewrite_action(a, entity, route_params, by_method, pure_fn_returns);
                }
            }
            for a in actions {
                rewrite_action(a, entity, route_params, by_method, pure_fn_returns);
            }
        }
    }
}

fn rewrite_action(
    action: &mut RouteAction,
    entity: &Entity,
    route_params: &[Param],
    by_method: &HashMap<String, Vec<UsingEntry>>,
    pure_fn_returns: &HashMap<String, Type>,
) {
    match action {
        RouteAction::Let { value, .. } => rewrite_expr(value, entity, route_params, by_method, pure_fn_returns),
        RouteAction::Return { values } => {
            for v in values {
                rewrite_expr(v, entity, route_params, by_method, pure_fn_returns);
            }
        }
        RouteAction::Throw { .. } => {}
        RouteAction::ThrowCustom { args, .. } => {
            for a in args {
                rewrite_expr(a, entity, route_params, by_method, pure_fn_returns);
            }
        }
        RouteAction::Send { message: _, args, dest, send_options } => {
            for a in args {
                rewrite_expr(a, entity, route_params, by_method, pure_fn_returns);
            }
            rewrite_expr(dest, entity, route_params, by_method, pure_fn_returns);
            if let Some(opts) = send_options {
                rewrite_expr(opts, entity, route_params, by_method, pure_fn_returns);
            }
        }
        RouteAction::Deploy { send_options, constructor_args, .. } => {
            if let Some(opts) = send_options {
                rewrite_expr(opts, entity, route_params, by_method, pure_fn_returns);
            }
            for a in constructor_args {
                rewrite_expr(a, entity, route_params, by_method, pure_fn_returns);
            }
        }
        RouteAction::Conditional { condition, then_actions, else_actions } => {
            rewrite_expr(condition, entity, route_params, by_method, pure_fn_returns);
            for a in then_actions {
                rewrite_action(a, entity, route_params, by_method, pure_fn_returns);
            }
            for a in else_actions {
                rewrite_action(a, entity, route_params, by_method, pure_fn_returns);
            }
        }
        RouteAction::Rescue { action: inner, .. } => {
            rewrite_action(inner, entity, route_params, by_method, pure_fn_returns);
        }
        RouteAction::CallRoute { args, .. } => {
            for a in args {
                rewrite_expr(a, entity, route_params, by_method, pure_fn_returns);
            }
        }
        RouteAction::Effect { args, .. } => {
            for a in args {
                rewrite_expr(a, entity, route_params, by_method, pure_fn_returns);
            }
        }
        RouteAction::VarCall { args, dest, send_options, .. } => {
            for a in args {
                rewrite_expr(a, entity, route_params, by_method, pure_fn_returns);
            }
            rewrite_expr(dest, entity, route_params, by_method, pure_fn_returns);
            if let Some(opts) = send_options {
                rewrite_expr(opts, entity, route_params, by_method, pure_fn_returns);
            }
        }
        RouteAction::UpdateCode { update_args, callback_args, .. } => {
            for a in update_args {
                rewrite_expr(a, entity, route_params, by_method, pure_fn_returns);
            }
            for a in callback_args {
                rewrite_expr(a, entity, route_params, by_method, pure_fn_returns);
            }
        }
        RouteAction::For { iter, body, .. } => {
            rewrite_expr(iter, entity, route_params, by_method, pure_fn_returns);
            for a in body {
                rewrite_action(a, entity, route_params, by_method, pure_fn_returns);
            }
        }
        RouteAction::Emit { args, .. } => {
            for a in args {
                rewrite_expr(a, entity, route_params, by_method, pure_fn_returns);
            }
        }
    }
}

fn rewrite_expr(
    expr: &mut Expr,
    entity: &Entity,
    route_params: &[Param],
    by_method: &HashMap<String, Vec<UsingEntry>>,
    pure_fn_returns: &HashMap<String, Type>,
) {
    // First recurse so inner expressions are rewritten before the
    // outer one (so `a.b(x).c(y)` gets fully resolved bottom-up).
    rewrite_children(expr, entity, route_params, by_method, pure_fn_returns);

    // Look at the (now-rewritten) expression: if it's a MethodCall with
    // a `using`-attached name and a type-compatible receiver, swap it
    // for a free FnCall.
    if let Expr::MethodCall(recv, method, args) = expr {
        if let Some(entries) = by_method.get(method.as_str()) {
            // Infer the receiver's type; if we cannot, leave the
            // expression as a regular method call (no V57 fires here —
            // that's the validator's job).
            if let Some(recv_ty) = infer_receiver_type(recv, entity, route_params, pure_fn_returns) {
                if let Some(_entry) = entries.iter().find(|e| using_target_matches(&e.target_type, &recv_ty)) {
                    let mut new_args = Vec::with_capacity(args.len() + 1);
                    new_args.push((**recv).clone());
                    new_args.extend(std::mem::take(args));
                    *expr = Expr::FnCall(method.clone(), new_args);
                }
            }
        }
    }
}

/// Extends `codegen::infer_expr_type` with knowledge of `pure fn` return
/// types. Without this, chained calls like `v.add(1).mul(2)` cannot
/// type-resolve their outer receiver (an FnCall on a user-defined fn).
fn infer_receiver_type(
    expr: &Expr,
    entity: &Entity,
    route_params: &[Param],
    pure_fn_returns: &HashMap<String, Type>,
) -> Option<Type> {
    match expr {
        Expr::FnCall(name, _args) => {
            if let Some(ty) = pure_fn_returns.get(name) {
                return Some(ty.clone());
            }
        }
        Expr::MethodCall(base, method, _args) => {
            if let Expr::Ident(lib) = base.as_ref() {
                if let Some(ty) = pure_fn_returns.get(&format!("{lib}.{method}")) {
                    return Some(ty.clone());
                }
            }
        }
        _ => {}
    }
    infer_expr_type(expr, entity, route_params)
}

fn using_target_matches(target: &Type, receiver: &Type) -> bool {
    canonical_for_using(target) == canonical_for_using(receiver)
}

fn canonical_for_using(ty: &Type) -> Type {
    match ty {
        Type::Simple(s) => {
            let canon = match s.as_str() {
                "uint256" | "U256" | "u256" => "u256",
                "Address" | "address" => "address",
                other => other,
            };
            Type::Simple(canon.to_string())
        }
        _ => ty.clone(),
    }
}

fn rewrite_children(
    expr: &mut Expr,
    entity: &Entity,
    route_params: &[Param],
    by_method: &HashMap<String, Vec<UsingEntry>>,
    pfr: &HashMap<String, Type>,
) {
    match expr {
        Expr::BinOp(l, _, r) => {
            rewrite_expr(l, entity, route_params, by_method, pfr);
            rewrite_expr(r, entity, route_params, by_method, pfr);
        }
        Expr::UnaryOp(_, e) => rewrite_expr(e, entity, route_params, by_method, pfr),
        Expr::FieldAccess(e, _) => rewrite_expr(e, entity, route_params, by_method, pfr),
        Expr::Index(b, k) => {
            rewrite_expr(b, entity, route_params, by_method, pfr);
            rewrite_expr(k, entity, route_params, by_method, pfr);
        }
        Expr::MethodCall(b, _, args) => {
            rewrite_expr(b, entity, route_params, by_method, pfr);
            for a in args {
                rewrite_expr(a, entity, route_params, by_method, pfr);
            }
        }
        Expr::FnCall(_, args) => {
            for a in args {
                rewrite_expr(a, entity, route_params, by_method, pfr);
            }
        }
        Expr::NamespacedCall { args, .. } => {
            for a in args {
                rewrite_expr(a, entity, route_params, by_method, pfr);
            }
        }
        Expr::If(c, t, e) => {
            rewrite_expr(c, entity, route_params, by_method, pfr);
            rewrite_expr(t, entity, route_params, by_method, pfr);
            if let Some(el) = e {
                rewrite_expr(el, entity, route_params, by_method, pfr);
            }
        }
        Expr::Let(_, v, b) => {
            rewrite_expr(v, entity, route_params, by_method, pfr);
            rewrite_expr(b, entity, route_params, by_method, pfr);
        }
        Expr::Block(items) => {
            for it in items {
                rewrite_expr(it, entity, route_params, by_method, pfr);
            }
        }
        Expr::Match(subject, arms) => {
            rewrite_expr(subject, entity, route_params, by_method, pfr);
            for arm in arms {
                rewrite_expr(&mut arm.body, entity, route_params, by_method, pfr);
            }
        }
        Expr::Range(lo, hi) => {
            rewrite_expr(lo, entity, route_params, by_method, pfr);
            rewrite_expr(hi, entity, route_params, by_method, pfr);
        }
        Expr::Tuple(elems) | Expr::ArrayLit(elems) => {
            for e in elems {
                rewrite_expr(e, entity, route_params, by_method, pfr);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                rewrite_expr(v, entity, route_params, by_method, pfr);
            }
        }
        Expr::RecordUpdate(base, fields) => {
            rewrite_expr(base, entity, route_params, by_method, pfr);
            for (_, v) in fields {
                rewrite_expr(v, entity, route_params, by_method, pfr);
            }
        }
        Expr::For(_, iter, body) => {
            rewrite_expr(iter, entity, route_params, by_method, pfr);
            rewrite_expr(body, entity, route_params, by_method, pfr);
        }
        Expr::Closure(_, body) => {
            rewrite_expr(body, entity, route_params, by_method, pfr);
        }
        Expr::Cast(e, _) => rewrite_expr(e, entity, route_params, by_method, pfr),
        Expr::Some(e) => rewrite_expr(e, entity, route_params, by_method, pfr),
        Expr::EnumVariantWithData(_, _, args) => {
            for a in args {
                rewrite_expr(a, entity, route_params, by_method, pfr);
            }
        }
        Expr::MacroRef(_, args) => {
            for a in args {
                rewrite_expr(a, entity, route_params, by_method, pfr);
            }
        }
        Expr::AddressOf { args, with_params, .. } => {
            for a in args {
                rewrite_expr(a, entity, route_params, by_method, pfr);
            }
            for (_, v) in with_params {
                rewrite_expr(v, entity, route_params, by_method, pfr);
            }
        }
        Expr::Encode { value, .. } => {
            rewrite_expr(value, entity, route_params, by_method, pfr);
        }
        // Leaves — nothing to rewrite.
        _ => {}
    }
}
