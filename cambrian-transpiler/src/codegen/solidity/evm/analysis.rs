// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM codegen — entity-AST walkers driving sidecar / iteration emission.

use std::collections::HashSet;

use crate::ast::{Entity, Expr, Member, Pattern, Program, PureFn, RouteAction, Type};
use std::collections::HashMap;

use crate::codegen::solidity::core::types::{
    is_hashmap_valued_expr, simplify_hashmap_let_alias_init, subst_hashmap_let_in_expr,
};
use std::collections::HashMap as StdHashMap;

// ---------------------------------------------------------------------------
// Shared entity / action drivers
// ---------------------------------------------------------------------------
//
// All four walkers below ask the same structural question — "does any
// expression *anywhere reachable from this entity* match a particular
// shape?". The shape predicate differs per walker, but the AST traversal
// frame is identical. We extract that frame once so a new `RouteAction`
// or `Expr` variant only needs to be handled in this file in one place.

/// Apply `walk` to every top-level expression an action holds, recursing
/// through nested actions (`Conditional`, `Rescue`, `For`).
fn action_walks_any<F: Fn(&Expr) -> bool>(action: &RouteAction, walk: &F) -> bool {
    crate::analysis::action_walks_any(action, walk)
}

/// Returns `true` iff `walk` matches any expression reachable from the
/// entity: route bodies (actions + where clauses) and member metadata
/// (defaults + transforms). The `walk` closure is responsible for the
/// recursive descent into sub-expressions; this driver only feeds it
/// the per-action / per-clause / per-default / per-transform roots.
fn entity_walks_any<F: Fn(&Expr) -> bool>(entity: &Entity, walk: F) -> bool {
    for route in &entity.routes {
        for action in route.body.all_actions() {
            if action_walks_any(action, &walk) {
                return true;
            }
        }
        for wc in &route.where_clauses {
            if walk(&wc.condition) {
                return true;
            }
        }
    }
    for member in &entity.members {
        if let Some(default) = &member.default_value {
            if walk(default) {
                return true;
            }
        }
        for transform in &member.transforms {
            if walk(&transform.body) {
                return true;
            }
        }
    }
    entity.macros.iter().any(|m| walk(&m.body))
}

/// EVM-3 Batch G2 — does any expression reachable from the entity
/// call `.exists(...)` on a *nested* indexing of the named HashMap
/// member (i.e. `m[k1].exists(k2)` shape, after Batch G2's
/// let-substitution collapses `let inner = m[k1]; inner.exists(k2)`
/// to that form)? This drives the 2-level sidecar emission
/// `mapping(K1 => mapping(K2 => bool)) <m>_inner_exists`. The
/// detection runs against the *substituted* expression form, so it
/// catches both shapes uniformly.
pub(super) fn member_inner_uses_exists(entity: &Entity, member_name: &str) -> bool {
    fn walk(expr: &Expr, entity: &Entity, member: &str) -> bool {
        match expr {
            Expr::MethodCall(base, method, args) if method == "exists" || method == "contains" => {
                if let Expr::Index(inner, _) = base.as_ref() {
                    if matches!(inner.as_ref(), Expr::Ident(n) if n == member) {
                        return true;
                    }
                }
                walk(base, entity, member) || args.iter().any(|a| walk(a, entity, member))
            }
            Expr::MethodCall(base, _, args) => {
                walk(base, entity, member)
                    || args.iter().any(|a| walk(a, entity, member))
            }
            Expr::BinOp(l, _, r) => walk(l, entity, member) || walk(r, entity, member),
            Expr::UnaryOp(_, x) => walk(x, entity, member),
            Expr::FieldAccess(b, _) => walk(b, entity, member),
            Expr::Index(b, k) => walk(b, entity, member) || walk(k, entity, member),
            Expr::If(c, t, e) => {
                walk(c, entity, member)
                    || walk(t, entity, member)
                    || e.as_ref().map_or(false, |x| walk(x, entity, member))
            }
            Expr::Let(p, v, b) => {
                // After Batch G2 the codegen substitutes HashMap-typed
                // let-bindings, but at detection time we walk the raw
                // AST: do an in-AST substitution if we recognize the
                // shape so we don't miss `let inner = m[k];
                // inner.exists(x)`.
                if let Pattern::Ident(name) = p {
                    let init = simplify_hashmap_let_alias_init(v).unwrap_or_else(|| (**v).clone());
                    if is_hashmap_valued_expr(&init, entity) {
                        let substituted = subst_hashmap_let_in_expr(b, name, &init);
                        return walk(&substituted, entity, member);
                    }
                    if let Expr::Index(base, _) = &init {
                        if matches!(base.as_ref(), Expr::Ident(n) if n == member) {
                            let substituted = subst_hashmap_let_in_expr(b, name, &init);
                            return walk(&substituted, entity, member);
                        }
                    }
                }
                walk(v, entity, member) || walk(b, entity, member)
            }
            Expr::Block(items) => items.iter().any(|e| walk(e, entity, member)),
            Expr::FnCall(_, args) | Expr::MacroRef(_, args) => {
                args.iter().any(|a| walk(a, entity, member))
            }
            Expr::Cast(e, _) => walk(e, entity, member),
            Expr::Tuple(es) => es.iter().any(|e| walk(e, entity, member)),
            Expr::Match(s, arms) => {
                walk(s, entity, member)
                    || arms.iter().any(|a| walk(&a.body, entity, member))
            }
            Expr::Some(e) => walk(e, entity, member),
            Expr::Range(s, e) => walk(s, entity, member) || walk(e, entity, member),
            Expr::For(_, it, body) => walk(it, entity, member) || walk(body, entity, member),
            Expr::Closure(_, b) => walk(b, entity, member),
            Expr::EnumVariantWithData(_, _, args) => {
                args.iter().any(|a| walk(a, entity, member))
            }
            _ => false,
        }
    }

    entity_walks_any(entity, |e| walk(e, entity, member_name))
        || routes_inner_exist_on_member_via_lets(entity, member_name)
}

