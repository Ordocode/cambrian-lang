// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Static resolution of `~> dest` targets.
//!
//! Pure AST analysis — no language-core types. Send *lowering* stays in
//! adapters; this module only classifies destinations for call/send graphs.

use std::collections::HashMap;

use crate::ast::{Entity, Expr, Pattern, Program, Route, RouteAction, RouteBody, Type};

use super::route_facts::route_can_fail_evm_lean;

/// What a `~> dest` clause targets, after best-effort static resolution.
/// Anything that can't be classified as `SameEntity` is `Raw`; the
/// validator (`L8`) catches truly unresolvable typed targets before
/// codegen uses this.
#[derive(Debug, Clone, PartialEq)]
pub enum SendTarget {
    /// Same-entity self-call: `dest == <Self>.address(id_args)`.
    SameEntity { id_args: Vec<Expr> },
    /// Cross-entity typed call: `dest == <Other>.address(id_args)` for
    /// `Other` declared in the same program.
    CrossEntity {
        entity: String,
        id_args: Vec<Expr>,
    },
    /// Typed cross-entity dispatch on an `Address<E>` ident whose
    /// identity-args we don't have statically.
    DynamicTyped {
        entity: String,
        dest_ident: String,
    },
    /// Untyped `address`-typed ident dispatch.
    DynamicUntyped { dest_ident: String },
    /// Typed call to an `extern entity` route.
    ExternEntity { entity: String },
    /// Raw value transfer — `dest` is an opaque `Address`-valued expression.
    Raw,
}

/// Classify a `dest` expression against the current `entity`.
///
/// Accepted statically-known shapes:
///   * `<Self>.address(id_args)` — `MethodCall(Ident("<Self>"), "address", id_args)`.
///   * `<Self>.address()` / `<Self>.address` — singleton sugar.
///   * `addressOf(<Self>.state(id_args))` — `FnCall("addressOf", [MethodCall(...state...)])`.
///   * Ident `m_x` / `p` typed `Address<E>` for in-program `E` — typed dispatch.
///   * Ident `m_x` / `p` typed `address` — untyped dispatch.
pub fn classify_dest(
    dest: &Expr,
    self_entity: &Entity,
    route: &Route,
    program: &Program,
) -> SendTarget {
    if let Some((ent, id_args)) = resolve_entity_address(dest, program) {
        if ent == self_entity.name {
            return SendTarget::SameEntity { id_args };
        }
        return SendTarget::CrossEntity {
            entity: ent,
            id_args,
        };
    }
    if let Expr::Ident(name) = dest {
        if let Some(ty) = ident_type(name, self_entity, route) {
            match ty {
                Type::TypedAddress(ent) if program.entities.iter().any(|e| e.name == ent) => {
                    return SendTarget::DynamicTyped {
                        entity: ent,
                        dest_ident: name.clone(),
                    };
                }
                Type::TypedAddress(ent)
                    if program.extern_entities.iter().any(|e| e.name == ent) =>
                {
                    return SendTarget::ExternEntity { entity: ent };
                }
                Type::Simple(s) if s == "address" => {
                    return SendTarget::DynamicUntyped {
                        dest_ident: name.clone(),
                    };
                }
                _ => {}
            }
        }
    }
    SendTarget::Raw
}

/// Classify a typed message destination, including source-level `let` aliases
/// and the unique-extern-route fallback used by the Lean backend.
pub fn classify_message_dest(
    dest: &Expr,
    message: &str,
    self_entity: &Entity,
    route: &Route,
    program: &Program,
    let_env: &HashMap<String, Expr>,
) -> SendTarget {
    let resolved_dest = match dest {
        Expr::Ident(name) => let_env.get(name).unwrap_or(dest),
        _ => dest,
    };
    let target = classify_dest(resolved_dest, self_entity, route, program);
    if !matches!(target, SendTarget::DynamicUntyped { .. } | SendTarget::Raw) {
        return target;
    }

    let mut matches = program
        .extern_entities
        .iter()
        .filter(|entity| entity.routes.iter().any(|route| route.name == message));
    let Some(entity) = matches.next() else {
        return target;
    };
    if matches.next().is_some() {
        return target;
    }
    SendTarget::ExternEntity {
        entity: entity.name.clone(),
    }
}

