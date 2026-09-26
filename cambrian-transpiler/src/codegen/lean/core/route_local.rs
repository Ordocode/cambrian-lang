// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! LeanCore state-local route builders (P4 step F / Local transitions).
//!
//! Owns artifacts that operate on `s : State` without mentioning `World`:
//! `_pre_*` Bool predicates, named `<E>.Local.<route>` / `<E>.Local.<route>_<phase>`
//! defs, transform application (`{ s with … }`), and the inner route body
//! consumed by the Lean-EVM adapter's thin `Routes.*` wrappers. Capture-as-
//! parameter plumbing goes through
//! [`phase_captured_vars_before`](super::super::member::phase_captured_vars_before).
//!
//! World wrapping, send/deploy interleaving, fuel/mutual SCC policy, and
//! public `gen_routes_module` orchestration stay in the adapter (`lean/route.rs`).

use std::collections::{HashMap, HashSet};

use crate::ast::{
    Entity, Expr, FromClauseKind, PhaseBlock, Program, Route, RouteAction, Type, WhereClause,
};

use super::super::expr::{
    arith_result_width, coerce_result_to_width, expr_needs_world, gen_expr, LeanExprCtx,
};
use super::super::LeanProfile;
use super::emitter::{doc_comment, push_indent};
use super::hashmap::hashmap_transform_fields;
use super::map_analysis::{member_is_hashmap, member_needs_keys_list};
use super::stmt::Carrier;
use super::types::{lower_type, LeanTypeCtx};
use crate::codegen::{collect_transforms, order_transforms_temporally, ResolvedTransform};

// ---------------------------------------------------------------------------
// From-clause checks + arg helpers (state-local)
// ---------------------------------------------------------------------------

/// Non-det `from Entity(m_member)` on EVM compares `msg.sender` to the stored
/// **address** slot when `m_member : address`. Numeric identity members must
/// lower via `Entity.address` (CREATE2), not `ctx.sender == s.<u64>` (BitVec
/// vs `Address` — lean_p3 / det_locker).
fn from_arg_member_is_address_type(ty: &Type) -> bool {
    matches!(ty, Type::Simple(s) if s == "address") || matches!(ty, Type::TypedAddress(_))
}

/// Lower one `from` clause to a sender check (`ctx.sender == …`).
pub(crate) fn from_clause_sender_check(
    program: &Program,
    entity: &Entity,
    clause: &crate::ast::FromClause,
    ctx: &LeanExprCtx<'_>,
    profile: LeanProfile,
    eq: &str,
) -> String {
    match clause.kind {
        FromClauseKind::Member => format!("ctx.sender {} s.{}", eq, clause.entity_name),
        FromClauseKind::Entity => {
            if !profile.deterministic_addresses && clause.args.len() == 1 {
                if let Expr::Ident(name) = &clause.args[0] {
                    if let Some(member) = entity.members.iter().find(|m| m.name == *name) {
                        if from_arg_member_is_address_type(&member.ty) {
                            return format!("ctx.sender {} s.{}", eq, name);
                        }
                    }
                }
            }
            let target = &clause.entity_name;
            if let Some(target_ent) = program.entities.iter().find(|e| e.name == *target) {
                let id_fields: Vec<String> = target_ent
                    .members
                    .iter()
                    .filter(|m| m.is_identity)
                    .map(|m| m.name.clone())
                    .collect();
                let id_record = if id_fields.is_empty() {
                    "{}".to_string()
                } else {
                    let pieces: Vec<String> = id_fields
                        .iter()
                        .zip(clause.args.iter())
                        .map(|(n, e)| format!("{} := {}", n, gen_expr(e, ctx)))
                        .collect();
                    format!("{{ {} }}", pieces.join(", "))
                };
                format!("ctx.sender {} {}.address ({})", eq, target, id_record)
            } else {
                "False".to_string()
            }
        }
    }
}

pub(crate) fn gen_from_checks(
    program: &Program,
    entity: &Entity,
    route: &Route,
    indent: usize,
    profile: LeanProfile,
) -> String {
    if route.from_clauses.is_empty() {
        return String::new();
    }
    let ctx = expr_ctx_for(
        Carrier::State { fail: false },
        program,
        entity,
        route,
        None,
        false,
        profile,
    );
    let mut checks: Vec<String> = Vec::new();
    for clause in &route.from_clauses {
        checks.push(from_clause_sender_check(program, entity, clause, &ctx, profile, "=="));
    }
    let cond = checks.join(" || ");
    // A custom-named `from … : throw Foo(...)` clause maps through
    // `custom_error_code`; a numeric `: throw N` uses its literal; the
    // bare clause falls back to 1.
    let code = route
        .from_clauses
        .iter()
        .find_map(|c| {
            c.error_name
                .as_ref()
                .map(|n| custom_error_code(n))
                .or(c.error_code)
        })
        .unwrap_or(1);
    let prefix = "  ".repeat(indent);
    format!(
        "{}if !({}) then throw (Cambrian.ThrowCode.ofNat {})\n",
        prefix, cond, code
    )
}

pub(crate) fn route_args_with_space(args: &str) -> String {
    if args.is_empty() {
        String::new()
    } else {
        format!(" {}", args)
    }
}