fn routes_inner_exist_on_member_via_lets(entity: &Entity, member_name: &str) -> bool {
    for route in &entity.routes {
        let mut aliases: StdHashMap<String, Expr> = StdHashMap::new();
        for action in route.body.all_actions() {
            if let RouteAction::Let {
                pattern: Pattern::Ident(let_name),
                value,
            } = action
            {
                let init =
                    simplify_hashmap_let_alias_init(value).unwrap_or_else(|| value.clone());
                if is_hashmap_valued_expr(&init, entity) {
                    aliases.insert(let_name.clone(), init);
                }
            } else if action_walks_any(action, &|e| {
                inner_exists_on_member_via_aliases(e, member_name, &aliases)
            }) {
                return true;
            }
        }
    }
    false
}

fn inner_exists_on_member_via_aliases(
    expr: &Expr,
    member_name: &str,
    aliases: &StdHashMap<String, Expr>,
) -> bool {
    match expr {
        Expr::MethodCall(base, method, args) if method == "exists" || method == "contains" => {
            let resolved = if let Expr::Ident(n) = base.as_ref() {
                aliases.get(n).map_or(base.as_ref(), |a| a)
            } else {
                base.as_ref()
            };
            if let Expr::Index(inner, _) = resolved {
                return matches!(inner.as_ref(), Expr::Ident(n) if n == member_name);
            }
            inner_exists_on_member_via_aliases(base, member_name, aliases)
                || args.iter().any(|a| inner_exists_on_member_via_aliases(a, member_name, aliases))
        }
        Expr::MethodCall(base, _, args) => {
            inner_exists_on_member_via_aliases(base, member_name, aliases)
                || args.iter().any(|a| inner_exists_on_member_via_aliases(a, member_name, aliases))
        }
        Expr::BinOp(l, _, r) => {
            inner_exists_on_member_via_aliases(l, member_name, aliases)
                || inner_exists_on_member_via_aliases(r, member_name, aliases)
        }
        Expr::UnaryOp(_, e) => inner_exists_on_member_via_aliases(e, member_name, aliases),
        Expr::FieldAccess(b, _) => inner_exists_on_member_via_aliases(b, member_name, aliases),
        Expr::Index(b, k) => {
            inner_exists_on_member_via_aliases(b, member_name, aliases)
                || inner_exists_on_member_via_aliases(k, member_name, aliases)
        }
        Expr::If(c, t, e) => {
            inner_exists_on_member_via_aliases(c, member_name, aliases)
                || inner_exists_on_member_via_aliases(t, member_name, aliases)
                || e.as_ref()
                    .map_or(false, |x| inner_exists_on_member_via_aliases(x, member_name, aliases))
        }
        Expr::Let(_, v, b) => {
            inner_exists_on_member_via_aliases(v, member_name, aliases)
                || inner_exists_on_member_via_aliases(b, member_name, aliases)
        }
        Expr::Block(items) => items
            .iter()
            .any(|e| inner_exists_on_member_via_aliases(e, member_name, aliases)),
        Expr::FnCall(_, args) | Expr::MacroRef(_, args) => args
            .iter()
            .any(|a| inner_exists_on_member_via_aliases(a, member_name, aliases)),
        Expr::Cast(e, _) => inner_exists_on_member_via_aliases(e, member_name, aliases),
        Expr::Tuple(es) => es
            .iter()
            .any(|e| inner_exists_on_member_via_aliases(e, member_name, aliases)),
        Expr::Match(s, arms) => {
            inner_exists_on_member_via_aliases(s, member_name, aliases)
                || arms
                    .iter()
                    .any(|a| inner_exists_on_member_via_aliases(&a.body, member_name, aliases))
        }
        Expr::Some(e) => inner_exists_on_member_via_aliases(e, member_name, aliases),
        Expr::Range(s, e) => {
            inner_exists_on_member_via_aliases(s, member_name, aliases)
                || inner_exists_on_member_via_aliases(e, member_name, aliases)
        }
        Expr::For(_, it, b) => {
            inner_exists_on_member_via_aliases(it, member_name, aliases)
                || inner_exists_on_member_via_aliases(b, member_name, aliases)
        }
        Expr::Closure(_, b) => inner_exists_on_member_via_aliases(b, member_name, aliases),
        Expr::EnumVariantWithData(_, _, args) => args
            .iter()
            .any(|a| inner_exists_on_member_via_aliases(a, member_name, aliases)),
        Expr::NamespacedCall { args, .. } => args
            .iter()
            .any(|a| inner_exists_on_member_via_aliases(a, member_name, aliases)),
        Expr::AddressOf { args, with_params, .. } => {
            args.iter().any(|a| inner_exists_on_member_via_aliases(a, member_name, aliases))
                || with_params
                    .iter()
                    .any(|(_, e)| inner_exists_on_member_via_aliases(e, member_name, aliases))
        }
        Expr::Encode { value, .. } => inner_exists_on_member_via_aliases(value, member_name, aliases),
        _ => false,
    }
}