/// True when `route` contains a typed or untyped dynamic-address send
/// (`Address<E>` / plain `address` dest). Those sites go through
/// `Dispatch.*` Except wrappers, so the caller is a fail surface (G-004).
pub fn route_has_dynamic_dispatch(
    program: &Program,
    entity: &Entity,
    route: &Route,
) -> bool {
    match &route.body {
        RouteBody::Unphased(actions) => {
            actions_have_dynamic_dispatch(actions, program, entity, route)
        }
        RouteBody::Phased(phases) => phases.iter().any(|phase| {
            actions_have_dynamic_dispatch(&phase.actions, program, entity, route)
        }),
        RouteBody::Mixed(phases, trailing) => {
            phases.iter().any(|phase| {
                actions_have_dynamic_dispatch(&phase.actions, program, entity, route)
            }) || actions_have_dynamic_dispatch(trailing, program, entity, route)
        }
    }
}

fn actions_have_dynamic_dispatch(
    actions: &[RouteAction],
    program: &Program,
    entity: &Entity,
    route: &Route,
) -> bool {
    for action in actions {
        let hit = match action {
            RouteAction::Send {
                message: Some(_),
                dest,
                ..
            }
            | RouteAction::VarCall { dest, .. } => matches!(
                classify_dest(dest, entity, route, program),
                SendTarget::DynamicTyped { .. } | SendTarget::DynamicUntyped { .. }
            ),
            RouteAction::Conditional {
                then_actions,
                else_actions,
                ..
            } => {
                actions_have_dynamic_dispatch(then_actions, program, entity, route)
                    || actions_have_dynamic_dispatch(else_actions, program, entity, route)
            }
            RouteAction::Rescue { action: inner, .. } => {
                actions_have_dynamic_dispatch(std::slice::from_ref(inner), program, entity, route)
            }
            RouteAction::For { body, .. } => {
                actions_have_dynamic_dispatch(body, program, entity, route)
            }
            _ => false,
        };
        if hit {
            return true;
        }
    }
    false
}

/// True when `route` contains an unrescued typed `Send` / `VarCall` whose
/// destination classifies as [`SendTarget::ExternEntity`].
///
/// Domain-neutral **syntactic** query — does not mean "can fail". The
/// evm×lean adapter ORs this into fail-mode (high-level ABI revert); other
/// domains may ignore it. `rescue` bodies are not walked (E26 on EVM/Lean).
fn callee_route_can_fail(program: &Program, target_entity: &str, message: &str) -> bool {
    let ent = match program.entities.iter().find(|e| e.name == target_entity) {
        Some(e) => e,
        None => return false,
    };
    let route = match ent.routes.iter().find(|r| r.name == message) {
        Some(r) => r,
        None => return false,
    };
    route_can_fail_evm_lean(ent, route)
}

/// True when `route` contains an unrescued typed cross-entity `Send` /
/// `VarCall` to an in-program entity route that can fail (EVM/Lean kernel).
///
/// The Lean adapter ORs this into fail-mode so total callers propagate callee
/// `.error` via `←` instead of `exceptGetD` (T-ARCH-002 / H-AX-08-01).
pub fn route_has_unrescued_failing_cross_send(
    program: &Program,
    entity: &Entity,
    route: &Route,
) -> bool {
    let empty = HashMap::new();
    match &route.body {
        RouteBody::Unphased(actions) => {
            actions_have_unrescued_failing_cross(actions, program, entity, route, &empty)
        }
        RouteBody::Phased(phases) => phases.iter().any(|phase| {
            actions_have_unrescued_failing_cross(&phase.actions, program, entity, route, &empty)
        }),
        RouteBody::Mixed(phases, trailing) => {
            phases.iter().any(|phase| {
                actions_have_unrescued_failing_cross(&phase.actions, program, entity, route, &empty)
            }) || actions_have_unrescued_failing_cross(trailing, program, entity, route, &empty)
        }
    }
}

