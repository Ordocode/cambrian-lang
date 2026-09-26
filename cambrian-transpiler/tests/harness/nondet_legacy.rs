// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! U4-6 Step 11: legacy nondet manifest emptied — baselines must stay at 0.

/// Explicit `deterministic_addresses: false` (repo-relative paths).
pub const NONDET_LEGACY_YAML: &[&str] = &[];

/// Implicit nondet: `target: evm` without `deterministic_addresses` field.
pub const NONDET_LEGACY_IMPLICIT: &[&str] = &[];