/// EVM-3 Batch G1 — does any expression *anywhere reachable from the
/// entity* call `.exists(...)` against the named HashMap member? This
/// is the source of truth for whether the `mapping(K => bool)
/// <m>_exists` companion storage gets emitted alongside the mapping.
///
/// Walks routes (bodies + where clauses), member transforms + defaults,
/// and pure functions called from within the entity. The pure-fn arm
/// closes the long-standing miss for shapes like
/// `pure fn balance_of(balances: HashMap<...>, owner) -> U256 {
///     if balances.exists(owner) { balances[owner] } else { 0 }
/// }` invoked as `balance_of(m_balances, sender)`: by inspecting the
/// pure fn's body we propagate "needs exists" from the parameter back
/// to the actual entity member at the call site.
pub(super) fn member_uses_exists(entity: &Entity, program: &Program, member_name: &str) -> bool {
    fn walk_expr(expr: &Expr, member_name: &str, program: &Program) -> bool {
        match expr {
            // Phase EVM-15 H3 (Cluster B): `m.contains(k)` shares
            // the `_exists` sidecar with `m.exists(k)`, so trigger
            // the same emission path.
            Expr::MethodCall(base, method, args) if method == "exists" || method == "contains" => {
                if matches!(base.as_ref(), Expr::Ident(n) if n == member_name) {
                    return true;
                }
                walk_expr(base, member_name, program)
                    || args.iter().any(|a| walk_expr(a, member_name, program))
            }
            Expr::MethodCall(base, _, args) => {
                walk_expr(base, member_name, program)
                    || args.iter().any(|a| walk_expr(a, member_name, program))
            }
            Expr::FnCall(name, args) => {
                if pure_fn_uses_exists_on_member(name, args, member_name, program) {
                    return true;
                }
                args.iter().any(|a| walk_expr(a, member_name, program))
            }
            Expr::MacroRef(_, args) => args.iter().any(|a| walk_expr(a, member_name, program)),
            Expr::BinOp(l, _, r) => {
                walk_expr(l, member_name, program) || walk_expr(r, member_name, program)
            }
            Expr::UnaryOp(_, e) => walk_expr(e, member_name, program),
            Expr::FieldAccess(b, _) => walk_expr(b, member_name, program),
            Expr::Index(b, k) => {
                walk_expr(b, member_name, program) || walk_expr(k, member_name, program)
            }
            Expr::If(c, t, e) => {
                walk_expr(c, member_name, program)
                    || walk_expr(t, member_name, program)
                    || e.as_ref()
                        .map_or(false, |x| walk_expr(x, member_name, program))
            }
            Expr::Let(_, val, body) => {
                walk_expr(val, member_name, program) || walk_expr(body, member_name, program)
            }
            Expr::Block(items) => items.iter().any(|e| walk_expr(e, member_name, program)),
            Expr::RecordConstruct(_, fields) => fields
                .iter()
                .any(|(_, e)| walk_expr(e, member_name, program)),
            Expr::RecordUpdate(b, fields) => {
                walk_expr(b, member_name, program)
                    || fields
                        .iter()
                        .any(|(_, e)| walk_expr(e, member_name, program))
            }
            Expr::Closure(_, b) => walk_expr(b, member_name, program),
            Expr::Cast(e, _) => walk_expr(e, member_name, program),
            Expr::Tuple(es) => es.iter().any(|e| walk_expr(e, member_name, program)),
            Expr::Match(s, arms) => {
                walk_expr(s, member_name, program)
                    || arms
                        .iter()
                        .any(|a| walk_expr(&a.body, member_name, program))
            }
            Expr::EnumVariantWithData(_, _, args) => {
                args.iter().any(|a| walk_expr(a, member_name, program))
            }
            Expr::Some(e) => walk_expr(e, member_name, program),
            Expr::Range(s, e) => {
                walk_expr(s, member_name, program) || walk_expr(e, member_name, program)
            }
            Expr::For(_, it, body) => {
                walk_expr(it, member_name, program) || walk_expr(body, member_name, program)
            }
            Expr::NamespacedCall { args, .. } => {
                args.iter().any(|a| walk_expr(a, member_name, program))
            }
            Expr::AddressOf {
                args, with_params, ..
            } => {
                args.iter().any(|a| walk_expr(a, member_name, program))
                    || with_params
                        .iter()
                        .any(|(_, e)| walk_expr(e, member_name, program))
            }
            Expr::Encode { value, .. } => walk_expr(value, member_name, program),
            _ => false,
        }
    }

    /// EVM-3 Batch G1: cross-pure-fn detection. A call
    /// `f(m_balances, ...)` propagates an `_exists` requirement onto
    /// `m_balances` if and only if the pure fn `f` itself calls
    /// `.exists(...)` on the parameter that aligns with `m_balances`'s
    /// argument position. We restrict to bare `Expr::Ident(name)`
    /// arguments — any expression-shaped argument is a let-binding /
    /// computed reshape that V30 rejects upfront.
    fn pure_fn_uses_exists_on_member(
        fn_name: &str,
        args: &[Expr],
        member_name: &str,
        program: &Program,
    ) -> bool {
        let pf = match program.pure_fns.iter().find(|p| p.name == fn_name) {
            Some(p) => p,
            None => return false,
        };
        for (i, arg) in args.iter().enumerate() {
            let matches_member = matches!(arg, Expr::Ident(n) if n == member_name);
            if !matches_member {
                continue;
            }
            let param_name = match pf.params.get(i) {
                Some(p) => &p.name,
                None => continue,
            };
            if param_uses_exists_in_pure_fn(pf, param_name) {
                return true;
            }
        }
        false
    }

    fn param_uses_exists_in_pure_fn(pf: &PureFn, param_name: &str) -> bool {
        // Re-uses the same walk_expr but with the program parameter
        // not threaded through, since pure fns can't transitively
        // call other pure fns with an exists-on-HashMap-param shape
        // (would be rejected by V31 anyway).
        fn walk(e: &Expr, name: &str) -> bool {
            match e {
                Expr::MethodCall(base, method, args) if method == "exists" => {
                    if matches!(base.as_ref(), Expr::Ident(n) if n == name) {
                        return true;
                    }
                    walk(base, name) || args.iter().any(|a| walk(a, name))
                }
                Expr::MethodCall(base, _, args) => {
                    walk(base, name) || args.iter().any(|a| walk(a, name))
                }
                Expr::BinOp(l, _, r) => walk(l, name) || walk(r, name),
                Expr::UnaryOp(_, x) => walk(x, name),
                Expr::FieldAccess(b, _) => walk(b, name),
                Expr::Index(b, k) => walk(b, name) || walk(k, name),
                Expr::If(c, t, e) => {
                    walk(c, name) || walk(t, name) || e.as_ref().map_or(false, |x| walk(x, name))
                }
                Expr::Let(_, v, b) => walk(v, name) || walk(b, name),
                Expr::Block(items) => items.iter().any(|e| walk(e, name)),
                Expr::FnCall(_, args) => args.iter().any(|a| walk(a, name)),
                Expr::Cast(e, _) => walk(e, name),
                Expr::Tuple(es) => es.iter().any(|e| walk(e, name)),
                Expr::Match(s, arms) => walk(s, name) || arms.iter().any(|a| walk(&a.body, name)),
                Expr::Some(e) => walk(e, name),
                Expr::Range(s, e) => walk(s, name) || walk(e, name),
                _ => false,
            }
        }
        walk(&pf.body, param_name)
    }

    entity_walks_any(entity, |e| walk_expr(e, member_name, program))
}

