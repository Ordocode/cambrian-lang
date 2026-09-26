// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! I7: multi-instance (`for { ... }` system) invariants are unsupported by
//! the TVM backend — the Acki Nacki codegen stops at the multi-entity
//! harness. Foundry / revm / Lean all support them, so this binds
//! `Domains(&[Domain::Tvm])`: the first purely-TVM validation rule.

use super::super::registry::ValidateCtx;
use super::super::Diagnostic;

#[allow(dead_code)]
pub const CODES: &[&str] = &["I7"];

pub fn run(ctx: &ValidateCtx<'_>, diags: &mut Vec<Diagnostic>) {
    for inv in &ctx.program.invariants {
        if !inv.is_single_entity() {
            diags.push(Diagnostic::error("I7",
                format!("invariant \"{}\": multi-instance ('for system') invariants are only supported on the EVM (Foundry/revm) targets, not on Acki Nacki",
                    inv.name)));
        }
    }
}