// ---------------------------------------------------------------------------
// Per-clause `_pre_<i>` predicate
// ---------------------------------------------------------------------------

pub(crate) fn emit_where_predicate(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    route: &Route,
    idx: usize,
    w: &WhereClause,
    phase: Option<&str>,
    profile: LeanProfile,
) {
    let suffix = match phase {
        Some(p) => format!("_{}_pre_{}", p, idx),
        None => format!("_pre_{}", idx),
    };
    let needs_world = expr_needs_world(entity, &w.condition);
    out.push_str(&format!("def {}{}", route.name, suffix));
    if needs_world {
        out.push_str(" (w : Cambrian.Generated.World)");
    }
    out.push_str(&format!(
        " (s : {}.State) (ctx : Cambrian.MsgCtx) (inst : {}.Identity)",
        entity.name, entity.name,
    ));
    for p in &route.params {
        out.push_str(&format!(
            " ({} : {})",
            super::types::lean_safe_ident(&p.name),
            lower_route_param_type(program, entity, &p.ty, profile),
        ));
    }
    // Per-phase where-preds may reference vars captured in strictly
    // earlier phases (`weight` in Governor.castVote's `tally:` where).
    // Mirror the captured-var threading we already do for member
    // transforms: append `(<name> : <ty>)` per capture in source order.
    let captured =
        super::super::member::phase_captured_vars_before(program, entity, route, phase, profile);
    for (name, ty) in &captured {
        out.push_str(&format!(
            " ({} : {})",
            super::types::lean_safe_ident(name),
            ty,
        ));
    }
    out.push_str(" : Bool :=\n");
    push_indent(out, 1);
    let mut ctx = expr_ctx_for(
        Carrier::State { fail: false },
        program,
        entity,
        route,
        phase,
        false,
        profile,
    );
    // `sys::address` in a where pred must resolve to this contract's
    // own address; the pred has `inst` as a parameter, so plumb it
    // through the expr context.
    ctx.instance_var = Some("inst".to_string());
    if needs_world {
        ctx.world_var = Some("w".to_string());
    }
    for (name, _) in &captured {
        ctx.lets.insert(name.clone());
    }
    out.push_str(&gen_expr(&w.condition, &ctx));
    out.push_str("\n\n");
}

/// Application of a `_pre_*` predicate. World-dependent predicates take `w`
/// first. `qualified` is `Some(entity)` for `<E>.Local.foo_pre_0`, `None`
/// for a same-namespace `foo_pre_0`.
pub(crate) fn pre_apply(
    qualified: Option<&str>,
    route: &str,
    phase: Option<&str>,
    idx: usize,
    needs_world: bool,
    route_args: &str,
    captured_suffix: &str,
) -> String {
    let q = match qualified {
        Some(e) => format!("{e}.Local."),
        None => String::new(),
    };
    let phase_infix = match phase {
        Some(p) => format!("_{p}"),
        None => String::new(),
    };
    let world = if needs_world { "w " } else { "" };
    format!(
        "{q}{route}{phase_infix}_pre_{idx} {world}s ctx inst{}{captured_suffix}",
        route_args_with_space(route_args),
    )
}

pub(crate) fn emit_pre_guard(
    out: &mut String,
    indent: usize,
    qualified: Option<&str>,
    entity: &Entity,
    route: &Route,
    phase: Option<&str>,
    idx: usize,
    w: &WhereClause,
    route_args: &str,
    captured_suffix: &str,
    skip_world_dependent: bool,
) {
    let needs_world = expr_needs_world(entity, &w.condition);
    if skip_world_dependent && needs_world {
        return;
    }
    push_indent(out, indent);
    out.push_str(&format!(
        "if !({}) then throw (Cambrian.ThrowCode.ofNat {})\n",
        pre_apply(
            qualified,
            &route.name,
            phase,
            idx,
            needs_world,
            route_args,
            captured_suffix,
        ),
        where_clause_throw_code(w),
    ));
}