/// EVM-13: Phase EVM-13 — does any expression *anywhere in the entity*
/// (route bodies, route `where` clauses, member transforms, member
/// defaults, route precondition checks) iterate the named HashMap
/// member via `.keys()`, `.values()`, or `.iter()`? When this returns
/// true the entity-contract emission block adds the parallel `K[]
/// <m>_keys` storage + `mapping(K => uint256) <m>_keys_index` lookup,
/// and the mapping transform pipeline injects the matching
/// push-if-new / swap-pop maintenance into every `insert` / `update` /
/// `remove` against the member.
///
/// We deliberately do *not* gate this on `is_mapping_type` here — the
/// caller already knows the member is a HashMap. Callers should still
/// short-circuit on non-mapping members so we don't waste cycles.
pub(super) fn member_is_iterated(
    entity: &Entity,
    member_name: &str,
    keys_map: &HashMap<String, Vec<(usize, String)>>,
) -> bool {
    fn is_iter_method(name: &str) -> bool {
        matches!(name, "keys" | "values" | "iter")
    }
    /// EVM-12 Batch B + EVM-13 Batch D: iterator-chain methods. When
    /// any of these is invoked directly on a bare HashMap-member
    /// `Ident`, the chain fuser treats the member as if `.iter()`
    /// (or `.values()` for `values`) had been written, so we must
    /// emit the parallel-keys companion the same way.
    fn is_chain_method(name: &str) -> bool {
        matches!(
            name,
            "fold" | "collect" | "filter" | "map" | "take" | "enumerate"
        )
    }

    fn walk_expr(
        expr: &Expr,
        member_name: &str,
        keys_map: &HashMap<String, Vec<(usize, String)>>,
    ) -> bool {
        match expr {
            Expr::FnCall(name, args) => {
                if callee_needs_keys_for_member_arg(name, name, args, member_name, keys_map) {
                    return true;
                }
                args.iter().any(|a| walk_expr(a, member_name, keys_map))
            }
            Expr::NamespacedCall {
                namespace,
                name,
                args,
                ..
            } => {
                let qualified = format!("{}::{}", namespace, name);
                if callee_needs_keys_for_member_arg(
                    &qualified,
                    name,
                    args,
                    member_name,
                    keys_map,
                ) {
                    return true;
                }
                args.iter().any(|a| walk_expr(a, member_name, keys_map))
            }
            Expr::MethodCall(base, method, args) => {
                // Phase EVM-15 H3 (Cluster B): `m.is_empty()` lowers
                // to `m_keys.length == 0`, which requires the keys
                // companion. Treat it as iteration so the companion
                // storage gets emitted.
                if matches!(method.as_str(), "is_empty" | "len" | "length") && args.is_empty() {
                    if let Expr::Ident(n) = base.as_ref() {
                        if n == member_name {
                            return true;
                        }
                    }
                }
                if (is_iter_method(method) && args.is_empty()) || is_chain_method(method) {
                    if let Expr::Ident(n) = base.as_ref() {
                        if n == member_name {
                            return true;
                        }
                    }
                }
                walk_expr(base, member_name, keys_map)
                    || args.iter().any(|a| walk_expr(a, member_name, keys_map))
            }
            Expr::For(_, it, body) => {
                // `for <pat> in <member>` is sugar for `<member>.iter()`
                // (Batch D); mark the member as iterated so its
                // companion storage is emitted.
                let iter_match = matches!(it.as_ref(), Expr::Ident(n) if n == member_name);
                iter_match
                    || walk_expr(it, member_name, keys_map)
                    || walk_expr(body, member_name, keys_map)
            }
            Expr::BinOp(l, _, r) => {
                walk_expr(l, member_name, keys_map) || walk_expr(r, member_name, keys_map)
            }
            Expr::UnaryOp(_, e) => walk_expr(e, member_name, keys_map),
            Expr::FieldAccess(b, _) => walk_expr(b, member_name, keys_map),
            Expr::Index(b, k) => {
                walk_expr(b, member_name, keys_map) || walk_expr(k, member_name, keys_map)
            }
            Expr::MacroRef(_, args) => args.iter().any(|a| walk_expr(a, member_name, keys_map)),
            Expr::If(c, t, e) => {
                walk_expr(c, member_name, keys_map)
                    || walk_expr(t, member_name, keys_map)
                    || e.as_ref()
                        .map_or(false, |x| walk_expr(x, member_name, keys_map))
            }
            Expr::Let(_, v, b) => {
                walk_expr(v, member_name, keys_map) || walk_expr(b, member_name, keys_map)
            }
            Expr::Block(items) => items.iter().any(|e| walk_expr(e, member_name, keys_map)),
            Expr::RecordConstruct(_, fields) => {
                fields
                    .iter()
                    .any(|(_, e)| walk_expr(e, member_name, keys_map))
            }
            Expr::RecordUpdate(b, fields) => {
                walk_expr(b, member_name, keys_map)
                    || fields
                        .iter()
                        .any(|(_, e)| walk_expr(e, member_name, keys_map))
            }
            Expr::Closure(_, b) => walk_expr(b, member_name, keys_map),
            Expr::Cast(e, _) => walk_expr(e, member_name, keys_map),
            Expr::Tuple(es) => es.iter().any(|e| walk_expr(e, member_name, keys_map)),
            Expr::Match(s, arms) => {
                walk_expr(s, member_name, keys_map)
                    || arms
                        .iter()
                        .any(|a| walk_expr(&a.body, member_name, keys_map))
            }
            Expr::EnumVariantWithData(lib, fn_name, args) => {
                let qualified = format!("{}::{}", lib, fn_name);
                if callee_needs_keys_for_member_arg(
                    &qualified,
                    fn_name,
                    args,
                    member_name,
                    keys_map,
                ) {
                    return true;
                }
                args.iter().any(|a| walk_expr(a, member_name, keys_map))
            }
            Expr::Some(e) => walk_expr(e, member_name, keys_map),
            Expr::Range(s, e) => {
                walk_expr(s, member_name, keys_map) || walk_expr(e, member_name, keys_map)
            }
            Expr::AddressOf {
                args, with_params, ..
            } => {
                args.iter().any(|a| walk_expr(a, member_name, keys_map))
                    || with_params
                        .iter()
                        .any(|(_, e)| walk_expr(e, member_name, keys_map))
            }
            Expr::Encode { value, .. } => walk_expr(value, member_name, keys_map),
            _ => false,
        }
    }

    entity_walks_any(entity, |e| walk_expr(e, member_name, keys_map))
        || routes_iterate_member_via_let_aliases(entity, member_name)
}

