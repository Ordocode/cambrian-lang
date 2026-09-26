// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! SolidityCore expressivity rules — constructs the Solidity carrier
//! language cannot lower (option (a): other cores on the same domain are
//! not rejected for these).
//!
//! E07 (closures / iterator chains), E08 (tagged-union enum lowering info),
//! E17 (HashMap transform shapes), E18 (tuple erasure), E19 (unsupported
//! generics), E20 (unresolvable bare types), E21 (unsupported casts),
//! E23 (mixed-type tuple destructure inference), E29 (effects in a `view`),
//! T39 (`expect state` on a member with no public getter).

use super::super::registry::ValidateCtx;
use super::super::Diagnostic;
use super::family_runners;
use super::tvm_constructs::collect_evm_compat;

#[allow(dead_code)]
pub const CODES: &[&str] = &["E07", "E08", "E17", "E18", "E19", "E20", "E21", "E23", "E29"];

pub fn run_e29(ctx: &ValidateCtx<'_>, diags: &mut Vec<Diagnostic>) {
    for entity in &ctx.program.entities {
        super::super::entity::check_view_effects_solidity(entity, diags);
    }
}

/// T39: `expect state` on a member with no public Solidity getter.
pub fn run_t39(ctx: &ValidateCtx<'_>, diags: &mut Vec<Diagnostic>) {
    super::super::test_checks::check_expect_state_getters_solidity(ctx.program, diags);
}

family_runners! {
    "evm_compat", collect_evm_compat,
    run_e07 => "E07",
    run_e08 => "E08",
    run_e17 => "E17",
    run_e18 => "E18",
    run_e19 => "E19",
    run_e20 => "E20",
    run_e21 => "E21",
    run_e23 => "E23",
}