/// State-local `macro_*` helpers for `@name(args)` (mirrors Solidity
/// `function macro_<name>`).
///
/// State-only macros (`world_dependent = false`) belong in the entity
/// file so member transforms can call them without importing Routes
/// (World import cycle). World-dependent macros stay in `*Routes.lean`
/// `Local`, which already imports `World`.
pub(crate) fn emit_macro_helpers(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    profile: LeanProfile,
    world_dependent: bool,
) -> bool {
    if entity.macros.is_empty() {
        return false;
    }
    let type_ctx = LeanTypeCtx::for_entity(
        program,
        &entity.name,
        &entity.records,
        &entity.enums,
        &entity.type_aliases,
        profile,
    );
    let start = out.len();
    for mac in &entity.macros {
        let needs_world = expr_needs_world(entity, &mac.body);
        if needs_world != world_dependent {
            continue;
        }
        out.push_str(&format!("def macro_{}", mac.name));
        if needs_world {
            out.push_str(" (w : Cambrian.Generated.World)");
        }
        out.push_str(&format!(
            " (s : {}.State) (ctx : Cambrian.MsgCtx) (inst : {}.Identity)",
            entity.name, entity.name,
        ));
        let mut params = HashSet::new();
        let mut hashmap_idents = HashSet::new();
        for p in &mac.params {
            out.push_str(&format!(
                " ({} : {})",
                super::types::lean_safe_ident(&p.name),
                lower_type(&p.ty, &type_ctx),
            ));
            params.insert(p.name.clone());
            if matches!(&p.ty, crate::ast::Type::Generic(g, _) if g == "HashMap") {
                hashmap_idents.insert(p.name.clone());
            }
        }
        out.push_str(&format!(
            " : {} :=\n  ",
            lower_type(&mac.return_type, &type_ctx),
        ));
        let expr_ctx = LeanExprCtx {
            type_ctx: LeanTypeCtx::for_entity(
                program,
                &entity.name,
                &entity.records,
                &entity.enums,
                &entity.type_aliases,
                profile,
            ),
            entity,
            local_enums: &entity.enums,
            pure_fns: &program.pure_fns,
            lets: HashSet::new(),
            route_params: params,
            route_name: None,
            phase_name: None,
            route_arg_names: vec![],
            in_transform: false,
            state_var: "s".to_string(),
            derived_helpers: HashSet::new(),
            world_var: if needs_world {
                Some("w".to_string())
            } else {
                None
            },
            instance_var: Some("inst".to_string()),
            qualified_instances: HashMap::new(),
            deploy_bindings: HashMap::new(),
            hashmap_idents,
            nat_idents: HashSet::new(),
            trace_acc_var: None,
            expected_bitvec_width: super::super::expr::bitvec_width_of_type(
                &mac.return_type,
                &type_ctx,
            ),
            expected_signed: Some(super::types::type_is_signed(
                &mac.return_type,
                &type_ctx,
            )),
            expected_collection: super::super::expr::collection_shape_of_type(
                &mac.return_type,
                &type_ctx,
            ),
            msg_sender_override: None,
            pure_fn: None,
        };
        out.push_str(&gen_expr(&mac.body, &expr_ctx));
        out.push_str("\n\n");
    }
    out.len() > start
}

// ---------------------------------------------------------------------------
// Body assembly
// ---------------------------------------------------------------------------