fn hashmap_insert_alias_member(value: &Expr) -> Option<&str> {
    if let Expr::MethodCall(base, method, args) = value {
        if (method == "insert" || method == "update") && args.len() == 2 {
            if let Expr::Ident(map_name) = base.as_ref() {
                return Some(map_name);
            }
        }
    }
    None
}

fn expr_iterates_member_via_aliases(
    expr: &Expr,
    member_name: &str,
    aliases: &StdHashMap<String, Expr>,
) -> bool {
    fn is_iter_method(name: &str) -> bool {
        matches!(name, "keys" | "values" | "iter")
    }
    fn is_chain_method(name: &str) -> bool {
        matches!(
            name,
            "fold" | "collect" | "filter" | "map" | "take" | "enumerate"
        )
    }
    match expr {
        Expr::MethodCall(base, method, args) => {
            let resolved = if let Expr::Ident(n) = base.as_ref() {
                aliases.get(n).map_or(base.as_ref(), |a| a)
            } else {
                base.as_ref()
            };
            if method == "is_empty" && args.is_empty() {
                if matches!(resolved, Expr::Ident(n) if n == member_name) {
                    return true;
                }
            }
            if (is_iter_method(method) && args.is_empty()) || is_chain_method(method) {
                if matches!(resolved, Expr::Ident(n) if n == member_name) {
                    return true;
                }
            }
            expr_iterates_member_via_aliases(base, member_name, aliases)
                || args.iter().any(|a| expr_iterates_member_via_aliases(a, member_name, aliases))
        }
        Expr::BinOp(l, _, r) => {
            expr_iterates_member_via_aliases(l, member_name, aliases)
                || expr_iterates_member_via_aliases(r, member_name, aliases)
        }
        Expr::UnaryOp(_, e) => expr_iterates_member_via_aliases(e, member_name, aliases),
        Expr::FieldAccess(b, _) => expr_iterates_member_via_aliases(b, member_name, aliases),
        Expr::Index(b, k) => {
            expr_iterates_member_via_aliases(b, member_name, aliases)
                || expr_iterates_member_via_aliases(k, member_name, aliases)
        }
        Expr::If(c, t, e) => {
            expr_iterates_member_via_aliases(c, member_name, aliases)
                || expr_iterates_member_via_aliases(t, member_name, aliases)
                || e.as_ref()
                    .map_or(false, |x| expr_iterates_member_via_aliases(x, member_name, aliases))
        }
        Expr::Let(_, v, b) => {
            expr_iterates_member_via_aliases(v, member_name, aliases)
                || expr_iterates_member_via_aliases(b, member_name, aliases)
        }
        Expr::Block(items) => items
            .iter()
            .any(|e| expr_iterates_member_via_aliases(e, member_name, aliases)),
        Expr::FnCall(_, args) | Expr::MacroRef(_, args) => args
            .iter()
            .any(|a| expr_iterates_member_via_aliases(a, member_name, aliases)),
        Expr::Cast(e, _) => expr_iterates_member_via_aliases(e, member_name, aliases),
        Expr::Tuple(es) => es
            .iter()
            .any(|e| expr_iterates_member_via_aliases(e, member_name, aliases)),
        Expr::Match(s, arms) => {
            expr_iterates_member_via_aliases(s, member_name, aliases)
                || arms
                    .iter()
                    .any(|a| expr_iterates_member_via_aliases(&a.body, member_name, aliases))
        }
        Expr::Some(e) => expr_iterates_member_via_aliases(e, member_name, aliases),
        Expr::Range(s, e) => {
            expr_iterates_member_via_aliases(s, member_name, aliases)
                || expr_iterates_member_via_aliases(e, member_name, aliases)
        }
        Expr::For(_, it, b) => {
            expr_iterates_member_via_aliases(it, member_name, aliases)
                || expr_iterates_member_via_aliases(b, member_name, aliases)
        }
        Expr::Closure(_, b) => expr_iterates_member_via_aliases(b, member_name, aliases),
        Expr::EnumVariantWithData(_, _, args) => args
            .iter()
            .any(|a| expr_iterates_member_via_aliases(a, member_name, aliases)),
        Expr::NamespacedCall { args, .. } => args
            .iter()
            .any(|a| expr_iterates_member_via_aliases(a, member_name, aliases)),
        Expr::AddressOf { args, with_params, .. } => {
            args.iter().any(|a| expr_iterates_member_via_aliases(a, member_name, aliases))
                || with_params
                    .iter()
                    .any(|(_, e)| expr_iterates_member_via_aliases(e, member_name, aliases))
        }
        Expr::Encode { value, .. } => expr_iterates_member_via_aliases(value, member_name, aliases),
        _ => false,
    }
}

