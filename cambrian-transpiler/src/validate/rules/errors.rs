// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! V38–V39: custom errors / throw.

use super::super::registry::ValidateCtx;
use super::super::{check_errors, Diagnostic};
use super::family_runners;

#[allow(dead_code)]
pub const CODES: &[&str] = &["V38", "V39"];

pub fn collect(ctx: &ValidateCtx<'_>, diags: &mut Vec<Diagnostic>) {
    check_errors(ctx.program, diags);
}

family_runners! {
    "errors", collect,
    run_v38 => "V38",
    run_v39 => "V39",
}