fn actions_have_unrescued_failing_cross(
    actions: &[RouteAction],
    program: &Program,
    entity: &Entity,
    route: &Route,
    outer_let_env: &HashMap<String, Expr>,
) -> bool {
    let mut let_env = outer_let_env.clone();
    for action in actions {
        let hit = match action {
            RouteAction::Send {
                message: Some(message),
                dest,
                ..
            }
            | RouteAction::VarCall { message, dest, .. } => match classify_message_dest(
                dest,
                message,
                entity,
                route,
                program,
                &let_env,
            ) {
                SendTarget::CrossEntity {
                    entity: target_ent, ..
                } => callee_route_can_fail(program, &target_ent, message),
                _ => false,
            },
            RouteAction::Conditional {
                then_actions,
                else_actions,
                ..
            } => {
                actions_have_unrescued_failing_cross(then_actions, program, entity, route, &let_env)
                    || actions_have_unrescued_failing_cross(
                        else_actions,
                        program,
                        entity,
                        route,
                        &let_env,
                    )
            }
            RouteAction::For { body, .. } => {
                actions_have_unrescued_failing_cross(body, program, entity, route, &let_env)
            }
            RouteAction::Rescue { .. } => false,
            _ => false,
        };
        if hit {
            return true;
        }
        if let RouteAction::Let {
            pattern: Pattern::Ident(name),
            value,
        } = action
        {
            let_env.insert(name.clone(), value.clone());
        }
    }
    false
}

pub fn route_has_unrescued_extern_send(
    program: &Program,
    entity: &Entity,
    route: &Route,
) -> bool {
    let empty = HashMap::new();
    match &route.body {
        RouteBody::Unphased(actions) => {
            actions_have_unrescued_extern(actions, program, entity, route, &empty)
        }
        RouteBody::Phased(phases) => phases.iter().any(|phase| {
            actions_have_unrescued_extern(&phase.actions, program, entity, route, &empty)
        }),
        RouteBody::Mixed(phases, trailing) => {
            phases.iter().any(|phase| {
                actions_have_unrescued_extern(&phase.actions, program, entity, route, &empty)
            }) || actions_have_unrescued_extern(trailing, program, entity, route, &empty)
        }
    }
}

fn actions_have_unrescued_extern(
    actions: &[RouteAction],
    program: &Program,
    entity: &Entity,
    route: &Route,
    outer_let_env: &HashMap<String, Expr>,
) -> bool {
    let mut let_env = outer_let_env.clone();
    for action in actions {
        let hit = match action {
            RouteAction::Send {
                message: Some(message),
                dest,
                ..
            }
            | RouteAction::VarCall { message, dest, .. } => matches!(
                classify_message_dest(dest, message, entity, route, program, &let_env),
                SendTarget::ExternEntity { .. }
            ),
            RouteAction::Conditional {
                then_actions,
                else_actions,
                ..
            } => {
                actions_have_unrescued_extern(then_actions, program, entity, route, &let_env)
                    || actions_have_unrescued_extern(else_actions, program, entity, route, &let_env)
            }
            RouteAction::For { body, .. } => {
                actions_have_unrescued_extern(body, program, entity, route, &let_env)
            }
            RouteAction::Rescue { .. } => false,
            _ => false,
        };
        if hit {
            return true;
        }
        if let RouteAction::Let {
            pattern: Pattern::Ident(name),
            value,
        } = action
        {
            let_env.insert(name.clone(), value.clone());
        }
    }
    false
}

