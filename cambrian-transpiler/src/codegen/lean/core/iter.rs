// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean P4c — `for` / `.fold` / iterator chains over `Vec` and `HashMap`.

use crate::ast::{Expr, Pattern, Type};

use super::super::expr::{gen_expr, map_type_of, pattern_to_lean, LeanExprCtx};
use super::hashmap::{ident_is_hashmap, map_member_name, try_gen_map_method_call};
use super::map_analysis::member_is_hashmap;
use super::map_analysis::member_needs_keys_list;

/// Binder text for a closure parameter, plus the statements that must open the
/// lambda body.
///
/// Legacy: `pattern_to_lean` verbatim, so `|(k, v)| …` becomes the pattern
/// lambda `fun (k, v) => …` (empty prefix — byte-identical to before).
///
/// Escrow: a pattern lambda is elaborated through a MATCHER
/// (`Cambrian.Generated.Pure.<f>.match_1`, `<E>.Members.M_<m>.<r>.match_1`) —
/// forbidden in the digest zone (PIPELINE.md §3.4), and this path had no escrow
/// hook at all (defect L1: `lean_iter` drops `pattern_to_lean(Pattern::Tuple)`
/// straight into a `List.map`/`List.filter`/`List.foldl` binder). A tuple binder
/// therefore becomes a fresh `__cbr_cN` plus `Prod` projections — the profile's
/// standing idiom (`lean_predictable::expand_tuple`, `lower::tuple_let_expand`):
///
/// ```text
/// (List.map (fun __cbr_c0 => let v := __cbr_c0.2; <body>) xs)
/// ```
///
/// `slot` separates the binders of ONE lambda (`List.foldl`'s accumulator and
/// element), so two tuple parameters never pick the same fresh name.
fn pattern_nat_binders(pat: &Pattern) -> Vec<String> {
    match pat {
        Pattern::Ident(name) => vec![name.clone()],
        Pattern::Wildcard => Vec::new(),
        Pattern::Deref(inner) | Pattern::Some(inner) => pattern_nat_binders(inner),
        Pattern::Tuple(parts) => parts.iter().flat_map(pattern_nat_binders).collect(),
        Pattern::None => Vec::new(),
    }
}

/// Clone `ctx` with range-element binders marked as `Nat` (UPSTREAM B-19).
fn with_range_nat_binders<'a>(ctx: &LeanExprCtx<'a>, pat: &Pattern) -> LeanExprCtx<'a> {
    ctx.dup().with_extra_nat_idents(pattern_nat_binders(pat))
}

fn closure_binder(pat: &Pattern, slot: usize) -> (String, String) {
    if let Pattern::Tuple(parts) = pat {
        if crate::codegen::lean::use_predictable_profile() {
            let names: Vec<String> = parts.iter().map(pattern_to_lean).collect();
            // A nested sub-pattern (`|((a, b), c)|`) has no projection spelling
            // here; fail loud rather than ship a matcher.
            if !names.iter().all(|n| {
                n.chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '\'')
            }) {
                super::super::evm::tuple_pattern_punt(
                    "an iterator-chain closure binder",
                    &pattern_to_lean(pat),
                );
            }
            return super::super::evm::closure_tuple_binder(&names, slot);
        }
    }
    (pattern_to_lean(pat), String::new())
}

#[derive(Debug)]
enum ChainStage<'a> {
    Keys,
    Values,
    Iter,
    Filter(&'a Pattern, &'a Expr),
    Map(&'a Pattern, &'a Expr),
    Take(&'a Expr),
    Enumerate,
}

#[derive(Debug)]
enum ChainTerminator<'a> {
    Collect,
    Fold {
        init: &'a Expr,
        acc_pat: &'a Pattern,
        iter_pat: &'a Pattern,
        body: &'a Expr,
    },
}

fn parse_collect_chain(expr: &Expr) -> (Expr, Vec<ChainStage<'_>>) {
    let mut stages = Vec::new();
    let mut cur = expr;
    loop {
        match cur {
            Expr::MethodCall(base, method, args) => match method.as_str() {
                "iter" if args.is_empty() => {
                    stages.push(ChainStage::Iter);
                    cur = base;
                }
                "values" if args.is_empty() => {
                    stages.push(ChainStage::Values);
                    cur = base;
                }
                "keys" if args.is_empty() => {
                    stages.push(ChainStage::Keys);
                    cur = base;
                }
                "filter" if args.len() == 1 => {
                    if let Expr::Closure(pats, body) = &args[0] {
                        if let Some(p) = pats.first() {
                            stages.push(ChainStage::Filter(p, body));
                        }
                    }
                    cur = base;
                }
                "map" if args.len() == 1 => {
                    if let Expr::Closure(pats, body) = &args[0] {
                        if let Some(p) = pats.first() {
                            stages.push(ChainStage::Map(p, body));
                        }
                    }
                    cur = base;
                }
                "take" if args.len() == 1 => {
                    stages.push(ChainStage::Take(&args[0]));
                    cur = base;
                }
                "enumerate" if args.is_empty() => {
                    stages.push(ChainStage::Enumerate);
                    cur = base;
                }
                _ => {
                    stages.reverse();
                    return (cur.clone(), stages);
                }
            },
            _ => {
                stages.reverse();
                return (cur.clone(), stages);
            }
        }
    }
}

