// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM world-model rules — shared by every core on `Domain::Evm`
//! (SolidityCore and LeanCore today).
//!
//! E09 (from-clause lowers to `msg.sender`), E16 (namespace lowering /
//! `address_of` outside deterministic mode / test-only `encode`), E22
//! (typed send target needs a declared interface), E28 (`Option` / collection
//! methods with no Solidity or Lean lowering), V33 (`from Entity(...)`
//! arity under CREATE2 address derivation), V62 (`msg::sender` in init route
//! under factory deploy), V63 (missing `#[factory_only]` on init route),
//! V36 (≤3 `indexed` event params —
//! the EVM ABI-log topic limit).

use super::super::registry::ValidateCtx;
use super::super::warnings;
use super::super::{check_events, Diagnostic};
use super::family_runners;
use super::tvm_constructs::collect_evm_compat;

#[allow(dead_code)]
pub const CODES: &[&str] = &["E09", "E16", "E22", "E28", "V33", "V62", "V63", "V36", "W10"];

fn collect(ctx: &ValidateCtx<'_>, diags: &mut Vec<Diagnostic>) {
    collect_evm_compat(ctx, diags);
    // V36 lives in the events walk but binds Domains([Evm]).
    check_events(ctx.program, diags);
    for entity in &ctx.program.entities {
        warnings::lint_missing_emit_on_ledger_routes(entity, diags);
    }
}

family_runners! {
    "evm_domain", collect,
    run_e09 => "E09",
    run_e16 => "E16",
    run_e22 => "E22",
    run_e28 => "E28",
    run_v33 => "V33",
    run_v62 => "V62",
    run_v63 => "V63",
    run_v36 => "V36",
    run_w10 => "W10",
}
