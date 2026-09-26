// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM codegen — pure functions, macros, stdlib helper free functions.

use super::ctx::{EmitScope, EvmCtx};
use super::expr::gen_expr_hoisted;
use super::iter::gen_for_scalar_reduce;
use super::scratch::EmitScratch;
use super::types::*;
use crate::ast::{Entity, Expr, Macro, Member, Pattern, Program, PureFn, RouteAction, Type};
use std::cell::RefCell;

// ---------------------------------------------------------------------------
// Pure function and macro lowering (Phase EVM-5)
// ---------------------------------------------------------------------------

pub(crate) fn gen_pure_fn_body_hoisted(
    pf: &PureFn,
    synthetic: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> (Vec<String>, String) {
    let scope = EmitScope::for_entity(synthetic).with_expected_ty(Some(&pf.return_type));
    if let Expr::For(pat, iter, body) = &pf.body {
        if is_scalar_numeric_type(&pf.return_type) {
            return gen_for_scalar_reduce(
                pat,
                iter,
                body,
                synthetic,
                ctx,
                &scope,
                scratch,
                Some(&pf.return_type),
            );
        }
    }
    let bound: std::collections::HashSet<String> =
        pf.params.iter().map(|p| p.name.clone()).collect();
    let body = rename_rebound_lets(&pf.body, &bound);
    gen_expr_hoisted(&body, synthetic, ctx, &scope, scratch)
}

/// Solidity rejects redeclaring a name in one scope, so a `let x` that
/// rebinds a parameter or an earlier `let x` on the same spine is renamed
/// to a fresh `x_<n>`. Inner rebindings are renamed first, so a chain of
/// `let x = …;` bindings resolves fully.
fn rename_rebound_lets(expr: &Expr, bound: &std::collections::HashSet<String>) -> Expr {
    let Expr::Let(Pattern::Ident(x), value, body) = expr else {
        return expr.clone();
    };
    let mut inner_bound = bound.clone();
    inner_bound.insert(x.clone());
    let mut body = rename_rebound_lets(body, &inner_bound);
    if !bound.contains(x) || expr_binds_name(&body, x) {
        return Expr::Let(Pattern::Ident(x.clone()), value.clone(), Box::new(body));
    }
    let fresh = (1..)
        .map(|n| format!("{x}_{n}"))
        .find(|c| !bound.contains(c) && !crate::ast::expr_mentions_ident(expr, c))
        .expect("unbounded candidates");
    crate::ast::rename_ident(&mut body, x, &fresh);
    Expr::Let(Pattern::Ident(fresh), value.clone(), Box::new(body))
}

fn pattern_binds(p: &Pattern, name: &str) -> bool {
    match p {
        Pattern::Ident(n) => n == name,
        Pattern::Tuple(ps) => ps.iter().any(|p| pattern_binds(p, name)),
        Pattern::Deref(p) | Pattern::Some(p) => pattern_binds(p, name),
        Pattern::Wildcard | Pattern::None => false,
    }
}

fn expr_binds_name(expr: &Expr, name: &str) -> bool {
    let mut found = false;
    let mut probe = expr.clone();
    crate::ast::map_expr(&mut probe, &mut |e| {
        found |= match e {
            Expr::Let(p, _, _) | Expr::For(p, _, _) => pattern_binds(p, name),
            Expr::Closure(ps, _) => ps.iter().any(|p| pattern_binds(p, name)),
            Expr::Match(_, arms) => arms.iter().any(|a| match &a.pattern {
                crate::ast::MatchPattern::Ident(n) => n == name,
                crate::ast::MatchPattern::EnumVariantWithData(_, _, ps) => {
                    ps.iter().any(|p| pattern_binds(p, name))
                }
                crate::ast::MatchPattern::Some(p) => pattern_binds(p, name),
                _ => false,
            }),
            _ => false,
        };
    });
    found
}

pub(crate) fn gen_pure_fn(pf: &PureFn, ctx: &EvmCtx, scratch: &RefCell<EmitScratch>) -> String {
    // Solidity does not allow `mapping(...)` as a memory parameter — the
    // only legal location is `storage`. When a Cambrian `pure fn` takes a
    // `HashMap<K,V>` we therefore emit it as a `storage` reference and
    // demote the function to `view` (storage reads are not pure in
    // Solidity's sense). The Cambrian-level purity contract is preserved
    // by the validator, which rejects mutating ops in pure-fn bodies.
    let has_mapping_param = pf.params.iter().any(|p| is_mapping_type(&p.ty));

    // EVM-3 Batch G1: track which HashMap params have `.exists()`
    // calls in the body. For each such param we synthesise a parallel
    // `mapping(K => bool) storage <param>_exists` parameter so the
    // body's `<param>_exists[k]` references resolve. Call sites
    // (`Expr::FnCall`) thread the corresponding `<member>_exists`
    // sidecar as the matching actual.
    let exists_params = ctx
        .lookup_pure_fn_exists_params(&pf.name, None)
        .unwrap_or_default();
    let exists_param_set: std::collections::HashSet<&str> =
        exists_params.iter().map(|(_, n)| n.as_str()).collect();
    let keys_params = ctx
        .lookup_pure_fn_keys_params(&pf.name, None)
        .unwrap_or_default();
    let keys_param_set: std::collections::HashSet<&str> =
        keys_params.iter().map(|(_, n)| n.as_str()).collect();

    // Synthesize a stand-in entity whose `members` mirror the function's
    // parameters. This lets `infer_let_type_entity` and
    // `infer_iter_elem_type_entity` resolve `Ident` references to a
    // parameter back to its declared type — required so that
    // `for x in xs { ... }` over a `Vec<T>` parameter knows the element
    // type `T` for the generated Solidity loop binding.
    let synthetic = Entity {
        name: format!("pure_fn_{}", pf.name),
        records: vec![],
        enums: vec![],
        type_aliases: vec![],
        constants: vec![],
        events: vec![],
        errors: vec![],
        macros: vec![],
        routes: vec![],
        members: pf
            .params
            .iter()
            .map(|p| Member {
                name: p.name.clone(),
                ty: p.ty.clone(),
                is_identity: false,
                default_value: None,
                transforms: vec![],
                span: crate::ast::Span::none(),
            })
            .collect(),
        span: crate::ast::Span::none(),
    };
    let (stmts, val) = gen_pure_fn_body_hoisted(&pf, &synthetic, ctx, scratch);
    let ret = match &pf.return_type {
        Type::Tuple(items) => items
            .iter()
            .map(|t| {
                if is_mapping_type(t) {
                    format!(
                        "{} storage",
                        sol_type_entity(&synthetic, t, false, ctx)
                    )
                } else {
                    sol_type_entity(&synthetic, t, true, ctx)
                }
            })
            .collect::<Vec<_>>()
            .join(", "),
        _ if is_mapping_type(&pf.return_type) => format!(
            "{} storage",
            sol_type_entity(&synthetic, &pf.return_type, false, ctx)
        ),
        _ => sol_type_entity(&synthetic, &pf.return_type, true, ctx),
    };
    // Free functions in Solidity cannot have visibility modifiers — only
    // mutability (`pure` / `view` / `payable`). Storage-mapping params
    // demote the function from `pure` to `view`.
    let purity = if has_mapping_param { "view" } else { "pure" };
    // Phase EVM-15 H2 (Cluster A): coerce a bare empty-string literal
    // (`""`) at a return position whose Solidity type is `address` to
    // `address(0)`. The Cambrian source idiom `pure fn zero_address()
    // -> address { "" }` (where `address` aliases `String` for Acki
    // Nacki) is otherwise rejected by solc.
    let return_val = coerce_return_value(&ret, val, &pf.body, &synthetic, ctx, scratch);
    let body_src = format!("{}\n{}", stmts.join("\n"), return_val);
    let mut params: Vec<String> = Vec::new();
    for p in &pf.params {
        let p_name = sol_sanitize_ident(&p.name);
        if is_mapping_type(&p.ty) {
            params.extend(format_hashmap_storage_params(
                &p.ty,
                &p_name,
                &body_src,
                exists_param_set.contains(p.name.as_str()),
                keys_param_set.contains(p.name.as_str()),
            ));
        } else {
            params.push(format!(
                "{} {}",
                sol_type_entity(&synthetic, &p.ty, true, ctx),
                p_name
            ));
        }
    }
    let mut out = format!(
        "function {}({}) {} returns ({}) {{\n",
        pf.name,
        params.join(", "),
        purity,
        ret
    );
    for s in &stmts {
        out.push_str(&format!("    {}\n", s));
    }
    if !stmts
        .iter()
        .any(|s| s.trim_start().starts_with("revert("))
    {
        out.push_str(&format!("    return {};\n", return_val));
    }
    out.push_str("}\n");
    out
}

pub(crate) fn gen_macro_fn(
    mac: &Macro,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let scope = EmitScope::for_entity(entity);
    let params: Vec<String> = mac
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
    let ret = sol_type_entity(entity, &mac.return_type, false, ctx);
    let (stmts, val) = gen_expr_hoisted(&mac.body, entity, ctx, &scope, scratch);
    let mut out = format!(
        "    function macro_{}({}) internal view returns ({}) {{\n",
        mac.name,
        params.join(", "),
        ret
    );
    for s in &stmts {
        out.push_str(&format!("        {}\n", s));
    }
    out.push_str(&format!("        return {};\n    }}\n", val));
    out
}

pub(crate) fn gen_program_pure_fns(
    program: &Program,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    if program.pure_fns.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for pf in &program.pure_fns {
        out.push_str(&gen_pure_fn(pf, ctx, scratch));
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Phase EVM-15 H3 (Cluster B): Cambrian stdlib helpers
// ---------------------------------------------------------------------------
//
// These mirror the Acki Nacki `cam_*` helpers in
// `src/codegen/expr.rs` (`muldiv`, `divc`, `divr`, …). On EVM the
// helpers are emitted as Solidity *free functions* (file-scope,
// before the contract body). Bare `min` / `max` / `clamp` / `divc` /
// `divr` / `divmod` keep those names so a user `pure fn` can shadow
// them; `std::math::<name>` rewrites to `_cam_std_<name>` /
// `_cam_muldiv` and is never shadowed.
//
// Helpers are emitted lazily: a pre-pass over the program AST
// detects which names are actually used, and only those helpers
// land in the output. This keeps the generated Solidity small for
// fixtures that don't rely on the stdlib.

pub(crate) const STDLIB_FN_NAMES: &[&str] =
    &["min", "max", "clamp", "muldiv", "divc", "divr", "divmod"];

/// `std::math::<name>` lowers to `_cam_std_<name>` so a user `pure fn`
/// with the same name never captures the namespaced call.
pub(crate) const STD_MATH_PREFIXED: &[&str] = &["min", "max", "clamp", "divc", "divr", "divmod"];

pub(crate) fn stdlib_fn_used_in_expr(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::FnCall(n, args) => n == name || args.iter().any(|a| stdlib_fn_used_in_expr(a, name)),
        Expr::BinOp(l, _, r) => stdlib_fn_used_in_expr(l, name) || stdlib_fn_used_in_expr(r, name),
        Expr::UnaryOp(_, e) => stdlib_fn_used_in_expr(e, name),
        Expr::Index(b, k) => stdlib_fn_used_in_expr(b, name) || stdlib_fn_used_in_expr(k, name),
        Expr::FieldAccess(b, _) => stdlib_fn_used_in_expr(b, name),
        Expr::MethodCall(b, _, args) => {
            stdlib_fn_used_in_expr(b, name) || args.iter().any(|a| stdlib_fn_used_in_expr(a, name))
        }
        Expr::NamespacedCall { args, .. } => args.iter().any(|a| stdlib_fn_used_in_expr(a, name)),
        Expr::If(c, t, e) => {
            stdlib_fn_used_in_expr(c, name)
                || stdlib_fn_used_in_expr(t, name)
                || e.as_ref()
                    .map_or(false, |x| stdlib_fn_used_in_expr(x, name))
        }
        Expr::Let(_, v, b) => stdlib_fn_used_in_expr(v, name) || stdlib_fn_used_in_expr(b, name),
        Expr::Match(s, arms) => {
            stdlib_fn_used_in_expr(s, name)
                || arms.iter().any(|a| stdlib_fn_used_in_expr(&a.body, name))
        }
        Expr::Tuple(items) | Expr::ArrayLit(items) => {
            items.iter().any(|i| stdlib_fn_used_in_expr(i, name))
        }
        Expr::RecordConstruct(_, fields) | Expr::RecordUpdate(_, fields) => {
            fields.iter().any(|(_, v)| stdlib_fn_used_in_expr(v, name))
        }
        Expr::Cast(e, _) | Expr::Some(e) => stdlib_fn_used_in_expr(e, name),
        Expr::Range(a, b) => stdlib_fn_used_in_expr(a, name) || stdlib_fn_used_in_expr(b, name),
        Expr::For(_, iter, body) => {
            stdlib_fn_used_in_expr(iter, name) || stdlib_fn_used_in_expr(body, name)
        }
        Expr::EnumVariantWithData(_, _, args) => {
            args.iter().any(|a| stdlib_fn_used_in_expr(a, name))
        }
        _ => false,
    }
}

pub(crate) fn stdlib_fn_used_in_action(action: &RouteAction, name: &str) -> bool {
    match action {
        RouteAction::Return { values } => values.iter().any(|v| stdlib_fn_used_in_expr(v, name)),
        RouteAction::Send {
            args,
            dest,
            send_options,
            ..
        } => {
            args.iter().any(|a| stdlib_fn_used_in_expr(a, name))
                || stdlib_fn_used_in_expr(dest, name)
                || send_options
                    .as_ref()
                    .map_or(false, |e| stdlib_fn_used_in_expr(e, name))
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            stdlib_fn_used_in_expr(condition, name)
                || then_actions
                    .iter()
                    .any(|a| stdlib_fn_used_in_action(a, name))
                || else_actions
                    .iter()
                    .any(|a| stdlib_fn_used_in_action(a, name))
        }
        RouteAction::Let { value, .. } => stdlib_fn_used_in_expr(value, name),
        RouteAction::Effect { args, .. } => args.iter().any(|a| stdlib_fn_used_in_expr(a, name)),
        RouteAction::Deploy {
            send_options,
            constructor_args,
            ..
        } => {
            send_options
                .as_ref()
                .map_or(false, |e| stdlib_fn_used_in_expr(e, name))
                || constructor_args
                    .iter()
                    .any(|a| stdlib_fn_used_in_expr(a, name))
        }
        RouteAction::Rescue { action, .. } => stdlib_fn_used_in_action(action, name),
        RouteAction::CallRoute { args, .. } => args.iter().any(|a| stdlib_fn_used_in_expr(a, name)),
        RouteAction::VarCall {
            args,
            dest,
            send_options,
            ..
        } => {
            args.iter().any(|a| stdlib_fn_used_in_expr(a, name))
                || stdlib_fn_used_in_expr(dest, name)
                || send_options
                    .as_ref()
                    .map_or(false, |e| stdlib_fn_used_in_expr(e, name))
        }
        RouteAction::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            update_args.iter().any(|a| stdlib_fn_used_in_expr(a, name))
                || callback_args
                    .iter()
                    .any(|a| stdlib_fn_used_in_expr(a, name))
        }
        RouteAction::For { iter, body, .. } => {
            stdlib_fn_used_in_expr(iter, name)
                || body.iter().any(|a| stdlib_fn_used_in_action(a, name))
        }
        RouteAction::Throw { .. } => false,
        RouteAction::ThrowCustom { args, .. } => {
            args.iter().any(|a| stdlib_fn_used_in_expr(a, name))
        }
        RouteAction::Emit { args, .. } => args.iter().any(|a| stdlib_fn_used_in_expr(a, name)),
    }
}

pub(crate) fn stdlib_fn_used_in_program(program: &Program, name: &str) -> bool {
    for ent in &program.entities {
        for r in &ent.routes {
            if r.body
                .all_actions()
                .iter()
                .any(|a| stdlib_fn_used_in_action(a, name))
            {
                return true;
            }
            for w in &r.where_clauses {
                if stdlib_fn_used_in_expr(&w.condition, name) {
                    return true;
                }
            }
        }
        for m in &ent.members {
            if let Some(d) = &m.default_value {
                if stdlib_fn_used_in_expr(d, name) {
                    return true;
                }
            }
            for t in &m.transforms {
                if stdlib_fn_used_in_expr(&t.body, name) {
                    return true;
                }
            }
        }
    }
    for pf in &program.pure_fns {
        if stdlib_fn_used_in_expr(&pf.body, name) {
            return true;
        }
    }
    false
}

/// `std::crypto::sha256` lowers to `bytes`; a `Vec<u8>` (`uint8[]`) slot
/// takes it through this copy.
pub(crate) const CAM_BYTES_TO_U8S_HELPER: &str = "function _cam_bytes_to_u8s(bytes memory b) pure returns (uint8[] memory out) { out = new uint8[](b.length); for (uint256 i = 0; i < b.length; ++i) { out[i] = uint8(b[i]); } }\n";

pub(crate) fn stdlib_helper_solidity(name: &str) -> Option<&'static str> {
    Some(match name {
        "min" => "function min(uint256 a, uint256 b) pure returns (uint256) { return a < b ? a : b; }\n",
        "max" => "function max(uint256 a, uint256 b) pure returns (uint256) { return a > b ? a : b; }\n",
        "clamp" => "function clamp(uint256 x, uint256 lo, uint256 hi) pure returns (uint256) { return x < lo ? lo : (x > hi ? hi : x); }\n",
        // `floor(a * b / z)` with a 512-bit intermediate (`cam_muldiv`).
        "muldiv" => CAM_STD_MULDIV_WIDE_HELPER,
        // Ceiling division — semantics match TVM Solidity `divc`.
        "divc" => "function divc(uint256 a, uint256 b) pure returns (uint256) { return (a + b - 1) / b; }\n",
        // Rounding division — semantics match TVM Solidity `divr`.
        "divr" => "function divr(uint256 a, uint256 b) pure returns (uint256) { return (a + (b / 2)) / b; }\n",
        "divmod" => "function divmod(uint256 a, uint256 b) pure returns (uint256, uint256) { return (a / b, a % b); }\n",
        _ => return None,
    })
}

