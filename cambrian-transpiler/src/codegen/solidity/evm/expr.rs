// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM adapter expression entry — re-exports core lowering.
//!
//! Adapter call sites import from here for discoverability. The full
//! `gen_expr` / `gen_expr_hoisted` implementation lives in [`crate::codegen::solidity::core::expr`]
//! (domain arms included for P5; deeper adapter/core split is a follow-up).
