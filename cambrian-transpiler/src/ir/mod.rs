// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Mid-level IR between AST validation and language-specific codegen.
//!
//! The LeanCore `Local` signature (see `codegen/lean/core/route_local.rs`) is the
//! contract for what a lowered route fragment must expose; this module is the
//! shared, target-agnostic representation that all backends can consume during
//! the P6 migration.
//!
//! Step A establishes the skeleton (`ResolvedType`, `TypedExpr`, `RouteIr`) and
//! AST lowering entry points. Step B adds shared type inference and explicit
//! numeric coercion nodes. Solidity-specific and Lean BitVec-width inference
//! remain printer-side until later phases.

pub mod expr;
pub mod lower;
pub mod route;
pub mod ty;

pub use expr::{
    coerce_binop_operands, coerce_kind_for_cast, infer_type, CoerceKind, InferCtx, TypedExpr,
    TypedExprKind, IR_BIN_LITERAL_CAST, IR_HEX_LITERAL_CAST,
};
pub use lower::{lower_expr, lower_program, lower_route, LowerCtx};
pub use route::{IrStmt, IrTransform, PhaseIr, ProgramIr, RouteIr};
pub use ty::{resolve_type, ResolvedType};
