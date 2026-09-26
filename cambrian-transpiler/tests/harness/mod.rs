// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Shared EVM test harness utilities (U4-6 det-only migration).
//!
//! Include from integration-test binaries:
//! ```ignore
//! #[path = "harness/mod.rs"]
//! mod harness;
//! ```

pub mod evm_project;
pub mod factory_oracle;
pub mod nondet_legacy;
