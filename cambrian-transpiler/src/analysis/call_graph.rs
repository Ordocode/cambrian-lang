// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Same-entity and cross-entity call/send edge extraction.

use std::collections::BTreeSet;

use crate::ast::{Entity, Program, Route, RouteAction};

use super::send_target::{classify_dest, SendTarget};
use super::walk::for_each_route_action;

/// Names of same-entity routes that `route` invokes synchronously —
/// via `call name(...)`, or a typed self `~>` / `var … ~>` send whose
/// destination resolves to `<Self>.address(...)`.
pub fn same_entity_callees(
    program: &Program,
    entity: &Entity,
    route: &Route,
) -> Vec<String> {
    let mut deps: BTreeSet<String> = BTreeSet::new();
    for_each_route_action(&route.body, |action| match action {
        RouteAction::CallRoute { name, .. } => {
            if entity.routes.iter().any(|r| r.name == *name) {
                deps.insert(name.clone());
            }
        }
        RouteAction::Send {
            message: Some(msg),
            dest,
            ..
        }
        | RouteAction::VarCall {
            message: msg, dest, ..
        } => {
            if let SendTarget::SameEntity { .. } = classify_dest(dest, entity, route, program)
            {
                if entity.routes.iter().any(|r| r.name == *msg) {
                    deps.insert(msg.clone());
                }
            }
        }
        _ => {}
    });
    deps.into_iter().collect()
}

/// Other in-program entities whose routes this entity's routes call
/// directly (typed cross-entity `~>` / `var … ~>`). Includes
/// `DynamicTyped` dispatch targets since those still need `Other.Routes.*`
/// in scope for Lean.
pub fn cross_entity_route_dependencies(program: &Program, entity: &Entity) -> Vec<String> {
    let mut deps = BTreeSet::new();
    for route in &entity.routes {
        for_each_route_action(&route.body, |action| {
            if let Some(ent) = cross_entity_target(action, entity, route, program) {
                deps.insert(ent);
            }
        });
    }
    deps.into_iter().collect()
}

fn cross_entity_target(
    action: &RouteAction,
    self_entity: &Entity,
    route: &Route,
    program: &Program,
) -> Option<String> {
    let dest = match action {
        RouteAction::Send { dest, .. } | RouteAction::VarCall { dest, .. } => dest,
        _ => return None,
    };
    match classify_dest(dest, self_entity, route, program) {
        SendTarget::CrossEntity { entity, .. } | SendTarget::DynamicTyped { entity, .. } => {
            Some(entity)
        }
        _ => None,
    }
}
