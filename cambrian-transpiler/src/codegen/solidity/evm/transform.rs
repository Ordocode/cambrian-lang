// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM codegen — mapping / member transform lowering.

use crate::ast::{Entity, Expr, Pattern, Type};
use std::collections::HashMap as StdHashMap;

use super::analysis::member_inner_uses_exists;
use crate::codegen::solidity::core::ctx::EmitScope;
use crate::codegen::solidity::core::ctx::EvmCtx;
use crate::codegen::solidity::core::expr::{gen_expr, gen_expr_hoisted};
use crate::codegen::solidity::core::scratch::EmitScratch;
use crate::codegen::solidity::core::types::*;
use std::cell::RefCell;

// ---------------------------------------------------------------------------
// Mapping member transform — emit mutation statements directly
// ---------------------------------------------------------------------------

/// Resolve a variable through a binding map (for nested mapping tracking).
pub(super) fn resolve_binding<'a>(
    expr: &'a Expr,
    bindings: &'a StdHashMap<String, Expr>,
) -> &'a Expr {
    if let Expr::Ident(name) = expr {
        if let Some(bound) = bindings.get(name) {
            return bound;
        }
    }
    expr
}

/// Collect (key_path, value) pairs from chained insert/update calls.
/// Returns the root mapping name and the list of (key_expr, value_expr) pairs.
pub(super) fn collect_update_chain<'a>(
    expr: &'a Expr,
    bindings: &StdHashMap<String, Expr>,
    member_name: &str,
) -> (String, Vec<(&'a Expr, &'a Expr)>) {
    match expr {
        Expr::MethodCall(base, method, args)
            if (method == "insert" || method == "update") && args.len() == 2 =>
        {
            let (root, mut ops) = collect_update_chain(base, bindings, member_name);
            ops.push((&args[0], &args[1]));
            (root, ops)
        }
        Expr::Ident(name) => (name.clone(), vec![]),
        Expr::EmptyCollection => (member_name.to_string(), vec![]),
        _ => {
            // Try to resolve through bindings
            let resolved = resolve_binding(expr, bindings);
            if let Expr::Index(map_base, _) = resolved {
                if let Expr::Ident(n) = map_base.as_ref() {
                    return (n.clone(), vec![]);
                }
            }
            (member_name.to_string(), vec![])
        }
    }
}