pub(crate) fn gen_stdlib_helpers(program: &Program) -> String {
    let mut out = String::new();
    let mut emitted: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
    let needs_muldiv = stdlib_fn_used_in_program(program, "muldiv")
        || std_ns_call_used_in_program(program, "std::math", "muldiv")
        || std_ns_call_used_in_program(program, "std::math", "muldivmod");
    for &name in STDLIB_FN_NAMES {
        // Skip the built-in helper if the user has defined a pure fn
        // with the same name — the user's definition wins at every
        // call site and emitting both produces a Solidity duplicate-
        // function error (e.g. `pure fn min(a: U256, b: U256)` in
        // examples/uniswap-v2/UniswapV2Pair.cam shadows the
        // `min(uint256, uint256)` helper below).
        //
        // `muldiv` is exempt: it emits as `_cam_muldiv`, which cannot
        // collide with a user `pure fn muldiv`, and `std::math::muldiv`
        // / `muldivmod` still need the helper.
        if name != "muldiv" && program.pure_fns.iter().any(|pf| pf.name == name) {
            continue;
        }
        let used = if name == "muldiv" {
            needs_muldiv
        } else {
            stdlib_fn_used_in_program(program, name)
        };
        if used {
            if let Some(body) = stdlib_helper_solidity(name) {
                out.push_str(body);
                emitted.insert(name);
            }
        }
    }
    let minmax = std_ns_call_used_in_program(program, "std::math", "minmax");
    for name in STD_MATH_PREFIXED {
        let used = std_ns_call_used_in_program(program, "std::math", name)
            || (minmax && (*name == "min" || *name == "max"));
        if used {
            if let Some(body) = stdlib_helper_solidity(name) {
                out.push_str(&body.replacen(
                    &format!("function {}(", name),
                    &format!("function _cam_std_{}(", name),
                    1,
                ));
            }
        }
    }
    if method_used_in_program(program, "split") {
        out.push_str(CAM_STRING_SPLIT_HELPER);
    }
    if std_ns_call_used_in_program(program, "std::crypto", "sha256") {
        out.push_str(CAM_BYTES_TO_U8S_HELPER);
    }
    out.push_str(&gen_std_ns_evm_helpers(program));
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

fn std_ns_call_used_in_expr(expr: &Expr, namespace: &str, name: &str) -> bool {
    match expr {
        Expr::NamespacedCall {
            namespace: ns,
            name: n,
            args,
            ..
        } => {
            ns == namespace && n == name
                || args
                    .iter()
                    .any(|a| std_ns_call_used_in_expr(a, namespace, name))
        }
        Expr::BinOp(l, _, r) => {
            std_ns_call_used_in_expr(l, namespace, name)
                || std_ns_call_used_in_expr(r, namespace, name)
        }
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) | Expr::Some(e) => {
            std_ns_call_used_in_expr(e, namespace, name)
        }
        Expr::Index(b, k) => {
            std_ns_call_used_in_expr(b, namespace, name)
                || std_ns_call_used_in_expr(k, namespace, name)
        }
        Expr::MethodCall(b, _, args) => {
            std_ns_call_used_in_expr(b, namespace, name)
                || args
                    .iter()
                    .any(|a| std_ns_call_used_in_expr(a, namespace, name))
        }
        Expr::If(c, t, e) => {
            std_ns_call_used_in_expr(c, namespace, name)
                || std_ns_call_used_in_expr(t, namespace, name)
                || e.as_ref()
                    .map_or(false, |x| std_ns_call_used_in_expr(x, namespace, name))
        }
        Expr::Let(_, v, b) | Expr::For(_, v, b) => {
            std_ns_call_used_in_expr(v, namespace, name)
                || std_ns_call_used_in_expr(b, namespace, name)
        }
        Expr::Match(s, arms) => {
            std_ns_call_used_in_expr(s, namespace, name)
                || arms
                    .iter()
                    .any(|a| std_ns_call_used_in_expr(&a.body, namespace, name))
        }
        Expr::Tuple(items) | Expr::ArrayLit(items) | Expr::Block(items) => items
            .iter()
            .any(|i| std_ns_call_used_in_expr(i, namespace, name)),
        Expr::RecordConstruct(_, fields) | Expr::RecordUpdate(_, fields) => fields
            .iter()
            .any(|(_, v)| std_ns_call_used_in_expr(v, namespace, name)),
        Expr::Range(a, b) => {
            std_ns_call_used_in_expr(a, namespace, name)
                || std_ns_call_used_in_expr(b, namespace, name)
        }
        Expr::Closure(_, body) => std_ns_call_used_in_expr(body, namespace, name),
        Expr::MacroRef(_, args) | Expr::EnumVariantWithData(_, _, args) => args
            .iter()
            .any(|a| std_ns_call_used_in_expr(a, namespace, name)),
        Expr::FnCall(_, args) => args
            .iter()
            .any(|a| std_ns_call_used_in_expr(a, namespace, name)),
        _ => false,
    }
}

