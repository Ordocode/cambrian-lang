// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM codegen — iteration / chain / fold lowering.

use crate::ast::{Entity, Expr, Pattern, Type};
use std::collections::HashMap as StdHashMap;

use super::ctx::EmitScope;
use super::ctx::EvmCtx;
use super::expr::gen_expr_hoisted;
use super::scratch::EmitScratch;
use super::types::*;
use std::cell::RefCell;

// ---------------------------------------------------------------------------
// `for x in iter { body }` lowering on EVM
// ---------------------------------------------------------------------------
//
// Cambrian `for` is value-producing — on Acki Nacki it lowers to
// `(iter).map(|p| body).collect::<Vec<_>>()`. To preserve that semantics
// on EVM we materialise a memory array and fill it as the loop runs:
//
//     T[] memory _result = new T[](N);
//     for (uint256 _i = 0; _i < N; ++_i) {
//         <bind loop variable>;
//         <body statements>;
//         _result[_i] = <body value>;
//     }
//     // value of the for-expression == _result
//
// Supported iterator shapes:
//
//   * `Expr::Range(start, end)`           → loop bound `(end - start)`,
//     binding `T name = start + _i;`
//   * any other expression that resolves
//     to a `Vec<T>`-typed value           → loop bound `vec.length`,
//     binding `T name = vec[_i];`
//
// Limitations (still produce `E07` warnings + fallback reverts at codegen):
//
//   * Iterator-method chain *prefixes*
//     (`xs.iter().filter(...).map(...).take(N)`) — chain fusion / closure
//     inlining still TODO. Direct `.fold(init, |acc, x| body)` over a
//     `Range` or `Vec<T>` *is* supported — see `gen_fold_loop` below.
//   * Bodies whose value type cannot be inferred and whose result the
//     user actually consumes (we fall back to `uint256` / `bytes32`).
//
// Tuple-destructured patterns *are* supported when the iterator yields
// a tuple shape (currently only `m.iter()` for `HashMap<K, V>`; chain
// fusion lands in EVM-12 Batch B). See `IterElem::Tuple` and
// `bind_loop_pattern` below.

/// One iteration step's value, exposed to a Cambrian pattern.
///
/// Solidity has no first-class tuple values, so we never materialise a
/// tuple as a single expression. Instead, tuple-yielding sources expose
/// per-component `(sol_type, sol_value_expr)` pairs and the binder
/// assigns each component into a separate Solidity local. This works
/// because the only shapes we accept on the consumer side
/// (`Pattern::Tuple` and through-passes for chain stages) decompose
/// positionally; nothing tries to pass a Cambrian tuple through a
/// non-tuple-shaped consumer.
pub(crate) enum IterElem {
    Scalar { ty: String, rhs: String },
    Tuple { components: Vec<(String, String)> },
}

/// Bind a Cambrian pattern against an `IterElem`, returning the
/// Solidity declaration statements that introduce the bound locals.
///
/// `Pattern::Ident(n)` over a scalar elem → `T n = rhs;`.
/// `Pattern::Wildcard` over a scalar elem → no decl.
/// `Pattern::Tuple(pats)` over a tuple elem with matching arity →
/// recurse component-by-component (with each component treated as a
/// scalar in this round; nested tuples aren't produced by any current
/// source).
///
/// Mismatches are surfaced as `Err(<diagnostic>)` so callers can
/// emit a clear `revert(...)` rather than miscompiling silently.
pub(crate) fn bind_loop_pattern(pat: &Pattern, elem: &IterElem) -> Result<Vec<String>, String> {
    match (pat, elem) {
        (Pattern::Ident(n), IterElem::Scalar { ty, rhs }) => {
            Ok(vec![format!("{} {} = {};", ty, n, rhs)])
        }
        (Pattern::Wildcard, IterElem::Scalar { .. }) => Ok(vec![]),
        // `|*x|` is the explicit-deref closure pattern Cambrian uses
        // to mark "I'm dereferencing the iterator's reference here".
        // On EVM iter elements are by-value already (Solidity has no
        // reference semantics for value types), so the deref pattern
        // is identity — defer to the inner pattern.
        (Pattern::Deref(inner), _) => bind_loop_pattern(inner, elem),
        (Pattern::Tuple(pats), IterElem::Tuple { components }) if pats.len() == components.len() => {
            let mut decls = Vec::with_capacity(pats.len());
            for (sub_pat, (ty, rhs)) in pats.iter().zip(components.iter()) {
                let sub_elem = IterElem::Scalar {
                    ty: ty.clone(),
                    rhs: rhs.clone(),
                };
                decls.extend(bind_loop_pattern(sub_pat, &sub_elem)?);
            }
            Ok(decls)
        }
        (Pattern::Tuple(_), IterElem::Scalar { .. }) => Err(
            "EVM: tuple destructuring requires a tuple-yielding iterator (e.g. `m.iter()`, `xs.enumerate()`)"
                .to_string(),
        ),
        (Pattern::Ident(_), IterElem::Tuple { .. }) | (Pattern::Wildcard, IterElem::Tuple { .. }) => {
            Err("EVM: tuple-yielding iterator must be destructured with a tuple pattern (e.g. `for (k, v) in m.iter()`)".to_string())
        }
        _ => Err("EVM: unsupported loop pattern".to_string()),
    }
}

