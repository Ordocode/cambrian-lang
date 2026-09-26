// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Per-route boolean facts (universal base) + policy-parameterized fail closure.
//!
//! Failure *propagation* is domain semantics — see
//! [`docs/plans/p2-kernel-analyses.md`] Step D. The kernel stores only
//! universal base facts; adapters instantiate [`FailPropagation`].

use std::collections::{HashMap, HashSet};

use crate::ast::{Entity, Expr, Program, Route, RouteAction, RouteBody};

use super::graphs::ProgramGraphs;
use super::walk::{for_each_action, for_each_route_action};

/// Universal, syntactic, per-route facts (kernel level).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteFacts {
    /// Body / where / transforms read `msg::value`.
    pub uses_msg_value: bool,
    /// The route's own body is a fail surface *without* following
    /// `call` edges: `from` / `where` / `throw` / `deploy` / raw transfer,
    /// or a nested `if`/`for` containing those. Deliberately excludes
    /// runtime traps (overflow) — those are domain semantics.
    pub local_fail_surface: bool,
    /// Route declares an explicit return type (view-shaped).
    pub is_view: bool,
    /// Unphased body needs World threading (effects / world-only `sys::*`).
    pub has_unphased_sends: bool,
    /// Phased body needs World threading.
    pub phased_needs_world_thread: bool,
    /// Same-entity `call name(...)` targets (CallRoute only — used by
    /// fail-closure; distinct from SCC `route_callees` which also
    /// includes typed self-sends).
    pub call_callees: Vec<String>,
}

/// Which call-graph edges transmit failure caller-ward. Adapter data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailPropagation {
    /// Sync same-entity `call` propagates (evm×lean: yes; TVM: no).
    pub same_entity_calls: bool,
    /// Typed cross-entity sends propagate (evm×lean: no — L11).
    pub cross_entity_sends: bool,
    /// `rescue` cuts propagation (TVM: yes; EVM/Lean: E26 rejects rescue).
    pub rescue_cuts: bool,
}

impl FailPropagation {
    /// Lean-EVM / validate L5–L11 pair policy — transcribed from
    /// `lean_route_can_fail` / `route_fail_mode`.
    pub const EVM_LEAN: Self = Self {
        same_entity_calls: true,
        cross_entity_sends: false,
        rescue_cuts: false, // E26: rescue is not modelled on EVM/Lean
    };
}

/// Compute universal [`RouteFacts`] for every route in `program`.
pub fn compute_route_facts(program: &Program) -> HashMap<(String, String), RouteFacts> {
    let mut out = HashMap::new();
    for entity in &program.entities {
        for route in &entity.routes {
            out.insert(
                (entity.name.clone(), route.name.clone()),
                compute_one(entity, route),
            );
        }
    }
    out
}

fn compute_one(entity: &Entity, route: &Route) -> RouteFacts {
    RouteFacts {
        uses_msg_value: route_uses_msg_value_inner(route, entity),
        local_fail_surface: local_fail_surface(route),
        is_view: route.return_type.is_some(),
        has_unphased_sends: route_has_unphased_sends(route),
        phased_needs_world_thread: route_phased_needs_world_thread(route),
        call_callees: call_callees(entity, route),
    }
}

fn call_callees(entity: &Entity, route: &Route) -> Vec<String> {
    let mut deps = HashSet::new();
    for_each_route_action(&route.body, |action| {
        if let RouteAction::CallRoute { name, .. } = action {
            if entity.routes.iter().any(|r| r.name == *name) {
                deps.insert(name.clone());
            }
        }
    });
    let mut v: Vec<_> = deps.into_iter().collect();
    v.sort();
    v
}

/// Closure of `local_fail_surface` under `policy` over CallRoute edges.
pub fn fail_closure(
    facts: &HashMap<(String, String), RouteFacts>,
    policy: FailPropagation,
) -> HashMap<(String, String), bool> {
    let mut can_fail: HashMap<(String, String), bool> = HashMap::new();
    for (key, f) in facts {
        can_fail.insert(key.clone(), f.local_fail_surface);
    }
    if !policy.same_entity_calls {
        return can_fail;
    }
    // Fixpoint over CallRoute edges (ignore cross_entity_sends — not in
    // call_callees). `rescue_cuts` is already baked into local_fail_surface
    // (we don't walk rescue bodies).
    let _ = policy.cross_entity_sends;
    let _ = policy.rescue_cuts;
    let mut changed = true;
    while changed {
        changed = false;
        for (key, f) in facts {
            if can_fail[key] {
                continue;
            }
            let propagates = f.call_callees.iter().any(|callee| {
                can_fail
                    .get(&(key.0.clone(), callee.clone()))
                    .copied()
                    .unwrap_or(false)
            });
            if propagates {
                can_fail.insert(key.clone(), true);
                changed = true;
            }
        }
    }
    can_fail
}