/// Build the function body for either an unphased route or a single
/// phase. The result is a self-contained Lean term that produces the
/// post-route value (`State`, `(State × T)`, or the wrapped variants).
///
/// Always State-carrier: named `<E>.Local.*` / phase helpers never bind `w`.
/// Pass [`Carrier::State`]; World bodies use the adapter's world-threaded path.
pub(crate) fn build_route_body(
    program: &Program,
    entity: &Entity,
    route: &Route,
    actions: &[RouteAction],
    phase: Option<&PhaseBlock>,
    return_ty: Option<&str>,
    carrier: Carrier,
    profile: LeanProfile,
) -> String {
    debug_assert!(
        !carrier.is_world(),
        "build_route_body is State-only (Local / phase); use world-threaded adapter path for World"
    );
    let fail_mode = carrier.is_fail();
    let phase_name = phase.map(|p| p.name.as_str());
    let route_args = route_arg_list(route);
    let (orders, _) = crate::validate::build_temporal_orders(entity);
    let transforms = order_transforms_temporally(
        collect_transforms(entity, &route.name, phase_name),
        &orders,
        &route.name,
    );
    let return_payload = find_return_payload(actions);

    let mut body = String::new();

    if fail_mode {
        body.push_str("do\n");
        if phase.is_none() {
            body.push_str(&gen_from_checks(program, entity, route, 1, profile));
            for (i, w) in route.where_clauses.iter().enumerate() {
                emit_pre_guard(
                    &mut body,
                    1,
                    None,
                    entity,
                    route,
                    None,
                    i,
                    w,
                    &route_args,
                    "",
                    true,
                );
            }
        }
        if let Some(p) = phase {
            let captured = super::super::member::phase_captured_vars_before(
                program,
                entity,
                route,
                Some(&p.name),
                profile,
            );
            let captured_suffix = if captured.is_empty() {
                String::new()
            } else {
                let names: Vec<String> = captured
                    .iter()
                    .map(|(n, _)| super::types::lean_safe_ident(n))
                    .collect();
                format!(" {}", names.join(" "))
            };
            for (i, w) in p.where_clauses.iter().enumerate() {
                emit_pre_guard(
                    &mut body,
                    1,
                    None,
                    entity,
                    route,
                    Some(&p.name),
                    i,
                    w,
                    &route_args,
                    &captured_suffix,
                    true,
                );
            }
        }
        push_indent(&mut body, 1);
        let bind = if transforms_use_checked_arith(&transforms, profile, &program.pure_fns) {
            "←"
        } else {
            ":="
        };
        body.push_str(&format!("let s {bind} "));
        body.push_str(&apply_transforms_term(
            program,
            entity,
            route,
            &transforms,
            profile,
        ));
        body.push('\n');
        let terminated = super::super::route::emit_actions(
            &mut body, program, entity, route, actions, phase, return_ty, true, 1, profile,
        );
        if !terminated {
            push_indent(&mut body, 1);
            match (return_ty, return_payload) {
                (Some(_), Some(payload_expr)) => {
                    let mut ctx =
                        expr_ctx_for(carrier, program, entity, route, phase_name, false, profile);
                    // Bind prior `let`s in this phase so payload idents resolve
                    // (UPSTREAM B-29).
                    for a in actions {
                        if let RouteAction::Let { pattern, .. } = a {
                            for n in pattern_binders(pattern) {
                                ctx.lets.insert(n);
                            }
                        }
                        if matches!(a, RouteAction::Return { .. }) {
                            break;
                        }
                    }
                    let payload =
                        render_return_payload(&payload_expr, &ctx, route.return_type.as_ref());
                    body.push_str(&format!("pure (s, {})\n", payload));
                }
                (Some(_), None) => body.push_str("pure (s, default)\n"),
                _ => body.push_str("pure s\n"),
            }
        }
    } else {
        // Raw return: build a `let`-chain feeding into a final tail
        // expression. Non-failing routes can still contain `Let`,
        // `Conditional`, and (state-only) `Return` actions; sends and
        // similar effects render as inline comments inside the chain.
        let mut tail_emitted = false;
        if !transforms.is_empty() {
            let bind = if transforms_use_checked_arith(&transforms, profile, &program.pure_fns) {
                "←"
            } else {
                ":="
            };
            body.push_str(&format!(
                "let s {bind} {}\n",
                apply_transforms_term(program, entity, route, &transforms, profile),
            ));
        }
        // View route whose `return(...)` lives only inside a conditional
        // (no top-level return): lift the whole action list into a
        // value-producing payload term so the returned value survives
        // instead of collapsing to `(s, default)`.
        if return_ty.is_some()
            && find_return_payload(actions).is_none()
            && actions.iter().any(action_can_return)
        {
            let term = lower_view_payload_term(
                program,
                entity,
                route,
                phase_name,
                actions,
                "default",
                carrier,
                &HashSet::new(),
                profile,
            );
            body.push_str(&format!("(s, {})", term));
            return body;
        }
        let mut lets_so_far: HashSet<String> = HashSet::new();
        for action in actions {
            match action {
                RouteAction::Let { pattern, value } => {
                    let mut ctx =
                        expr_ctx_for(carrier, program, entity, route, phase_name, false, profile);
                    ctx.lets.extend(lets_so_far.iter().cloned());
                    body.push_str(&super::super::expr::lower_let_binding(
                        pattern,
                        &gen_expr(value, &ctx),
                    ));
                    body.push('\n');
                    for n in pattern_binders(pattern) {
                        lets_so_far.insert(n);
                    }
                }
                RouteAction::Return { values } => {
                    let mut ctx =
                        expr_ctx_for(carrier, program, entity, route, phase_name, false, profile);
                    ctx.lets.extend(lets_so_far.iter().cloned());
                    let payload = render_return_values(values, &ctx, route.return_type.as_ref());
                    if return_ty.is_some() {
                        body.push_str(&format!("(s, {})", payload));
                    } else {
                        // `return` in a non-view route is unusual but
                        // not invalid (it just discards the payload).
                        // Emit a self-documenting comment + `s`.
                        body.push_str(&format!(
                            "/- return ignored (non-view route): {} -/ s",
                            payload,
                        ));
                    }
                    tail_emitted = true;
                    break;
                }
                // Intentionally unsupported on Lean (validator-guarded):
                // emit the documented marker.
                RouteAction::Effect { .. }
                | RouteAction::Rescue { .. }
                | RouteAction::UpdateCode { .. } => {
                    body.push_str(&format!("-- {}\n", describe_skipped(action)));
                }
                // World-effecting actions force the world-threaded lowering,
                // so they cannot reach this state-only view path. Fail closed.
                RouteAction::Send { .. }
                | RouteAction::VarCall { .. }
                | RouteAction::Deploy { .. }
                | RouteAction::CallRoute { .. }
                | RouteAction::Emit { .. } => {
                    super::stmt::unmodeled_cell(
                        super::stmt::Carrier::State { fail: false },
                        format_args!(
                            "world-effecting action {} on the state-only view path (route `{}`)",
                            describe_skipped(action),
                            route.name,
                        ),
                    );
                }
                RouteAction::Conditional {
                    condition,
                    then_actions,
                    else_actions,
                } => {
                    // A conditional carrying a `return(...)` is lifted into
                    // the payload term *before* this loop (see the
                    // `lower_view_payload_term` shortcut above), so this arm
                    // only sees value-less conditionals on a non-view route.
                    // The branches contain no state writes (member transforms
                    // are unconditional) and no threading effects (those force
                    // the world-threaded lowering), so there is nothing to
                    // emit beyond a structural note.
                    let ctx =
                        expr_ctx_for(carrier, program, entity, route, phase_name, false, profile);
                    body.push_str(&format!(
                        "-- if {} then ... else ... (no value / no state effect on this path)\n",
                        gen_expr(condition, &ctx),
                    ));
                    let _ = (then_actions, else_actions);
                }
                RouteAction::Throw { .. } | RouteAction::ThrowCustom { .. } => {
                    // Unreachable: any `throw` (incl. nested in `if`/`for`)
                    // flips fail_mode to true, routing the route through the
                    // `Except`-framed emitter. Fail closed rather than emit a
                    // plausible-but-wrong total route.
                    super::stmt::unmodeled_cell(
                        super::stmt::Carrier::State { fail: false },
                        format_args!(
                            "throw in a route classified non-failing (route `{}`)",
                            route.name
                        ),
                    );
                }
                RouteAction::For { .. } => {
                    // Effect-free state-only `for` has no observable effect
                    // (no throw / world / escaping state). Elide like
                    // `lower_action` — do NOT emit bare `do`/`for` then a
                    // sibling `s` (T-LEAN-ST-001 / T-LEAN-ST-003).
                    body.push_str(
                        "-- elided: effect-free for-loop body (non-failing, no state/world effect)\n",
                    );
                }
            }
        }
        if !tail_emitted {
            match (return_ty, return_payload) {
                (Some(_), Some(payload_expr)) => {
                    let ctx =
                        expr_ctx_for(carrier, program, entity, route, phase_name, false, profile);
                    let payload =
                        render_return_payload(&payload_expr, &ctx, route.return_type.as_ref());
                    body.push_str(&format!("(s, {})", payload));
                }
                (Some(_), None) => {
                    body.push_str("(s, default)");
                }
                _ => body.push('s'),
            }
        }
    }
    body
}