fn hashmap_keys_list(member: &str, ctx: &LeanExprCtx<'_>) -> String {
    if member_needs_keys_list(ctx.entity, member) {
        format!("{}.{}_keys", ctx.state_var, member)
    } else {
        format!("Cambrian.AddressMap.keys {}.{}", ctx.state_var, member)
    }
}

fn hashmap_values_list(member: &str, ctx: &LeanExprCtx<'_>) -> String {
    let keys = hashmap_keys_list(member, ctx);
    let map = format!("{}.{}", ctx.state_var, member);
    format!(
        "(List.map (fun k => (Cambrian.AddressMap.lookup {} k |>.get!)) {})",
        map, keys
    )
}

fn hashmap_iter_pairs(member: &str, ctx: &LeanExprCtx<'_>) -> String {
    let keys = hashmap_keys_list(member, ctx);
    let map = format!("{}.{}", ctx.state_var, member);
    format!(
        "(List.map (fun k => (k, Cambrian.AddressMap.lookup {} k |>.get!)) {})",
        map, keys
    )
}

/// Resolve an iterator expression to a `List` Lean term.
pub fn resolve_iter_list(iter: &Expr, ctx: &LeanExprCtx<'_>) -> Option<String> {
    if let Expr::Range(start, end) = iter {
        return Some(super::super::expr::gen_range_list(start, end, ctx));
    }
    if let Expr::Ident(name) = iter {
        if let Some(m) = ctx.entity.members.iter().find(|x| x.name == *name) {
            if matches!(&m.ty, Type::Generic(g, _) if g == "HashMap") {
                return Some(hashmap_iter_pairs(name, ctx));
            }
        }
    }
    if let Expr::MethodCall(base, method, args) = iter {
        if method == "keys" && args.is_empty() {
            return try_gen_map_method_call(base, "keys", &[], ctx);
        }
    }
    Some(gen_expr(iter, ctx))
}

