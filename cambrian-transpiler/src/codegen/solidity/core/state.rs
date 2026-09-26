// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM codegen — payload-enum helpers (no thread-local state).

use super::types::solidity_default_for_member_ty;

use crate::ast::{Entity, EnumDecl, EnumVariant};

use super::ctx::EvmCtx;

/// Phase EVM-4 J1: any variant carries fields → tagged union. Unit-only
/// enums keep the simple `enum X { ... }` Solidity form.
pub(crate) fn is_payload_enum(decl: &EnumDecl) -> bool {
    decl.variants.iter().any(|v| !v.fields.is_empty())
}

/// Phase EVM-4 J1: deterministic field name for payload-variant
/// position `idx`. Lowercased variant prefix avoids collisions across
/// variants (`Deposit(u64)` -> `deposit_0`, `Withdraw(u64, String)` ->
/// `withdraw_0`, `withdraw_1`).
pub(crate) fn payload_enum_field_name(variant: &EnumVariant, idx: usize) -> String {
    format!("{}_{}", variant.name.to_lowercase(), idx)
}

/// Phase EVM-4 J1/J2: build an `<Enum>({tag: ..., field_a: ..., ...})`
/// Solidity struct-literal expression. `active` selects which variant
/// of the enum is being constructed — its payload positions get the
/// matching `value_for_active(idx) -> Option<String>` value, every
/// other variant's payload field is zero-initialised via
/// `solidity_default_for_member_ty`.
pub(crate) fn payload_enum_construct<F>(
    entity: &Entity,
    decl: &EnumDecl,
    active: &str,
    ctx: &EvmCtx,
    value_for_active: F,
) -> String
where
    F: FnMut(usize) -> Option<String>,
{
    let mut value_for_active = value_for_active;
    let mut fields = vec![format!("tag: {}_Tag.{}", decl.name, active)];
    for variant in &decl.variants {
        for (idx, field_ty) in variant.fields.iter().enumerate() {
            let fname = payload_enum_field_name(variant, idx);
            let val = if variant.name == active {
                value_for_active(idx)
                    .unwrap_or_else(|| solidity_default_for_member_ty(entity, field_ty, ctx))
            } else {
                solidity_default_for_member_ty(entity, field_ty, ctx)
            };
            fields.push(format!("{}: {}", fname, val));
        }
    }
    format!("{}({{{}}})", decl.name, fields.join(", "))
}

/// Zero-initialise every payload field of `decl` (tag set to first variant).
pub(crate) fn payload_enum_zero_literal(entity: &Entity, decl: &EnumDecl, ctx: &EvmCtx) -> String {
    let tag = decl
        .variants
        .first()
        .map(|v| v.name.as_str())
        .unwrap_or("None");
    payload_enum_construct(entity, decl, tag, ctx, |_| None)
}
