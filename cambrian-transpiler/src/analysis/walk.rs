// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Shared route-action traversal frame.
//!
//! Enumerates nested actions (`Conditional` / `Rescue` / `For` / phases)
//! in one place so a new `RouteAction` variant is handled once.

use crate::ast::{Expr, RouteAction, RouteBody};

/// Apply `f` to every action reachable from `body` (including nested
/// actions inside `Conditional` / `Rescue` / `For`).
pub fn for_each_route_action(body: &RouteBody, mut f: impl FnMut(&RouteAction)) {
    match body {
        RouteBody::Unphased(actions) | RouteBody::Mixed(_, actions) => {
            for action in actions {
                for_each_action(action, &mut f);
            }
        }
        RouteBody::Phased(phases) => {
            for phase in phases {
                for action in &phase.actions {
                    for_each_action(action, &mut f);
                }
            }
        }
    }
}

/// Apply `f` to `action` and every nested action it contains.
pub fn for_each_action(action: &RouteAction, f: &mut impl FnMut(&RouteAction)) {
    f(action);
    match action {
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            for a in then_actions {
                for_each_action(a, f);
            }
            for a in else_actions {
                for_each_action(a, f);
            }
        }
        RouteAction::Rescue { action, .. } => {
            for_each_action(action, f);
        }
        RouteAction::For { body, .. } => {
            for a in body {
                for_each_action(a, f);
            }
        }
        _ => {}
    }
}

/// Apply `f` to every root expression held directly by `action`
/// (not nested actions' expressions — use [`for_each_action`] + this).
pub fn for_each_action_root_expr(action: &RouteAction, mut f: impl FnMut(&Expr)) {
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
            for a in args {
                f(a);
            }
            f(dest);
            if let Some(opts) = send_options {
                f(opts);
            }
        }
        RouteAction::Conditional { condition, .. } => f(condition),
        RouteAction::Return { values } => {
            for e in values {
                f(e);
            }
        }
        RouteAction::Let { value, .. } => f(value),
        RouteAction::Effect { args, .. }
        | RouteAction::CallRoute { args, .. }
        | RouteAction::ThrowCustom { args, .. }
        | RouteAction::Emit { args, .. } => {
            for a in args {
                f(a);
            }
        }
        RouteAction::Deploy {
            send_options,
            constructor_args,
            ..
        } => {
            if let Some(opts) = send_options {
                f(opts);
            }
            for a in constructor_args {
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
        RouteAction::For { iter, .. } => f(iter),
        RouteAction::Rescue { .. } | RouteAction::Throw { .. } => {}
    }
}

/// True iff `walk` matches any root expression on `action` or any nested
/// action (`Conditional` / `Rescue` / `For`). The `walk` closure owns
/// recursive descent into sub-expressions.
pub fn action_walks_any(action: &RouteAction, walk: &dyn Fn(&Expr) -> bool) -> bool {
    let mut found = false;
    for_each_action(action, &mut |a| {
        if found {
            return;
        }
        for_each_action_root_expr(a, |e| {
            if walk(e) {
                found = true;
            }
        });
    });
    found
}
