// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean codegen — dynamic-address dispatch for `~> dest` / `var x = msg(args) ~> dest`
//! where `dest` is an `address`-typed ident (member or route parameter)
//! and we cannot statically pick an `(entity, identity)` to thread the
//! call through `Other.Routes.<msg> w id ctx args …`.
//!
//! Two situations are handled:
//!
//! * **`DynamicTyped` (`Address<E>`)** — `dest` carries enough type info
//!   to pin the callee entity `E`, but the identity args aren't available
//!   at codegen time. We model this as an opaque axiomatic call into
//!   `Cambrian.Generated.Dispatch.<E>.<route>`, mirroring `lean_extern`.
//!
//! * **`DynamicUntyped` (`address`)** — `dest` is an opaque address; we
//!   pick the first in-program route signature that matches the message
//!   name (the cross-entity ERC20-style "trait" pattern; both projects
//!   currently use this for token calls inside UniswapV2Pair). The
//!   axiom name is `Cambrian.Generated.Dispatch.Untyped.<msg>` and the
//!   first param is the opaque address. The validator (L8) keeps the
//!   "no matching route" case as an error.
//!
//! All emitted snippets follow the same convention as
//! [`lean_send`]: a single `let`-chain that writes `s` back into `w`,
//! runs the opaque call, and re-reads `s` from the updated world.

use std::collections::BTreeSet;

use crate::ast::{Entity, Expr, Program, Route};

use super::super::core::types::{lower_type, LeanTypeCtx};
use super::super::expr::{bind_message_args_for_total_call, LeanExprCtx};
use super::super::LeanProfile;
use super::send::widen_send_value;
use super::world::entity_field_name;

/// Lower a `Send` / `VarCall` whose dest is an `Address<E>` ident for
/// an in-program entity `E`. Emits an opaque-axiom call that:
///
///   1. writes back `s → w`,
///   2. evaluates `Cambrian.Generated.Dispatch.<E>.<msg> w <dest> ctx args…`,
///   3. re-reads `s := w.storage.<caller> inst`.
///
/// Axioms / wrappers return `Except ThrowCode (World × T)` (void:
/// `Except ThrowCode World`). The caller is a fail surface; bind with
/// `←`. Value transfer always fail-propagates (never `getD`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_dynamic_send(
    bind_name: &str,
    target_entity: &str,
    target_route: Option<&Route>,
    msg_name: &str,
    args: &[Expr],
    dest_ident: &str,
    caller_entity: &Entity,
    ctx: &LeanExprCtx<'_>,
    caller_fail: bool,
    send_value: Option<&Expr>,
) -> String {
    let bind_owned = super::super::core::types::lean_safe_bind(bind_name);
    let bind_name = bind_owned.as_str();
    let caller_field = entity_field_name(&caller_entity.name);

    let writeback = format!(
        "let w := Cambrian.Generated.World.with{} w inst s",
        caller_entity.name,
    );
    let reread = format!("let s := w.storage.{} inst", caller_field);

    let value_term = send_value
        .map(|v| widen_send_value(v, ctx))
        .unwrap_or_else(|| "0#256".to_string());
    let dest_term = resolve_ident(dest_ident, caller_entity, ctx);

    // When the dispatch carries `{ value: V }`, EVM actually moves ETH from the
    // caller to the destination as part of the CALL. The opaque `Dispatch.*`
    // axiom only models the callee's own state change, so we must debit the
    // caller / credit the destination explicitly via `WorldState.transfer`
    // before the dispatch — otherwise the Lean ledger diverges from EVM ground
    // truth (T-X-005 / LEAN-H7).
    let transfer = if send_value.is_some() {
        let sender_term = format!("{}.address inst", caller_entity.name);
        format!(
            "let w ← (match Cambrian.WorldState.transfer w ({}) ({}) ({}) with\n\
             | .ok w' => Except.ok w'\n\
             | .error _ => throw (Cambrian.ThrowCode.ofNat 90))\n",
            sender_term, dest_term, value_term,
        )
    } else {
        String::new()
    };

    let call_core = bind_message_args_for_total_call(args, ctx, |terms| {
        let arg_suffix = if terms.is_empty() {
            String::new()
        } else {
            format!(" {}", terms.join(" "))
        };
        format!(
            "Cambrian.Generated.Dispatch.{}.{} w {} {} ctx{}",
            target_entity, msg_name, dest_term, value_term, arg_suffix,
        )
    });
    let is_view = target_route.is_some_and(|r| r.return_type.is_some());
    let bind_term = dispatch_bind_term(bind_name, &call_core, is_view, caller_fail);

    format!("{}\n{}{}\n{}", writeback, transfer, bind_term, reread)
}

