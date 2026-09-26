// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use super::{collect_match_pattern_names, collect_pattern_names, pretty_type, Diagnostic};
use crate::ast::*;
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// V8: Action targets — cross-entity validation
// ---------------------------------------------------------------------------

pub(super) fn check_action_targets(
    entity: &Entity,
    all_entities: &[Entity],
    extern_entities: &[ExternEntity],
    diags: &mut Vec<Diagnostic>,
) {
    let route_names: HashSet<&str> = entity.routes.iter().map(|r| r.name.as_str()).collect();
    let entity_names: HashSet<&str> = all_entities.iter().map(|e| e.name.as_str()).collect();
    let extern_names: HashSet<&str> = extern_entities.iter().map(|e| e.name.as_str()).collect();
    for route in &entity.routes {
        for action in route.body.all_actions() {
            check_action_target_rec(
                action,
                &route_names,
                &entity_names,
                &extern_names,
                all_entities,
                entity,
                &route.name,
                diags,
            );
        }
    }
}

/// Phase EVM-6 M2 (V32): expected `deploy Entity(args)` arity for the
/// target entity's constructor. Sums identity members with init-route
/// parameters. Public so the EVM codegen can reuse the same definition.
pub fn expected_constructor_arity(entity: &Entity) -> usize {
    let identity_count = entity.members.iter().filter(|m| m.is_identity).count();
    let init_param_count = entity
        .routes
        .iter()
        .find(|r| r.is_init || r.name == "constructor")
        .map(|r| r.params.len())
        .unwrap_or(0);
    identity_count + init_param_count
}

