// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use super::Diagnostic;
use crate::ast::*;
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// W1: Members with no transforms
// ---------------------------------------------------------------------------

pub(super) fn lint_unused_members(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    for member in &entity.members {
        if member.is_identity {
            continue;
        }
        if member.transforms.is_empty() {
            diags.push(Diagnostic::warning(
                "W1",
                format!(
                    "Member '{}' in entity '{}' has no transforms (dead state?)",
                    member.name, entity.name
                ),
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// W2: Unused pure functions — never called from any entity
// ---------------------------------------------------------------------------

pub(super) fn lint_unused_pure_fns(program: &Program, diags: &mut Vec<Diagnostic>) {
    let mut called: HashSet<&str> = HashSet::new();
    for entity in &program.entities {
        collect_fn_calls_entity(entity, &mut called);
    }
    // Pure fns can call other pure fns (e.g. `permit_digest` calling
    // `eip712_domain_typehash`). Without this scan those helpers are
    // wrongly flagged W2 even though they're transitively reachable
    // from an entity.
    for pf in &program.pure_fns {
        collect_fn_calls_expr(&pf.body, &mut called);
    }
    for f in &program.pure_fns {
        if !called.contains(f.name.as_str()) {
            diags.push(Diagnostic::warning(
                "W2",
                format!("Pure function '{}' is never called", f.name),
            ));
        }
    }
}

fn collect_fn_calls_entity<'a>(entity: &'a Entity, called: &mut HashSet<&'a str>) {
    for route in &entity.routes {
        for wc in &route.where_clauses {
            collect_fn_calls_expr(&wc.condition, called);
        }
        for fc in &route.from_clauses {
            for a in &fc.args {
                collect_fn_calls_expr(a, called);
            }
        }
        for action in route.body.all_actions() {
            collect_fn_calls_action(action, called);
        }
    }
    for member in &entity.members {
        for t in &member.transforms {
            collect_fn_calls_expr(&t.body, called);
        }
    }
    for mac in &entity.macros {
        collect_fn_calls_expr(&mac.body, called);
    }
}

fn collect_fn_calls_action<'a>(action: &'a RouteAction, called: &mut HashSet<&'a str>) {
    match action {
        RouteAction::Let { value, .. } => collect_fn_calls_expr(value, called),
        RouteAction::Return { values } => {
            for v in values {
                collect_fn_calls_expr(v, called);
            }
        }
        RouteAction::Send {
            args,
            dest,
            send_options,
            ..
        } => {
            for a in args {
                collect_fn_calls_expr(a, called);
            }
            collect_fn_calls_expr(dest, called);
            if let Some(opts) = send_options {
                collect_fn_calls_expr(opts, called);
            }
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            collect_fn_calls_expr(condition, called);
            for a in then_actions {
                collect_fn_calls_action(a, called);
            }
            for a in else_actions {
                collect_fn_calls_action(a, called);
            }
        }
        RouteAction::Effect { args, .. } => {
            for a in args {
                collect_fn_calls_expr(a, called);
            }
        }
        RouteAction::Deploy {
            send_options,
            constructor_args,
            ..
        } => {
            if let Some(opts) = send_options {
                collect_fn_calls_expr(opts, called);
            }
            for a in constructor_args {
                collect_fn_calls_expr(a, called);
            }
        }
        RouteAction::Rescue { action, .. } => {
            collect_fn_calls_action(action, called);
        }
        RouteAction::Throw { .. } => {}
        RouteAction::ThrowCustom { args, .. } => {
            for a in args {
                collect_fn_calls_expr(a, called);
            }
        }
        RouteAction::CallRoute { args, .. } => {
            for a in args {
                collect_fn_calls_expr(a, called);
            }
        }
        RouteAction::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            for a in update_args {
                collect_fn_calls_expr(a, called);
            }
            for a in callback_args {
                collect_fn_calls_expr(a, called);
            }
        }
        RouteAction::VarCall {
            args,
            dest,
            send_options,
            ..
        } => {
            for a in args {
                collect_fn_calls_expr(a, called);
            }
            collect_fn_calls_expr(dest, called);
            if let Some(opts) = send_options {
                collect_fn_calls_expr(opts, called);
            }
        }
        RouteAction::For { iter, body, .. } => {
            collect_fn_calls_expr(iter, called);
            for a in body {
                collect_fn_calls_action(a, called);
            }
        }
        RouteAction::Emit { args, .. } => {
            for a in args {
                collect_fn_calls_expr(a, called);
            }
        }
    }
}

fn collect_fn_calls_expr<'a>(expr: &'a Expr, called: &mut HashSet<&'a str>) {
    match expr {
        Expr::FnCall(name, args) => {
            called.insert(name.as_str());
            for a in args {
                collect_fn_calls_expr(a, called);
            }
        }
        Expr::BinOp(l, _, r) | Expr::Range(l, r) => {
            collect_fn_calls_expr(l, called);
            collect_fn_calls_expr(r, called);
        }
        Expr::UnaryOp(_, e)
        | Expr::FieldAccess(e, _)
        | Expr::Cast(e, _)
        | Expr::Some(e)
        | Expr::Closure(_, e) => {
            collect_fn_calls_expr(e, called);
        }
        Expr::Index(e, idx) => {
            collect_fn_calls_expr(e, called);
            collect_fn_calls_expr(idx, called);
        }
        Expr::MethodCall(e, _, args) => {
            collect_fn_calls_expr(e, called);
            for a in args {
                collect_fn_calls_expr(a, called);
            }
        }
        Expr::If(cond, t, e) => {
            collect_fn_calls_expr(cond, called);
            collect_fn_calls_expr(t, called);
            if let Some(el) = e {
                collect_fn_calls_expr(el, called);
            }
        }
        Expr::Let(_, val, body) | Expr::For(_, val, body) => {
            collect_fn_calls_expr(val, called);
            collect_fn_calls_expr(body, called);
        }
        Expr::Block(stmts) | Expr::ArrayLit(stmts) => {
            for s in stmts {
                collect_fn_calls_expr(s, called);
            }
        }
        Expr::Tuple(elems) => {
            for e in elems {
                collect_fn_calls_expr(e, called);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                collect_fn_calls_expr(v, called);
            }
        }
        Expr::RecordUpdate(base, fields) => {
            collect_fn_calls_expr(base, called);
            for (_, v) in fields {
                collect_fn_calls_expr(v, called);
            }
        }
        Expr::Match(subject, arms) => {
            collect_fn_calls_expr(subject, called);
            for arm in arms {
                collect_fn_calls_expr(&arm.body, called);
            }
        }
        Expr::MacroRef(_, args) | Expr::EnumVariantWithData(_, _, args) => {
            for a in args {
                collect_fn_calls_expr(a, called);
            }
        }
        Expr::NamespacedCall { args, .. } => {
            for a in args {
                collect_fn_calls_expr(a, called);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// W3: Unused constants — never referenced in entity
// ---------------------------------------------------------------------------

pub(super) fn lint_unused_constants(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    let mut used: HashSet<&str> = HashSet::new();
    collect_fn_calls_entity(entity, &mut HashSet::new());
    collect_idents_entity(entity, &mut used);

    for c in &entity.constants {
        if !used.contains(c.name.as_str()) {
            diags.push(Diagnostic::warning(
                "W3",
                format!(
                    "Constant '{}' in entity '{}' is never used",
                    c.name, entity.name
                ),
            ));
        }
    }
}

fn collect_idents_entity<'a>(entity: &'a Entity, used: &mut HashSet<&'a str>) {
    for route in &entity.routes {
        for wc in &route.where_clauses {
            collect_idents_expr(&wc.condition, used);
        }
        for action in route.body.all_actions() {
            collect_idents_action(action, used);
        }
    }
    for member in &entity.members {
        for t in &member.transforms {
            collect_idents_expr(&t.body, used);
        }
    }
    for mac in &entity.macros {
        collect_idents_expr(&mac.body, used);
    }
}

fn collect_idents_action<'a>(action: &'a RouteAction, used: &mut HashSet<&'a str>) {
    match action {
        RouteAction::Let { value, .. } => collect_idents_expr(value, used),
        RouteAction::Return { values } => {
            for v in values {
                collect_idents_expr(v, used);
            }
        }
        RouteAction::Send {
            args,
            dest,
            send_options,
            ..
        } => {
            for a in args {
                collect_idents_expr(a, used);
            }
            collect_idents_expr(dest, used);
            if let Some(opts) = send_options {
                collect_idents_expr(opts, used);
            }
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            collect_idents_expr(condition, used);
            for a in then_actions {
                collect_idents_action(a, used);
            }
            for a in else_actions {
                collect_idents_action(a, used);
            }
        }
        RouteAction::Effect { args, .. } => {
            for a in args {
                collect_idents_expr(a, used);
            }
        }
        RouteAction::Deploy {
            send_options,
            constructor_args,
            ..
        } => {
            if let Some(opts) = send_options {
                collect_idents_expr(opts, used);
            }
            for a in constructor_args {
                collect_idents_expr(a, used);
            }
        }
        RouteAction::Rescue { action, .. } => {
            collect_idents_action(action, used);
        }
        RouteAction::Throw { .. } => {}
        RouteAction::ThrowCustom { args, .. } => {
            for a in args {
                collect_idents_expr(a, used);
            }
        }
        RouteAction::CallRoute { args, .. } => {
            for a in args {
                collect_idents_expr(a, used);
            }
        }
        RouteAction::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            for a in update_args {
                collect_idents_expr(a, used);
            }
            for a in callback_args {
                collect_idents_expr(a, used);
            }
        }
        RouteAction::VarCall {
            args,
            dest,
            send_options,
            ..
        } => {
            for a in args {
                collect_idents_expr(a, used);
            }
            collect_idents_expr(dest, used);
            if let Some(opts) = send_options {
                collect_idents_expr(opts, used);
            }
        }
        RouteAction::For { iter, body, .. } => {
            collect_idents_expr(iter, used);
            for a in body {
                collect_idents_action(a, used);
            }
        }
        RouteAction::Emit { args, .. } => {
            for a in args {
                collect_idents_expr(a, used);
            }
        }
    }
}

fn collect_idents_expr<'a>(expr: &'a Expr, used: &mut HashSet<&'a str>) {
    match expr {
        Expr::Ident(name) => {
            used.insert(name.as_str());
        }
        Expr::FnCall(name, args) => {
            used.insert(name.as_str());
            for a in args {
                collect_idents_expr(a, used);
            }
        }
        Expr::BinOp(l, _, r) | Expr::Range(l, r) => {
            collect_idents_expr(l, used);
            collect_idents_expr(r, used);
        }
        Expr::UnaryOp(_, e)
        | Expr::FieldAccess(e, _)
        | Expr::Cast(e, _)
        | Expr::Some(e)
        | Expr::Closure(_, e) => {
            collect_idents_expr(e, used);
        }
        Expr::Index(e, idx) => {
            collect_idents_expr(e, used);
            collect_idents_expr(idx, used);
        }
        Expr::MethodCall(e, _, args) => {
            collect_idents_expr(e, used);
            for a in args {
                collect_idents_expr(a, used);
            }
        }
        Expr::If(cond, t, e) => {
            collect_idents_expr(cond, used);
            collect_idents_expr(t, used);
            if let Some(el) = e {
                collect_idents_expr(el, used);
            }
        }
        Expr::Let(_, val, body) | Expr::For(_, val, body) => {
            collect_idents_expr(val, used);
            collect_idents_expr(body, used);
        }
        Expr::Block(stmts) | Expr::ArrayLit(stmts) | Expr::Tuple(stmts) => {
            for s in stmts {
                collect_idents_expr(s, used);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                collect_idents_expr(v, used);
            }
        }
        Expr::RecordUpdate(base, fields) => {
            collect_idents_expr(base, used);
            for (_, v) in fields {
                collect_idents_expr(v, used);
            }
        }
        Expr::Match(subject, arms) => {
            collect_idents_expr(subject, used);
            for arm in arms {
                collect_idents_expr(&arm.body, used);
            }
        }
        Expr::MacroRef(_, args) | Expr::EnumVariantWithData(_, _, args) => {
            for a in args {
                collect_idents_expr(a, used);
            }
        }
        Expr::NamespacedCall { args, .. } => {
            for a in args {
                collect_idents_expr(a, used);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// W9: Entity-local record / enum / type alias shadows a program-scope name
// ---------------------------------------------------------------------------

pub(super) fn lint_duplicate_user_types(program: &Program, diags: &mut Vec<Diagnostic>) {
    for entity in &program.entities {
        for rec in &entity.records {
            if program.records.iter().any(|p| p.name == rec.name) {
                diags.push(Diagnostic::warning(
                    "W9",
                    format!(
                        "Entity '{}' declares record '{}' that already exists at program scope. These are distinct types; lift the nested declaration or drop the program-scope copy so they share one type.",
                        entity.name, rec.name
                    ),
                ));
            }
        }
        for en in &entity.enums {
            if program.enums.iter().any(|p| p.name == en.name) {
                diags.push(Diagnostic::warning(
                    "W9",
                    format!(
                        "Entity '{}' declares enum '{}' that already exists at program scope. These are distinct types; lift the nested declaration or drop the program-scope copy so they share one type.",
                        entity.name, en.name
                    ),
                ));
            }
        }
        for ta in &entity.type_aliases {
            if program.type_aliases.iter().any(|p| p.name == ta.name) {
                diags.push(Diagnostic::warning(
                    "W9",
                    format!(
                        "Entity '{}' declares type alias '{}' that already exists at program scope. These are distinct types; lift the nested declaration or drop the program-scope copy so they share one type.",
                        entity.name, ta.name
                    ),
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// W10: Mutating route updates balance/allowance ledger state but has no
// `emit` in the route body (SD-03 observability hygiene; EVM domain only).
// ---------------------------------------------------------------------------

fn type_is_u256(ty: &Type) -> bool {
    matches!(ty, Type::Simple(s) if s == "U256" || s == "uint256")
}

fn type_is_u256_hashmap(ty: &Type) -> bool {
    match ty {
        Type::Generic(name, args) if name == "HashMap" && args.len() == 2 => {
            type_is_u256(&args[1])
        }
        _ => false,
    }
}

fn is_ledger_member(member: &Member) -> bool {
    let n = member.name.to_ascii_lowercase();
    if !n.contains("balance") && !n.contains("allowance") {
        return false;
    }
    type_is_u256(&member.ty) || type_is_u256_hashmap(&member.ty)
}

fn route_body_has_emit(body: &RouteBody) -> bool {
    body.all_actions()
        .iter()
        .any(|a| matches!(a, RouteAction::Emit { .. }))
}

/// Routes that touch ledger members via member transforms but omit `emit`.
pub(super) fn lint_missing_emit_on_ledger_routes(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    if entity.events.is_empty() {
        return;
    }

    let mut ledger_routes: HashSet<String> = HashSet::new();
    for member in &entity.members {
        if !is_ledger_member(member) {
            continue;
        }
        for t in &member.transforms {
            ledger_routes.insert(t.route_name.clone());
        }
    }
    if ledger_routes.is_empty() {
        return;
    }

    for route in &entity.routes {
        if route.is_view || route.recover_tag.is_some() {
            continue;
        }
        if !ledger_routes.contains(&route.name) {
            continue;
        }
        if route_body_has_emit(&route.body) {
            continue;
        }
        diags.push(Diagnostic::warning(
            "W10",
            format!(
                "Entity '{}' route '{}' updates balance/allowance member state but has no `emit` in the route body. Member transforms do not emit logs — add an explicit `emit` for observability (see LANGUAGE.md § Member-centric entities and events).",
                entity.name, route.name
            ),
        ));
    }
}

// ---------------------------------------------------------------------------
// W11: Init-route call immediately followed by msg { sender } — ctor runs
// under the prior MsgCtx (often default sender = 0). Put msg before call.
// ---------------------------------------------------------------------------

pub(super) fn lint_ctor_before_msg(
    label: &str,
    entity: &Entity,
    steps: &[TestStep],
    diags: &mut Vec<Diagnostic>,
) {
    if crate::spec_normalize::has_ctor_before_msg_pattern(entity, steps) {
        diags.push(Diagnostic::warning(
            "W11",
            format!(
                "{}: `call <init-route>` is immediately followed by `msg {{ sender: … }}`; the init route runs under the current message context (often default `sender = 0`). Put `msg {{ sender: … }}` before `call constructor()` / the init route.",
                label
            ),
        ));
    }
}