fn std_ns_call_used_in_action(action: &RouteAction, namespace: &str, name: &str) -> bool {
    match action {
        RouteAction::Return { values } => values
            .iter()
            .any(|v| std_ns_call_used_in_expr(v, namespace, name)),
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
            args.iter()
                .any(|a| std_ns_call_used_in_expr(a, namespace, name))
                || std_ns_call_used_in_expr(dest, namespace, name)
                || send_options
                    .as_ref()
                    .map_or(false, |e| std_ns_call_used_in_expr(e, namespace, name))
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            std_ns_call_used_in_expr(condition, namespace, name)
                || then_actions
                    .iter()
                    .any(|a| std_ns_call_used_in_action(a, namespace, name))
                || else_actions
                    .iter()
                    .any(|a| std_ns_call_used_in_action(a, namespace, name))
        }
        RouteAction::Let { value, .. } => std_ns_call_used_in_expr(value, namespace, name),
        RouteAction::Effect { args, .. } => args
            .iter()
            .any(|a| std_ns_call_used_in_expr(a, namespace, name)),
        RouteAction::Deploy {
            send_options,
            constructor_args,
            ..
        } => {
            send_options
                .as_ref()
                .map_or(false, |e| std_ns_call_used_in_expr(e, namespace, name))
                || constructor_args
                    .iter()
                    .any(|a| std_ns_call_used_in_expr(a, namespace, name))
        }
        RouteAction::Rescue { action, .. } => std_ns_call_used_in_action(action, namespace, name),
        RouteAction::CallRoute { args, .. }
        | RouteAction::Emit { args, .. }
        | RouteAction::ThrowCustom { args, .. } => args
            .iter()
            .any(|a| std_ns_call_used_in_expr(a, namespace, name)),
        RouteAction::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            update_args
                .iter()
                .any(|a| std_ns_call_used_in_expr(a, namespace, name))
                || callback_args
                    .iter()
                    .any(|a| std_ns_call_used_in_expr(a, namespace, name))
        }
        RouteAction::For { iter, body, .. } => {
            std_ns_call_used_in_expr(iter, namespace, name)
                || body
                    .iter()
                    .any(|a| std_ns_call_used_in_action(a, namespace, name))
        }
        RouteAction::Throw { .. } => false,
    }
}

