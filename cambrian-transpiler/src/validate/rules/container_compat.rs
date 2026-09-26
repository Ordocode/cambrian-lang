// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Container-specific hard rejections for expressions without a native lowering.

use super::family_runners;
use super::tvm_constructs::collect_evm_compat;

#[allow(dead_code)]
pub const CODES: &[&str] = &["E24", "E25"];

family_runners! {
    "evm_compat", collect_evm_compat,
    run_e24 => "E24",
    run_e25 => "E25",
}
