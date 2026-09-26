// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean P4c — `HashMap<K,V>` expression and member-transform lowering.

use crate::ast::{Entity, Expr, Member};

use super::super::expr::{gen_expr, LeanExprCtx};
use super::map_analysis::{member_is_hashmap, member_needs_keys_list};

pub fn map_member_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(name) => Some(name.clone()),
        Expr::MethodCall(base, _, _) | Expr::Index(base, _) => map_member_name(base),
        _ => None,
    }
}

/// True when `name` resolves to a `HashMap`-typed binding in the
/// current context — either an entity state member, or a route /
/// pure-fn parameter / `let` whose type the caller marked as map.
pub(super) fn ident_is_hashmap(name: &str, ctx: &LeanExprCtx<'_>) -> bool {
    if ctx
        .entity
        .members
        .iter()
        .any(|m| m.name == name && member_is_hashmap(m))
    {
        return true;
    }
    ctx.hashmap_idents.contains(name)
}

fn member_map_access(member: &str, ctx: &LeanExprCtx<'_>) -> String {
    if ctx.in_transform {
        format!("{}.{}", ctx.state_var, member)
    } else {
        format!("{}.{}", ctx.state_var, member)
    }
}

fn member_keys_access(member: &str, ctx: &LeanExprCtx<'_>) -> String {
    format!("{}.{}_keys", ctx.state_var, member)
}

/// Lower `EmptyCollection` / `{}` for a known HashMap member context.
pub fn gen_empty_map() -> String {
    "Cambrian.AddressMap.empty".to_string()
}

/// Try to lower `m[k]` when base is a HashMap member or map-typed ident.
pub fn try_gen_map_index(base: &Expr, idx: &Expr, ctx: &LeanExprCtx<'_>) -> Option<String> {
    let name = match base {
        Expr::TemporalRef(name) => name.clone(),
        _ => map_member_name(base)?,
    };
    if !ident_is_hashmap(&name, ctx) {
        return None;
    }
    let map = gen_expr(base, ctx);
    let k = gen_expr(idx, ctx);
    Some(format!(
        "(Cambrian.AddressMap.lookup {} {} |>.get!)",
        map, k
    ))
}

/// Lower `.insert` / `.update` / `.remove` / membership / keys on a map receiver.
pub fn try_gen_map_method_call(
    base: &Expr,
    method: &str,
    args: &[Expr],
    ctx: &LeanExprCtx<'_>,
) -> Option<String> {
    let mut is_state_member = false;
    let map = match base {
        // Chained `{}.insert(k, v)` / `{}.update(...)` shapes in map
        // member transforms: the receiver is a literal empty map.
        Expr::EmptyCollection => "Cambrian.AddressMap.empty".to_string(),
        // Chained `m.update(k1, v1).update(k2, v2)` shapes: the inner
        // method-call also returns a map, so we recurse and use its
        // lowered term as the receiver of the outer call. Without
        // this branch, only the *last* `.update(...)` in a chain
        // survives the lowering — earlier updates get silently
        // dropped, which corrupts every transfer/burn/delegate that
        // touches two map entries.
        Expr::MethodCall(inner_base, inner_method, inner_args) => {
            if let Some(inner_term) =
                try_gen_map_method_call(inner_base, inner_method, inner_args, ctx)
            {
                let name = map_member_name(base).unwrap_or_default();
                is_state_member = ctx
                    .entity
                    .members
                    .iter()
                    .any(|m| m.name == name && member_is_hashmap(m));
                format!("({})", inner_term)
            } else {
                return None;
            }
        }
        _ => {
            let name = map_member_name(base)?;
            if !ident_is_hashmap(&name, ctx) {
                return None;
            }
            // For state members we use the `s.<name>` projection (so
            // unphased transform threading stays correct); for plain
            // idents (pure-fn params, `let` bindings) we use the
            // ident name directly.
            is_state_member = ctx
                .entity
                .members
                .iter()
                .any(|m| m.name == name && member_is_hashmap(m));
            if is_state_member {
                member_map_access(&name, ctx)
            } else {
                gen_expr(base, ctx)
            }
        }
    };
    // Bind a name to thread the existing `member_needs_keys_list`
    // lookup path below.
    let name = map_member_name(base).unwrap_or_default();
    match method {
        "insert" | "update" if args.len() == 2 => {
            let k = gen_expr(&args[0], ctx);
            let v = gen_expr(&args[1], ctx);
            Some(format!("Cambrian.AddressMap.insert {} {} {}", map, k, v))
        }
        "remove" if args.len() == 1 => {
            let k = gen_expr(&args[0], ctx);
            Some(format!("Cambrian.AddressMap.remove {} {}", map, k))
        }
        "keys" if args.is_empty() => {
            if is_state_member && member_needs_keys_list(ctx.entity, &name) {
                Some(member_keys_access(&name, ctx))
            } else {
                Some(format!("Cambrian.AddressMap.keys {}", map))
            }
        }
        "values" if args.is_empty() => Some(format!("Cambrian.AddressMap.values {}", map)),
        "len" if args.is_empty() => {
            if is_state_member && member_needs_keys_list(ctx.entity, &name) {
                Some(format!("({}).length", member_keys_access(&name, ctx)))
            } else {
                Some(format!("Cambrian.AddressMap.length {}", map))
            }
        }
        "is_empty" if args.is_empty() => {
            if is_state_member && member_needs_keys_list(ctx.entity, &name) {
                Some(format!("({}).isEmpty", member_keys_access(&name, ctx)))
            } else {
                Some(format!("Cambrian.AddressMap.isEmpty {}", map))
            }
        }
        "exists" | "contains" if args.len() == 1 => {
            let k = gen_expr(&args[0], ctx);
            Some(format!("Cambrian.AddressMap.contains {} {}", map, k))
        }
        "collect" if args.is_empty() => {
            // Defer to inner — handled by lean_iter when chained.
            None
        }
        _ => None,
    }
}