fn routes_iterate_member_via_let_aliases(entity: &Entity, member_name: &str) -> bool {
    for route in &entity.routes {
        let mut aliases: StdHashMap<String, Expr> = StdHashMap::new();
        for action in route.body.all_actions() {
            if let RouteAction::Let {
                pattern: Pattern::Ident(let_name),
                value,
            } = action
            {
                if let Some(map_name) = hashmap_insert_alias_member(value) {
                    if map_name == member_name {
                        aliases.insert(let_name.clone(), Expr::Ident(member_name.to_string()));
                    }
                }
            } else if action_walks_any(action, &|e| {
                expr_iterates_member_via_aliases(e, member_name, &aliases)
            }) {
                return true;
            }
        }
    }
    false
}

fn callee_needs_keys_for_member_arg(
    callee_key: &str,
    callee_short: &str,
    args: &[Expr],
    member_name: &str,
    keys_map: &HashMap<String, Vec<(usize, String)>>,
) -> bool {
    let suffix = format!("::{}", callee_short);
    let mut suffix_hit: Option<&Vec<(usize, String)>> = None;
    for (key, hits) in keys_map {
        if key.ends_with(&suffix) {
            if suffix_hit.is_some() {
                suffix_hit = None;
                break;
            }
            suffix_hit = Some(hits);
        }
    }
    let hits = keys_map
        .get(callee_key)
        .or_else(|| keys_map.get(callee_short))
        .or(suffix_hit);
    match hits {
        Some(hits) => args.iter().enumerate().any(|(i, arg)| {
            matches!(arg, Expr::Ident(n) if n == member_name)
                && hits.iter().any(|(idx, _)| *idx == i)
        }),
        None => false,
    }
}