/// Convenience: facts + evm×lean fail closure for one program.
pub fn evm_lean_can_fail(program: &Program) -> HashMap<(String, String), bool> {
    let facts = compute_route_facts(program);
    fail_closure(&facts, FailPropagation::EVM_LEAN)
}

/// evm×lean can-fail for a single entity (all its routes). Used by
/// validate L-rules and Lean `route_fail_mode` without building a full
/// `Program`.
pub fn entity_evm_lean_can_fail(entity: &Entity) -> HashMap<String, bool> {
    let mut facts = HashMap::new();
    for route in &entity.routes {
        facts.insert(
            (entity.name.clone(), route.name.clone()),
            compute_one(entity, route),
        );
    }
    fail_closure(&facts, FailPropagation::EVM_LEAN)
        .into_iter()
        .map(|((_, route), v)| (route, v))
        .collect()
}

/// Single-route lookup matching historical `route_fail_mode` /
/// `lean_route_can_fail`.
pub fn route_can_fail_evm_lean(entity: &Entity, route: &Route) -> bool {
    entity_evm_lean_can_fail(entity)
        .get(&route.name)
        .copied()
        .unwrap_or(false)
}

fn local_fail_surface(route: &Route) -> bool {
    if !route.from_clauses.is_empty() || !route.where_clauses.is_empty() {
        return true;
    }
    if let RouteBody::Phased(phases) | RouteBody::Mixed(phases, _) = &route.body {
        if phases.iter().any(|p| !p.where_clauses.is_empty()) {
            return true;
        }
    }
    let mut found = false;
    walk_actions_skip_rescue(&route.body, &mut |action| {
        if matches!(
            action,
            RouteAction::Throw { .. }
                | RouteAction::ThrowCustom { .. }
                | RouteAction::Deploy { .. }
                | RouteAction::Send { message: None, .. }
        ) {
            found = true;
        }
    });
    found
}

fn walk_actions_skip_rescue(body: &RouteBody, f: &mut impl FnMut(&RouteAction)) {
    match body {
        RouteBody::Unphased(a) => {
            for a in a {
                walk_action_skip_rescue(a, f);
            }
        }
        RouteBody::Mixed(phases, bare) => {
            for p in phases {
                for a in &p.actions {
                    walk_action_skip_rescue(a, f);
                }
            }
            for a in bare {
                walk_action_skip_rescue(a, f);
            }
        }
        RouteBody::Phased(phases) => {
            for p in phases {
                for a in &p.actions {
                    walk_action_skip_rescue(a, f);
                }
            }
        }
    }
}

fn walk_action_skip_rescue(action: &RouteAction, f: &mut impl FnMut(&RouteAction)) {
    f(action);
    match action {
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            for a in then_actions {
                walk_action_skip_rescue(a, f);
            }
            for a in else_actions {
                walk_action_skip_rescue(a, f);
            }
        }
        RouteAction::For { body, .. } => {
            for a in body {
                walk_action_skip_rescue(a, f);
            }
        }
        RouteAction::Rescue { .. } => {
            // Do not enter — matches lean_route_can_fail / route_fail_mode.
        }
        _ => {}
    }
}