fn std_ns_call_used_in_program(program: &Program, namespace: &str, name: &str) -> bool {
    for ent in &program.entities {
        for r in &ent.routes {
            if r.body
                .all_actions()
                .iter()
                .any(|a| std_ns_call_used_in_action(a, namespace, name))
            {
                return true;
            }
            for w in &r.where_clauses {
                if std_ns_call_used_in_expr(&w.condition, namespace, name) {
                    return true;
                }
            }
        }
        for m in &ent.members {
            if let Some(d) = &m.default_value {
                if std_ns_call_used_in_expr(d, namespace, name) {
                    return true;
                }
            }
            for t in &m.transforms {
                if std_ns_call_used_in_expr(&t.body, namespace, name) {
                    return true;
                }
            }
        }
    }
    for pf in &program.pure_fns {
        if std_ns_call_used_in_expr(&pf.body, namespace, name) {
            return true;
        }
    }
    false
}

fn std_format_pad6_used_in_expr(expr: &Expr) -> bool {
    match expr {
        Expr::NamespacedCall {
            namespace,
            name,
            args,
            ..
        } if namespace == "std::str" && name == "format" => {
            args.first()
                .is_some_and(|fmt| matches!(fmt, Expr::StringLiteral(s) if s == "{:06}"))
                || args.iter().any(std_format_pad6_used_in_expr)
        }
        Expr::BinOp(l, _, r) => std_format_pad6_used_in_expr(l) || std_format_pad6_used_in_expr(r),
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) | Expr::Some(e) => {
            std_format_pad6_used_in_expr(e)
        }
        Expr::Index(b, k) => std_format_pad6_used_in_expr(b) || std_format_pad6_used_in_expr(k),
        Expr::MethodCall(b, _, args) => {
            std_format_pad6_used_in_expr(b) || args.iter().any(std_format_pad6_used_in_expr)
        }
        Expr::If(c, t, e) => {
            std_format_pad6_used_in_expr(c)
                || std_format_pad6_used_in_expr(t)
                || e.as_ref()
                    .map_or(false, |x| std_format_pad6_used_in_expr(x))
        }
        Expr::Let(_, v, b) | Expr::For(_, v, b) => {
            std_format_pad6_used_in_expr(v) || std_format_pad6_used_in_expr(b)
        }
        Expr::Match(s, arms) => {
            std_format_pad6_used_in_expr(s)
                || arms.iter().any(|a| std_format_pad6_used_in_expr(&a.body))
        }
        Expr::Tuple(items) | Expr::ArrayLit(items) | Expr::Block(items) => {
            items.iter().any(std_format_pad6_used_in_expr)
        }
        Expr::RecordConstruct(_, fields) | Expr::RecordUpdate(_, fields) => {
            fields.iter().any(|(_, v)| std_format_pad6_used_in_expr(v))
        }
        Expr::Range(a, b) => std_format_pad6_used_in_expr(a) || std_format_pad6_used_in_expr(b),
        Expr::Closure(_, body) => std_format_pad6_used_in_expr(body),
        Expr::MacroRef(_, args) | Expr::EnumVariantWithData(_, _, args) => {
            args.iter().any(std_format_pad6_used_in_expr)
        }
        Expr::FnCall(_, args) => args.iter().any(std_format_pad6_used_in_expr),
        _ => false,
    }
}

