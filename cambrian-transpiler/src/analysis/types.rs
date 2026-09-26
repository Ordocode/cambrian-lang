// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Type-reference graph and declaration ordering.
//!
//! Lean has no forward references for `structure` / `inductive` (outside a
//! `mutual` block); EVM benefits from the same dependency order even though
//! Solidity tolerates forward references.

use std::collections::HashSet;

use crate::ast::{EnumDecl, Record, Type};
use crate::graph;

/// One user-defined type declaration (record or enum) for dependency ordering.
pub enum TypeItem<'a> {
    Record(&'a Record),
    Enum(&'a EnumDecl),
}

impl<'a> TypeItem<'a> {
    pub fn name(&self) -> &'a str {
        match self {
            TypeItem::Record(r) => &r.name,
            TypeItem::Enum(e) => &e.name,
        }
    }

    /// Names of *sibling* records / enums this item references in its
    /// field / variant-payload types.
    fn deps(&self, universe: &HashSet<&str>) -> Vec<String> {
        let mut acc = HashSet::new();
        match self {
            TypeItem::Record(r) => {
                for f in &r.fields {
                    collect_type_names(&f.ty, &mut acc);
                }
            }
            TypeItem::Enum(e) => {
                for v in &e.variants {
                    for t in &v.fields {
                        collect_type_names(t, &mut acc);
                    }
                }
            }
        }
        // Sort so the topological visit order is deterministic: HashSet
        // iteration order varies per process (SipHash seed).
        let mut deps: Vec<String> = acc
            .into_iter()
            .filter(|n| universe.contains(n.as_str()) && n != self.name())
            .collect();
        deps.sort();
        deps
    }
}

/// Collect every base type name mentioned in `ty` (recursing through
/// generics / tuples / typed-addresses).
fn collect_type_names(ty: &Type, into: &mut HashSet<String>) {
    match ty {
        Type::Simple(n) | Type::TypedAddress(n) => {
            into.insert(n.clone());
        }
        Type::Generic(n, params) => {
            into.insert(n.clone());
            for p in params {
                collect_type_names(p, into);
            }
        }
        Type::Tuple(parts) => {
            for p in parts {
                collect_type_names(p, into);
            }
        }
    }
}

/// Topologically order `records` + `enums` so every type is emitted
/// after the sibling types it references. Cycles fall back to declaration
/// order on the back-edge (`dfs_topo` skips already-visited nodes).
pub fn order_type_items<'a>(
    records: &'a [Record],
    enums: &'a [EnumDecl],
) -> Vec<TypeItem<'a>> {
    let items: Vec<TypeItem<'a>> = records
        .iter()
        .map(TypeItem::Record)
        .chain(enums.iter().map(TypeItem::Enum))
        .collect();
    let universe: HashSet<&str> = items.iter().map(|i| i.name()).collect();
    let by_name: std::collections::HashMap<String, usize> = items
        .iter()
        .enumerate()
        .map(|(i, it)| (it.name().to_string(), i))
        .collect();
    let deps_of = |idx: usize| -> Vec<usize> {
        items[idx]
            .deps(&universe)
            .into_iter()
            .filter_map(|d| by_name.get(d.as_str()).copied())
            .collect()
    };
    let ordered = graph::dfs_topo(items.len(), deps_of);
    let mut out: Vec<TypeItem<'a>> = Vec::with_capacity(items.len());
    let mut src: Vec<Option<TypeItem<'a>>> = items.into_iter().map(Some).collect();
    for idx in ordered {
        out.push(src[idx].take().expect("each index visited once"));
    }
    out
}
