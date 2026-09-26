// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Shared graph utilities for declaration / call-graph ordering.
//!
//! Used by the Lean backend (type topo, route SCCs, entity SCCs). Kept
//! algorithmically faithful to the previous private implementations so
//! emitted declaration order stays byte-identical.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// DFS topological order over nodes `0..n`.
///
/// `deps(i)` returns the indices that `i` depends on (must appear before
/// `i`). Cycles fall back to declaration order on the back-edge: a node
/// already on the recursion stack (or finished) is skipped, matching the
/// previous `lean_types::order_type_items` behaviour.
pub fn dfs_topo(n: usize, mut deps: impl FnMut(usize) -> Vec<usize>) -> Vec<usize> {
    // 0 = unvisited, 1 = on-stack, 2 = done
    let mut state = vec![0u8; n];
    let mut ordered: Vec<usize> = Vec::with_capacity(n);

    fn visit(
        idx: usize,
        deps_of: &mut dyn FnMut(usize) -> Vec<usize>,
        state: &mut [u8],
        ordered: &mut Vec<usize>,
    ) {
        match state[idx] {
            1 | 2 => return,
            _ => {}
        }
        state[idx] = 1;
        for di in deps_of(idx) {
            if di < state.len() {
                visit(di, deps_of, state, ordered);
            }
        }
        state[idx] = 2;
        ordered.push(idx);
    }

    for i in 0..n {
        visit(i, &mut deps, &mut state, &mut ordered);
    }
    ordered
}

/// Options for [`tarjan_sccs`].
#[derive(Debug, Clone, Copy, Default)]
pub struct TarjanOptions {
    /// Reverse the condensation so dependees appear before dependents
    /// (entity-level emission). Route SCCs leave this false — Tarjan's
    /// natural order already matches their historical emit order.
    pub reverse_condensation: bool,
    /// When true, sort each SCC by the source index of its members
    /// (declaration order). Used by route SCCs.
    pub sort_sccs_by_source_order: bool,
}

/// Tarjan strongly connected components over string-named nodes
/// (R. Tarjan, 1972).
///
/// `names` is the discovery order. `deps[v]` lists callees / dependees of
/// `v`; edges to names outside `deps`'s key set are ignored (same filter
/// as the previous Lean copies).
pub fn tarjan_sccs(
    names: &[String],
    deps: &HashMap<String, Vec<String>>,
    opts: TarjanOptions,
) -> Vec<Vec<String>> {
    let mut index_of: HashMap<String, usize> = HashMap::new();
    let mut lowlink: HashMap<String, usize> = HashMap::new();
    let mut on_stack: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = Vec::new();
    let mut index = 0usize;
    let mut sccs: Vec<Vec<String>> = Vec::new();

    fn strongconnect(
        v: &str,
        deps: &HashMap<String, Vec<String>>,
        index_of: &mut HashMap<String, usize>,
        lowlink: &mut HashMap<String, usize>,
        on_stack: &mut HashSet<String>,
        stack: &mut Vec<String>,
        index: &mut usize,
        sccs: &mut Vec<Vec<String>>,
    ) {
        index_of.insert(v.to_string(), *index);
        lowlink.insert(v.to_string(), *index);
        *index += 1;
        stack.push(v.to_string());
        on_stack.insert(v.to_string());

        if let Some(callees) = deps.get(v) {
            for w in callees {
                if !deps.contains_key(w) {
                    continue;
                }
                if !index_of.contains_key(w) {
                    strongconnect(w, deps, index_of, lowlink, on_stack, stack, index, sccs);
                    let lw = *lowlink.get(w).unwrap();
                    let lv = *lowlink.get(v).unwrap();
                    lowlink.insert(v.to_string(), lv.min(lw));
                } else if on_stack.contains(w) {
                    let iw = *index_of.get(w).unwrap();
                    let lv = *lowlink.get(v).unwrap();
                    lowlink.insert(v.to_string(), lv.min(iw));
                }
            }
        }

        if lowlink.get(v) == index_of.get(v) {
            let mut comp = Vec::new();
            loop {
                let w = stack.pop().unwrap();
                on_stack.remove(&w);
                comp.push(w.clone());
                if w == v {
                    break;
                }
            }
            sccs.push(comp);
        }
    }

    for n in names {
        if !index_of.contains_key(n) {
            strongconnect(
                n,
                deps,
                &mut index_of,
                &mut lowlink,
                &mut on_stack,
                &mut stack,
                &mut index,
                &mut sccs,
            );
        }
    }

    if opts.sort_sccs_by_source_order {
        let source_idx: HashMap<&str, usize> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i))
            .collect();
        for comp in &mut sccs {
            comp.sort_by_key(|n| source_idx.get(n.as_str()).copied().unwrap_or(usize::MAX));
        }
    }

    if opts.reverse_condensation {
        sccs.reverse();
    }

    sccs
}

/// Convenience: build a name→index map for `Hash`+`Eq` keys.
#[allow(dead_code)]
pub fn index_map<T: Eq + Hash + Clone>(items: &[T]) -> HashMap<T, usize> {
    items
        .iter()
        .enumerate()
        .map(|(i, t)| (t.clone(), i))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dfs_topo_respects_deps() {
        // 0 depends on 1; 1 depends on nothing → order [1, 0, 2]
        let order = dfs_topo(3, |i| match i {
            0 => vec![1],
            _ => vec![],
        });
        assert_eq!(order, vec![1, 0, 2]);
    }

    #[test]
    fn tarjan_mutual_cycle_one_scc() {
        let names = vec!["a".into(), "b".into()];
        let mut deps = HashMap::new();
        deps.insert("a".into(), vec!["b".into()]);
        deps.insert("b".into(), vec!["a".into()]);
        let sccs = tarjan_sccs(
            &names,
            &deps,
            TarjanOptions {
                reverse_condensation: false,
                sort_sccs_by_source_order: true,
            },
        );
        assert_eq!(sccs.len(), 1);
        assert_eq!(sccs[0], vec!["a".to_string(), "b".to_string()]);
    }
}
