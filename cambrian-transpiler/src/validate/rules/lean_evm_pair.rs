// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean-EVM adapter-pair contracts — rules about how LeanCore models the
//! EVM world (atomic-step semantics, failure surfaces). Bound
//! `Pair(Domain::Evm, Language::Lean)`.
//!
//! L8 (typed send must statically resolve), L9 (capturing call against a
//! failing route needs a fail surface), L10 (sub-call interleavings elided
//! in atomic steps), L11 (fire-and-forget self-send needs a fail surface).
//! `rescue`/`recover` is **E26** on the EVM domain (Acki Nacki bounce only).

use super::family_runners;
use super::lean_lang::collect as collect_lean_compat;

#[allow(dead_code)]
pub const CODES: &[&str] = &["L8", "L9", "L10", "L11"];

family_runners! {
    "lean_compat", collect_lean_compat,
    run_l8 => "L8",
    run_l9 => "L9",
    run_l10 => "L10",
    run_l11 => "L11",
}
