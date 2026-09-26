// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Solidity core — expression lowering (language + EVM domain arms).
//!
//! Domain-specific arms (`AddressOf`, `msg::`, `sys::`, `evm::`, CREATE2) live
//! here for P5; a future pass may split a thin adapter dispatcher in `evm/expr`.

use crate::ast::{BinOp, Entity, EnumDecl, Expr, MatchArm, MatchPattern, Pattern, Type, UnaryOp};
use crate::ir::{CoerceKind, ResolvedType, TypedExpr, TypedExprKind};
use std::cell::RefCell;

use super::create2::gen_create2_address_expr;
use super::ctx::{EmitScope, EvmCtx};
use super::iter::{gen_fold_loop, gen_for_loop, lower_iter_chain, parse_iter_chain};
use super::scratch::EmitScratch;
use super::option::{option_some_from_sol_inner, strip_memory_ty};
use super::state::{is_payload_enum, payload_enum_construct, payload_enum_field_name};
use super::types::*;
use crate::codegen::solidity::unlowered::UNLOWERED_VALUE_MARKER;

pub(crate) fn gen_expr_address_deterministic(
    entity_name: &str,
    args: &[Expr],
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> Option<String> {
    let scope = EmitScope::none();
    let identity_exprs: Vec<String> = args
        .iter()
        .filter_map(|e| gen_expr(e, ctx, &scope, scratch))
        .collect();
    if identity_exprs.len() != args.len() {
        return None;
    }
    Some(gen_create2_address_expr(entity_name, &identity_exprs))
}

/// `Lib::fn(args)` parses as `EnumVariantWithData("Lib", "fn", args)`.
fn library_qualifies(qualifier: &str, fn_name: &str, ctx: &EvmCtx) -> bool {
    ctx.library_has_fn(qualifier, fn_name)
}

fn hashmap_keys_sidecar_actual(
    arg_expr: &Expr,
    param_idx: usize,
    fn_name: &str,
    lib_qualifier: Option<&str>,
    entity: Option<&Entity>,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> Option<String> {
    if let Expr::Ident(arg_name) = arg_expr {
        if let Some(e) = entity {
            if e.members.iter().any(|m| m.name == *arg_name)
                && ctx.hashmap_member_is_iterated(e, arg_name)
            {
                return Some(format!("{}_keys", arg_name));
            }
        }
        if let Some(ty) = scratch.borrow().lookup_let_binding(arg_name) {
            if ty.starts_with("mapping(") && ty.contains("storage") && !arg_name.ends_with("_keys")
            {
                return Some(format!("{}_keys", arg_name));
            }
        }
        let needs_keys = ctx
            .lookup_pure_fn_keys_params(fn_name, lib_qualifier)
            .map(|ks| ks.iter().any(|(idx, _)| *idx == param_idx))
            .unwrap_or(false);
        if needs_keys {
            return Some(format!("{}_keys", arg_name));
        }
        if let Some(params) = ctx.lookup_pure_fn_params(fn_name) {
            if let Some(Type::Generic(g, ps)) = params.get(param_idx) {
                if g == "HashMap" && !ps.is_empty() {
                    return Some(format!("new {}[](0)", sol_type(&ps[0], false)));
                }
            }
        }
    }
    None
}

fn hashmap_exists_sidecar_actual(
    arg_expr: &Expr,
    entity: Option<&Entity>,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> Option<String> {
    if let Expr::Ident(arg_name) = arg_expr {
        if let Some(e) = entity {
            if e.members.iter().any(|m| m.name == *arg_name)
                && ctx.member_has_exists_sidecar(&e.name, arg_name)
            {
                return Some(format!("{}_exists", arg_name));
            }
        }
        if let Some(ty) = scratch.borrow().lookup_let_binding(arg_name) {
            if ty.starts_with("mapping(") && ty.contains("storage") && !arg_name.ends_with("_exists")
            {
                return Some(format!("{}_exists", arg_name));
            }
        }
    }
    None
}

fn weave_pure_fn_sidecars(
    fn_name: &str,
    lib_qualifier: Option<&str>,
    args: &[Expr],
    mut rendered: Vec<String>,
    entity: Option<&Entity>,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> Option<Vec<String>> {
    if let Some(exists_params) = ctx.lookup_pure_fn_exists_params(fn_name, lib_qualifier) {
        let mut woven: Vec<String> = Vec::with_capacity(rendered.len() + exists_params.len());
        for (i, slot) in rendered.drain(..).enumerate() {
            woven.push(slot);
            if exists_params.iter().any(|(idx, _)| *idx == i) {
                if let Some(sidecar) =
                    hashmap_exists_sidecar_actual(args.get(i)?, entity, ctx, scratch)
                {
                    woven.push(sidecar);
                } else {
                    return None;
                }
            }
        }
        rendered = woven;
    }
    if let Some(keys_params) = ctx.lookup_pure_fn_keys_params(fn_name, lib_qualifier) {
        let mut woven: Vec<String> = Vec::with_capacity(rendered.len() + keys_params.len());
        for (i, slot) in rendered.drain(..).enumerate() {
            woven.push(slot);
            if keys_params.iter().any(|(idx, _)| *idx == i) {
                if let Some(sidecar) = hashmap_keys_sidecar_actual(
                    args.get(i)?,
                    i,
                    fn_name,
                    lib_qualifier,
                    entity,
                    ctx,
                    scratch,
                ) {
                    // When the mapping arg lowered as `sut.m_balances()`, qualify
                    // the parallel `_keys` sidecar as `sut.m_balances_keys()`.
                    let qualified = woven
                        .last()
                        .and_then(|slot| slot.rsplit_once('.'))
                        .map(|(prefix, _)| format!("{}.{sidecar}", prefix = prefix))
                        .unwrap_or(sidecar);
                    woven.push(qualified);
                } else {
                    return None;
                }
            }
        }
        rendered = woven;
    }
    Some(rendered)
}

fn format_pure_fn_call(
    fn_name: &str,
    lib_qualifier: Option<&str>,
    args: &[Expr],
    rendered: Vec<String>,
    entity: Option<&Entity>,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> Option<String> {
    let lib = lib_qualifier
        .map(|s| s.to_string())
        .or_else(|| ctx.lookup_library_for_fn(fn_name));
    let woven = weave_pure_fn_sidecars(
        fn_name,
        lib.as_deref(),
        args,
        rendered,
        entity,
        ctx,
        scratch,
    )?;
    let qualified = match &lib {
        Some(lib) => format!("{}.{}", lib, fn_name),
        None => fn_name.to_string(),
    };
    Some(format!("{}({})", qualified, woven.join(", ")))
}

fn narrow_pure_fn_args(
    fn_name: &str,
    args: &[Expr],
    rendered: &mut [String],
    scope: &EmitScope,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) {
    if let Some(param_types) = ctx.lookup_pure_fn_params(fn_name) {
        let entity_for_cast = scope.entity;
        for (i, slot) in rendered.iter_mut().enumerate() {
            if let Some(pty) = param_types.get(i) {
                let target = sol_type(pty, true);
                if target.starts_with("int") {
                    if let Some(Expr::UnaryOp(_, inner)) = args.get(i) {
                        if let Expr::IntLiteral(v) = inner.as_ref() {
                            *slot = format!("{}(-{})", target, v);
                            continue;
                        }
                    }
                }
                *slot = match (entity_for_cast.as_ref(), args.get(i)) {
                    (Some(e), Some(arg_expr)) => {
                        maybe_narrow_cast(&target, slot, arg_expr, e, ctx, scratch)
                    }
                    _ => wrap_narrow_cast(&target, slot),
                };
            }
        }
    }
}

/// Solidity-specific binop coercion (P6 C2 residual).
///
/// Portable [`crate::ir::coerce_binop_operands`] targets Rust CamCast rules;
/// this mirrors the mixed signed/unsigned → `int256` promotion in this printer.
fn solidity_coerce_binop_operands(
    mut lhs: TypedExpr,
    mut rhs: TypedExpr,
    op: &BinOp,
    lty: &str,
    rty: &str,
) -> (TypedExpr, TypedExpr) {
    if matches!(op, BinOp::And | BinOp::Or) {
        return (lhs, rhs);
    }
    if !matches!(
        op,
        BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Mod
            | BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge
            | BinOp::Eq
            | BinOp::Ne
    ) {
        return (lhs, rhs);
    }
    let l_signed = lty.starts_with("int") && lty != "int_literal";
    let r_signed = rty.starts_with("int") && rty != "int_literal";
    let l_unsigned = lty.starts_with("uint");
    let r_unsigned = rty.starts_with("uint");
    if (l_signed && r_unsigned) || (l_unsigned && r_signed) {
        let wrap = |te: TypedExpr| TypedExpr {
            ty: ResolvedType::simple("int256"),
            kind: TypedExprKind::Coerce {
                kind: CoerceKind::SignPromote,
                expr: Box::new(te),
                to: ResolvedType::simple("int256"),
            },
        };
        lhs = wrap(lhs);
        rhs = wrap(rhs);
    }
    (lhs, rhs)
}

fn print_coerced_sol_operand(te: &TypedExpr, rendered: &str, source_sol_ty: &str) -> String {
    match &te.kind {
        TypedExprKind::Coerce {
            kind: CoerceKind::SignPromote,
            ..
        } => {
            if source_sol_ty.starts_with("uint") {
                format!("int256(uint256({}))", rendered)
            } else {
                format!("int256({})", rendered)
            }
        }
        TypedExprKind::Coerce {
            kind: CoerceKind::Narrow,
            to,
            ..
        } => {
            let target = match to {
                ResolvedType::Simple(name) => name.as_str(),
                _ => "uint256",
            };
            wrap_narrow_cast_with_actual(target, rendered, Some(source_sol_ty))
        }
        _ => rendered.to_string(),
    }
}

pub(crate) fn gen_binop_operands_solidity(
    lhs: &Expr,
    rhs: &Expr,
    op: &BinOp,
    entity: &Entity,
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> Option<(String, String)> {
    let l = gen_expr(lhs, ctx, scope, scratch)?;
    let r = gen_expr(rhs, ctx, scope, scratch)?;
    let lty = infer_expr_sol_ty(lhs, entity, ctx, scratch, InferSolMode::Flow);
    let rty = infer_expr_sol_ty(rhs, entity, ctx, scratch, InferSolMode::Flow);
    let te_l = TypedExpr {
        ty: ResolvedType::simple("unknown"),
        kind: TypedExprKind::AstPassthrough(Box::new(lhs.clone())),
    };
    let te_r = TypedExpr {
        ty: ResolvedType::simple("unknown"),
        kind: TypedExprKind::AstPassthrough(Box::new(rhs.clone())),
    };
    let (te_l, te_r) = solidity_coerce_binop_operands(te_l, te_r, op, &lty, &rty);
    let mut ls = print_coerced_sol_operand(&te_l, &l, &lty);
    let mut rs = print_coerced_sol_operand(&te_r, &r, &rty);
    if matches!(
        op,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
    ) {
        if lty == "address" && rty == "int_literal" {
            rs = maybe_narrow_cast("address", &rs, rhs, entity, ctx, scratch);
        } else if rty == "address" && lty == "int_literal" {
            ls = maybe_narrow_cast("address", &ls, lhs, entity, ctx, scratch);
        } else if lty == "bytes4" && rty == "int_literal" {
            rs = maybe_narrow_cast("bytes4", &rs, rhs, entity, ctx, scratch);
        } else if rty == "bytes4" && lty == "int_literal" {
            ls = maybe_narrow_cast("bytes4", &ls, lhs, entity, ctx, scratch);
        }
    }
    if matches!(
        op,
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
    ) {
        let numeric_or_lit = |ty: &str| {
            ty.starts_with("uint")
                || ty.starts_with("int")
                || ty == "int256"
                || ty == "uint256"
                || ty == "int_literal"
        };
        let address_as_uint = |addr_operand: &str| -> String {
            let cast_ty = scope_expected_sol_ty(scope, entity, ctx)
                .filter(|t| t.starts_with("uint") && *t != "uint256")
                .unwrap_or_else(|| "uint256".to_string());
            if cast_ty == "uint256" {
                format!("uint256(uint160({}))", addr_operand)
            } else {
                format!("{}(uint160({}))", cast_ty, addr_operand)
            }
        };
        if lty == "address" && numeric_or_lit(&rty) {
            ls = address_as_uint(&ls);
        }
        if rty == "address" && numeric_or_lit(&lty) {
            rs = address_as_uint(&rs);
        }
    }
    if matches!(op, BinOp::And | BinOp::Or) {
        let ls = if lty == "bool" { ls } else { format!("({} != 0)", ls) };
        let rs = if rty == "bool" { rs } else { format!("({} != 0)", rs) };
        return Some((ls, rs));
    }
    Some((ls, rs))
}

/// When a single-field record is used as a scalar operand (common in
/// `.fold(0, |acc, x| Rec { field: acc + x })`), read the lone field.
fn array_memory_sum_fn(elem_ty: &str) -> &'static str {
    match elem_ty {
        "uint64" => "_cam_sum_uint64",
        _ => "_cam_sum_uint256",
    }
}

fn record_scalar_binop_lhs(
    lhs: &Expr,
    ls: &str,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let lty = infer_let_type_entity(lhs, entity, ctx, scratch);
    if lty.ends_with(" memory") && !lty.starts_with("Option_") {
        let rec_name = lty.strip_suffix(" memory").unwrap_or(&lty);
        if let Some(rec) = entity
            .records
            .iter()
            .find(|r| r.name == rec_name)
            .or_else(|| ctx.lookup_record(rec_name))
        {
            if rec.fields.len() == 1 {
                return format!("{}.{}", ls, rec.fields[0].name);
            }
        }
    }
    ls.to_string()
}

fn solidity_length_expr(
    base: &Expr,
    base_str: &str,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let base_ty = infer_let_type_entity(base, entity, ctx, scratch);
    if base_ty == "string memory" || base_ty == "string" {
        format!("bytes({}).length", base_str)
    } else {
        format!("{}.length", base_str)
    }
}

pub(crate) fn binop_str(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::WrappingAdd => "+",
        BinOp::WrappingSub => "-",
        BinOp::WrappingMul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
    }
}

pub(crate) fn gen_temporal_ref_sol(member_name: &str, scope: &EmitScope) -> String {
    let storage = sol_sanitize_ident(member_name);
    let Some(entity) = scope.entity else {
        return storage;
    };
    let Some(route) = scope.route else {
        return storage;
    };
    let member = match entity.members.iter().find(|m| m.name == member_name) {
        Some(m) => m,
        None => return storage,
    };
    let transform = match member
        .transforms
        .iter()
        .find(|t| t.route_name == *route && t.phase.as_deref() == scope.phase)
    {
        Some(t) => t,
        None => return storage,
    };
    if is_mapping_type(&member.ty) {
        return storage;
    }
    if is_vec_type(&member.ty) && body_is_member_push(&transform.body, member_name) {
        return storage;
    }
    format!("next_{}", member_name)
}

fn body_is_member_push(body: &Expr, member_name: &str) -> bool {
    let mut cursor = body;
    loop {
        match cursor {
            Expr::Let(_, _, inner) => cursor = inner,
            Expr::Block(items) => match items.last() {
                Some(e) => cursor = e,
                None => return false,
            },
            Expr::MethodCall(base, method, args) if method == "push" && args.len() == 1 => {
                return matches!(base.as_ref(), Expr::Ident(n) if n == member_name);
            }
            _ => return false,
        }
    }
}

pub(crate) fn payload_or_unit_enum_cond(subj: &str, en: &str, vn: &str, ctx: &EvmCtx) -> String {
    if let Some(decl) = ctx.lookup_enum(en) {
        if is_payload_enum(&decl) {
            return format!("{}.tag == {}_Tag.{}", subj, en, vn);
        }
    }
    format!("{} == {}.{}", subj, en, vn)
}

pub(crate) fn subst_payload_bindings(
    body: &Expr,
    subj_expr: &Expr,
    decl: &EnumDecl,
    variant: &str,
    payload_pats: &[Pattern],
) -> Expr {
    let mut out = body.clone();
    if let Some(v) = decl.variants.iter().find(|v| v.name == variant) {
        for (i, pat) in payload_pats.iter().enumerate() {
            if let Pattern::Ident(binder) = pat {
                let field = payload_enum_field_name(v, i);
                let access = Expr::FieldAccess(Box::new(subj_expr.clone()), field);
                out = subst_ident_in_expr(&out, binder, &access);
            }
        }
    }
    out
}

pub(crate) fn sol_msg_field(field: &str) -> Option<&'static str> {
    match field {
        "sender" => Some("msg.sender"),
        "value" => Some("msg.value"),
        "timestamp" | "createdAt" => Some("block.timestamp"),
        "logicaltime" => Some("block.number"),
        // `msg::int` / `msg::ext` distinguish internal vs external TVM
        // messages; on EVM every entry point is "external", so `int`
        // is always false and `ext` is always true.
        "int" => Some("false"),
        "ext" => Some("true"),
        // `msg::body` and `msg::currencies` and `msg::pubkey` are TVM-
        // specific (raw cell body, ECC-7 currency map, sender pubkey).
        // None of them have an EVM analogue — fall through to None so
        // the validator can flag use sites with E12.
        _ => None,
    }
}

pub(crate) fn sol_sys_field(field: &str) -> Option<&'static str> {
    match field {
        // `sys::*` carries portable system-context reads. The
        // canonical cross-target names are listed first; the older
        // TVM-flavoured aliases (`now`, `logicaltime`, `rnd_seed`)
        // remain for backwards compatibility.
        "timestamp" | "now" => Some("block.timestamp"),
        "block_number" | "logicaltime" => Some("block.number"),
        "prevrandao" | "rnd_seed" => Some("uint256(block.prevrandao)"),
        "address" => Some("address(this)"),
        "balance" => Some("address(this).balance"),
        "coinbase" => Some("block.coinbase"),
        "chainid" => Some("block.chainid"),
        "basefee" => Some("block.basefee"),
        "gas_left" => Some("gasleft()"),
        // EIP-4844 / Cancun: per-block blob base fee. Lives in
        // `block.*` like its sibling base-fee accessor.
        "blobbasefee" => Some("block.blobbasefee"),
        // Transaction-context reads: `tx.origin` (the EOA that
        // initiated the call chain) and `tx.gasprice` (the legacy /
        // effective gas price of the current transaction). These
        // sit in `sys::*` rather than `evm::*` because they are
        // zero-arg context reads rather than precompile-style
        // function calls.
        "origin" => Some("tx.origin"),
        "gasprice" => Some("tx.gasprice"),
        // `sys::pubkey` / `sys::seqno` are TVM-only (the contract's
        // own keypair). EVM has no equivalent — the validator should
        // route these to E12. Falling through to None surfaces the
        // "/* unsupported expr */" sentinel if someone bypasses it.
        _ => None,
    }
}

/// Mirrors the arms of [`gen_evm_ns`].
pub(crate) fn evm_ns_call_supported(name: &str, arity: usize) -> bool {
    match name {
        "ecrecover" => arity == 4,
        "keccak256Packed" | "sha256" | "ripemd160" => arity >= 1,
        "keccak256" | "balance" | "blockhash" => arity == 1,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// `evm::*` namespace lowering (adapter domain)
// ---------------------------------------------------------------------------
// reserved for **state-changing / EVM-specific impure operations** that
// have no portable analogue (future: `evm::selfdestruct`, `evm::create2(...)`,
// direct opcode access, etc.). Pure reads of system context (timestamp,
// address, balance, block number, ...) belong in `sys::*` instead.
//
// Pure EVM-target intrinsics also live here when they have no portable
// analogue: `evm::ecrecover(hash, v, r, s) -> address` (signature
// recovery) and `evm::keccak256Packed(args...) -> bytes32`
// (`abi.encodePacked` flavour of `hashOf`, needed for EIP-712 prefix
// hashes). They're flagged as `evm::` rather than promoted to bare
// `FnCall` because they are EVM-specific — using them on a non-EVM
// target should fail at validation time.
pub(crate) fn gen_evm_ns(
    name: &str,
    args: &[Expr],
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> Option<String> {
    match name {
        "ecrecover" if args.len() == 4 => {
            let rendered: Vec<String> = args
                .iter()
                .filter_map(|e| gen_expr(e, ctx, &scope, scratch))
                .collect();
            if rendered.len() != args.len() {
                return None;
            }
            // Solidity `ecrecover` is `(bytes32, uint8, bytes32, bytes32)
            // -> address`. Cast every argument explicitly so the call
            // typechecks when the Cambrian source spells `hash`/`r`/`s`
            // as `U256` (which is the type `evm::keccak256Packed` and
            // `hashOf` both return).
            Some(format!(
                "ecrecover(bytes32({}), uint8({}), bytes32({}), bytes32({}))",
                rendered[0], rendered[1], rendered[2], rendered[3],
            ))
        }
        // `evm::keccak256(x)` — `abi.encode` flavour, same as `hashOf(x)`.
        "keccak256" if args.len() == 1 => {
            let v = gen_expr(&args[0], ctx, &scope, scratch)?;
            Some(format!("uint256(keccak256(abi.encode({})))", v))
        }
        "keccak256Packed" if !args.is_empty() => {
            let rendered: Vec<String> = args
                .iter()
                .filter_map(|e| gen_expr(e, ctx, &scope, scratch))
                .collect();
            if rendered.len() != args.len() {
                return None;
            }
            // Mirrors `hashOf(...)`'s uint256-cast convention so the
            // result slots cleanly into a Cambrian `U256` slot.
            Some(format!(
                "uint256(keccak256(abi.encodePacked({})))",
                rendered.join(", ")
            ))
        }
        // EVM precompile 0x02 — SHA-256. Mirrors `keccak256Packed`'s
        // `abi.encodePacked` flavour and uint256-cast convention so the
        // result fits a `U256` slot. Variadic.
        "sha256" if !args.is_empty() => {
            let rendered: Vec<String> = args
                .iter()
                .filter_map(|e| gen_expr(e, ctx, &scope, scratch))
                .collect();
            if rendered.len() != args.len() {
                return None;
            }
            Some(format!(
                "uint256(sha256(abi.encodePacked({})))",
                rendered.join(", ")
            ))
        }
        // EVM precompile 0x03 — RIPEMD-160. Returns `bytes20` natively;
        // widen through `uint160` then `uint256` so it slots into a
        // Cambrian `U256`. Variadic, `abi.encodePacked` convention.
        "ripemd160" if !args.is_empty() => {
            let rendered: Vec<String> = args
                .iter()
                .filter_map(|e| gen_expr(e, ctx, &scope, scratch))
                .collect();
            if rendered.len() != args.len() {
                return None;
            }
            Some(format!(
                "uint256(uint160(ripemd160(abi.encodePacked({}))))",
                rendered.join(", ")
            ))
        }
        // `evm::balance(addr)` — read the ETH balance of an arbitrary
        // address. Distinct from `sys::balance` which always reads
        // `address(this).balance`. Impure (state read).
        "balance" if args.len() == 1 => {
            let v = gen_expr(&args[0], ctx, &scope, scratch)?;
            Some(format!("{}.balance", v))
        }
        // `evm::blockhash(n)` — the canonical hash of a recent block.
        // Solidity `blockhash` returns `bytes32`; widen to `uint256` so
        // it composes with Cambrian's `U256` arithmetic. Impure (block
        // context).
        "blockhash" if args.len() == 1 => {
            let v = gen_expr(&args[0], ctx, &scope, scratch)?;
            Some(format!("uint256(blockhash({}))", v))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Expression lowering — adapter entry (`gen_expr`) + hoisted variant
// ---------------------------------------------------------------------------

pub(crate) fn gen_expr(
    expr: &Expr,
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> Option<String> {
    match expr {
        Expr::IntLiteral(v) => {
            if !v.fits_u128() {
                Some(format!(
                    "uint256(0x{})",
                    crate::ast::u256_hex_digits(v)
                ))
            } else {
                Some(v.to_display_decimal())
            }
        }
        Expr::BoolLiteral(v) => Some(if *v { "true" } else { "false" }.to_string()),
        Expr::StringLiteral(v) => Some(solidity_string_literal(v)),
        Expr::BytesLiteral(bytes) => {
            let hex: String = bytes.iter().map(|b| format!("{:02x}", b)).collect();
            Some(format!("hex\"{}\"", hex))
        }
        Expr::None => Some("0".to_string()),
        Expr::EmptyCollection => Some("0".to_string()),
        Expr::Ident(name) => Some(sol_sanitize_ident(name)),
        // T-EVM-EX-013: `^m` → `next_m` only when the current transform
        // batch actually declares that local (scalar transform at
        // `(route, phase)`). Otherwise → storage `m` (cross-phase,
        // map/vec-push, or outside transform emit). Mirrors Lean
        // `gen_temporal_ref`'s two-arm rule.
        Expr::TemporalRef(name) => Some(gen_temporal_ref_sol(name, scope)),
        Expr::BinOp(lhs, op, rhs) => match op {
            BinOp::WrappingAdd | BinOp::WrappingSub | BinOp::WrappingMul => {
                let l = gen_expr(lhs, ctx, &scope, scratch)?;
                let r = gen_expr(rhs, ctx, &scope, scratch)?;
                let operand_ty = |e: &Expr| match (scope.entity, e) {
                    (Some(entity), _) => Some(infer_let_type_entity(e, entity, ctx, scratch)),
                    (None, Expr::Ident(name)) => scratch.borrow().lookup_let_binding(name),
                    _ => None,
                };
                let signed = [lhs, rhs]
                    .iter()
                    .any(|e| operand_ty(e).is_some_and(|t| t.starts_with("int")));
                let suffix = if signed { "s" } else { "" };
                let name = match op {
                    BinOp::WrappingAdd => "_wadd",
                    BinOp::WrappingSub => "_wsub",
                    BinOp::WrappingMul => "_wmul",
                    _ => unreachable!(),
                };
                Some(format!("{name}{suffix}({l}, {r})"))
            }
            _ => {
                if let Some(entity) = scope.entity {
                    if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) {
                        let lty =
                            infer_expr_sol_ty(lhs, entity, ctx, scratch, InferSolMode::Flow);
                        let rty =
                            infer_expr_sol_ty(rhs, entity, ctx, scratch, InferSolMode::Flow);
                        if lty.ends_with("[] memory") || rty.ends_with("[] memory") {
                            let l = gen_expr(lhs, ctx, &scope, scratch)?;
                            let r = gen_expr(rhs, ctx, &scope, scratch)?;
                            let expr = if lty.ends_with("[] memory") {
                                let elem = lty.strip_suffix("[] memory").unwrap_or("uint256");
                                format!(
                                    "({}({}) + {})",
                                    array_memory_sum_fn(elem),
                                    l,
                                    r
                                )
                            } else {
                                let elem = rty.strip_suffix("[] memory").unwrap_or("uint256");
                                format!(
                                    "({} + {}({}))",
                                    l,
                                    array_memory_sum_fn(elem),
                                    r
                                )
                            };
                            return Some(expr);
                        }
                        let infer_lty = infer_let_type_entity(lhs, entity, ctx, scratch);
                        if infer_lty.ends_with(" memory") && !infer_lty.starts_with("Option_") {
                            let l = gen_expr(lhs, ctx, &scope, scratch)?;
                            let r = gen_expr(rhs, ctx, &scope, scratch)?;
                            let ls = record_scalar_binop_lhs(lhs, &l, entity, ctx, scratch);
                            return Some(format!("({} {} {})", ls, binop_str(op), r));
                        }
                    }
                    if let Some((l, r)) =
                        gen_binop_operands_solidity(lhs, rhs, op, &entity, ctx, &scope, scratch)
                    {
                        let bin_expr = Expr::BinOp(lhs.clone(), op.clone(), rhs.clone());
                        let rendered = format!("({} {} {})", l, binop_str(op), r);
                        // T-EVM-EX-006: comparisons / bool logic stay bool even when
                        // `expected_ty` is numeric (e.g. `if` conditions in U256 pure fns).
                        if matches!(
                            op,
                            BinOp::Eq
                                | BinOp::Ne
                                | BinOp::Lt
                                | BinOp::Le
                                | BinOp::Gt
                                | BinOp::Ge
                                | BinOp::And
                                | BinOp::Or
                        ) {
                            return Some(rendered);
                        }
                        if let Some(target) = scope_expected_sol_ty(&scope, entity, ctx) {
                            return Some(maybe_narrow_cast(
                                &target,
                                &rendered,
                                &bin_expr,
                                entity,
                                ctx,
                                scratch,
                            ));
                        }
                        return Some(rendered);
                    }
                }
                let l = gen_expr(lhs, ctx, &scope, scratch)?;
                let r = gen_expr(rhs, ctx, &scope, scratch)?;
                Some(format!("({} {} {})", l, binop_str(op), r))
            }
        },
        Expr::UnaryOp(op, value) => {
            let v = gen_expr(value, ctx, &scope, scratch)?;
            match op {
                UnaryOp::Not => Some(format!("(!{})", v)),
                UnaryOp::Neg => {
                    if !matches!(value.as_ref(), Expr::IntLiteral(_)) {
                        if let Some(entity) = scope.entity {
                            let inner_ty = infer_let_type_entity(value, entity, ctx, scratch);
                            if inner_ty.starts_with("uint") {
                                return Some(format!("({}(0) - {})", inner_ty, v));
                            }
                        }
                    }
                    Some(format!("(-{})", v))
                }
                UnaryOp::Deref => Some(v),
            }
        }
        Expr::FieldAccess(base, field) => {
            if let Expr::Ident(name) = base.as_ref() {
                if let Ok(idx) = field.parse::<usize>() {
                    if let Some(comp) = scratch.borrow().lookup_tuple_component(name, idx) {
                        return Some(comp);
                    }
                    if let Some((en, var)) = scratch.borrow().lookup_payload_enum_binding(name) {
                        if let Some(decl) = ctx.lookup_enum(&en) {
                            if let Some(v) = decl.variants.iter().find(|v| v.name == var) {
                                let fname = payload_enum_field_name(v, idx);
                                return Some(format!("{}.{}", sol_sanitize_ident(name), fname));
                            }
                        }
                    }
                }
            }
            let b = gen_expr(base, ctx, &scope, scratch)?;
            Some(format!("{}.{}", b, field))
        }
        Expr::Tuple(items) => {
            let rendered: Vec<String> = items
                .iter()
                .filter_map(|e| gen_expr(e, ctx, &scope, scratch))
                .collect();
            if rendered.len() != items.len() {
                return None;
            }
            Some(format!("({})", rendered.join(", ")))
        }
        Expr::RecordConstruct(name, fields) => {
            let entity = scope.entity;
            let record = entity
                .as_ref()
                .and_then(|e| e.records.iter().find(|r| r.name == *name))
                .or_else(|| ctx.lookup_record(name));
            let field_types: std::collections::HashMap<String, String> = record
                .map(|rec| {
                    rec.fields
                        .iter()
                        .map(|f| {
                            let ty = sol_type_entity(entity.as_ref().unwrap(), &f.ty, true, ctx);
                            (f.name.clone(), ty)
                        })
                        .collect()
                })
                .unwrap_or_default();
            let entity_for_cast = entity.as_ref();
            let rendered: Vec<String> = fields
                .iter()
                .map(|(k, v)| {
                    gen_expr(v, ctx, &scope, scratch).map(|rv| {
                        let cast_rv = match (field_types.get(k), entity_for_cast) {
                            (Some(ty), Some(e)) => maybe_narrow_cast(ty, &rv, v, e, ctx, scratch),
                            (Some(ty), None) => wrap_narrow_cast(ty, &rv),
                            _ => rv,
                        };
                        format!("{}: {}", k, cast_rv)
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(format!("{}({{{}}})", name, rendered.join(", ")))
        }
        Expr::Index(base, index) => {
            let resolved_base = match base.as_ref() {
                Expr::Ident(n) => scratch
                    .borrow()
                    .lookup_hashmap_alias(n)
                    .unwrap_or_else(|| base.as_ref().clone()),
                _ => base.as_ref().clone(),
            };
            let b = gen_expr(&resolved_base, ctx, &scope, scratch)?;
            let i = gen_expr(index, ctx, &scope, scratch)?;
            Some(format!("{}[{}]", b, i))
        }
        Expr::FnCall(name, args) => {
            // Deterministic-mode synonym: `addressOf(Entity.state(id_args...))`
            // lowers to the same CREATE2 expression as `Entity.address(id_args...)`.
            // Falls back to the generic FnCall handler in non-deterministic mode so
            // Solidity rejects `addressOf(...)` with an unknown-identifier error
            // rather than silently emitting wrong code.
            if name == "addressOf" && ctx.is_deterministic_mode() && !args.is_empty() {
                let state_call = match args.len() {
                    1 | 2 => Some(&args[0]),
                    _ => None,
                };
                if let Some(Expr::MethodCall(base, method, state_args)) = state_call {
                    if method == "state" {
                        if let Expr::Ident(entity_name) = base.as_ref() {
                            if entity_name
                                .chars()
                                .next()
                                .map_or(false, |c| c.is_uppercase())
                            {
                                return gen_expr_address_deterministic(
                                    entity_name,
                                    state_args,
                                    ctx,
                                    scratch,
                                );
                            }
                        }
                    }
                }
            }
            let mut rendered: Vec<String> = args
                .iter()
                .filter_map(|e| gen_expr(e, ctx, &scope, scratch))
                .collect();
            if rendered.len() != args.len() {
                return None;
            }
            narrow_pure_fn_args(name, args, &mut rendered, &scope, ctx, scratch);
            // `hashOf(args...)` is the cross-target Cambrian builtin for
            // structured hashing. On EVM it lowers to keccak256(abi.encode(...))
            // which mirrors OZ's `proposalId` / `operationId` derivation.
            // The result is uint256 so it composes with bytes32 / U256 storage.
            if name == "hashOf" {
                return Some(format!(
                    "uint256(keccak256(abi.encode({})))",
                    rendered.join(", ")
                ));
            }
            if name == "address" && args.len() == 1 {
                if let Expr::IntLiteral(v) = &args[0] {
                    return Some(hex_literal_to_sol_for_ty(
                        &crate::ast::u256_hex_digits(v),
                        Some("address"),
                    ));
                }
                if let Some(inner) = gen_expr(&args[0], ctx, &scope, scratch) {
                    return Some(format!("address(uint160(uint256({})))", inner));
                }
            }
            format_pure_fn_call(
                name,
                None,
                args,
                rendered,
                scope.entity.as_deref(),
                ctx,
                scratch,
            )
        }
        Expr::AddressOf {
            entity_name, args, ..
        } => {
            // Test-only `address_of Entity(id_args...)` keyword form; on EVM in
            // deterministic mode it lowers to the same CREATE2 expression as
            // `Entity.address(id_args...)`.
            if ctx.is_deterministic_mode() {
                gen_expr_address_deterministic(entity_name, args, ctx, scratch)
            } else {
                None
            }
        }
        Expr::MethodCall(base, method, args) => {
            let base = if let Expr::Ident(n) = base.as_ref() {
                scratch
                    .borrow()
                    .lookup_hashmap_alias(n)
                    .unwrap_or_else(|| base.as_ref().clone())
            } else {
                base.as_ref().clone()
            };
            let hashmap_member = match (&base, scope.entity) {
                (Expr::Ident(n), Some(entity)) => entity.members.iter().any(|m| {
                    &m.name == n && matches!(&m.ty, Type::Generic(g, _) if g == "HashMap")
                }),
                _ => false,
            };
            match method.as_str() {
            "len" | "length" if args.is_empty() && hashmap_member => {
                let Expr::Ident(name) = &base else { unreachable!() };
                Some(format!("{}_keys.length", name))
            }
            "len" | "length" if args.is_empty() => {
                let b = gen_expr(&base, ctx, &scope, scratch)?;
                if let Some(entity) = scope.entity {
                    Some(solidity_length_expr(&base, &b, entity, ctx, scratch))
                } else {
                    Some(format!("{}.length", b))
                }
            }
            "cam_get" if args.len() == 1 => {
                let b = gen_expr(&base, ctx, &scope, scratch)?;
                let k = gen_expr(&args[0], ctx, &scope, scratch)?;
                Some(format!("{}[{}]", b, k))
            }
            // Phase EVM-15 H3 (Cluster B): `m.contains(k)` is the
            // Cambrian alias for `m.exists(k)` exposed as a HashMap
            // method (mirrors `cam_contains` on Acki Nacki). Reuses
            // the existing `_exists` sidecar — call sites are
            // detected by the same `member_uses_exists` predicate.
            "contains" if args.len() == 1 => {
                if let Expr::Ident(name) = &base {
                    let k = gen_expr(&args[0], ctx, &scope, scratch)?;
                    return Some(format!("{}_exists[{}]", name, k));
                }
                if let Expr::Index(inner_base, k1) = &base {
                    if let Expr::Ident(name) = inner_base.as_ref() {
                        let k1s = gen_expr(k1, ctx, &scope, scratch)?;
                        let k2s = gen_expr(&args[0], ctx, &scope, scratch)?;
                        return Some(format!("{}_inner_exists[{}][{}]", name, k1s, k2s));
                    }
                }
                None
            }
            // Phase EVM-15 H3 (Cluster B): `m.is_empty()` lowers to
            // `m_keys.length == 0`. The `_keys` companion is emitted
            // by `member_is_iterated` whenever `.keys()` /
            // `.values()` / `.is_empty()` appear, so the read here
            // is always backed by storage. (`is_empty` itself is one
            // of the triggering call sites.)
            "is_empty" if args.is_empty() && hashmap_member => {
                let Expr::Ident(name) = &base else { unreachable!() };
                Some(format!("({}_keys.length == 0)", name))
            }
            "is_empty" if args.is_empty() => {
                let base_ty = match (scope.entity, &base) {
                    (Some(entity), _) => Some(infer_let_type_entity(&base, entity, ctx, scratch)),
                    (None, Expr::Ident(name)) => scratch.borrow().lookup_let_binding(name),
                    _ => None,
                }
                .unwrap_or_default();
                let base_ty = strip_memory_ty(&base_ty);
                if !(base_ty.ends_with("[]") || base_ty == "string" || base_ty == "bytes") {
                    return match &base {
                        Expr::Ident(name) => Some(format!("({}_keys.length == 0)", name)),
                        _ => None,
                    };
                }
                let b = gen_expr(&base, ctx, &scope, scratch)?;
                let len = match scope.entity {
                    Some(entity) => solidity_length_expr(&base, &b, entity, ctx, scratch),
                    None => format!("{}.length", b),
                };
                Some(format!("({} == 0)", len))
            }
            "exists" if args.len() == 1 => {
                if let Expr::Ident(name) = &base {
                    let k = gen_expr(&args[0], ctx, &scope, scratch)?;
                    return Some(format!("{}_exists[{}]", name, k));
                }
                // EVM-3 Batch G2: nested-mapping exists. Recognises
                // `m[k1].exists(k2)` (which, after Batch G2's
                // let-substitution, is also the lowered form of
                // `let inner = m[k1]; inner.exists(k2)`) and binds it
                // to a 2-level sidecar `mapping(K1 => mapping(K2 =>
                // bool)) m_inner_exists`. The sidecar emission is
                // gated on the same `member_uses_exists` predicate
                // that triggers the top-level `_exists` companion.
                if let Expr::Index(inner_base, k1) = &base {
                    if let Expr::Ident(name) = inner_base.as_ref() {
                        let k1s = gen_expr(k1, ctx, &scope, scratch)?;
                        let k2s = gen_expr(&args[0], ctx, &scope, scratch)?;
                        return Some(format!("{}_inner_exists[{}][{}]", name, k1s, k2s));
                    }
                }
                None
            }
            // EVM-13: `m.keys()` lowers to the parallel storage array
            // emitted alongside the HashMap. Solidity copies storage to
            // memory automatically when the result is bound to a
            // memory-typed local, so callers in expression position
            // ("Vec<K> ks = m.keys();") get a memory copy and callers
            // in iteration position get a storage walk through the
            // existing for/fold loop machinery.
            //
            // We deliberately don't gate this on "is the parallel
            // storage actually emitted?" — the entity-contract emission
            // pass uses the same `member_is_iterated` predicate to
            // decide that, so by the time we reach this arm the
            // storage is guaranteed to be present. If a future caller
            // ever bypasses the predicate, the resulting Solidity will
            // fail to compile (good — it surfaces the inconsistency).
            "keys" if args.is_empty() => {
                if let Expr::Ident(name) = &base {
                    Some(format!("{}_keys", name))
                } else {
                    None
                }
            }
            // `String.split(delim)` — no Solidity member; call the emitted
            // `_cam_string_split` helper (T-EVM-LCC-001).
            "split" if args.len() == 1 => {
                let b = gen_expr(&base, ctx, &scope, scratch)?;
                let d = gen_expr(&args[0], ctx, &scope, scratch)?;
                Some(format!("_cam_string_split({}, {})", b, d))
            }
            // EVM-13: `<iter>.collect()` is a no-op on EVM — Cambrian's
            // iterator chains materialise into Vecs by default, and
            // the only currently lowered chain prefix is `m.keys()`,
            // which already produces a Solidity array. Just defer to
            // the inner expression.
            "collect" if args.is_empty() => gen_expr(&base, ctx, &scope, scratch),
            "insert" | "update" | "remove" => None,
            // G-U3: `m_xyz.push(arg)` is statement-shaped; the
            // transform-body matcher (`match_member_push`) handles
            // it before this arm fires. Reaching this arm means
            // `.push(...)` was used outside transform-statement
            // context (e.g. inside a route action expression),
            // which has no scalar value to bind to. Returning None
            // surfaces this as a clear codegen error rather than
            // emitting silently-wrong Solidity.
            "push" => None,
            "unwrap" if args.is_empty() => gen_expr(&base, ctx, &scope, scratch),
            "address" => {
                if args.is_empty() {
                    if let Some(entity) = scope.entity {
                        let base_ty = infer_let_type_entity(&base, entity, ctx, scratch);
                        if base_ty == "address" {
                            return gen_expr(&base, ctx, &scope, scratch);
                        }
                    }
                }
                if let Expr::Ident(entity_name) = &base {
                    if entity_name
                        .chars()
                        .next()
                        .map_or(false, |c| c.is_uppercase())
                    {
                        if ctx.is_deterministic_mode() {
                            return gen_expr_address_deterministic(entity_name, args, ctx, scratch);
                        }
                        if !args.is_empty() {
                            let rendered: Vec<String> = args
                                .iter()
                                .filter_map(|e| gen_expr(e, ctx, &scope, scratch))
                                .collect();
                            if rendered.len() == args.len() {
                                if rendered.len() == 1 {
                                    let a = &rendered[0];
                                    if a.starts_with("address(") {
                                        return Some(a.clone());
                                    }
                                    return Some(format!("address(uint160(uint256({})))", a));
                                }
                            }
                        }
                    }
                }
                if !args.is_empty() {
                    gen_expr(&args[0], ctx, &scope, scratch)
                } else {
                    None
                }
            }
            _ => {
                if let Expr::Ident(lib_name) = &base {
                    if library_qualifies(lib_name, method, ctx) {
                        let mut rendered: Vec<String> = args
                            .iter()
                            .filter_map(|e| gen_expr(e, ctx, &scope, scratch))
                            .collect();
                        if rendered.len() != args.len() {
                            return None;
                        }
                        narrow_pure_fn_args(method, args, &mut rendered, &scope, ctx, scratch);
                        if let Some(call) = format_pure_fn_call(
                            method,
                            Some(lib_name.as_str()),
                            args,
                            rendered,
                            scope.entity.as_deref(),
                            ctx,
                            scratch,
                        ) {
                            return Some(call);
                        }
                    }
                }
                let b = gen_expr(&base, ctx, &scope, scratch)?;
                let rendered: Vec<String> = args
                    .iter()
                    .filter_map(|e| gen_expr(e, ctx, &scope, scratch))
                    .collect();
                if rendered.len() != args.len() {
                    return None;
                }
                Some(format!("{}.{}({})", b, method, rendered.join(", ")))
            }
            }
        }
        Expr::MacroRef(name, args) => {
            let rendered: Vec<String> = args
                .iter()
                .filter_map(|e| {
                    let s = gen_expr(e, ctx, &scope, scratch)?;
                    if let Some(entity) = scope.entity {
                        let arg_ty = infer_let_type_entity(e, entity, ctx, scratch);
                        let rec_name = arg_ty.strip_suffix(" memory").unwrap_or(&arg_ty);
                        if let Some(rec) = entity
                            .records
                            .iter()
                            .find(|r| r.name == rec_name)
                            .or_else(|| ctx.lookup_record(rec_name))
                        {
                            if let Some(field) = rec
                                .fields
                                .iter()
                                .find(|f| matches!(&f.ty, Type::Simple(n) if n == "u64" || n == "U64"))
                            {
                                return Some(format!("{}.{}", s, field.name));
                            }
                        }
                    }
                    Some(s)
                })
                .collect();
            if rendered.len() != args.len() {
                return None;
            }
            Some(format!("macro_{}({})", name, rendered.join(", ")))
        }
        // Invariant trace-state accessors lower to handler storage
        // counters (`_traceLen`, `_traceCount_<route>`) and per-route
        // "last action" flags (`_traceLast_<route>`). The invariant
        // handler declares and maintains these.
        Expr::TraceField(field) => match field.as_str() {
            "length" => Some("_traceLen".to_string()),
            _ => None,
        },
        Expr::TraceCall { name, route } => match name.as_str() {
            "count" => Some(format!("_traceCount_{}", route)),
            "lastWas" => Some(format!("_traceLast_{}", route)),
            _ => None,
        },
        Expr::MsgField(field) => sol_msg_field(field).map(str::to_string),
        Expr::SysField(field) => sol_sys_field(field).map(str::to_string),
        Expr::Cast(value, ty) => {
            let mut v = gen_expr(value, ctx, &scope, scratch)?;
            let t = sol_type(ty, false);
            let is_literal = match value.as_ref() {
                Expr::IntLiteral(_) => true,
                Expr::UnaryOp(crate::ast::UnaryOp::Neg, e) => matches!(e.as_ref(), Expr::IntLiteral(_)),
                _ => false,
            };
            let src_ty = match (scope.entity, value.as_ref()) {
                _ if is_literal => None,
                (Some(entity), _) => Some(infer_let_type_entity(value, entity, ctx, scratch)),
                (None, Expr::Ident(name)) => scratch.borrow().lookup_let_binding(name),
                _ => None,
            };
            if let Some(src_ty) = src_ty.as_deref() {
                let src_ty = src_ty.trim_end_matches(" memory");
                if src_ty == "bool"
                    && (t.starts_with("uint") || t.starts_with("int"))
                {
                    return Some(format!("({} ? {}(1) : {}(0))", v, t, t));
                }
                // Solidity rejects a sign change combined with a width change,
                // and a bare sign change would wrap instead of panicking.
                let signed = |s: &str| s.starts_with("int");
                let unsigned = |s: &str| s.starts_with("uint");
                if signed(src_ty) && unsigned(&t) {
                    v = format!("_camI2U(int256({}))", v);
                } else if unsigned(src_ty) && signed(&t) {
                    v = format!("_camU2I(uint256({}))", v);
                }
                if t == "uint256" && v.starts_with("_camI2U(")
                    || t == "int256" && v.starts_with("_camU2I(")
                {
                    return Some(v);
                }
            }
            // Phase EVM-P0-B: emit explicit casts for narrow integer
            // targets too. Pre-P0-B `as u8`–`as u128` was a no-op
            // because narrow types widened to uint256, so the Cambrian
            // surface cast didn't need a Solidity counterpart. Now
            // narrow types survive, so `x as u32` must lower to
            // `uint32(x)` to actually downcast (and silence solc's
            // implicit-conversion errors at the use site).
            match t.as_str() {
                "uint256" | "int256" | "bool" | "address" | "bytes32" | "bytes4" => {
                    Some(format!("{}({})", t, v))
                }
                "uint128" | "uint64" | "uint32" | "uint16" | "uint8" | "int128" | "int64"
                | "int32" | "int16" | "int8" => Some(wrap_checked_narrow_cast(&t, &v)),
                _ => Some(v),
            }
        }
        Expr::EnumVariant(enum_name, variant) => {
            // Phase EVM-4 J2: payload-bearing enums lower to a Solidity
            // `struct` (`enum <N>_Tag` + `struct <N>` from J1). A unit
            // variant within a payload enum still needs the struct
            // constructor; only fully-unit enums keep the `Enum.Variant`
            // dotted form.
            if let Some(decl) = ctx.lookup_enum(enum_name) {
                if is_payload_enum(&decl) {
                    // Find the active variant — for a unit-arity
                    // EnumVariant the value-for-active callback is
                    // never invoked (no payload positions), so a panic
                    // never fires.
                    let _ = decl.variants.iter().find(|v| &v.name == variant)?;
                    let entity = scope.entity?;
                    return Some(payload_enum_construct(&entity, &decl, variant, ctx, |_| {
                        None
                    }));
                }
            }
            Some(format!("{}.{}", enum_name, variant))
        }
        Expr::NamespacedCall {
            namespace,
            name,
            args,
            ..
        } if crate::codegen::stdlib::is_std_namespace(namespace) => {
            let arg_strs: Vec<String> = args
                .iter()
                .filter_map(|e| gen_expr(e, ctx, &scope, scratch))
                .collect();
            if arg_strs.len() != args.len() {
                return None;
            }
            Some(crate::codegen::stdlib::gen_std_call_evm(
                namespace, name, &arg_strs,
            ))
        }
        Expr::NamespacedCall {
            namespace,
            name,
            args,
            ..
        } if namespace == "evm" => {
            // `evm::foo(args)` — reserved namespace for state-changing /
            // EVM-specific intrinsics (mirrors the role of `gosh::*` on
            // Acki Nacki). The turbofish form (`evm::name::<T>(...)`)
            // parses to `NamespacedCall`; the bare form parses to
            // `EnumVariantWithData("evm", ...)` (see arm below).
            gen_evm_ns(name.as_str(), args, ctx, scope, scratch)
        }
        // `evm::name(args)` without turbofish parses as
        // `EnumVariantWithData("evm", name, args)` per cambrian.lalrpop.
        // Route those through the same EVM-namespace dispatch table.
        Expr::EnumVariantWithData(enum_name, variant, args) if enum_name == "evm" => {
            gen_evm_ns(variant.as_str(), args, ctx, scope, scratch)
        }
        // SD-01d: `Lib::fn(args)` disambiguation — parser emits
        // `EnumVariantWithData` when the qualifier is not an enum.
        Expr::EnumVariantWithData(enum_name, variant, args)
            if library_qualifies(enum_name, variant, ctx) =>
        {
            let mut rendered: Vec<String> = args
                .iter()
                .filter_map(|e| gen_expr(e, ctx, &scope, scratch))
                .collect();
            if rendered.len() != args.len() {
                return None;
            }
            narrow_pure_fn_args(variant, args, &mut rendered, &scope, ctx, scratch);
            format_pure_fn_call(
                variant,
                Some(enum_name.as_str()),
                args,
                rendered,
                scope.entity.as_deref(),
                ctx,
                scratch,
            )
        }
        // Phase EVM-4 J2: tagged-union constructor in expression position.
        // Only succeeds when every payload arg lowers inline (gen_expr →
        // Some); otherwise the hoisted path takes over.
        Expr::EnumVariantWithData(enum_name, variant, args) => {
            let decl = ctx.lookup_enum(enum_name)?;
            if !is_payload_enum(&decl) {
                return None;
            }
            let entity = scope.entity?;
            let arg_strs: Vec<String> = args
                .iter()
                .filter_map(|e| gen_expr(e, ctx, &scope, scratch))
                .collect();
            if arg_strs.len() != args.len() {
                return None;
            }
            Some(payload_enum_construct(&entity, &decl, variant, ctx, |i| {
                Some(arg_strs.get(i).cloned().unwrap_or_else(|| "0".to_string()))
            }))
        }
        Expr::Some(inner) => {
            let v = gen_expr(inner, ctx, &scope, scratch)?;
            let expected_inner = match scope.expected_ty {
                Some(Type::Generic(g, ps)) if g == "Option" && ps.len() == 1 => Some(&ps[0]),
                _ => None,
            };
            let inner_ty = match (expected_inner, scope.entity) {
                (Some(t), Some(entity)) => sol_type_entity(entity, t, true, ctx),
                (None, Some(entity)) => infer_let_type_entity(inner, entity, ctx, scratch),
                _ => "uint256".to_string(),
            };
            Some(option_some_from_sol_inner(&inner_ty, &v))
        }
        Expr::If(cond, then, else_opt) => {
            let c = gen_expr(cond, ctx, &scope, scratch)?;
            let t = gen_expr(then, ctx, &scope, scratch)?;
            let e = match else_opt {
                Some(e) => gen_expr(e, ctx, &scope, scratch)?,
                None => "0".to_string(),
            };
            Some(format!("({} ? {} : {})", c, t, e))
        }
        Expr::Match(subject, arms) => {
            if crate::codegen::std_str::parse_str_meta_from_expr(subject).is_some() {
                return None;
            }
            // Only lower simple enum/literal matches inline
            if arms.iter().all(|arm| {
                matches!(
                    arm.body,
                    Expr::IntLiteral(_)
                        | Expr::BoolLiteral(_)
                        | Expr::Ident(_)
                        | Expr::EnumVariant(_, _)
                )
            }) {
                // Simple pattern: just emit nested ternaries
                gen_match_simple(subject, arms, ctx, scope, scratch)
            } else {
                None
            }
        }
        _ => None,
    }
}

pub(crate) fn gen_match_simple(
    subject: &Expr,
    arms: &[MatchArm],
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> Option<String> {
    let s = gen_expr(subject, ctx, &scope, scratch)?;
    let option_struct = scope.entity.and_then(|entity| {
        let ty = infer_let_type_entity(subject, entity, ctx, scratch);
        let name = strip_memory_ty(&ty);
        name.starts_with("Option_").then(|| name.to_string())
    });
    // Build nested ternaries from right to left. With exhaustive patterns
    // the last arm is the unconditional else; the `0` fallback only
    // type-checks for numeric arm bodies.
    let exhaustive = match_patterns_exhaustive(arms, ctx);
    let mut result = "0".to_string();
    for (idx, arm) in arms.iter().enumerate().rev() {
        if exhaustive && idx + 1 == arms.len() {
            let body_expr = match &arm.pattern {
                MatchPattern::Some(Pattern::Ident(binder)) => subst_ident_in_expr(
                    &arm.body,
                    binder,
                    &Expr::FieldAccess(Box::new(subject.clone()), "some_0".into()),
                ),
                MatchPattern::EnumVariantWithData(en, vn, payload_pats) => {
                    let decl = ctx.lookup_enum(en)?;
                    if !is_payload_enum(&decl) {
                        return None;
                    }
                    subst_payload_bindings(&arm.body, subject, &decl, vn, payload_pats)
                }
                _ => arm.body.clone(),
            };
            result = gen_expr(&body_expr, ctx, &scope, scratch)?;
            continue;
        }
        match &arm.pattern {
            MatchPattern::Wildcard | MatchPattern::Ident(_) => {
                let body = gen_expr(&arm.body, ctx, &scope, scratch)?;
                result = body;
            }
            MatchPattern::EnumVariant(en, vn) => {
                let body = gen_expr(&arm.body, ctx, &scope, scratch)?;
                let cond = payload_or_unit_enum_cond(&s, en, vn, ctx);
                result = format!("({} ? {} : {})", cond, body, result);
            }
            MatchPattern::EnumVariantWithData(en, vn, payload_pats) => {
                // Phase EVM-4 J3: only lower if the enum is a payload
                // enum we can resolve. The body needs binding
                // substitution before passing through gen_expr.
                let decl = ctx.lookup_enum(en)?;
                if !is_payload_enum(&decl) {
                    return None;
                }
                let substituted =
                    subst_payload_bindings(&arm.body, subject, &decl, vn, payload_pats);
                let body = gen_expr(&substituted, ctx, &scope, scratch)?;
                let cond = payload_or_unit_enum_cond(&s, en, vn, ctx);
                result = format!("({} ? {} : {})", cond, body, result);
            }
            MatchPattern::IntLiteral(n) => {
                let body = gen_expr(&arm.body, ctx, &scope, scratch)?;
                result = format!("({} == {} ? {} : {})", s, n, body, result);
            }
            MatchPattern::BoolLiteral(b) => {
                let body = gen_expr(&arm.body, ctx, &scope, scratch)?;
                let bv = if *b { "true" } else { "false" };
                result = format!("({} == {} ? {} : {})", s, bv, body, result);
            }
            MatchPattern::None => {
                let body = gen_expr(&arm.body, ctx, &scope, scratch)?;
                let cond = match &option_struct {
                    Some(name) => format!("{s}.tag == {name}_Tag.None"),
                    None => format!("{s} == 0"),
                };
                result = format!("({} ? {} : {})", cond, body, result);
            }
            MatchPattern::Some(inner) => {
                let body_expr = if let Pattern::Ident(binder) = inner {
                    subst_ident_in_expr(
                        &arm.body,
                        binder,
                        &Expr::FieldAccess(Box::new(subject.clone()), "some_0".into()),
                    )
                } else {
                    arm.body.clone()
                };
                let body = gen_expr(&body_expr, ctx, &scope, scratch)?;
                let cond = match &option_struct {
                    Some(name) => format!("{s}.tag == {name}_Tag.Some"),
                    None => format!("{s} != 0"),
                };
                result = format!("({} ? {} : {})", cond, body, result);
            }
        }
    }
    Some(result)
}

/// Quote a string value as a Solidity literal. Non-ASCII text needs the
/// `unicode"..."` form; plain literals accept printable ASCII only.
pub(crate) fn solidity_string_literal(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 2);
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    if v.is_ascii() {
        format!("\"{out}\"")
    } else {
        format!("unicode\"{out}\"")
    }
}

fn match_patterns_exhaustive(arms: &[MatchArm], ctx: &EvmCtx) -> bool {
    let has = |f: &dyn Fn(&MatchPattern) -> bool| arms.iter().any(|a| f(&a.pattern));
    if has(&|p| matches!(p, MatchPattern::Wildcard | MatchPattern::Ident(_))) {
        return true;
    }
    if has(&|p| matches!(p, MatchPattern::None))
        && has(&|p| matches!(p, MatchPattern::Some(Pattern::Ident(_) | Pattern::Wildcard)))
    {
        return true;
    }
    if has(&|p| matches!(p, MatchPattern::BoolLiteral(true)))
        && has(&|p| matches!(p, MatchPattern::BoolLiteral(false)))
    {
        return true;
    }
    let mut enum_name: Option<&str> = None;
    let mut variants = std::collections::HashSet::new();
    for arm in arms {
        match &arm.pattern {
            MatchPattern::EnumVariant(en, vn) | MatchPattern::EnumVariantWithData(en, vn, _) => {
                if enum_name.map_or(false, |n| n != en) {
                    return false;
                }
                enum_name = Some(en);
                variants.insert(vn.as_str());
            }
            _ => return false,
        }
    }
    enum_name
        .and_then(|en| ctx.lookup_enum(en))
        .map_or(false, |decl| decl.variants.iter().all(|v| variants.contains(v.name.as_str())))
}

fn gen_match_on_std_parse(
    subject: &Expr,
    arms: &[MatchArm],
    meta: crate::codegen::std_str::ParseStrMeta,
    entity: &Entity,
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> (Vec<String>, String) {
    let args: Vec<String> = match subject {
        Expr::NamespacedCall { args, .. } => args
            .iter()
            .filter_map(|e| gen_expr(e, ctx, &scope, scratch))
            .collect(),
        _ => vec![],
    };
    let try_call = crate::codegen::stdlib::gen_std_try_parse_evm(&meta, &args);
    let ok_tmp = scratch.borrow_mut().next_tmp();
    let val_tmp = scratch.borrow_mut().next_tmp();
    let val_sol_ty = if meta.signed { "int256" } else { "uint256" };
    let val_binding_ty = if meta.signed {
        if meta.bits >= 128 {
            "int256".to_string()
        } else {
            format!("int{}", meta.bits)
        }
    } else if meta.bits >= 256 {
        "uint256".to_string()
    } else {
        format!("uint{}", meta.bits)
    };
    let parse_meta_sol_ty = crate::codegen::std_str::parse_str_meta_sol_type(&meta);
    let match_ty = if arms.iter().all(|a| matches!(a.body, Expr::BoolLiteral(_))) {
        "bool".to_string()
    } else {
        parse_meta_sol_ty
    };
    let tmp = scratch.borrow_mut().next_tmp();
    let mut stmts = vec![format!(
        "(bool {}, {} {}) = {};",
        ok_tmp, val_sol_ty, val_tmp, try_call
    )];
    stmts.push(format!(
        "{} {} = {};",
        match_ty,
        tmp,
        default_sol_literal(&match_ty)
    ));
    let mut first = true;
    for arm in arms {
        let (cond, bind_name, effective_body) = match &arm.pattern {
            MatchPattern::None => (Some(format!("!{}", ok_tmp)), None, arm.body.clone()),
            MatchPattern::Some(inner) => {
                let bind = match inner {
                    Pattern::Ident(n) => Some(n.clone()),
                    _ => None,
                };
                (Some(ok_tmp.clone()), bind, arm.body.clone())
            }
            MatchPattern::Wildcard | MatchPattern::Ident(_) => (None, None, arm.body.clone()),
            _ => (None, None, arm.body.clone()),
        };
        let (body_stmts, body_str) = gen_expr_hoisted(&effective_body, entity, ctx, scope, scratch);
        if let Some(c) = cond {
            stmts.push(if first {
                format!("if ({}) {{", c)
            } else {
                format!("}} else if ({}) {{", c)
            });
            first = false;
            if let Some(bind) = bind_name {
                if val_binding_ty == val_sol_ty {
                    stmts.push(format!("    {} {} = {};", val_binding_ty, bind, val_tmp));
                } else {
                    stmts.push(format!(
                        "    {} {} = {}({});",
                        val_binding_ty, bind, val_binding_ty, val_tmp
                    ));
                }
            }
        } else {
            stmts.push(if first {
                "{".to_string()
            } else {
                "} else {".to_string()
            });
            first = false;
        }
        for s in body_stmts {
            stmts.push(format!("    {}", s));
        }
        let body_val = maybe_narrow_cast(
            &match_ty,
            &body_str,
            &effective_body,
            entity,
            ctx,
            scratch,
        );
        stmts.push(format!("    {} = {};", tmp, body_val));
    }
    if !first {
        stmts.push("}".to_string());
    }
    (stmts, tmp)
}

/// Solidity has no first-class tuple values. When Cambrian binds
/// `let tagged = (a, b)` we destructure into `tagged_0`, `tagged_1` and
/// rewrite `tagged.0` field access to those locals.
pub(crate) fn emit_tuple_ident_destructure(
    name: &str,
    elem_tys: &[String],
    val_str: &str,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let comp_names: Vec<String> = elem_tys
        .iter()
        .enumerate()
        .map(|(i, _)| format!("{}_{}", sol_sanitize_ident(name), i))
        .collect();
    let binders: Vec<String> = elem_tys
        .iter()
        .zip(comp_names.iter())
        .map(|(ty, c)| format!("{} {}", ty, c))
        .collect();
    scratch
        .borrow_mut()
        .push_tuple_binding(name, comp_names.clone());
    for (comp, ty) in comp_names.iter().zip(elem_tys.iter()) {
        scratch.borrow_mut().push_let_binding(comp, ty);
    }
    format!("({}) = {};", binders.join(", "), val_str)
}

// ---------------------------------------------------------------------------
// Expression lowering — hoisted (can emit pre-statements)
// ---------------------------------------------------------------------------

/// Returns (pre_statements, value_expr).
/// pre_statements are complete Solidity statements (no trailing newline).
pub(crate) fn gen_expr_hoisted(
    expr: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> (Vec<String>, String) {
    // Bare `m.values()` has no Solidity member — materialise as an
    // implicit `.collect()` over the HashMap values walk.
    if let Expr::MethodCall(base, method, args) = expr {
        if method == "values" && args.is_empty() {
            if let Expr::Ident(name) = base.as_ref() {
                if entity
                    .members
                    .iter()
                    .any(|m| m.name == *name && is_mapping_type(&m.ty))
                {
                    let wrapped = Expr::MethodCall(
                        Box::new(expr.clone()),
                        "collect".to_string(),
                        vec![],
                    );
                    if let Some((src, stages, term)) = parse_iter_chain(&wrapped) {
                        return lower_iter_chain(src, &stages, &term, entity, ctx, scope, scratch);
                    }
                }
            }
        }
    }
    // EVM-12 Batch B: chain fusion intercepts `<src>.<...stages>.collect()`
    // and `<src>.<...stages>.fold(...)` BEFORE `gen_expr`'s catch-all
    // method-call rendering would emit `xs.take(n).collect()` as a
    // literal Solidity method-call (which Solidity has no built-in for).
    // The detector requires at least one peelable stage, so simple shapes
    // like a bare `m.keys()` or a free-standing `.collect()` continue to
    // route through `gen_expr` unchanged.
    if let Some((src, stages, term)) = parse_iter_chain(expr) {
        return lower_iter_chain(src, &stages, &term, entity, ctx, scope, scratch);
    }
    if let Some(simple) = gen_expr(expr, ctx, &scope, scratch) {
        return (vec![], simple);
    }
    match expr {
        Expr::Let(Pattern::Ident(name), val, body) => {
            // EVM-3 Batch G2: Solidity disallows `mapping(...)` as a
            // local variable. When the bound value is HashMap-typed
            // (bare member, nested-map index, or the idiomatic
            // `if m.exists(k) { m[k] } else { {} }` boilerplate),
            // skip emitting a Solidity local and inline the value
            // expression at every use of `name` in `body`. Idiomatic
            // patterns like `let inner = m[k]; inner.exists(x)` then
            // collapse to `m[k].exists(x)`, which the
            // `MethodCall::exists` arm handles via the 2-level
            // sidecar.
            let simplified_val = simplify_hashmap_let_alias_init(val)
                .unwrap_or_else(|| (**val).clone());
            if is_hashmap_valued_expr(&simplified_val, entity) {
                let substituted = subst_hashmap_let_in_expr(body, name, &simplified_val);
                return gen_expr_hoisted(&substituted, entity, ctx, scope, scratch);
            }
            if let Expr::Tuple(elems) = val.as_ref() {
                if tuple_contains_hashmap_slot(elems, entity) {
                    return gen_expr_hoisted(body, entity, ctx, scope, scratch);
                }
            }
            let (mut stmts, val_str) = gen_expr_hoisted(val, entity, ctx, scope, scratch);
            if let Some(elem_tys) = infer_tuple_elem_types(val, entity, ctx, scratch) {
                if elem_tys.len() > 1 {
                    let comp_names: Vec<String> = elem_tys
                        .iter()
                        .enumerate()
                        .map(|(i, _)| format!("{}_{}", sol_sanitize_ident(name), i))
                        .collect();
                    stmts.push(emit_tuple_ident_destructure(
                        name,
                        &elem_tys,
                        &val_str,
                        scratch,
                    ));
                    let (body_stmts, body_str) =
                        gen_expr_hoisted(body, entity, ctx, scope, scratch);
                    for comp in comp_names.iter().rev() {
                        scratch.borrow_mut().pop_let_binding(comp);
                    }
                    scratch.borrow_mut().pop_tuple_binding(name);
                    stmts.extend(body_stmts);
                    return (stmts, body_str);
                }
            }
            if let Expr::RecordUpdate(base, updates) = body.as_ref() {
                if let Expr::Ident(base_name) = base.as_ref() {
                    if base_name == name
                        && matches!(
                            val.as_ref(),
                            Expr::IntLiteral(v) if *v == cambrian_core::U256::ZERO
                        )
                    {
                        if let Some(rec_ty) =
                            infer_record_from_update_fields(entity, ctx, updates, scratch)
                        {
                            let san = sol_sanitize_ident(name);
                            let record_name = rec_ty
                                .strip_suffix(" memory")
                                .unwrap_or(&rec_ty)
                                .to_string();
                            let default_init = solidity_default_for_member_ty(
                                entity,
                                &Type::Simple(record_name.clone()),
                                ctx,
                            );
                            let mut stmts = Vec::new();
                            stmts.push(format!("{} {} = {};", rec_ty, san, default_init));
                            scratch.borrow_mut().push_let_binding(name, &rec_ty);
                            let field_types: std::collections::HashMap<String, String> = entity
                                .records
                                .iter()
                                .find(|r| r.name == record_name)
                                .or_else(|| ctx.lookup_record(&record_name))
                                .map(|rec| {
                                    rec.fields
                                        .iter()
                                        .map(|f| {
                                            (
                                                f.name.clone(),
                                                sol_type_entity(entity, &f.ty, true, ctx),
                                            )
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            for (field, val) in updates {
                                let (val_stmts, val_str) =
                                    gen_expr_hoisted(val, entity, ctx, scope, scratch);
                                stmts.extend(val_stmts);
                                let val_str = match field_types.get(field) {
                                    Some(field_ty) => maybe_narrow_cast(
                                        field_ty, &val_str, val, entity, ctx, scratch,
                                    ),
                                    None => val_str,
                                };
                                stmts.push(format!("{}.{} = {};", san, field, val_str));
                            }
                            scratch.borrow_mut().pop_let_binding(name);
                            return (stmts, san);
                        }
                    }
                }
            }
            let let_ty = infer_let_type_entity(val, entity, ctx, scratch);
            // Phase EVM-P0-B follow-up: explicit cast when the let
            // slot is narrow but the rendered RHS is wider (typical
            // for `let x: uint64 = _now - last_claim;` where `_now` is
            // `block.timestamp`'s `uint256`). Skipped when the
            // expression's `actual_sol_type` already matches.
            let val_str = maybe_narrow_cast(&let_ty, &val_str, val, entity, ctx, scratch);
            stmts.push(format!("{} {} = {};", let_ty, sol_sanitize_ident(name), val_str));
            // Phase EVM-15 H2: register the let-binding's type so any
            // downstream `Expr::Ident(name)` in the body resolves
            // correctly (e.g. `let p = m_proposals[k]; p { ... }`
            // needs `p`'s type when emitting the `RecordUpdate`).
            scratch.borrow_mut().push_let_binding(name, &let_ty);
            if let Expr::EnumVariantWithData(en, var, _) = val.as_ref() {
                scratch
                    .borrow_mut()
                    .push_payload_enum_binding(name, en, var);
            }
            let (body_stmts, body_str) = gen_expr_hoisted(body, entity, ctx, scope, scratch);
            scratch.borrow_mut().pop_let_binding(name);
            if matches!(val.as_ref(), Expr::EnumVariantWithData(_, _, _)) {
                scratch.borrow_mut().pop_payload_enum_binding(name);
            }
            stmts.extend(body_stmts);
            (stmts, body_str)
        }
        Expr::Let(Pattern::Wildcard, val, body) => {
            // Wildcard let — evaluate val for side effects (mapping ops), then continue
            let (mut stmts, _val_str) = gen_expr_hoisted(val, entity, ctx, scope, scratch);
            let (body_stmts, body_str) = gen_expr_hoisted(body, entity, ctx, scope, scratch);
            stmts.extend(body_stmts);
            (stmts, body_str)
        }
        Expr::Let(Pattern::Tuple(pats), val, body) => {
            let (mut stmts, val_str) = gen_expr_hoisted(val, entity, ctx, scope, scratch);
            // EVM-H10: native Solidity tuple destructure (not `expr[i]`).
            let inferred = infer_tuple_elem_types(val, entity, ctx, scratch);
            let mut binders: Vec<String> = Vec::with_capacity(pats.len());
            let mut bound_names: Vec<String> = Vec::new();
            for (i, pat) in pats.iter().enumerate() {
                let slot_ty = inferred
                    .as_ref()
                    .and_then(|tys| tys.get(i).cloned())
                    .unwrap_or_else(|| "uint256".to_string());
                match pat {
                    Pattern::Ident(n) => {
                        binders.push(format!("{} {}", slot_ty, sol_sanitize_ident(n)));
                        scratch.borrow_mut().push_let_binding(n, &slot_ty);
                        bound_names.push(n.clone());
                    }
                    Pattern::Wildcard => binders.push(String::new()),
                    _ => binders.push(String::new()),
                }
            }
            stmts.push(format!("({}) = {};", binders.join(", "), val_str));
            let (body_stmts, body_str) = gen_expr_hoisted(body, entity, ctx, scope, scratch);
            for n in &bound_names {
                scratch.borrow_mut().pop_let_binding(n);
            }
            stmts.extend(body_stmts);
            (stmts, body_str)
        }
        Expr::If(cond, then_expr, else_opt) => {
            let (cond_stmts, cond_str) = gen_expr_hoisted(cond, entity, ctx, scope, scratch);
            let (then_stmts, then_str) = gen_expr_hoisted(then_expr, entity, ctx, scope, scratch);
            let (else_stmts, else_str) = match else_opt {
                Some(e) => gen_expr_hoisted(e, entity, ctx, scope, scratch),
                // EVM-H9: type-aware default (bool → false, not `0`).
                None => {
                    let ty = infer_let_type_entity(then_expr, entity, ctx, scratch);
                    (vec![], default_sol_literal(&ty))
                }
            };
            if cond_stmts.is_empty() && then_stmts.is_empty() && else_stmts.is_empty() {
                return (vec![], format!("({} ? {} : {})", cond_str, then_str, else_str));
            }
            // Need temp variable
            let tmp = scratch.borrow_mut().next_tmp();
            let ty = infer_let_type_entity(
                &Expr::If(cond.clone(), then_expr.clone(), else_opt.clone()),
                entity,
                ctx,
                scratch,
            );
            let mut stmts = cond_stmts;
            stmts.push(format!("{} {};", ty, tmp));
            stmts.push(format!("if ({}) {{", cond_str));
            for s in then_stmts {
                stmts.push(format!("    {}", s));
            }
            stmts.push(format!("    {} = {};", tmp, then_str));
            stmts.push("} else {".to_string());
            for s in else_stmts {
                stmts.push(format!("    {}", s));
            }
            stmts.push(format!("    {} = {};", tmp, else_str));
            stmts.push("}".to_string());
            (stmts, tmp)
        }
        Expr::Match(subject, arms) => {
            if let Some(meta) = crate::codegen::std_str::parse_str_meta_from_expr(subject) {
                return gen_match_on_std_parse(subject, arms, meta, entity, ctx, scope, scratch);
            }
            let (subj_stmts, subj_str) = gen_expr_hoisted(subject, entity, ctx, scope, scratch);
            let tmp = scratch.borrow_mut().next_tmp();
            let first_body_ty =
                infer_match_result_ty(subject, arms, entity, ctx, scratch);
            let mut stmts = subj_stmts;
            stmts.push(format!(
                "{} {} = {};",
                first_body_ty,
                tmp,
                default_sol_literal(&first_body_ty)
            ));
            // Phase EVM-4 J3: payload-pattern binding substitution
            // rewrites the arm body so `Expr::Ident(binder)` becomes
            // `Expr::FieldAccess(Expr::Ident(subj_str), field)`. We
            // synthesise the subject as `Ident(subj_str)` because the
            // hoisted subject already produced a Solidity local /
            // value held in `subj_str`.
            let subj_expr = Expr::Ident(subj_str.clone());
            let option_struct = {
                let ty = infer_let_type_entity(subject, entity, ctx, scratch);
                let name = strip_memory_ty(&ty);
                name.starts_with("Option_").then(|| name.to_string())
            };
            let mut first = true;
            for arm in arms {
                let (cond, effective_body): (Option<String>, Expr) = match &arm.pattern {
                    MatchPattern::Wildcard | MatchPattern::Ident(_) => (None, arm.body.clone()),
                    MatchPattern::EnumVariant(en, vn) => (
                        Some(payload_or_unit_enum_cond(&subj_str, en, vn, ctx)),
                        arm.body.clone(),
                    ),
                    MatchPattern::EnumVariantWithData(en, vn, payload_pats) => {
                        // Phase EVM-4 J3: tag check + binding
                        // substitution. If we can't resolve the enum
                        // declaration, leave bindings in place — solc
                        // will surface the unresolved identifier.
                        let body = if let Some(decl) = ctx.lookup_enum(en) {
                            subst_payload_bindings(&arm.body, &subj_expr, &decl, vn, payload_pats)
                        } else {
                            arm.body.clone()
                        };
                        (
                            Some(payload_or_unit_enum_cond(&subj_str, en, vn, ctx)),
                            body,
                        )
                    }
                    MatchPattern::IntLiteral(n) => {
                        (Some(format!("{} == {}", subj_str, n)), arm.body.clone())
                    }
                    MatchPattern::BoolLiteral(b) => (
                        Some(format!(
                            "{} == {}",
                            subj_str,
                            if *b { "true" } else { "false" }
                        )),
                        arm.body.clone(),
                    ),
                    MatchPattern::None => {
                        let cond = match &option_struct {
                            Some(name) => format!("{subj_str}.tag == {name}_Tag.None"),
                            None => format!("{} == 0", subj_str),
                        };
                        (Some(cond), arm.body.clone())
                    }
                    MatchPattern::Some(inner) => {
                        let cond = match &option_struct {
                            Some(name) => format!("{subj_str}.tag == {name}_Tag.Some"),
                            None => format!("{} != 0", subj_str),
                        };
                        let body = if let Pattern::Ident(binder) = inner {
                            subst_ident_in_expr(
                                &arm.body,
                                binder,
                                &Expr::FieldAccess(
                                    Box::new(subj_expr.clone()),
                                    "some_0".into(),
                                ),
                            )
                        } else {
                            arm.body.clone()
                        };
                        (Some(cond), body)
                    }
                };
                let (body_stmts, body_str) = gen_expr_hoisted(&effective_body, entity, ctx, scope, scratch);
                if let Some(c) = cond {
                    stmts.push(if first {
                        format!("if ({}) {{", c)
                    } else {
                        format!("}} else if ({}) {{", c)
                    });
                    first = false;
                } else {
                    // Wildcard / binding
                    stmts.push(if first { "{".to_string() } else { "} else {".to_string() });
                    first = false;
                    if let MatchPattern::Ident(bind) = &arm.pattern {
                        stmts.push(format!("    uint256 {} = {};", bind, subj_str));
                    }
                }
                for s in body_stmts {
                    stmts.push(format!("    {}", s));
                }
                let body_val = maybe_narrow_cast(
                    &first_body_ty,
                    &body_str,
                    &effective_body,
                    entity,
                    ctx,
                    scratch,
                );
                stmts.push(format!("    {} = {};", tmp, body_val));
            }
            if !first {
                stmts.push("}".to_string());
            }
            (stmts, tmp)
        }
        Expr::Block(items) => {
            // Block is a sequence where the last item is the value
            let mut stmts = vec![];
            let mut last = "0".to_string();
            for item in items {
                let (s, v) = gen_expr_hoisted(item, entity, ctx, scope, scratch);
                stmts.extend(s);
                last = v;
            }
            (stmts, last)
        }
        Expr::ArrayLit(items) => {
            let tmp = scratch.borrow_mut().next_tmp();
            let elem_ty = items
                .first()
                .map(|e| infer_let_type_entity(e, entity, ctx, scratch))
                .unwrap_or_else(|| "uint256".to_string());
            let n = items.len();
            let mut stmts = vec![format!(
                "{0}[] memory {1} = new {0}[]({2});",
                elem_ty, tmp, n
            )];
            for (i, item) in items.iter().enumerate() {
                let (item_stmts, item_str) = gen_expr_hoisted(item, entity, ctx, scope, scratch);
                stmts.extend(item_stmts);
                stmts.push(format!("{}[{}] = {};", tmp, i, item_str));
            }
            (stmts, tmp)
        }
        Expr::RecordUpdate(base, updates) => {
            let (mut stmts, base_str) = gen_expr_hoisted(base, entity, ctx, scope, scratch);
            let tmp = scratch.borrow_mut().next_tmp();
            let ty = infer_let_type_entity(
                &Expr::RecordUpdate(base.clone(), updates.clone()),
                entity,
                ctx,
                scratch,
            );
            let base_ty = infer_let_type_entity(base, entity, ctx, scratch);
            let init = if ty.ends_with(" memory") && base_ty != ty {
                if let Some(member_name) = scratch.borrow().lookup_transform_member_name() {
                    member_name.clone()
                } else {
                    let record_name = ty.strip_suffix(" memory").unwrap_or(&ty);
                    solidity_default_for_member_ty(
                        entity,
                        &Type::Simple(record_name.to_string()),
                        ctx,
                    )
                }
            } else {
                base_str.clone()
            };
            stmts.push(format!("{} {} = {};", ty, tmp, init));
            // Phase EVM-P0-B follow-up: cast narrow record fields so
            // assignments from wider intermediates (e.g. `uint256`
            // arithmetic results) don't trip Solidity's implicit
            // conversion check.
            let record_name = ty
                .strip_suffix(" memory")
                .unwrap_or(&ty)
                .to_string();
            let field_types: std::collections::HashMap<String, String> = entity
                .records
                .iter()
                .find(|r| r.name == record_name)
                .or_else(|| ctx.lookup_record(&record_name))
                .map(|rec| {
                    rec.fields
                        .iter()
                        .map(|f| (f.name.clone(), sol_type_entity(entity, &f.ty, true, ctx)))
                        .collect()
                })
                .unwrap_or_default();
            for (field, val) in updates {
                let (val_stmts, val_str) = gen_expr_hoisted(val, entity, ctx, scope, scratch);
                stmts.extend(val_stmts);
                let val_str = match field_types.get(field) {
                    Some(field_ty) => maybe_narrow_cast(field_ty, &val_str, val, entity, ctx, scratch),
                    None => val_str,
                };
                stmts.push(format!("{}.{} = {};", tmp, field, val_str));
            }
            (stmts, tmp)
        }
        Expr::EnumVariantWithData(enum_name, variant, args) => {
            // `evm::name(args)` (no turbofish) routes through the EVM
            // namespace dispatch — same path as the `NamespacedCall`
            // case above. Argument hoisting still happens via the
            // `gen_evm_ns` -> `gen_expr` chain (each arg is inline).
            if enum_name == "evm" {
                if let Some(rendered) = gen_evm_ns(variant.as_str(), args, ctx, scope, scratch) {
                    return (Vec::new(), rendered);
                }
            }
            // SD-01d: `Lib::fn(args)` → library call with HashMap sidecars.
            if library_qualifies(enum_name, variant, ctx) {
                if ctx.lookup_pure_fn_exists_params(variant, Some(enum_name)).is_some()
                    || ctx.lookup_pure_fn_keys_params(variant, Some(enum_name)).is_some()
                {
                    if !args.iter().all(|a| matches!(a, Expr::Ident(_))) {
                        return (
                            vec![format!(
                                "// SD-01d: library fn `{}::{}` requires bare member identifiers for HashMap sidecars",
                                enum_name, variant
                            )],
                            "0".to_string(),
                        );
                    }
                }
                let mut stmts: Vec<String> = Vec::new();
                let mut arg_strs: Vec<String> = Vec::with_capacity(args.len());
                for a in args {
                    let (s, v) = gen_expr_hoisted(a, entity, ctx, scope, scratch);
                    stmts.extend(s);
                    arg_strs.push(v);
                }
                if let Some(call) = format_pure_fn_call(
                    variant,
                    Some(enum_name.as_str()),
                    args,
                    arg_strs,
                    scope.entity.as_deref(),
                    ctx,
                    scratch,
                )
                {
                    return (stmts, call);
                }
            }
            // Phase EVM-4 J2: tagged-union constructor. The hoisted
            // path collects per-arg statements first so subexpressions
            // (let, if, calls...) can sequence cleanly before the
            // struct literal.
            if let Some(decl) = ctx.lookup_enum(enum_name) {
                if is_payload_enum(&decl) {
                    let mut stmts: Vec<String> = Vec::new();
                    let mut arg_strs: Vec<String> = Vec::with_capacity(args.len());
                    for a in args {
                        let (s, v) = gen_expr_hoisted(a, entity, ctx, scope, scratch);
                        stmts.extend(s);
                        arg_strs.push(v);
                    }
                    let lit = payload_enum_construct(entity, &decl, variant, ctx, |i| {
                        Some(arg_strs.get(i).cloned().unwrap_or_else(|| "0".to_string()))
                    });
                    return (stmts, lit);
                }
            }
            let stmts = vec![format!(
                "revert(\"EVM: enum variant {}::{} with data not representable\");",
                enum_name, variant
            )];
            let _ = args;
            (stmts, "0".to_string())
        }
        Expr::NamespacedCall { namespace, name, .. } => {
            // Phase EVM-2 K3 (defence in depth): the validator's E15
            // (gosh::* in expr position) and E16 (any other namespace
            // in expr position) reject this shape. If we still got
            // here, the validator was bypassed — fail loudly in debug
            // builds and leave a self-documenting marker in release.
            debug_assert!(
                false,
                "EVM-2 K3: `{}::{}` namespaced call in expr position should have been rejected by validator E15/E16",
                namespace, name
            );
            (
                vec![format!(
                    "// EVM-2 K3: `{}::{}` should have been rejected by E15/E16 {}",
                    namespace, name, UNLOWERED_VALUE_MARKER
                )],
                "0".to_string(),
            )
        }
        Expr::For(pat, iter, body) => gen_for_loop(pat, iter, body, entity, ctx, scope, scratch),
        // `<iter>.fold(init, |acc, x| body)` — single-step (no chain
        // stages) accumulator loop. Chain-fusion already intercepted
        // every shape with peelable stages above; this arm covers the
        // bare-source case (`Range`, `Vec<T>`, `m.iter()`-direct) that
        // `parse_iter_chain` rejects with `None`.
        Expr::MethodCall(base, method, args)
            if method == "fold"
                && args.len() == 2
                && matches!(&args[1], Expr::Closure(p, _) if p.len() == 2) =>
        {
            let (closure_params, closure_body) = match &args[1] {
                Expr::Closure(params, body) => (params, body.as_ref()),
                _ => unreachable!("guarded by matches! above"),
            };
            gen_fold_loop(
                base,
                &args[0],
                &closure_params[0],
                &closure_params[1],
                closure_body,
                entity,
                ctx,
                scope,
                scratch,
            )
        }
        Expr::BinOp(lhs, op, rhs) => {
            let (mut stmts, ls) = gen_expr_hoisted(lhs, entity, ctx, scope, scratch);
            let (rs_stmts, rs) = gen_expr_hoisted(rhs, entity, ctx, scope, scratch);
            stmts.extend(rs_stmts);
            let expr = if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) {
                let lty = infer_expr_sol_ty(lhs, entity, ctx, scratch, InferSolMode::Decl);
                let rty = infer_expr_sol_ty(rhs, entity, ctx, scratch, InferSolMode::Decl);
                let ls = record_scalar_binop_lhs(lhs, &ls, entity, ctx, scratch);
                if lty.ends_with("[] memory") || rty.ends_with("[] memory") {
                    if lty.ends_with("[] memory") {
                        let elem = lty.strip_suffix("[] memory").unwrap_or("uint256");
                        format!("({}({}) + {})", array_memory_sum_fn(elem), ls, rs)
                    } else {
                        let elem = rty.strip_suffix("[] memory").unwrap_or("uint256");
                        format!("({} + {}({}))", ls, array_memory_sum_fn(elem), rs)
                    }
                } else if let Some((ls, rs)) =
                    gen_binop_operands_solidity(lhs, rhs, op, entity, ctx, scope, scratch)
                {
                    format!("({} {} {})", ls, binop_str(op), rs)
                } else {
                    format!("({} {} {})", ls, binop_str(op), rs)
                }
            } else if matches!(op, BinOp::And | BinOp::Or) {
                let lty = infer_expr_sol_ty(lhs, entity, ctx, scratch, InferSolMode::Flow);
                let rty = infer_expr_sol_ty(rhs, entity, ctx, scratch, InferSolMode::Flow);
                let ls = if lty == "bool" { ls } else { format!("({} != 0)", ls) };
                let rs = if rty == "bool" { rs } else { format!("({} != 0)", rs) };
                format!("({} {} {})", ls, binop_str(op), rs)
            } else if let Some((ls, rs)) =
                gen_binop_operands_solidity(lhs, rhs, op, entity, ctx, scope, scratch)
            {
                format!("({} {} {})", ls, binop_str(op), rs)
            } else {
                format!("({} {} {})", ls, binop_str(op), rs)
            };
            (stmts, expr)
        }
        Expr::Range(_, _) => (
            vec!["revert(\"EVM: range expression only valid as `for` iterator on EVM\");".to_string()],
            "0".to_string(),
        ),
        Expr::Closure(_, _) => (
            vec![
                "revert(\"EVM: closure-as-value not supported (use `for x in iter { ... }` directly)\");"
                    .to_string(),
            ],
            "0".to_string(),
        ),
        // Phase EVM-2 K3 (defence in depth): generic args-hoisting
        // fallbacks for shapes whose `gen_expr` path returns `None`
        // when an arg needs sequenced setup (HashMap-typed args, large
        // expressions). Without these the catch-all below would emit
        // the legacy `/* unsupported expr */ 0` sentinel — a silent
        // miscompile.
        Expr::FnCall(name, args) => {
            // Pure fns whose body calls `.exists(param)` / iterates a
            // HashMap param need parallel sidecars threaded after each
            // bare `Ident` argument (see `lookup_pure_fn_*_params`).
            if ctx.lookup_pure_fn_exists_params(name, None).is_some()
                || ctx.lookup_pure_fn_keys_params(name, None).is_some()
            {
                if !args.iter().all(|a| matches!(a, Expr::Ident(_))) {
                    return (
                        vec![format!(
                            "// EVM-2 K3: pure fn `{}` takes a HashMap-typed param requiring sidecar args — call sites must pass a bare entity-member identifier (got non-Ident arg).",
                            name
                        )],
                        "0".to_string(),
                    );
                }
            }
            let mut stmts: Vec<String> = Vec::new();
            let mut arg_strs: Vec<String> = Vec::with_capacity(args.len());
            for a in args {
                let (s, v) = gen_expr_hoisted(a, entity, ctx, scope, scratch);
                stmts.extend(s);
                arg_strs.push(v);
            }
            let call = format_pure_fn_call(
                name,
                None,
                args,
                arg_strs.clone(),
                scope.entity.as_deref(),
                ctx,
                scratch,
            )
                .unwrap_or_else(|| format!("{}({})", name, arg_strs.join(", ")));
            (stmts, call)
        }
        Expr::MethodCall(base, method, args) => {
            if method == "collect" && args.is_empty() {
                return gen_expr_hoisted(base, entity, ctx, scope, scratch);
            }
            if (method == "len" || method == "length") && args.is_empty() {
                if let Expr::Ident(n) = base.as_ref() {
                    if entity.members.iter().any(|m| {
                        &m.name == n && matches!(&m.ty, Type::Generic(g, _) if g == "HashMap")
                    }) {
                        return (Vec::new(), format!("{}_keys.length", n));
                    }
                }
                let (stmts, base_str) = gen_expr_hoisted(base, entity, ctx, scope, scratch);
                return (
                    stmts,
                    solidity_length_expr(base, &base_str, entity, ctx, scratch),
                );
            }
            if method == "split" && args.len() == 1 {
                let (mut stmts, base_str) = gen_expr_hoisted(base, entity, ctx, scope, scratch);
                let (s, delim) = gen_expr_hoisted(&args[0], entity, ctx, scope, scratch);
                stmts.extend(s);
                return (stmts, format!("_cam_string_split({}, {})", base_str, delim));
            }
            if let Expr::Ident(lib_name) = base.as_ref() {
                if library_qualifies(lib_name, method, ctx) {
                    let mut arg_strs: Vec<String> = Vec::with_capacity(args.len());
                    let mut stmts: Vec<String> = Vec::new();
                    for a in args {
                        let (s, v) = gen_expr_hoisted(a, entity, ctx, scope, scratch);
                        stmts.extend(s);
                        arg_strs.push(v);
                    }
                    narrow_pure_fn_args(method, args, &mut arg_strs, scope, ctx, scratch);
                    if let Some(call) = format_pure_fn_call(
                        method,
                        Some(lib_name.as_str()),
                        args,
                        arg_strs,
                        scope.entity.as_deref(),
                        ctx,
                        scratch,
                    ) {
                        return (stmts, call);
                    }
                }
            }
            let (mut stmts, base_str) = gen_expr_hoisted(base, entity, ctx, scope, scratch);
            let mut arg_strs: Vec<String> = Vec::with_capacity(args.len());
            for a in args {
                let (s, v) = gen_expr_hoisted(a, entity, ctx, scope, scratch);
                stmts.extend(s);
                arg_strs.push(v);
            }
            (
                stmts,
                format!("{}.{}({})", base_str, method, arg_strs.join(", ")),
            )
        }
        _ => {
            // Phase EVM-2 K3 (defence in depth): the validator's E16
            // rejects every `Expr` shape that has no defined EVM
            // lowering. If we still got here, either:
            //   1. an internal pre-pass synthesised an AST node that
            //      didn't go through validation (audit the caller), or
            //   2. a new `Expr` variant was added without updating
            //      `check_evm_compat_expr`.
            // Emit a self-documenting sentinel so `hoist_is_real` is
            // false (I17 / `require(true)` fallback) rather than a
            // silently-wrong value. Do not `debug_assert` — invariant
            // classification and coverage tests inject synthetic nodes.
            //
            // `hoist_is_real` only guards predicate positions. In a value
            // position the `0` IS the result, so the marker below is what
            // stops the build: `find_unlowered_values` fails the transpile
            // before a `return 0;` masquerading as a computation can ship.
            (
                vec![format!(
                    "// EVM-2 K3: unsupported expression — should have been rejected by E16 {}",
                    UNLOWERED_VALUE_MARKER
                )],
                "0".to_string(),
            )
        }
    }
}