/// Resolve an iterator expression into a hoisting prefix, a count
/// expression, and an `IterElem` factory keyed by the loop's index
/// variable name.
///
/// The factory produces the per-iteration `IterElem` given the index
/// variable's Solidity name — this lets callers thread the same `_i`
/// into both the bound locals and any sibling code.
pub(crate) fn resolve_iter_source(
    iter: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> (Vec<String>, String, Box<dyn Fn(&str) -> IterElem>) {
    if let Expr::Range(start, end) = iter {
        let (mut s_stmts, s_str) = gen_expr_hoisted(start, entity, ctx, scope, scratch);
        let (e_stmts, e_str) = gen_expr_hoisted(end, entity, ctx, scope, scratch);
        s_stmts.extend(e_stmts);
        // Phase EVM-P0-B: pick the range's element type from the
        // bounds. Pre-P0-B every range element widened to `uint256`,
        // which clashed with the new narrow-type survival in member /
        // pure-fn return positions (e.g. `for i in 0..n { i }` over
        // `n: u32` would build a `uint256[]` that solc can't return
        // as `uint32[]`). Inference rule: prefer either bound's
        // narrow type, fall back to `uint256` for U256 / unknown.
        let elem_ty = infer_range_elem_ty(
            start,
            end,
            entity,
            ctx,
            scratch,
            super::types::scope_expected_vec_elem_sol_ty(scope, entity, ctx).as_deref(),
        );
        let start_tmp = scratch.borrow_mut().next_tmp();
        let end_tmp = scratch.borrow_mut().next_tmp();
        s_stmts.push(format!("{} {} = {};", elem_ty, start_tmp, s_str));
        s_stmts.push(format!("{} {} = {};", elem_ty, end_tmp, e_str));
        let count_expr = format!("({} - {})", end_tmp, start_tmp);
        let factory: Box<dyn Fn(&str) -> IterElem> = {
            let start_tmp = start_tmp.clone();
            let elem_ty = elem_ty.clone();
            Box::new(move |idx: &str| {
                // The loop index is always `uint256`; if the range's
                // element type is narrower we explicitly downcast to
                // keep `start + idx` width-consistent.
                let rhs = if elem_ty == "uint256" {
                    format!("{} + {}", start_tmp, idx)
                } else {
                    format!("{} + {}({})", start_tmp, elem_ty, idx)
                };
                IterElem::Scalar {
                    ty: elem_ty.clone(),
                    rhs,
                }
            })
        };
        return (s_stmts, count_expr, factory);
    }
    // EVM-13: HashMap iteration sources. Three shapes resolve here:
    //   * `m.iter()` — yields `(K, V)` tuples (Batch A).
    //   * Bare `m` as iter source — sugar for `m.iter()` (Batch B);
    //     the chain fuser strips a leading `.iter()` stage so a bare
    //     HashMap ident reaches us in chain contexts too.
    //   * `m.values()` — yields scalar `V` per iteration (Batch D); we
    //     still walk the parallel `<m>_keys` array and look up
    //     `m[m_keys[i]]`, but expose only the value component to the
    //     caller's pattern binder. The companion storage is
    //     guaranteed present because `member_is_iterated` recognises
    //     `iter` and `values` as iteration use sites.
    enum MapIterShape<'a> {
        IterOrSelf(&'a String),
        Values(&'a String),
    }
    let shape: Option<MapIterShape> = match iter {
        Expr::MethodCall(base, method, args) if method == "iter" && args.is_empty() => {
            if let Expr::Ident(name) = base.as_ref() {
                Some(MapIterShape::IterOrSelf(name))
            } else {
                None
            }
        }
        Expr::MethodCall(base, method, args) if method == "values" && args.is_empty() => {
            if let Expr::Ident(name) = base.as_ref() {
                Some(MapIterShape::Values(name))
            } else {
                None
            }
        }
        Expr::Ident(name) => Some(MapIterShape::IterOrSelf(name)),
        _ => None,
    };
    if let Some(shape) = shape {
        let name = match &shape {
            MapIterShape::IterOrSelf(n) | MapIterShape::Values(n) => *n,
        };
        if let Some(member) = entity.members.iter().find(|m| m.name == *name) {
            if let Type::Generic(g, params) = &member.ty {
                if g == "HashMap" && params.len() >= 2 {
                    let k_ty = sol_type_entity(entity, &params[0], true, ctx);
                    let v_ty = sol_type_entity(entity, &params[1], true, ctx);
                    let map_name = name.clone();
                    let count_expr = format!("{}_keys.length", map_name);
                    match shape {
                        MapIterShape::IterOrSelf(_) => {
                            let factory: Box<dyn Fn(&str) -> IterElem> =
                                Box::new(move |idx: &str| IterElem::Tuple {
                                    components: vec![
                                        (k_ty.clone(), format!("{}_keys[{}]", map_name, idx)),
                                        (
                                            v_ty.clone(),
                                            format!("{}[{}_keys[{}]]", map_name, map_name, idx),
                                        ),
                                    ],
                                });
                            return (vec![], count_expr, factory);
                        }
                        MapIterShape::Values(_) => {
                            let factory: Box<dyn Fn(&str) -> IterElem> =
                                Box::new(move |idx: &str| IterElem::Scalar {
                                    ty: v_ty.clone(),
                                    rhs: format!("{}[{}_keys[{}]]", map_name, map_name, idx),
                                });
                            return (vec![], count_expr, factory);
                        }
                    }
                }
            }
        }
    }
    // Nested HashMap slot `m[outer].keys().collect()` — after Batch G2
    // let-substitution collapses `let inner = m[outer]; inner.keys()`
    // to this shape. There is no per-outer inner `_keys` companion yet,
    // so materialise an empty key iteration (count 0) with the correct
    // inner key element type.
    if let Expr::Index(inner, outer_k) = iter {
        if let Expr::Ident(name) = inner.as_ref() {
        if let Some(inner_k_ty) = nested_hashmap_inner_key_type(entity, name) {
            let k_ty = sol_type_entity(entity, &inner_k_ty, true, ctx);
            let (outer_stmts, _) = gen_expr_hoisted(outer_k, entity, ctx, scope, scratch);
            let zero_rhs = if k_ty == "uint256" {
                "0".to_string()
            } else {
                format!("{}(0)", k_ty)
            };
            let factory: Box<dyn Fn(&str) -> IterElem> = Box::new(move |_idx: &str| IterElem::Scalar {
                ty: k_ty.clone(),
                rhs: zero_rhs.clone(),
            });
            return (outer_stmts, "0".to_string(), factory);
        }
        }
    }
    let (vec_stmts, vec_str) = gen_expr_hoisted(iter, entity, ctx, scope, scratch);
    let elem_param_ty = infer_iter_elem_type_entity(iter, entity, ctx, scratch)
        .map(|t| sol_type_entity(entity, &t, true, ctx))
        .unwrap_or_else(|| "uint256".to_string());
    let count_expr = format!("{}.length", vec_str);
    let factory: Box<dyn Fn(&str) -> IterElem> = {
        let vec_str = vec_str.clone();
        let elem_param_ty = elem_param_ty.clone();
        Box::new(move |idx: &str| IterElem::Scalar {
            ty: elem_param_ty.clone(),
            rhs: format!("{}[{}]", vec_str, idx),
        })
    };
    (vec_stmts, count_expr, factory)
}

// ---------------------------------------------------------------------------
// EVM-12 Batch B: iterator-chain fusion
// ---------------------------------------------------------------------------
//
// Recognises chains of the form
//
//     <source> [ .iter() | .enumerate() | .filter(|p| body)
//              | .map(|p| body) | .take(n) ]*  ( .collect() | .fold(init, |acc, p| body) )
//
// and lowers them to a single Solidity for-loop. Each stage is inlined
// at codegen time:
//
//   * `.iter()` is a no-op pass-through (Vec is already iterable; for
//     HashMap sources `m.iter()` is recognised by `resolve_iter_source`
//     directly).
//   * `.enumerate()` wraps a scalar element into `(uint256, T)`.
//   * `.filter(|p| pred)` becomes `if (!pred) continue;`.
//   * `.map(|p| f)` rebinds the current value to the body's result.
//   * `.take(n)` becomes a counter + `if (taken >= n) break;`.
//   * `.collect()` materialises a `T[] memory` and trims via
//     `assembly { mstore(arr, len) }` (gas-friendly, single pass).
//   * `.fold(init, |acc, p| body)` threads `acc` across iterations.
//
// Closure parameters are introduced as fresh Solidity locals scoped to
// the stage's `{ ... }` block — Solidity allows shadowing across nested
// scopes, so two `.filter(|x| ...).map(|x| ...)` stages don't collide.

#[derive(Debug)]
pub(crate) enum ChainStage<'a> {
    Iter,
    /// Peeled from `.keys()` — the source must already yield scalar
    /// key elements (member `_keys` storage or a nested-slot stub).
    Keys,
    /// EVM-13 Batch D: peeled when the chain root is a HashMap source
    /// — collapses a tuple-shaped current `(K, V)` into a scalar
    /// current `V`. Lets `m.values().collect()` and friends fuse
    /// through the same code path as `m.iter().filter(...).map(...)`.
    Values,
    Enumerate,
    Filter(&'a Pattern, &'a Expr),
    Map(&'a Pattern, &'a Expr),
    Take(&'a Expr),
}

#[derive(Debug)]
pub(crate) enum ChainTerminator<'a> {
    Collect,
    Fold {
        init: &'a Expr,
        acc_pat: &'a Pattern,
        var_pat: &'a Pattern,
        body: &'a Expr,
    },
}

/// Walk `expr` and try to decompose it as a chain. Returns
/// `Some((source, stages, terminator))` when at least one chain stage
/// is present (or the terminator wraps a non-trivial source). Returns
/// `None` when the expression has no chain shape — caller falls back
/// to the existing single-step lowerings (`gen_for_loop`,
/// `gen_fold_loop`, the `MethodCall("collect")` and `MethodCall("keys")`
/// arms).
pub(crate) fn parse_iter_chain(
    expr: &Expr,
) -> Option<(&Expr, Vec<ChainStage<'_>>, ChainTerminator<'_>)> {
    let (terminator, mut cursor) = match expr {
        Expr::MethodCall(base, m, args) if m == "collect" && args.is_empty() => {
            (ChainTerminator::Collect, base.as_ref())
        }
        Expr::MethodCall(base, m, args)
            if m == "fold"
                && args.len() == 2
                && matches!(&args[1], Expr::Closure(p, _) if p.len() == 2) =>
        {
            let (acc_pat, var_pat, body) = match &args[1] {
                Expr::Closure(params, body) => (&params[0], &params[1], body.as_ref()),
                _ => return None,
            };
            (
                ChainTerminator::Fold {
                    init: &args[0],
                    acc_pat,
                    var_pat,
                    body,
                },
                base.as_ref(),
            )
        }
        _ => return None,
    };

    let mut stages_rev = Vec::new();
    loop {
        match cursor {
            Expr::MethodCall(b, m, args) if m == "iter" && args.is_empty() => {
                stages_rev.push(ChainStage::Iter);
                cursor = b.as_ref();
            }
            Expr::MethodCall(b, m, args) if m == "keys" && args.is_empty() => {
                // Bare `m.keys().collect()` keeps the dedicated `gen_expr`
                // `{m}_keys` path; only nested-slot `m[outer].keys()`
                // (after HashMap-let substitution) diverts here.
                if matches!(
                    b.as_ref(),
                    Expr::Index(inner, _) if matches!(inner.as_ref(), Expr::Ident(_))
                ) {
                    stages_rev.push(ChainStage::Keys);
                    cursor = b.as_ref();
                } else {
                    break;
                }
            }
            Expr::MethodCall(b, m, args) if m == "values" && args.is_empty() => {
                stages_rev.push(ChainStage::Values);
                cursor = b.as_ref();
            }
            Expr::MethodCall(b, m, args) if m == "enumerate" && args.is_empty() => {
                stages_rev.push(ChainStage::Enumerate);
                cursor = b.as_ref();
            }
            Expr::MethodCall(b, m, args)
                if m == "filter"
                    && args.len() == 1
                    && matches!(&args[0], Expr::Closure(p, _) if p.len() == 1) =>
            {
                if let Expr::Closure(params, body) = &args[0] {
                    stages_rev.push(ChainStage::Filter(&params[0], body.as_ref()));
                    cursor = b.as_ref();
                    continue;
                }
                break;
            }
            Expr::MethodCall(b, m, args)
                if m == "map"
                    && args.len() == 1
                    && matches!(&args[0], Expr::Closure(p, _) if p.len() == 1) =>
            {
                if let Expr::Closure(params, body) = &args[0] {
                    stages_rev.push(ChainStage::Map(&params[0], body.as_ref()));
                    cursor = b.as_ref();
                    continue;
                }
                break;
            }
            Expr::MethodCall(b, m, args) if m == "take" && args.len() == 1 => {
                stages_rev.push(ChainStage::Take(&args[0]));
                cursor = b.as_ref();
            }
            _ => break,
        }
    }
    stages_rev.reverse();

    // For `.collect()` over a bare `m.keys()` source we already have a
    // dedicated lowering; let that path keep handling it. Same for a
    // bare `.fold` over a scalar source — the existing `gen_fold_loop`
    // covers it. Only divert through the chain emitter when there is
    // at least one peelable stage.
    if stages_rev.is_empty() {
        return None;
    }
    Some((cursor, stages_rev, terminator))
}

/// Recursively rename `Expr::Ident(name)` per `name_map` in a freshly
/// cloned expression. Currently unused — chain-stage closure params
/// are kept verbatim because each stage emits its own `{}` Solidity
/// block. Retained here behind `#[allow(dead_code)]` because Batch C
/// (record/tuple fold accumulators) will reuse the substitution
/// machinery for inlining accumulator-pattern field reads.
#[allow(dead_code)]
pub(crate) fn rename_idents_in_expr(expr: &Expr, name_map: &StdHashMap<String, String>) -> Expr {
    fn rename_pat(p: &Pattern, _name_map: &StdHashMap<String, String>) -> Pattern {
        // Patterns introduce *new* names (which we won't substitute);
        // just clone.
        p.clone()
    }
    match expr {
        Expr::Ident(name) => {
            if let Some(new_name) = name_map.get(name) {
                Expr::Ident(new_name.clone())
            } else {
                expr.clone()
            }
        }
        Expr::TemporalRef(_)
        | Expr::IntLiteral(_)
        | Expr::BoolLiteral(_)
       
       
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::None
        | Expr::EmptyCollection
        | Expr::MsgField(_)
        | Expr::SysField(_)
        | Expr::TraceField(_)
        | Expr::TraceCall { .. } => expr.clone(),
        Expr::BinOp(l, op, r) => Expr::BinOp(
            Box::new(rename_idents_in_expr(l, name_map)),
            op.clone(),
            Box::new(rename_idents_in_expr(r, name_map)),
        ),
        Expr::UnaryOp(op, e) => {
            Expr::UnaryOp(op.clone(), Box::new(rename_idents_in_expr(e, name_map)))
        }
        Expr::FieldAccess(b, f) => {
            Expr::FieldAccess(Box::new(rename_idents_in_expr(b, name_map)), f.clone())
        }
        Expr::Index(b, k) => Expr::Index(
            Box::new(rename_idents_in_expr(b, name_map)),
            Box::new(rename_idents_in_expr(k, name_map)),
        ),
        Expr::FnCall(name, args) => Expr::FnCall(
            name.clone(),
            args.iter()
                .map(|a| rename_idents_in_expr(a, name_map))
                .collect(),
        ),
        Expr::MacroRef(name, args) => Expr::MacroRef(
            name.clone(),
            args.iter()
                .map(|a| rename_idents_in_expr(a, name_map))
                .collect(),
        ),
        Expr::MethodCall(b, m, args) => Expr::MethodCall(
            Box::new(rename_idents_in_expr(b, name_map)),
            m.clone(),
            args.iter()
                .map(|a| rename_idents_in_expr(a, name_map))
                .collect(),
        ),
        Expr::If(c, t, e) => Expr::If(
            Box::new(rename_idents_in_expr(c, name_map)),
            Box::new(rename_idents_in_expr(t, name_map)),
            e.as_ref()
                .map(|x| Box::new(rename_idents_in_expr(x, name_map))),
        ),
        Expr::Let(p, v, b) => Expr::Let(
            rename_pat(p, name_map),
            Box::new(rename_idents_in_expr(v, name_map)),
            Box::new(rename_idents_in_expr(b, name_map)),
        ),
        Expr::Block(items) => Expr::Block(
            items
                .iter()
                .map(|i| rename_idents_in_expr(i, name_map))
                .collect(),
        ),
        Expr::ArrayLit(items) => Expr::ArrayLit(
            items
                .iter()
                .map(|i| rename_idents_in_expr(i, name_map))
                .collect(),
        ),
        Expr::Cast(e, t) => Expr::Cast(Box::new(rename_idents_in_expr(e, name_map)), t.clone()),
        Expr::Some(e) => Expr::Some(Box::new(rename_idents_in_expr(e, name_map))),
        Expr::Range(s, e) => Expr::Range(
            Box::new(rename_idents_in_expr(s, name_map)),
            Box::new(rename_idents_in_expr(e, name_map)),
        ),
        Expr::Closure(params, body) => {
            // Closure parameters shadow outer names — restrict substitution.
            let mut shadowed = name_map.clone();
            for p in params {
                for n in pattern_idents(p) {
                    shadowed.remove(&n);
                }
            }
            Expr::Closure(
                params.clone(),
                Box::new(rename_idents_in_expr(body, &shadowed)),
            )
        }
        Expr::For(p, it, body) => Expr::For(
            p.clone(),
            Box::new(rename_idents_in_expr(it, name_map)),
            Box::new(rename_idents_in_expr(body, name_map)),
        ),
        Expr::Tuple(items) => Expr::Tuple(
            items
                .iter()
                .map(|i| rename_idents_in_expr(i, name_map))
                .collect(),
        ),
        Expr::RecordConstruct(name, fields) => Expr::RecordConstruct(
            name.clone(),
            fields
                .iter()
                .map(|(n, e)| (n.clone(), rename_idents_in_expr(e, name_map)))
                .collect(),
        ),
        Expr::RecordUpdate(b, fields) => Expr::RecordUpdate(
            Box::new(rename_idents_in_expr(b, name_map)),
            fields
                .iter()
                .map(|(n, e)| (n.clone(), rename_idents_in_expr(e, name_map)))
                .collect(),
        ),
        Expr::Match(s, arms) => Expr::Match(
            Box::new(rename_idents_in_expr(s, name_map)),
            arms.iter()
                .map(|a| crate::ast::MatchArm {
                    pattern: a.pattern.clone(),
                    body: rename_idents_in_expr(&a.body, name_map),
                })
                .collect(),
        ),
        Expr::EnumVariant(en, v) => Expr::EnumVariant(en.clone(), v.clone()),
        Expr::EnumVariantWithData(en, v, args) => Expr::EnumVariantWithData(
            en.clone(),
            v.clone(),
            args.iter()
                .map(|a| rename_idents_in_expr(a, name_map))
                .collect(),
        ),
        Expr::NamespacedCall {
            namespace,
            name,
            type_params,
            args,
        } => Expr::NamespacedCall {
            namespace: namespace.clone(),
            name: name.clone(),
            type_params: type_params.clone(),
            args: args
                .iter()
                .map(|a| rename_idents_in_expr(a, name_map))
                .collect(),
        },
        Expr::AddressOf {
            entity_name,
            args,
            with_params,
        } => Expr::AddressOf {
            entity_name: entity_name.clone(),
            args: args
                .iter()
                .map(|a| rename_idents_in_expr(a, name_map))
                .collect(),
            with_params: with_params
                .iter()
                .map(|(n, e)| (n.clone(), rename_idents_in_expr(e, name_map)))
                .collect(),
        },
        Expr::Encode { target_type, value } => Expr::Encode {
            target_type: target_type.clone(),
            value: Box::new(rename_idents_in_expr(value, name_map)),
        },
    }
}

/// Collect leaf identifier names introduced by a pattern (idents,
/// deref-of-ident, tuple components recursively).
#[allow(dead_code)]
pub(crate) fn pattern_idents(pat: &Pattern) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(p: &Pattern, out: &mut Vec<String>) {
        match p {
            Pattern::Ident(n) => out.push(n.clone()),
            Pattern::Wildcard | Pattern::None => {}
            Pattern::Tuple(items) => {
                for i in items {
                    walk(i, out);
                }
            }
            Pattern::Deref(inner) | Pattern::Some(inner) => walk(inner, out),
        }
    }
    walk(pat, &mut out);
    out
}