fn route_uses_msg_value_inner(route: &Route, entity: &Entity) -> bool {
    fn walk(expr: &Expr) -> bool {
        match expr {
            Expr::MsgField(f) if f == "value" => true,
            Expr::BinOp(l, _, r) => walk(l) || walk(r),
            Expr::UnaryOp(_, e) => walk(e),
            Expr::Index(b, k) => walk(b) || walk(k),
            Expr::FieldAccess(b, _) => walk(b),
            Expr::MethodCall(b, _, args) => walk(b) || args.iter().any(walk),
            Expr::FnCall(_, args) | Expr::MacroRef(_, args) => args.iter().any(walk),
            Expr::NamespacedCall { args, .. } => args.iter().any(walk),
            Expr::If(c, t, e) => walk(c) || walk(t) || e.as_ref().map_or(false, |x| walk(x)),
            Expr::Let(_, v, b) => walk(v) || walk(b),
            Expr::Match(s, arms) => walk(s) || arms.iter().any(|a| walk(&a.body)),
            Expr::Tuple(es) | Expr::ArrayLit(es) => es.iter().any(walk),
            Expr::RecordConstruct(_, fs) => fs.iter().any(|(_, e)| walk(e)),
            Expr::RecordUpdate(b, fs) => walk(b) || fs.iter().any(|(_, e)| walk(e)),
            Expr::Cast(e, _) | Expr::Some(e) => walk(e),
            Expr::Range(a, b) => walk(a) || walk(b),
            Expr::For(_, it, body) => walk(it) || walk(body),
            Expr::Block(items) => items.iter().any(walk),
            Expr::EnumVariantWithData(_, _, args) => args.iter().any(walk),
            _ => false,
        }
    }
    fn walk_action(a: &RouteAction) -> bool {
        match a {
            RouteAction::Send {
                args,
                dest,
                send_options,
                ..
            } => args.iter().any(walk) || walk(dest) || send_options.as_ref().map_or(false, walk),
            RouteAction::Conditional {
                condition,
                then_actions,
                else_actions,
            } => {
                walk(condition)
                    || then_actions.iter().any(walk_action)
                    || else_actions.iter().any(walk_action)
            }
            RouteAction::Return { values } => values.iter().any(walk),
            RouteAction::Let { value, .. } => walk(value),
            RouteAction::Effect { args, .. } => args.iter().any(walk),
            RouteAction::Deploy {
                send_options,
                constructor_args,
                ..
            } => send_options.as_ref().map_or(false, walk) || constructor_args.iter().any(walk),
            RouteAction::Rescue { action, .. } => walk_action(action),
            RouteAction::CallRoute { args, .. } => args.iter().any(walk),
            RouteAction::VarCall {
                args,
                dest,
                send_options,
                ..
            } => args.iter().any(walk) || walk(dest) || send_options.as_ref().map_or(false, walk),
            RouteAction::UpdateCode {
                update_args,
                callback_args,
                ..
            } => update_args.iter().any(walk) || callback_args.iter().any(walk),
            RouteAction::For { iter, body, .. } => walk(iter) || body.iter().any(walk_action),
            RouteAction::Throw { .. } => false,
            RouteAction::ThrowCustom { args, .. } => args.iter().any(walk),
            RouteAction::Emit { args, .. } => args.iter().any(walk),
        }
    }
    if route.body.all_actions().iter().any(|a| walk_action(a)) {
        return true;
    }
    for w in &route.where_clauses {
        if walk(&w.condition) {
            return true;
        }
    }
    for m in &entity.members {
        for t in &m.transforms {
            if t.route_name == route.name && walk(&t.body) {
                return true;
            }
        }
    }
    false
}

/// Public accessor used by EVM payable detection.
pub fn route_uses_msg_value(route: &Route, entity: &Entity) -> bool {
    compute_one(entity, route).uses_msg_value
}

pub fn route_has_unphased_sends(route: &Route) -> bool {
    // needs entity? only route body — recompute without full facts
    match &route.body {
        RouteBody::Unphased(actions) | RouteBody::Mixed(_, actions) => {
            actions_need_world_thread(actions)
        }
        RouteBody::Phased(_) => false,
    }
}

pub fn route_phased_needs_world_thread(route: &Route) -> bool {
    if let RouteBody::Phased(phases) = &route.body {
        phases.iter().any(|p| actions_need_world_thread(&p.actions))
    } else {
        false
    }
}

pub fn route_is_view(route: &Route) -> bool {
    route.return_type.is_some()
}

/// Syntactic read-only check for EVM `view` inference (SD-01a). Fills gaps
/// when a route only reads state (e.g. `totalSupply() -> T => [ return(m_x) ]`)
/// so `check totalSupply() == …` compiles inside Foundry `view` invariants.
pub fn route_infer_evm_view(entity: &Entity, route: &Route) -> bool {
    if route.is_view || route.is_pure {
        return false;
    }
    if route.is_init
        || route.name == "constructor"
        || route.name == "receive"
        || route.name == "fallback"
        || route.recover_tag.is_some()
        || route.is_accept
    {
        return false;
    }
    if route_has_member_transforms(entity, route) {
        return false;
    }
    if route_uses_msg_value(route, entity) {
        return false;
    }
    !route_has_mutating_actions(route)
}