/// Compose a single Lean expression that produces the post-route state
/// by applying every transform on top of the inbound `s`. Returns
/// `s` literally when there are no transforms.
///
/// When any transform body uses checked `+`/`-`/`*` under
/// `lean.numerics: overflow-panic`, the term has type
/// `RouteResult State` (bind with `←`); otherwise it is a total `State`.
pub(crate) fn apply_transforms_term(
    program: &Program,
    entity: &Entity,
    route: &Route,
    transforms: &[ResolvedTransform<'_>],
    profile: LeanProfile,
) -> String {
    if transforms.is_empty() {
        return "s".to_string();
    }
    let route_args = route_arg_list(route);
    let route_args_sp = route_args_with_space(&route_args);
    let any_checked = transforms_use_checked_arith(transforms, profile, &program.pure_fns);

    if !any_checked {
        let mut parts: Vec<String> = Vec::new();
        for rt in transforms {
            let captured = super::super::member::phase_captured_vars_before(
                program,
                entity,
                route,
                rt.phase.as_deref(),
                profile,
            );
            let captured_sp = if captured.is_empty() {
                String::new()
            } else {
                let names: Vec<String> = captured
                    .iter()
                    .map(|(n, _)| super::types::lean_safe_ident(n))
                    .collect();
                format!(" {}", names.join(" "))
            };
            if member_is_hashmap(rt.member) && member_needs_keys_list(entity, &rt.member.name) {
                let ctx = expr_ctx_for(
                    Carrier::State { fail: false },
                    program,
                    entity,
                    route,
                    rt.phase.as_deref(),
                    false,
                    profile,
                );
                parts.extend(hashmap_transform_fields(
                    entity,
                    rt.member,
                    &route.name,
                    rt.phase.as_deref(),
                    &format!("{}{}", route_args_sp, captured_sp),
                    &rt.transform.body,
                    &ctx,
                ));
            } else {
                parts.push(format!(
                    "{} := {}.Members.M_{}.{}{} s ctx inst{}{}",
                    rt.member.name,
                    entity.name,
                    rt.member.name,
                    route.name,
                    match rt.phase.as_deref() {
                        Some(p) => format!("_{}", p),
                        None => String::new(),
                    },
                    route_args_sp,
                    captured_sp,
                ));
            }
        }
        return format!("{{ s with {} }}", parts.join(", "));
    }

    // Monadic path: bind RouteResult transforms, then `pure` the record update.
    let mut lines: Vec<String> = Vec::new();
    let mut with_parts: Vec<String> = Vec::new();
    for rt in transforms {
        let captured = super::super::member::phase_captured_vars_before(
            program,
            entity,
            route,
            rt.phase.as_deref(),
            profile,
        );
        let captured_sp = if captured.is_empty() {
            String::new()
        } else {
            let names: Vec<String> = captured
                .iter()
                .map(|(n, _)| super::types::lean_safe_ident(n))
                .collect();
            format!(" {}", names.join(" "))
        };
        let call = format!(
            "{}.Members.M_{}.{}{} s ctx inst{}{}",
            entity.name,
            rt.member.name,
            route.name,
            match rt.phase.as_deref() {
                Some(p) => format!("_{}", p),
                None => String::new(),
            },
            route_args_sp,
            captured_sp,
        );
        let binder = format!("__{}", rt.member.name);
        if transform_body_checked(&rt.transform.body, profile, &program.pure_fns) {
            lines.push(format!("  let {binder} ← {call}"));
        } else {
            lines.push(format!("  let {binder} := {call}"));
        }
        with_parts.push(format!("{} := {binder}", rt.member.name));
    }
    lines.push(format!("  pure {{ s with {} }}", with_parts.join(", ")));
    format!("(do\n{})", lines.join("\n"))
}

pub(crate) fn transforms_use_checked_arith(
    transforms: &[ResolvedTransform<'_>],
    profile: LeanProfile,
    pures: &[crate::ast::PureFn],
) -> bool {
    transforms
        .iter()
        .any(|rt| transform_body_checked(&rt.transform.body, profile, pures))
}

fn transform_body_checked(
    body: &crate::ast::Expr,
    profile: LeanProfile,
    pures: &[crate::ast::PureFn],
) -> bool {
    super::super::expr::expr_forces_fail_surface(body, pures)
        || (profile.overflow_panic
            && !profile.nat_numerics
            && super::super::expr::expr_has_checked_binop(body))
}

pub(crate) fn find_return_payload(actions: &[RouteAction]) -> Option<Vec<crate::ast::Expr>> {
    for a in actions {
        if let RouteAction::Return { values } = a {
            return Some(values.clone());
        }
    }
    None
}

pub(crate) fn render_return_values(
    values: &[crate::ast::Expr],
    ctx: &LeanExprCtx<'_>,
    expected_ty: Option<&Type>,
) -> String {
    if values.is_empty() {
        return "()".to_string();
    }
    if values.len() == 1 {
        let term = gen_expr(&values[0], ctx);
        // UPSTREAM B-31: coerce a single numeric payload to the declared
        // return width (`sys::blockNumber` is U256 / BitVec 256 in the
        // width model but `-> u64` is BitVec 64).
        if let Some(ty) = expected_ty {
            return coerce_arg_to_return_ty(&values[0], &term, ty, ctx);
        }
        return term;
    }
    let parts: Vec<String> = values.iter().map(|v| gen_expr(v, ctx)).collect();
    format!("({})", parts.join(", "))
}

fn coerce_arg_to_return_ty(
    expr: &crate::ast::Expr,
    term: &str,
    ty: &Type,
    ctx: &LeanExprCtx<'_>,
) -> String {
    let use_nat = ctx.type_ctx.use_nat_numerics;
    let target_bits = match ty {
        Type::Simple(s) => match s.as_str() {
            "u8" | "i8" if use_nat => None,
            "u16" | "i16" if use_nat => None,
            "u32" | "i32" if use_nat => None,
            "u64" | "i64" if use_nat => None,
            "u128" | "i128" if use_nat => None,
            "U256" | "uint256" if use_nat => None,
            "u8" | "i8" => Some(8u32),
            "u16" | "i16" => Some(16),
            "u32" | "i32" => Some(32),
            "u64" | "i64" => Some(64),
            "u128" | "i128" => Some(128),
            "U256" | "uint256" => Some(256),
            "address" => Some(160),
            "pubkey" => Some(256),
            _ => None,
        },
        _ => None,
    };
    let Some(n) = target_bits else {
        return term.to_string();
    };
    // Only widen/narrow when the AST has a known source width that
    // disagrees with the declared return type (B-31: `sys::blockNumber`
    // is BitVec 256, `-> u64` is 64; `a + b` over `u8` params is 8).
    // Literals and width-invisible arithmetic are already guided by
    // `ctx.expected_bitvec_width` inside `gen_expr`; coercing them again
    // double-ascribes (`(((0 : BitVec 64) : BitVec 64))`).
    match arith_result_width(expr, ctx) {
        Some(src) if src != n => coerce_result_to_width(expr, term, n, ctx),
        _ => term.to_string(),
    }
}

pub(crate) fn render_return_payload(
    values: &[crate::ast::Expr],
    ctx: &LeanExprCtx<'_>,
    expected_ty: Option<&Type>,
) -> String {
    render_return_values(values, ctx, expected_ty)
}

/// True when `action` (or any action nested in its `if` / `for` bodies)
/// is a `return(...)`. Used to detect view routes whose only `return`
/// lives inside a conditional, so the payload can be lifted into a
/// value-producing `if … then … else …` term instead of collapsing to
/// `default` (see [`lower_view_payload_term`]).
pub(crate) fn action_can_return(action: &RouteAction) -> bool {
    match action {
        RouteAction::Return { .. } => true,
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            then_actions.iter().any(action_can_return) || else_actions.iter().any(action_can_return)
        }
        RouteAction::For { body, .. } => body.iter().any(action_can_return),
        _ => false,
    }
}

/// Collect the identifier names bound by a `let` pattern so they can be
/// added to the expression context's `lets` set (otherwise a subsequent
/// reference to the binding would mis-resolve as a member read).
pub(crate) fn pattern_binders(pat: &crate::ast::Pattern) -> Vec<String> {
    use crate::ast::Pattern;
    match pat {
        Pattern::Ident(name) => vec![name.clone()],
        Pattern::Deref(inner) | Pattern::Some(inner) => pattern_binders(inner),
        Pattern::Tuple(parts) => parts.iter().flat_map(pattern_binders).collect(),
        Pattern::Wildcard | Pattern::None => Vec::new(),
    }
}

/// Lower a *non-failing* view route's action list into a Lean value
/// expression yielding the route's `return(...)` payload. This is what
/// lets a getter written as `if c => [return a] else [return b]` keep
/// its value instead of erasing it to `default`.
///
/// * `Let` introduces a term-level binding (tracked in `lets` so later
///   references resolve to the local rather than a member read);
/// * `Return` becomes the tail value;
/// * `Conditional` becomes `if cond then <then> else <else>`, with each
///   branch lowered recursively and `fallthrough` carrying the value to
///   use when a branch reaches its end without returning.
///
/// Effectful actions (`Send` / `Emit` / `Deploy` / `gosh::*` / …) never
/// contribute a value here: the world-threaded body already emits them
/// for their effects, so they are skipped while computing the payload.
///
/// [`Carrier`] selects the expression context: World for Routes bodies
/// (`w`/`inst` in scope), State for named Local / phase bodies.
pub(crate) fn lower_view_payload_term(
    program: &Program,
    entity: &Entity,
    route: &Route,
    phase_name: Option<&str>,
    actions: &[RouteAction],
    fallthrough: &str,
    carrier: Carrier,
    lets: &HashSet<String>,
    profile: LeanProfile,
) -> String {
    let make_ctx = |lets: &HashSet<String>| {
        let mut ctx = expr_ctx_for(carrier, program, entity, route, phase_name, false, profile);
        ctx.lets = lets.clone();
        ctx
    };

    let Some((head, rest)) = actions.split_first() else {
        return fallthrough.to_string();
    };

    match head {
        RouteAction::Let { pattern, value } => {
            let ctx = make_ctx(lets);
            let val_str = gen_expr(value, &ctx);
            let mut next_lets = lets.clone();
            for n in pattern_binders(pattern) {
                next_lets.insert(n);
            }
            let cont = lower_view_payload_term(
                program,
                entity,
                route,
                phase_name,
                rest,
                fallthrough,
                carrier,
                &next_lets,
                profile,
            );
            format!(
                "{}\n{}",
                super::super::expr::lower_let_binding(pattern, &val_str),
                cont
            )
        }
        RouteAction::Return { values } => {
            let ctx = make_ctx(lets);
            render_return_values(values, &ctx, route.return_type.as_ref())
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            let ctx = make_ctx(lets);
            let cond = gen_expr(condition, &ctx);
            // Value reached when a branch falls through without returning
            // (the actions after the `if`, lowered with the same `lets`).
            let cont = lower_view_payload_term(
                program,
                entity,
                route,
                phase_name,
                rest,
                fallthrough,
                carrier,
                lets,
                profile,
            );
            let then_v = lower_view_payload_term(
                program,
                entity,
                route,
                phase_name,
                then_actions,
                &cont,
                carrier,
                lets,
                profile,
            );
            let else_v = lower_view_payload_term(
                program,
                entity,
                route,
                phase_name,
                else_actions,
                &cont,
                carrier,
                lets,
                profile,
            );
            format!("if {} then ({}) else ({})", cond, then_v, else_v)
        }
        // Effects / throws / loops don't produce a value on this path.
        // Skip and continue with the remaining actions.
        _ => lower_view_payload_term(
            program,
            entity,
            route,
            phase_name,
            rest,
            fallthrough,
            carrier,
            lets,
            profile,
        ),
    }
}

pub(crate) fn describe_skipped(action: &RouteAction) -> &'static str {
    match action {
        // Not supported on the Lean (EVM-flavoured) target.
        RouteAction::Effect { .. } => "not supported on Lean: namespaced effect (gosh::* etc.)",
        RouteAction::Rescue { .. } => {
            "not supported on Lean: rescue/recover (TVM async bounce recovery; E26)"
        }
        RouteAction::UpdateCode { .. } => {
            "not supported on Lean: gosh::updateCode (on-chain code upgrade)"
        }
        RouteAction::For { .. } => "skipped: action-level for-loop",
        // `Send` / `VarCall` / `Deploy` / `Emit` / `CallRoute` force the
        // world-threaded lowering (`action_needs_world_thread`) and are
        // handled there, so they never reach this fallback.
        _ => "skipped: action",
    }
}

