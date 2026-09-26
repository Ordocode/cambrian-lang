// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Temporal `^member` dependency DAG (V3).
//!
//! Owns construction of per-route member transform order. Validate still
//! surfaces V3 diagnostics; codegen consumes the orders without recomputing.

use std::collections::{HashMap, HashSet};

use crate::ast::{Entity, Expr};

/// Per-route temporal dependency info.
#[derive(Debug, Clone)]
pub struct TemporalOrder {
    pub route_name: String,
    pub order: Vec<String>,
}

/// A V3 diagnostic produced while building temporal orders. Validate
/// converts these into its own `Diagnostic` type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemporalError {
    pub code: &'static str,
    pub message: String,
}

/// Build temporal DAG for all routes, check for cycles, return computation order.
pub fn build_temporal_orders(entity: &Entity) -> (Vec<TemporalOrder>, Vec<TemporalError>) {
    let mut orders = Vec::new();
    let mut diags = Vec::new();

    let member_names: HashSet<&str> = entity.members.iter().map(|m| m.name.as_str()).collect();

    let mut route_transforms: HashMap<&str, Vec<(&str, &Expr)>> = HashMap::new();
    for member in &entity.members {
        for transform in &member.transforms {
            route_transforms
                .entry(transform.route_name.as_str())
                .or_default()
                .push((member.name.as_str(), &transform.body));
        }
    }

    for (route_name, transforms) in &route_transforms {
        let mut deps: HashMap<String, Vec<String>> = HashMap::new();
        let mut members_in_route: Vec<String> = Vec::new();
        let mut seen_members: HashSet<String> = HashSet::new();

        for (member_name, body) in transforms {
            if seen_members.insert(member_name.to_string()) {
                members_in_route.push(member_name.to_string());
            }
            let temporal_refs = collect_temporal_refs(body);
            for tref in &temporal_refs {
                if member_names.contains(tref.as_str()) {
                    deps.entry(member_name.to_string())
                        .or_default()
                        .push(tref.clone());
                } else {
                    diags.push(TemporalError {
                        code: "V3",
                        message: format!(
                            "Temporal reference '^{}' in member '{}' route '{}' does not refer to a known member",
                            tref, member_name, route_name
                        ),
                    });
                }
            }
            deps.entry(member_name.to_string()).or_default();
        }

        match topological_sort_owned(&members_in_route, &deps) {
            Ok(sorted) => {
                orders.push(TemporalOrder {
                    route_name: route_name.to_string(),
                    order: sorted,
                });
            }
            Err(cycle) => {
                diags.push(TemporalError {
                    code: "V3",
                    message: format!(
                        "Temporal cycle in route '{}': {}",
                        route_name,
                        cycle.join(" -> ")
                    ),
                });
            }
        }
    }

    (orders, diags)
}

fn collect_temporal_refs(expr: &Expr) -> Vec<String> {
    let mut refs = Vec::new();
    collect_temporal_refs_rec(expr, &mut refs);
    refs
}