/// Walk an expression for pure-fn / iterator uses that require `{member}`'s
/// parallel `_keys` sidecar (catalog invariant checks included).
fn expr_needs_hashmap_keys_sidecar(
    expr: &Expr,
    member_name: &str,
    keys_map: &HashMap<String, Vec<(usize, String)>>,
) -> bool {
    match expr {
        Expr::FnCall(name, args) => {
            if callee_needs_keys_for_member_arg(name, name, args, member_name, keys_map) {
                return true;
            }
            args.iter()
                .any(|a| expr_needs_hashmap_keys_sidecar(a, member_name, keys_map))
        }
        Expr::NamespacedCall {
            namespace,
            name,
            args,
            ..
        } => {
            let qualified = format!("{}::{}", namespace, name);
            if callee_needs_keys_for_member_arg(&qualified, name, args, member_name, keys_map) {
                return true;
            }
            args.iter()
                .any(|a| expr_needs_hashmap_keys_sidecar(a, member_name, keys_map))
        }
        Expr::BinOp(l, _, r) => {
            expr_needs_hashmap_keys_sidecar(l, member_name, keys_map)
                || expr_needs_hashmap_keys_sidecar(r, member_name, keys_map)
        }
        Expr::UnaryOp(_, e) => expr_needs_hashmap_keys_sidecar(e, member_name, keys_map),
        Expr::MethodCall(recv, _, args) => {
            expr_needs_hashmap_keys_sidecar(recv, member_name, keys_map)
                || args
                    .iter()
                    .any(|a| expr_needs_hashmap_keys_sidecar(a, member_name, keys_map))
        }
        Expr::If(c, t, e) => {
            expr_needs_hashmap_keys_sidecar(c, member_name, keys_map)
                || expr_needs_hashmap_keys_sidecar(t, member_name, keys_map)
                || e.as_ref()
                    .map_or(false, |x| expr_needs_hashmap_keys_sidecar(x, member_name, keys_map))
        }
        Expr::Let(_, v, b) => {
            expr_needs_hashmap_keys_sidecar(v, member_name, keys_map)
                || expr_needs_hashmap_keys_sidecar(b, member_name, keys_map)
        }
        Expr::Block(items) => items
            .iter()
            .any(|e| expr_needs_hashmap_keys_sidecar(e, member_name, keys_map)),
        Expr::FieldAccess(_, field) if field == member_name => true,
        Expr::FieldAccess(b, _) => expr_needs_hashmap_keys_sidecar(b, member_name, keys_map),
        Expr::Index(b, k) => {
            expr_needs_hashmap_keys_sidecar(b, member_name, keys_map)
                || expr_needs_hashmap_keys_sidecar(k, member_name, keys_map)
        }
        _ => false,
    }
}

fn program_invariants_iterate_member(
    program: &Program,
    entity_name: &str,
    member_name: &str,
    keys_map: &HashMap<String, Vec<(usize, String)>>,
) -> bool {
    program.invariants.iter().any(|inv| {
        inv.instances
            .iter()
            .any(|i| i.entity == entity_name)
            && inv
                .checks
                .iter()
                .any(|check| expr_needs_hashmap_keys_sidecar(check, member_name, keys_map))
    })
}

