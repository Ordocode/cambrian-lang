// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! V34–V35: `emit` against declared events (universal kernel semantics).
//!
//! V36 (≤3 `indexed` params) is an EVM ABI-log limit and lives in
//! `evm_domain.rs` with a `Domains([Evm])` binding.

use super::super::registry::ValidateCtx;
use super::super::{check_events, Diagnostic};
use super::family_runners;

#[allow(dead_code)]
pub const CODES: &[&str] = &["V34", "V35"];

pub fn collect(ctx: &ValidateCtx<'_>, diags: &mut Vec<Diagnostic>) {
    check_events(ctx.program, diags);
}

family_runners! {
    "events", collect,
    run_v34 => "V34",
    run_v35 => "V35",
}
