// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Solidity codegen — language-level core (types, expressions, pure fns, iter).

pub(crate) mod alias;
pub mod create2;
pub mod ctx;
pub mod expr;
pub mod iter;
pub mod library;
pub mod option;
pub mod pure;
pub mod scratch;
pub mod state;
pub mod types;

pub(crate) use ctx::{
    active_ctx, gen_expr_test, gen_expr_test_ir, reset_active_ctx, set_active_ctx,
};
pub use ctx::{EmitScope, EvmCtx};
pub(crate) use scratch::{reset_active_scratch, EmitScratch};
