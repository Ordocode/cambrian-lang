// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Dependency order for top-level `pure fn` declarations.
//!
//! Lean has no forward references outside `mutual`, so a `pure fn` must be
//! emitted after everything it calls. Records and enums were already ordered
//! this way; pure fns were emitted in source order, which happens to work
//! only while every call points backwards. It stopped working the moment a
//! project imported a library: the importer's functions are parsed first and
//! call into definitions that land hundreds of lines below them.

use std::collections::HashSet;

use crate::ast::{Expr, PureFn};
use crate::graph;

use super::route_facts::walk_expr_children;

fn collect(expr: &Expr, universe: &HashSet<&str>, self_name: &str, out: &mut Vec<String>) {
    if let Expr::FnCall(name, _) = expr {
        if universe.contains(name.as_str()) && name != self_name && !out.contains(name) {
            out.push(name.clone());
        }
    }
    walk_expr_children(expr, &mut |child| collect(child, universe, self_name, out));
}

/// Pure-fn callees of `f`, restricted to the top-level universe.
pub fn pure_fn_callees(f: &PureFn, universe: &HashSet<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    collect(&f.body, universe, &f.name, &mut out);
    out
}

/// Top-level pure fns in callee-first order.
///
/// Mutual recursion keeps source order for the cycle (the back edge is
/// dropped, matching `order_type_items`) — Lean needs a `mutual` block for
/// that shape, which is a separate concern from plain forward references.
pub fn order_pure_fns(pure_fns: &[PureFn]) -> Vec<&PureFn> {
    let universe: HashSet<&str> = pure_fns.iter().map(|f| f.name.as_str()).collect();
    let names: Vec<String> = pure_fns.iter().map(|f| f.name.clone()).collect();
    let by_name = graph::index_map(&names);
    let callees: Vec<Vec<usize>> = pure_fns
        .iter()
        .map(|f| {
            pure_fn_callees(f, &universe)
                .into_iter()
                .filter_map(|name| by_name.get(&name).copied())
                .collect()
        })
        .collect();

    graph::dfs_topo(pure_fns.len(), |idx| callees[idx].clone())
        .into_iter()
        .filter_map(|idx| pure_fns.get(idx))
        .collect()
}
