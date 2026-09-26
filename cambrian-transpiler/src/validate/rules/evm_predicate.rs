// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! I17: EVM/Solidity cannot lower this invariant `check` expression.
//!
//! Bound to [`Language::Solidity`] (same idea as E07): Foundry / revm /
//! cargo-fuzz only. Lean already lowers ComputeExpr via `gen_expr` and
//! must not fire.

use crate::codegen::predicate_expr::{classify_invariant_check, i17_message, CheckKind};
use crate::codegen::solidity::EvmCtx;

use super::super::registry::ValidateCtx;
use super::super::Diagnostic;

#[allow(dead_code)]
pub const CODES: &[&str] = &["I17"];

pub fn run(ctx: &ValidateCtx<'_>, diags: &mut Vec<Diagnostic>) {
    let evm_ctx = EvmCtx::build(ctx.program, ctx.deterministic, true);
    for inv in &ctx.program.invariants {
        for idx in 0..inv.checks.len() {
            if matches!(
                classify_invariant_check(ctx.program, inv, idx, &evm_ctx),
                CheckKind::Unsupported
            ) {
                diags.push(Diagnostic::error("I17", i17_message(&inv.name, idx)));
            }
        }
    }
}