fn route_has_member_transforms(entity: &Entity, route: &Route) -> bool {
    entity
        .members
        .iter()
        .any(|m| m.transforms.iter().any(|t| t.route_name == route.name))
}

fn route_has_mutating_actions(route: &Route) -> bool {
    let mut mutating = false;
    for_each_route_action(&route.body, |action| {
        if action_is_mutating(action) {
            mutating = true;
        }
    });
    mutating
}

fn expr_is_hashmap_mutation(expr: &Expr) -> bool {
    match expr {
        Expr::MethodCall(base, method, _)
            if matches!(method.as_str(), "insert" | "update" | "remove") =>
        {
            matches!(base.as_ref(), Expr::Ident(_))
        }
        Expr::BinOp(l, _, r) => expr_is_hashmap_mutation(l) || expr_is_hashmap_mutation(r),
        Expr::UnaryOp(_, e) => expr_is_hashmap_mutation(e),
        Expr::If(c, t, e) => {
            expr_is_hashmap_mutation(c)
                || expr_is_hashmap_mutation(t)
                || e.as_ref().map_or(false, |x| expr_is_hashmap_mutation(x))
        }
        Expr::Let(_, v, b) => expr_is_hashmap_mutation(v) || expr_is_hashmap_mutation(b),
        Expr::Block(items) => items.iter().any(expr_is_hashmap_mutation),
        _ => false,
    }
}

fn action_is_mutating(action: &RouteAction) -> bool {
    match action {
        RouteAction::Let { value, .. } => expr_is_hashmap_mutation(value),
        RouteAction::Send { .. }
        | RouteAction::Deploy { .. }
        | RouteAction::Emit { .. }
        | RouteAction::VarCall { .. }
        | RouteAction::UpdateCode { .. }
        | RouteAction::CallRoute { .. } => true,
        RouteAction::Return { values } => values.iter().any(expr_is_hashmap_mutation),
        RouteAction::Conditional { then_actions, else_actions, .. } => {
            then_actions.iter().any(action_is_mutating)
                || else_actions.iter().any(action_is_mutating)
        }
        RouteAction::For { body, .. } => body.iter().any(action_is_mutating),
        RouteAction::Rescue { action, .. } => action_is_mutating(action),
        _ => false,
    }
}

fn actions_need_world_thread(actions: &[RouteAction]) -> bool {
    let mut found = false;
    for a in actions {
        for_each_action(a, &mut |inner| {
            if action_needs_world_thread(inner) || action_exprs_require_world(inner) {
                found = true;
            }
        });
    }
    found
}

fn action_needs_world_thread(action: &RouteAction) -> bool {
    matches!(
        action,
        RouteAction::Send { .. }
            | RouteAction::VarCall { .. }
            | RouteAction::Deploy { .. }
            | RouteAction::Emit { .. }
            | RouteAction::CallRoute { .. }
    )
}

fn action_exprs_require_world(action: &RouteAction) -> bool {
    let mut need = false;
    walk_action_root_exprs(action, &mut |e| {
        if expr_requires_world(e) {
            need = true;
        }
    });
    need
}

/// True when a `sys::*` field has no State/MsgCtx fallback and must be
/// lowered against a World binder (`sys::balance`, `sys::blockNumber`,
/// `sys::chainid`). Shared by route-facts world-threading and Lean
/// `_pre_*` / macro world params (UPSTREAM B-31).
pub fn sys_field_needs_world(field: &str) -> bool {
    matches!(
        field,
        "balance" | "blockNumber" | "block_number" | "number" | "chainid" | "chainId"
    )
}

fn expr_requires_world(expr: &Expr) -> bool {
    match expr {
        Expr::SysField(f) if sys_field_needs_world(f) => true,
        _ => {
            let mut need = false;
            walk_expr_children(expr, &mut |child| {
                if expr_requires_world(child) {
                    need = true;
                }
            });
            need
        }
    }
}