/// Build a key path prefix for nested mapping access.
/// e.g. bindings["inner"] = m_allowances[sender] → prefix = "[sender]"
pub(super) fn key_path_for_binding(
    base: &Expr,
    bindings: &StdHashMap<String, Expr>,
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let resolved = resolve_binding(base, bindings);
    match resolved {
        Expr::Index(_, key) => gen_expr(key, ctx, &scope, scratch)
            .map(|k| format!("[{}]", k))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Generate Solidity statements for a mapping-typed member transform body.
/// Handles: let bindings, insert/update/remove chains, nested mappings.
/// Look up the Solidity type of a HashMap member's key (or `uint256` as a
/// fallback if the member isn't actually a HashMap).
///
/// Phase EVM-15 H2: callers use this for emitting *local* `_cam_tmp_*`
/// snapshot declarations, so reference types must carry the `memory`
/// data-location annotation (`in_param=true`). Pre-H2 the `false`
/// path produced bare `string`/`Pool` locals which solc rejects with
/// "Data location must be storage, memory or calldata".
pub(super) fn mapping_key_type_for(
    entity: &Entity,
    member_name: &str,
    ctx: &EvmCtx,
    _scope: &EmitScope,
    _scratch: &RefCell<EmitScratch>,
) -> String {
    if let Some(m) = entity.members.iter().find(|m| m.name == member_name) {
        if let Type::Generic(g, params) = &m.ty {
            if g == "HashMap" && !params.is_empty() {
                return sol_type_entity(entity, &params[0], true, ctx);
            }
        }
    }
    "uint256".to_string()
}

/// Look up the Solidity type of a HashMap member's value (or `uint256` as a
/// fallback if the member isn't actually a HashMap). See
/// [`mapping_key_type_for`] for the rationale on `in_param=true`.
pub(super) fn mapping_value_type_for(
    entity: &Entity,
    member_name: &str,
    ctx: &EvmCtx,
    _scope: &EmitScope,
    _scratch: &RefCell<EmitScratch>,
) -> String {
    if let Some(m) = entity.members.iter().find(|m| m.name == member_name) {
        if let Type::Generic(g, params) = &m.ty {
            if g == "HashMap" && params.len() >= 2 {
                return sol_type_entity(entity, &params[1], true, ctx);
            }
        }
    }
    "uint256".to_string()
}

/// Phase EVM-15 H2 (Cluster A): look up the *inner* HashMap key type
/// for a member typed `HashMap<K1, HashMap<K2, V>>`. Used by the
/// nested-update lowering so the inner-key snapshot temp gets `K2`
/// rather than the previous hard-coded `uint256` (which broke
/// `m_allowances[address][address] = ...` in token).
pub(super) fn nested_mapping_inner_key_type_for(
    entity: &Entity,
    member_name: &str,
    ctx: &EvmCtx,
    _scope: &EmitScope,
    _scratch: &RefCell<EmitScratch>,
) -> String {
    if let Some(m) = entity.members.iter().find(|m| m.name == member_name) {
        if let Type::Generic(g, params) = &m.ty {
            if g == "HashMap" && params.len() >= 2 {
                if let Type::Generic(g2, p2) = &params[1] {
                    if g2 == "HashMap" && !p2.is_empty() {
                        return sol_type_entity(entity, &p2[0], true, ctx);
                    }
                }
            }
        }
    }
    "uint256".to_string()
}

/// Companion to [`nested_mapping_inner_key_type_for`] for the inner
/// value type of `HashMap<K1, HashMap<K2, V>>`.
pub(super) fn nested_mapping_inner_value_type_for(
    entity: &Entity,
    member_name: &str,
    ctx: &EvmCtx,
    _scope: &EmitScope,
    _scratch: &RefCell<EmitScratch>,
) -> String {
    if let Some(m) = entity.members.iter().find(|m| m.name == member_name) {
        if let Type::Generic(g, params) = &m.ty {
            if g == "HashMap" && params.len() >= 2 {
                if let Type::Generic(g2, p2) = &params[1] {
                    if g2 == "HashMap" && p2.len() >= 2 {
                        return sol_type_entity(entity, &p2[1], true, ctx);
                    }
                }
            }
        }
    }
    "uint256".to_string()
}

/// Lower one mapping member's `in route(...) => transform_body` into two
/// streams of Solidity statements:
///
/// - `setup` reads the **pre-route** state and stashes every key, value,
///   and condition into freshly named locals (`_pmk_*`, `_pmv_*`,
///   `_pmc_*`). It does **not** touch storage.
/// - `writes` mutates the storage mapping using only those locals — every
///   `m_x[_pmk_n] = _pmv_n;` is independent of what other transforms
///   read.
///
/// `emit_transforms_sol_split` interleaves the `setup` of every transform
/// (mapping or scalar) into the route body up front, then drains every
/// transform's `writes` afterwards. That preserves the Cambrian
/// functional-update guarantee: every transform's RHS observes the same
/// pre-route snapshot of every member, regardless of source order.
///
/// Without the split, a mapping transform's `m_x[k] = v;` fires inline,
/// and any subsequent transform that reads `m_x[k]` (or even the same
/// transform's later reads) sees the just-written post-state — the
/// SSTORE-before-READ bug that surfaced in `delegate(to)` where
/// `m_delegates`'s commit corrupted `m_votes`'s "old delegate" lookup.
pub(super) fn gen_mapping_transform_split(
    member_name: &str,
    expr: &Expr,
    entity: &Entity,
    bindings: &mut StdHashMap<String, Expr>,
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> (Vec<String>, Vec<String>) {
    let mut setup: Vec<String> = Vec::new();
    let mut writes: Vec<String> = Vec::new();

    match expr {
        Expr::Let(Pattern::Ident(name), val, body) => {
            // EVM-3 Batch G2: HashMap-typed let bindings can't lower
            // to a Solidity local (mappings are storage-only).
            // Substitute uses of `name` inside `body` with the bound
            // expression and recurse, skipping the local emission.
            // Mirrors the same special case in `gen_expr_hoisted`.
            let simplified_val =
                simplify_hashmap_let_alias_init(val).unwrap_or_else(|| (**val).clone());
            if is_hashmap_valued_expr(&simplified_val, entity) {
                bindings.insert(name.clone(), simplified_val.clone());
                let substituted = subst_hashmap_let_in_expr(body, name, &simplified_val);
                let (s2, w2) = gen_mapping_transform_split(
                    member_name,
                    &substituted,
                    entity,
                    bindings,
                    ctx,
                    scope,
                    scratch,
                );
                setup.extend(s2);
                writes.extend(w2);
                return (setup, writes);
            }
            if let Expr::Tuple(elems) = val.as_ref() {
                if tuple_contains_hashmap_slot(elems, entity)
                    || (name.starts_with('_') && elems.len() > 1)
                {
                    let (s2, w2) = gen_mapping_transform_split(
                        member_name,
                        body,
                        entity,
                        bindings,
                        ctx,
                        scope,
                        scratch,
                    );
                    setup.extend(s2);
                    writes.extend(w2);
                    return (setup, writes);
                }
            }
            let (val_stmts, val_str) = gen_expr_hoisted(val, entity, ctx, scope, scratch);
            let let_ty = infer_let_type_entity(val, entity, ctx, scratch);
            setup.extend(val_stmts);
            let val_str = maybe_narrow_cast(&let_ty, &val_str, val, entity, ctx, scratch);
            setup.push(format!(
                "{} {} = {};",
                let_ty,
                sol_sanitize_ident(name),
                val_str
            ));
            bindings.insert(name.clone(), *val.clone());
            scratch.borrow_mut().push_let_binding(name, &let_ty);
            if let Expr::EnumVariantWithData(en, var, _) = val.as_ref() {
                scratch
                    .borrow_mut()
                    .push_payload_enum_binding(name, en, var);
            }
            let (s2, w2) = gen_mapping_transform_split(
                member_name,
                body,
                entity,
                bindings,
                ctx,
                scope,
                scratch,
            );
            scratch.borrow_mut().pop_let_binding(name);
            if matches!(val.as_ref(), Expr::EnumVariantWithData(_, _, _)) {
                scratch.borrow_mut().pop_payload_enum_binding(name);
            }
            setup.extend(s2);
            writes.extend(w2);
        }
        Expr::Let(Pattern::Wildcard, _, body) => {
            let (s, w) = gen_mapping_transform_split(
                member_name,
                body,
                entity,
                bindings,
                ctx,
                scope,
                scratch,
            );
            setup.extend(s);
            writes.extend(w);
        }
        Expr::MethodCall(base, method, args)
            if (method == "insert" || method == "update") && args.len() == 2 =>
        {
            let indexed_inner_outer = match base.as_ref() {
                Expr::Index(inner, outer_k) => {
                    if matches!(inner.as_ref(), Expr::Ident(n) if n == member_name) {
                        Some(outer_k.as_ref())
                    } else {
                        None
                    }
                }
                _ => None,
            };
            if let Some(outer_k) = indexed_inner_outer {
                // `m[outer].update(ik, iv)` after HashMap-let substitution.
                let root = member_name;
                let outer_tmp = scratch.borrow_mut().next_tmp();
                let outer_str = gen_expr(outer_k, ctx, &scope, scratch)
                    .unwrap_or_else(|| "/* key */".to_string());
                let ok_ty = mapping_key_type_for(entity, member_name, ctx, scope, scratch);
                setup.push(format!("{} {} = {};", ok_ty, outer_tmp, outer_str));

                let ik_tmp = scratch.borrow_mut().next_tmp();
                let ik_str = gen_expr(&args[0], ctx, &scope, scratch)
                    .unwrap_or_else(|| "/* key */".to_string());
                let ik_ty =
                    nested_mapping_inner_key_type_for(entity, member_name, ctx, scope, scratch);
                setup.push(format!("{} {} = {};", ik_ty, ik_tmp, ik_str));

                let iv_tmp = scratch.borrow_mut().next_tmp();
                let (iv_stmts, iv_str) = gen_expr_hoisted(&args[1], entity, ctx, scope, scratch);
                setup.extend(iv_stmts);
                let iv_ty = nested_mapping_inner_value_type_for(
                    entity,
                    member_name,
                    ctx,
                    scope,
                    scratch,
                );
                let iv_str = maybe_narrow_cast(&iv_ty, &iv_str, &args[1], entity, ctx, scratch);
                setup.push(format!("{} {} = {};", iv_ty, iv_tmp, iv_str));

                writes.push(format!(
                    "{}[{}][{}] = {};",
                    root, outer_tmp, ik_tmp, iv_tmp
                ));
                if member_inner_uses_exists(entity, member_name) {
                    writes.push(format!(
                        "{}_inner_exists[{}][{}] = true;",
                        root, outer_tmp, ik_tmp
                    ));
                }
            } else {
            // EVM-13: when this HashMap is iterated anywhere in the
            // entity, every insert/update has to maintain the parallel
            // `<m>_keys` array. Resolved once per insert chain — the
            // entity walk is O(routes × actions × expr-size), so we
            // hoist it out of the per-operation loop below.
            let iterated = ctx.hashmap_member_is_iterated(entity, member_name);
            let has_exists_sidecar = ctx.member_has_exists_sidecar(&entity.name, member_name);
            let (root, ops) = collect_update_chain(expr, bindings, member_name);
            for (k, v) in ops {
                let k_tmp = scratch.borrow_mut().next_tmp();
                let k_str =
                    gen_expr(k, ctx, &scope, scratch).unwrap_or_else(|| "/* key */".to_string());
                let k_ty = mapping_key_type_for(entity, member_name, ctx, scope, scratch);
                setup.push(format!("{} {} = {};", k_ty, k_tmp, k_str));

                match v {
                    Expr::MethodCall(inner_base, inner_method, inner_args)
                        if (inner_method == "update" || inner_method == "insert")
                            && inner_args.len() == 2 =>
                    {
                        // Nested mapping: m_x[k1][k2] = v. We snapshot
                        // every sub-key + value into temps and emit the
                        // assignment at write time. Inner-key/value types
                        // are uint256 by default (precise resolution would
                        // require walking nested HashMap params, which no
                        // in-tree example exercises today).
                        //
                        // EVM-3 Batch G2: when the inner update's base
                        // resolves (after binding-substitution) to the
                        // *same* outer-key index — the canonical shape
                        // `m.update(k, m[k].update(k2, v))` produced by
                        // collapsing `let inner = m[k]; ...` — the
                        // outer key already covers the first
                        // subscript, so path_prefix must stay empty
                        // and we emit `m[k1][k2] = v` exactly once.
                        // Without this guard the previous pipeline
                        // doubled the prefix and produced an invalid
                        // triple-subscript Solidity assignment.
                        let resolved_inner = resolve_binding(inner_base, bindings);
                        let path_prefix = if let Expr::Index(b, _) = &resolved_inner {
                            if matches!(b.as_ref(), Expr::Ident(n) if n == member_name) {
                                String::new()
                            } else {
                                key_path_for_binding(inner_base, bindings, ctx, scope, scratch)
                            }
                        } else {
                            key_path_for_binding(inner_base, bindings, ctx, scope, scratch)
                        };
                        let (_, inner_ops) = collect_update_chain(v, bindings, member_name);
                        let inner_emits_sidecar = member_inner_uses_exists(entity, member_name);
                        let ik_ty = nested_mapping_inner_key_type_for(
                            entity,
                            member_name,
                            ctx,
                            scope,
                            scratch,
                        );
                        let iv_ty = nested_mapping_inner_value_type_for(
                            entity,
                            member_name,
                            ctx,
                            scope,
                            scratch,
                        );
                        for (ik, iv) in inner_ops {
                            let ik_tmp = scratch.borrow_mut().next_tmp();
                            let ik_str = gen_expr(ik, ctx, &scope, scratch)
                                .unwrap_or_else(|| "/* key */".to_string());
                            setup.push(format!("{} {} = {};", ik_ty, ik_tmp, ik_str));

                            let iv_tmp = scratch.borrow_mut().next_tmp();
                            let (iv_stmts, iv_str) =
                                gen_expr_hoisted(iv, entity, ctx, scope, scratch);
                            setup.extend(iv_stmts);
                            // Narrow when the RHS is wider than the declared
                            // mapping value type (e.g. `block.timestamp + delay`
                            // is `uint256` but the slot is `uint64`) — otherwise
                            // solc 9574 rejects the assignment (T-G-001).
                            let iv_str =
                                maybe_narrow_cast(&iv_ty, &iv_str, iv, entity, ctx, scratch);
                            setup.push(format!("{} {} = {};", iv_ty, iv_tmp, iv_str));

                            writes.push(format!(
                                "{}[{}]{}[{}] = {};",
                                root, k_tmp, path_prefix, ik_tmp, iv_tmp
                            ));
                            // EVM-3 Batch G2: maintain the 2-level
                            // sidecar so subsequent `.exists()` reads
                            // (which lower to `m_inner_exists[...]`)
                            // observe the just-written entry.
                            if inner_emits_sidecar && path_prefix.is_empty() {
                                writes.push(format!(
                                    "{}_inner_exists[{}][{}] = true;",
                                    root, k_tmp, ik_tmp
                                ));
                            }
                        }
                    }
                    _ => {
                        let (v_stmts, v_str) = gen_expr_hoisted(v, entity, ctx, scope, scratch);
                        setup.extend(v_stmts);
                        let v_ty = mapping_value_type_for(entity, member_name, ctx, scope, scratch);
                        // Narrow when the RHS is wider than the declared
                        // mapping value type (e.g. `sys::timestamp + delay`
                        // → `uint256` RHS into a `uint64` slot). Mirrors the
                        // scalar member-transform path (T-G-001 / solc 9574).
                        let v_str = maybe_narrow_cast(&v_ty, &v_str, v, entity, ctx, scratch);
                        let v_rhs = if v_ty.starts_with("mapping(") {
                            // Solidity rejects `mapping` locals — assign the
                            // lowered RHS directly into the storage slot.
                            v_str
                        } else {
                            let v_tmp = scratch.borrow_mut().next_tmp();
                            setup.push(format!("{} {} = {};", v_ty, v_tmp, v_str));
                            v_tmp
                        };
                        // EVM-13: flip the existence flag on first
                        // insert, and — when the map is iterated —
                        // also push the key into the parallel index
                        // array. This must run *before* the value write
                        // so subsequent reads (e.g. via `m.exists(k)`
                        // later in the route) see a consistent state.
                        if has_exists_sidecar {
                            writes.push(format!("if (!{}_exists[{}]) {{", root, k_tmp));
                            if iterated {
                                writes.push(format!(
                                    "    {}_keys_index[{}] = {}_keys.length;",
                                    root, k_tmp, root
                                ));
                                writes.push(format!("    {}_keys.push({});", root, k_tmp));
                            }
                            writes.push(format!("    {}_exists[{}] = true;", root, k_tmp));
                            writes.push("}".to_string());
                        }
                        let slot_ref = format!("{}[{}]", root, k_tmp);
                        let normalized_rhs = if v_rhs.contains(&k_str) {
                            v_rhs.replace(&k_str, &k_tmp)
                        } else {
                            v_rhs.clone()
                        };
                        if normalized_rhs == slot_ref {
                            writes.push(format!(
                                "// HashMap alias index (self-update elided): {}[{}]",
                                root,
                                k_str
                            ));
                        } else {
                            writes.push(format!("{}[{}] = {};", root, k_tmp, v_rhs));
                        }
                    }
                }
            }
            }
        }
        Expr::MethodCall(base, method, args) if method == "remove" && args.len() == 1 => {
            let base_str = if let Expr::Ident(n) = base.as_ref() {
                n.clone()
            } else {
                member_name.to_string()
            };
            let k_tmp = scratch.borrow_mut().next_tmp();
            let k_str =
                gen_expr(&args[0], ctx, &scope, scratch).unwrap_or_else(|| "/* key */".to_string());
            let k_ty = mapping_key_type_for(entity, member_name, ctx, scope, scratch);
            setup.push(format!("{} {} = {};", k_ty, k_tmp, k_str));
            // EVM-13: when iterated, swap-pop the key out of the
            // parallel index array. Standard idiom: read the popped
            // key's index, overwrite that slot with the last key, fix
            // the relocated key's reverse-index entry, then `pop()`.
            // The `if` guard around the whole block makes a
            // double-remove a no-op rather than corrupting the array
            // (would otherwise underflow `m_keys.length - 1`).
            if ctx.hashmap_member_is_iterated(entity, member_name) {
                let idx_tmp = scratch.borrow_mut().next_tmp();
                let last_tmp = scratch.borrow_mut().next_tmp();
                let last_key_tmp = scratch.borrow_mut().next_tmp();
                writes.push(format!("if ({}_exists[{}]) {{", base_str, k_tmp));
                writes.push(format!(
                    "    uint256 {} = {}_keys_index[{}];",
                    idx_tmp, base_str, k_tmp
                ));
                writes.push(format!(
                    "    uint256 {} = {}_keys.length - 1;",
                    last_tmp, base_str
                ));
                writes.push(format!("    if ({} != {}) {{", idx_tmp, last_tmp));
                writes.push(format!(
                    "        {} {} = {}_keys[{}];",
                    k_ty, last_key_tmp, base_str, last_tmp
                ));
                writes.push(format!(
                    "        {}_keys[{}] = {};",
                    base_str, idx_tmp, last_key_tmp
                ));
                writes.push(format!(
                    "        {}_keys_index[{}] = {};",
                    base_str, last_key_tmp, idx_tmp
                ));
                writes.push("    }".to_string());
                writes.push(format!("    {}_keys.pop();", base_str));
                writes.push(format!("    delete {}_keys_index[{}];", base_str, k_tmp));
                writes.push(format!("    delete {}_exists[{}];", base_str, k_tmp));
                writes.push("}".to_string());
            } else if ctx.member_has_exists_sidecar(&entity.name, member_name) {
                // Not iterated, so there is no key array to swap-pop,
                // but the sidecar still has to follow the value —
                // otherwise `.exists(k)` keeps answering true for an
                // entry that was just removed.
                writes.push(format!("delete {}_exists[{}];", base_str, k_tmp));
            }
            writes.push(format!("delete {}[{}];", base_str, k_tmp));
        }
        Expr::If(cond, then_b, else_b) => {
            // Snapshot the condition into a local up front so it observes
            // the pre-route state. The branches' setup and writes then
            // both live inside the conditional block on the write side,
            // because each branch's setup declares branch-local temps
            // that the writes consume.
            let cond_tmp = scratch.borrow_mut().next_tmp();
            let cond_str =
                gen_expr(cond, ctx, &scope, scratch).unwrap_or_else(|| "false".to_string());
            setup.push(format!("bool {} = {};", cond_tmp, cond_str));

            let mut then_bindings = bindings.clone();
            let (then_setup, then_writes) = gen_mapping_transform_split(
                member_name,
                then_b,
                entity,
                &mut then_bindings,
                ctx,
                scope,
                scratch,
            );

            let mut block: Vec<String> = vec![format!("if ({}) {{", cond_tmp)];
            for s in &then_setup {
                block.push(format!("    {}", s));
            }
            for s in &then_writes {
                block.push(format!("    {}", s));
            }

            if let Some(else_b) = else_b {
                let mut else_bindings = bindings.clone();
                let (else_setup, else_writes) = gen_mapping_transform_split(
                    member_name,
                    else_b,
                    entity,
                    &mut else_bindings,
                    ctx,
                    scope,
                    scratch,
                );
                block.push("} else {".to_string());
                for s in &else_setup {
                    block.push(format!("    {}", s));
                }
                for s in &else_writes {
                    block.push(format!("    {}", s));
                }
            }
            block.push("}".to_string());
            writes.extend(block);
        }
        Expr::Block(items) => {
            for it in items {
                let (s, w) = gen_mapping_transform_split(
                    member_name,
                    it,
                    entity,
                    bindings,
                    ctx,
                    scope,
                    scratch,
                );
                setup.extend(s);
                writes.extend(w);
            }
        }
        Expr::Ident(name) if name == member_name => {}
        Expr::EmptyCollection => {}
        // Phase EVM-15 H3 (Cluster B): `HashMap::new()` (parsed as
        // `EnumVariantWithData("HashMap", "new", [])`) is a no-op
        // for storage-resident mappings — Solidity initialises
        // them empty by default. Without this arm the generic
        // fallback below emits the
        // `revert("EVM: enum variant HashMap::new with data not
        // representable")` placeholder.
        Expr::EnumVariantWithData(en, var, _) if en == "HashMap" && var == "new" => {}
        other => {
            // Phase EVM-2 K3 (defence in depth): the validator's E17
            // rejects any HashMap member-transform body that doesn't
            // match a recognised shape (insert/update/remove + their
            // composition wrappers). Anything that still falls through
            // here either bypassed validation or has a synthesised
            // shape from a pre-pass — fail loudly in debug, emit a
            // self-documenting marker in release.
            debug_assert!(
                false,
                "EVM-2 K3: HashMap transform `{}` body shape should have been rejected by validator E17: {:?}",
                member_name, other
            );
            let (stmts, _val) = gen_expr_hoisted(other, entity, ctx, scope, scratch);
            if stmts.iter().all(|s| s.starts_with("//")) || stmts.is_empty() {
                writes.push(format!(
                    "// EVM-2 K3: mapping `{}` transform shape should have been rejected by E17",
                    member_name
                ));
            } else {
                writes.extend(stmts);
            }
        }
    }

    (setup, writes)
}