/// Resolve a Cambrian ident (member / route param / `let`) to the
/// matching Lean term using the same priority order as
/// [`crate::codegen::lean::expr::gen_ident`].
fn resolve_ident(name: &str, entity: &Entity, ctx: &LeanExprCtx<'_>) -> String {
    if ctx.lets.contains(name) || ctx.route_params.contains(name) {
        return super::super::core::types::lean_safe_ident(name);
    }
    if let Some(member) = entity.members.iter().find(|m| m.name == name) {
        if member.is_identity {
            if let Some(inst) = &ctx.instance_var {
                return format!("{}.{}", inst, name);
            }
        }
        return format!("{}.{}", ctx.state_var, name);
    }
    if entity.constants.iter().any(|c| c.name == name) {
        return format!("{}.{}", entity.name, name);
    }
    super::super::core::types::lean_safe_ident(name)
}

/// Lower a `Send` / `VarCall` whose dest is a plain `address` ident.
/// Picks the first in-program route signature matching `msg_name` to
/// determine the return type. Emits a call into
/// `Cambrian.Generated.Dispatch.Untyped.<msg>` — the corresponding
/// axiom is synthesised by [`gen_dispatch_module`] below.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_dynamic_send_untyped(
    bind_name: &str,
    msg_name: &str,
    args: &[Expr],
    dest_ident: &str,
    caller_entity: &Entity,
    ctx: &LeanExprCtx<'_>,
    caller_fail: bool,
    send_value: Option<&Expr>,
    program: &Program,
) -> String {
    let bind_owned = super::super::core::types::lean_safe_bind(bind_name);
    let bind_name = bind_owned.as_str();
    let caller_field = entity_field_name(&caller_entity.name);

    let writeback = format!(
        "let w := Cambrian.Generated.World.with{} w inst s",
        caller_entity.name,
    );
    let reread = format!("let s := w.storage.{} inst", caller_field);

    let value_term = send_value
        .map(|v| widen_send_value(v, ctx))
        .unwrap_or_else(|| "0#256".to_string());
    let dest_term = resolve_ident(dest_ident, caller_entity, ctx);

    let transfer = if send_value.is_some() {
        let sender_term = format!("{}.address inst", caller_entity.name);
        format!(
            "let w ← (match Cambrian.WorldState.transfer w ({}) ({}) ({}) with\n\
             | .ok w' => Except.ok w'\n\
             | .error _ => throw (Cambrian.ThrowCode.ofNat 90))\n",
            sender_term, dest_term, value_term,
        )
    } else {
        String::new()
    };

    let call_core = bind_message_args_for_total_call(args, ctx, |terms| {
        let arg_suffix = if terms.is_empty() {
            String::new()
        } else {
            format!(" {}", terms.join(" "))
        };
        format!(
            "Cambrian.Generated.Dispatch.Untyped.{} w {} {} ctx{}",
            msg_name, dest_term, value_term, arg_suffix,
        )
    });
    let is_view = first_matching_route(program, msg_name)
        .is_some_and(|(_, r)| r.return_type.is_some());
    let bind_term = dispatch_bind_term(bind_name, &call_core, is_view, caller_fail);

    format!("{}\n{}{}\n{}", writeback, transfer, bind_term, reread)
}

fn dispatch_bind_term(bind_name: &str, call: &str, is_view: bool, caller_fail: bool) -> String {
    match (is_view, caller_fail) {
        (true, true) if bind_name == "_" => format!("let (w, _) ← {}", call),
        (true, true) => format!("let (w, {}) ← {}", bind_name, call),
        (false, true) if bind_name == "_" => format!("let w ← {}", call),
        (false, true) => format!("let w ← {}\nlet {} : Unit := ()", call, bind_name),
        (_, false) => format!(
            "-- G-004: dynamic dispatch without caller fail surface\n\
             let {} := __G004_dispatch_needs_fail_surface ({})",
            if bind_name == "_" {
                "w".to_string()
            } else {
                bind_name.to_string()
            },
            call
        ),
    }
}

