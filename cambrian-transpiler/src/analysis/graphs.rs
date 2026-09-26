// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Program-wide dependency graphs (built once per program).

use std::collections::HashMap;

use crate::ast::{Entity, EnumDecl, Program, Record};
use crate::graph::{self, TarjanOptions};

use super::call_graph::{cross_entity_route_dependencies, same_entity_callees};
use super::route_facts::{compute_route_facts, RouteFacts};
use super::temporal::TemporalOrder;
use super::types::{order_type_items, TypeItem};

/// Stable identifier for a program- or entity-scoped type item.
/// `index` is into the scope's `records` then `enums` (records first).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeItemId {
    Program { index: usize },
    Entity { entity: String, index: usize },
}

/// Graphs and orderings computed once per program in the kernel.
#[derive(Debug, Clone, Default)]
pub struct ProgramGraphs {
    /// Program-scope records+enums in dependency order.
    pub program_type_order: Vec<TypeItemId>,
    /// Entity name → entity-local records+enums in dependency order.
    pub entity_type_order: HashMap<String, Vec<TypeItemId>>,
    /// (entity, route) → same-entity synchronous callees (call / self-send).
    pub route_callees: HashMap<(String, String), Vec<String>>,
    /// Entity name → route SCCs, callees-first, sources-ordered within SCC.
    pub route_sccs: HashMap<String, Vec<Vec<String>>>,
    /// Entity name → sibling entities its routes call.
    pub entity_send_deps: HashMap<String, Vec<String>>,
    /// Entity-level SCCs of the send graph, dependees-first.
    pub entity_sccs: Vec<Vec<String>>,
    /// Entity name → per-route temporal member order.
    pub temporal: HashMap<String, Vec<TemporalOrder>>,
    /// (entity, route) → universal per-route base facts.
    pub route_facts: HashMap<(String, String), RouteFacts>,
}

impl ProgramGraphs {
    /// Build all kernel graphs for `program`.
    pub fn build(program: &Program) -> Self {
        let mut g = ProgramGraphs::default();

        let prog_ordered = order_type_items(&program.records, &program.enums);
        g.program_type_order =
            items_to_program_ids(&prog_ordered, &program.records, &program.enums);

        for entity in &program.entities {
            let ordered = order_type_items(&entity.records, &entity.enums);
            g.entity_type_order.insert(
                entity.name.clone(),
                items_to_entity_ids(&ordered, &entity.name, &entity.records, &entity.enums),
            );
            let (orders, _) = super::temporal::build_temporal_orders(entity);
            g.temporal.insert(entity.name.clone(), orders);

            for route in &entity.routes {
                g.route_callees.insert(
                    (entity.name.clone(), route.name.clone()),
                    same_entity_callees(program, entity, route),
                );
            }

            let mut send_deps = cross_entity_route_dependencies(program, entity);
            // Drop self-edges — same-entity DynamicTyped lands here otherwise.
            send_deps.retain(|d| d != &entity.name);
            g.entity_send_deps
                .insert(entity.name.clone(), send_deps);

            g.route_sccs
                .insert(entity.name.clone(), compute_route_sccs(entity, &g));
        }

        g.entity_sccs = compute_entity_sccs(program, &g);
        g.route_facts = compute_route_facts(program);
        g
    }