fn resolve_chain_source(source: &Expr, stages: &[ChainStage<'_>], ctx: &LeanExprCtx<'_>) -> String {
    let member = map_member_name(source).or_else(|| {
        if let Expr::Ident(n) = source {
            Some(n.clone())
        } else {
            None
        }
    });
    if let Some(name) = member {
        let is_state_member = ctx
            .entity
            .members
            .iter()
            .any(|m| m.name == name && member_is_hashmap(m));
        if is_state_member {
            let wants_values = stages.iter().any(|s| matches!(s, ChainStage::Values));
            let wants_keys_only = stages.iter().any(|s| matches!(s, ChainStage::Keys))
                && !stages.iter().any(|s| matches!(s, ChainStage::Iter));
            if wants_values {
                return hashmap_values_list(&name, ctx);
            }
            if wants_keys_only {
                return hashmap_keys_list(&name, ctx);
            }
            return hashmap_iter_pairs(&name, ctx);
        }
        if ident_is_hashmap(&name, ctx) {
            let map = gen_expr(source, ctx);
            let has_iter = stages.iter().any(|s| matches!(s, ChainStage::Iter));
            let wants_values = stages.iter().any(|s| matches!(s, ChainStage::Values));
            let wants_keys_only = stages.iter().any(|s| matches!(s, ChainStage::Keys)) && !has_iter;
            if wants_values {
                return format!("(Cambrian.AddressMap.values {})", map);
            }
            if wants_keys_only {
                return format!("(Cambrian.AddressMap.keys {})", map);
            }
            // `m.iter().fold` on a param: `AddressMap` is already `List (K × V)` — keep
            // the map term (byte-identical to the pre-VFOLD accidental win).
            if has_iter && stages.iter().all(|s| matches!(s, ChainStage::Iter)) {
                return map;
            }
            return format!(
                "(List.map (fun k => (k, (Cambrian.AddressMap.lookup {} k |>.get!))) (Cambrian.AddressMap.keys {}))",
                map, map
            );
        }
    }
    if map_type_of(source, ctx).is_some() {
        let map = gen_expr(source, ctx);
        let has_iter = stages.iter().any(|s| matches!(s, ChainStage::Iter));
        let wants_values = stages.iter().any(|s| matches!(s, ChainStage::Values));
        let wants_keys_only = stages.iter().any(|s| matches!(s, ChainStage::Keys)) && !has_iter;
        if wants_values {
            return format!("(Cambrian.AddressMap.values {})", map);
        }
        if wants_keys_only {
            return format!("(Cambrian.AddressMap.keys {})", map);
        }
        if has_iter && stages.iter().all(|s| matches!(s, ChainStage::Iter)) {
            return map;
        }
        return format!(
            "(List.map (fun k => (k, (Cambrian.AddressMap.lookup {} k |>.get!))) (Cambrian.AddressMap.keys {}))",
            map, map
        );
    }
    resolve_iter_list(source, ctx).unwrap_or_else(|| gen_expr(source, ctx))
}

fn apply_chain_stages(list: String, stages: &[ChainStage<'_>], ctx: &LeanExprCtx<'_>) -> String {
    let mut cur = list;
    for stage in stages {
        cur = match stage {
            ChainStage::Keys | ChainStage::Iter => cur,
            ChainStage::Values => {
                // If `cur` is already a values list (from resolve), keep it.
                cur
            }
            ChainStage::Filter(pat, body) => {
                let (p, proj) = closure_binder(pat, 0);
                let b = gen_expr(body, ctx);
                format!("(List.filter (fun {} => {}{}) {})", p, proj, b, cur)
            }
            ChainStage::Map(pat, body) => {
                let (p, proj) = closure_binder(pat, 0);
                let b = gen_expr(body, ctx);
                format!("(List.map (fun {} => {}{}) {})", p, proj, b, cur)
            }
            ChainStage::Take(n) => {
                format!("(List.take {} {})", gen_expr(n, ctx), cur)
            }
            ChainStage::Enumerate => format!("(List.zip (List.range {}.length) {})", cur, cur,),
        };
    }
    cur
}

pub fn try_lower_iter_chain(expr: &Expr, ctx: &LeanExprCtx<'_>) -> Option<String> {
    let (source, stages, term) = match expr {
        Expr::MethodCall(base, method, args) if method == "collect" && args.is_empty() => {
            let (src, st) = parse_collect_chain(base);
            (src, st, ChainTerminator::Collect)
        }
        Expr::MethodCall(base, method, args) if method == "fold" && args.len() == 2 => {
            let (src, st) = parse_collect_chain(base);
            let Expr::Closure(params, body) = &args[1] else {
                return None;
            };
            let acc_pat = params.first()?;
            let iter_pat = params.get(1)?;
            (
                src,
                st,
                ChainTerminator::Fold {
                    init: &args[0],
                    acc_pat,
                    iter_pat,
                    body,
                },
            )
        }
        _ => return None,
    };

    let base_list = resolve_chain_source(&source, &stages, ctx);
    let chained = apply_chain_stages(base_list, &stages, ctx);

    match term {
        ChainTerminator::Collect => Some(chained),
        ChainTerminator::Fold {
            init,
            acc_pat,
            iter_pat,
            body,
        } => {
            // Both binders belong to the SAME lambda, so they take distinct
            // fresh-name slots; the accumulator's projections precede the
            // element's, matching the R3 predictor's nesting.
            let (acc, acc_proj) = closure_binder(acc_pat, 0);
            let (iter, iter_proj) = closure_binder(iter_pat, 1);
            // UPSTREAM B-19: range element binders are `Nat`.
            let body_ctx = if matches!(source, Expr::Range(_, _)) {
                with_range_nat_binders(ctx, iter_pat)
            } else {
                ctx.dup()
            };
            Some(format!(
                "(List.foldl (fun {} {} => {}{}{}) {} {})",
                acc,
                iter,
                acc_proj,
                iter_proj,
                gen_expr(body, &body_ctx),
                gen_expr(init, ctx),
                chained
            ))
        }
    }
}

/// `for pat in iter { body }` → `List.map`.
pub fn gen_for_expr(pat: &Pattern, iter: &Expr, body: &Expr, ctx: &LeanExprCtx<'_>) -> String {
    let list = resolve_iter_list(iter, ctx).unwrap_or_else(|| gen_expr(iter, ctx));
    let (binder, proj) = closure_binder(pat, 0);
    let body_ctx = if matches!(iter, Expr::Range(_, _)) {
        with_range_nat_binders(ctx, pat)
    } else {
        ctx.dup()
    };
    let mut body_term = gen_expr(body, &body_ctx);
    // A body built only from the `Nat` range binder and literals is `Nat`;
    // the element type is the range bounds' width.
    if let Expr::Range(lo, hi) = iter {
        let bound_w = super::super::expr::bitvec_width(hi, ctx)
            .or_else(|| super::super::expr::bitvec_width(lo, ctx));
        if let Some(w) = bound_w {
            if !ctx.type_ctx.use_nat_numerics && is_nat_valued(body, &body_ctx) {
                body_term = format!("(BitVec.ofNat {} {})", w, body_term);
            }
        }
    }
    format!(
        "(List.map (fun {} => {}{}) {})",
        binder, proj, body_term, list
    )
}

fn is_nat_valued(expr: &Expr, ctx: &LeanExprCtx<'_>) -> bool {
    fn walk(e: &Expr, ctx: &LeanExprCtx<'_>, seen_binder: &mut bool) -> bool {
        match e {
            Expr::Ident(n) if ctx.nat_idents.contains(n) => {
                *seen_binder = true;
                true
            }
            Expr::IntLiteral(_) => true,
            Expr::BinOp(l, op, r) => {
                matches!(
                    op,
                    crate::ast::BinOp::Add
                        | crate::ast::BinOp::Sub
                        | crate::ast::BinOp::Mul
                        | crate::ast::BinOp::Div
                        | crate::ast::BinOp::Mod
                ) && walk(l, ctx, seen_binder)
                    && walk(r, ctx, seen_binder)
            }
            _ => false,
        }
    }
    let mut seen = false;
    walk(expr, ctx, &mut seen) && seen
}

pub fn try_gen_iter_expr(expr: &Expr, ctx: &LeanExprCtx<'_>) -> Option<String> {
    try_lower_iter_chain(expr, ctx)
}

pub fn try_gen_fold_method(
    base: &Expr,
    init: &Expr,
    closure: &Expr,
    ctx: &LeanExprCtx<'_>,
) -> Option<String> {
    let Expr::Closure(params, body) = closure else {
        return None;
    };
    let acc_pat = params.first()?;
    let list = resolve_iter_list(base, ctx)?;
    let (acc, acc_proj) = closure_binder(acc_pat, 0);
    let (elem, elem_proj) = params
        .get(1)
        .map(|p| closure_binder(p, 1))
        .unwrap_or_else(|| ("__e".to_string(), String::new()));
    let body_ctx = if matches!(base, Expr::Range(_, _)) {
        // Range elements are `Nat` (`List.range`). Mark Cam's second
        // param so body idents coerce via ofNat — and use that name as
        // the foldl binder so it cannot shadow an outer `x`.
        let mut c = ctx.dup();
        if let Some(iter_pat) = params.get(1) {
            c = with_range_nat_binders(&c, iter_pat);
        }
        c
    } else {
        ctx.dup()
    };
    Some(format!(
        "(List.foldl (fun {} {} => {}{}{}) {} {})",
        acc,
        elem,
        acc_proj,
        elem_proj,
        gen_expr(body, &body_ctx),
        gen_expr(init, ctx),
        list
    ))
}

/// Fail-mode `.fold`: accumulator is `RouteResult`, each step binds then
/// runs the closure body as `RouteResult` (PW3-O-007 checked `as uN`).
pub fn try_gen_fold_method_fail(
    base: &Expr,
    init: &Expr,
    closure: &Expr,
    ctx: &LeanExprCtx<'_>,
) -> Option<String> {
    let Expr::Closure(params, body) = closure else {
        return None;
    };
    if !super::super::expr::expr_has_narrow_cast(body)
        && !super::super::expr::expr_has_div0_op(body)
        && !super::super::expr::expr_forces_fail_surface(body, ctx.pure_fns)
    {
        return None;
    }
    let acc_pat = params.first()?;
    let list = resolve_iter_list(base, ctx)?;
    let (acc, acc_proj) = closure_binder(acc_pat, 0);
    let (elem, elem_proj) = params
        .get(1)
        .map(|p| closure_binder(p, 1))
        .unwrap_or_else(|| ("__e".to_string(), String::new()));
    let body_ctx = if matches!(base, Expr::Range(_, _)) {
        let mut c = ctx.dup();
        if let Some(iter_pat) = params.get(1) {
            c = with_range_nat_binders(&c, iter_pat);
        }
        c
    } else {
        ctx.dup()
    };
    let step = super::super::expr::gen_expr_as_route_result(body, &body_ctx);
    let init_rr = super::super::expr::gen_expr_as_route_result(init, ctx);
    Some(format!(
        "(List.foldl (fun __rr {} => {}(__rr >>= fun {} => {}{})) {} {})",
        elem,
        elem_proj,
        acc,
        acc_proj,
        step,
        init_rr,
        list
    ))
}
