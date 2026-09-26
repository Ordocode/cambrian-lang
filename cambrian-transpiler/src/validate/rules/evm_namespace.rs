// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! E12: `evm::*` namespace forbidden off the EVM domain.
//!
//! The mirror image of `tvm_constructs`: `evm::` calls belong to the EVM
//! world model, so the binding is `NotDomains(&[Evm])` — it fires on TVM,
//! Container, and any future non-EVM domain.

use super::super::registry::ValidateCtx;
use super::super::{check_non_evm_target_compat, Diagnostic};

#[allow(dead_code)]
pub const CODES: &[&str] = &["E12"];

pub fn run(ctx: &ValidateCtx<'_>, diags: &mut Vec<Diagnostic>) {
    diags.extend(check_non_evm_target_compat(ctx.program, ctx.target_name()));
}
