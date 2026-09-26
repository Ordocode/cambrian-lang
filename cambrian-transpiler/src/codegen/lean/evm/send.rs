// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean codegen — same-entity sends (P3.8).
//!
//! Lowers the two send shapes the Cambrian language exposes inside
//! route bodies:
//!
//! * **`~> dest [with { value: V }]`** (untyped value transfer, no
//!   `var` capture) → `Cambrian.WorldState.transfer w from dest V`.
//!   `dest` is *any* address-valued expression; no entity-resolution
//!   is required. The transfer returns `Except TransferError World`;
//!   we either propagate via `←` (route has a fail surface), or
//!   silently fall back to the unchanged `w` (best-effort under the
//!   abstract atomic model — gas-style failures aren't modelled in
//!   P3, see [docs/PLAN_LEAN_TARGET.md](../../docs/PLAN_LEAN_TARGET.md) §P3.8).
//! * **`var x = msg(args) ~> dest` / `send msg(args) ~> dest`** —
//!   typed self-calls. P3 only supports the **same-entity** case:
//!   `dest` must statically resolve to `<Self>.address(...)` with
//!   identity arguments expressible at codegen time. Lowers to
//!   `let (w, x) := <Self>.Routes.<msg> w id' (ctx with sender := <Self>.address inst) args`.
//!   Anything else falls through to validator rule `L8` (see
//!   [`super::super::core::emitter`] dispatch in `lean_route.rs`).
//!
//! The helpers here are pure expression-string builders. Body
//! threading (writeback `s → w` before a send, re-read `s := w.storage.<e> inst`
//! after) lives in [`super::super::route`].

use std::collections::HashMap;

use crate::ast::{Entity, Expr, Pattern, Program, Route, RouteAction, RouteBody, Type};

use super::super::expr::{
    bind_message_args_for_total_call, coerce_arg, coerce_term_to_width, gen_expr, LeanExprCtx,
};
use super::world::entity_field_name;

// Re-exports for adapter-internal call sites (P2 Step A).
pub(crate) use crate::analysis::{classify_dest, SendTarget};

/// Widen a Cambrian send-`value` expression to `Cambrian.U256`.
/// Numeric literals are typed via `Cambrian.U256` directly (Lean's
/// `OfNat` does the rest); non-literal expressions are widened via
/// `BitVec.zeroExtend` from their inferred source width.
pub(crate) fn widen_send_value(value: &Expr, ctx: &LeanExprCtx<'_>) -> String {
    let term = gen_expr(value, ctx);
    // Under `numerics: nat` the value is a `Nat`; native balances stay
    // `Cambrian.U256` (`BitVec 256`), so inject via `ofNat`.
    if ctx.type_ctx.use_nat_numerics {
        return format!("(BitVec.ofNat 256 ({}))", term);
    }
    coerce_term_to_width(value, &term, 256, ctx)
}

/// Pull the `value` field out of a send-options record `{ value: V, … }`.
/// Returns `None` when options are absent or have no `value` field.
pub(crate) fn extract_send_value(send_options: Option<&Expr>) -> Option<Expr> {
    let opts = send_options?;
    match opts {
        Expr::RecordConstruct(_, fields) => fields
            .iter()
            .find(|(k, _)| k == "value")
            .map(|(_, v)| v.clone()),
        _ => None,
    }
}

fn coerce_expr_to_type(expr: &Expr, ty: &Type, ctx: &LeanExprCtx<'_>) -> String {
    let term = gen_expr(expr, ctx);
    // UPSTREAM B-19 residual: range-loop binders are `Nat` (`List.range`).
    // Identity fields and route params are typically `BitVec 64`; without
    // `BitVec.ofNat` Lean tries `HAdd Nat Nat (BitVec 64)` at the call site.
    if matches!(expr, Expr::Ident(n) if ctx.nat_idents.contains(n)) {
        coerce_arg(expr, &term, ty, ctx)
    } else {
        term
    }
}