fn std_format_pad6_used_in_program(program: &Program) -> bool {
    for ent in &program.entities {
        for r in &ent.routes {
            for w in &r.where_clauses {
                if std_format_pad6_used_in_expr(&w.condition) {
                    return true;
                }
            }
            if r.body
                .all_actions()
                .iter()
                .any(|a| std_format_pad6_used_in_action(a))
            {
                return true;
            }
        }
        for m in &ent.members {
            if let Some(d) = &m.default_value {
                if std_format_pad6_used_in_expr(d) {
                    return true;
                }
            }
            for t in &m.transforms {
                if std_format_pad6_used_in_expr(&t.body) {
                    return true;
                }
            }
        }
    }
    program
        .pure_fns
        .iter()
        .any(|pf| std_format_pad6_used_in_expr(&pf.body))
}

fn std_format_pad6_used_in_action(action: &RouteAction) -> bool {
    match action {
        RouteAction::Return { values } => values.iter().any(std_format_pad6_used_in_expr),
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
            args.iter().any(std_format_pad6_used_in_expr)
                || std_format_pad6_used_in_expr(dest)
                || send_options
                    .as_ref()
                    .map_or(false, |e| std_format_pad6_used_in_expr(e))
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            std_format_pad6_used_in_expr(condition)
                || then_actions.iter().any(std_format_pad6_used_in_action)
                || else_actions.iter().any(std_format_pad6_used_in_action)
        }
        RouteAction::Let { value, .. } => std_format_pad6_used_in_expr(value),
        RouteAction::Effect { args, .. } => args.iter().any(std_format_pad6_used_in_expr),
        RouteAction::Deploy {
            send_options,
            constructor_args,
            ..
        } => {
            send_options
                .as_ref()
                .map_or(false, |e| std_format_pad6_used_in_expr(e))
                || constructor_args.iter().any(std_format_pad6_used_in_expr)
        }
        RouteAction::Rescue { action, .. } => std_format_pad6_used_in_action(action),
        RouteAction::CallRoute { args, .. }
        | RouteAction::Emit { args, .. }
        | RouteAction::ThrowCustom { args, .. } => args.iter().any(std_format_pad6_used_in_expr),
        RouteAction::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            update_args.iter().any(std_format_pad6_used_in_expr)
                || callback_args.iter().any(std_format_pad6_used_in_expr)
        }
        RouteAction::For { iter, body, .. } => {
            std_format_pad6_used_in_expr(iter) || body.iter().any(std_format_pad6_used_in_action)
        }
        RouteAction::Throw { .. } => false,
    }
}