/// Resolve the static Cambrian type of `name` in this route's declared scope.
/// Checks route parameters first, then entity members.
pub fn ident_type(name: &str, entity: &Entity, route: &Route) -> Option<Type> {
    if let Some(p) = route.params.iter().find(|p| p.name == name) {
        return Some(p.ty.clone());
    }
    entity
        .members
        .iter()
        .find(|m| m.name == name)
        .map(|m| m.ty.clone())
}

/// If `dest` is `<E>.address(...)`, `addressOf(<E>.state(...))`, or
/// `address_of <E>(...)`, return `(E, id_args)` when `E` is in-program.
pub fn resolve_entity_address(dest: &Expr, program: &Program) -> Option<(String, Vec<Expr>)> {
    let in_program = |name: &str| program.entities.iter().any(|e| e.name == name);
    match dest {
        Expr::MethodCall(base, method, args) if method == "address" => {
            if let Expr::Ident(name) = base.as_ref() {
                if in_program(name) {
                    return Some((name.clone(), args.clone()));
                }
            }
        }
        Expr::FieldAccess(base, field) if field == "address" => {
            if let Expr::Ident(name) = base.as_ref() {
                if in_program(name) {
                    return Some((name.clone(), Vec::new()));
                }
            }
        }
        Expr::FnCall(name, args) if name == "addressOf" && args.len() == 1 => {
            if let Expr::MethodCall(base, method, state_args) = &args[0] {
                if method == "state" {
                    if let Expr::Ident(ent_name) = base.as_ref() {
                        if in_program(ent_name) {
                            return Some((ent_name.clone(), state_args.clone()));
                        }
                    }
                }
            }
        }
        Expr::AddressOf {
            entity_name, args, ..
        } if in_program(entity_name) => {
            return Some((entity_name.clone(), args.clone()));
        }
        _ => {}
    }
    None
}

fn send_options_has_value(send_options: Option<&Expr>) -> bool {
    let Some(opts) = send_options else {
        return false;
    };
    match opts {
        Expr::RecordConstruct(_, fields) => fields.iter().any(|(k, _)| k == "value"),
        _ => false,
    }
}

fn action_has_value_bearing_transfer(action: &RouteAction) -> bool {
    match action {
        RouteAction::Send { send_options, .. }
        | RouteAction::VarCall { send_options, .. } => send_options_has_value(send_options.as_ref()),
        RouteAction::Deploy { send_options, .. } => send_options_has_value(send_options.as_ref()),
        _ => false,
    }
}

fn actions_have_value_bearing_transfer(actions: &[RouteAction]) -> bool {
    for action in actions {
        if action_has_value_bearing_transfer(action) {
            return true;
        }
        match action {
            RouteAction::Conditional {
                then_actions,
                else_actions,
                ..
            } => {
                if actions_have_value_bearing_transfer(then_actions)
                    || actions_have_value_bearing_transfer(else_actions)
                {
                    return true;
                }
            }
            RouteAction::For { body, .. } => {
                if actions_have_value_bearing_transfer(body) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// True when `route` contains a typed send / deploy with `{ value: … }`.
///
/// Promoted into Lean fail-mode so underfunded transfers propagate via `←`
/// instead of `exceptGetD` (W2-BC-01 / T-ARCH-010, 011).
pub fn route_has_value_bearing_transfer(_program: &Program, _entity: &Entity, route: &Route) -> bool {
    match &route.body {
        RouteBody::Unphased(actions) => actions_have_value_bearing_transfer(actions),
        RouteBody::Phased(phases) => phases
            .iter()
            .any(|phase| actions_have_value_bearing_transfer(&phase.actions)),
        RouteBody::Mixed(phases, trailing) => {
            phases
                .iter()
                .any(|phase| actions_have_value_bearing_transfer(&phase.actions))
                || actions_have_value_bearing_transfer(trailing)
        }
    }
}