// ---------------------------------------------------------------------------
// Throw-code helpers
// ---------------------------------------------------------------------------

pub(crate) fn custom_error_code(name: &str) -> u32 {
    crate::codegen::types::cambrian_function_id(name)
}

/// Resolve the numeric throw code a `where` clause should emit: a custom
/// (`error_name`) clause maps through [`custom_error_code`]; a plain
/// `throw N` clause uses its literal `error_code`.
pub(crate) fn where_clause_throw_code(w: &crate::ast::WhereClause) -> u32 {
    match &w.error_name {
        Some(name) => custom_error_code(name),
        None => w.error_code,
    }
}

// ---------------------------------------------------------------------------
// Param / expr-ctx / indent helpers
// ---------------------------------------------------------------------------

pub(crate) fn lower_route_param_type(
    program: &Program,
    entity: &Entity,
    ty: &crate::ast::Type,
    profile: LeanProfile,
) -> String {
    let ctx = LeanTypeCtx::for_entity(
        program,
        &entity.name,
        &entity.records,
        &entity.enums,
        &entity.type_aliases,
        profile,
    );
    lower_type(ty, &ctx)
}

pub(crate) fn route_arg_list(route: &Route) -> String {
    route
        .params
        .iter()
        .map(|p| super::types::lean_safe_ident(&p.name))
        .collect::<Vec<String>>()
        .join(" ")
}