fn gen_std_ns_evm_helpers(program: &Program) -> String {
    let mut out = String::new();
    if std_ns_call_used_in_program(program, "std::math", "pow") {
        out.push_str(CAM_STD_POW_HELPER);
    }
    if std_ns_call_used_in_program(program, "std::math", "modpow2") {
        out.push_str(CAM_STD_MODPOW2_HELPER);
    }
    if std_ns_call_used_in_program(program, "std::math", "abs") {
        out.push_str(CAM_STD_ABS_HELPER);
    }
    if std_ns_call_used_in_program(program, "std::math", "sign") {
        out.push_str(CAM_STD_SIGN_HELPER);
    }
    if std_ns_call_used_in_program(program, "std::math", "muldivmod") {
        out.push_str(CAM_STD_MULDIVMOD_HELPER);
    }
    if crate::codegen::std_str::std_str_parse_used_in_program(program) {
        out.push_str(CAM_STD_TRY_PARSE_RADIX_HELPER);
        if crate::codegen::std_str::std_str_parse_needs_signed_helper(program) {
            out.push_str(CAM_STD_TRY_PARSE_RADIX_SIGNED_HELPER);
        }
    }
    if std_ns_call_used_in_program(program, "std::str", "format") {
        out.push_str(CAM_STD_ITOA_HELPER);
        if std_format_pad6_used_in_program(program) {
            out.push_str(CAM_STD_FORMAT_PAD6_HELPER);
        }
    }
    out
}

/// True when any `.<method>(...)` call appears in the program AST.
fn method_used_in_expr(expr: &Expr, method: &str) -> bool {
    match expr {
        Expr::MethodCall(b, m, args) => {
            m == method
                || method_used_in_expr(b, method)
                || args.iter().any(|a| method_used_in_expr(a, method))
        }
        Expr::FnCall(_, args) | Expr::NamespacedCall { args, .. } => {
            args.iter().any(|a| method_used_in_expr(a, method))
        }
        Expr::BinOp(l, _, r) => method_used_in_expr(l, method) || method_used_in_expr(r, method),
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) | Expr::Some(e) => {
            method_used_in_expr(e, method)
        }
        Expr::Index(b, k) => method_used_in_expr(b, method) || method_used_in_expr(k, method),
        Expr::If(c, t, e) => {
            method_used_in_expr(c, method)
                || method_used_in_expr(t, method)
                || e.as_ref().map_or(false, |x| method_used_in_expr(x, method))
        }
        Expr::Let(_, v, b) => method_used_in_expr(v, method) || method_used_in_expr(b, method),
        Expr::Match(s, arms) => {
            method_used_in_expr(s, method)
                || arms.iter().any(|a| method_used_in_expr(&a.body, method))
        }
        Expr::Tuple(items) | Expr::Block(items) => {
            items.iter().any(|i| method_used_in_expr(i, method))
        }
        Expr::RecordConstruct(_, fields) | Expr::RecordUpdate(_, fields) => {
            fields.iter().any(|(_, v)| method_used_in_expr(v, method))
        }
        Expr::Range(a, b) => method_used_in_expr(a, method) || method_used_in_expr(b, method),
        Expr::Closure(_, body) => method_used_in_expr(body, method),
        Expr::MacroRef(_, args) | Expr::EnumVariantWithData(_, _, args) => {
            args.iter().any(|a| method_used_in_expr(a, method))
        }
        _ => false,
    }
}

fn method_used_in_action(action: &RouteAction, method: &str) -> bool {
    match action {
        RouteAction::Return { values } => values.iter().any(|v| method_used_in_expr(v, method)),
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
            args.iter().any(|a| method_used_in_expr(a, method))
                || method_used_in_expr(dest, method)
                || send_options
                    .as_ref()
                    .map_or(false, |e| method_used_in_expr(e, method))
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            method_used_in_expr(condition, method)
                || then_actions
                    .iter()
                    .any(|a| method_used_in_action(a, method))
                || else_actions
                    .iter()
                    .any(|a| method_used_in_action(a, method))
        }
        RouteAction::Let { value, .. } => method_used_in_expr(value, method),
        RouteAction::CallRoute { args, .. }
        | RouteAction::Effect { args, .. }
        | RouteAction::Emit { args, .. }
        | RouteAction::ThrowCustom { args, .. } => {
            args.iter().any(|a| method_used_in_expr(a, method))
        }
        RouteAction::Deploy {
            send_options,
            constructor_args,
            ..
        } => {
            send_options
                .as_ref()
                .map_or(false, |e| method_used_in_expr(e, method))
                || constructor_args
                    .iter()
                    .any(|a| method_used_in_expr(a, method))
        }
        RouteAction::Rescue { action, .. } => method_used_in_action(action, method),
        RouteAction::For { iter, body, .. } => {
            method_used_in_expr(iter, method)
                || body.iter().any(|a| method_used_in_action(a, method))
        }
        RouteAction::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            update_args.iter().any(|a| method_used_in_expr(a, method))
                || callback_args.iter().any(|a| method_used_in_expr(a, method))
        }
        _ => false,
    }
}

fn method_used_in_program(program: &Program, method: &str) -> bool {
    for e in &program.entities {
        for r in &e.routes {
            if r.body
                .all_actions()
                .iter()
                .any(|a| method_used_in_action(a, method))
            {
                return true;
            }
            for w in &r.where_clauses {
                if method_used_in_expr(&w.condition, method) {
                    return true;
                }
            }
        }
        for m in &e.members {
            for t in &m.transforms {
                if method_used_in_expr(&t.body, method) {
                    return true;
                }
            }
        }
    }
    for pf in &program.pure_fns {
        if method_used_in_expr(&pf.body, method) {
            return true;
        }
    }
    false
}

/// Byte-scan `String.split` + atoi/itoa for COBOL/CSV pure helpers
/// (T-EVM-LCC-001 / T-EVM-LCC-002).
#[allow(dead_code)] // reserved; parse helpers currently use radix scanners instead
const CAM_STD_ATOI_HELPER: &str = r#"function _cam_atoi(string memory s) pure returns (int256) {
    bytes memory b = bytes(s);
    int256 sign = 1;
    uint256 i = 0;
    if (b.length > 0 && b[0] == "-") { sign = -1; i = 1; }
    int256 n = 0;
    for (; i < b.length; ++i) {
        uint8 c = uint8(b[i]);
        if (c < 48 || c > 57) break;
        n = n * 10 + int256(uint256(c - 48));
    }
    return n * sign;
}
"#;

const CAM_STD_ITOA_HELPER: &str = r#"function _cam_itoa(int256 v) pure returns (string memory) {
    if (v == 0) return "0";
    bool neg = v < 0;
    uint256 n = uint256(neg ? -v : v);
    bytes memory tmp = new bytes(78);
    uint256 len = 0;
    while (n > 0) {
        tmp[len] = bytes1(uint8(48 + (n % 10)));
        n /= 10;
        unchecked { ++len; }
    }
    bytes memory out = new bytes(neg ? len + 1 : len);
    uint256 j = 0;
    if (neg) { out[0] = "-"; j = 1; }
    for (uint256 k = 0; k < len; ++k) {
        out[j + k] = tmp[len - 1 - k];
    }
    return string(out);
}
"#;