/// Emit the per-program `Cambrian/Generated/Dispatch.lean` module
/// wrapping in-program callee routes as `Except` (G-004 / PN-106 mirror).
pub fn gen_dispatch_module(program: &Program, profile: LeanProfile) -> String {
    let (typed, untyped) = collect_dispatch_sites(program);
    if typed.is_empty() && untyped.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str("/-\n  Auto-generated dispatch table for dynamic-address sends.\n");
    out.push_str("  Each entry models `~> dest` / `var x = msg(args) ~> dest` where `dest`\n");
    out.push_str("  is an `address`-typed value the codegen couldn't pin to a concrete\n");
    out.push_str("  `(entity, identity)` at compile time. Wrappers return Except so CALL\n");
    out.push_str("  revert (where/throw / underfunded value) fail-closes like EVM.\n-/\n\n");
    out.push_str("import Cambrian.Prelude\n");
    out.push_str("import Cambrian.Generated.World\n\n");
    out.push_str("set_option linter.unusedVariables false\n\n");
    out.push_str("namespace Cambrian.Generated.Dispatch\n\n");

    for (entity_name, route_name) in &typed {
        emit_typed_wrapper(&mut out, program, entity_name, route_name, profile);
    }

    if !untyped.is_empty() {
        out.push_str("namespace Untyped\n\n");
        for msg in &untyped {
            emit_untyped_wrapper(&mut out, program, msg, profile);
        }
        out.push_str("end Untyped\n\n");
    }

    out.push_str("end Cambrian.Generated.Dispatch\n");
    out
}

fn emit_typed_wrapper(
    out: &mut String,
    program: &Program,
    entity_name: &str,
    route_name: &str,
    profile: LeanProfile,
) {
    let Some(entity) = program.entities.iter().find(|e| e.name == entity_name) else {
        return;
    };
    let Some(route) = entity.routes.iter().find(|r| r.name == route_name) else {
        return;
    };
    let ctx = LeanTypeCtx::for_entity(
        program,
        &entity.name,
        &entity.records,
        &entity.enums,
        &entity.type_aliases,
        profile,
    );
    let kw = if dispatch_computable_fail(program, entity, route) {
        "def"
    } else {
        "opaque"
    };
    out.push_str(&format!("namespace {}\n", entity_name));
    out.push_str(&format!(
        "  /-- Dynamic dispatch for `{}.{}` against an `Address<{}>` (Except; may revert). -/\n  {} {} (w : Cambrian.Generated.World)\n             (dest : Cambrian.Address)\n             (value : Cambrian.U256)\n             (ctx : Cambrian.MsgCtx)",
        entity_name, route_name, entity_name, kw, route_name,
    ));
    for p in &route.params {
        out.push_str(&format!(" ({} : {})", p.name, lower_type(&p.ty, &ctx)));
    }
    out.push_str(&format!(
        "\n      : Except Cambrian.ThrowCode ({})",
        dispatch_result_ty(route, &ctx),
    ));
    if kw == "def" {
        out.push_str(" :=\n  throw (Cambrian.ThrowCode.ofNat 1)\n");
    } else {
        out.push('\n');
    }
    out.push_str(&format!("end {}\n\n", entity_name));
}

fn emit_untyped_wrapper(out: &mut String, program: &Program, msg: &str, profile: LeanProfile) {
    let Some((entity, route)) = first_matching_route(program, msg) else {
        out.push_str(&format!(
            "  -- {}: no in-program route matched; axiom omitted\n\n",
            msg,
        ));
        return;
    };
    let ctx = LeanTypeCtx::for_entity(
        program,
        &entity.name,
        &entity.records,
        &entity.enums,
        &entity.type_aliases,
        profile,
    );
    let kw = if dispatch_computable_fail(program, entity, route) {
        "def"
    } else {
        "opaque"
    };
    out.push_str(&format!(
        "  /-- Dynamic dispatch for `{}` (signature from `{}.{}`; Except; may revert). -/\n  {} {} (w : Cambrian.Generated.World)\n             (dest : Cambrian.Address)\n             (value : Cambrian.U256)\n             (ctx : Cambrian.MsgCtx)",
        msg, entity.name, route.name, kw, msg,
    ));
    for p in &route.params {
        out.push_str(&format!(" ({} : {})", p.name, lower_type(&p.ty, &ctx)));
    }
    out.push_str(&format!(
        "\n      : Except Cambrian.ThrowCode ({})",
        dispatch_result_ty(route, &ctx),
    ));
    if kw == "def" {
        out.push_str(" :=\n  throw (Cambrian.ThrowCode.ofNat 1)\n\n");
    } else {
        out.push_str("\n\n");
    }
}

fn dispatch_result_ty(route: &Route, ctx: &LeanTypeCtx<'_>) -> String {
    match &route.return_type {
        Some(t) => format!("Cambrian.Generated.World × {}", lower_type(t, ctx)),
        None => "Cambrian.Generated.World".to_string(),
    }
}

