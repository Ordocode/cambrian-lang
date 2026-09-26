// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! LeanCore expressivity rules — constructs the Lean carrier language
//! cannot lower, regardless of domain (correct for a future second Lean
//! domain).
//!
//! L1 (unsupported generics), L2 (closure-as-value / unsupported fold
//! accumulators), L5 (vacuous `expect throw` against total routes), L6
//! (`expect return` without a return type), L7 (`skip from` not honoured),
//! L13 (`std::crypto` has no Lean lowering), L14 (unknown simple type),
//! L15 (HashMap transform shapes — Lean mirror of Solidity E17),
//! L16 (namespaced call arity / name has no Lean lowering — UPSTREAM B-33).

use super::super::registry::ValidateCtx;
use super::super::{check_lean_target_compat, Diagnostic};
use super::family_runners;

#[allow(dead_code)]
pub const CODES: &[&str] = &["L1", "L2", "L5", "L6", "L7", "L13", "L14", "L15", "L16"];

pub fn collect(ctx: &ValidateCtx<'_>, diags: &mut Vec<Diagnostic>) {
    diags.extend(check_lean_target_compat(ctx.program));
}

family_runners! {
    "lean_compat", collect,
    run_l1 => "L1",
    run_l2 => "L2",
    run_l5 => "L5",
    run_l6 => "L6",
    run_l7 => "L7",
    run_l13 => "L13",
    run_l14 => "L14",
    run_l15 => "L15",
    run_l16 => "L16",
}