    /// Resolve program-scope type items in dependency order.
    pub fn resolve_program_types<'a>(&self, program: &'a Program) -> Vec<TypeItem<'a>> {
        resolve_ids(&self.program_type_order, &program.records, &program.enums)
    }

    /// Resolve entity-local type items in dependency order.
    pub fn resolve_entity_types<'a>(&self, entity: &'a Entity) -> Vec<TypeItem<'a>> {
        let Some(ids) = self.entity_type_order.get(&entity.name) else {
            return Vec::new();
        };
        resolve_ids(ids, &entity.records, &entity.enums)
    }

    /// Same-entity callees for `(entity, route)`, empty if unknown.
    pub fn callees(&self, entity: &str, route: &str) -> &[String] {
        self.route_callees
            .get(&(entity.to_string(), route.to_string()))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Cross-entity send dependencies for `entity`.
    pub fn send_deps(&self, entity: &str) -> &[String] {
        self.entity_send_deps
            .get(entity)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

fn compute_route_sccs(entity: &Entity, g: &ProgramGraphs) -> Vec<Vec<String>> {
    let names: Vec<String> = entity.routes.iter().map(|r| r.name.clone()).collect();
    let mut deps: HashMap<String, Vec<String>> = HashMap::new();
    for r in &entity.routes {
        deps.insert(
            r.name.clone(),
            g.callees(&entity.name, &r.name).to_vec(),
        );
    }
    graph::tarjan_sccs(
        &names,
        &deps,
        TarjanOptions {
            reverse_condensation: false,
            sort_sccs_by_source_order: true,
        },
    )
}

fn compute_entity_sccs(program: &Program, g: &ProgramGraphs) -> Vec<Vec<String>> {
    let names: Vec<String> = program
        .entities
        .iter()
        .filter(|e| !e.routes.is_empty())
        .map(|e| e.name.clone())
        .collect();
    let mut deps: HashMap<String, Vec<String>> = HashMap::new();
    for e in &program.entities {
        if e.routes.is_empty() {
            continue;
        }
        let mut ds = g.send_deps(&e.name).to_vec();
        ds.retain(|d| d != &e.name && names.iter().any(|n| n == d));
        deps.insert(e.name.clone(), ds);
    }
    graph::tarjan_sccs(
        &names,
        &deps,
        TarjanOptions {
            reverse_condensation: true,
            sort_sccs_by_source_order: false,
        },
    )
}

fn items_to_program_ids(
    items: &[TypeItem<'_>],
    records: &[Record],
    enums: &[EnumDecl],
) -> Vec<TypeItemId> {
    items
        .iter()
        .filter_map(|it| source_index(it, records, enums).map(|index| TypeItemId::Program { index }))
        .collect()
}

fn items_to_entity_ids(
    items: &[TypeItem<'_>],
    entity: &str,
    records: &[Record],
    enums: &[EnumDecl],
) -> Vec<TypeItemId> {
    items
        .iter()
        .filter_map(|it| {
            source_index(it, records, enums).map(|index| TypeItemId::Entity {
                entity: entity.to_string(),
                index,
            })
        })
        .collect()
}

fn source_index(item: &TypeItem<'_>, records: &[Record], enums: &[EnumDecl]) -> Option<usize> {
    match item {
        TypeItem::Record(r) => records.iter().position(|x| x.name == r.name),
        TypeItem::Enum(e) => enums
            .iter()
            .position(|x| x.name == e.name)
            .map(|i| records.len() + i),
    }
}

fn resolve_ids<'a>(
    ids: &[TypeItemId],
    records: &'a [Record],
    enums: &'a [EnumDecl],
) -> Vec<TypeItem<'a>> {
    ids.iter()
        .filter_map(|id| {
            let index = match id {
                TypeItemId::Program { index } | TypeItemId::Entity { index, .. } => *index,
            };
            if index < records.len() {
                Some(TypeItem::Record(&records[index]))
            } else {
                enums.get(index - records.len()).map(TypeItem::Enum)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::*;

    fn parse(src: &str) -> Program {
        crate::ProgramParser::new()
            .parse(src)
            .unwrap_or_else(|e| panic!("parse failed: {e}"))
    }

    #[test]
    fn mutual_call_cycle_one_scc() {
        let program = parse(
            r#"
            entity E {
                routes {
                    a() => [ call b() ]
                    b() => [ call a() ]
                }
            }
            "#,
        );
        let g = ProgramGraphs::build(&program);
        let sccs = g.route_sccs.get("E").unwrap();
        assert_eq!(sccs.len(), 1);
        assert_eq!(sccs[0], vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn type_order_puts_dep_first() {
        let program = parse(
            r#"
            record A { b: B }
            record B { x: u64 }
            entity E {
                routes {
                    noop() => []
                }
            }
            "#,
        );
        let g = ProgramGraphs::build(&program);
        let ordered = g.resolve_program_types(&program);
        assert_eq!(ordered[0].name(), "B");
        assert_eq!(ordered[1].name(), "A");
    }

    #[test]
    fn cross_entity_chain_dependees_first() {
        let program = parse(
            r#"
            entity A {
                identity id: u64
                routes {
                    bump() => []
                }
            }
            entity B {
                identity id: u64
                routes {
                    poke() => [ bump() ~> A.address(id) ]
                }
            }
            "#,
        );
        let g = ProgramGraphs::build(&program);
        assert_eq!(g.send_deps("B"), &["A".to_string()]);
        assert!(g.send_deps("A").is_empty());
        // Acyclic: two singleton SCCs (order among them is Tarjan discovery
        // + reverse_condensation; Lake import edges, not vec order, matter).
        assert_eq!(g.entity_sccs.len(), 2);
        assert!(g.entity_sccs.iter().any(|s| s == &vec!["A".to_string()]));
        assert!(g.entity_sccs.iter().any(|s| s == &vec!["B".to_string()]));
    }
}