/// Build a [`LeanExprCtx`] for the given [`Carrier`].
///
/// This is the **only** public constructor that decides whether `w` /
/// `inst` are in scope — call sites pass the carrier that matches the
/// emitted signature; they must not re-derive "has world?" from proxies
/// like `phase.is_none()` (C-1 × Local regression class).
pub(crate) fn expr_ctx_for<'a>(
    carrier: Carrier,
    program: &'a Program,
    entity: &'a Entity,
    route: &'a Route,
    phase: Option<&'a str>,
    in_transform: bool,
    profile: LeanProfile,
) -> LeanExprCtx<'a> {
    let with_world = carrier.is_world();
    let mut params = HashSet::new();
    for p in &route.params {
        params.insert(p.name.clone());
    }
    let type_ctx = LeanTypeCtx::for_entity(
        program,
        &entity.name,
        &entity.records,
        &entity.enums,
        &entity.type_aliases,
        profile,
    );
    let expected_bitvec_width = route
        .return_type
        .as_ref()
        .and_then(|ty| super::super::expr::bitvec_width_of_type(ty, &type_ctx));
    let expected_signed = route
        .return_type
        .as_ref()
        .map(|ty| super::types::type_is_signed(ty, &type_ctx));
    LeanExprCtx {
        type_ctx,
        entity,
        local_enums: &entity.enums,
        pure_fns: &program.pure_fns,
        lets: HashSet::new(),
        route_params: params,
        route_name: Some(route.name.clone()),
        phase_name: phase.map(|s| s.to_string()),
        route_arg_names: route.params.iter().map(|p| p.name.clone()).collect(),
        in_transform,
        state_var: "s".to_string(),
        derived_helpers: HashSet::new(),
        world_var: if with_world {
            Some("w".to_string())
        } else {
            None
        },
        // Local / phase / `_pre_*` signatures all bind `inst : Identity`.
        // Only World additionally binds `w`.
        instance_var: Some("inst".to_string()),
        qualified_instances: HashMap::new(),
        deploy_bindings: HashMap::new(),
        hashmap_idents: std::collections::HashSet::new(),
        nat_idents: std::collections::HashSet::new(),
        trace_acc_var: None,
        expected_bitvec_width,
        expected_signed,
        expected_collection: None,
        msg_sender_override: None,
        pure_fn: None,
    }
}

