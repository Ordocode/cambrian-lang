// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Kernel-facing Lean emission helpers (types, exprs support, pure, iter, maps,
//! state-local route builders).

pub mod deceq;
pub mod emitter;
pub mod hashmap;
pub mod iter;
pub mod map_analysis;
pub mod pure;
pub mod route_local;
pub mod state;
pub mod stmt;
pub mod types;