/// Keys for [`EvmCtx::hashmap_iterated_members`]: `"{entity}::{member}"` for
/// every HashMap member that needs a parallel `_keys` array.
pub(crate) fn build_hashmap_iterated_member_keys(
    program: &Program,
    keys_map: &HashMap<String, Vec<(usize, String)>>,
) -> HashSet<String> {
    let mut keys = HashSet::new();
    for entity in &program.entities {
        for member in &entity.members {
            if matches!(&member.ty, Type::Generic(g, _) if g == "HashMap") {
                if member_is_iterated(entity, &member.name, keys_map)
                    || program_invariants_iterate_member(
                        program,
                        &entity.name,
                        &member.name,
                        keys_map,
                    )
                {
                    keys.insert(format!("{}::{}", entity.name, member.name));
                }
            }
        }
    }
    keys
}

/// Keys for [`EvmCtx::hashmap_exists_sidecar`]: `"{entity}::{member}"` for
/// every HashMap member that gets a `mapping(K => bool) {m}_exists` slot.
pub(crate) fn build_hashmap_exists_sidecar_keys(
    program: &Program,
    keys_map: &HashMap<String, Vec<(usize, String)>>,
) -> HashSet<String> {
    let mut keys = HashSet::new();
    for entity in &program.entities {
        for member in &entity.members {
            if matches!(&member.ty, Type::Generic(g, _) if g == "HashMap") {
                if member_is_iterated(entity, &member.name, keys_map)
                    || program_invariants_iterate_member(
                        program,
                        &entity.name,
                        &member.name,
                        keys_map,
                    )
                    || member_uses_exists(entity, program, &member.name)
                {
                    keys.insert(format!("{}::{}", entity.name, member.name));
                }
            }
        }
    }
    keys
}

/// Backwards-compatible shim around the narrow transform-only walker
/// retained for `storage_layout::compute_layout`, which only sees an
/// entity (no `Program` available). Cross-pure-fn detection happens
/// in `member_uses_exists`; under-detection here can at worst leave a
/// storage slot unallocated for a sidecar that the codegen will still
/// emit, which would only matter for a hypothetical revm test that
/// directly pokes the sidecar's slot. None of our current revm
/// fixtures do that.
pub(crate) fn transforms_use_exists(members: &[Member], member_name: &str) -> bool {
    fn walk(expr: &Expr, member_name: &str) -> bool {
        match expr {
            Expr::MethodCall(base, method, _) if method == "exists" => {
                matches!(base.as_ref(), Expr::Ident(n) if n == member_name)
            }
            Expr::BinOp(l, _, r) => walk(l, member_name) || walk(r, member_name),
            Expr::Let(_, val, body) => walk(val, member_name) || walk(body, member_name),
            Expr::If(c, t, e) => {
                walk(c, member_name)
                    || walk(t, member_name)
                    || e.as_ref().map_or(false, |e| walk(e, member_name))
            }
            Expr::MethodCall(base, _, args) => {
                walk(base, member_name) || args.iter().any(|a| walk(a, member_name))
            }
            Expr::FnCall(_, args) => args.iter().any(|a| walk(a, member_name)),
            _ => false,
        }
    }
    members
        .iter()
        .any(|m| m.transforms.iter().any(|t| walk(&t.body, member_name)))
}

// ---------------------------------------------------------------------------
// Phase EVM-P0-A: per-slot tuple element type inference for `let (a, b, c)`
// destructure (closes EVM_GAPS § 1.12).
// ---------------------------------------------------------------------------
//
// `EvmActionEmitter::emit_let` previously hardcoded `uint256` for every
// tuple slot, so `let (owner, count, paused) = readState();` lost the
// `address` / `uint256` / `bool` distinction and Solidity rejected the
// destructure when the function actually returned a non-uint256 head.
//
// Recognised sources (each component returns a Solidity type string;
// unknown shapes default to `"uint256"` and the matching call site keeps
// the existing widening behaviour):
//
//   * `Expr::Tuple([e1, e2, ...])` — recurse per element using
//     `infer_let_type_entity` (which already handles bool / address /
//     records / pure-fn returns / etc.).
//   * `Expr::FnCall(name, _)` of a known program-level pure fn whose
//     return type is `Type::Tuple(items)` — read from the
//     `PURE_FN_RETURNS` registry and lower each component via
//     `sol_type_entity` for record / enum awareness.
//   * `Expr::FnCall("divmod", _)` (and any future stdlib helper that
//     returns a tuple) — pre-baked tuple types, since stdlib helpers
//     aren't in `program.pure_fns`.
//   * `Expr::If(_, then, else)` — propagate the then-branch's tuple
//     types (matches `infer_let_type_entity`'s policy for `If`).
//
// Returns `Some(Vec<String>)` only when *every* slot has a confidently
// inferred type; otherwise returns `None` so the caller falls back to
// the per-slot `uint256` default (preserved behaviour).