fn collect_temporal_refs_rec(expr: &Expr, refs: &mut Vec<String>) {
    match expr {
        Expr::TemporalRef(name) => refs.push(name.clone()),
        Expr::BinOp(l, _, r) => {
            collect_temporal_refs_rec(l, refs);
            collect_temporal_refs_rec(r, refs);
        }
        Expr::UnaryOp(_, e) => collect_temporal_refs_rec(e, refs),
        Expr::FieldAccess(e, _) => collect_temporal_refs_rec(e, refs),
        Expr::Index(e, idx) => {
            collect_temporal_refs_rec(e, refs);
            collect_temporal_refs_rec(idx, refs);
        }
        Expr::MethodCall(e, _, args) => {
            collect_temporal_refs_rec(e, refs);
            for a in args {
                collect_temporal_refs_rec(a, refs);
            }
        }
        Expr::FnCall(_, args) => {
            for a in args {
                collect_temporal_refs_rec(a, refs);
            }
        }
        Expr::If(cond, then_e, else_e) => {
            collect_temporal_refs_rec(cond, refs);
            collect_temporal_refs_rec(then_e, refs);
            if let Some(el) = else_e {
                collect_temporal_refs_rec(el, refs);
            }
        }
        Expr::Let(_, val, body) => {
            collect_temporal_refs_rec(val, refs);
            collect_temporal_refs_rec(body, refs);
        }
        Expr::Block(stmts) => {
            for s in stmts {
                collect_temporal_refs_rec(s, refs);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                collect_temporal_refs_rec(v, refs);
            }
        }
        Expr::RecordUpdate(base, fields) => {
            collect_temporal_refs_rec(base, refs);
            for (_, v) in fields {
                collect_temporal_refs_rec(v, refs);
            }
        }
        Expr::Closure(_, body) => collect_temporal_refs_rec(body, refs),
        Expr::Cast(e, _) => collect_temporal_refs_rec(e, refs),
        Expr::Tuple(elems) => {
            for e in elems {
                collect_temporal_refs_rec(e, refs);
            }
        }
        Expr::MacroRef(_, args) => {
            for a in args {
                collect_temporal_refs_rec(a, refs);
            }
        }
        Expr::Match(subject, arms) => {
            collect_temporal_refs_rec(subject, refs);
            for arm in arms {
                collect_temporal_refs_rec(&arm.body, refs);
            }
        }
        Expr::Some(inner) => collect_temporal_refs_rec(inner, refs),
        Expr::ArrayLit(elems) => {
            for e in elems {
                collect_temporal_refs_rec(e, refs);
            }
        }
        Expr::EnumVariantWithData(_, _, args) => {
            for a in args {
                collect_temporal_refs_rec(a, refs);
            }
        }
        Expr::NamespacedCall { args, .. } => {
            for a in args {
                collect_temporal_refs_rec(a, refs);
            }
        }
        Expr::Range(start, end) => {
            collect_temporal_refs_rec(start, refs);
            collect_temporal_refs_rec(end, refs);
        }
        Expr::For(_, iter, body) => {
            collect_temporal_refs_rec(iter, refs);
            collect_temporal_refs_rec(body, refs);
        }
        Expr::IntLiteral(_)
       
       
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::BoolLiteral(_)
        | Expr::EmptyCollection
        | Expr::Ident(_)
        | Expr::MsgField(_)
        | Expr::SysField(_)
        | Expr::TraceField(_)
        | Expr::TraceCall { .. }
        | Expr::EnumVariant(_, _)
        | Expr::None
        | Expr::AddressOf { .. }
        | Expr::Encode { .. } => {}
    }
}

fn topological_sort_owned(
    nodes: &[String],
    deps: &HashMap<String, Vec<String>>,
) -> Result<Vec<String>, Vec<String>> {
    let node_set: HashSet<&str> = nodes.iter().map(|s| s.as_str()).collect();
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();

    for node in nodes {
        in_degree.entry(node.as_str()).or_insert(0);
        adj.entry(node.as_str()).or_default();
    }

    for (node, node_deps) in deps {
        for dep in node_deps {
            if node_set.contains(dep.as_str()) {
                adj.entry(dep.as_str()).or_default().push(node.as_str());
                *in_degree.entry(node.as_str()).or_insert(0) += 1;
            }
        }
    }

    let mut queue: Vec<&str> = nodes
        .iter()
        .map(|s| s.as_str())
        .filter(|n| in_degree.get(n).copied().unwrap_or(0) == 0)
        .collect();
    queue.sort();

    let mut result = Vec::new();

    while let Some(node) = queue.pop() {
        result.push(node.to_string());
        if let Some(neighbors) = adj.get(node) {
            for &next in neighbors {
                let deg = in_degree.get_mut(next).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push(next);
                    queue.sort();
                }
            }
        }
    }

    if result.len() != nodes.len() {
        let sorted_set: HashSet<&str> = result.iter().map(|s| s.as_str()).collect();
        let cycle: Vec<String> = nodes
            .iter()
            .filter(|n| !sorted_set.contains(n.as_str()))
            .cloned()
            .collect();
        Err(cycle)
    } else {
        Ok(result)
    }
}
