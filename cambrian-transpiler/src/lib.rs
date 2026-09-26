// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

pub mod analysis;
pub mod ast;
pub mod catalog_stem;
pub mod desugar;
pub mod manifest;
pub mod merge_catalog;
pub mod diagnostic;
pub mod ir;
pub mod parse_recover;
pub mod validate;
pub mod codegen;
pub mod graph;
pub mod pretty;
pub mod project;
pub mod spec_normalize;
pub mod sourcemap;
pub mod target;
pub mod using_rewrite;

#[allow(clippy::all)]
#[allow(unused)]
mod cambrian;

pub use cambrian::*;