/// After lowering a map transform body, produce optional `_keys` field update.
pub fn keys_update_for_transform_body(
    member: &Member,
    body: &Expr,
    ctx: &LeanExprCtx<'_>,
) -> Option<String> {
    if !member_needs_keys_list(ctx.entity, &member.name) {
        return None;
    }
    let keys = member_keys_access(&member.name, ctx);
    keys_update_term(member, body, ctx, &keys)
        .map(|term| format!("{}_keys := {}", member.name, term))
}

fn keys_update_term(
    member: &Member,
    body: &Expr,
    ctx: &LeanExprCtx<'_>,
    keys: &str,
) -> Option<String> {
    if matches!(body, Expr::EmptyCollection) {
        return Some("[]".to_string());
    }
    if let Expr::If(cond, then_body, else_body) = body {
        let then_update = keys_update_term(member, then_body, ctx, keys);
        let else_update = else_body
            .as_deref()
            .and_then(|body| keys_update_term(member, body, ctx, keys));
        if then_update.is_none() && else_update.is_none() {
            return None;
        }
        let cond = gen_expr(cond, ctx);
        return Some(format!(
            "if {} then {} else {}",
            cond,
            then_update.unwrap_or_else(|| keys.to_string()),
            else_update.unwrap_or_else(|| keys.to_string()),
        ));
    }
    if let Expr::Block(items) = body {
        return items
            .last()
            .and_then(|tail| keys_update_term(member, tail, ctx, keys));
    }
    if let Expr::Let(_, _, tail) = body {
        return keys_update_term(member, tail, ctx, keys);
    }
    if let Expr::MethodCall(base, method, args) = body {
        let root = map_member_name(base)?;
        if root != member.name {
            return None;
        }
        match method.as_str() {
            "insert" | "update" if args.len() == 2 => {
                let chain = collect_insert_keys(body, &member.name);
                if chain.is_empty() {
                    let k = gen_expr(&args[0], ctx);
                    return Some(format!("Cambrian.AddressMap.pushKeyIfNew {} {}", keys, k));
                }
                let mut term = keys.to_string();
                for k in chain {
                    let k_term = gen_expr(k, ctx);
                    term = format!("Cambrian.AddressMap.pushKeyIfNew ({}) {}", term, k_term);
                }
                return Some(term);
            }
            "remove" if args.len() == 1 => {
                let k = gen_expr(&args[0], ctx);
                return Some(format!("Cambrian.AddressMap.removeKey {} {}", keys, k));
            }
            _ => {}
        }
    }
    if let Some(k) = extract_last_remove_key(body, &member.name) {
        let k_term = gen_expr(&k, ctx);
        return Some(format!("Cambrian.AddressMap.removeKey {} {}", keys, k_term));
    }
    None
}

fn collect_insert_keys<'a>(expr: &'a Expr, member: &str) -> Vec<&'a Expr> {
    match expr {
        Expr::MethodCall(base, method, args)
            if (method == "insert" || method == "update") && args.len() == 2 =>
        {
            let mut keys = collect_insert_keys(base, member);
            if map_member_name(expr).as_deref() == Some(member) {
                keys.push(&args[0]);
            }
            keys
        }
        _ => Vec::new(),
    }
}

fn extract_last_remove_key<'a>(expr: &'a Expr, member: &str) -> Option<&'a Expr> {
    match expr {
        Expr::MethodCall(base, method, args) if method == "remove" && args.len() == 1 => {
            if map_member_name(base).as_deref() == Some(member) {
                Some(&args[0])
            } else {
                extract_last_remove_key(base, member)
            }
        }
        Expr::MethodCall(base, _, _) => extract_last_remove_key(base, member),
        _ => None,
    }
}

/// Build `{ s with m := …, m_keys := … }` fields for one HashMap member transform.
pub fn hashmap_transform_fields(
    entity: &Entity,
    member: &Member,
    route_name: &str,
    phase: Option<&str>,
    route_args: &str,
    body: &Expr,
    ctx: &LeanExprCtx<'_>,
) -> Vec<String> {
    let phase_suffix = phase.map(|p| format!("_{}", p)).unwrap_or_default();
    let map_val = format!(
        "{} := {}.Members.M_{}.{}{} {} ctx inst{}",
        member.name, entity.name, member.name, route_name, phase_suffix, ctx.state_var, route_args,
    );
    let mut fields = vec![map_val];
    if let Some(keys_field) = keys_update_for_transform_body(member, body, ctx) {
        fields.push(keys_field);
    }
    fields
}