/// Per-param variant of [`coerce_expr_to_type`] over *pre-rendered* arg terms
/// (the FARC arg sequencing renders terms inside
/// `bind_message_args_for_total_call`, so the coercion must run on the
/// rendered term — a plain render on the pure path, a `__p{i}` binder on the
/// fail path).
fn coerce_route_arg_terms(
    route: &Route,
    args: &[Expr],
    terms: &[String],
    ctx: &LeanExprCtx<'_>,
) -> Vec<String> {
    terms
        .iter()
        .zip(args.iter())
        .enumerate()
        .map(|(i, (term, arg))| match route.params.get(i) {
            Some(p) if matches!(arg, Expr::Ident(n) if ctx.nat_idents.contains(n)) => {
                coerce_arg(arg, term, &p.ty, ctx)
            }
            _ => term.clone(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// True when `route` contains at least one world-effect action (`Send`,
/// `VarCall`, `deploy`, …) **or** an expression that has no State/MsgCtx
/// fallback and needs `w` in scope (`sys::balance` / `sys::blockNumber` /
/// `sys::chainid`) in its top-level (unphased) body.
pub(crate) fn route_has_unphased_sends(route: &Route) -> bool {
    crate::analysis::route_has_unphased_sends(route)
}

/// True when *any* phase in a phased route needs World threading.
pub(crate) fn route_phased_needs_world_thread(route: &Route) -> bool {
    crate::analysis::route_phased_needs_world_thread(route)
}

/// True when any route on `entity` contains a typed send / var-call
/// whose destination resolves to an `extern entity` route (lowered via
/// the `Cambrian.Generated.Extern` axioms). Used to decide whether the
/// entity's `Routes` module must `import Cambrian.Generated.Extern`.
pub(crate) fn route_set_calls_extern(program: &Program, entity: &Entity) -> bool {
    for route in &entity.routes {
        let empty = HashMap::new();
        let found = match &route.body {
            RouteBody::Unphased(actions) => {
                actions_call_extern(actions, program, entity, route, &empty)
            }
            RouteBody::Phased(phases) => phases
                .iter()
                .any(|phase| actions_call_extern(&phase.actions, program, entity, route, &empty)),
            RouteBody::Mixed(phases, trailing) => {
                phases.iter().any(|phase| {
                    actions_call_extern(&phase.actions, program, entity, route, &empty)
                }) || actions_call_extern(trailing, program, entity, route, &empty)
            }
        };
        if found {
            return true;
        }
    }
    false
}

fn actions_call_extern(
    actions: &[RouteAction],
    program: &Program,
    entity: &Entity,
    route: &Route,
    outer_let_env: &HashMap<String, Expr>,
) -> bool {
    let mut let_env = outer_let_env.clone();
    for action in actions {
        let calls_extern = match action {
            RouteAction::Send {
                message: Some(message),
                dest,
                ..
            }
            | RouteAction::VarCall { message, dest, .. } => matches!(
                crate::analysis::classify_message_dest(
                    dest, message, entity, route, program, &let_env,
                ),
                SendTarget::ExternEntity { .. }
            ),
            RouteAction::Conditional {
                then_actions,
                else_actions,
                ..
            } => {
                actions_call_extern(then_actions, program, entity, route, &let_env)
                    || actions_call_extern(else_actions, program, entity, route, &let_env)
            }
            RouteAction::For { body, .. } => {
                actions_call_extern(body, program, entity, route, &let_env)
            }
            RouteAction::Rescue { action, .. } => actions_call_extern(
                std::slice::from_ref(action.as_ref()),
                program,
                entity,
                route,
                &let_env,
            ),
            _ => false,
        };
        if calls_extern {
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

// ---------------------------------------------------------------------------
// Target classification — owned by `crate::analysis::send_target` (P2).
// ---------------------------------------------------------------------------

/// EVM `payable` surface: `accept`, `receive` / `fallback`, or any route
/// that reads `msg::value` (T-ARCH-013 / W2-BC-07b).
fn route_accepts_native_value(route: &Route, entity: &Entity) -> bool {
    route.is_accept
        || route.name == "receive"
        || route.name == "fallback"
        || crate::analysis::route_uses_msg_value(route, entity)
}

/// Throw code for value-bearing typed sends to non-payable callees.
const NON_PAYABLE_VALUE_THROW: u32 = 13;

/// Shared lowering for typed calls (same- or cross-entity).
#[allow(clippy::too_many_arguments)]
fn lower_typed_var_call(
    bind_name: &str,
    target_entity: &str,
    target_route: &Route,
    args: &[Expr],
    id_args: &[Expr],
    caller_entity: &Entity,
    target_entity_def: &Entity,
    ctx: &LeanExprCtx<'_>,
    callee_fail: bool,
    caller_fail: bool,
    is_view: bool,
    send_value: Option<&Expr>,
) -> String {
    let bind_owned = super::super::core::types::lean_safe_bind(bind_name);
    let bind_name = bind_owned.as_str();
    let caller_field = entity_field_name(&caller_entity.name);
    let target_field = entity_field_name(target_entity);
    let id_members: Vec<&crate::ast::Member> = target_entity_def
        .members
        .iter()
        .filter(|m| m.is_identity)
        .collect();

    let id_record = if id_members.is_empty() {
        "{}".to_string()
    } else {
        let pieces: Vec<String> = id_members
            .iter()
            .zip(id_args.iter())
            .map(|(member, expr)| {
                format!(
                    "{} := {}",
                    member.name,
                    coerce_expr_to_type(expr, &member.ty, ctx)
                )
            })
            .collect();
        format!("{{ {} }}", pieces.join(", "))
    };

    // A send that carries `{ value: V }` must expose `V` to the callee as
    // `msg::value` (EVM credits `msg.value` to the callee). Thread the value
    // into the callee's `ctx` via `MsgCtx.withValue`, not only into the ledger
    // `callParams.value` — otherwise the callee sees `msg::value == 0`
    // (T-X-006 / LEAN-H4).
    let value_term = send_value.map(|v| widen_send_value(v, ctx));
    let ctx_swap = match &value_term {
        Some(vt) => format!(
            "let ctx' := Cambrian.MsgCtx.withValue {{ ctx with sender := {}.address inst }} ({})",
            caller_entity.name, vt,
        ),
        None => format!(
            "let ctx' := {{ ctx with sender := {}.address inst }}",
            caller_entity.name,
        ),
    };
    let call_core = bind_message_args_for_total_call(args, ctx, |terms| {
        let coerced = coerce_route_arg_terms(target_route, args, terms, ctx);
        let arg_suffix = if coerced.is_empty() {
            String::new()
        } else {
            format!(" {}", coerced.join(" "))
        };
        format!(
            "{}.Routes.{} w id' ctx'{}",
            target_entity, target_route.name, arg_suffix,
        )
    });
    let writeback = format!(
        "let w := Cambrian.Generated.World.with{} w inst s",
        caller_entity.name,
    );
    let id_let = format!("let id' : {}.Identity := {}", target_entity, id_record,);
    let reread = format!("let s := w.storage.{} inst", caller_field);
    let is_self = caller_entity.name == target_entity;

    // Fire-and-forget typed sends to non-payable callees reject at the call
    // site (T-ARCH-013 / H-AX-01-02). Capturing `var x = … with { value }`
    // routes through `WorldState.call` so PM-039 can bind the returned pair
    // monadically even when the callee does not read `msg::value`.
    if send_value.is_some()
        && !route_accepts_native_value(target_route, target_entity_def)
        && bind_name == "_"
        && !ctx.type_ctx.use_predictable_profile
    {
        return format!(
            "{}\n{}\n{}\nthrow (Cambrian.ThrowCode.ofNat {})",
            writeback,
            id_let,
            ctx_swap,
            NON_PAYABLE_VALUE_THROW,
        );
    }

    let bind_term = if send_value.is_none() {
        match (is_view, callee_fail, caller_fail) {
            (true, false, _) => format!("let (w, {}) := {}", bind_name, call_core),
            (false, false, _) => format!("let w := {}\nlet {} : Unit := ()", call_core, bind_name,),
            (true, true, true) => format!("let (w, {}) ← {}", bind_name, call_core),
            (false, true, true) => {
                format!("let w ← {}\nlet {} : Unit := ()", call_core, bind_name,)
            }
            // Same-entity silent recovery is a validator contract breach (L9/L11):
            // emit a non-compiling sentinel instead of `exceptGetD`.
            (true, true, false) if is_self => {
                silent_self_fail_sentinel(bind_name, &target_route.name, true)
            }
            (false, true, false) if is_self => {
                silent_self_fail_sentinel(bind_name, &target_route.name, false)
            }
            (true, true, false) => format!(
                "let (w, {}) := Cambrian.exceptGetD ({}) (w, default)",
                bind_name, call_core,
            ),
            (false, true, false) => format!(
                "let w := Cambrian.exceptGetD ({}) w\nlet {} : Unit := ()",
                call_core, bind_name,
            ),
        }
    } else {
        let value_term = value_term.clone().expect("send_value present");
        let call_params = format!(
            "let callParams : Cambrian.WorldState.CallParams := {{ to := {}.address id', src := {}.address inst, value := {} }}",
            target_entity, caller_entity.name, value_term,
        );
        // `WorldState.call` requires `body : World → Except ε (World × α)`.
        // View callees already return `World × T`; non-view fallible callees
        // return bare `World` and must be paired with `()` (B-11).
        let call_body = if callee_fail && is_view {
            format!("fun w => {}", call_core)
        } else if callee_fail {
            format!("fun w => ({}).map (fun w' => (w', ()))", call_core)
        } else if is_view {
            format!(
                "fun w => (Except.ok ({}) : Except Cambrian.ThrowCode _)",
                call_core
            )
        } else {
            format!(
                "fun w => (Except.ok ({}, ()) : Except Cambrian.ThrowCode _)",
                call_core
            )
        };
        match (is_view, callee_fail, caller_fail) {
            // Total view returns `World × T`; fallback must be `(w, default)`,
            // not `(w, ())` (UPSTREAM B-32). Non-view arms keep `(w, ())`
            // because B-11 pairs bare `World` with `Unit`.
            (true, false, _) => format!(
                "{}\nlet (w, {}) := Cambrian.exceptGetD (Cambrian.WorldState.call w callParams {}) (w, default)",
                call_params, bind_name, call_body,
            ),
            // Non-view callees return `World`; `WorldState.call` requires
            // `body : World → Except ε (World × α)`. Use the prepared
            // `call_body` (`Except.ok (call_core, ())`) and bind `.fst`.
            (false, false, _) => format!(
                "{}\nlet w := (Cambrian.exceptGetD (Cambrian.WorldState.call w callParams {}) (w, ())).fst\nlet {} : Unit := ()",
                call_params, call_body, bind_name,
            ),
            (true, true, true) => format!(
                "{}\nlet (w, {}) ← Cambrian.WorldState.call w callParams {}",
                call_params, bind_name, call_body,
            ),
            (false, true, true) => format!(
                "{}\nlet (w, _) ← Cambrian.WorldState.call w callParams {}\nlet {} : Unit := ()",
                call_params, call_body, bind_name,
            ),
            (true, true, false) if is_self => {
                silent_self_fail_sentinel(bind_name, &target_route.name, true)
            }
            (false, true, false) if is_self => {
                silent_self_fail_sentinel(bind_name, &target_route.name, false)
            }
            (true, true, false) => format!(
                "{}\nlet (w, {}) := Cambrian.exceptGetD (Cambrian.WorldState.call w callParams {}) (w, default)",
                call_params, bind_name, call_body,
            ),
            (false, true, false) => format!(
                "{}\nlet w := (Cambrian.exceptGetD (Cambrian.WorldState.call w callParams {}) (w, ())).fst\nlet {} : Unit := ()",
                call_params, call_body, bind_name,
            ),
        }
    };

    let _ = target_field;
    format!(
        "{}\n{}\n{}\n{}\n{}",
        writeback, id_let, ctx_swap, bind_term, reread,
    )
}

/// Non-compiling sentinel for same-entity failing calls that would otherwise
/// recover via `Cambrian.exceptGetD` (L9 capture / L11 fire-and-forget).
fn silent_self_fail_sentinel(bind_name: &str, route_name: &str, is_view: bool) -> String {
    if bind_name != "_" {
        // L9: capturing var-call from a failing route without rescue / fail surface.
        if is_view {
            format!(
                "-- L9: capture from failing route '{}' without fail surface / rescue\n\
                 let (w, {}) := __L9_capture_from_failing_route_without_rescue",
                route_name, bind_name,
            )
        } else {
            format!(
                "-- L9: capture from failing route '{}' without fail surface / rescue\n\
                 let w := __L9_capture_from_failing_route_without_rescue\n\
                 let {} : Unit := ()",
                route_name, bind_name,
            )
        }
    } else {
        // L11: fire-and-forget self-send to a failing route without fail surface.
        format!(
            "-- L11: internal send to failing route '{}' without fail surface\n\
             let w := __L11_internal_call_to_failing_route_without_fail_surface",
            route_name,
        )
    }
}

// ---------------------------------------------------------------------------
// Lowering — raw value transfer
// ---------------------------------------------------------------------------

/// Lower a `~> dest with { value: V }` send (no `var` capture, no
/// `message`). Produces a Lean term that consumes the current
/// `(w, s)` and yields a new `(w', s')` pair:
///
/// * `s` is written back into `w` first (so the transfer sees any
///   intermediate state),
/// * `Cambrian.WorldState.transfer` runs,
/// * the resulting `w'` is unpacked; `s` is re-read so subsequent
///   transforms see the updated balance map.
///
/// When `fail_mode` is true, the transfer's `Except` propagates via
/// `←` and ThrowCode `90` is used for any `TransferError`. Value-bearing
/// raw transfers always participate in fail-mode (`action_can_throw`),
/// matching EVM's revert-on-underfund (T-X-002 / LEAN-H3) — never the
/// silent `toOption.getD w` recovery.
pub(crate) fn lower_raw_transfer(
    entity: &Entity,
    dest: &Expr,
    value: Option<&Expr>,
    ctx: &LeanExprCtx<'_>,
    fail_mode: bool,
) -> String {
    let field = entity_field_name(&entity.name);
    let dest_term = gen_expr(dest, ctx);
    // `value` is whatever the Cambrian author wrote (`m_balance` →
    // `BitVec 128`, a `u64` literal, …). `WorldState.transfer`
    // expects `Cambrian.U256` (`BitVec 256`). Always widen via
    // `.zeroExtend 256` so the call site is well-typed regardless of
    // the source width. Literal `0` short-circuits to `0#256`.
    let value_term = match value {
        Some(v) => widen_send_value(v, ctx),
        None => "0#256".to_string(),
    };
    let sender_term = format!("{}.address inst", entity.name);
    let writeback = format!(
        "let w := Cambrian.Generated.World.with{} w inst s",
        entity.name,
    );
    let reread = format!("let s := w.storage.{} inst", field);

    // Always fail-propagate: underfunded transfer must revert like EVM.
    let _ = fail_mode;
    format!(
        "{}\n\
         let w ← (match Cambrian.WorldState.transfer w ({}) ({}) ({}) with\n\
            | .ok w' => Except.ok w'\n\
            | .error _ => throw (Cambrian.ThrowCode.ofNat 90))\n\
         {}",
        writeback, sender_term, dest_term, value_term, reread,
    )
}

// ---------------------------------------------------------------------------
// Lowering — typed self-call (var-capture form)
// ---------------------------------------------------------------------------

/// Lower `var x = msg(args) ~> <Self>.address(id_args)` to a typed
/// self-call. Caller must have already classified `dest` as
/// `SendTarget::SameEntity` and resolved `target_route` against the
/// entity's route list.
///
/// The emitted snippet:
///
/// * writes back `s → w`,
/// * builds `let id' : <Self>.Identity := { … }` from `id_args` (or
///   `{}` for singletons),
/// * calls `<Self>.Routes.<msg> w id' ctx' args` with `ctx'` having
///   its sender swapped to `<Self>.address inst`,
/// * destructures the result into `(w, x)` (view routes) or `(w, _)`
///   (pure mutators — `x` is bound to `()`),
/// * re-reads `s := w.storage.<entity> inst`.
///
/// When `target_route` can fail (`route_fail_mode(target_route)`),
/// the destructuring uses the `←` Except plumbing (the enclosing
/// route must therefore also be in `fail_mode`; `L9` enforces this
/// at the validator level).
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_self_var_call(
    bind_name: &str,
    target_route: &Route,
    args: &[Expr],
    id_args: &[Expr],
    entity: &Entity,
    ctx: &LeanExprCtx<'_>,
    callee_fail: bool,
    caller_fail: bool,
    is_view: bool,
    send_value: Option<&Expr>,
) -> String {
    lower_typed_var_call(
        bind_name,
        &entity.name,
        target_route,
        args,
        id_args,
        entity,
        entity,
        ctx,
        callee_fail,
        caller_fail,
        is_view,
        send_value,
    )
}

/// Lower a typed send / var-call to an `extern entity` route. The
/// foreign contract is modelled by the opaque
/// `Cambrian.Generated.Extern.<E>.Routes.<msg>` axiom
/// (`Except ThrowCode (World × Ret)` / `Except ThrowCode World` — PN-106),
/// so a foreign revert can abort the caller. The destination address is
/// *not* an argument of the axiom (keyed by entity+route name only), so
/// `dest` is intentionally ignored here.
///
/// `callee_fail` is always true for extern. Bind with `←` when the caller
/// is a fail surface; never `exceptGetD` (that would reintroduce PN-106).
pub(crate) fn lower_extern_call(
    bind_name: &str,
    ext_entity: &str,
    msg: &str,
    args: &[Expr],
    ctx: &LeanExprCtx<'_>,
    caller_fail: bool,
    is_view: bool,
) -> String {
    let bind_owned = super::super::core::types::lean_safe_bind(bind_name);
    let bind_name = bind_owned.as_str();
    let call_core = bind_message_args_for_total_call(args, ctx, |terms| {
        // Keep the pre-FARC per-arg parens: legacy emission is byte-for-byte
        // (` (term)` per arg), and compound args stay precedence-safe.
        let arg_suffix: String = terms.iter().map(|t| format!(" ({t})")).collect();
        format!(
            "Cambrian.Generated.Extern.{}.Routes.{} w ctx{}",
            ext_entity, msg, arg_suffix
        )
    });
    match (is_view, caller_fail) {
        (true, true) if bind_name == "_" => {
            format!("let (w, _) ← {}\n", call_core)
        }
        (true, true) => {
            format!("let (w, {}) ← {}\n", bind_name, call_core)
        }
        (false, true) => {
            if bind_name == "_" {
                format!("let w ← {}\n", call_core)
            } else {
                format!(
                    "let w ← {}\nlet {} : Unit := ()\n",
                    call_core, bind_name
                )
            }
        }
        (_, false) => format!(
            "-- PN-106: extern call without caller fail surface (overlay bug)\n\
             let {} := __PN106_extern_needs_fail_surface ({})\n",
            if bind_name == "_" { "w".to_string() } else { bind_name.to_string() },
            call_core
        ),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_cross_var_call(
    bind_name: &str,
    target_entity: &str,
    target_route: &Route,
    args: &[Expr],
    id_args: &[Expr],
    caller_entity: &Entity,
    target_entity_def: &Entity,
    ctx: &LeanExprCtx<'_>,
    callee_fail: bool,
    caller_fail: bool,
    is_view: bool,
    send_value: Option<&Expr>,
) -> String {
    lower_typed_var_call(
        bind_name,
        target_entity,
        target_route,
        args,
        id_args,
        caller_entity,
        target_entity_def,
        ctx,
        callee_fail,
        caller_fail,
        is_view,
        send_value,
    )
}

/// Lower `call routeName(args)` — a synchronous, **same-instance**,
/// **same-`ctx`** invocation of one of this entity's own routes
/// (usually `private`). Unlike a typed send there is no message
/// dispatch, no identity record, no `ctx.sender` swap, and no value
/// transfer: the callee runs against the *current* `inst` and `ctx`,
/// in this contract's own code. The emitted snippet writes `s` back to
/// `w`, invokes `<Self>.Routes.<name>`, then re-reads `s` so following
/// actions see the post-call state. Any return value is discarded
/// (`call` is a statement, not a capturing expression).
///
/// The fail/view matrix mirrors [`lower_typed_var_call`]: a failing
/// callee propagates via `←` when the caller is itself a fail surface,
/// otherwise it is recovered best-effort through `Cambrian.exceptGetD`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_call_route(
    target_route: &Route,
    args: &[Expr],
    entity: &Entity,
    ctx: &LeanExprCtx<'_>,
    callee_fail: bool,
    caller_fail: bool,
    is_view: bool,
) -> String {
    let field = entity_field_name(&entity.name);
    let call_core = bind_message_args_for_total_call(args, ctx, |terms| {
        let coerced = coerce_route_arg_terms(target_route, args, terms, ctx);
        let arg_suffix = if coerced.is_empty() {
            String::new()
        } else {
            format!(" {}", coerced.join(" "))
        };
        format!("{}.Routes.{} w inst ctx{}", entity.name, target_route.name, arg_suffix)
    });
    let writeback = format!(
        "let w := Cambrian.Generated.World.with{} w inst s",
        entity.name,
    );
    let reread = format!("let s := w.storage.{} inst", field);
    let bind_term = match (is_view, callee_fail, caller_fail) {
        (false, false, _) => format!("let w := {}", call_core),
        (true, false, _) => format!("let (w, _) := {}", call_core),
        (false, true, true) => format!("let w ← {}", call_core),
        (true, true, true) => format!("let (w, _) ← {}", call_core),
        // L11: `call` to a failing route without fail surface must not
        // compile with silent `exceptGetD` recovery.
        (false, true, false) | (true, true, false) => format!(
            "-- L11: internal call to failing route '{}' without fail surface\n\
             let w := __L11_internal_call_to_failing_route_without_fail_surface",
            target_route.name,
        ),
    };
    format!("{}\n{}\n{}", writeback, bind_term, reread)
}

pub(crate) fn resolve_route<'a>(entity: &'a Entity, message: &str) -> Option<&'a Route> {
    entity.routes.iter().find(|r| r.name == message)
}

/// `receive` preferred over `fallback` for plain value transfers (EVM P0-E).
pub(crate) fn resolve_payable_entry_route(entity: &Entity) -> Option<&Route> {
    entity
        .routes
        .iter()
        .find(|r| r.name == "receive")
        .or_else(|| entity.routes.iter().find(|r| r.name == "fallback"))
}

/// Lower `~> Entity.address(...) with { value }` when the target entity
/// declares `receive` / `fallback`. Routes through `WorldState.call` into
/// the payable entry route (W2-BC-02 / T-ARCH-026, 031).
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_raw_value_receive(
    program: &Program,
    caller_entity: &Entity,
    target: &SendTarget,
    value: &Expr,
    ctx: &LeanExprCtx<'_>,
    caller_fail: bool,
) -> Option<String> {
    let (target_name, id_args) = match target {
        SendTarget::CrossEntity { entity, id_args } => (entity.as_str(), id_args),
        SendTarget::SameEntity { id_args } => (caller_entity.name.as_str(), id_args),
        _ => return None,
    };
    let target_ent = program
        .entities
        .iter()
        .find(|e| e.name == target_name)?;
    let recv = resolve_payable_entry_route(target_ent)?;
    let callee_fail = super::super::route::route_fail_mode(program, target_ent, recv);
    let is_view = super::super::route::route_is_view(recv);
    Some(if target_name == caller_entity.name {
        lower_self_var_call(
            "_",
            recv,
            &[],
            id_args,
            caller_entity,
            ctx,
            callee_fail,
            caller_fail,
            is_view,
            Some(value),
        )
    } else {
        lower_cross_var_call(
            "_",
            target_name,
            recv,
            &[],
            id_args,
            caller_entity,
            target_ent,
            ctx,
            callee_fail,
            caller_fail,
            is_view,
            Some(value),
        )
    })
}

/// Resolve a same-entity self-call's target route.
pub(crate) fn resolve_self_route<'a>(entity: &'a Entity, message: &str) -> Option<&'a Route> {
    resolve_route(entity, message)
}