fn check_action_target_rec(
    action: &RouteAction,
    route_names: &HashSet<&str>,
    entity_names: &HashSet<&str>,
    extern_names: &HashSet<&str>,
    all_entities: &[Entity],
    entity: &Entity,
    route_name: &str,
    diags: &mut Vec<Diagnostic>,
) {
    match action {
        RouteAction::CallRoute { name, .. } => {
            if !route_names.contains(name.as_str()) {
                diags.push(Diagnostic::error(
                    "V8",
                    format!(
                        "call action references undefined route '{}' in route '{}' of entity '{}'",
                        name, route_name, entity.name
                    ),
                ));
            }
        }
        RouteAction::Deploy {
            entity: deploy_entity,
            constructor_args,
            ..
        } => {
            if !entity_names.contains(deploy_entity.as_str())
                && !extern_names.contains(deploy_entity.as_str())
            {
                diags.push(Diagnostic::warning("W5",
                    format!("deploy action references entity '{}' which is not defined in this program, in route '{}' of entity '{}'",
                        deploy_entity, route_name, entity.name)));
            } else if let Some(target) = all_entities.iter().find(|e| e.name == *deploy_entity) {
                // V32: deploy arity must match the target entity's
                // identity members + init-route parameters. Skipped
                // when the target is an extern entity (signature
                // unknown) or unknown (W5 already covers it).
                let expected = expected_constructor_arity(target);
                if constructor_args.len() != expected {
                    diags.push(Diagnostic::error("V32",
                        format!(
                            "deploy {}(...) in route '{}' of entity '{}': expected {} constructor arguments (identity members + init parameters), got {}",
                            deploy_entity, route_name, entity.name, expected, constructor_args.len(),
                        )));
                }
            }
        }
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            for a in then_actions {
                check_action_target_rec(
                    a,
                    route_names,
                    entity_names,
                    extern_names,
                    all_entities,
                    entity,
                    route_name,
                    diags,
                );
            }
            for a in else_actions {
                check_action_target_rec(
                    a,
                    route_names,
                    entity_names,
                    extern_names,
                    all_entities,
                    entity,
                    route_name,
                    diags,
                );
            }
        }
        RouteAction::Rescue { action, .. } => {
            check_action_target_rec(
                action,
                route_names,
                entity_names,
                extern_names,
                all_entities,
                entity,
                route_name,
                diags,
            );
        }
        RouteAction::UpdateCode {
            callback_route,
            update_args,
            ..
        } => {
            if !route_names.contains(callback_route.as_str()) {
                diags.push(Diagnostic::error("V8",
                    format!("updateCode callback references undefined route '{}' in route '{}' of entity '{}'",
                        callback_route, route_name, entity.name)));
            }
            if update_args.len() != 2 {
                diags.push(Diagnostic::error("V25",
                    format!("gosh::updateCode requires exactly 2 arguments (code, wasmHash), got {} in route '{}' of entity '{}'",
                        update_args.len(), route_name, entity.name)));
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// V26: updateCode must be terminal action
// ---------------------------------------------------------------------------

pub(super) fn check_update_code_terminal(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    for route in &entity.routes {
        check_update_code_in_actions(route.body.all_actions(), &entity.name, &route.name, diags);
    }
}

fn check_update_code_in_actions(
    actions: Vec<&RouteAction>,
    entity_name: &str,
    route_name: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let mut seen_update_code = false;
    for action in &actions {
        if seen_update_code {
            diags.push(Diagnostic::error("V26",
                format!("Actions after gosh::updateCode(...) in route '{}' of entity '{}' will never execute. \
                         updateCode must be the last action.",
                    route_name, entity_name)));
            break;
        }
        if matches!(action, RouteAction::UpdateCode { .. }) {
            seen_update_code = true;
        }
        if let RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } = action
        {
            let then_refs: Vec<&RouteAction> = then_actions.iter().collect();
            check_update_code_in_actions(then_refs, entity_name, route_name, diags);
            let else_refs: Vec<&RouteAction> = else_actions.iter().collect();
            check_update_code_in_actions(else_refs, entity_name, route_name, diags);
        }
        if let RouteAction::For { body, .. } = action {
            let body_refs: Vec<&RouteAction> = body.iter().collect();
            check_update_code_in_actions(body_refs, entity_name, route_name, diags);
        }
    }
}

// ---------------------------------------------------------------------------
// V1: Every member transform references an existing route
// V2: Routes with no member transforms
// V9: View routes must not have member transforms
// V10: Pure routes must not have member transforms
// V11: Pure route purity
// V12: Init route constraints
// ---------------------------------------------------------------------------

pub(super) fn check_route_references(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    let route_names: HashSet<&str> = entity.routes.iter().map(|r| r.name.as_str()).collect();

    for member in &entity.members {
        for transform in &member.transforms {
            if !route_names.contains(transform.route_name.as_str()) {
                diags.push(Diagnostic::error(
                    "V1",
                    format!(
                        "Member '{}' has transform for unknown route '{}' in entity '{}'",
                        member.name, transform.route_name, entity.name
                    ),
                ));
            }
        }
    }

    // V2
    let transformed_routes: HashSet<&str> = entity
        .members
        .iter()
        .flat_map(|m| m.transforms.iter().map(|t| t.route_name.as_str()))
        .collect();

    for route in &entity.routes {
        if !transformed_routes.contains(route.name.as_str()) && route.body.is_empty() {
            diags.push(Diagnostic::warning("V2",
                format!("Route '{}' in entity '{}' has no member transforms and empty body (pure view?)",
                    route.name, entity.name)));
        }
    }

    // V9
    for route in &entity.routes {
        if route.is_view && transformed_routes.contains(route.name.as_str()) {
            diags.push(Diagnostic::error(
                "V9",
                format!(
                    "View route '{}' in entity '{}' must not modify state (has member transforms)",
                    route.name, entity.name
                ),
            ));
        }
    }
    for route in &entity.routes {
        if !route.is_view {
            continue;
        }
        if let Some(callee) = find_mutating_call_target(entity, route, &transformed_routes) {
            diags.push(Diagnostic::error(
                "V9",
                format!(
                    "View route '{}' in entity '{}' must not modify state (calls route '{}', which has member transforms or state-changing actions)",
                    route.name, entity.name, callee
                ),
            ));
        }
    }

    // V10
    for route in &entity.routes {
        if route.is_pure && transformed_routes.contains(route.name.as_str()) {
            diags.push(Diagnostic::error(
                "V10",
                format!(
                    "Pure route '{}' in entity '{}' must not modify state (has member transforms)",
                    route.name, entity.name
                ),
            ));
        }
    }

    // V11
    let member_names_set: HashSet<&str> = entity.members.iter().map(|m| m.name.as_str()).collect();
    for route in &entity.routes {
        if route.is_pure {
            let route_param_names: HashSet<&str> =
                route.params.iter().map(|p| p.name.as_str()).collect();
            for action in route.body.all_actions() {
                check_pure_route_action(
                    action,
                    &route.name,
                    &entity.name,
                    &route_param_names,
                    &member_names_set,
                    diags,
                );
            }
        }
    }

    // V12
    let init_routes: Vec<&Route> = entity.routes.iter().filter(|r| r.is_init).collect();
    if init_routes.len() > 1 {
        diags.push(Diagnostic::error(
            "V12",
            format!(
                "Entity '{}' has {} init routes (at most one allowed)",
                entity.name,
                init_routes.len()
            ),
        ));
    }
    for route in &init_routes {
        if route.return_type.is_some() {
            diags.push(Diagnostic::error(
                "V12",
                format!(
                    "Init route '{}' in entity '{}' must not have a return type",
                    route.name, entity.name
                ),
            ));
        }
    }

    // V42: a route that returns a value must declare a return type. Without a
    // `-> T` the backends have no signature to emit the value against — on EVM
    // this produces `return <expr>;` inside a function with no `returns(...)`
    // clause (solc 8863). A bare `return` / `return()` (no value) is a valid
    // early exit and is exempt.
    for route in &entity.routes {
        if route.return_type.is_none()
            && route
                .body
                .all_actions()
                .iter()
                .any(|a| action_returns_value(a))
        {
            diags.push(Diagnostic::error("V42",
                format!("Route '{}' in entity '{}' returns a value (`return(...)`) but declares no return type; add a `-> T` return type",
                    route.name, entity.name)));
        }
    }
}

// ---------------------------------------------------------------------------
// V67: missing / mistyped return values
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ValueCategory {
    Numeric,
    String,
    Bool,
}

impl ValueCategory {
    fn name(self) -> &'static str {
        match self {
            ValueCategory::Numeric => "numeric",
            ValueCategory::String => "String",
            ValueCategory::Bool => "bool",
        }
    }
}

fn type_category(ty: &Type) -> Option<ValueCategory> {
    use crate::codegen::stdlib::{is_numeric_type, is_string_type};
    if is_numeric_type(ty) {
        Some(ValueCategory::Numeric)
    } else if is_string_type(ty) {
        Some(ValueCategory::String)
    } else if matches!(ty, Type::Simple(s) if s == "bool") {
        Some(ValueCategory::Bool)
    } else {
        None
    }
}

/// Category of `expr` when it is certain without scope information:
/// literals, program `pure fn` calls, and `std::` calls.
fn expr_value_category(expr: &Expr, pure_fns: &[PureFn]) -> Option<ValueCategory> {
    match expr {
        Expr::StringLiteral(_) => Some(ValueCategory::String),
        Expr::IntLiteral(_) => Some(ValueCategory::Numeric),
        Expr::BoolLiteral(_) => Some(ValueCategory::Bool),
        Expr::FnCall(name, _) => pure_fns
            .iter()
            .find(|f| f.name == *name)
            .and_then(|f| type_category(&f.return_type)),
        Expr::NamespacedCall {
            namespace, name, ..
        } if namespace.starts_with("std::") => {
            crate::codegen::stdlib::infer_std_call_return_type(namespace, name)
                .and_then(|t| type_category(&t))
        }
        _ => None,
    }
}

fn value_mismatch(declared: &Type, value: &Expr, pure_fns: &[PureFn]) -> Option<String> {
    let want = type_category(declared)?;
    let got = expr_value_category(value, pure_fns)?;
    (want != got).then(|| {
        format!(
            "value has {} type but `{}` is declared",
            got.name(),
            crate::pretty::fmt_type(declared)
        )
    })
}

fn collect_returns(action: &RouteAction, out: &mut Vec<Vec<Expr>>) {
    crate::analysis::walk::for_each_action(action, &mut |a| {
        if let RouteAction::Return { values } = a {
            if !values.is_empty() {
                out.push(values.clone());
            }
        }
    });
}

fn action_throws(action: &RouteAction) -> bool {
    let mut found = false;
    crate::analysis::walk::for_each_action(action, &mut |a| {
        if matches!(a, RouteAction::Throw { .. } | RouteAction::ThrowCustom { .. }) {
            found = true;
        }
    });
    found
}

pub(super) fn check_route_returns(entity: &Entity, program: &Program, diags: &mut Vec<Diagnostic>) {
    for route in &entity.routes {
        let Some(ret_ty) = &route.return_type else {
            continue;
        };
        let actions = route.body.all_actions();
        let mut returns = Vec::new();
        for a in &actions {
            collect_returns(a, &mut returns);
        }
        if returns.is_empty() {
            if !actions.iter().any(|a| action_throws(a)) {
                diags.push(Diagnostic::error(
                    "V67",
                    format!(
                        "Route '{}' in entity '{}' declares return type `{}` but never returns a value; add `return(...)`",
                        route.name,
                        entity.name,
                        crate::pretty::fmt_type(ret_ty)
                    ),
                ));
            }
            continue;
        }
        let slots: Vec<&Type> = match ret_ty {
            Type::Tuple(items) => items.iter().collect(),
            other => vec![other],
        };
        for values in returns {
            if values.len() != slots.len() {
                continue;
            }
            for (ty, value) in slots.iter().zip(&values) {
                if let Some(why) = value_mismatch(ty, value, &program.pure_fns) {
                    diags.push(Diagnostic::error(
                        "V67",
                        format!(
                            "Route '{}' in entity '{}' returns a mistyped value: {}",
                            route.name, entity.name, why
                        ),
                    ));
                }
            }
        }
    }
}

pub(super) fn check_pure_fn_return(pure_fn: &PureFn, program: &Program, diags: &mut Vec<Diagnostic>) {
    let mut tail = &pure_fn.body;
    loop {
        match tail {
            Expr::Let(_, _, body) => tail = body,
            Expr::Block(items) => match items.last() {
                Some(last) => tail = last,
                None => return,
            },
            _ => break,
        }
    }
    if let Some(why) = value_mismatch(&pure_fn.return_type, tail, &program.pure_fns) {
        diags.push(Diagnostic::error(
            "V67",
            format!("pure fn '{}' returns a mistyped value: {}", pure_fn.name, why),
        ));
    }
}

// ---------------------------------------------------------------------------
// V60: Multiple `from` clauses must share the same `: throw` annotation
// ---------------------------------------------------------------------------

/// When a route lists more than one `from` branch, every branch must
/// agree on the failure surface (`: throw N` or `: throw CustomErr(...)`).
/// Mixed numeric codes or mixing named vs numeric throws are rejected
/// (T-ARCH-025 / W2-BC-07b).
pub(super) fn check_from_clause_uniform_throw(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    for route in &entity.routes {
        if route.from_clauses.len() < 2 {
            continue;
        }
        let mut keys: Vec<FromThrowKey> = route
            .from_clauses
            .iter()
            .map(from_throw_key)
            .collect();
        let first = &keys[0];
        if keys.iter().all(|k| k == first) {
            continue;
        }
        diags.push(Diagnostic::error(
            "V60",
            format!(
                "entity '{}' route '{}': multiple `from` clauses must share the same `: throw` annotation (found mixed throw codes)",
                entity.name, route.name
            ),
        ));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FromThrowKey {
    Default,
    Numeric(u32),
    Named(String),
}

// ---------------------------------------------------------------------------
// V63 / V64: `#[factory_only]` on init routes (BUG-U4 owner spec #5)
// ---------------------------------------------------------------------------

/// Under `deterministic_addresses`, the init/constructor route must carry
/// `#[factory_only]` so authors explicitly acknowledge factory-guarded deploy.
pub(super) fn check_factory_only_required_on_init(
    entity: &Entity,
    diags: &mut Vec<Diagnostic>,
) {
    let init_route = entity
        .routes
        .iter()
        .find(|r| crate::ast::is_init_route(r));
    if let Some(route) = init_route {
        if !route.factory_only {
            diags.push(
                Diagnostic::error(
                    "V63",
                    format!(
                        "entity '{}' route '{}': `#[factory_only]` is required on the init/constructor route when `deterministic_addresses: true`",
                        entity.name,
                        route.name
                    ),
                )
                .with_span(route.span),
            );
        }
    }
}

/// `#[factory_only]` is only meaningful on init/constructor routes.
pub(super) fn check_factory_only_placement(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    for route in &entity.routes {
        if route.factory_only && !crate::ast::is_init_route(route) {
            diags.push(
                Diagnostic::error(
                    "V64",
                    format!(
                        "entity '{}' route '{}': `#[factory_only]` is only allowed on the init/constructor route",
                        entity.name,
                        route.name
                    ),
                )
                .with_span(route.span),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// V62: `msg::sender` in init route under factory deploy (deterministic mode)
// ---------------------------------------------------------------------------

/// Under `deterministic_addresses`, CREATE2 construction runs with
/// `msg.sender == factory`. Init-route logic must not treat `msg::sender`
/// as the external deploy caller — use explicit constructor parameters
/// (BUG-U4 / SMAFD BC-RHT-001).
pub(super) fn check_init_route_msg_sender_under_factory(
    entity: &Entity,
    diags: &mut Vec<Diagnostic>,
) {
    let init_route = entity
        .routes
        .iter()
        .find(|r| r.is_init || r.name == "constructor");
    let init_name = init_route.map(|r| r.name.as_str());

    if let Some(route) = init_route {
        for action in route.body.all_actions() {
            if route_action_references_msg_sender(action) {
                push_v62_init_msg_sender(entity, route, diags);
                return;
            }
        }
    }

    if let Some(name) = init_name {
        for member in &entity.members {
            for transform in &member.transforms {
                if transform.route_name != name {
                    continue;
                }
                if expr_references_msg_sender(&transform.body) {
                    push_v62_init_msg_sender(
                        entity,
                        init_route.expect("init route name without route"),
                        diags,
                    );
                    return;
                }
            }
        }
    }
}

fn push_v62_init_msg_sender(entity: &Entity, route: &Route, diags: &mut Vec<Diagnostic>) {
    diags.push(
        Diagnostic::error(
            "V62",
            format!(
                "entity '{}' route '{}': `msg::sender` in the init/constructor route is the factory address under `deterministic_addresses` (CREATE2), not the external deploy caller — declare an explicit constructor parameter and pass it via `factory.deploy*(...)`",
                entity.name,
                route.name
            ),
        )
        .with_span(route.span),
    );
}

fn expr_references_msg_sender(expr: &Expr) -> bool {
    let mut found = false;
    walk_expr_subtree(expr, &mut |e| {
        if matches!(e, Expr::MsgField(f) if f == "sender") {
            found = true;
        }
    });
    found
}

fn walk_expr_subtree(expr: &Expr, f: &mut impl FnMut(&Expr)) {
    f(expr);
    match expr {
        Expr::BinOp(l, _, r) => {
            walk_expr_subtree(l, f);
            walk_expr_subtree(r, f);
        }
        Expr::UnaryOp(_, inner) => walk_expr_subtree(inner, f),
        Expr::FieldAccess(inner, _) => walk_expr_subtree(inner, f),
        Expr::Index(arr, idx) => {
            walk_expr_subtree(arr, f);
            walk_expr_subtree(idx, f);
        }
        Expr::FnCall(_, args) => {
            for a in args {
                walk_expr_subtree(a, f);
            }
        }
        Expr::MethodCall(recv, _, args) => {
            walk_expr_subtree(recv, f);
            for a in args {
                walk_expr_subtree(a, f);
            }
        }
        Expr::If(c, t, e) => {
            walk_expr_subtree(c, f);
            walk_expr_subtree(t, f);
            if let Some(e) = e {
                walk_expr_subtree(e, f);
            }
        }
        Expr::Let(_, value, body) => {
            walk_expr_subtree(value, f);
            walk_expr_subtree(body, f);
        }
        Expr::Range(a, b) => {
            walk_expr_subtree(a, f);
            walk_expr_subtree(b, f);
        }
        Expr::Cast(inner, _) => walk_expr_subtree(inner, f),
        Expr::Tuple(items) | Expr::ArrayLit(items) => {
            for it in items {
                walk_expr_subtree(it, f);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                walk_expr_subtree(v, f);
            }
        }
        Expr::RecordUpdate(base, fields) => {
            walk_expr_subtree(base, f);
            for (_, v) in fields {
                walk_expr_subtree(v, f);
            }
        }
        Expr::Block(stmts) => {
            for s in stmts {
                walk_expr_subtree(s, f);
            }
        }
        Expr::Match(subject, arms) => {
            walk_expr_subtree(subject, f);
            for arm in arms {
                walk_expr_subtree(&arm.body, f);
            }
        }
        Expr::Closure(_, body) => walk_expr_subtree(body, f),
        Expr::For(_, iter, body) => {
            walk_expr_subtree(iter, f);
            walk_expr_subtree(body, f);
        }
        Expr::Some(inner) => walk_expr_subtree(inner, f),
        Expr::EnumVariantWithData(_, _, args) | Expr::MacroRef(_, args) => {
            for a in args {
                walk_expr_subtree(a, f);
            }
        }
        Expr::NamespacedCall { args, .. } => {
            for a in args {
                walk_expr_subtree(a, f);
            }
        }
        Expr::AddressOf {
            args, with_params, ..
        } => {
            for a in args {
                walk_expr_subtree(a, f);
            }
            for (_, v) in with_params {
                walk_expr_subtree(v, f);
            }
        }
        Expr::Encode { value, .. } => walk_expr_subtree(value, f),
        _ => {}
    }
}

fn route_action_references_msg_sender(action: &RouteAction) -> bool {
    match action {
        RouteAction::Send {
            args,
            dest,
            send_options,
            ..
        }
        | RouteAction::VarCall {
            args,
            dest,
            send_options,
            ..
        } => {
            args.iter().any(expr_references_msg_sender)
                || expr_references_msg_sender(dest)
                || send_options
                    .as_ref()
                    .is_some_and(expr_references_msg_sender)
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            expr_references_msg_sender(condition)
                || then_actions
                    .iter()
                    .any(route_action_references_msg_sender)
                || else_actions
                    .iter()
                    .any(route_action_references_msg_sender)
        }
        RouteAction::Return { values } => values.iter().any(expr_references_msg_sender),
        RouteAction::Let { value, .. } => expr_references_msg_sender(value),
        RouteAction::Effect { args, .. } => args.iter().any(expr_references_msg_sender),
        RouteAction::Deploy {
            constructor_args,
            send_options,
            ..
        } => {
            constructor_args.iter().any(expr_references_msg_sender)
                || send_options
                    .as_ref()
                    .is_some_and(expr_references_msg_sender)
        }
        RouteAction::Rescue { action, .. } => route_action_references_msg_sender(action),
        RouteAction::ThrowCustom { args, .. } => args.iter().any(expr_references_msg_sender),
        RouteAction::Emit { args, .. } => args.iter().any(expr_references_msg_sender),
        RouteAction::CallRoute { args, .. } => args.iter().any(expr_references_msg_sender),
        RouteAction::UpdateCode { update_args, callback_args, .. } => {
            update_args.iter().any(expr_references_msg_sender)
                || callback_args.iter().any(expr_references_msg_sender)
        }
        RouteAction::For { iter, body, .. } => {
            expr_references_msg_sender(iter) || body.iter().any(route_action_references_msg_sender)
        }
        RouteAction::Throw { .. } => false,
    }
}

fn from_throw_key(fc: &FromClause) -> FromThrowKey {
    if let Some(name) = &fc.error_name {
        FromThrowKey::Named(name.clone())
    } else if let Some(code) = fc.error_code {
        FromThrowKey::Numeric(code)
    } else {
        FromThrowKey::Default
    }
}

// ---------------------------------------------------------------------------
// V43: Temporal `^member` only inside member transform bodies
// ---------------------------------------------------------------------------

/// LANGUAGE.md scopes `^member` to member transforms. Using it in
/// `where` / `from` / route actions / defaults would emit out-of-scope
/// `next_*` locals on EVM (Wave 4 deferred follow-up for T-EVM-EX-013).
pub(super) fn check_temporal_ref_placement(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    for route in &entity.routes {
        let route_ctx = format!("route '{}'", route.name);
        for wc in &route.where_clauses {
            reject_temporal_in_expr(&wc.condition, &format!("{} where clause", route_ctx), diags);
            for a in &wc.error_args {
                reject_temporal_in_expr(a, &format!("{} where error args", route_ctx), diags);
            }
        }
        for fc in &route.from_clauses {
            for a in &fc.args {
                reject_temporal_in_expr(a, &format!("{} from clause", route_ctx), diags);
            }
            if let Some(opts) = &fc.with_params {
                reject_temporal_in_expr(opts, &format!("{} from with", route_ctx), diags);
            }
            for a in &fc.error_args {
                reject_temporal_in_expr(a, &format!("{} from error args", route_ctx), diags);
            }
        }
        match &route.body {
            RouteBody::Phased(phases) | RouteBody::Mixed(phases, _) => {
                for phase in phases {
                    let phase_ctx = format!("{} phase '{}'", route_ctx, phase.name);
                    for wc in &phase.where_clauses {
                        reject_temporal_in_expr(
                            &wc.condition,
                            &format!("{} where clause", phase_ctx),
                            diags,
                        );
                        for a in &wc.error_args {
                            reject_temporal_in_expr(
                                a,
                                &format!("{} where error args", phase_ctx),
                                diags,
                            );
                        }
                    }
                }
            }
            RouteBody::Unphased(_) => {}
        }
        for action in route.body.all_actions() {
            reject_temporal_in_action(action, &route_ctx, diags);
        }
    }

    for member in &entity.members {
        if let Some(default) = &member.default_value {
            reject_temporal_in_expr(default, &format!("member '{}' default", member.name), diags);
        }
    }
}

fn reject_temporal_in_expr(expr: &Expr, ctx: &str, diags: &mut Vec<Diagnostic>) {
    match expr {
        Expr::TemporalRef(name) => {
            diags.push(Diagnostic::error(
                "V43",
                format!(
                    "Temporal reference '^{}' is only allowed inside a member transform body (found in {})",
                    name, ctx
                ),
            ));
        }
        Expr::BinOp(l, _, r) => {
            reject_temporal_in_expr(l, ctx, diags);
            reject_temporal_in_expr(r, ctx, diags);
        }
        Expr::UnaryOp(_, e)
        | Expr::FieldAccess(e, _)
        | Expr::Cast(e, _)
        | Expr::Some(e)
        | Expr::Closure(_, e)
        | Expr::Encode { value: e, .. } => reject_temporal_in_expr(e, ctx, diags),
        Expr::Index(e, idx) => {
            reject_temporal_in_expr(e, ctx, diags);
            reject_temporal_in_expr(idx, ctx, diags);
        }
        Expr::MethodCall(e, _, args) => {
            reject_temporal_in_expr(e, ctx, diags);
            for a in args {
                reject_temporal_in_expr(a, ctx, diags);
            }
        }
        Expr::FnCall(_, args)
        | Expr::MacroRef(_, args)
        | Expr::Tuple(args)
        | Expr::ArrayLit(args)
        | Expr::EnumVariantWithData(_, _, args) => {
            for a in args {
                reject_temporal_in_expr(a, ctx, diags);
            }
        }
        Expr::NamespacedCall { args, .. } => {
            for a in args {
                reject_temporal_in_expr(a, ctx, diags);
            }
        }
        Expr::If(c, t, e) => {
            reject_temporal_in_expr(c, ctx, diags);
            reject_temporal_in_expr(t, ctx, diags);
            if let Some(el) = e {
                reject_temporal_in_expr(el, ctx, diags);
            }
        }
        Expr::Let(_, val, body) => {
            reject_temporal_in_expr(val, ctx, diags);
            reject_temporal_in_expr(body, ctx, diags);
        }
        Expr::Block(items) => {
            for e in items {
                reject_temporal_in_expr(e, ctx, diags);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                reject_temporal_in_expr(v, ctx, diags);
            }
        }
        Expr::RecordUpdate(base, fields) => {
            reject_temporal_in_expr(base, ctx, diags);
            for (_, v) in fields {
                reject_temporal_in_expr(v, ctx, diags);
            }
        }
        Expr::Match(subj, arms) => {
            reject_temporal_in_expr(subj, ctx, diags);
            for arm in arms {
                reject_temporal_in_expr(&arm.body, ctx, diags);
            }
        }
        Expr::Range(s, e) => {
            reject_temporal_in_expr(s, ctx, diags);
            reject_temporal_in_expr(e, ctx, diags);
        }
        Expr::For(_, iter, body) => {
            reject_temporal_in_expr(iter, ctx, diags);
            reject_temporal_in_expr(body, ctx, diags);
        }
        Expr::AddressOf {
            args, with_params, ..
        } => {
            for a in args {
                reject_temporal_in_expr(a, ctx, diags);
            }
            for (_, v) in with_params {
                reject_temporal_in_expr(v, ctx, diags);
            }
        }
        _ => {}
    }
}

fn reject_temporal_in_action(action: &RouteAction, route_ctx: &str, diags: &mut Vec<Diagnostic>) {
    match action {
        RouteAction::Let { value, .. } => {
            reject_temporal_in_expr(value, &format!("{} let", route_ctx), diags);
        }
        RouteAction::Return { values } => {
            for v in values {
                reject_temporal_in_expr(v, &format!("{} return", route_ctx), diags);
            }
        }
        RouteAction::Send {
            args,
            dest,
            send_options,
            ..
        } => {
            for a in args {
                reject_temporal_in_expr(a, &format!("{} send", route_ctx), diags);
            }
            reject_temporal_in_expr(dest, &format!("{} send dest", route_ctx), diags);
            if let Some(opts) = send_options {
                reject_temporal_in_expr(opts, &format!("{} send options", route_ctx), diags);
            }
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            reject_temporal_in_expr(condition, &format!("{} if", route_ctx), diags);
            for a in then_actions {
                reject_temporal_in_action(a, route_ctx, diags);
            }
            for a in else_actions {
                reject_temporal_in_action(a, route_ctx, diags);
            }
        }
        RouteAction::Effect { args, .. } => {
            for a in args {
                reject_temporal_in_expr(a, &format!("{} effect", route_ctx), diags);
            }
        }
        RouteAction::Deploy {
            constructor_args,
            send_options,
            ..
        } => {
            for a in constructor_args {
                reject_temporal_in_expr(a, &format!("{} deploy", route_ctx), diags);
            }
            if let Some(opts) = send_options {
                reject_temporal_in_expr(opts, &format!("{} deploy options", route_ctx), diags);
            }
        }
        RouteAction::Rescue { action, .. } => {
            reject_temporal_in_action(action, route_ctx, diags);
        }
        RouteAction::Throw { .. } => {}
        RouteAction::ThrowCustom { args, .. } => {
            for a in args {
                reject_temporal_in_expr(a, &format!("{} throw", route_ctx), diags);
            }
        }
        RouteAction::CallRoute { args, .. } => {
            for a in args {
                reject_temporal_in_expr(a, &format!("{} call", route_ctx), diags);
            }
        }
        RouteAction::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            for a in update_args {
                reject_temporal_in_expr(a, &format!("{} updateCode", route_ctx), diags);
            }
            for a in callback_args {
                reject_temporal_in_expr(a, &format!("{} updateCode", route_ctx), diags);
            }
        }
        RouteAction::VarCall {
            args,
            dest,
            send_options,
            ..
        } => {
            for a in args {
                reject_temporal_in_expr(a, &format!("{} var call", route_ctx), diags);
            }
            reject_temporal_in_expr(dest, &format!("{} var call dest", route_ctx), diags);
            if let Some(opts) = send_options {
                reject_temporal_in_expr(opts, &format!("{} var call options", route_ctx), diags);
            }
        }
        RouteAction::For { iter, body, .. } => {
            reject_temporal_in_expr(iter, &format!("{} for", route_ctx), diags);
            for a in body {
                reject_temporal_in_action(a, route_ctx, diags);
            }
        }
        RouteAction::Emit { args, .. } => {
            for a in args {
                reject_temporal_in_expr(a, &format!("{} emit", route_ctx), diags);
            }
        }
    }
}

/// V9: first route reachable from `route` through `call` chains that has
/// member transforms or state-changing actions (send / deploy / emit /
/// platform effect / code update).
fn find_mutating_call_target(
    entity: &Entity,
    route: &Route,
    transformed_routes: &HashSet<&str>,
) -> Option<String> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = call_targets(route);
    while let Some(name) = stack.pop() {
        if !visited.insert(name.clone()) {
            continue;
        }
        let Some(callee) = entity.routes.iter().find(|r| r.name == name) else {
            continue;
        };
        if transformed_routes.contains(callee.name.as_str()) || route_has_state_changing_action(callee) {
            return Some(callee.name.clone());
        }
        stack.extend(call_targets(callee));
    }
    None
}

fn call_targets(route: &Route) -> Vec<String> {
    let mut out = Vec::new();
    for action in route.body.all_actions() {
        crate::analysis::walk::for_each_action(action, &mut |a| {
            if let RouteAction::CallRoute { name, .. } = a {
                out.push(name.clone());
            }
        });
    }
    out
}

/// E29: actions Solidity rejects in a `view` function. Lean threads the
/// world through views, so this is Solidity-only. A capturing
/// `var x = f() ~> d` stays allowed: it reads another contract.
pub(super) fn check_view_effects_solidity(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    for route in entity.routes.iter().filter(|r| r.is_view) {
        if let Some(kind) = first_view_forbidden_action(route) {
            diags.push(Diagnostic::error(
                "E29",
                format!(
                    "View route '{}' in entity '{}' contains {}, which Solidity rejects in a `view` function; drop `view` or move the effect to a non-view route",
                    route.name, entity.name, kind
                ),
            ));
        }
    }
}

fn first_view_forbidden_action(route: &Route) -> Option<&'static str> {
    let mut found = None;
    for action in route.body.all_actions() {
        crate::analysis::walk::for_each_action(action, &mut |a| {
            if found.is_some() {
                return;
            }
            found = match a {
                RouteAction::Send { message: Some(_), .. } => Some("a message send"),
                RouteAction::Send { .. } => Some("a value transfer"),
                RouteAction::Deploy { .. } => Some("a `deploy`"),
                RouteAction::Emit { .. } => Some("an `emit`"),
                RouteAction::Effect { .. } | RouteAction::UpdateCode { .. } => {
                    Some("a platform effect")
                }
                _ => None,
            };
        });
    }
    found
}

fn route_has_state_changing_action(route: &Route) -> bool {
    let mut found = false;
    for action in route.body.all_actions() {
        crate::analysis::walk::for_each_action(action, &mut |a| {
            if matches!(
                a,
                RouteAction::Send { .. }
                    | RouteAction::VarCall { .. }
                    | RouteAction::Deploy { .. }
                    | RouteAction::Emit { .. }
                    | RouteAction::Effect { .. }
                    | RouteAction::UpdateCode { .. }
            ) {
                found = true;
            }
        });
    }
    found
}

/// True when `action` (or any action nested in its `if` / `for` bodies) is a
/// `return(<value>)` with at least one returned expression. Used by V42.
fn action_returns_value(action: &RouteAction) -> bool {
    match action {
        RouteAction::Return { values } => !values.is_empty(),
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            then_actions.iter().any(action_returns_value)
                || else_actions.iter().any(action_returns_value)
        }
        RouteAction::For { body, .. } => body.iter().any(action_returns_value),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// V11: Pure route purity — no msg::, members, macros, temporals
// ---------------------------------------------------------------------------

fn check_pure_route_action(
    action: &RouteAction,
    route_name: &str,
    entity_name: &str,
    route_params: &HashSet<&str>,
    member_names: &HashSet<&str>,
    diags: &mut Vec<Diagnostic>,
) {
    match action {
        RouteAction::Return { values } => {
            for v in values {
                check_pure_route_expr(
                    v,
                    route_name,
                    entity_name,
                    route_params,
                    member_names,
                    diags,
                );
            }
        }
        RouteAction::Send {
            message,
            args,
            dest,
            send_options,
        } => {
            let msg_name = message.as_deref().unwrap_or("(transfer)");
            diags.push(Diagnostic::error(
                "V11",
                format!(
                    "Pure route '{}' in entity '{}' contains send action '{}' (impure)",
                    route_name, entity_name, msg_name
                ),
            ));
            for a in args {
                check_pure_route_expr(
                    a,
                    route_name,
                    entity_name,
                    route_params,
                    member_names,
                    diags,
                );
            }
            check_pure_route_expr(
                dest,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
            if let Some(opts) = send_options {
                check_pure_route_expr(
                    opts,
                    route_name,
                    entity_name,
                    route_params,
                    member_names,
                    diags,
                );
            }
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            check_pure_route_expr(
                condition,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
            for a in then_actions {
                check_pure_route_action(
                    a,
                    route_name,
                    entity_name,
                    route_params,
                    member_names,
                    diags,
                );
            }
            for a in else_actions {
                check_pure_route_action(
                    a,
                    route_name,
                    entity_name,
                    route_params,
                    member_names,
                    diags,
                );
            }
        }
        RouteAction::Let { value, .. } => {
            check_pure_route_expr(
                value,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
        }
        RouteAction::Effect {
            namespace, name, ..
        } => {
            diags.push(Diagnostic::error(
                "V11",
                format!(
                    "pure route '{}' in entity '{}' cannot use effect '{}::{}'",
                    route_name, entity_name, namespace, name
                ),
            ));
        }
        RouteAction::Deploy { entity, .. } => {
            diags.push(Diagnostic::error(
                "V11",
                format!(
                    "pure route '{}' in entity '{}' cannot deploy '{}'",
                    route_name, entity_name, entity
                ),
            ));
        }
        RouteAction::Rescue { tag, action } => {
            diags.push(Diagnostic::error(
                "V11",
                format!(
                    "pure route '{}' in entity '{}' cannot use rescue '{}'",
                    route_name, entity_name, tag
                ),
            ));
            check_pure_route_action(
                action,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
        }
        RouteAction::Throw { .. } => {}
        RouteAction::ThrowCustom { args, .. } => {
            for a in args {
                check_pure_route_expr(
                    a,
                    route_name,
                    entity_name,
                    route_params,
                    member_names,
                    diags,
                );
            }
        }
        RouteAction::CallRoute { name, .. } => {
            diags.push(Diagnostic::error(
                "V11",
                format!(
                    "pure route '{}' in entity '{}' cannot call private route '{}'",
                    route_name, entity_name, name
                ),
            ));
        }
        RouteAction::UpdateCode { .. } => {
            diags.push(Diagnostic::error(
                "V11",
                format!(
                    "pure route '{}' in entity '{}' cannot use updateCode",
                    route_name, entity_name
                ),
            ));
        }
        RouteAction::VarCall { message, .. } => {
            diags.push(Diagnostic::error(
                "V11",
                format!(
                    "pure route '{}' in entity '{}' cannot use var call '{}'",
                    route_name, entity_name, message
                ),
            ));
        }
        RouteAction::For { iter, body, .. } => {
            check_pure_route_expr(
                iter,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
            for a in body {
                check_pure_route_action(
                    a,
                    route_name,
                    entity_name,
                    route_params,
                    member_names,
                    diags,
                );
            }
        }
        RouteAction::Emit { event_name, .. } => {
            diags.push(Diagnostic::error(
                "V11",
                format!(
                    "pure route '{}' in entity '{}' cannot emit event '{}'",
                    route_name, entity_name, event_name
                ),
            ));
        }
    }
}

fn check_pure_route_expr(
    expr: &Expr,
    route_name: &str,
    entity_name: &str,
    route_params: &HashSet<&str>,
    member_names: &HashSet<&str>,
    diags: &mut Vec<Diagnostic>,
) {
    match expr {
        Expr::MsgField(field) => {
            diags.push(Diagnostic::error(
                "V11",
                format!(
                    "Pure route '{}' in entity '{}' references msg::{} (impure)",
                    route_name, entity_name, field
                ),
            ));
        }
        Expr::SysField(field) => {
            diags.push(Diagnostic::error(
                "V11",
                format!(
                    "Pure route '{}' in entity '{}' references sys::{} (impure)",
                    route_name, entity_name, field
                ),
            ));
        }
        Expr::TemporalRef(name) => {
            diags.push(Diagnostic::error(
                "V11",
                format!(
                    "Pure route '{}' in entity '{}' references temporal ^{} (impure)",
                    route_name, entity_name, name
                ),
            ));
        }
        Expr::MacroRef(name, args) => {
            diags.push(Diagnostic::error(
                "V11",
                format!(
                    "Pure route '{}' in entity '{}' references macro @{} (macros may access state)",
                    route_name, entity_name, name
                ),
            ));
            for a in args {
                check_pure_route_expr(
                    a,
                    route_name,
                    entity_name,
                    route_params,
                    member_names,
                    diags,
                );
            }
        }
        Expr::Ident(name) => {
            if member_names.contains(name.as_str()) && !route_params.contains(name.as_str()) {
                diags.push(Diagnostic::error(
                    "V11",
                    format!(
                        "Pure route '{}' in entity '{}' references member '{}' (impure)",
                        route_name, entity_name, name
                    ),
                ));
            }
        }
        Expr::BinOp(l, _, r) => {
            check_pure_route_expr(
                l,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
            check_pure_route_expr(
                r,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
        }
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) => {
            check_pure_route_expr(
                e,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
        }
        Expr::Index(e, idx) => {
            check_pure_route_expr(
                e,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
            check_pure_route_expr(
                idx,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
        }
        Expr::MethodCall(e, _, args) => {
            check_pure_route_expr(
                e,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
            for a in args {
                check_pure_route_expr(
                    a,
                    route_name,
                    entity_name,
                    route_params,
                    member_names,
                    diags,
                );
            }
        }
        Expr::FnCall(_, args) => {
            for a in args {
                check_pure_route_expr(
                    a,
                    route_name,
                    entity_name,
                    route_params,
                    member_names,
                    diags,
                );
            }
        }
        Expr::If(cond, then_e, else_e) => {
            check_pure_route_expr(
                cond,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
            check_pure_route_expr(
                then_e,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
            if let Some(el) = else_e {
                check_pure_route_expr(
                    el,
                    route_name,
                    entity_name,
                    route_params,
                    member_names,
                    diags,
                );
            }
        }
        Expr::Let(pat, val, body) => {
            check_pure_route_expr(
                val,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
            let mut extended = route_params.clone();
            collect_pattern_names(pat, &mut extended);
            check_pure_route_expr(
                body,
                route_name,
                entity_name,
                &extended,
                member_names,
                diags,
            );
        }
        Expr::Block(stmts) => {
            for s in stmts {
                check_pure_route_expr(
                    s,
                    route_name,
                    entity_name,
                    route_params,
                    member_names,
                    diags,
                );
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                check_pure_route_expr(
                    v,
                    route_name,
                    entity_name,
                    route_params,
                    member_names,
                    diags,
                );
            }
        }
        Expr::RecordUpdate(base, fields) => {
            check_pure_route_expr(
                base,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
            for (_, v) in fields {
                check_pure_route_expr(
                    v,
                    route_name,
                    entity_name,
                    route_params,
                    member_names,
                    diags,
                );
            }
        }
        Expr::Closure(params, body) => {
            let mut extended = route_params.clone();
            for p in params {
                collect_pattern_names(p, &mut extended);
            }
            check_pure_route_expr(
                body,
                route_name,
                entity_name,
                &extended,
                member_names,
                diags,
            );
        }
        Expr::Tuple(elems) | Expr::ArrayLit(elems) => {
            for e in elems {
                check_pure_route_expr(
                    e,
                    route_name,
                    entity_name,
                    route_params,
                    member_names,
                    diags,
                );
            }
        }
        Expr::Match(subject, arms) => {
            check_pure_route_expr(
                subject,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
            for arm in arms {
                let mut extended = route_params.clone();
                collect_match_pattern_names(&arm.pattern, &mut extended);
                check_pure_route_expr(
                    &arm.body,
                    route_name,
                    entity_name,
                    &extended,
                    member_names,
                    diags,
                );
            }
        }
        Expr::Some(inner) => {
            check_pure_route_expr(
                inner,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
        }
        Expr::EnumVariantWithData(_, _, args) => {
            for a in args {
                check_pure_route_expr(
                    a,
                    route_name,
                    entity_name,
                    route_params,
                    member_names,
                    diags,
                );
            }
        }
        Expr::NamespacedCall { args, .. } => {
            for a in args {
                check_pure_route_expr(
                    a,
                    route_name,
                    entity_name,
                    route_params,
                    member_names,
                    diags,
                );
            }
        }
        Expr::Range(start, end) => {
            check_pure_route_expr(
                start,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
            check_pure_route_expr(
                end,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
        }
        Expr::For(pat, iter, body) => {
            check_pure_route_expr(
                iter,
                route_name,
                entity_name,
                route_params,
                member_names,
                diags,
            );
            let mut extended = route_params.clone();
            collect_pattern_names(pat, &mut extended);
            check_pure_route_expr(
                body,
                route_name,
                entity_name,
                &extended,
                member_names,
                diags,
            );
        }
        Expr::TraceField(_) | Expr::TraceCall { .. } => {
            diags.push(Diagnostic::error("I16",
                format!("route '{}' in entity '{}': trace:: accessors are only valid inside invariant 'assume'/'check' expressions",
                    route_name, entity_name)));
        }
        Expr::IntLiteral(_)
       
       
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::BoolLiteral(_)
        | Expr::EmptyCollection
        | Expr::EnumVariant(_, _)
        | Expr::None
        | Expr::AddressOf { .. }
        | Expr::Encode { .. } => {}
    }
}

// ---------------------------------------------------------------------------
// V4: Pure function purity — no entity state references
// ---------------------------------------------------------------------------

/// Entity member names a `pure fn` could read by mistake: every member of
/// every entity that is not also a constant (constants stay readable).
pub(super) fn pure_fn_state_names(program: &Program) -> HashSet<&str> {
    let consts: HashSet<&str> = program
        .libraries
        .iter()
        .flat_map(|l| l.constants.iter().map(|c| c.name.as_str()))
        .chain(program.entities.iter().flat_map(|e| e.constants.iter().map(|c| c.name.as_str())))
        .collect();
    program
        .entities
        .iter()
        .flat_map(|e| e.members.iter().map(|m| m.name.as_str()))
        .filter(|n| !consts.contains(n))
        .collect()
}

pub(super) fn check_pure_fn_purity(
    pure_fn: &PureFn,
    state: &HashSet<&str>,
    diags: &mut Vec<Diagnostic>,
) {
    let param_names: HashSet<&str> = pure_fn.params.iter().map(|p| p.name.as_str()).collect();

    check_purity_expr(&pure_fn.body, &pure_fn.name, state, &param_names, diags);
}

fn check_purity_expr(
    expr: &Expr,
    fn_name: &str,
    state: &HashSet<&str>,
    allowed_idents: &HashSet<&str>,
    diags: &mut Vec<Diagnostic>,
) {
    match expr {
        Expr::Ident(name) if !allowed_idents.contains(name.as_str()) && state.contains(name.as_str()) => {
            diags.push(Diagnostic::error(
                "V4",
                format!(
                    "Pure function '{}' references entity member '{}' (impure); pass it as a parameter",
                    fn_name, name
                ),
            ));
        }
        Expr::MsgField(field) => {
            diags.push(Diagnostic::error(
                "V4",
                format!(
                    "Pure function '{}' references msg::{} (impure)",
                    fn_name, field
                ),
            ));
        }
        Expr::SysField(field) => {
            diags.push(Diagnostic::error(
                "V4",
                format!(
                    "Pure function '{}' references sys::{} (impure)",
                    fn_name, field
                ),
            ));
        }
        Expr::TemporalRef(name) => {
            diags.push(Diagnostic::error(
                "V4",
                format!(
                    "Pure function '{}' references temporal ^{} (impure)",
                    fn_name, name
                ),
            ));
        }
        Expr::MacroRef(name, args) => {
            diags.push(Diagnostic::error(
                "V4",
                format!(
                    "Pure function '{}' references macro @{} (macros may access state)",
                    fn_name, name
                ),
            ));
            for a in args {
                check_purity_expr(a, fn_name, state, allowed_idents, diags);
            }
        }
        Expr::BinOp(l, _, r) => {
            check_purity_expr(l, fn_name, state, allowed_idents, diags);
            check_purity_expr(r, fn_name, state, allowed_idents, diags);
        }
        Expr::UnaryOp(_, e) => check_purity_expr(e, fn_name, state, allowed_idents, diags),
        Expr::FieldAccess(e, _) => check_purity_expr(e, fn_name, state, allowed_idents, diags),
        Expr::Index(e, idx) => {
            check_purity_expr(e, fn_name, state, allowed_idents, diags);
            check_purity_expr(idx, fn_name, state, allowed_idents, diags);
        }
        Expr::MethodCall(e, _, args) => {
            check_purity_expr(e, fn_name, state, allowed_idents, diags);
            for a in args {
                check_purity_expr(a, fn_name, state, allowed_idents, diags);
            }
        }
        Expr::FnCall(_, args) => {
            for a in args {
                check_purity_expr(a, fn_name, state, allowed_idents, diags);
            }
        }
        Expr::If(cond, then_e, else_e) => {
            check_purity_expr(cond, fn_name, state, allowed_idents, diags);
            check_purity_expr(then_e, fn_name, state, allowed_idents, diags);
            if let Some(el) = else_e {
                check_purity_expr(el, fn_name, state, allowed_idents, diags);
            }
        }
        Expr::Let(pat, val, body) => {
            check_purity_expr(val, fn_name, state, allowed_idents, diags);
            let mut extended = allowed_idents.clone();
            collect_pattern_names(pat, &mut extended);
            check_purity_expr(body, fn_name, state, &extended, diags);
        }
        Expr::Block(stmts) => {
            for s in stmts {
                check_purity_expr(s, fn_name, state, allowed_idents, diags);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                check_purity_expr(v, fn_name, state, allowed_idents, diags);
            }
        }
        Expr::RecordUpdate(base, fields) => {
            check_purity_expr(base, fn_name, state, allowed_idents, diags);
            for (_, v) in fields {
                check_purity_expr(v, fn_name, state, allowed_idents, diags);
            }
        }
        Expr::Closure(params, body) => {
            let mut extended = allowed_idents.clone();
            for p in params {
                collect_pattern_names(p, &mut extended);
            }
            check_purity_expr(body, fn_name, state, &extended, diags);
        }
        Expr::Cast(e, _) => check_purity_expr(e, fn_name, state, allowed_idents, diags),
        Expr::Tuple(elems) => {
            for e in elems {
                check_purity_expr(e, fn_name, state, allowed_idents, diags);
            }
        }
        Expr::Match(subject, arms) => {
            check_purity_expr(subject, fn_name, state, allowed_idents, diags);
            for arm in arms {
                let mut extended = allowed_idents.clone();
                collect_match_pattern_names(&arm.pattern, &mut extended);
                check_purity_expr(&arm.body, fn_name, state, &extended, diags);
            }
        }
        Expr::Some(inner) => check_purity_expr(inner, fn_name, state, allowed_idents, diags),
        Expr::ArrayLit(elems) => {
            for e in elems {
                check_purity_expr(e, fn_name, state, allowed_idents, diags);
            }
        }
        Expr::EnumVariantWithData(enum_name, variant, args) => {
            // `evm::foo(args)` parses as `EnumVariantWithData("evm",
            // "foo", args)` (the LALRPOP grammar reuses the
            // `<Ident>::<Ident>(args)` rule for both enum variants and
            // EVM-namespace calls). Apply the same pure-intrinsic
            // allowlist as the `NamespacedCall` arm so that
            // `evm::balance` / `evm::blockhash` inside a `pure fn` are
            // flagged with V4 while `evm::ecrecover`,
            // `evm::keccak256Packed`, `evm::sha256`, and `evm::ripemd160`
            // remain pure.
            if enum_name == "evm" {
                let is_pure_intrinsic = matches!(
                    variant.as_str(),
                    "ecrecover" | "keccak256Packed" | "sha256" | "ripemd160"
                );
                if !is_pure_intrinsic {
                    diags.push(Diagnostic::error(
                        "V4",
                        format!(
                            "Pure function '{}' references evm::{}(...) (impure)",
                            fn_name, variant
                        ),
                    ));
                }
            }
            for a in args {
                check_purity_expr(a, fn_name, state, allowed_idents, diags);
            }
        }
        Expr::NamespacedCall {
            namespace,
            name,
            args,
            ..
        } => {
            if namespace == "evm" {
                // `evm::ecrecover`, `evm::keccak256Packed`, `evm::sha256`,
                // and `evm::ripemd160` are pure EVM-target intrinsics
                // (deterministic, no state read or write — they map onto
                // EVM precompiles 0x01-0x03 plus the Solidity hash globals).
                // `evm::balance` and `evm::blockhash` read live state /
                // block context and are therefore impure.
                let is_pure_intrinsic = matches!(
                    name.as_str(),
                    "ecrecover" | "keccak256Packed" | "sha256" | "ripemd160"
                );
                if !is_pure_intrinsic {
                    diags.push(Diagnostic::error(
                        "V4",
                        format!(
                            "Pure function '{}' references evm::{}(...) (impure)",
                            fn_name, name
                        ),
                    ));
                }
            }
            for a in args {
                check_purity_expr(a, fn_name, state, allowed_idents, diags);
            }
        }
        Expr::Range(start, end) => {
            check_purity_expr(start, fn_name, state, allowed_idents, diags);
            check_purity_expr(end, fn_name, state, allowed_idents, diags);
        }
        Expr::For(pat, iter, body) => {
            check_purity_expr(iter, fn_name, state, allowed_idents, diags);
            let mut extended = allowed_idents.clone();
            collect_pattern_names(pat, &mut extended);
            check_purity_expr(body, fn_name, state, &extended, diags);
        }
        Expr::TraceField(_) | Expr::TraceCall { .. } => {
            diags.push(Diagnostic::error("I16",
                format!("Pure function '{}': trace:: accessors are only valid inside invariant 'assume'/'check' expressions", fn_name)));
        }
        Expr::IntLiteral(_)
       
       
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::BoolLiteral(_)
        | Expr::EmptyCollection
        | Expr::Ident(_)
        | Expr::EnumVariant(_, _)
        | Expr::None
        | Expr::AddressOf { .. }
        | Expr::Encode { .. } => {}
    }
}

// ---------------------------------------------------------------------------
// V7: Constant values are valid literal expressions
// ---------------------------------------------------------------------------

pub(super) fn check_constant_values(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    for constant in &entity.constants {
        if !is_const_expr(&constant.value) {
            diags.push(Diagnostic::error(
                "V7",
                format!(
                    "Constant '{}' in entity '{}' has non-literal value",
                    constant.name, entity.name
                ),
            ));
        }
    }
}

fn is_const_expr(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::IntLiteral(_)
           
           
            | Expr::StringLiteral(_)
            | Expr::BytesLiteral(_)
            | Expr::BoolLiteral(_)
    )
}

// ---------------------------------------------------------------------------
// V49: Bare stdlib calls (docs/STDLIB.md)
// ---------------------------------------------------------------------------

/// Function names that must be spelled `std::math::…`, `std::str::…`, or `std::crypto::…`.
pub(super) fn bare_stdlib_fn_names() -> HashSet<&'static str> {
    [
        "min",
        "max",
        "abs",
        "clamp",
        "muldiv",
        "muldivmod",
        "divmod",
        "divc",
        "divr",
        "sign",
        "minmax",
        "modpow2",
        "pow",
        "sha256",
    ]
    .into_iter()
    .collect()
}

pub(super) fn std_path_for_bare_fn(name: &str) -> String {
    if name == "sha256" {
        "std::crypto::sha256".to_string()
    } else {
        format!("std::math::{}", name)
    }
}

pub(super) fn check_bare_stdlib_call(
    name: &str,
    known_fns: &HashSet<&str>,
    ctx: &str,
    diags: &mut Vec<Diagnostic>,
) {
    if bare_stdlib_fn_names().contains(name) && !known_fns.contains(name) {
        diags.push(Diagnostic::error(
            "V49",
            format!(
                "Bare stdlib call '{}(...)' in {}. Use {} (see docs/STDLIB.md).",
                name,
                ctx,
                std_path_for_bare_fn(name),
            ),
        ));
    }
}

/// V49 for `pure fn` bodies (program scope and libraries).
pub(super) fn check_bare_stdlib_calls_pure_fns(program: &Program, diags: &mut Vec<Diagnostic>) {
    let mut known_fns: HashSet<&str> = program.pure_fns.iter().map(|f| f.name.as_str()).collect();
    for lib in &program.libraries {
        for pf in &lib.pure_fns {
            known_fns.insert(pf.name.as_str());
        }
    }
    let lib_fns = program
        .libraries
        .iter()
        .flat_map(|l| l.pure_fns.iter().map(move |pf| (Some(l.name.as_str()), pf)));
    let fns = program.pure_fns.iter().map(|pf| (None, pf)).chain(lib_fns);
    for (lib, pf) in fns {
        let ctx = match lib {
            Some(l) => format!("pure fn '{}::{}'", l, pf.name),
            None => format!("pure fn '{}'", pf.name),
        };
        let called = std::cell::RefCell::new(Vec::new());
        let _ = crate::codegen::solidity::core::types::subst_expr_where(&pf.body, &|e| {
            if let Expr::FnCall(name, _) = e {
                called.borrow_mut().push(name.clone());
            }
            None
        });
        for name in called.into_inner() {
            check_bare_stdlib_call(&name, &known_fns, &ctx, diags);
        }
    }
}

// ---------------------------------------------------------------------------
// V8: Undefined references — basic name resolution
// ---------------------------------------------------------------------------

pub(super) fn check_undefined_refs(
    entity: &Entity,
    pure_fns: &[PureFn],
    all_entities: &[Entity],
    libraries: &[LibraryDecl],
    program_aliases: &[TypeAlias],
    diags: &mut Vec<Diagnostic>,
) {
    let mut known_fns: HashSet<&str> = pure_fns.iter().map(|f| f.name.as_str()).collect();
    // Phase Library-3: every library's `pure fn` is callable from
    // entity bodies that have `using LibName for T;` in scope. The
    // method-call rewrite (codegen/using_rewrite) turns `recv.fn(args)`
    // into `Expr::FnCall("fn", ...)`, so V8 must accept those names.
    for lib in libraries {
        for pf in &lib.pure_fns {
            known_fns.insert(pf.name.as_str());
        }
    }

    let builtins: HashSet<&str> = [
        "len",
        "exists",
        "insert",
        "remove",
        "update",
        "keys",
        "iter",
        "filter",
        "map",
        "fold",
        "take",
        "collect",
        "enumerate",
        "contains",
        "is_empty",
        "addressOf",
        "hashOf",
        "stateInit",
    ]
    .into_iter()
    .collect();

    let entity_names: HashSet<&str> = all_entities.iter().map(|e| e.name.as_str()).collect();

    let member_names: HashSet<&str> = entity.members.iter().map(|m| m.name.as_str()).collect();

    let const_names: HashSet<&str> = entity.constants.iter().map(|c| c.name.as_str()).collect();

    let macro_names: HashSet<&str> = entity.macros.iter().map(|m| m.name.as_str()).collect();

    let record_names: HashSet<&str> = entity.records.iter().map(|r| r.name.as_str()).collect();

    let mut record_names_ext: HashSet<&str> = record_names.clone();
    record_names_ext.extend(entity_names.iter());
    let record_names = &record_names_ext;

    for mac in &entity.macros {
        check_macro_refs_expr(
            &mac.body,
            entity,
            &known_fns,
            &builtins,
            &member_names,
            &const_names,
            &macro_names,
            record_names,
            &mac.params,
            &format!("macro '{}'", mac.name),
            diags,
        );
    }

    for route in &entity.routes {
        let route_ctx = format!("route '{}'", route.name);
        let route_params: HashSet<&str> = route.params.iter().map(|p| p.name.as_str()).collect();

        for wc in &route.where_clauses {
            check_entity_expr_refs(
                &wc.condition,
                entity,
                &known_fns,
                &builtins,
                &member_names,
                &const_names,
                &macro_names,
                record_names,
                &route_params,
                &route_ctx,
                diags,
            );
        }

        let var_names = super::collect_var_defs_for_route(route);
        let mut action_scope: HashSet<&str> = route_params.iter().copied().collect();
        action_scope.extend(member_names.iter());
        action_scope.extend(const_names.iter());
        for vn in &var_names {
            action_scope.insert(vn.as_str());
        }
        check_route_actions_expr_refs(
            route,
            entity,
            &known_fns,
            &builtins,
            &member_names,
            &const_names,
            &macro_names,
            record_names,
            &action_scope,
            &route_ctx,
            diags,
        );
    }

    for member in &entity.members {
        for transform in &member.transforms {
            let ctx = format!(
                "member '{}' in route '{}'",
                member.name, transform.route_name
            );
            let var_names = entity
                .routes
                .iter()
                .find(|r| r.name == transform.route_name)
                .map(super::collect_var_defs_for_route)
                .unwrap_or_default();
            let mut scope: HashSet<&str> = member_names.iter().copied().collect();
            scope.extend(const_names.iter());
            for p in &transform.params {
                collect_pattern_names(p, &mut scope);
            }
            for vn in &var_names {
                scope.insert(vn.as_str());
            }
            check_entity_expr_refs(
                &transform.body,
                entity,
                &known_fns,
                &builtins,
                &member_names,
                &const_names,
                &macro_names,
                record_names,
                &scope,
                &ctx,
                diags,
            );
            if let Some(route) = entity
                .routes
                .iter()
                .find(|r| r.name == transform.route_name)
            {
                check_member_transform_type(
                    member,
                    transform,
                    entity,
                    route,
                    pure_fns,
                    program_aliases,
                    diags,
                );
            }
        }
    }
}

fn infer_transform_body_type(
    expr: &Expr,
    entity: &Entity,
    route: &Route,
    pure_fns: &[PureFn],
) -> Option<Type> {
    match expr {
        Expr::StringLiteral(_) => Some(Type::Simple("String".to_string())),
        Expr::IntLiteral(_) => Some(Type::Simple("u64".to_string())),
        Expr::BoolLiteral(_) => Some(Type::Simple("bool".to_string())),
        Expr::FnCall(name, _) => pure_fns
            .iter()
            .find(|f| f.name == *name)
            .map(|f| f.return_type.clone()),
        Expr::NamespacedCall {
            namespace, name, ..
        } if namespace.starts_with("std::") => {
            crate::codegen::stdlib::infer_std_call_return_type(namespace, name)
        }
        Expr::Ident(name) => entity
            .members
            .iter()
            .find(|m| m.name == *name)
            .map(|m| m.ty.clone())
            .or_else(|| {
                route
                    .params
                    .iter()
                    .find(|p| p.name == *name)
                    .map(|p| p.ty.clone())
            }),
        _ => None,
    }
}

fn check_member_transform_type(
    member: &Member,
    transform: &MemberTransform,
    entity: &Entity,
    route: &Route,
    pure_fns: &[PureFn],
    program_aliases: &[TypeAlias],
    diags: &mut Vec<Diagnostic>,
) {
    let body_ty = infer_transform_body_type(&transform.body, entity, route, pure_fns);
    let Some(body_ty) = body_ty else {
        return;
    };
    let unfold = |ty: &Type| unfold_alias(ty, &entity.type_aliases, program_aliases, 0);
    if transform_types_compatible(&unfold(&member.ty), &unfold(&body_ty), &transform.body) {
        return;
    }
    diags.push(
        Diagnostic::error("V50", v50_mismatch_message(member, transform, &body_ty))
            .with_span(transform.span),
    );
}

pub(super) fn unfold_alias(ty: &Type, local: &[TypeAlias], program: &[TypeAlias], depth: usize) -> Type {
    if depth > 64 {
        return ty.clone();
    }
    if let Type::Simple(name) = ty {
        if let Some(a) = local.iter().chain(program.iter()).find(|a| a.name == *name) {
            return unfold_alias(&a.ty, local, program, depth + 1);
        }
    }
    ty.clone()
}

fn is_plain_address(ty: &Type) -> bool {
    matches!(ty, Type::Simple(s) if s == "address" || s == "Address")
}

fn typed_address_entity(ty: &Type) -> Option<&str> {
    match ty {
        Type::TypedAddress(name) => Some(name.as_str()),
        _ => None,
    }
}

fn v50_mismatch_message(member: &Member, transform: &MemberTransform, body_ty: &Type) -> String {
    use crate::codegen::stdlib::{is_numeric_type, is_string_type};
    let got = crate::pretty::fmt_type(body_ty);
    let expected = crate::pretty::fmt_type(&member.ty);
    let prefix = format!(
        "member '{}' transform in route '{}': expression has type `{}` but member expects `{}`",
        member.name, transform.route_name, got, expected
    );
    let hint = if is_string_type(&member.ty) && is_numeric_type(body_ty)
        || is_numeric_type(&member.ty) && is_string_type(body_ty)
    {
        " — use an explicit `std::str::parse_uint` / `std::str::format` call (see docs/STDLIB.md)"
            .to_string()
    } else if is_plain_address(body_ty) && typed_address_entity(&member.ty).is_some()
        || typed_address_entity(body_ty).is_some() && is_plain_address(&member.ty)
        || typed_address_entity(body_ty).is_some() && typed_address_entity(&member.ty).is_some()
    {
        " — declare the source as the required `Address<Entity>` or use a supported explicit typed-address conversion"
            .to_string()
    } else {
        String::new()
    };
    format!("{prefix}{hint}")
}

fn transform_types_compatible(member_ty: &Type, body_ty: &Type, body: &Expr) -> bool {
    if member_ty == body_ty {
        return true;
    }
    use crate::codegen::stdlib::{
        is_numeric_type, is_std_numeric_to_string, is_std_string_to_numeric, is_string_type,
    };
    if is_string_type(member_ty) && is_numeric_type(body_ty) && is_std_numeric_to_string(body) {
        return true;
    }
    if is_numeric_type(member_ty) && is_string_type(body_ty) && is_std_string_to_numeric(body) {
        return true;
    }
    if is_numeric_type(member_ty) && is_numeric_type(body_ty) {
        return true;
    }
    if is_string_type(member_ty) && is_string_type(body_ty) {
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// V51: Unused `let` bindings in route bodies and pure-fn bodies
// ---------------------------------------------------------------------------

fn pattern_bound_idents(pat: &Pattern) -> Vec<String> {
    match pat {
        Pattern::Ident(name) => vec![name.clone()],
        Pattern::Wildcard | Pattern::None => vec![],
        Pattern::Tuple(pats) => pats.iter().flat_map(pattern_bound_idents).collect(),
        Pattern::Deref(inner) | Pattern::Some(inner) => pattern_bound_idents(inner),
    }
}

fn pattern_binds_name(pat: &Pattern, name: &str) -> bool {
    match pat {
        Pattern::Ident(n) => n == name,
        Pattern::Wildcard | Pattern::None => false,
        Pattern::Tuple(pats) => pats.iter().any(|p| pattern_binds_name(p, name)),
        Pattern::Deref(inner) | Pattern::Some(inner) => pattern_binds_name(inner, name),
    }
}

fn expr_mentions_binding(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::Ident(n) => n == name,
        Expr::Let(pat, val, body) => {
            expr_mentions_binding(val, name)
                || (!pattern_binds_name(pat, name) && expr_mentions_binding(body, name))
        }
        Expr::BinOp(l, _, r) => expr_mentions_binding(l, name) || expr_mentions_binding(r, name),
        Expr::UnaryOp(_, e) | Expr::Some(e) | Expr::Closure(_, e) => expr_mentions_binding(e, name),
        Expr::FnCall(_, args)
        | Expr::MacroRef(_, args)
        | Expr::NamespacedCall { args, .. }
        | Expr::EnumVariantWithData(_, _, args)
        | Expr::ArrayLit(args) => args.iter().any(|a| expr_mentions_binding(a, name)),
        Expr::MethodCall(base, _, args) => {
            expr_mentions_binding(base, name) || args.iter().any(|a| expr_mentions_binding(a, name))
        }
        Expr::If(c, t, e) => {
            expr_mentions_binding(c, name)
                || expr_mentions_binding(t, name)
                || e.as_ref().is_some_and(|el| expr_mentions_binding(el, name))
        }
        Expr::Match(s, arms) => {
            expr_mentions_binding(s, name)
                || arms
                    .iter()
                    .any(|arm| expr_mentions_binding(&arm.body, name))
        }
        Expr::Tuple(items) | Expr::Block(items) => {
            items.iter().any(|i| expr_mentions_binding(i, name))
        }
        Expr::RecordConstruct(_, fields) => {
            fields.iter().any(|(_, v)| expr_mentions_binding(v, name))
        }
        Expr::RecordUpdate(base, fields) => {
            expr_mentions_binding(base, name)
                || fields.iter().any(|(_, v)| expr_mentions_binding(v, name))
        }
        Expr::FieldAccess(base, _) => expr_mentions_binding(base, name),
        Expr::Index(base, idx) => {
            expr_mentions_binding(base, name) || expr_mentions_binding(idx, name)
        }
        Expr::Cast(e, _) => expr_mentions_binding(e, name),
        Expr::Range(s, e) => expr_mentions_binding(s, name) || expr_mentions_binding(e, name),
        Expr::For(pat, iter, body) => {
            expr_mentions_binding(iter, name)
                || (!pattern_binds_name(pat, name) && expr_mentions_binding(body, name))
        }
        Expr::AddressOf {
            args, with_params, ..
        } => {
            args.iter().any(|a| expr_mentions_binding(a, name))
                || with_params
                    .iter()
                    .any(|(_, v)| expr_mentions_binding(v, name))
        }
        Expr::Encode { value, .. } => expr_mentions_binding(value, name),
        Expr::IntLiteral(_)
       
       
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::BoolLiteral(_)
        | Expr::EmptyCollection
        | Expr::EnumVariant(_, _)
        | Expr::None
        | Expr::MsgField(_)
        | Expr::SysField(_)
        | Expr::TraceField(_)
        | Expr::TraceCall { .. }
        | Expr::TemporalRef(_) => false,
    }
}

fn action_mentions_binding(action: &RouteAction, name: &str) -> bool {
    match action {
        RouteAction::Let { value, .. } => expr_mentions_binding(value, name),
        RouteAction::Return { values } => values.iter().any(|v| expr_mentions_binding(v, name)),
        RouteAction::Send {
            args,
            dest,
            send_options,
            ..
        }
        | RouteAction::VarCall {
            args,
            dest,
            send_options,
            ..
        } => {
            args.iter().any(|a| expr_mentions_binding(a, name))
                || expr_mentions_binding(dest, name)
                || send_options
                    .as_ref()
                    .is_some_and(|o| expr_mentions_binding(o, name))
        }
        RouteAction::Deploy {
            constructor_args,
            send_options,
            ..
        } => {
            constructor_args
                .iter()
                .any(|a| expr_mentions_binding(a, name))
                || send_options
                    .as_ref()
                    .is_some_and(|o| expr_mentions_binding(o, name))
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            expr_mentions_binding(condition, name)
                || actions_mention_binding(then_actions, name)
                || actions_mention_binding(else_actions, name)
        }
        RouteAction::Effect { args, .. }
        | RouteAction::CallRoute { args, .. }
        | RouteAction::Emit { args, .. }
        | RouteAction::ThrowCustom { args, .. } => {
            args.iter().any(|a| expr_mentions_binding(a, name))
        }
        RouteAction::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            update_args.iter().any(|a| expr_mentions_binding(a, name))
                || callback_args.iter().any(|a| expr_mentions_binding(a, name))
        }
        RouteAction::For { iter, body, .. } => {
            expr_mentions_binding(iter, name) || actions_mention_binding(body, name)
        }
        RouteAction::Rescue { action, .. } => action_mentions_binding(action, name),
        RouteAction::Throw { .. } => false,
    }
}

fn actions_mention_binding(actions: &[RouteAction], name: &str) -> bool {
    action_list_mentions_binding_from_index(actions, 0, name)
}

fn action_list_mentions_binding_from_index(
    actions: &[RouteAction],
    start: usize,
    name: &str,
) -> bool {
    for action in actions.iter().skip(start) {
        if action_mentions_binding(action, name) {
            return true;
        }
    }
    false
}

fn push_v51_unused_let(name: &str, ctx: &str, diags: &mut Vec<Diagnostic>) {
    diags.push(Diagnostic::error(
        "V51",
        format!(
            "Unused `let {name}` binding in {ctx} — remove the binding or reference `{name}` in a later expression or action.",
            name = name,
            ctx = ctx,
        ),
    ));
}

fn check_unused_lets_in_actions(actions: &[RouteAction], ctx: &str, diags: &mut Vec<Diagnostic>) {
    for (i, action) in actions.iter().enumerate() {
        if let RouteAction::Let { pattern, value: _ } = action {
            for name in pattern_bound_idents(pattern) {
                let rest = &actions[i + 1..];
                if !actions_mention_binding(rest, &name) {
                    push_v51_unused_let(&name, ctx, diags);
                }
            }
        }
        match action {
            RouteAction::Conditional {
                then_actions,
                else_actions,
                ..
            } => {
                check_unused_lets_in_actions(then_actions, &format!("{ctx} if-branch"), diags);
                check_unused_lets_in_actions(else_actions, &format!("{ctx} else-branch"), diags);
            }
            RouteAction::For { body, .. } => {
                check_unused_lets_in_actions(body, &format!("{ctx} for-body"), diags);
            }
            RouteAction::Rescue { action: inner, .. } => {
                check_unused_lets_in_actions(std::slice::from_ref(inner), ctx, diags);
            }
            _ => {}
        }
    }
}

fn check_unused_lets_route_body(body: &RouteBody, route_ctx: &str, diags: &mut Vec<Diagnostic>) {
    match body {
        RouteBody::Unphased(actions) => check_unused_lets_in_actions(actions, route_ctx, diags),
        RouteBody::Phased(phases) => {
            check_unused_lets_in_phases(phases, &[], route_ctx, diags);
        }
        RouteBody::Mixed(phases, actions) => {
            check_unused_lets_in_phases(phases, actions, route_ctx, diags);
            check_unused_lets_in_actions(actions, route_ctx, diags);
        }
    }
}

/// V51 over a phased route body. A `let` bound at the top level of a phase
/// is in scope for every later phase — codegen hoists such bindings to
/// function scope (cross-phase lets), and per-phase `where` clauses on later
/// phases may read them (the same direction V28 enforces for `var`s) — so
/// usage is searched in the rest of the binding phase, in every later
/// phase's `where` clauses and actions, and in the trailing bare actions of
/// a `Mixed` body. `let`s inside nested blocks (`if` / `for` / `rescue`)
/// stay block-scoped, exactly as in an unphased body.
fn check_unused_lets_in_phases(
    phases: &[crate::ast::PhaseBlock],
    trailing: &[RouteAction],
    route_ctx: &str,
    diags: &mut Vec<Diagnostic>,
) {
    for (pi, phase) in phases.iter().enumerate() {
        let ctx = format!("{route_ctx} phase '{}'", phase.name);
        for (i, action) in phase.actions.iter().enumerate() {
            if let RouteAction::Let { pattern, value: _ } = action {
                for name in pattern_bound_idents(pattern) {
                    let used_in_same_phase =
                        actions_mention_binding(&phase.actions[i + 1..], &name);
                    let used_in_later_phase = phases[pi + 1..].iter().any(|later| {
                        later.where_clauses.iter().any(|w| {
                            expr_mentions_binding(&w.condition, &name)
                                || w.error_args.iter().any(|a| expr_mentions_binding(a, &name))
                        }) || actions_mention_binding(&later.actions, &name)
                    });
                    let used_in_trailing = actions_mention_binding(trailing, &name);
                    if !used_in_same_phase && !used_in_later_phase && !used_in_trailing {
                        push_v51_unused_let(&name, &ctx, diags);
                    }
                }
            }
            match action {
                RouteAction::Conditional {
                    then_actions,
                    else_actions,
                    ..
                } => {
                    check_unused_lets_in_actions(then_actions, &format!("{ctx} if-branch"), diags);
                    check_unused_lets_in_actions(
                        else_actions,
                        &format!("{ctx} else-branch"),
                        diags,
                    );
                }
                RouteAction::For { body, .. } => {
                    check_unused_lets_in_actions(body, &format!("{ctx} for-body"), diags);
                }
                RouteAction::Rescue { action: inner, .. } => {
                    check_unused_lets_in_actions(std::slice::from_ref(inner), &ctx, diags);
                }
                _ => {}
            }
        }
    }
}

pub(super) fn check_unused_route_lets_entity(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    for route in &entity.routes {
        let ctx = format!("route '{}' in entity '{}'", route.name, entity.name);
        check_unused_lets_route_body(&route.body, &ctx, diags);
    }
}

fn check_unused_lets_expr(expr: &Expr, ctx: &str, diags: &mut Vec<Diagnostic>) {
    match expr {
        Expr::Let(pat, val, body) => {
            check_unused_lets_expr(val, ctx, diags);
            for name in pattern_bound_idents(pat) {
                if !expr_mentions_binding(body, &name) {
                    push_v51_unused_let(&name, ctx, diags);
                }
            }
            check_unused_lets_expr(body, ctx, diags);
        }
        Expr::Block(items) => {
            for item in items {
                check_unused_lets_expr(item, ctx, diags);
            }
        }
        Expr::If(c, t, e) => {
            check_unused_lets_expr(c, ctx, diags);
            check_unused_lets_expr(t, ctx, diags);
            if let Some(el) = e {
                check_unused_lets_expr(el, ctx, diags);
            }
        }
        Expr::Match(s, arms) => {
            check_unused_lets_expr(s, ctx, diags);
            for arm in arms {
                check_unused_lets_expr(&arm.body, ctx, diags);
            }
        }
        Expr::BinOp(l, _, r) => {
            check_unused_lets_expr(l, ctx, diags);
            check_unused_lets_expr(r, ctx, diags);
        }
        Expr::UnaryOp(_, e)
        | Expr::Some(e)
        | Expr::Closure(_, e)
        | Expr::Cast(e, _)
        | Expr::FieldAccess(e, _) => check_unused_lets_expr(e, ctx, diags),
        Expr::Index(base, idx) => {
            check_unused_lets_expr(base, ctx, diags);
            check_unused_lets_expr(idx, ctx, diags);
        }
        Expr::FnCall(_, args)
        | Expr::MacroRef(_, args)
        | Expr::NamespacedCall { args, .. }
        | Expr::EnumVariantWithData(_, _, args)
        | Expr::ArrayLit(args) => {
            for a in args {
                check_unused_lets_expr(a, ctx, diags);
            }
        }
        Expr::MethodCall(base, _, args) => {
            check_unused_lets_expr(base, ctx, diags);
            for a in args {
                check_unused_lets_expr(a, ctx, diags);
            }
        }
        Expr::Tuple(items) => {
            for item in items {
                check_unused_lets_expr(item, ctx, diags);
            }
        }
        Expr::For(_, iter, body) => {
            check_unused_lets_expr(iter, ctx, diags);
            check_unused_lets_expr(body, ctx, diags);
        }
        Expr::Range(s, e) => {
            check_unused_lets_expr(s, ctx, diags);
            check_unused_lets_expr(e, ctx, diags);
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                check_unused_lets_expr(v, ctx, diags);
            }
        }
        Expr::RecordUpdate(base, fields) => {
            check_unused_lets_expr(base, ctx, diags);
            for (_, v) in fields {
                check_unused_lets_expr(v, ctx, diags);
            }
        }
        Expr::AddressOf {
            args, with_params, ..
        } => {
            for a in args {
                check_unused_lets_expr(a, ctx, diags);
            }
            for (_, v) in with_params {
                check_unused_lets_expr(v, ctx, diags);
            }
        }
        Expr::Encode { value, .. } => check_unused_lets_expr(value, ctx, diags),
        _ => {}
    }
}

pub(super) fn check_unused_lets_pure_fn(pure_fn: &PureFn, diags: &mut Vec<Diagnostic>) {
    let ctx = format!("pure fn '{}'", pure_fn.name);
    check_unused_lets_expr(&pure_fn.body, &ctx, diags);
}

#[allow(clippy::too_many_arguments)]
fn check_route_actions_expr_refs(
    route: &Route,
    entity: &Entity,
    known_fns: &HashSet<&str>,
    builtins: &HashSet<&str>,
    member_names: &HashSet<&str>,
    const_names: &HashSet<&str>,
    macro_names: &HashSet<&str>,
    record_names: &HashSet<&str>,
    scope: &HashSet<&str>,
    route_ctx: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let mut scope: HashSet<String> = scope.iter().map(|s| (*s).to_string()).collect();
    match &route.body {
        RouteBody::Unphased(actions) => {
            for action in actions {
                check_single_route_action_expr_refs(
                    action,
                    route,
                    entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    &mut scope,
                    route_ctx,
                    diags,
                );
            }
        }
        RouteBody::Phased(phases) => {
            for phase in phases {
                for action in &phase.actions {
                    check_single_route_action_expr_refs(
                        action,
                        route,
                        entity,
                        known_fns,
                        builtins,
                        member_names,
                        const_names,
                        macro_names,
                        record_names,
                        &mut scope,
                        route_ctx,
                        diags,
                    );
                }
            }
        }
        RouteBody::Mixed(phases, actions) => {
            for phase in phases {
                for action in &phase.actions {
                    check_single_route_action_expr_refs(
                        action,
                        route,
                        entity,
                        known_fns,
                        builtins,
                        member_names,
                        const_names,
                        macro_names,
                        record_names,
                        &mut scope,
                        route_ctx,
                        diags,
                    );
                }
            }
            for action in actions {
                check_single_route_action_expr_refs(
                    action,
                    route,
                    entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    &mut scope,
                    route_ctx,
                    diags,
                );
            }
        }
    }
}

fn scope_name_refs(scope: &HashSet<String>) -> HashSet<&str> {
    scope.iter().map(|s| s.as_str()).collect()
}

fn collect_pattern_name_strings(pat: &Pattern, names: &mut HashSet<String>) {
    match pat {
        Pattern::Ident(name) => {
            names.insert(name.clone());
        }
        Pattern::Wildcard | Pattern::None => {}
        Pattern::Tuple(pats) => {
            for p in pats {
                collect_pattern_name_strings(p, names);
            }
        }
        Pattern::Deref(inner) | Pattern::Some(inner) => collect_pattern_name_strings(inner, names),
    }
}

#[allow(clippy::too_many_arguments)]
fn check_single_route_action_expr_refs(
    action: &RouteAction,
    route: &Route,
    entity: &Entity,
    known_fns: &HashSet<&str>,
    builtins: &HashSet<&str>,
    member_names: &HashSet<&str>,
    const_names: &HashSet<&str>,
    macro_names: &HashSet<&str>,
    record_names: &HashSet<&str>,
    scope: &mut HashSet<String>,
    route_ctx: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let scope_refs = scope_name_refs(scope);
    match action {
        RouteAction::Let { pattern, value } => {
            check_entity_expr_refs(
                value,
                entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                &scope_refs,
                route_ctx,
                diags,
            );
            collect_pattern_name_strings(pattern, scope);
        }
        RouteAction::Return { values } => {
            for v in values {
                check_entity_expr_refs(
                    v,
                    entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    &scope_refs,
                    &format!("{} return", route_ctx),
                    diags,
                );
            }
        }
        RouteAction::Send {
            args,
            dest,
            send_options,
            ..
        } => {
            for a in args {
                check_entity_expr_refs(
                    a,
                    entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    &scope_refs,
                    &format!("{} send", route_ctx),
                    diags,
                );
            }
            check_entity_expr_refs(
                dest,
                entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                &scope_refs,
                &format!("{} send dest", route_ctx),
                diags,
            );
            if let Some(opts) = send_options {
                check_entity_expr_refs(
                    opts,
                    entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    &scope_refs,
                    &format!("{} send options", route_ctx),
                    diags,
                );
            }
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            check_entity_expr_refs(
                condition,
                entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                &scope_refs,
                &format!("{} if", route_ctx),
                diags,
            );
            for a in then_actions {
                check_single_route_action_expr_refs(
                    a,
                    route,
                    entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    scope,
                    route_ctx,
                    diags,
                );
            }
            for a in else_actions {
                check_single_route_action_expr_refs(
                    a,
                    route,
                    entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    scope,
                    route_ctx,
                    diags,
                );
            }
        }
        RouteAction::Effect { args, .. } => {
            for a in args {
                check_entity_expr_refs(
                    a,
                    entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    &scope_refs,
                    &format!("{} effect", route_ctx),
                    diags,
                );
            }
        }
        RouteAction::Deploy {
            constructor_args,
            send_options,
            ..
        } => {
            for a in constructor_args {
                check_entity_expr_refs(
                    a,
                    entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    &scope_refs,
                    &format!("{} deploy", route_ctx),
                    diags,
                );
            }
            if let Some(opts) = send_options {
                check_entity_expr_refs(
                    opts,
                    entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    &scope_refs,
                    &format!("{} deploy options", route_ctx),
                    diags,
                );
            }
        }
        RouteAction::Rescue { action, .. } => {
            check_single_route_action_expr_refs(
                action,
                route,
                entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                scope,
                route_ctx,
                diags,
            );
        }
        RouteAction::Throw { .. } => {}
        RouteAction::ThrowCustom { args, .. } => {
            for a in args {
                check_entity_expr_refs(
                    a,
                    entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    &scope_refs,
                    &format!("{} throw", route_ctx),
                    diags,
                );
            }
        }
        RouteAction::CallRoute { args, .. } => {
            for a in args {
                check_entity_expr_refs(
                    a,
                    entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    &scope_refs,
                    &format!("{} call", route_ctx),
                    diags,
                );
            }
        }
        RouteAction::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            for a in update_args {
                check_entity_expr_refs(
                    a,
                    entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    &scope_refs,
                    &format!("{} updateCode", route_ctx),
                    diags,
                );
            }
            for a in callback_args {
                check_entity_expr_refs(
                    a,
                    entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    &scope_refs,
                    &format!("{} updateCode", route_ctx),
                    diags,
                );
            }
        }
        RouteAction::Emit { args, .. } => {
            for a in args {
                check_entity_expr_refs(
                    a,
                    entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    &scope_refs,
                    &format!("{} emit", route_ctx),
                    diags,
                );
            }
        }
        RouteAction::VarCall { args, .. } => {
            for a in args {
                check_entity_expr_refs(
                    a,
                    entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    &scope_refs,
                    &format!("{} var call", route_ctx),
                    diags,
                );
            }
        }
        RouteAction::For {
            pattern,
            iter,
            body,
        } => {
            check_entity_expr_refs(
                iter,
                entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                &scope_refs,
                &format!("{} for", route_ctx),
                diags,
            );
            collect_pattern_name_strings(pattern, scope);
            for a in body {
                check_single_route_action_expr_refs(
                    a,
                    route,
                    entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    scope,
                    route_ctx,
                    diags,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn check_macro_refs_expr(
    expr: &Expr,
    entity: &Entity,
    known_fns: &HashSet<&str>,
    builtins: &HashSet<&str>,
    member_names: &HashSet<&str>,
    const_names: &HashSet<&str>,
    macro_names: &HashSet<&str>,
    record_names: &HashSet<&str>,
    params: &[Param],
    ctx: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let param_names: HashSet<&str> = params.iter().map(|p| p.name.as_str()).collect();
    let mut scope = param_names;
    scope.extend(member_names.iter());
    scope.extend(const_names.iter());

    check_entity_expr_refs(
        expr,
        entity,
        known_fns,
        builtins,
        member_names,
        const_names,
        macro_names,
        record_names,
        &scope,
        ctx,
        diags,
    );
}

#[allow(clippy::too_many_arguments)]
fn check_entity_expr_refs(
    expr: &Expr,
    _entity: &Entity,
    known_fns: &HashSet<&str>,
    builtins: &HashSet<&str>,
    member_names: &HashSet<&str>,
    const_names: &HashSet<&str>,
    macro_names: &HashSet<&str>,
    record_names: &HashSet<&str>,
    scope: &HashSet<&str>,
    ctx: &str,
    diags: &mut Vec<Diagnostic>,
) {
    match expr {
        Expr::Ident(name) => {
            let is_external_entity_ref = name
                .chars()
                .next()
                .map(|c| c.is_uppercase())
                .unwrap_or(false);
            if !scope.contains(name.as_str())
                && !member_names.contains(name.as_str())
                && !const_names.contains(name.as_str())
                && !record_names.contains(name.as_str())
                && !is_external_entity_ref
            {
                diags.push(Diagnostic::error(
                    "V8",
                    format!("Undefined identifier '{}' in {}", name, ctx),
                ));
            }
        }
        Expr::FnCall(name, args) => {
            check_bare_stdlib_call(name, known_fns, ctx, diags);
            if !known_fns.contains(name.as_str()) && !builtins.contains(name.as_str()) {
                diags.push(Diagnostic::error(
                    "V8",
                    format!("Undefined function '{}' in {}", name, ctx),
                ));
            }
            for a in args {
                check_entity_expr_refs(
                    a,
                    _entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    scope,
                    ctx,
                    diags,
                );
            }
        }
        Expr::MacroRef(name, args) => {
            if !macro_names.contains(name.as_str()) {
                diags.push(Diagnostic::error(
                    "V8",
                    format!("Undefined macro '@{}' in {}", name, ctx),
                ));
            }
            for a in args {
                check_entity_expr_refs(
                    a,
                    _entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    scope,
                    ctx,
                    diags,
                );
            }
        }
        Expr::TemporalRef(name) => {
            if !member_names.contains(name.as_str()) {
                diags.push(Diagnostic::error(
                    "V8",
                    format!("Undefined temporal reference '^{}' in {}", name, ctx),
                ));
            }
        }
        Expr::BinOp(l, _, r) => {
            check_entity_expr_refs(
                l,
                _entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                scope,
                ctx,
                diags,
            );
            check_entity_expr_refs(
                r,
                _entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                scope,
                ctx,
                diags,
            );
        }
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) => {
            check_entity_expr_refs(
                e,
                _entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                scope,
                ctx,
                diags,
            );
        }
        Expr::Index(e, idx) => {
            check_entity_expr_refs(
                e,
                _entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                scope,
                ctx,
                diags,
            );
            check_entity_expr_refs(
                idx,
                _entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                scope,
                ctx,
                diags,
            );
        }
        Expr::MethodCall(e, _, args) => {
            check_entity_expr_refs(
                e,
                _entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                scope,
                ctx,
                diags,
            );
            for a in args {
                check_entity_expr_refs(
                    a,
                    _entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    scope,
                    ctx,
                    diags,
                );
            }
        }
        Expr::If(cond, then_e, else_e) => {
            check_entity_expr_refs(
                cond,
                _entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                scope,
                ctx,
                diags,
            );
            check_entity_expr_refs(
                then_e,
                _entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                scope,
                ctx,
                diags,
            );
            if let Some(el) = else_e {
                check_entity_expr_refs(
                    el,
                    _entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    scope,
                    ctx,
                    diags,
                );
            }
        }
        Expr::Let(pat, val, body) => {
            check_entity_expr_refs(
                val,
                _entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                scope,
                ctx,
                diags,
            );
            let mut extended = scope.clone();
            collect_pattern_names(pat, &mut extended);
            check_entity_expr_refs(
                body,
                _entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                &extended,
                ctx,
                diags,
            );
        }
        Expr::Block(stmts) => {
            for s in stmts {
                check_entity_expr_refs(
                    s,
                    _entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    scope,
                    ctx,
                    diags,
                );
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                check_entity_expr_refs(
                    v,
                    _entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    scope,
                    ctx,
                    diags,
                );
            }
        }
        Expr::RecordUpdate(base, fields) => {
            check_entity_expr_refs(
                base,
                _entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                scope,
                ctx,
                diags,
            );
            for (_, v) in fields {
                check_entity_expr_refs(
                    v,
                    _entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    scope,
                    ctx,
                    diags,
                );
            }
        }
        Expr::Closure(params, body) => {
            let mut extended = scope.clone();
            for p in params {
                collect_pattern_names(p, &mut extended);
            }
            check_entity_expr_refs(
                body,
                _entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                &extended,
                ctx,
                diags,
            );
        }
        Expr::Tuple(elems) => {
            for e in elems {
                check_entity_expr_refs(
                    e,
                    _entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    scope,
                    ctx,
                    diags,
                );
            }
        }
        Expr::Match(subject, arms) => {
            check_entity_expr_refs(
                subject,
                _entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                scope,
                ctx,
                diags,
            );
            for arm in arms {
                let mut extended = scope.clone();
                collect_match_pattern_names(&arm.pattern, &mut extended);
                check_entity_expr_refs(
                    &arm.body,
                    _entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    &extended,
                    ctx,
                    diags,
                );
            }
        }
        Expr::Some(inner) => {
            check_entity_expr_refs(
                inner,
                _entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                scope,
                ctx,
                diags,
            );
        }
        Expr::ArrayLit(elems) => {
            for e in elems {
                check_entity_expr_refs(
                    e,
                    _entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    scope,
                    ctx,
                    diags,
                );
            }
        }
        Expr::EnumVariantWithData(_, _, args) => {
            for a in args {
                check_entity_expr_refs(
                    a,
                    _entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    scope,
                    ctx,
                    diags,
                );
            }
        }
        Expr::NamespacedCall { args, .. } => {
            for a in args {
                check_entity_expr_refs(
                    a,
                    _entity,
                    known_fns,
                    builtins,
                    member_names,
                    const_names,
                    macro_names,
                    record_names,
                    scope,
                    ctx,
                    diags,
                );
            }
        }
        Expr::Range(start, end) => {
            check_entity_expr_refs(
                start,
                _entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                scope,
                ctx,
                diags,
            );
            check_entity_expr_refs(
                end,
                _entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                scope,
                ctx,
                diags,
            );
        }
        Expr::For(pat, iter, body) => {
            check_entity_expr_refs(
                iter,
                _entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                scope,
                ctx,
                diags,
            );
            let mut extended = scope.clone();
            collect_pattern_names(pat, &mut extended);
            check_entity_expr_refs(
                body,
                _entity,
                known_fns,
                builtins,
                member_names,
                const_names,
                macro_names,
                record_names,
                &extended,
                ctx,
                diags,
            );
        }
        Expr::TraceField(_) | Expr::TraceCall { .. } => {
            diags.push(Diagnostic::error("I16",
                format!("{}: trace:: accessors are only valid inside invariant 'assume'/'check' expressions", ctx)));
        }
        Expr::IntLiteral(_)
       
       
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::BoolLiteral(_)
        | Expr::EmptyCollection
        | Expr::MsgField(_)
        | Expr::SysField(_)
        | Expr::EnumVariant(_, _)
        | Expr::None
        | Expr::AddressOf { .. }
        | Expr::Encode { .. } => {}
    }
}

// ---------------------------------------------------------------------------
// V8: rescue/recover matching
// ---------------------------------------------------------------------------

pub(super) fn check_bounce_handlers(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    let route_names: HashSet<&str> = entity
        .routes
        .iter()
        .filter(|r| !r.name.starts_with("onBounce_") && !r.name.starts_with("recover_"))
        .map(|r| r.name.as_str())
        .collect();

    let mut rescue_tags: HashSet<String> = HashSet::new();
    for route in &entity.routes {
        collect_rescue_tags_in_body(&route.body, &mut rescue_tags);
    }

    let recover_tags: HashSet<&str> = entity
        .routes
        .iter()
        .filter_map(|r| r.recover_tag.as_deref())
        .collect();

    for route in &entity.routes {
        if let Some(ref tag) = route.recover_tag {
            if route.name.starts_with("onBounce_") && !route_names.contains(tag.as_str()) {
                diags.push(Diagnostic::error(
                    "V8",
                    format!(
                        "onBounce handler references unknown route '{}' in entity '{}'",
                        tag, entity.name
                    ),
                ));
            }
        }
    }

    let recover_tags_needing_rescue: HashSet<&str> = entity
        .routes
        .iter()
        .filter(|r| r.recover_tag.is_some() && !r.name.starts_with("onBounce_"))
        .filter_map(|r| r.recover_tag.as_deref())
        .collect();

    for tag in &rescue_tags {
        if !recover_tags.contains(tag.as_str()) {
            diags.push(Diagnostic::error(
                "V8",
                format!(
                    "rescue tag '{}' has no matching 'recover {}' handler in entity '{}'",
                    tag, tag, entity.name
                ),
            ));
        }
    }

    for tag in &recover_tags_needing_rescue {
        if !rescue_tags.contains(*tag) {
            diags.push(Diagnostic::error(
                "V8",
                format!(
                    "recover handler '{}' has no matching 'rescue {}:' action in entity '{}'",
                    tag, tag, entity.name
                ),
            ));
        }
    }
}

pub(super) fn check_rescue_bounce_false(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    for route in &entity.routes {
        for action in route.body.all_actions() {
            check_rescue_bounce_false_in_action(action, &entity.name, &route.name, diags);
        }
    }
}

fn check_rescue_bounce_false_in_action(
    action: &RouteAction,
    entity_name: &str,
    route_name: &str,
    diags: &mut Vec<Diagnostic>,
) {
    match action {
        RouteAction::Rescue { tag, action } => {
            let opts = match action.as_ref() {
                RouteAction::Send { send_options, .. } => send_options.as_ref(),
                RouteAction::Deploy { send_options, .. } => send_options.as_ref(),
                _ => None,
            };
            if let Some(Expr::RecordConstruct(_, fields) | Expr::RecordUpdate(_, fields)) = opts {
                for (key, val) in fields {
                    if key == "bounce" {
                        if let Expr::BoolLiteral(false) = val {
                            diags.push(Diagnostic::error(
                                "V26",
                                format!(
                                    "rescue '{}' in route '{}' of entity '{}' has bounce: false — \
                                    the message will never bounce and recover will never be called",
                                    tag, route_name, entity_name
                                ),
                            ));
                        }
                    }
                }
            }
        }
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            for a in then_actions {
                check_rescue_bounce_false_in_action(a, entity_name, route_name, diags);
            }
            for a in else_actions {
                check_rescue_bounce_false_in_action(a, entity_name, route_name, diags);
            }
        }
        RouteAction::For { body, .. } => {
            for a in body {
                check_rescue_bounce_false_in_action(a, entity_name, route_name, diags);
            }
        }
        _ => {}
    }
}

fn collect_rescue_tags_in_body(body: &RouteBody, tags: &mut HashSet<String>) {
    match body {
        RouteBody::Phased(phases) => {
            for phase in phases {
                for action in &phase.actions {
                    collect_rescue_tags_in_action(action, tags);
                }
            }
        }
        RouteBody::Unphased(actions) => {
            for action in actions {
                collect_rescue_tags_in_action(action, tags);
            }
        }
        RouteBody::Mixed(phases, actions) => {
            for phase in phases {
                for action in &phase.actions {
                    collect_rescue_tags_in_action(action, tags);
                }
            }
            for action in actions {
                collect_rescue_tags_in_action(action, tags);
            }
        }
    }
}

fn collect_rescue_tags_in_action(action: &RouteAction, tags: &mut HashSet<String>) {
    match action {
        RouteAction::Rescue { tag, .. } => {
            tags.insert(tag.clone());
        }
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            for a in then_actions {
                collect_rescue_tags_in_action(a, tags);
            }
            for a in else_actions {
                collect_rescue_tags_in_action(a, tags);
            }
        }
        RouteAction::For { body, .. } => {
            for a in body {
                collect_rescue_tags_in_action(a, tags);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// W4: Deprecated bounce handlers
// ---------------------------------------------------------------------------

pub(super) fn check_deprecated_bounce_handlers(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    for route in &entity.routes {
        if route.name == "onDeployBounce" || route.name == "onTransferBounce" {
            diags.push(Diagnostic::warning("W4",
                format!("'{}' in entity '{}' is deprecated. Use 'rescue tag: ...' / 'recover tag(body: CamData)' instead.",
                    route.name, entity.name)));
        }
    }
}

// ---------------------------------------------------------------------------
// V16-V17: Identity member constraints
// ---------------------------------------------------------------------------

pub(super) fn check_identity_members(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    for member in &entity.members {
        if !member.is_identity {
            continue;
        }

        // V16
        if !member.transforms.is_empty() {
            diags.push(Diagnostic::error(
                "V16",
                format!(
                    "Identity member '{}' in entity '{}' must not have transforms \
                         (identity fields are set at deploy time via static variables)",
                    member.name, entity.name
                ),
            ));
        }

        // V17
        if member.default_value.is_some() {
            diags.push(Diagnostic::error(
                "V17",
                format!(
                    "Identity member '{}' in entity '{}' must not have a default value \
                         (identity fields are set at deploy time via genaddr --data)",
                    member.name, entity.name
                ),
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// V13-V15, V54: Phase consistency checks for phased routes
// ---------------------------------------------------------------------------

pub(super) fn check_phase_consistency(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    for route in &entity.routes {
        if route.body.is_mixed() {
            // PM-030: mixed phase/bare body is V54 (V27 reserved for var-in-where).
            diags.push(Diagnostic::error(
                "V54",
                format!(
                    "Route '{}' in entity '{}' mixes phase blocks and bare actions",
                    route.name, entity.name
                ),
            ));
            continue;
        }
        if let Some(phases) = route.body.phases() {
            // V13
            let mut seen = HashSet::new();
            for phase in phases {
                if !seen.insert(phase.name.as_str()) {
                    diags.push(Diagnostic::error(
                        "V13",
                        format!(
                            "Duplicate phase '{}' in route '{}' of entity '{}'",
                            phase.name, route.name, entity.name
                        ),
                    ));
                }
            }

            let phase_names: HashSet<&str> = phases.iter().map(|p| p.name.as_str()).collect();

            for member in &entity.members {
                for transform in &member.transforms {
                    if transform.route_name != route.name {
                        continue;
                    }

                    // V14
                    if transform.phase.is_none() {
                        diags.push(Diagnostic::error("V14",
                            format!("Member '{}' has unphased transform for phased route '{}' in entity '{}'",
                                member.name, route.name, entity.name)));
                    }

                    // V15
                    if let Some(ref tag) = transform.phase {
                        if !phase_names.contains(tag.as_str()) {
                            diags.push(Diagnostic::error("V15",
                                format!("Member '{}' references unknown phase '{}' in route '{}' of entity '{}'",
                                    member.name, tag, route.name, entity.name)));
                        }
                    }
                }
            }
        } else {
            for member in &entity.members {
                for transform in &member.transforms {
                    if transform.route_name != route.name {
                        continue;
                    }

                    // V14
                    if transform.phase.is_some() {
                        diags.push(Diagnostic::error("V14",
                            format!("Member '{}' has phased transform for unphased route '{}' in entity '{}'",
                                member.name, route.name, entity.name)));
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// V23: Named message sends must go to typed Address<Entity> destinations
// ---------------------------------------------------------------------------

pub(super) fn check_typed_send_destinations(
    entity: &Entity,
    all_entities: &[Entity],
    extern_entities: &[ExternEntity],
    diags: &mut Vec<Diagnostic>,
) {
    for route in &entity.routes {
        for action in route.body.all_actions() {
            check_typed_send_in_action(action, entity, route, all_entities, extern_entities, diags);
        }
    }
}

fn check_typed_send_in_action(
    action: &RouteAction,
    entity: &Entity,
    route: &Route,
    all_entities: &[Entity],
    extern_entities: &[ExternEntity],
    diags: &mut Vec<Diagnostic>,
) {
    match action {
        RouteAction::Send {
            message: Some(msg),
            dest,
            ..
        } => {
            let resolved = resolve_dest_entity(dest, entity, route);
            match resolved {
                None => {
                    diags.push(Diagnostic::error(
                        "V23",
                        format!(
                            "Named message '{}' in {}.{} sent to untyped address. \
                             Use Address<Entity> type on the destination variable, \
                             or only use plain transfers (~>) for untyped addresses.",
                            msg, entity.name, route.name
                        ),
                    ));
                }
                Some(target_entity_name) => {
                    if let Some(target) = all_entities.iter().find(|e| e.name == target_entity_name)
                    {
                        let has_route = target.routes.iter().any(|r| r.name == *msg);
                        if !has_route {
                            diags.push(Diagnostic::error(
                                "V23",
                                format!(
                                    "Route '{}' not found on entity '{}' (referenced in {}.{}).",
                                    msg, target_entity_name, entity.name, route.name
                                ),
                            ));
                        }
                    } else if let Some(ext) = extern_entities
                        .iter()
                        .find(|e| e.name == target_entity_name)
                    {
                        let has_route = ext.routes.iter().any(|r| r.name == *msg);
                        if !has_route {
                            diags.push(Diagnostic::error("V23",
                                format!(
                                    "Route '{}' not found on extern entity '{}' (referenced in {}.{}).",
                                    msg, target_entity_name, entity.name, route.name)));
                        }
                    }
                }
            }
        }
        RouteAction::Rescue { action, .. } => {
            check_typed_send_in_action(action, entity, route, all_entities, extern_entities, diags);
        }
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            for a in then_actions {
                check_typed_send_in_action(a, entity, route, all_entities, extern_entities, diags);
            }
            for a in else_actions {
                check_typed_send_in_action(a, entity, route, all_entities, extern_entities, diags);
            }
        }
        RouteAction::For { body, .. } => {
            for a in body {
                check_typed_send_in_action(a, entity, route, all_entities, extern_entities, diags);
            }
        }
        RouteAction::VarCall {
            name,
            message,
            dest,
            ..
        } => {
            let ctx = format!("{}.{}", entity.name, route.name);
            let resolved = resolve_dest_entity(dest, entity, route);
            match resolved {
                None => {
                    diags.push(Diagnostic::error(
                        "V23",
                        format!(
                            "var call '{}' in {} targets untyped address. \
                             Use Address<Entity> type on the destination variable.",
                            name, ctx
                        ),
                    ));
                }
                Some(target_entity_name) => {
                    if let Some(target) = all_entities.iter().find(|e| e.name == target_entity_name)
                    {
                        let target_route = target.routes.iter().find(|r| r.name == *message);
                        match target_route {
                            None => {
                                diags.push(Diagnostic::error(
                                    "V23",
                                    format!(
                                        "Route '{}' not found on entity '{}' (var call in {}).",
                                        message, target_entity_name, ctx
                                    ),
                                ));
                            }
                            Some(tr) => {
                                if tr.return_type.is_none() {
                                    diags.push(Diagnostic::error("V24",
                                        format!(
                                            "var call '{}' in {} targets route '{}' on '{}' which has no return type. \
                                             Use a regular send action for void routes.",
                                            name, ctx, message, target_entity_name)));
                                }
                            }
                        }
                    } else if let Some(ext) = extern_entities
                        .iter()
                        .find(|e| e.name == target_entity_name)
                    {
                        let target_route = ext.routes.iter().find(|r| r.name == *message);
                        match target_route {
                            None => {
                                diags.push(Diagnostic::error("V23",
                                    format!(
                                        "Route '{}' not found on extern entity '{}' (var call in {}).",
                                        message, target_entity_name, ctx)));
                            }
                            Some(tr) => {
                                if tr.return_type.is_none() {
                                    diags.push(Diagnostic::error("V24",
                                        format!(
                                            "var call '{}' in {} targets route '{}' on extern entity '{}' which has no return type. \
                                             Use a regular send action for void routes.",
                                            name, ctx, message, target_entity_name)));
                                }
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// V30 / V31: Extern entity well-formedness
// ---------------------------------------------------------------------------

/// V30: duplicate extern entity name within the program; collision with a
/// real entity name. V31: duplicate route signature within a single
/// extern entity block.
pub(super) fn check_extern_entities(program: &Program, diags: &mut Vec<Diagnostic>) {
    use std::collections::HashSet;

    let real_names: HashSet<&str> = program.entities.iter().map(|e| e.name.as_str()).collect();
    let mut seen_extern: HashSet<&str> = HashSet::new();

    for ext in &program.extern_entities {
        if real_names.contains(ext.name.as_str()) {
            diags.push(Diagnostic::error(
                "V30",
                format!(
                    "extern entity '{}' collides with a real entity declaration in this program",
                    ext.name
                ),
            ));
        }
        if !seen_extern.insert(ext.name.as_str()) {
            diags.push(Diagnostic::error(
                "V30",
                format!("extern entity '{}' is declared more than once", ext.name),
            ));
        }

        let mut seen_routes: HashSet<&str> = HashSet::new();
        for r in &ext.routes {
            if !seen_routes.insert(r.name.as_str()) {
                diags.push(Diagnostic::error(
                    "V31",
                    format!(
                        "extern entity '{}': route '{}' is declared more than once",
                        ext.name, r.name
                    ),
                ));
            }
        }
    }
}

pub(super) fn resolve_dest_entity_for_evm(
    dest: &Expr,
    entity: &Entity,
    route: &Route,
) -> Option<String> {
    resolve_dest_entity(dest, entity, route)
}

fn resolve_dest_entity(dest: &Expr, entity: &Entity, route: &Route) -> Option<String> {
    if let Expr::MethodCall(receiver, method, _) = dest {
        if method == "address" {
            if let Expr::Ident(name) = receiver.as_ref() {
                if name
                    .chars()
                    .next()
                    .map(|c| c.is_uppercase())
                    .unwrap_or(false)
                {
                    return Some(name.clone());
                }
            }
        }
    }

    // `addressOf(Entity.state(args))` — equivalent to `Entity.address(args)`.
    if let Expr::FnCall(fn_name, args) = dest {
        if fn_name == "addressOf" && args.len() == 1 {
            if let Expr::MethodCall(inner_recv, inner_method, _) = &args[0] {
                if inner_method == "state" {
                    if let Expr::Ident(name) = inner_recv.as_ref() {
                        if name
                            .chars()
                            .next()
                            .map(|c| c.is_uppercase())
                            .unwrap_or(false)
                        {
                            return Some(name.clone());
                        }
                    }
                }
            }
        }
    }

    // Test-only `address_of Entity(args)` keyword form.
    if let Expr::AddressOf { entity_name, .. } = dest {
        if entity_name
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false)
        {
            return Some(entity_name.clone());
        }
    }

    if let Expr::Ident(var_name) = dest {
        if let Some(member) = entity.members.iter().find(|m| m.name == *var_name) {
            if let Type::TypedAddress(entity_name) = &member.ty {
                return Some(entity_name.clone());
            }
        }
        if let Some(param) = route.params.iter().find(|p| p.name == *var_name) {
            if let Type::TypedAddress(entity_name) = &param.ty {
                return Some(entity_name.clone());
            }
        }
    }

    if matches!(dest, Expr::MsgField(f) if f == "sender") {
        for wc in &route.where_clauses {
            if let Some(name) = extract_entity_from_where_condition(&wc.condition, entity) {
                return Some(name);
            }
        }
    }

    if let Expr::MacroRef(macro_name, _) = dest {
        if let Some(mac) = entity.macros.iter().find(|m| m.name == *macro_name) {
            if let Type::TypedAddress(entity_name) = &mac.return_type {
                return Some(entity_name.clone());
            }
        }
    }

    if let Expr::MethodCall(receiver, method, _) = dest {
        if method == "unwrap" {
            if let Expr::Ident(var_name) = receiver.as_ref() {
                if let Some(member) = entity.members.iter().find(|m| m.name == *var_name) {
                    if let Type::Generic(outer, params) = &member.ty {
                        if outer == "Option" && params.len() == 1 {
                            if let Type::TypedAddress(entity_name) = &params[0] {
                                return Some(entity_name.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

fn extract_entity_from_where_condition(expr: &Expr, entity: &Entity) -> Option<String> {
    match expr {
        Expr::BinOp(lhs, BinOp::Eq, rhs) => {
            if matches!(lhs.as_ref(), Expr::MsgField(f) if f == "sender") {
                return extract_entity_from_addr_or_macro(rhs, entity);
            }
            if matches!(rhs.as_ref(), Expr::MsgField(f) if f == "sender") {
                return extract_entity_from_addr_or_macro(lhs, entity);
            }
            None
        }
        Expr::BinOp(lhs, BinOp::And | BinOp::Or, rhs) => {
            extract_entity_from_where_condition(lhs, entity)
                .or_else(|| extract_entity_from_where_condition(rhs, entity))
        }
        _ => None,
    }
}

fn extract_entity_from_addr_or_macro(expr: &Expr, entity: &Entity) -> Option<String> {
    if let Some(name) = extract_entity_from_addr_call(expr) {
        return Some(name);
    }
    if let Expr::MacroRef(macro_name, _) = expr {
        if let Some(mac) = entity.macros.iter().find(|m| m.name == *macro_name) {
            if let Type::TypedAddress(entity_name) = &mac.return_type {
                return Some(entity_name.clone());
            }
        }
    }
    None
}

fn extract_entity_from_addr_call(expr: &Expr) -> Option<String> {
    match expr {
        Expr::MethodCall(receiver, method, _) if method == "address" => {
            if let Expr::Ident(name) = receiver.as_ref() {
                if name
                    .chars()
                    .next()
                    .map(|c| c.is_uppercase())
                    .unwrap_or(false)
                {
                    return Some(name.clone());
                }
            }
            None
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// V20: Empty string literal "" must not be used in address context
// ---------------------------------------------------------------------------

pub(super) fn check_empty_string_as_address(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    let addr_members: HashSet<&str> = entity
        .members
        .iter()
        .filter(|m| {
            matches!(&m.ty, Type::Simple(t) if t == "address")
                || matches!(&m.ty, Type::TypedAddress(_))
        })
        .map(|m| m.name.as_str())
        .collect();

    for route in &entity.routes {
        for wc in &route.where_clauses {
            check_empty_string_addr_expr(&wc.condition, &addr_members, entity, route, diags);
        }
        for action in route.body.all_actions() {
            check_empty_string_addr_action(action, &addr_members, entity, route, diags);
        }
    }
    for member in &entity.members {
        for transform in &member.transforms {
            if let Some(route) = entity
                .routes
                .iter()
                .find(|r| r.name == transform.route_name)
                .or(entity.routes.first())
            {
                check_empty_string_addr_expr(&transform.body, &addr_members, entity, route, diags);
            }
        }
    }
}

fn is_addr_expr(expr: &Expr, addr_members: &HashSet<&str>) -> bool {
    match expr {
        Expr::MsgField(f) if f == "sender" => true,
        Expr::SysField(f) if f == "address" => true,
        Expr::Ident(name) => addr_members.contains(name.as_str()),
        _ => false,
    }
}

fn check_empty_string_addr_expr(
    expr: &Expr,
    addr_members: &HashSet<&str>,
    entity: &Entity,
    route: &Route,
    diags: &mut Vec<Diagnostic>,
) {
    match expr {
        Expr::BinOp(lhs, BinOp::Eq | BinOp::Ne, rhs) => {
            let lhs_is_addr = is_addr_expr(lhs, addr_members);
            let rhs_is_addr = is_addr_expr(rhs, addr_members);
            let lhs_is_empty = matches!(lhs.as_ref(), Expr::StringLiteral(s) if s.is_empty());
            let rhs_is_empty = matches!(rhs.as_ref(), Expr::StringLiteral(s) if s.is_empty());

            if (lhs_is_addr && rhs_is_empty) || (rhs_is_addr && lhs_is_empty) {
                diags.push(Diagnostic::error(
                    "V20",
                    format!(
                        "Empty string \"\" compared with address in route '{}' of entity '{}'. \
                             Use `msg::int` to check for internal messages, \
                             or compare with a concrete address.",
                        route.name, entity.name
                    ),
                ));
            }
            check_empty_string_addr_expr(lhs, addr_members, entity, route, diags);
            check_empty_string_addr_expr(rhs, addr_members, entity, route, diags);
        }
        Expr::BinOp(lhs, _, rhs) => {
            check_empty_string_addr_expr(lhs, addr_members, entity, route, diags);
            check_empty_string_addr_expr(rhs, addr_members, entity, route, diags);
        }
        Expr::UnaryOp(_, inner) => {
            check_empty_string_addr_expr(inner, addr_members, entity, route, diags)
        }
        Expr::FnCall(_, args) | Expr::MacroRef(_, args) => {
            for a in args {
                check_empty_string_addr_expr(a, addr_members, entity, route, diags);
            }
        }
        Expr::MethodCall(base, _, args) => {
            check_empty_string_addr_expr(base, addr_members, entity, route, diags);
            for a in args {
                check_empty_string_addr_expr(a, addr_members, entity, route, diags);
            }
        }
        Expr::Cast(inner, _) => {
            check_empty_string_addr_expr(inner, addr_members, entity, route, diags)
        }
        Expr::If(c, t, e) => {
            check_empty_string_addr_expr(c, addr_members, entity, route, diags);
            check_empty_string_addr_expr(t, addr_members, entity, route, diags);
            if let Some(el) = e {
                check_empty_string_addr_expr(el, addr_members, entity, route, diags);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                check_empty_string_addr_expr(v, addr_members, entity, route, diags);
            }
        }
        Expr::RecordUpdate(base, fields) => {
            check_empty_string_addr_expr(base, addr_members, entity, route, diags);
            for (_, v) in fields {
                check_empty_string_addr_expr(v, addr_members, entity, route, diags);
            }
        }
        _ => {}
    }
}

fn check_empty_string_addr_action(
    action: &RouteAction,
    addr_members: &HashSet<&str>,
    entity: &Entity,
    route: &Route,
    diags: &mut Vec<Diagnostic>,
) {
    if let RouteAction::Conditional {
        condition,
        then_actions,
        else_actions,
    } = action
    {
        check_empty_string_addr_expr(condition, addr_members, entity, route, diags);
        for a in then_actions {
            check_empty_string_addr_action(a, addr_members, entity, route, diags);
        }
        for a in else_actions {
            check_empty_string_addr_action(a, addr_members, entity, route, diags);
        }
    }
}

// ---------------------------------------------------------------------------
// V45: Multiple `deploy Entity(...)` in one route
// ---------------------------------------------------------------------------

pub(super) fn check_duplicate_deploys(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    for route in &entity.routes {
        let mut deploys: Vec<(&str, &[Expr])> = Vec::new();
        collect_deploys_in_actions(route.body.all_actions().as_slice(), &mut deploys);
        // Group by entity name, preserving first-seen order for stable diagnostics.
        let mut seen: Vec<&str> = Vec::new();
        for (name, _) in &deploys {
            if !seen.contains(name) {
                seen.push(name);
            }
        }
        for name in seen {
            let group: Vec<&[Expr]> = deploys
                .iter()
                .filter(|(n, _)| *n == name)
                .map(|(_, args)| *args)
                .collect();
            if group.len() < 2 {
                continue;
            }
            let mut identical = false;
            'pairs: for i in 0..group.len() {
                for j in (i + 1)..group.len() {
                    if deploy_args_eq(group[i], group[j]) {
                        identical = true;
                        break 'pairs;
                    }
                }
            }
            if identical {
                diags.push(Diagnostic::error(
                    "V45",
                    format!(
                        "route '{}' of entity '{}' deploys '{}' more than once with identical \
                         constructor arguments (CREATE2 / occupancy collision)",
                        route.name, entity.name, name,
                    ),
                ));
            } else {
                diags.push(Diagnostic::warning(
                    "V45",
                    format!(
                        "route '{}' of entity '{}' deploys '{}' more than once; ensure \
                         constructor arguments yield distinct addresses",
                        route.name, entity.name, name,
                    ),
                ));
            }
        }
    }
}

/// True when two `deploy Entity(...)` argument lists collide on CREATE2:
/// identical AST, **or** each pair const-folds to the same integer
/// (`Vault(0)` vs `Vault(0+0)`).
fn deploy_args_eq(a: &[Expr], b: &[Expr]) -> bool {
    if a == b {
        return true;
    }
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(x, y)| {
        x == y
            || match (const_eval_u256(x), const_eval_u256(y)) {
                (Some(u), Some(v)) => u == v,
                _ => false,
            }
    })
}

/// Fold a deploy-arg / fuzz-range tree of integer literals and `+`/`-`/`*`
/// (and wrapping variants / trivial `as` casts) to `U256`. Used by V45 and T24.
pub(crate) fn const_eval_u256(e: &Expr) -> Option<cambrian_core::U256> {
    match e {
        Expr::IntLiteral(n) => Some(*n),
        Expr::Cast(inner, _) => const_eval_u256(inner),
        Expr::BinOp(l, op, r) => {
            let a = const_eval_u256(l)?;
            let b = const_eval_u256(r)?;
            match op {
                BinOp::Add | BinOp::WrappingAdd => Some(a.wrapping_add(b)),
                BinOp::Sub | BinOp::WrappingSub => Some(a.wrapping_sub(b)),
                BinOp::Mul | BinOp::WrappingMul => Some(a.wrapping_mul(b)),
                _ => None,
            }
        }
        _ => None,
    }
}

fn collect_deploys_in_actions<'a>(
    actions: &[&'a RouteAction],
    out: &mut Vec<(&'a str, &'a [Expr])>,
) {
    for action in actions {
        collect_deploys_in_action(action, out);
    }
}

fn collect_deploys_in_action<'a>(action: &'a RouteAction, out: &mut Vec<(&'a str, &'a [Expr])>) {
    match action {
        RouteAction::Deploy {
            entity,
            constructor_args,
            ..
        } => {
            out.push((entity.as_str(), constructor_args.as_slice()));
        }
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            for a in then_actions {
                collect_deploys_in_action(a, out);
            }
            for a in else_actions {
                collect_deploys_in_action(a, out);
            }
        }
        RouteAction::For { body, .. } => {
            for a in body {
                collect_deploys_in_action(a, out);
            }
        }
        RouteAction::Rescue { action: inner, .. } => {
            collect_deploys_in_action(inner, out);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// V47: Unreachable / overlapping match arms
// ---------------------------------------------------------------------------

pub(super) fn check_match_arm_priority_entity(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    for route in &entity.routes {
        for action in route.body.all_actions() {
            check_match_arm_priority_action(action, &entity.name, Some(&route.name), diags);
        }
    }
    for member in &entity.members {
        for transform in &member.transforms {
            check_match_arm_priority_expr(
                &transform.body,
                &entity.name,
                Some(&transform.route_name),
                diags,
            );
        }
    }
}

pub(super) fn check_match_arm_priority_pure_fn(pure_fn: &PureFn, diags: &mut Vec<Diagnostic>) {
    check_match_arm_priority_expr(&pure_fn.body, &pure_fn.name, None, diags);
}

fn check_match_arm_priority_action(
    action: &RouteAction,
    owner: &str,
    route: Option<&str>,
    diags: &mut Vec<Diagnostic>,
) {
    walk_action_exprs(action, &mut |e| {
        check_match_arm_priority_expr(e, owner, route, diags);
    });
}

fn check_match_arm_priority_expr(
    expr: &Expr,
    owner: &str,
    route: Option<&str>,
    diags: &mut Vec<Diagnostic>,
) {
    if let Expr::Match(_, arms) = expr {
        check_match_arms_list(arms, owner, route, diags);
    }
    walk_subexprs(expr, &mut |e| {
        check_match_arm_priority_expr(e, owner, route, diags);
    });
}

fn check_match_arms_list(
    arms: &[MatchArm],
    owner: &str,
    route: Option<&str>,
    diags: &mut Vec<Diagnostic>,
) {
    let mut seen_catch_all = false;
    let mut seen_keys: Vec<String> = Vec::new();
    for arm in arms {
        if seen_catch_all {
            push_v47(
                owner,
                route,
                "unreachable match arm after a catch-all (`_` or bare identifier) pattern",
                diags,
            );
        }
        match &arm.pattern {
            MatchPattern::Wildcard | MatchPattern::Ident(_) => {
                seen_catch_all = true;
            }
            MatchPattern::IntLiteral(n) => {
                let key = format!("int:{n}");
                if seen_keys.contains(&key) {
                    push_v47(
                        owner,
                        route,
                        &format!("duplicate match arm for integer literal {n}"),
                        diags,
                    );
                } else {
                    seen_keys.push(key);
                }
            }
            MatchPattern::BoolLiteral(b) => {
                let key = format!("bool:{b}");
                if seen_keys.contains(&key) {
                    push_v47(
                        owner,
                        route,
                        &format!("duplicate match arm for boolean literal {b}"),
                        diags,
                    );
                } else {
                    seen_keys.push(key);
                }
            }
            MatchPattern::None => {
                let key = "none".to_string();
                if seen_keys.contains(&key) {
                    push_v47(owner, route, "duplicate match arm for `none`", diags);
                } else {
                    seen_keys.push(key);
                }
            }
            MatchPattern::EnumVariant(en, var) => {
                let key = format!("enum:{en}::{var}");
                if seen_keys.contains(&key) {
                    push_v47(
                        owner,
                        route,
                        &format!("duplicate match arm for `{en}::{var}`"),
                        diags,
                    );
                } else {
                    seen_keys.push(key);
                }
            }
            MatchPattern::EnumVariantWithData(_, _, _) | MatchPattern::Some(_) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// V66: Non-exhaustive match
// ---------------------------------------------------------------------------

pub(super) fn check_match_exhaustive_entity(
    entity: &Entity,
    program: &Program,
    diags: &mut Vec<Diagnostic>,
) {
    let enums: Vec<&EnumDecl> = entity.enums.iter().chain(program.enums.iter()).collect();
    for route in &entity.routes {
        for action in route.body.all_actions() {
            walk_action_exprs(action, &mut |e| {
                check_match_exhaustive_expr(e, &enums, &entity.name, Some(&route.name), diags);
            });
        }
    }
    for member in &entity.members {
        for transform in &member.transforms {
            check_match_exhaustive_expr(
                &transform.body,
                &enums,
                &entity.name,
                Some(&transform.route_name),
                diags,
            );
        }
    }
}

pub(super) fn check_match_exhaustive_pure_fn(
    pure_fn: &PureFn,
    program: &Program,
    diags: &mut Vec<Diagnostic>,
) {
    let enums: Vec<&EnumDecl> = program.enums.iter().collect();
    check_match_exhaustive_expr(&pure_fn.body, &enums, &pure_fn.name, None, diags);
}

fn check_match_exhaustive_expr(
    expr: &Expr,
    enums: &[&EnumDecl],
    owner: &str,
    route: Option<&str>,
    diags: &mut Vec<Diagnostic>,
) {
    if let Expr::Match(_, arms) = expr {
        if let Some(i) = arms
            .iter()
            .position(|a| matches!(a.pattern, MatchPattern::Wildcard | MatchPattern::Ident(_)))
        {
            if arms_cover_all(&arms[..i], enums) {
                push_v47(
                    owner,
                    route,
                    "unreachable catch-all arm: the arms before it already cover every case",
                    diags,
                );
            }
        }
        if let Some(missing) = match_missing_cases(arms, enums) {
            let where_ = match route {
                Some(r) => format!("route '{r}' of entity '{owner}'"),
                None => format!("pure fn '{owner}'"),
            };
            diags.push(Diagnostic::error(
                "V66",
                format!(
                    "non-exhaustive match in {where_}: missing {missing}. Add the missing arms or a `_` arm."
                ),
            ));
        }
    }
    walk_subexprs(expr, &mut |e| {
        check_match_exhaustive_expr(e, enums, owner, route, diags);
    });
}

/// Human-readable description of the uncovered cases, or `None` when the
/// arms are exhaustive (or the scrutinee's enum is unknown here).
fn match_missing_cases(arms: &[MatchArm], enums: &[&EnumDecl]) -> Option<String> {
    if arms
        .iter()
        .any(|a| matches!(a.pattern, MatchPattern::Wildcard | MatchPattern::Ident(_)))
    {
        return None;
    }
    let mut enum_name: Option<&str> = None;
    let mut covered: HashSet<&str> = HashSet::new();
    let (mut has_some, mut has_none, mut has_true, mut has_false, mut has_int) =
        (false, false, false, false, false);
    for arm in arms {
        match &arm.pattern {
            MatchPattern::EnumVariant(en, var) | MatchPattern::EnumVariantWithData(en, var, _) => {
                enum_name = Some(en.as_str());
                covered.insert(var.as_str());
            }
            MatchPattern::Some(_) => has_some = true,
            MatchPattern::None => has_none = true,
            MatchPattern::BoolLiteral(true) => has_true = true,
            MatchPattern::BoolLiteral(false) => has_false = true,
            MatchPattern::IntLiteral(_) => has_int = true,
            MatchPattern::Wildcard | MatchPattern::Ident(_) => {}
        }
    }
    if let Some(en) = enum_name {
        let decl = enums.iter().find(|d| d.name == en)?;
        let missing: Vec<String> = decl
            .variants
            .iter()
            .filter(|v| !covered.contains(v.name.as_str()))
            .map(|v| format!("`{}::{}`", en, v.name))
            .collect();
        return (!missing.is_empty()).then(|| missing.join(", "));
    }
    if has_some || has_none {
        return match (has_some, has_none) {
            (true, false) => Some("`none`".to_string()),
            (false, true) => Some("`some(_)`".to_string()),
            _ => None,
        };
    }
    if has_true || has_false {
        return match (has_true, has_false) {
            (true, false) => Some("`false`".to_string()),
            (false, true) => Some("`true`".to_string()),
            _ => None,
        };
    }
    if has_int {
        return Some("a catch-all arm for the remaining integer values".to_string());
    }
    None
}

/// Whether `arms` (none of them a catch-all) match every value of a
/// scrutinee whose case set is finite and known: all variants of a declared
/// enum, `some` + `none`, or `true` + `false`.
pub(crate) fn arms_cover_all(arms: &[MatchArm], enums: &[&EnumDecl]) -> bool {
    let Some(first) = arms.first() else { return false };
    let finite = match &first.pattern {
        MatchPattern::EnumVariant(en, _) | MatchPattern::EnumVariantWithData(en, _, _) => {
            enums.iter().any(|d| &d.name == en)
        }
        MatchPattern::Some(_) | MatchPattern::None | MatchPattern::BoolLiteral(_) => true,
        _ => false,
    };
    fn irrefutable(p: &Pattern) -> bool {
        match p {
            Pattern::Ident(_) | Pattern::Wildcard => true,
            Pattern::Tuple(ps) => ps.iter().all(irrefutable),
            Pattern::Deref(p) => irrefutable(p),
            Pattern::Some(_) | Pattern::None => false,
        }
    }
    let refutable_payload = arms.iter().any(|a| match &a.pattern {
        MatchPattern::Some(inner) => !irrefutable(inner),
        MatchPattern::EnumVariantWithData(_, _, binds) => !binds.iter().all(irrefutable),
        _ => false,
    });
    finite && !refutable_payload && match_missing_cases(arms, enums).is_none()
}

fn push_v47(owner: &str, route: Option<&str>, detail: &str, diags: &mut Vec<Diagnostic>) {
    let where_ = match route {
        Some(r) => format!("route '{r}' of entity '{owner}'"),
        None => format!("pure fn '{owner}'"),
    };
    diags.push(Diagnostic::warning("V47", format!("{detail} in {where_}")));
}

// ---------------------------------------------------------------------------
// V48: `if` without `else` in value-required contexts
// ---------------------------------------------------------------------------

pub(super) fn check_if_requires_else_entity(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    for route in &entity.routes {
        for action in route.body.all_actions() {
            check_if_requires_else_action(action, &entity.name, Some(&route.name), diags);
        }
    }
    for member in &entity.members {
        for transform in &member.transforms {
            // Transform bodies always produce a value.
            check_if_requires_else_expr(
                &transform.body,
                true,
                &entity.name,
                Some(&transform.route_name),
                diags,
            );
        }
    }
}

pub(super) fn check_if_requires_else_pure_fn(pure_fn: &PureFn, diags: &mut Vec<Diagnostic>) {
    check_if_requires_else_expr(&pure_fn.body, true, &pure_fn.name, None, diags);
}

fn check_if_requires_else_action(
    action: &RouteAction,
    owner: &str,
    route: Option<&str>,
    diags: &mut Vec<Diagnostic>,
) {
    match action {
        RouteAction::Let { value, .. } => {
            check_if_requires_else_expr(value, true, owner, route, diags);
        }
        RouteAction::Return { values } => {
            for v in values {
                check_if_requires_else_expr(v, true, owner, route, diags);
            }
        }
        RouteAction::Send {
            args,
            dest,
            send_options,
            ..
        }
        | RouteAction::VarCall {
            args,
            dest,
            send_options,
            ..
        } => {
            for a in args {
                check_if_requires_else_expr(a, true, owner, route, diags);
            }
            check_if_requires_else_expr(dest, true, owner, route, diags);
            if let Some(opts) = send_options {
                check_if_requires_else_expr(opts, true, owner, route, diags);
            }
        }
        RouteAction::Deploy {
            constructor_args,
            send_options,
            ..
        } => {
            for a in constructor_args {
                check_if_requires_else_expr(a, true, owner, route, diags);
            }
            if let Some(opts) = send_options {
                check_if_requires_else_expr(opts, true, owner, route, diags);
            }
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            // Condition is a value; branches are statement actions (exempt).
            check_if_requires_else_expr(condition, true, owner, route, diags);
            for a in then_actions {
                check_if_requires_else_action(a, owner, route, diags);
            }
            for a in else_actions {
                check_if_requires_else_action(a, owner, route, diags);
            }
        }
        RouteAction::For { iter, body, .. } => {
            check_if_requires_else_expr(iter, true, owner, route, diags);
            for a in body {
                check_if_requires_else_action(a, owner, route, diags);
            }
        }
        RouteAction::Rescue { action: inner, .. } => {
            check_if_requires_else_action(inner, owner, route, diags);
        }
        RouteAction::CallRoute { args, .. }
        | RouteAction::Effect { args, .. }
        | RouteAction::Emit { args, .. }
        | RouteAction::ThrowCustom { args, .. } => {
            for a in args {
                check_if_requires_else_expr(a, true, owner, route, diags);
            }
        }
        RouteAction::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            for a in update_args {
                check_if_requires_else_expr(a, true, owner, route, diags);
            }
            for a in callback_args {
                check_if_requires_else_expr(a, true, owner, route, diags);
            }
        }
        RouteAction::Throw { .. } => {}
    }
}

fn check_if_requires_else_expr(
    expr: &Expr,
    value_ctx: bool,
    owner: &str,
    route: Option<&str>,
    diags: &mut Vec<Diagnostic>,
) {
    match expr {
        Expr::If(c, t, e) => {
            if value_ctx && e.is_none() {
                let where_ = match route {
                    Some(r) => format!("route '{r}' of entity '{owner}'"),
                    None => format!("pure fn '{owner}'"),
                };
                diags.push(Diagnostic::error(
                    "V48",
                    format!(
                        "if-without-else in a value context in {where_}; \
                         add an `else` branch (expression `if` must produce a value)",
                    ),
                ));
            }
            check_if_requires_else_expr(c, true, owner, route, diags);
            check_if_requires_else_expr(t, value_ctx, owner, route, diags);
            if let Some(el) = e {
                check_if_requires_else_expr(el, value_ctx, owner, route, diags);
            }
        }
        Expr::Let(_, value, body) => {
            check_if_requires_else_expr(value, true, owner, route, diags);
            check_if_requires_else_expr(body, value_ctx, owner, route, diags);
        }
        Expr::Block(items) => {
            let n = items.len();
            for (i, item) in items.iter().enumerate() {
                // Only the last expression of a block is the produced value.
                check_if_requires_else_expr(item, value_ctx && i + 1 == n, owner, route, diags);
            }
        }
        Expr::Match(scrut, arms) => {
            check_if_requires_else_expr(scrut, true, owner, route, diags);
            for arm in arms {
                check_if_requires_else_expr(&arm.body, value_ctx, owner, route, diags);
            }
        }
        Expr::BinOp(l, _, r) => {
            check_if_requires_else_expr(l, true, owner, route, diags);
            check_if_requires_else_expr(r, true, owner, route, diags);
        }
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) | Expr::Some(e) => {
            check_if_requires_else_expr(e, true, owner, route, diags);
        }
        Expr::Index(b, k) => {
            check_if_requires_else_expr(b, true, owner, route, diags);
            check_if_requires_else_expr(k, true, owner, route, diags);
        }
        Expr::FnCall(_, args) | Expr::MacroRef(_, args) | Expr::NamespacedCall { args, .. } => {
            for a in args {
                check_if_requires_else_expr(a, true, owner, route, diags);
            }
        }
        Expr::MethodCall(base, _, args) => {
            check_if_requires_else_expr(base, true, owner, route, diags);
            for a in args {
                check_if_requires_else_expr(a, true, owner, route, diags);
            }
        }
        Expr::Tuple(items) => {
            for i in items {
                check_if_requires_else_expr(i, true, owner, route, diags);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                check_if_requires_else_expr(v, true, owner, route, diags);
            }
        }
        Expr::RecordUpdate(base, fields) => {
            check_if_requires_else_expr(base, true, owner, route, diags);
            for (_, v) in fields {
                check_if_requires_else_expr(v, true, owner, route, diags);
            }
        }
        Expr::Range(a, b) => {
            check_if_requires_else_expr(a, true, owner, route, diags);
            check_if_requires_else_expr(b, true, owner, route, diags);
        }
        Expr::For(_, iter, body) => {
            check_if_requires_else_expr(iter, true, owner, route, diags);
            check_if_requires_else_expr(body, true, owner, route, diags);
        }
        Expr::Closure(_, body) => check_if_requires_else_expr(body, true, owner, route, diags),
        Expr::EnumVariantWithData(_, _, args) | Expr::ArrayLit(args) => {
            for a in args {
                check_if_requires_else_expr(a, true, owner, route, diags);
            }
        }
        Expr::AddressOf {
            args, with_params, ..
        } => {
            for a in args {
                check_if_requires_else_expr(a, true, owner, route, diags);
            }
            for (_, v) in with_params {
                check_if_requires_else_expr(v, true, owner, route, diags);
            }
        }
        Expr::Encode { value, .. } => {
            check_if_requires_else_expr(value, true, owner, route, diags);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Shared action/expr walk helpers for V46–V48
// ---------------------------------------------------------------------------

fn walk_action_exprs(action: &RouteAction, f: &mut dyn FnMut(&Expr)) {
    match action {
        RouteAction::Let { value, .. } => f(value),
        RouteAction::Return { values } => {
            for v in values {
                f(v);
            }
        }
        RouteAction::Send {
            args,
            dest,
            send_options,
            ..
        }
        | RouteAction::VarCall {
            args,
            dest,
            send_options,
            ..
        } => {
            for a in args {
                f(a);
            }
            f(dest);
            if let Some(opts) = send_options {
                f(opts);
            }
        }
        RouteAction::Deploy {
            constructor_args,
            send_options,
            ..
        } => {
            for a in constructor_args {
                f(a);
            }
            if let Some(opts) = send_options {
                f(opts);
            }
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            f(condition);
            for a in then_actions {
                walk_action_exprs(a, f);
            }
            for a in else_actions {
                walk_action_exprs(a, f);
            }
        }
        RouteAction::For { iter, body, .. } => {
            f(iter);
            for a in body {
                walk_action_exprs(a, f);
            }
        }
        RouteAction::Rescue { action: inner, .. } => walk_action_exprs(inner, f),
        RouteAction::CallRoute { args, .. }
        | RouteAction::Effect { args, .. }
        | RouteAction::Emit { args, .. }
        | RouteAction::ThrowCustom { args, .. } => {
            for a in args {
                f(a);
            }
        }
        RouteAction::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            for a in update_args {
                f(a);
            }
            for a in callback_args {
                f(a);
            }
        }
        RouteAction::Throw { .. } => {}
    }
}

/// Immediate child expressions only (not the node itself). Used to recurse
/// after handling a node-specific check.
fn walk_subexprs(expr: &Expr, f: &mut dyn FnMut(&Expr)) {
    match expr {
        Expr::BinOp(l, _, r) | Expr::Index(l, r) | Expr::Range(l, r) => {
            f(l);
            f(r);
        }
        Expr::UnaryOp(_, e)
        | Expr::FieldAccess(e, _)
        | Expr::Cast(e, _)
        | Expr::Some(e)
        | Expr::Closure(_, e) => f(e),
        Expr::FnCall(_, args) | Expr::MacroRef(_, args) | Expr::NamespacedCall { args, .. } => {
            for a in args {
                f(a);
            }
        }
        Expr::MethodCall(base, _, args) => {
            f(base);
            for a in args {
                f(a);
            }
        }
        Expr::EnumVariantWithData(_, _, args) | Expr::ArrayLit(args) => {
            for a in args {
                f(a);
            }
        }
        Expr::If(c, t, e) => {
            f(c);
            f(t);
            if let Some(el) = e {
                f(el);
            }
        }
        Expr::Let(_, v, b) => {
            f(v);
            f(b);
        }
        Expr::Match(s, arms) => {
            f(s);
            for arm in arms {
                f(&arm.body);
            }
        }
        Expr::Tuple(items) | Expr::Block(items) => {
            for i in items {
                f(i);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                f(v);
            }
        }
        Expr::RecordUpdate(base, fields) => {
            f(base);
            for (_, v) in fields {
                f(v);
            }
        }
        Expr::For(_, iter, body) => {
            f(iter);
            f(body);
        }
        Expr::AddressOf {
            args, with_params, ..
        } => {
            for a in args {
                f(a);
            }
            for (_, v) in with_params {
                f(v);
            }
        }
        Expr::Encode { value, .. } => f(value),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// V52 — recursive / cyclic records and type aliases
// ---------------------------------------------------------------------------

/// Reject records that participate in a cycle of *strict* field references
/// (UPSTREAM B-22) and type aliases that unfold forever (PW3-O-005).
///
/// For **records**, `Option` / `Vec` / `HashMap` wrappers break the cycle —
/// those types have inhabitable defaults. A bare `record Node { next: Node }`
/// (or A↔B) makes Lean `default_for_type` recurse forever.
///
/// For **aliases**, those wrappers do **not** break the cycle: `type A = Option<A>`
/// still infinite-unfolds in `resolve_user_type`. Alias cycles are rejected first;
/// record checks then unfold remaining (acyclic) aliases so
/// `type A = Node; record Node { next: A }` is still V52.
pub fn check_recursive_records(program: &Program, diags: &mut Vec<Diagnostic>) {
    check_cyclic_aliases(&program.type_aliases, "program", diags);
    let program_alias_map = alias_map(&program.type_aliases);
    check_recursive_records_in(&program.records, &program_alias_map, "program", diags);
    for ent in &program.entities {
        let merged_aliases = merge_aliases(&program.type_aliases, &ent.type_aliases);
        if !ent.type_aliases.is_empty() {
            check_cyclic_aliases(&merged_aliases, &format!("entity '{}'", ent.name), diags);
        }
        let mut universe = program.records.clone();
        for r in &ent.records {
            if let Some(slot) = universe.iter().position(|x| x.name == r.name) {
                universe[slot] = r.clone();
            } else {
                universe.push(r.clone());
            }
        }
        if !ent.records.is_empty() {
            let alias_map = alias_map(&merged_aliases);
            check_recursive_records_in(
                &universe,
                &alias_map,
                &format!("entity '{}'", ent.name),
                diags,
            );
        }
    }
}

fn merge_aliases(program: &[TypeAlias], entity: &[TypeAlias]) -> Vec<TypeAlias> {
    let mut out = program.to_vec();
    for ta in entity {
        if let Some(slot) = out.iter().position(|x| x.name == ta.name) {
            out[slot] = ta.clone();
        } else {
            out.push(ta.clone());
        }
    }
    out
}

fn alias_map(aliases: &[TypeAlias]) -> std::collections::HashMap<String, Type> {
    aliases
        .iter()
        .map(|ta| (ta.name.clone(), ta.ty.clone()))
        .collect()
}

fn check_cyclic_aliases(aliases: &[TypeAlias], scope: &str, diags: &mut Vec<Diagnostic>) {
    if aliases.is_empty() {
        return;
    }
    let names: Vec<String> = aliases.iter().map(|a| a.name.clone()).collect();
    let name_set: HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
    let mut deps: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for a in aliases {
        let mut refs = HashSet::new();
        collect_alias_refs(&a.ty, &name_set, &mut refs);
        let mut edge: Vec<String> = refs.into_iter().collect();
        edge.sort();
        deps.insert(a.name.clone(), edge);
    }
    let sccs = crate::graph::tarjan_sccs(&names, &deps, crate::graph::TarjanOptions::default());
    for scc in sccs {
        let cyclic = if scc.len() > 1 {
            true
        } else if let Some(name) = scc.first() {
            deps.get(name)
                .map(|e| e.iter().any(|x| x == name))
                .unwrap_or(false)
        } else {
            false
        };
        if !cyclic {
            continue;
        }
        diags.push(Diagnostic::error(
            "V52",
            format!(
                "Cyclic type alias in {scope}: {} \
                 — an alias cycle (including through Option/Vec/HashMap) unfolds forever.",
                scc.join(" → "),
            ),
        ));
    }
}

fn collect_alias_refs(ty: &Type, alias_names: &HashSet<&str>, into: &mut HashSet<String>) {
    match ty {
        Type::Simple(n) => {
            if alias_names.contains(n.as_str()) {
                into.insert(n.clone());
            }
        }
        Type::Generic(_, params) => {
            for p in params {
                collect_alias_refs(p, alias_names, into);
            }
        }
        Type::Tuple(parts) => {
            for p in parts {
                collect_alias_refs(p, alias_names, into);
            }
        }
        Type::TypedAddress(_) => {}
    }
}

fn check_recursive_records_in(
    records: &[Record],
    aliases: &std::collections::HashMap<String, Type>,
    scope: &str,
    diags: &mut Vec<Diagnostic>,
) {
    if records.is_empty() {
        return;
    }
    let names: Vec<String> = records.iter().map(|r| r.name.clone()).collect();
    let name_set: HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
    let mut deps: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for r in records {
        let mut refs = HashSet::new();
        for f in &r.fields {
            collect_strict_record_refs(&f.ty, aliases, &mut refs);
        }
        let mut edge: Vec<String> = refs
            .into_iter()
            .filter(|n| name_set.contains(n.as_str()))
            .collect();
        edge.sort();
        deps.insert(r.name.clone(), edge);
    }
    let sccs = crate::graph::tarjan_sccs(&names, &deps, crate::graph::TarjanOptions::default());
    for scc in sccs {
        let cyclic = if scc.len() > 1 {
            true
        } else if let Some(name) = scc.first() {
            deps.get(name)
                .map(|e| e.iter().any(|x| x == name))
                .unwrap_or(false)
        } else {
            false
        };
        if !cyclic {
            continue;
        }
        diags.push(Diagnostic::error(
            "V52",
            format!(
                "Recursive record type without Option/Vec/HashMap indirection in {scope}: {} \
                 — wrap the back-reference in Option (or Vec/HashMap), or the Lean/EVM \
                 backends cannot build a finite default.",
                scc.join(" → "),
            ),
        ));
    }
}

fn unfold_aliases(ty: &Type, aliases: &std::collections::HashMap<String, Type>) -> Type {
    let mut cur = ty.clone();
    for _ in 0..=aliases.len() {
        match &cur {
            Type::Simple(n) if aliases.contains_key(n) => {
                cur = aliases.get(n).cloned().unwrap();
            }
            Type::Generic(n, params) => {
                let unfolded: Vec<Type> =
                    params.iter().map(|p| unfold_aliases(p, aliases)).collect();
                return Type::Generic(n.clone(), unfolded);
            }
            Type::Tuple(parts) => {
                let unfolded: Vec<Type> =
                    parts.iter().map(|p| unfold_aliases(p, aliases)).collect();
                return Type::Tuple(unfolded);
            }
            _ => return cur,
        }
    }
    cur
}

fn collect_strict_record_refs(
    ty: &Type,
    aliases: &std::collections::HashMap<String, Type>,
    into: &mut HashSet<String>,
) {
    let ty = unfold_aliases(ty, aliases);
    match ty {
        Type::Simple(n) => {
            into.insert(n);
        }
        Type::Generic(n, _) if matches!(n.as_str(), "Option" | "Vec" | "HashMap") => {}
        Type::Generic(_, params) => {
            for p in params {
                collect_strict_record_refs(&p, aliases, into);
            }
        }
        Type::Tuple(parts) => {
            for p in parts {
                collect_strict_record_refs(&p, aliases, into);
            }
        }
        Type::TypedAddress(_) => {}
    }
}

// ---------------------------------------------------------------------------
// V53 — nested pattern under `let some(...)` must be a plain identifier
// ---------------------------------------------------------------------------

/// Reject `let some(<pat>)` where `<pat>` is not an identifier / `_`
/// (UPSTREAM B-30). Nested patterns are stringified as binder names
/// (`let Option.none := …`) and silently drop refutability checks.
pub fn check_let_some_nested_patterns(program: &Program, diags: &mut Vec<Diagnostic>) {
    for pure_fn in &program.pure_fns {
        walk_expr_lets_some(&pure_fn.body, &format!("pure fn '{}'", pure_fn.name), diags);
    }
    for entity in &program.entities {
        for route in &entity.routes {
            match &route.body {
                RouteBody::Unphased(actions) => {
                    check_actions_let_some(actions, &format!("route '{}'", route.name), diags);
                }
                RouteBody::Phased(phases) => {
                    for ph in phases {
                        check_actions_let_some(
                            &ph.actions,
                            &format!("route '{}' phase '{}'", route.name, ph.name),
                            diags,
                        );
                    }
                }
                RouteBody::Mixed(phases, actions) => {
                    for ph in phases {
                        check_actions_let_some(
                            &ph.actions,
                            &format!("route '{}' phase '{}'", route.name, ph.name),
                            diags,
                        );
                    }
                    check_actions_let_some(actions, &format!("route '{}'", route.name), diags);
                }
            }
        }
        for m in &entity.members {
            for tr in &m.transforms {
                walk_expr_lets_some(&tr.body, &format!("member '{}' transform", m.name), diags);
            }
            if let Some(d) = &m.default_value {
                walk_expr_lets_some(d, &format!("member '{}' default", m.name), diags);
            }
        }
    }
}

fn check_actions_let_some(actions: &[RouteAction], ctx: &str, diags: &mut Vec<Diagnostic>) {
    for a in actions {
        match a {
            RouteAction::Let { pattern, value } => {
                check_pattern_let_some(pattern, ctx, diags);
                walk_expr_lets_some(value, ctx, diags);
            }
            RouteAction::Conditional {
                condition,
                then_actions,
                else_actions,
            } => {
                walk_expr_lets_some(condition, ctx, diags);
                check_actions_let_some(then_actions, ctx, diags);
                check_actions_let_some(else_actions, ctx, diags);
            }
            RouteAction::For {
                iter,
                body,
                pattern,
            } => {
                walk_expr_lets_some(iter, ctx, diags);
                check_pattern_let_some(pattern, ctx, diags);
                check_actions_let_some(body, ctx, diags);
            }
            RouteAction::Rescue { action, .. } => {
                check_actions_let_some(std::slice::from_ref(action.as_ref()), ctx, diags);
            }
            RouteAction::Return { values } => {
                for v in values {
                    walk_expr_lets_some(v, ctx, diags);
                }
            }
            RouteAction::Send { args, .. }
            | RouteAction::VarCall { args, .. }
            | RouteAction::CallRoute { args, .. }
            | RouteAction::Emit { args, .. }
            | RouteAction::ThrowCustom { args, .. } => {
                for v in args {
                    walk_expr_lets_some(v, ctx, diags);
                }
            }
            RouteAction::Deploy {
                constructor_args, ..
            } => {
                for v in constructor_args {
                    walk_expr_lets_some(v, ctx, diags);
                }
            }
            _ => {}
        }
    }
}

fn check_pattern_let_some(pat: &Pattern, ctx: &str, diags: &mut Vec<Diagnostic>) {
    match pat {
        Pattern::Some(inner) => {
            if !matches!(inner.as_ref(), Pattern::Ident(_) | Pattern::Wildcard) {
                diags.push(Diagnostic::error(
                    "V53",
                    format!(
                        "Nested pattern in `let some(...)` in {ctx} — the inner pattern must be \
                         an identifier (or `_`); nested patterns are not supported and would be \
                         emitted as a binder name.",
                    ),
                ));
            }
            check_pattern_let_some(inner, ctx, diags);
        }
        Pattern::Tuple(parts) => {
            for p in parts {
                check_pattern_let_some(p, ctx, diags);
            }
        }
        Pattern::Deref(inner) => check_pattern_let_some(inner, ctx, diags),
        _ => {}
    }
}

fn walk_expr_lets_some(expr: &Expr, ctx: &str, diags: &mut Vec<Diagnostic>) {
    match expr {
        Expr::Let(pattern, value, body) => {
            check_pattern_let_some(pattern, ctx, diags);
            walk_expr_lets_some(value, ctx, diags);
            walk_expr_lets_some(body, ctx, diags);
        }
        Expr::BinOp(a, _, b) | Expr::Index(a, b) | Expr::Range(a, b) => {
            walk_expr_lets_some(a, ctx, diags);
            walk_expr_lets_some(b, ctx, diags);
        }
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) => {
            walk_expr_lets_some(e, ctx, diags);
        }
        Expr::FnCall(_, args) | Expr::MethodCall(_, _, args) => {
            for a in args {
                walk_expr_lets_some(a, ctx, diags);
            }
        }
        Expr::If(cond, then_b, else_b) => {
            walk_expr_lets_some(cond, ctx, diags);
            walk_expr_lets_some(then_b, ctx, diags);
            if let Some(e) = else_b {
                walk_expr_lets_some(e, ctx, diags);
            }
        }
        Expr::Match(scrutinee, arms) => {
            walk_expr_lets_some(scrutinee, ctx, diags);
            for arm in arms {
                walk_expr_lets_some(&arm.body, ctx, diags);
            }
        }
        Expr::Tuple(parts) | Expr::ArrayLit(parts) => {
            for p in parts {
                walk_expr_lets_some(p, ctx, diags);
            }
        }
        Expr::Closure(_, body) | Expr::For(_, _, body) | Expr::Some(body) => {
            walk_expr_lets_some(body, ctx, diags);
        }
        Expr::Block(stmts) => {
            for s in stmts {
                walk_expr_lets_some(s, ctx, diags);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, e) in fields {
                walk_expr_lets_some(e, ctx, diags);
            }
        }
        Expr::RecordUpdate(base, fields) => {
            walk_expr_lets_some(base, ctx, diags);
            for (_, e) in fields {
                walk_expr_lets_some(e, ctx, diags);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// V65 — arithmetic is undefined on `Option<_>`
// ---------------------------------------------------------------------------

fn type_is_option(ty: &Type) -> bool {
    matches!(ty, Type::Generic(n, _) if n == "Option")
}

fn is_arith_binop(op: &BinOp) -> bool {
    matches!(
        op,
        BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Mod
            | BinOp::WrappingAdd
            | BinOp::WrappingSub
            | BinOp::WrappingMul
    )
}

fn bind_option_ident(pat: &Pattern, value_is_option: bool, env: &mut HashSet<String>) {
    if value_is_option {
        if let Pattern::Ident(n) = pat {
            env.insert(n.clone());
        }
    }
}

fn expr_is_option(expr: &Expr, env: &HashSet<String>) -> bool {
    match expr {
        Expr::Some(_) | Expr::None => true,
        Expr::Ident(n) => env.contains(n),
        Expr::Cast(_, ty) => type_is_option(ty),
        Expr::Let(pat, value, body) => {
            let mut nested = env.clone();
            bind_option_ident(pat, expr_is_option(value, env), &mut nested);
            expr_is_option(body, &nested)
        }
        Expr::Block(stmts) => stmts.last().is_some_and(|e| expr_is_option(e, env)),
        Expr::If(_, t, e) => {
            expr_is_option(t, env) || e.as_ref().is_some_and(|el| expr_is_option(el, env))
        }
        _ => false,
    }
}

fn push_v65(ctx: &str, diags: &mut Vec<Diagnostic>) {
    diags.push(Diagnostic::error(
        "V65",
        format!(
            "{ctx}: arithmetic `+` / `-` / `*` / `/` / `%` (and wrapping variants) is not defined on `Option<_>`"
        ),
    ));
}

fn walk_expr_option_arith(
    expr: &Expr,
    env: &HashSet<String>,
    ctx: &str,
    diags: &mut Vec<Diagnostic>,
) {
    match expr {
        Expr::BinOp(l, op, r) if is_arith_binop(op) => {
            if expr_is_option(l, env) || expr_is_option(r, env) {
                push_v65(ctx, diags);
            }
            walk_expr_option_arith(l, env, ctx, diags);
            walk_expr_option_arith(r, env, ctx, diags);
        }
        Expr::Let(pat, value, body) => {
            walk_expr_option_arith(value, env, ctx, diags);
            let mut nested = env.clone();
            bind_option_ident(pat, expr_is_option(value, env), &mut nested);
            walk_expr_option_arith(body, &nested, ctx, diags);
        }
        Expr::BinOp(l, _, r) | Expr::Index(l, r) | Expr::Range(l, r) => {
            walk_expr_option_arith(l, env, ctx, diags);
            walk_expr_option_arith(r, env, ctx, diags);
        }
        Expr::UnaryOp(_, e)
        | Expr::FieldAccess(e, _)
        | Expr::Cast(e, _)
        | Expr::Some(e)
        | Expr::Closure(_, e) => {
            walk_expr_option_arith(e, env, ctx, diags);
        }
        Expr::For(_, iter, body) => {
            walk_expr_option_arith(iter, env, ctx, diags);
            walk_expr_option_arith(body, env, ctx, diags);
        }
        Expr::Encode { value, .. } => walk_expr_option_arith(value, env, ctx, diags),
        Expr::FnCall(_, args)
        | Expr::MacroRef(_, args)
        | Expr::NamespacedCall { args, .. }
        | Expr::EnumVariantWithData(_, _, args)
        | Expr::Tuple(args)
        | Expr::ArrayLit(args) => {
            for a in args {
                walk_expr_option_arith(a, env, ctx, diags);
            }
        }
        Expr::MethodCall(base, _, args) => {
            walk_expr_option_arith(base, env, ctx, diags);
            for a in args {
                walk_expr_option_arith(a, env, ctx, diags);
            }
        }
        Expr::If(c, t, e) => {
            walk_expr_option_arith(c, env, ctx, diags);
            walk_expr_option_arith(t, env, ctx, diags);
            if let Some(el) = e {
                walk_expr_option_arith(el, env, ctx, diags);
            }
        }
        Expr::Match(scrutinee, arms) => {
            walk_expr_option_arith(scrutinee, env, ctx, diags);
            for arm in arms {
                walk_expr_option_arith(&arm.body, env, ctx, diags);
            }
        }
        Expr::Block(stmts) => {
            let mut nested = env.clone();
            for s in stmts {
                if let Expr::Let(pat, value, body) = s {
                    walk_expr_option_arith(value, &nested, ctx, diags);
                    bind_option_ident(pat, expr_is_option(value, &nested), &mut nested);
                    walk_expr_option_arith(body, &nested, ctx, diags);
                } else {
                    walk_expr_option_arith(s, &nested, ctx, diags);
                }
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, e) in fields {
                walk_expr_option_arith(e, env, ctx, diags);
            }
        }
        Expr::RecordUpdate(base, fields) => {
            walk_expr_option_arith(base, env, ctx, diags);
            for (_, e) in fields {
                walk_expr_option_arith(e, env, ctx, diags);
            }
        }
        _ => {}
    }
}

fn walk_actions_option_arith(
    actions: &[RouteAction],
    env: &mut HashSet<String>,
    ctx: &str,
    diags: &mut Vec<Diagnostic>,
) {
    for action in actions {
        match action {
            RouteAction::Let { pattern, value } => {
                walk_expr_option_arith(value, env, ctx, diags);
                bind_option_ident(pattern, expr_is_option(value, env), env);
            }
            RouteAction::Conditional {
                condition,
                then_actions,
                else_actions,
            } => {
                walk_expr_option_arith(condition, env, ctx, diags);
                walk_actions_option_arith(then_actions, env, ctx, diags);
                walk_actions_option_arith(else_actions, env, ctx, diags);
            }
            RouteAction::For { iter, body, .. } => {
                walk_expr_option_arith(iter, env, ctx, diags);
                walk_actions_option_arith(body, env, ctx, diags);
            }
            RouteAction::Rescue { action: inner, .. } => {
                walk_actions_option_arith(std::slice::from_ref(inner.as_ref()), env, ctx, diags);
            }
            other => walk_action_exprs(other, &mut |e| {
                walk_expr_option_arith(e, env, ctx, diags);
            }),
        }
    }
}

fn seed_option_env(entity: &Entity, params: &[Param]) -> HashSet<String> {
    let mut env = HashSet::new();
    for m in &entity.members {
        if type_is_option(&m.ty) {
            env.insert(m.name.clone());
        }
    }
    for p in params {
        if type_is_option(&p.ty) {
            env.insert(p.name.clone());
        }
    }
    env
}

fn walk_test_steps_option_arith(
    steps: &[TestStep],
    env: &mut HashSet<String>,
    ctx: &str,
    diags: &mut Vec<Diagnostic>,
) {
    for step in steps {
        match step {
            TestStep::Let { name, ty, value } => {
                walk_expr_option_arith(value, env, ctx, diags);
                let is_opt = ty.as_ref().is_some_and(type_is_option) || expr_is_option(value, env);
                if is_opt {
                    env.insert(name.clone());
                }
            }
            TestStep::SetContext { fields, .. } => {
                for (_, e) in fields {
                    walk_expr_option_arith(e, env, ctx, diags);
                }
            }
            TestStep::SetRegistry {
                code_hash,
                code_depth,
                wasm_hash,
                ..
            } => {
                walk_expr_option_arith(code_hash, env, ctx, diags);
                walk_expr_option_arith(code_depth, env, ctx, diags);
                walk_expr_option_arith(wasm_hash, env, ctx, diags);
            }
            TestStep::Call { args, .. } | TestStep::ExpectEmit { args, .. } => {
                for a in args {
                    walk_expr_option_arith(a, env, ctx, diags);
                }
            }
            TestStep::DeployPeer {
                args, init_state, ..
            } => {
                for a in args {
                    walk_expr_option_arith(a, env, ctx, diags);
                }
                for (_, e) in init_state {
                    walk_expr_option_arith(e, env, ctx, diags);
                }
            }
            TestStep::ExpectState { fields } => {
                for (_, e) in fields {
                    walk_expr_option_arith(e, env, ctx, diags);
                }
            }
            TestStep::ExpectReturn { value }
            | TestStep::ExpectReturnLens { value, .. }
            | TestStep::ExpectPred { cond: value }
            | TestStep::Assume { cond: value }
            | TestStep::SkipIf { cond: value }
            | TestStep::AdvanceTime { secs: value } => {
                walk_expr_option_arith(value, env, ctx, diags);
            }
            TestStep::ExpectReturnTuple { values } => {
                for v in values {
                    walk_expr_option_arith(v, env, ctx, diags);
                }
            }
            TestStep::Bound { lo, hi, .. } => {
                walk_expr_option_arith(lo, env, ctx, diags);
                walk_expr_option_arith(hi, env, ctx, diags);
            }
            TestStep::ExpectThrow { .. } | TestStep::ExpectEffects { .. } => {}
        }
    }
}

/// Reject arithmetic on `Option<_>` operands (routes, `pure fn`, spec lets).
pub fn check_option_arithmetic(program: &Program, diags: &mut Vec<Diagnostic>) {
    for pure_fn in &program.pure_fns {
        let mut env = HashSet::new();
        for p in &pure_fn.params {
            if type_is_option(&p.ty) {
                env.insert(p.name.clone());
            }
        }
        walk_expr_option_arith(
            &pure_fn.body,
            &env,
            &format!("pure fn '{}'", pure_fn.name),
            diags,
        );
    }
    for entity in &program.entities {
        for route in &entity.routes {
            let mut env = seed_option_env(entity, &route.params);
            let ctx = format!("entity '{}' route '{}'", entity.name, route.name);
            for w in &route.where_clauses {
                walk_expr_option_arith(&w.condition, &env, &ctx, diags);
                for a in &w.error_args {
                    walk_expr_option_arith(a, &env, &ctx, diags);
                }
            }
            match &route.body {
                RouteBody::Unphased(actions) => {
                    walk_actions_option_arith(actions, &mut env, &ctx, diags);
                }
                RouteBody::Phased(phases) => {
                    for ph in phases {
                        for w in &ph.where_clauses {
                            walk_expr_option_arith(&w.condition, &env, &ctx, diags);
                        }
                        walk_actions_option_arith(&ph.actions, &mut env, &ctx, diags);
                    }
                }
                RouteBody::Mixed(phases, actions) => {
                    for ph in phases {
                        for w in &ph.where_clauses {
                            walk_expr_option_arith(&w.condition, &env, &ctx, diags);
                        }
                        walk_actions_option_arith(&ph.actions, &mut env, &ctx, diags);
                    }
                    walk_actions_option_arith(actions, &mut env, &ctx, diags);
                }
            }
        }
        for m in &entity.members {
            let env = seed_option_env(entity, &[]);
            for tr in &m.transforms {
                walk_expr_option_arith(
                    &tr.body,
                    &env,
                    &format!("entity '{}' member '{}'", entity.name, m.name),
                    diags,
                );
            }
            if let Some(d) = &m.default_value {
                walk_expr_option_arith(
                    d,
                    &env,
                    &format!("entity '{}' member '{}' default", entity.name, m.name),
                    diags,
                );
            }
        }
    }
    for test in &program.tests {
        let mut env = HashSet::new();
        walk_test_steps_option_arith(
            &test.body,
            &mut env,
            &format!("test \"{}\"", test.name),
            diags,
        );
    }
    for fuzz in &program.fuzz_tests {
        let mut env = HashSet::new();
        for p in &fuzz.params {
            if type_is_option(&p.ty) {
                env.insert(p.name.clone());
            }
        }
        walk_test_steps_option_arith(
            &fuzz.body,
            &mut env,
            &format!("fuzz \"{}\"", fuzz.name),
            diags,
        );
    }
    for prop in &program.properties {
        let mut env = HashSet::new();
        for p in &prop.params {
            if type_is_option(&p.ty) {
                env.insert(p.name.clone());
            }
        }
        walk_test_steps_option_arith(
            &prop.body,
            &mut env,
            &format!("property \"{}\"", prop.name),
            diags,
        );
    }
    for inv in &program.invariants {
        let env = HashSet::new();
        let ctx = format!("invariant \"{}\"", inv.name);
        for c in &inv.checks {
            walk_expr_option_arith(c, &env, &ctx, diags);
        }
    }
}

// ---------------------------------------------------------------------------
// V68: record literal fields / V69: mixed-sign arithmetic / V70: transform arity
// ---------------------------------------------------------------------------

fn visit_expr_deep(expr: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(expr);
    walk_subexprs(expr, &mut |e| visit_expr_deep(e, f));
}

/// Every expression owned by `entity`, paired with the route whose
/// parameters are in scope (transforms resolve to their route).
fn for_each_entity_expr<'a>(entity: &'a Entity, f: &mut dyn FnMut(&Expr, Option<&'a Route>)) {
    for route in &entity.routes {
        for wc in &route.where_clauses {
            f(&wc.condition, Some(route));
        }
        if let RouteBody::Phased(phases) | RouteBody::Mixed(phases, _) = &route.body {
            for ph in phases {
                for wc in &ph.where_clauses {
                    f(&wc.condition, Some(route));
                }
            }
        }
        for action in route.body.all_actions() {
            walk_action_exprs(action, &mut |e| f(e, Some(route)));
        }
    }
    for member in &entity.members {
        for t in &member.transforms {
            let route = entity.routes.iter().find(|r| r.name == t.route_name);
            f(&t.body, route);
        }
        if let Some(d) = &member.default_value {
            f(d, None);
        }
    }
    for c in &entity.constants {
        f(&c.value, None);
    }
    for mac in &entity.macros {
        f(&mac.body, None);
    }
}

pub(super) fn check_record_literals_entity(
    entity: &Entity,
    program: &Program,
    diags: &mut Vec<Diagnostic>,
) {
    let records: Vec<&Record> = entity.records.iter().chain(program.records.iter()).collect();
    let where_ = format!("entity '{}'", entity.name);
    for_each_entity_expr(entity, &mut |e, _| {
        visit_expr_deep(e, &mut |x| check_record_literal(x, &records, &where_, diags));
    });
}

pub(super) fn check_record_literals_pure_fn(
    pure_fn: &PureFn,
    program: &Program,
    diags: &mut Vec<Diagnostic>,
) {
    let records: Vec<&Record> = program.records.iter().collect();
    let where_ = format!("pure fn '{}'", pure_fn.name);
    visit_expr_deep(&pure_fn.body, &mut |x| {
        check_record_literal(x, &records, &where_, diags)
    });
}

fn check_record_literal(expr: &Expr, records: &[&Record], where_: &str, diags: &mut Vec<Diagnostic>) {
    let Expr::RecordConstruct(name, fields) = expr else {
        return;
    };
    let Some(rec) = records.iter().find(|r| r.name == *name) else {
        return;
    };
    let mut seen: HashSet<&str> = HashSet::new();
    for (f, _) in fields {
        if !seen.insert(f.as_str()) {
            diags.push(Diagnostic::error(
                "V68",
                format!("{where_}: record literal `{name}` sets field `{f}` more than once"),
            ));
        }
        if !rec.fields.iter().any(|rf| rf.name == *f) {
            diags.push(Diagnostic::error(
                "V68",
                format!("{where_}: record `{name}` has no field `{f}`"),
            ));
        }
    }
    let missing: Vec<&str> = rec
        .fields
        .iter()
        .map(|rf| rf.name.as_str())
        .filter(|n| !seen.contains(n))
        .collect();
    if !missing.is_empty() {
        diags.push(Diagnostic::error(
            "V68",
            format!(
                "{where_}: record literal `{name}` is missing field(s) {}; every field must be set (use `base {{ field: v }}` to update an existing value)",
                missing.iter().map(|m| format!("`{m}`")).collect::<Vec<_>>().join(", ")
            ),
        ));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Signedness {
    Signed,
    Unsigned,
}

fn int_signedness(ty: &Type) -> Option<Signedness> {
    let Type::Simple(n) = ty else {
        return None;
    };
    match n.as_str() {
        "i8" | "i16" | "i32" | "i64" | "i128" | "I256" | "int8" | "int16" | "int32" | "int64"
        | "int128" | "int256" => Some(Signedness::Signed),
        "u8" | "u16" | "u32" | "u64" | "u128" | "U256" | "usize" | "uint8" | "uint16"
        | "uint32" | "uint64" | "uint128" | "uint256" => Some(Signedness::Unsigned),
        _ => None,
    }
}

struct SignEnv<'a> {
    vars: HashMap<String, Type>,
    local_aliases: &'a [TypeAlias],
    program: &'a Program,
}

impl SignEnv<'_> {
    fn unfold(&self, ty: &Type) -> Type {
        unfold_alias(ty, self.local_aliases, &self.program.type_aliases, 0)
    }

    /// Declared type of `expr` when it is fixed by a declaration; literals
    /// and anything inferred from context yield `None`.
    fn declared_type(&self, expr: &Expr) -> Option<Type> {
        match expr {
            Expr::Ident(n) | Expr::TemporalRef(n) => self.vars.get(n).map(|t| self.unfold(t)),
            Expr::Cast(_, ty) => Some(self.unfold(ty)),
            Expr::FnCall(name, _) => self
                .program
                .pure_fns
                .iter()
                .find(|f| f.name == *name)
                .map(|f| self.unfold(&f.return_type)),
            Expr::UnaryOp(UnaryOp::Neg, e) => self.declared_type(e),
            Expr::BinOp(l, op, r) if is_arith_op(op) => {
                self.declared_type(l).or_else(|| self.declared_type(r))
            }
            Expr::Index(base, _) => match self.declared_type(base)? {
                Type::Generic(g, ps) if g == "HashMap" && ps.len() == 2 => Some(self.unfold(&ps[1])),
                Type::Generic(g, ps) if g == "Vec" && ps.len() == 1 => Some(self.unfold(&ps[0])),
                _ => None,
            },
            _ => None,
        }
    }
}

fn is_arith_op(op: &BinOp) -> bool {
    matches!(
        op,
        BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Mod
            | BinOp::WrappingAdd
            | BinOp::WrappingSub
            | BinOp::WrappingMul
    )
}

fn check_mixed_sign(expr: &Expr, env: &SignEnv<'_>, where_: &str, diags: &mut Vec<Diagnostic>) {
    visit_expr_deep(expr, &mut |x| {
        let Expr::BinOp(l, op, r) = x else {
            return;
        };
        if !(is_arith_op(op)
            || matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne))
        {
            return;
        }
        let (Some(lt), Some(rt)) = (env.declared_type(l), env.declared_type(r)) else {
            return;
        };
        let (Some(ls), Some(rs)) = (int_signedness(&lt), int_signedness(&rt)) else {
            return;
        };
        if ls == rs {
            return;
        }
        let unsigned = if ls == Signedness::Unsigned { &lt } else { &rt };
        let Type::Simple(u) = unsigned else {
            return;
        };
        // Mixed operands widen to a signed type of twice the unsigned width
        // (LANGUAGE.md "Automatic Type Widening", rule 2); none exists past
        // 128 bits (rule 3). Comparisons promote to 256-bit signed, which
        // is exact for `u128` but not for `U256`.
        let no_common_type = if is_arith_op(op) {
            matches!(u.as_str(), "u128" | "uint128" | "U256" | "uint256")
        } else {
            matches!(u.as_str(), "U256" | "uint256")
        };
        if no_common_type {
            diags.push(Diagnostic::error(
                "V69",
                format!(
                    "{where_}: operator mixes `{}` and `{}`; no signed type covers both ranges, cast one side explicitly with `as`",
                    pretty_type(&lt),
                    pretty_type(&rt)
                ),
            ));
        }
    });
}

pub(super) fn check_mixed_sign_entity(entity: &Entity, program: &Program, diags: &mut Vec<Diagnostic>) {
    let mut members: HashMap<String, Type> = HashMap::new();
    for m in &entity.members {
        members.insert(m.name.clone(), m.ty.clone());
    }
    let where_ = format!("entity '{}'", entity.name);
    for_each_entity_expr(entity, &mut |e, route| {
        let mut vars = members.clone();
        if let Some(r) = route {
            for p in &r.params {
                vars.insert(p.name.clone(), p.ty.clone());
            }
        }
        let env = SignEnv {
            vars,
            local_aliases: &entity.type_aliases,
            program,
        };
        check_mixed_sign(e, &env, &where_, diags);
    });
}

pub(super) fn check_mixed_sign_pure_fn(pure_fn: &PureFn, program: &Program, diags: &mut Vec<Diagnostic>) {
    let env = SignEnv {
        vars: pure_fn.params.iter().map(|p| (p.name.clone(), p.ty.clone())).collect(),
        local_aliases: &[],
        program,
    };
    check_mixed_sign(&pure_fn.body, &env, &format!("pure fn '{}'", pure_fn.name), diags);
}

pub(super) fn check_transform_arity(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    for member in &entity.members {
        for t in &member.transforms {
            let Some(route) = entity.routes.iter().find(|r| r.name == t.route_name) else {
                continue;
            };
            // `in r()` binds nothing and ignores every parameter.
            if !t.params.is_empty() && t.params.len() != route.params.len() {
                diags.push(
                    Diagnostic::error(
                        "V70",
                        format!(
                            "member '{}' transform `in {}(..)` binds {} parameter(s) but route '{}' declares {}; bind every parameter (use `_` for unused ones) or none",
                            member.name,
                            t.route_name,
                            t.params.len(),
                            route.name,
                            route.params.len()
                        ),
                    )
                    .with_span(t.span),
                );
            }
        }
    }
}