/// Rename leaves of `pat` per `name_map`. Idents not in the map keep
/// their original names. Used together with `rename_idents_in_expr`
/// to give each chain stage's closure params a unique Solidity local
/// per stage. Currently unused (see `rename_idents_in_expr`).
#[allow(dead_code)]
pub(crate) fn rename_pattern_leaves(
    pat: &Pattern,
    name_map: &StdHashMap<String, String>,
) -> Pattern {
    match pat {
        Pattern::Ident(n) => {
            if let Some(new_name) = name_map.get(n) {
                Pattern::Ident(new_name.clone())
            } else {
                pat.clone()
            }
        }
        Pattern::Wildcard | Pattern::None => pat.clone(),
        Pattern::Tuple(items) => Pattern::Tuple(
            items
                .iter()
                .map(|i| rename_pattern_leaves(i, name_map))
                .collect(),
        ),
        Pattern::Deref(inner) => Pattern::Deref(Box::new(rename_pattern_leaves(inner, name_map))),
        Pattern::Some(inner) => Pattern::Some(Box::new(rename_pattern_leaves(inner, name_map))),
    }
}

/// Apply a chain-stage closure with one parameter to `current`. Emits
/// the inner-scope binding statements + body evaluation. Returns the
/// body's `(stmts, value_str)` and the bound names available for
/// subsequent reads (used by `Map` to materialise the new current).
///
/// The closure's parameter names are renamed to fresh per-stage tmps
/// so repeated names across stages (`.filter(|x|...).map(|x|...)`)
/// don't collide.
pub(crate) fn apply_one_arg_closure(
    pat: &Pattern,
    body: &Expr,
    current: &IterElem,
    entity: &Entity,
    _stage_label: &str,
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> Result<(Vec<String>, String), String> {
    // Each stage in `lower_iter_chain` wraps its closure body in its
    // own `{ ... }` Solidity block, so two stages with the same param
    // name (e.g. `.filter(|x| ...).map(|x| ...)`) never share a scope
    // and Solidity's lexical block-scoping handles shadowing for us.
    // We therefore preserve the user's chosen names verbatim and emit
    // readable codegen.
    //
    // Phase EVM-P0-B: register the closure parameter in the
    // `LET_BINDING_TYPES` stack so the body's BinOp / FieldAccess /
    // FnCall type inference resolves the parameter to its concrete
    // element type. Necessary so that
    // `xs.iter().map(|x| x * 2).collect()` over a `Vec<u32>` picks
    // `uint32[]` (not the bare `uint256[]` fallback).
    let mut decls = bind_loop_pattern(pat, current)?;
    let pushed = push_iter_pattern_bindings(pat, current, scratch);
    let (body_stmts, body_val) = gen_expr_hoisted(body, entity, ctx, scope, scratch);
    pop_iter_pattern_bindings(&pushed, scratch);
    decls.extend(body_stmts);
    Ok((decls, body_val))
}

/// Pre-pass over chain stages to determine the element type carried
/// at the terminator. Used by `.collect()` to allocate a typed result
/// array up front, before the body emission walks the stages a second
/// time.
///
/// `start` is the source's `IterElem` shape (the chain emitter's
/// `current` immediately after `resolve_iter_source` factories the
/// first element). For tuple-shaped sources, a `Values` stage
/// projects out the V component; `Map` rebinds to its body's
/// inferred type; other stages preserve the shape.
pub(crate) fn infer_chain_terminator_elem_ty(
    start: &IterElem,
    stages: &[ChainStage],
    entity: &Entity,
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> String {
    // Phase EVM-P0-B: track the current scalar element type as we
    // walk the chain, and push each `.map(|x| body)` closure
    // parameter into the LET_BINDING_TYPES stack so the body's
    // type inference picks up the narrow type of the inbound element.
    // Without this, `xs.iter().map(|x| x * 2).collect()` over a
    // `Vec<u32>` would mis-allocate `uint256[]` because the body's
    // BinOp shape falls back to `uint256` once the closure parameter's
    // type is unknown.
    let mut current_scalar_ty: String = match start {
        IterElem::Scalar { ty, .. } => ty.clone(),
        IterElem::Tuple { .. } => "uint256".to_string(),
    };
    let mut enumerate_elem_ty: Option<String> = None;
    let mut is_tuple = matches!(start, IterElem::Tuple { .. });
    let mut pushed: Vec<String> = Vec::new();
    for stage in stages {
        match stage {
            ChainStage::Values => {
                if let IterElem::Tuple { components } = start {
                    if components.len() >= 2 {
                        current_scalar_ty = components[1].0.clone();
                    }
                }
                is_tuple = false;
            }
            ChainStage::Filter(pat, body) => {
                if is_tuple {
                    let elem = enumerate_elem_ty
                        .as_ref()
                        .unwrap_or(&current_scalar_ty)
                        .clone();
                    let tuple_elem = IterElem::Tuple {
                        components: vec![
                            ("uint256".to_string(), String::new()),
                            (elem, String::new()),
                        ],
                    };
                    pushed.extend(push_iter_pattern_bindings(pat, &tuple_elem, scratch));
                    let _ = infer_let_type_entity(body, entity, ctx, scratch);
                }
            }
            ChainStage::Map(pat, body) => {
                let current_elem = if is_tuple {
                    let elem = enumerate_elem_ty
                        .as_ref()
                        .unwrap_or(&current_scalar_ty)
                        .clone();
                    IterElem::Tuple {
                        components: vec![
                            ("uint256".to_string(), String::new()),
                            (elem, String::new()),
                        ],
                    }
                } else {
                    IterElem::Scalar {
                        ty: current_scalar_ty.clone(),
                        rhs: String::new(),
                    }
                };
                pushed.extend(push_iter_pattern_bindings(pat, &current_elem, scratch));
                let raw = infer_let_type_entity(body, entity, ctx, scratch);
                current_scalar_ty = raw.strip_suffix(" memory").unwrap_or(&raw).to_string();
                is_tuple = false;
            }
            ChainStage::Enumerate => {
                enumerate_elem_ty = Some(current_scalar_ty.clone());
                is_tuple = true;
            }
            ChainStage::Keys | ChainStage::Iter | ChainStage::Take(_) => {
                /* shape preserved */
            }
        }
    }
    for n in pushed.iter().rev() {
        scratch.borrow_mut().pop_let_binding(n);
    }
    let _ = is_tuple;
    if let Some(hint) = super::types::scope_expected_vec_elem_sol_ty(scope, entity, ctx) {
        if let (Some(hw), Some(rw)) = (
            fold_acc_uint_bit_width(&hint),
            fold_acc_uint_bit_width(&current_scalar_ty),
        ) {
            if hw < rw {
                return hint;
            }
        }
    }
    current_scalar_ty
}

/// Lower a parsed chain into a single Solidity for-loop.
pub(crate) fn lower_iter_chain(
    source: &Expr,
    stages: &[ChainStage],
    terminator: &ChainTerminator,
    entity: &Entity,
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> (Vec<String>, String) {
    let idx = scratch.borrow_mut().next_tmp();
    let count_tmp = scratch.borrow_mut().next_tmp();
    let (mut stmts, count_expr, factory) =
        resolve_iter_source(source, entity, ctx, &scope, scratch);

    let mut current = factory(&idx);

    // Pre-pass: simulate stage transitions to determine whether the
    // chain ultimately produces a scalar that collect / fold can
    // accept. This must mirror the actual stage handling below.
    fn projected_terminator_shape(start: &IterElem, stages: &[ChainStage]) -> &'static str {
        let mut is_tuple = matches!(start, IterElem::Tuple { .. });
        for stage in stages {
            match stage {
                ChainStage::Values => is_tuple = false,
                ChainStage::Enumerate => is_tuple = true,
                ChainStage::Map(_, _) => is_tuple = false,
                ChainStage::Keys
                | ChainStage::Iter
                | ChainStage::Filter(_, _)
                | ChainStage::Take(_) => { /* shape preserved */
                }
            }
        }
        if is_tuple {
            "tuple"
        } else {
            "scalar"
        }
    }
    let final_shape = projected_terminator_shape(&current, stages);
    let nested_inner_keys_collect = matches!(
        source,
        Expr::Index(inner, _) if matches!(inner.as_ref(), Expr::Ident(_))
    ) && stages.iter().any(|s| matches!(s, ChainStage::Keys));

    // Allocate the result array (collect) or accumulator (fold) up
    // front, using a one-shot pre-pass over stages to learn the final
    // mapped type for collect.
    let result_name;
    let mut acc_name = String::new();
    let mut collect_pos_local = String::new();
    match terminator {
        ChainTerminator::Collect => {
            if final_shape == "tuple" {
                return (
                    vec!["revert(\"EVM: .collect() over a tuple-yielding chain (e.g. .enumerate().collect()) requires a record/tuple element type - pending EVM-12 Batch C\");".to_string()],
                    "0".to_string(),
                );
            }
            let elem_ty =
                infer_chain_terminator_elem_ty(&current, stages, entity, ctx, scope, scratch);
            let collect_count_local = format!("_cam_collect_n_{}", scratch.borrow_mut().next_tmp());
            collect_pos_local = format!("_cam_collect_pos_{}", scratch.borrow_mut().next_tmp());
            result_name = scratch.borrow_mut().next_tmp();
            stmts.push(format!(
                "uint256 {} = {};",
                collect_count_local,
                count_expr.clone()
            ));
            stmts.push(format!("uint256 {} = 0;", collect_pos_local));
            stmts.push(format!(
                "{0}[] memory {1} = new {0}[]({2});",
                elem_ty, result_name, collect_count_local
            ));
            if nested_inner_keys_collect {
                if let Expr::Index(inner, outer_k) = source {
                    if let Expr::Ident(member_name) = inner.as_ref() {
                        let outer_s = super::expr::gen_expr(outer_k, ctx, &scope, scratch)
                            .unwrap_or_else(|| "/* outer */".to_string());
                        stmts.push(format!(
                            "// lowered from {}[{}].keys().collect() (no per-outer _keys companion yet)",
                            member_name, outer_s
                        ));
                    }
                }
            }
        }
        ChainTerminator::Fold {
            init,
            acc_pat,
            var_pat,
            body,
        } => {
            // EVM-12 Batch C: HashMap accs are unsupported; emit the
            // same workaround diagnostic as the non-chain `gen_fold_loop`.
            if is_hashmap_acc_init(init, entity, ctx, scratch) {
                return (
                    vec!["revert(\"EVM: HashMap accumulator in `.fold` is not supported (Solidity has no in-memory mappings); use a per-entity storage staging map and write into it from the loop body, then read at the end\");".to_string()],
                    "0".to_string(),
                );
            }
            acc_name = match acc_pat {
                Pattern::Ident(n) => n.clone(),
                Pattern::Wildcard => format!("_cam_fold_acc_{}", scratch.borrow_mut().next_tmp()),
                _ => {
                    return (
                        vec!["revert(\"EVM: chain `.fold` accumulator pattern must be an identifier or `_`; use a record-typed acc and reference fields via `acc.field` instead of destructuring\");".to_string()],
                        "0".to_string(),
                    );
                }
            };
            let elem_ty =
                infer_chain_terminator_elem_ty(&current, stages, entity, ctx, scope, scratch);
            let fold_ctx = FoldAccTyCtx {
                acc_name: &acc_name,
                var_pat,
                elem_ty: Some(&elem_ty),
                expected_ty: scope.expected_ty,
            };
            let acc_ty = infer_fold_acc_ty(init, body, entity, ctx, scratch, &fold_ctx);
            let (init_stmts, init_str) = gen_expr_hoisted(init, entity, ctx, scope, scratch);
            stmts.extend(init_stmts);
            stmts.push(format!("{} {} = {};", acc_ty, acc_name, init_str));
            result_name = acc_name.clone();
        }
    }

    // Take counter (allocated before the loop, used inside).
    let taken_var: Option<String> = if stages.iter().any(|s| matches!(s, ChainStage::Take(_))) {
        let t = format!("_cam_taken_{}", scratch.borrow_mut().next_tmp());
        stmts.push(format!("uint256 {} = 0;", t));
        Some(t)
    } else {
        None
    };

    stmts.push(format!("uint256 {} = {};", count_tmp, count_expr));
    stmts.push(format!(
        "for (uint256 {0} = 0; {0} < {1}; ++{0}) {{",
        idx, count_tmp
    ));

    let mut body_lines: Vec<String> = Vec::new();

    for (s_idx, stage) in stages.iter().enumerate() {
        match stage {
            ChainStage::Iter | ChainStage::Keys => { /* pass-through: source already yields elements */ }
            ChainStage::Values => match &current {
                IterElem::Tuple { components } if components.len() >= 2 => {
                    // Tuple-shaped current is `(K, V, ...)` — collapse
                    // to scalar `V`. The chain root must be a
                    // HashMap source (resolve_iter_source produces a
                    // 2-component tuple); other shapes hit the
                    // unsupported arm below.
                    let (v_ty, v_rhs) = components[1].clone();
                    current = IterElem::Scalar {
                        ty: v_ty,
                        rhs: v_rhs,
                    };
                }
                _ => {
                    return (
                        vec!["revert(\"EVM: `.values()` is only supported on HashMap iterators (use `m.values()` directly)\");".to_string()],
                        "0".to_string(),
                    );
                }
            },
            ChainStage::Enumerate => match &current {
                IterElem::Scalar { ty, rhs } => {
                    let elem_tmp = format!("_cam_enum_elem_{}", scratch.borrow_mut().next_tmp());
                    body_lines.push(format!("{} {} = {};", ty, elem_tmp, rhs));
                    current = IterElem::Tuple {
                        components: vec![
                            ("uint256".to_string(), idx.clone()),
                            (ty.clone(), elem_tmp),
                        ],
                    };
                }
                IterElem::Tuple { .. } => {
                    return (
                        vec!["revert(\"EVM: enumerate over a tuple-yielding iterator is unsupported\");".to_string()],
                        "0".to_string(),
                    );
                }
            },
            ChainStage::Filter(pat, body) => {
                let label = format!("filter{}", s_idx);
                body_lines.push("{".to_string());
                let res =
                    apply_one_arg_closure(pat, body, &current, entity, &label, ctx, scope, scratch);
                let (decls, body_val) = match res {
                    Ok(v) => v,
                    Err(e) => {
                        return (
                            vec![format!("revert(\"{}\");", e.replace('"', "\\\""))],
                            "0".to_string(),
                        );
                    }
                };
                for d in decls {
                    body_lines.push(format!("    {}", d));
                }
                body_lines.push(format!("    if (!({})) {{ continue; }}", body_val));
                body_lines.push("}".to_string());
            }
            ChainStage::Map(pat, body) => {
                // Phase EVM-P0-B: temporarily install the closure
                // param's type so the body's narrow integer types
                // survive through `infer_let_type_entity`.
                let pushed = push_iter_pattern_bindings(pat, &current, scratch);
                let new_ty_raw = infer_let_type_entity(body, entity, ctx, scratch);
                pop_iter_pattern_bindings(&pushed, scratch);
                let new_ty_stripped = new_ty_raw
                    .strip_suffix(" memory")
                    .unwrap_or(&new_ty_raw)
                    .to_string();
                let new_local = format!("_cam_map_{}_{}", s_idx, scratch.borrow_mut().next_tmp());
                body_lines.push(format!("{} {};", new_ty_stripped, new_local));
                body_lines.push("{".to_string());
                let label = format!("map{}", s_idx);
                let res =
                    apply_one_arg_closure(pat, body, &current, entity, &label, ctx, scope, scratch);
                let (decls, body_val) = match res {
                    Ok(v) => v,
                    Err(e) => {
                        return (
                            vec![format!("revert(\"{}\");", e.replace('"', "\\\""))],
                            "0".to_string(),
                        );
                    }
                };
                for d in decls {
                    body_lines.push(format!("    {}", d));
                }
                body_lines.push(format!("    {} = {};", new_local, body_val));
                body_lines.push("}".to_string());
                current = IterElem::Scalar {
                    ty: new_ty_stripped,
                    rhs: new_local,
                };
            }
            ChainStage::Take(n_expr) => {
                let (n_stmts, n_str) = gen_expr_hoisted(n_expr, entity, ctx, scope, scratch);
                for ns in n_stmts {
                    body_lines.push(ns);
                }
                let t = taken_var.as_ref().expect("taken_var allocated above");
                body_lines.push(format!("if ({} >= ({})) {{ break; }}", t, n_str));
                body_lines.push(format!("{} = {} + 1;", t, t));
            }
        }
    }

    match terminator {
        ChainTerminator::Collect => {
            let value_expr = match &current {
                IterElem::Scalar { rhs, .. } => rhs.clone(),
                IterElem::Tuple { .. } => {
                    return (
                        vec!["revert(\"EVM: .collect() over a tuple-yielding chain (e.g. .enumerate().collect()) requires a record/tuple element type - pending EVM-12 Batch C\");".to_string()],
                        "0".to_string(),
                    );
                }
            };
            body_lines.push(format!(
                "{}[{}] = {};",
                result_name, collect_pos_local, value_expr
            ));
            body_lines.push(format!(
                "{} = {} + 1;",
                collect_pos_local, collect_pos_local
            ));
        }
        ChainTerminator::Fold { var_pat, body, .. } => {
            body_lines.push("{".to_string());
            let res = apply_one_arg_closure(
                var_pat, body, &current, entity, "foldvar", ctx, scope, scratch,
            );
            let (decls, body_val) = match res {
                Ok(v) => v,
                Err(e) => {
                    return (
                        vec![format!("revert(\"{}\");", e.replace('"', "\\\""))],
                        "0".to_string(),
                    );
                }
            };
            for d in decls {
                body_lines.push(format!("    {}", d));
            }
            body_lines.push(format!("    {} = {};", acc_name, body_val));
            body_lines.push("}".to_string());
        }
    }

    for line in body_lines {
        stmts.push(format!("    {}", line));
    }
    stmts.push("}".to_string());

    if matches!(terminator, ChainTerminator::Collect) {
        // Trim the result memory array's length to the actual count
        // pushed. Solidity has no built-in `.length = n` for memory
        // arrays; the standard idiom is a single mstore.
        stmts.push(format!(
            "assembly {{ mstore({}, {}) }}",
            result_name, collect_pos_local
        ));
    }

    (stmts, result_name)
}

pub(crate) fn gen_for_loop(
    pat: &Pattern,
    iter: &Expr,
    body: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> (Vec<String>, String) {
    // Phase EVM-P0-B: with narrow integer survival, the for-loop's
    // body type now needs to see the loop variable's concrete narrow
    // type so the generated `T[] memory _result` matches the body
    // expression. Push the loop pattern's identifiers + types onto
    // the `LET_BINDING_TYPES` stack before inferring `body`'s type
    // (and pop them again afterwards). Without this, e.g.
    // `for x in xs { x * 2 }` over `xs: Vec<u32>` would pick
    // `uint256[]` for the result instead of `uint32[]`, mismatching
    // the route's `Vec<u32>` return type at the solc level.
    let result = scratch.borrow_mut().next_tmp();
    let idx = scratch.borrow_mut().next_tmp();
    let count_tmp = scratch.borrow_mut().next_tmp();

    let (mut stmts, count_expr, factory) = resolve_iter_source(iter, entity, ctx, &scope, scratch);
    let elem = factory(&idx);
    let bind_decls = match bind_loop_pattern(pat, &elem) {
        Ok(d) => d,
        Err(diag) => {
            return (
                vec![format!(
                    "revert(\"{}\");",
                    diag.replace('\\', "\\\\").replace('"', "\\\"")
                )],
                "0".to_string(),
            );
        }
    };
    let pushed_bindings = push_iter_pattern_bindings(pat, &elem, scratch);

    let body_ty_raw = infer_let_type_entity(body, entity, ctx, scratch);
    let elem_ty = body_ty_raw
        .strip_suffix(" memory")
        .unwrap_or(&body_ty_raw)
        .to_string();

    stmts.push(format!("uint256 {} = {};", count_tmp, count_expr));
    stmts.push(format!(
        "{0}[] memory {1} = new {0}[]({2});",
        elem_ty, result, count_tmp
    ));
    stmts.push(format!(
        "for (uint256 {0} = 0; {0} < {1}; ++{0}) {{",
        idx, count_tmp
    ));
    for d in bind_decls {
        stmts.push(format!("    {}", d));
    }
    let (body_stmts, body_val) = gen_expr_hoisted(body, entity, ctx, scope, scratch);
    for s in body_stmts {
        stmts.push(format!("    {}", s));
    }
    stmts.push(format!("    {}[{}] = {};", result, idx, body_val));
    stmts.push("}".to_string());
    pop_iter_pattern_bindings(&pushed_bindings, scratch);
    (stmts, result)
}

/// When a `for` loop is the entire body of a scalar-returning `pure fn`
/// (`for k in m.keys() { k + 1 } -> u64`), aggregate via `+` instead of
/// materialising a `T[]` (which would not coerce to the declared return).
pub(crate) fn gen_for_scalar_reduce(
    pat: &Pattern,
    iter: &Expr,
    body: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
    expected_ty: Option<&crate::ast::Type>,
) -> (Vec<String>, String) {
    let acc = scratch.borrow_mut().next_tmp();
    let idx = scratch.borrow_mut().next_tmp();
    let count_tmp = scratch.borrow_mut().next_tmp();

    let (mut stmts, count_expr, factory) = resolve_iter_source(iter, entity, ctx, scope, scratch);
    let elem = factory(&idx);
    let bind_decls = match bind_loop_pattern(pat, &elem) {
        Ok(d) => d,
        Err(diag) => {
            return (
                vec![format!(
                    "revert(\"{}\");",
                    diag.replace('\\', "\\\\").replace('"', "\\\"")
                )],
                "0".to_string(),
            );
        }
    };
    let pushed_bindings = push_iter_pattern_bindings(pat, &elem, scratch);

    let acc_ty = infer_scalar_numeric_acc_ty(body, entity, ctx, scratch, expected_ty);

    stmts.push(format!("{} {} = 0;", acc_ty, acc));
    stmts.push(format!("uint256 {} = {};", count_tmp, count_expr));
    stmts.push(format!(
        "for (uint256 {0} = 0; {0} < {1}; ++{0}) {{",
        idx, count_tmp
    ));
    for d in bind_decls {
        stmts.push(format!("    {}", d));
    }
    let body_scope = scope.with_expected_ty(expected_ty);
    let (body_stmts, body_val) = gen_expr_hoisted(body, entity, ctx, &body_scope, scratch);
    for s in body_stmts {
        stmts.push(format!("    {}", s));
    }
    stmts.push(format!("    {} = {} + {};", acc, acc, body_val));
    stmts.push("}".to_string());
    pop_iter_pattern_bindings(&pushed_bindings, scratch);
    (stmts, acc)
}

/// Phase EVM-P0-B: push the loop-pattern's bound names + types onto
/// `LET_BINDING_TYPES` so subsequent body-expression type inference
/// (e.g. `infer_let_type_entity` for the body's return value) can
/// resolve loop variables to their concrete narrow Solidity type.
/// Returns the list of names pushed; pass to `pop_iter_pattern_bindings`
/// after the body has been emitted to keep the stack hygienic.
pub(crate) fn push_iter_pattern_bindings(
    pat: &Pattern,
    elem: &IterElem,
    scratch: &RefCell<EmitScratch>,
) -> Vec<String> {
    let mut pushed = Vec::new();
    match (pat, elem) {
        (Pattern::Ident(name), IterElem::Scalar { ty, .. }) => {
            scratch.borrow_mut().push_let_binding(name, ty);
            pushed.push(name.clone());
        }
        (Pattern::Tuple(pats), IterElem::Tuple { components }) => {
            for (p, (ty, _)) in pats.iter().zip(components.iter()) {
                if let Pattern::Ident(n) = p {
                    scratch.borrow_mut().push_let_binding(n, ty);
                    pushed.push(n.clone());
                }
            }
        }
        _ => {}
    }
    pushed
}

pub(crate) fn pop_iter_pattern_bindings(names: &[String], scratch: &RefCell<EmitScratch>) {
    for n in names.iter().rev() {
        scratch.borrow_mut().pop_let_binding(n);
    }
}

/// Type of a value-producing `for pat in iter { body }` expression — the
/// same `T[] memory` that `gen_for_loop` materialises (see module header).
pub(crate) fn infer_for_expr_type_entity(
    pat: &Pattern,
    iter: &Expr,
    body: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let loop_elem_ty = infer_iter_elem_type_entity(iter, entity, ctx, scratch)
        .map(|t| sol_type_entity(entity, &t, true, ctx))
        .unwrap_or_else(|| "uint256".to_string());

    let pushed = match pat {
        Pattern::Ident(name) => {
            scratch.borrow_mut().push_let_binding(name, &loop_elem_ty);
            vec![name.clone()]
        }
        Pattern::Tuple(pats) => {
            let mut names = Vec::new();
            for p in pats {
                if let Pattern::Ident(n) = p {
                    scratch.borrow_mut().push_let_binding(n, &loop_elem_ty);
                    names.push(n.clone());
                }
            }
            names
        }
        _ => Vec::new(),
    };

    let body_ty_raw = infer_let_type_entity(body, entity, ctx, scratch);
    pop_iter_pattern_bindings(&pushed, scratch);

    let elem_ty = body_ty_raw
        .strip_suffix(" memory")
        .unwrap_or(&body_ty_raw)
        .to_string();
    format!("{}[] memory", elem_ty)
}

// ---------------------------------------------------------------------------
// `<iter>.fold(init, |acc, x| body)` lowering on EVM
// ---------------------------------------------------------------------------
//
// The `.fold` builtin already parses (`Expr::MethodCall("fold", [init,
// Expr::Closure([acc, x], body)])`) and works on the Acki Nacki / native
// Rust targets. On EVM we lower it to an imperative for-loop with an
// accumulator threaded across iterations — the canonical shape for
// bounded numeric iterations like Babylonian sqrt or fixed-point Newton
// refinements:
//
//     T_acc acc = <init>;
//     uint256 _N = <count>;
//     for (uint256 _i = 0; _i < _N; ++_i) {
//         T_var x = <bind: start + _i  OR  vec[_i]>;
//         <body statements>
//         acc = <body value>;
//     }
//     // value of the .fold expression == acc
//
// Supported iterator shapes (sibling of `gen_for_loop`):
//
//   * `Expr::Range(start, end)` → loop bound `(end - start)`,
//     binding `uint256 x = start + _i;`
//   * any other expression that resolves to a `Vec<T>`-typed value →
//     loop bound `vec.length`, binding `T x = vec[_i];`
//
// Supported patterns: `Pattern::Ident` and `Pattern::Wildcard` for both
// `acc` and the loop variable. Tuple-destructured patterns
// (`|(a, b), (i, x)|`, used heavily in `multisig.cam` on Acki Nacki) are
// deferred — they would need either a `record` accumulator or first-class
// tuple locals, neither of which Solidity exposes naturally.
//
// Iterator-method chain prefixes (`.iter().filter(...).map(...).fold(...)`)
// are still not supported; the punt-with-revert in the comment block
// above `gen_for_loop` continues to apply for those.
/// True when a `.keys()` fold closure uses the loop variable as a scalar
/// operand (e.g. `acc + k`), as opposed to only as an index (`m[k]`).
fn fold_body_uses_loop_var_as_scalar_operand(body: &Expr, var_name: &str) -> bool {
    match body {
        Expr::Ident(name) => name == var_name,
        Expr::BinOp(lhs, _, rhs) => {
            matches!(lhs.as_ref(), Expr::Ident(n) if n == var_name)
                || matches!(rhs.as_ref(), Expr::Ident(n) if n == var_name)
                || fold_body_uses_loop_var_as_scalar_operand(lhs.as_ref(), var_name)
                || fold_body_uses_loop_var_as_scalar_operand(rhs.as_ref(), var_name)
        }
        Expr::UnaryOp(_, inner) => {
            fold_body_uses_loop_var_as_scalar_operand(inner.as_ref(), var_name)
        }
        Expr::Block(items) => items
            .iter()
            .any(|e| fold_body_uses_loop_var_as_scalar_operand(e, var_name)),
        _ => false,
    }
}

/// Context for `.fold` accumulator type inference.
pub(crate) struct FoldAccTyCtx<'a> {
    pub acc_name: &'a str,
    pub var_pat: &'a Pattern,
    pub elem_ty: Option<&'a str>,
    pub expected_ty: Option<&'a crate::ast::Type>,
}

fn fold_acc_uint_bit_width(ty: &str) -> Option<u32> {
    ty.strip_prefix("uint").and_then(|n| n.parse().ok())
}

/// Pick the widest compatible numeric accumulator type among candidates.
/// TI-18: drop the `uint256` default when a narrower `uintN` is present.
/// Unify range loop-element types from bound expressions and optional `Vec<T>` hint.
fn pick_range_elem_ty(candidates: Vec<String>, hint: Option<&str>) -> String {
    let best = pick_best_fold_acc_ty(candidates.clone());
    if let Some(h) = hint {
        if candidates.iter().any(|c| c.as_str() == h) {
            return h.to_string();
        }
    }
    best
}

fn infer_range_elem_ty(
    start: &Expr,
    end: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
    vec_elem_hint: Option<&str>,
) -> String {
    let lty = infer_let_type_entity(start, entity, ctx, scratch);
    let rty = infer_let_type_entity(end, entity, ctx, scratch);
    let candidates = if matches!(start, Expr::IntLiteral(_)) {
        vec![rty]
    } else if matches!(end, Expr::IntLiteral(_)) {
        vec![lty]
    } else {
        vec![lty, rty]
    };
    pick_range_elem_ty(candidates, vec_elem_hint)
}

fn pick_best_fold_acc_ty(candidates: Vec<String>) -> String {
    if candidates.is_empty() {
        return "uint256".to_string();
    }
    for c in &candidates {
        if c.ends_with(" memory") || c.starts_with("Option_") || c.starts_with("mapping(") {
            return c.clone();
        }
    }
    let has_narrow_uint = candidates.iter().any(|c| {
        matches!(fold_acc_uint_bit_width(c), Some(w) if w < 256)
    });
    let candidates: Vec<String> = if has_narrow_uint {
        candidates
            .into_iter()
            .filter(|c| c.as_str() != "uint256")
            .collect()
    } else {
        candidates
    };
    let mut best_uint: Option<(u32, String)> = None;
    for c in &candidates {
        let base = c.as_str();
        if let Some(w) = fold_acc_uint_bit_width(base) {
            if best_uint.as_ref().map(|(bw, _)| w > *bw).unwrap_or(true) {
                best_uint = Some((w, c.clone()));
            }
        }
    }
    if let Some((_, ty)) = best_uint {
        return ty;
    }
    candidates
        .iter()
        .find(|c| *c != "uint256")
        .cloned()
        .unwrap_or_else(|| "uint256".to_string())
}

/// Resolve a `pure fn` return type from a synthetic codegen entity
/// (`pure_fn_{name}` or `lib_{Library}_{name}`).
fn pure_fn_return_sol_ty(entity: &Entity, ctx: &EvmCtx) -> Option<String> {
    let fn_name = if let Some(n) = entity.name.strip_prefix("pure_fn_") {
        Some(n.to_string())
    } else if let Some(rest) = entity.name.strip_prefix("lib_") {
        let mut hits: Vec<&String> = ctx
            .pure_fn_returns
            .keys()
            .filter(|fn_name| rest.ends_with(&format!("_{}", fn_name)))
            .collect();
        hits.sort_by_key(|n| n.len());
        hits.last().map(|s| (*s).clone())
    } else {
        None
    };
    fn_name
        .and_then(|n| ctx.lookup_pure_fn_return(&n))
        .map(|t| sol_type_entity(entity, &t, true, ctx))
}

/// Unify numeric accumulator / reduce types from body inference and optional hints.
pub(crate) fn infer_scalar_numeric_acc_ty(
    body: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
    expected_ty: Option<&crate::ast::Type>,
) -> String {
    let body_ty = infer_let_type_entity(body, entity, ctx, scratch);
    let mut candidates = vec![body_ty];
    if let Some(t) = expected_ty {
        candidates.push(sol_type_entity(entity, t, true, ctx));
    }
    if let Some(ret) = pure_fn_return_sol_ty(entity, ctx) {
        candidates.push(ret);
    }
    pick_best_fold_acc_ty(candidates)
}

fn infer_fold_acc_ty_from_shape_tail(
    _init: &Expr,
    body: &Expr,
    init_ty: &str,
    body_ty_unbound: &str,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> Option<String> {
    if init_ty != "uint256" {
        return Some(init_ty.to_string());
    }
    let tail: &Expr = match body {
        Expr::Block(items) => items.last().unwrap_or(body),
        _ => body,
    };
    match tail {
        Expr::RecordConstruct(name, _) => Some(format!("{} memory", name)),
        Expr::RecordUpdate(_, updates) => infer_record_from_update_fields(entity, ctx, updates, scratch)
            .map(|name| format!("{} memory", name))
            .or_else(|| Some(body_ty_unbound.to_string())),
        Expr::Cast(_, _) | Expr::ArrayLit(_) => Some(body_ty_unbound.to_string()),
        _ => None,
    }
}

/// EVM-12 Batch C: pick the Solidity type for a `.fold` accumulator.
///
/// Binds the accumulator (and loop variable when `elem_ty` is known) before
/// inferring the closure body so `acc + m[k]` resolves to the value type,
/// not the key type. Candidates are unified from init, bound body, optional
/// `pure fn` return type, and (only when the key is a scalar operand) the
/// iterator element type.
pub(crate) fn infer_fold_acc_ty(
    init: &Expr,
    body: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
    fold_ctx: &FoldAccTyCtx,
) -> String {
    let init_ty = infer_let_type_entity(init, entity, ctx, scratch);
    let body_ty_unbound = infer_let_type_entity(body, entity, ctx, scratch);

    let tentative_acc = if init_ty != "uint256" {
        init_ty.clone()
    } else {
        pure_fn_return_sol_ty(entity, ctx).unwrap_or_else(|| init_ty.clone())
    };

    scratch
        .borrow_mut()
        .push_let_binding(fold_ctx.acc_name, &tentative_acc);
    let loop_var = match fold_ctx.var_pat {
        Pattern::Ident(var_name) => {
            if let Some(et) = fold_ctx.elem_ty {
                scratch.borrow_mut().push_let_binding(var_name, et);
            }
            Some(var_name.clone())
        }
        _ => None,
    };

    let body_ty = infer_let_type_entity(body, entity, ctx, scratch);

    scratch.borrow_mut().pop_let_binding(fold_ctx.acc_name);
    if let Some(var_name) = loop_var {
        scratch.borrow_mut().pop_let_binding(&var_name);
    }

    let mut candidates = vec![init_ty.clone(), body_ty.clone(), body_ty_unbound.clone()];
    if let Some(shape) = infer_fold_acc_ty_from_shape_tail(
        init,
        body,
        &init_ty,
        &body_ty_unbound,
        entity,
        ctx,
        scratch,
    ) {
        candidates.push(shape);
    }
    if let Some(ret_ty) = pure_fn_return_sol_ty(entity, ctx) {
        candidates.push(ret_ty);
    }
    if let Some(t) = fold_ctx.expected_ty {
        candidates.push(sol_type_entity(entity, t, true, ctx));
    }
    if let (Some(et), Pattern::Ident(var_name)) = (fold_ctx.elem_ty, fold_ctx.var_pat) {
        if fold_body_uses_loop_var_as_scalar_operand(body, var_name) {
            candidates.push(et.to_string());
        }
    }

    // SD-01-EVM-UP: a `pure fn` returning U256 must fold with a uint256 acc
    // even when the init literal is `0` (otherwise TI-18 narrows to uint64).
    if pure_fn_return_sol_ty(entity, ctx).as_deref() == Some("uint256") {
        return "uint256".to_string();
    }

    pick_best_fold_acc_ty(candidates)
}

/// EVM-12 Batch C: detect a fold-acc init that designates a HashMap
/// (either `EmptyCollection` literal flowing into a `HashMap`-shaped
/// inferred type, or any explicitly HashMap-valued init expression).
/// Used by `gen_fold_loop` and `lower_iter_chain` to emit a focused
/// "use a storage staging map" diagnostic instead of silently
/// miscompiling.
pub(crate) fn is_hashmap_acc_init(
    init: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> bool {
    let ty = infer_let_type_entity(init, entity, ctx, scratch);
    // Cambrian HashMaps lower to Solidity `mapping(K => V)` types.
    // Plain `EmptyCollection` infers to a sentinel which won't carry
    // "mapping" — but in fold-acc context the user typically writes
    // `xs.fold({}, |acc, x| acc.insert(...))` where the closure body
    // produces a `HashMap`-shaped result. We catch the body-shape
    // case by checking the closure result-type would propagate to the
    // acc, which is what `infer_let_type_entity` already does for
    // `RecordConstruct` / `Cast` / `If` arms. For the bare
    // `{}`-then-`acc.insert(...)` shape (common with builder chains),
    // we fall back to a heuristic: a `MethodCall` with `insert` /
    // `remove` / `update` whose base is `Expr::Ident(acc-name)` or
    // `EmptyCollection` is a strong signal — the user is treating the
    // acc as a HashMap.
    if ty.starts_with("mapping(") {
        return true;
    }
    // Heuristic: a bare `{}` (`Expr::EmptyCollection`) used as a fold
    // accumulator overwhelmingly means "start with an empty HashMap"
    // — the only other in-memory shape reachable through an empty
    // collection literal is a fresh Vec, which would normally be
    // initialised with `[]`. Flag this so the user gets the explicit
    // staging-map workaround instead of a silent miscompile.
    matches!(init, Expr::EmptyCollection)
}

pub(crate) fn gen_fold_loop(
    iter: &Expr,
    init: &Expr,
    acc_pat: &Pattern,
    var_pat: &Pattern,
    body: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> (Vec<String>, String) {
    if is_hashmap_acc_init(init, entity, ctx, scratch) {
        return (
            vec![
                "revert(\"EVM: HashMap accumulator in `.fold` is not supported (Solidity has no in-memory mappings); use a per-entity storage staging map and write into it from the loop body, then read at the end\");".to_string(),
            ],
            "0".to_string(),
        );
    }

    // Scalar / record acc are both Pattern::Ident in Cambrian (records
    // are referenced by field access, e.g. `acc.total`), and
    // Pattern::Wildcard generates a synthesised name.  Tuple/record
    // *destructure* patterns on the accumulator are not supported on
    // EVM and are rejected with a focused error.
    let acc_name = match acc_pat {
        Pattern::Ident(n) => n.clone(),
        Pattern::Wildcard => format!("_cam_fold_acc_{}", scratch.borrow_mut().next_tmp()),
        _ => {
            return (
                vec![
                    "revert(\"EVM: `.fold` accumulator pattern must be an identifier or `_`; use a record-typed acc and reference fields via `acc.field` instead of destructuring\");"
                        .to_string(),
                ],
                "0".to_string(),
            );
        }
    };

    // Records / strings / arrays come back annotated with " memory".
    // Keep the annotation so the local declaration is well-typed
    // (`MyRec memory acc = MyRec(...);`); for value types like
    // `uint256` the suffix is absent and the declaration stays
    // `uint256 acc = 0;`.
    //
    let idx = scratch.borrow_mut().next_tmp();
    let count_tmp = scratch.borrow_mut().next_tmp();

    let (mut stmts, count_expr, factory) = resolve_iter_source(iter, entity, ctx, &scope, scratch);
    let elem = factory(&idx);
    let elem_ty = match &elem {
        IterElem::Scalar { ty, .. } => ty.clone(),
        IterElem::Tuple { .. } => "uint256".to_string(),
    };
    let fold_ctx = FoldAccTyCtx {
        acc_name: &acc_name,
        var_pat,
        elem_ty: Some(&elem_ty),
        expected_ty: scope.expected_ty,
    };
    let acc_ty = infer_fold_acc_ty(init, body, entity, ctx, scratch, &fold_ctx);
    let bind_decls = match bind_loop_pattern(var_pat, &elem) {
        Ok(d) => d,
        Err(diag) => {
            return (
                vec![format!(
                    "revert(\"{}\");",
                    diag.replace('\\', "\\\\").replace('"', "\\\"")
                )],
                "0".to_string(),
            );
        }
    };

    let (init_stmts, init_str) = gen_expr_hoisted(init, entity, ctx, scope, scratch);
    stmts.extend(init_stmts);
    let init_str = if init_str == "0" && acc_ty.ends_with(" memory") && !acc_ty.starts_with("Option_") {
        let record_name = acc_ty.strip_suffix(" memory").unwrap_or(&acc_ty);
        if is_entity_record(entity, record_name) || ctx.is_program_record(record_name) {
            solidity_default_for_member_ty(entity, &Type::Simple(record_name.to_string()), ctx)
        } else {
            init_str
        }
    } else {
        init_str
    };
    stmts.push(format!("{} {} = {};", acc_ty, acc_name, init_str));

    stmts.push(format!("uint256 {} = {};", count_tmp, count_expr));
    stmts.push(format!(
        "for (uint256 {0} = 0; {0} < {1}; ++{0}) {{",
        idx, count_tmp
    ));
    for d in bind_decls {
        stmts.push(format!("    {}", d));
    }
    let pushed_bindings = push_iter_pattern_bindings(var_pat, &elem, scratch);
    scratch.borrow_mut().push_let_binding(&acc_name, &acc_ty);
    let (body_stmts, body_val) = gen_expr_hoisted(body, entity, ctx, scope, scratch);
    scratch.borrow_mut().pop_let_binding(&acc_name);
    pop_iter_pattern_bindings(&pushed_bindings, scratch);
    for s in body_stmts {
        stmts.push(format!("    {}", s));
    }
    stmts.push(format!("    {} = {};", acc_name, body_val));
    stmts.push("}".to_string());
    (stmts, acc_name)
}