// Port of `cambrian-core` `u512_mul` + `u512_div_by_u256` (`cam_muldiv`):
// 128-bit limbs, restoring division. `a==0 || b==0` returns 0 even when
// `z==0`; otherwise a zero divisor or a quotient that does not fit `uint256`
// reverts. Named `_cam_muldiv` so it cannot collide with a user `pure fn muldiv`.
const CAM_STD_MULDIV_WIDE_HELPER: &str = r#"// floor(a * b / z) with a 512-bit intermediate (docs/STDLIB.md).
function _cam_muldiv(uint256 a, uint256 b, uint256 z) pure returns (uint256) {
    if (a == 0 || b == 0) return 0;
    (uint256 n_hi, uint256 n_lo) = _cam_u512_mul(a, b);
    return _cam_u512_div_u256(n_hi, n_lo, z);
}
function _cam_u512_mul(uint256 a, uint256 b) pure returns (uint256 n_hi, uint256 n_lo) {
    uint256 mask = type(uint128).max;
    uint256 a0 = a & mask;
    uint256 a1 = a >> 128;
    uint256 b0 = b & mask;
    uint256 b1 = b >> 128;
    uint256 c1;
    (n_lo, c1) = _cam_u512_mul_lo(a0, a1, b0, b1, mask);
    n_hi = _cam_u512_mul_hi(a0, a1, b0, b1, mask, c1);
}
function _cam_u512_mul_lo(uint256 a0, uint256 a1, uint256 b0, uint256 b1, uint256 mask) pure returns (uint256 n_lo, uint256 c1) {
    uint256 ll = a0 * b0;
    uint256 t = (ll >> 128) + ((a0 * b1) & mask);
    c1 = t >> 128;
    t = (t & mask) + ((a1 * b0) & mask);
    c1 += t >> 128;
    n_lo = ((t & mask) << 128) | (ll & mask);
}
function _cam_u512_mul_hi(uint256 a0, uint256 a1, uint256 b0, uint256 b1, uint256 mask, uint256 c1) pure returns (uint256 n_hi) {
    uint256 lh = a0 * b1;
    uint256 hl = a1 * b0;
    uint256 hh = a1 * b1;
    uint256 t = (lh >> 128) + (hl >> 128);
    uint256 c2 = t >> 128;
    t = (t & mask) + (hh & mask);
    c2 += t >> 128;
    t = (t & mask) + c1;
    c2 += t >> 128;
    n_hi = (((hh >> 128) + c2) << 128) | (t & mask);
}
function _cam_u512_div_u256(uint256 n_hi, uint256 n_lo, uint256 d) pure returns (uint256 quot) {
    if (n_hi == 0) return n_lo / d;
    uint256 n_lz = _cam_clz(n_hi);
    uint256 d_lz = _cam_clz(d);
    if (n_lz > d_lz + 256) return 0;
    uint256 i = (256 + d_lz) - n_lz + 1;
    while (i > 0) {
        unchecked { --i; }
        (uint256 dsh_hi, uint256 dsh_lo) = _cam_u512_shl_u256(d, i);
        if (_cam_u512_gte(n_hi, n_lo, dsh_hi, dsh_lo)) {
            (n_hi, n_lo) = _cam_u512_sub(n_hi, n_lo, dsh_hi, dsh_lo);
            require(i < 256, "wide_muldiv result exceeds U256::MAX");
            quot += uint256(1) << i;
        }
    }
}
function _cam_u512_gte(uint256 a_hi, uint256 a_lo, uint256 b_hi, uint256 b_lo) pure returns (bool) {
    return a_hi > b_hi || (a_hi == b_hi && a_lo >= b_lo);
}
function _cam_u512_sub(uint256 a_hi, uint256 a_lo, uint256 b_hi, uint256 b_lo) pure returns (uint256 hi, uint256 lo) {
    uint256 borrow = a_lo < b_lo ? 1 : 0;
    lo = borrow == 1 ? (type(uint256).max - b_lo + a_lo + 1) : a_lo - b_lo;
    hi = a_hi - b_hi - borrow;
}
function _cam_u512_shl_u256(uint256 v, uint256 shift) pure returns (uint256 hi, uint256 lo) {
    if (shift == 0) return (0, v);
    if (shift >= 512) return (0, 0);
    if (shift >= 256) {
        uint256 s = shift - 256;
        if (s == 0) return (v, 0);
        return (v << s, 0);
    }
    return (v >> (256 - shift), v << shift);
}
function _cam_clz(uint256 x) pure returns (uint256 n) {
    if (x == 0) return 256;
    n = 255;
    if (x >= 0x100000000000000000000000000000000) { x >>= 128; n -= 128; }
    if (x >= 0x10000000000000000) { x >>= 64; n -= 64; }
    if (x >= 0x100000000) { x >>= 32; n -= 32; }
    if (x >= 0x10000) { x >>= 16; n -= 16; }
    if (x >= 0x100) { x >>= 8; n -= 8; }
    if (x >= 0x10) { x >>= 4; n -= 4; }
    if (x >= 0x4) { x >>= 2; n -= 2; }
    if (x >= 0x2) { n -= 1; }
}
"#;

const CAM_STD_MULDIVMOD_HELPER: &str = r#"function _cam_muldivmod(uint256 x, uint256 y, uint256 z) pure returns (uint256 q, uint256 r) {
    q = _cam_muldiv(x, y, z);
    r = mulmod(x, y, z);
}
"#;

const CAM_STD_ABS_HELPER: &str = r#"function _cam_abs(uint256 x) pure returns (uint256) { return x; }
function _cam_abs(int256 x) pure returns (int256) { return x >= 0 ? x : -x; }
"#;

const CAM_STD_SIGN_HELPER: &str = r#"function _cam_sign(uint256 x) pure returns (int256) { return x > 0 ? int256(1) : int256(0); }
function _cam_sign(int256 x) pure returns (int256) {
    if (x > 0) return 1;
    if (x < 0) return -1;
    return 0;
}
"#;