/// Void callees whose `where` is literally `false` get a computational
/// `throw` so `#eval` can observe revert (G-004 failAlways / failDeposit).
/// Other callees stay `opaque` — cannot import `*Routes` here without a
/// lake cycle (ERC20Routes already imports Dispatch).
fn dispatch_computable_fail(_program: &Program, _entity: &Entity, route: &Route) -> bool {
    route.return_type.is_none()
        && route
            .where_clauses
            .iter()
            .any(|w| matches!(w.condition, Expr::BoolLiteral(false)))
}

/// Walk every route of every entity to find dispatch sites:
///   * typed: `(entity_name, route_name)` for every `Address<E>` dest
///     with `msg(args) ~> dest` (or `~> dest` with a typed call).
///   * untyped: distinct message names targeting plain `address` idents.
fn collect_dispatch_sites(program: &Program) -> (BTreeSet<(String, String)>, BTreeSet<String>) {
    let mut typed = BTreeSet::new();
    let mut untyped = BTreeSet::new();
    for entity in &program.entities {
        for route in &entity.routes {
            walk_route(route, entity, program, &mut typed, &mut untyped);
        }
    }
    (typed, untyped)
}

/// True when `route` contains a typed or untyped dynamic-address send
/// (`Address<E>` / plain `address` dest). Those sites go through
/// `Dispatch.*` Except wrappers, so the caller is a fail surface (G-004).
pub(crate) fn route_has_dynamic_dispatch(
    program: &Program,
    entity: &Entity,
    route: &Route,
) -> bool {
    crate::analysis::route_has_dynamic_dispatch(program, entity, route)
}

fn walk_route(
    route: &Route,
    entity: &Entity,
    program: &Program,
    typed: &mut BTreeSet<(String, String)>,
    untyped: &mut BTreeSet<String>,
) {
    use crate::ast::RouteBody;
    match &route.body {
        RouteBody::Unphased(actions) | RouteBody::Mixed(_, actions) => {
            for a in actions {
                walk_action(a, entity, route, program, typed, untyped);
            }
        }
        RouteBody::Phased(phases) => {
            for p in phases {
                for a in &p.actions {
                    walk_action(a, entity, route, program, typed, untyped);
                }
            }
        }
    }
}

fn walk_action(
    action: &crate::ast::RouteAction,
    entity: &Entity,
    route: &Route,
    program: &Program,
    typed: &mut BTreeSet<(String, String)>,
    untyped: &mut BTreeSet<String>,
) {
    use crate::ast::RouteAction;
    match action {
        RouteAction::Send {
            message: Some(msg),
            dest,
            ..
        }
        | RouteAction::VarCall {
            message: msg, dest, ..
        } if matches!(action, RouteAction::Send { .. })
            || matches!(action, RouteAction::VarCall { .. }) =>
        {
            // Unified handling: extract dispatch flavour via classify_dest.
            let _ = msg;
            match super::send::classify_dest(dest, entity, route, program) {
                super::send::SendTarget::DynamicTyped { entity: ent, .. } => {
                    if let RouteAction::Send {
                        message: Some(m), ..
                    } = action
                    {
                        typed.insert((ent, m.clone()));
                    } else if let RouteAction::VarCall { message, .. } = action {
                        typed.insert((ent, message.clone()));
                    }
                }
                super::send::SendTarget::DynamicUntyped { .. } => {
                    if let RouteAction::Send {
                        message: Some(m), ..
                    } = action
                    {
                        untyped.insert(m.clone());
                    } else if let RouteAction::VarCall { message, .. } = action {
                        untyped.insert(message.clone());
                    }
                }
                _ => {}
            }
        }
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            for a in then_actions {
                walk_action(a, entity, route, program, typed, untyped);
            }
            for a in else_actions {
                walk_action(a, entity, route, program, typed, untyped);
            }
        }
        RouteAction::Rescue { action: inner, .. } => {
            walk_action(inner, entity, route, program, typed, untyped);
        }
        RouteAction::For { body, .. } => {
            for a in body {
                walk_action(a, entity, route, program, typed, untyped);
            }
        }
        _ => {}
    }
}

fn first_matching_route<'a>(program: &'a Program, msg: &str) -> Option<(&'a Entity, &'a Route)> {
    for e in &program.entities {
        if let Some(r) = e.routes.iter().find(|r| r.name == msg) {
            return Some((e, r));
        }
    }
    None
}
