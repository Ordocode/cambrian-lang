// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! V40–V41: `receive` / `fallback` route shape and uniqueness.
//!
//! `receive` / `fallback` are EVM dispatcher concepts, so these bind to
//! `Domains(&[Evm])` and run for every EVM-domain core.

use super::super::registry::ValidateCtx;
use super::super::{check_receive_fallback, Diagnostic};
use super::family_runners;

#[allow(dead_code)]
pub const CODES: &[&str] = &["V40", "V41"];

pub fn collect(ctx: &ValidateCtx<'_>, diags: &mut Vec<Diagnostic>) {
    check_receive_fallback(ctx.program, diags);
}

family_runners! {
    "receive_fallback", collect,
    run_v40 => "V40",
    run_v41 => "V41",
}