pub(crate) fn write_indented(out: &mut String, body: &str, indent: usize) {
    let prefix: String = "  ".repeat(indent);
    for (i, line) in body.split_inclusive('\n').enumerate() {
        if i == 0 || !line.trim().is_empty() {
            out.push_str(&prefix);
        }
        out.push_str(line);
    }
}

// ---------------------------------------------------------------------------
// Phase State helpers
// ---------------------------------------------------------------------------

/// Emit a named `<E>.Local.<route>` def whose body already operates on
/// `s : State`. The adapter builds `body` via [`build_route_body`] (and may
/// apply escrow rewriting) before calling this; World wrapping lives in the
/// thin `Routes.<route>` wrapper.
pub(crate) fn emit_local_route_def(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    route: &Route,
    return_ty: Option<&str>,
    fail_mode: bool,
    profile: LeanProfile,
    body: &str,
) {
    out.push_str(&format!(
        "def {} (s : {}.State) (ctx : Cambrian.MsgCtx) (inst : {}.Identity)",
        route.name, entity.name, entity.name,
    ));
    for p in &route.params {
        out.push_str(&format!(
            " ({} : {})",
            super::types::lean_safe_ident(&p.name),
            lower_route_param_type(program, entity, &p.ty, profile),
        ));
    }
    let core = match return_ty {
        Some(rt) => format!("{}.State × {}", entity.name, rt),
        None => format!("{}.State", entity.name),
    };
    let full = if fail_mode {
        format!("Cambrian.RouteResult ({})", core)
    } else {
        core
    };
    out.push_str(&format!(" : {} :=\n", full));
    write_indented(out, body, 1);
    out.push_str("\n\n");
}

/// Emit a per-phase helper `def` whose body already operates on `s : State`.
/// Lives in `<E>.Local` as `<route>_<phase>`. The adapter builds `body` via
/// [`build_route_body`] (and may apply escrow rewriting) before calling this.
/// Emit a per-phase helper `def` whose body already operates on `s : State`.
/// Lives in `<E>.Local` as `<route>_<phase>`. The adapter builds `body` via
/// [`build_route_body`] (and may apply escrow rewriting) before calling this.
///
/// When `return_ty` is `Some` (UPSTREAM B-29 — the phase that yields a view
/// route's payload), the helper returns `State × T` (or `RouteResult` thereof)
/// so the phased entry can destructure the payload instead of rebuilding it
/// out of scope.
pub(crate) fn emit_phase_def(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    route: &Route,
    ph: &PhaseBlock,
    profile: LeanProfile,
    phase_fail: bool,
    return_ty: Option<&str>,
    body: &str,
) {
    doc_comment(
        out,
        0,
        &format!("Phase `{}` of route `{}`.", ph.name, route.name),
    );
    out.push_str(&format!(
        "def {}_{} (s : {}.State) (ctx : Cambrian.MsgCtx) (inst : {}.Identity)",
        route.name, ph.name, entity.name, entity.name,
    ));
    for p in &route.params {
        out.push_str(&format!(
            " ({} : {})",
            p.name,
            lower_route_param_type(program, entity, &p.ty, profile),
        ));
    }
    let core = match return_ty {
        Some(rt) => format!("{}.State × {}", entity.name, rt),
        None => format!("{}.State", entity.name),
    };
    let ret = if phase_fail {
        format!("Cambrian.RouteResult ({})", core)
    } else {
        core
    };
    out.push_str(&format!(" : {} :=\n", ret));
    write_indented(out, body, 1);
    out.push_str("\n\n");
}
