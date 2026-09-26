// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! TVM-model constructs used on a domain that does not implement them.
//!
//! These rules reject (or warn about) constructs that belong to the TVM
//! message/SDK model — `gosh::*`, `msg::pubkey` / `currencies` / `body`,
//! `sys::pubkey` / `seqno`, `accept`, test `registry` blocks, TVM platform
//! effects, and `rescue`/`recover` bounce handling. Both `Domain::Tvm` and
//! `Domain::Container` (the native/WASM test-bed runtime, which lowers these
//! via portable `CamData` accessors)
//! implement the model, so the binding is `NotDomains(&[Tvm, Container])`:
//! today that fires only on the EVM domain, but any future domain without
//! the TVM message model inherits the rejection automatically.
//!
//! The diagnostics come from the shared EVM compat walk, filtered by code.
//! Container-unsupported (`E01`/`E02`/`E06`) uses `NotDomains(&[Tvm])` so it
//! also fires on Container; non-TVM-model codes use
//! `NotDomains(&[Tvm, Container])`.

use super::super::registry::ValidateCtx;
use super::super::{check_evm_target_compat_with, Diagnostic};
use super::family_runners;

#[allow(dead_code)]
pub const CONTAINER_UNSUPPORTED_CODES: &[&str] = &["E01", "E02", "E06"];
#[allow(dead_code)]
pub const NON_TVM_MODEL_CODES: &[&str] = &[
    "E03", "E04", "E05", "E10", "E11", "E13", "E14", "E15", "E26",
];

/// Shared EVM-compat walk used by every E* filter in this module and by
/// sibling Solidity / Container / EVM-domain families (same cache key).
pub fn collect_evm_compat(ctx: &ValidateCtx<'_>, diags: &mut Vec<Diagnostic>) {
    diags.extend(check_evm_target_compat_with(ctx.program, ctx.deterministic));
}

family_runners! {
    "evm_compat", collect_evm_compat,
    run_e01 => "E01",
    run_e02 => "E02",
    run_e06 => "E06",
    run_e03 => "E03",
    run_e04 => "E04",
    run_e05 => "E05",
    run_e10 => "E10",
    run_e11 => "E11",
    run_e13 => "E13",
    run_e14 => "E14",
    run_e15 => "E15",
    run_e26 => "E26",
}