fn walk_action_root_exprs(action: &RouteAction, f: &mut impl FnMut(&Expr)) {
    match action {
        RouteAction::Let { value, .. } => f(value),
        RouteAction::Return { values } => {
            for v in values {
                f(v);
            }
        }
        RouteAction::Send {
            args,
            dest,
            send_options,
            ..
        }
        | RouteAction::VarCall {
            args,
            dest,
            send_options,
            ..
        } => {
            for a in args {
                f(a);
            }
            f(dest);
            if let Some(opts) = send_options {
                f(opts);
            }
        }
        RouteAction::Deploy {
            constructor_args,
            send_options,
            ..
        } => {
            for a in constructor_args {
                f(a);
            }
            if let Some(opts) = send_options {
                f(opts);
            }
        }
        RouteAction::Conditional { condition, .. } => f(condition),
        RouteAction::For { iter, .. } => f(iter),
        RouteAction::CallRoute { args, .. }
        | RouteAction::Effect { args, .. }
        | RouteAction::Emit { args, .. }
        | RouteAction::ThrowCustom { args, .. } => {
            for a in args {
                f(a);
            }
        }
        RouteAction::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            for a in update_args {
                f(a);
            }
            for a in callback_args {
                f(a);
            }
        }
        RouteAction::Rescue { .. } | RouteAction::Throw { .. } => {}
    }
}

pub(crate) fn walk_expr_children(expr: &Expr, f: &mut impl FnMut(&Expr)) {
    match expr {
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Some(e) | Expr::Cast(e, _) => f(e),
        Expr::BinOp(a, _, b) | Expr::Index(a, b) | Expr::Range(a, b) => {
            f(a);
            f(b);
        }
        Expr::MethodCall(recv, _, args) => {
            f(recv);
            for a in args {
                f(a);
            }
        }
        Expr::If(a, b, c) => {
            f(a);
            f(b);
            if let Some(e) = c {
                f(e);
            }
        }
        Expr::FnCall(_, args)
        | Expr::MacroRef(_, args)
        | Expr::Tuple(args)
        | Expr::ArrayLit(args)
        | Expr::EnumVariantWithData(_, _, args) => {
            for a in args {
                f(a);
            }
        }
        Expr::NamespacedCall { args, .. } => {
            for a in args {
                f(a);
            }
        }
        Expr::Encode { value, .. } => f(value),
        Expr::AddressOf {
            args, with_params, ..
        } => {
            for a in args {
                f(a);
            }
            for (_, e) in with_params {
                f(e);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, e) in fields {
                f(e);
            }
        }
        Expr::RecordUpdate(base, fields) => {
            f(base);
            for (_, e) in fields {
                f(e);
            }
        }
        Expr::Match(scrut, arms) => {
            f(scrut);
            for arm in arms {
                f(&arm.body);
            }
        }
        Expr::Block(es) => {
            for e in es {
                f(e);
            }
        }
        Expr::Let(_, v, body) => {
            f(v);
            f(body);
        }
        Expr::Closure(_, body) => f(body),
        Expr::For(_, iter, body) => {
            f(iter);
            f(body);
        }
        Expr::IntLiteral(_)
       
       
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::BoolLiteral(_)
        | Expr::EmptyCollection
        | Expr::Ident(_)
        | Expr::TemporalRef(_)
        | Expr::MsgField(_)
        | Expr::SysField(_)
        | Expr::TraceField(_)
        | Expr::TraceCall { .. }
        | Expr::EnumVariant(_, _)
        | Expr::None => {}
    }
}

/// Attach route facts onto an existing [`ProgramGraphs`].
#[allow(dead_code)]
pub fn attach_route_facts(graphs: &mut ProgramGraphs, program: &Program) {
    graphs.route_facts = compute_route_facts(program);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Program {
        crate::ProgramParser::new()
            .parse(src)
            .unwrap_or_else(|e| panic!("parse failed: {e}"))
    }

    #[test]
    fn fail_closure_propagates_via_call() {
        let program = parse(
            r#"
            entity E {
                routes {
                    helper() where (false) : throw 1 => []
                    wrapper() => [ call helper() ]
                }
            }
            "#,
        );
        let can = evm_lean_can_fail(&program);
        assert_eq!(can.get(&("E".into(), "helper".into())), Some(&true));
        assert_eq!(can.get(&("E".into(), "wrapper".into())), Some(&true));
    }

    #[test]
    fn local_fail_surface_ignores_rescue_body() {
        let program = parse(
            r#"
            entity E {
                identity id: u64
                routes {
                    ok() => [
                        rescue tag: deploy E with { value: 1, stateInit: E.state(id) }
                    ]
                    recover tag() => []
                }
            }
            "#,
        );
        let facts = compute_route_facts(&program);
        let f = facts.get(&("E".into(), "ok".into())).unwrap();
        assert!(!f.local_fail_surface, "rescue body must not count on Lean");
    }
}