const CAM_STD_POW_HELPER: &str = r#"function _cam_pow(uint256 base, uint256 exp) pure returns (uint256) {
    if (exp == 0) return 1;
    uint256 result = 1;
    while (exp > 0) {
        if (exp % 2 == 1) result = result * base;
        base = base * base;
        exp /= 2;
    }
    return result;
}
"#;

const CAM_STD_MODPOW2_HELPER: &str = r#"function _cam_modpow2(uint256 x, uint256 n) pure returns (uint256) {
    return x & ((uint256(1) << n) - 1);
}
"#;

const CAM_STD_TRY_PARSE_RADIX_HELPER: &str = r#"function _cam_try_parse_radix(string memory s, uint256 radix, uint256 maxBits) pure returns (bool ok, uint256 value) {
    bytes memory b = bytes(s);
    uint256 i = 0;
    uint256 base = radix;
    if (b.length >= 2 && b[0] == "0" && (b[1] == "x" || b[1] == "X")) {
        if (base != 0 && base != 16) return (false, 0);
        base = 16;
        i = 2;
    } else if (base == 0) {
        base = 10;
    }
    if (base < 2 || base > 36) return (false, 0);
    if (i >= b.length) return (false, 0);
    uint256 maxV = maxBits >= 256 ? type(uint256).max : ((uint256(1) << maxBits) - 1);
    uint256 n = 0;
    for (; i < b.length; ++i) {
        uint8 c = uint8(b[i]);
        uint256 digit;
        if (c >= 48 && c <= 57) {
            digit = uint256(c - 48);
        } else if (c >= 65 && c <= 90) {
            digit = uint256(c - 55);
        } else if (c >= 97 && c <= 122) {
            digit = uint256(c - 87);
        } else {
            return (false, 0);
        }
        if (digit >= base) return (false, 0);
        if (n > type(uint256).max / base) return (false, 0);
        uint256 scaled = n * base;
        if (digit > type(uint256).max - scaled) return (false, 0);
        uint256 next = scaled + digit;
        if (next > maxV) return (false, 0);
        n = next;
    }
    return (true, n);
}

function _cam_parse_radix_or_zero(string memory s, uint256 radix, uint256 maxBits) pure returns (uint256) {
    (bool ok, uint256 v) = _cam_try_parse_radix(s, radix, maxBits);
    return ok ? v : 0;
}
"#;

const CAM_STD_TRY_PARSE_RADIX_SIGNED_HELPER: &str = r#"function _cam_try_parse_radix_signed(string memory s, uint256 radix, uint256 maxBits) pure returns (bool ok, int256 value) {
    bytes memory b = bytes(s);
    bool neg = false;
    uint256 i = 0;
    if (b.length > 0 && b[0] == "-") {
        neg = true;
        i = 1;
    }
    uint256 base = radix;
    if (b.length >= i + 2 && b[i] == "0" && (b[i + 1] == "x" || b[i + 1] == "X")) {
        if (base != 0 && base != 16) return (false, 0);
        base = 16;
        i += 2;
    } else if (base == 0) {
        base = 10;
    }
    if (base < 2 || base > 36) return (false, 0);
    if (i >= b.length) return (false, 0);
    uint256 maxPos = maxBits >= 256 ? type(uint256).max : ((uint256(1) << (maxBits - 1)) - 1);
    uint256 maxMag = neg ? (maxPos + 1) : maxPos;
    uint256 n = 0;
    for (; i < b.length; ++i) {
        uint8 c = uint8(b[i]);
        uint256 digit;
        if (c >= 48 && c <= 57) {
            digit = uint256(c - 48);
        } else if (c >= 65 && c <= 90) {
            digit = uint256(c - 55);
        } else if (c >= 97 && c <= 122) {
            digit = uint256(c - 87);
        } else {
            return (false, 0);
        }
        if (digit >= base) return (false, 0);
        if (n > type(uint256).max / base) return (false, 0);
        uint256 scaled = n * base;
        if (digit > type(uint256).max - scaled) return (false, 0);
        uint256 next = scaled + digit;
        if (next > maxMag) return (false, 0);
        n = next;
    }
    int256 out = neg ? -int256(n) : int256(n);
    return (true, out);
}

function _cam_parse_radix_signed_or_zero(string memory s, uint256 radix, uint256 maxBits) pure returns (int256) {
    (bool ok, int256 v) = _cam_try_parse_radix_signed(s, radix, maxBits);
    return ok ? v : int256(0);
}
"#;

const CAM_STD_FORMAT_PAD6_HELPER: &str = r#"function _cam_format_pad6(uint256 v, uint256 width) pure returns (string memory) {
    string memory t = _cam_itoa(int256(v));
    bytes memory tb = bytes(t);
    if (tb.length >= width) return t;
  bytes memory out = new bytes(width);
  uint256 pad = width - tb.length;
  for (uint256 i = 0; i < pad; ++i) { out[i] = "0"; }
  for (uint256 j = 0; j < tb.length; ++j) { out[pad + j] = tb[j]; }
  return string(out);
}
"#;

const CAM_STRING_SPLIT_HELPER: &str = r#"function _cam_string_split(string memory s, string memory delim) pure returns (string[] memory) {
    bytes memory sb = bytes(s);
    bytes memory db = bytes(delim);
    if (db.length == 0) {
        string[] memory one = new string[](1);
        one[0] = s;
        return one;
    }
    uint256 count = 1;
    for (uint256 i = 0; i + db.length <= sb.length; ++i) {
        bool match_ = true;
        for (uint256 j = 0; j < db.length; ++j) {
            if (sb[i + j] != db[j]) { match_ = false; break; }
        }
        if (match_) {
            unchecked { ++count; }
            i += db.length - 1;
        }
    }
    string[] memory parts = new string[](count);
    uint256 start = 0;
    uint256 idx = 0;
    for (uint256 i = 0; i + db.length <= sb.length; ++i) {
        bool match_ = true;
        for (uint256 j = 0; j < db.length; ++j) {
            if (sb[i + j] != db[j]) { match_ = false; break; }
        }
        if (match_) {
            parts[idx] = _cam_string_slice(sb, start, i);
            unchecked { ++idx; }
            start = i + db.length;
            i += db.length - 1;
        }
    }
    parts[idx] = _cam_string_slice(sb, start, sb.length);
    return parts;
}
function _cam_string_slice(bytes memory sb, uint256 start, uint256 end_) pure returns (string memory) {
    bytes memory out = new bytes(end_ - start);
    for (uint256 i = start; i < end_; ++i) {
        out[i - start] = sb[i];
    }
    return string(out);
}
"#;
