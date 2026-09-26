// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM/Solidity codegen cluster — public face (mirrors `codegen::lean` layout).
//!
//! - [`core`] — language-level lowering (types, expressions, pure fns, libraries)
//! - [`evm`] — EVM adapter (entities, routes, factory, emitter, CEI ordering)

pub mod core;
pub mod evm;
pub mod unlowered;

pub(crate) use core::expr::gen_expr;
pub(crate) use core::types::{is_mapping_type, sol_type_entity};
pub(crate) use core::{
    active_ctx, gen_expr_test, gen_expr_test_ir, reset_active_ctx, reset_active_scratch,
    set_active_ctx, EmitScope, EmitScratch, EvmCtx,
};
pub(crate) use evm::analysis::transforms_use_exists;
pub use evm::{gen_evm_solidity, gen_evm_solidity_opts};
pub use unlowered::{find_unlowered_values, unlowered_value_report, UnloweredValue};
pub(crate) use evm::route::has_non_identity_init_params;
/// Audit / predictor mirror of EVM IR expr materialization (`emit_let`, send args, …).
pub use evm::route::materialize_typed_expr;
pub use evm::EvmActionEmitter;
