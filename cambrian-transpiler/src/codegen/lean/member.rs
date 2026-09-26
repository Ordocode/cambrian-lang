// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean codegen — per-(member, route, phase) `def`s (P1.4 / P1.5).
//!
//! For every non-identity member with at least one declared transform,
//! we emit a sub-namespace `<Entity>.Members.M_<member>` that contains
//! one `def` per `(route, phase)` for which the transform is declared.
//! The signature mirrors the route's parameter list, but the function
//! returns the *member type* (not the full state), so route assemblers
//! can apply each transform independently — preserving Cambrian's
//! field-independent decomposition.
//!
//! See [docs/PLAN_LEAN_TARGET.md](../../docs/PLAN_LEAN_TARGET.md) §P1.4.

use std::collections::HashSet;

use crate::ast::{Entity, Member, MemberTransform, Pattern, Program, Route};

use super::core::emitter::{doc_comment, push_indent};
use super::core::types::{lower_type, LeanTypeCtx};
use super::evm::send::{classify_dest, SendTarget};
use super::expr::{gen_expr, LeanExprCtx};
use super::LeanProfile;

/// Emit `<Entity>.Members.M_<name>` namespaces for every non-identity
/// member that has at least one transform. Returns an empty string
/// when no member needs lowering.
pub fn gen_member_module(program: &Program, entity: &Entity, profile: LeanProfile) -> String {
    let mut out = String::new();
    let target_members: Vec<&Member> = entity
        .members
        .iter()
        .filter(|m| !m.is_identity && !m.transforms.is_empty())
        .collect();
    if target_members.is_empty() {
        return out;
    }
    for member in target_members {
        emit_member_namespace(&mut out, program, entity, member, profile);
    }
    out
}

fn emit_member_namespace(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    member: &Member,
    profile: LeanProfile,
) {
    out.push_str(&format!(
        "namespace {}.Members.M_{}\n\n",
        entity.name, member.name,
    ));
    let mut def_names: Vec<String> = Vec::new();
    for tr in &member.transforms {
        emit_transform_def(out, program, entity, member, tr, profile);
        let phase_suffix = match &tr.phase {
            Some(p) => format!("_{}", p),
            None => String::new(),
        };
        def_names.push(format!("{}{}", tr.route_name, phase_suffix));
    }
    // Register each transform into the `cambrian_member_simp` set so
    // `simp [cambrian_member_simp]` unfolds the post-route member value
    // into its defining arithmetic (see Prelude). Emitted inside the
    // member namespace, so short names suffice. Skipped when
    // `lean.proof_helpers: false` (the tags are proof-assistance only).
    if !def_names.is_empty() && profile.proof_helpers {
        out.push_str(&format!(
            "attribute [cambrian_member_simp] {}\n\n",
            def_names.join(" "),
        ));
    }
    out.push_str(&format!(
        "end {}.Members.M_{}\n\n",
        entity.name, member.name
    ));
}

fn emit_transform_def(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    member: &Member,
    tr: &MemberTransform,
    profile: LeanProfile,
) {
    let route = entity.routes.iter().find(|r| r.name == tr.route_name);
    // `in r()` binds none of `r`'s parameters, but the route still passes
    // every argument, so each one becomes an anonymous binder.
    let route_params: Vec<(String, String)> = match route {
        Some(r) => r
            .params
            .iter()
            .enumerate()
            .map(|(i, rp)| {
                (
                    tr.params.get(i).map(pattern_binder).unwrap_or_else(|| "_".to_string()),
                    rp.ty.clone().into_lean_string(program, entity, profile),
                )
            })
            .collect(),
        None => Vec::new(),
    };

    // Cambrian phased routes can capture `var x = msg(...) ~> dest` in
    // an earlier phase and reference `x` from a transform body tagged
    // with a later phase. We surface those captures as extra parameters
    // so the transform `def` is well-typed.
    let captured: Vec<(String, String)> = match route {
        Some(r) => phase_captured_vars_before(program, entity, r, tr.phase.as_deref(), profile),
        None => Vec::new(),
    };

    let phase_suffix = match &tr.phase {
        Some(p) => format!("_{}", p),
        None => String::new(),
    };

    let type_ctx = LeanTypeCtx::for_entity(
        program,
        &entity.name,
        &entity.records,
        &entity.enums,
        &entity.type_aliases,
        profile,
    );

    doc_comment(
        out,
        0,
        &format!(
            "Member transform of `{}` for route `{}`{}.",
            member.name,
            tr.route_name,
            tr.phase
                .as_deref()
                .map(|p| format!(" (phase `{}`)", p))
                .unwrap_or_default(),
        ),
    );
    out.push_str(&format!(
        "def {}{} (s : {}.State) (ctx : Cambrian.MsgCtx) (inst : {}.Identity)",
        tr.route_name, phase_suffix, entity.name, entity.name,
    ));
    for (name, ty) in &route_params {
        out.push_str(&format!(
            " ({} : {})",
            super::core::types::lean_safe_ident(name),
            ty,
        ));
    }
    for (name, ty) in &captured {
        out.push_str(&format!(
            " ({} : {})",
            super::core::types::lean_safe_ident(name),
            ty,
        ));
    }
    let member_ty = lower_type(&member.ty, &type_ctx);
    let checked_transform = (profile.overflow_panic
        && !profile.nat_numerics
        && super::expr::expr_has_checked_binop(&tr.body))
        || super::expr::expr_forces_fail_surface(&tr.body, &program.pure_fns);
    if checked_transform {
        out.push_str(&format!(" : Cambrian.RouteResult ({member_ty}) :=\n"));
    } else {
        out.push_str(&format!(" : {member_ty} :=\n"));
    }

    let mut params_in_scope: HashSet<String> =
        route_params.iter().map(|(n, _)| n.clone()).collect();
    for (n, _) in &captured {
        params_in_scope.insert(n.clone());
    }
    let expr_ctx = LeanExprCtx {
        type_ctx: LeanTypeCtx {
            program: type_ctx.program,
            proof_helpers: type_ctx.proof_helpers,
            use_nat_numerics: type_ctx.use_nat_numerics,
            overflow_panic: type_ctx.overflow_panic,
            use_predictable_profile: type_ctx.use_predictable_profile,
            deterministic_addresses: type_ctx.deterministic_addresses,
            entity_name: type_ctx.entity_name,
            local_records: type_ctx.local_records,
            local_enums: type_ctx.local_enums,
            local_aliases: type_ctx.local_aliases,
        },
        entity,
        local_enums: &entity.enums,
        pure_fns: &program.pure_fns,
        lets: HashSet::new(),
        route_params: params_in_scope,
        route_name: Some(tr.route_name.clone()),
        phase_name: tr.phase.clone(),
        route_arg_names: route_params.iter().map(|(n, _)| n.clone()).collect(),
        in_transform: true,
        state_var: "s".to_string(),
        derived_helpers: std::collections::HashSet::new(),
        world_var: None,
        instance_var: Some("inst".to_string()),
        qualified_instances: std::collections::HashMap::new(),
        deploy_bindings: std::collections::HashMap::new(),
        hashmap_idents: std::collections::HashSet::new(),
        nat_idents: std::collections::HashSet::new(),
        trace_acc_var: None,
        expected_bitvec_width: super::expr::bitvec_width_of_type(&member.ty, &type_ctx),
        expected_signed: Some(super::core::types::type_is_signed(&member.ty, &type_ctx)),
        expected_collection: super::expr::collection_shape_of_type(&member.ty, &type_ctx),
        msg_sender_override: None,
        pure_fn: None,
    };
    // Escrow: expand a tuple-pattern `let` in the body so it no longer
    // elaborates to a matcher (`M_<field>.<route>.match_1`). The rewrite is a
    // byte-identical no-op for every body without a tuple-let, so non-tuple
    // members keep the original single-line `push_indent + push_str` framing;
    // only an actually-rewritten (multi-line) body switches to per-line indent.
    let body_expr = if checked_transform {
        super::expr::gen_expr_as_route_result(&tr.body, &expr_ctx)
    } else {
        let term = gen_expr(&tr.body, &expr_ctx);
        match (
            super::expr::arith_result_width(&tr.body, &expr_ctx),
            expr_ctx.expected_bitvec_width,
        ) {
            (Some(src), Some(dst)) if src != dst && !type_ctx.use_nat_numerics => {
                super::expr::coerce_result_to_width(&tr.body, &term, dst, &expr_ctx)
            }
            _ => term,
        }
    };
    let rewritten = super::evm::rewrite_member_body(&body_expr, profile.predictable);
    if rewritten != body_expr {
        // Indent every non-blank line by one level (mirror of
        // `lean_route::write_indented`); the leading line is always prefixed.
        let prefix = "  ";
        for (i, line) in rewritten.split_inclusive('\n').enumerate() {
            if i == 0 || !line.trim().is_empty() {
                out.push_str(prefix);
            }
            out.push_str(line);
        }
    } else {
        push_indent(out, 1);
        out.push_str(&body_expr);
    }
    out.push_str("\n\n");
    let _ = (route, member);
}

/// Collect every `var x = msg(args) ~> dest` capture appearing in
/// a phase of `route` that lies strictly before `current_phase`. The
/// returned `(name, lean_type)` pairs become extra parameters on
/// member-transform defs tagged with `current_phase`.
pub(crate) fn phase_captured_vars_before(
    program: &Program,
    entity: &Entity,
    route: &Route,
    current_phase: Option<&str>,
    profile: LeanProfile,
) -> Vec<(String, String)> {
    use crate::ast::{RouteAction, RouteBody};
    let phases = match &route.body {
        RouteBody::Phased(p) => p.as_slice(),
        _ => return Vec::new(),
    };
    let Some(cur) = current_phase else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for ph in phases {
        if ph.name == cur {
            break;
        }
        for action in &ph.actions {
            if let RouteAction::VarCall {
                name,
                message,
                dest,
                ..
            } = action
            {
                if !seen.insert(name.clone()) {
                    continue;
                }
                let ty = captured_var_lean_type(message, dest, entity, route, program, profile);
                out.push((name.clone(), ty));
            }
        }
    }
    out
}

fn captured_var_lean_type(
    message: &str,
    dest: &crate::ast::Expr,
    entity: &Entity,
    route: &Route,
    program: &Program,
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
    let target = classify_dest(dest, entity, route, program);
    let target_entity = match &target {
        SendTarget::SameEntity { .. } => Some(entity.name.as_str()),
        SendTarget::CrossEntity { entity: e, .. } | SendTarget::DynamicTyped { entity: e, .. } => {
            Some(e.as_str())
        }
        SendTarget::DynamicUntyped { .. } | SendTarget::ExternEntity { .. } | SendTarget::Raw => {
            None
        }
    };
    let lookup = |ent_name: &str| -> Option<String> {
        let ent = program.entities.iter().find(|e| e.name == ent_name)?;
        let r = ent.routes.iter().find(|r| r.name == message)?;
        r.return_type.as_ref().map(|t| lower_type(t, &ctx))
    };
    if let Some(en) = target_entity {
        if let Some(ty) = lookup(en) {
            return ty;
        }
    }
    // Fall back: find *some* in-program entity exposing this route.
    for e in &program.entities {
        if let Some(ty) = lookup(&e.name) {
            return ty;
        }
    }
    // Final fallback — `Cambrian.U256` is the most common return type
    // for read-only views (`balanceOf`, `getVotes`); a wrong choice
    // here surfaces as a Lean type error at the call site.
    "Cambrian.U256".to_string()
}

fn pattern_binder(pat: &Pattern) -> String {
    match pat {
        Pattern::Ident(name) => name.clone(),
        Pattern::Wildcard => "_".to_string(),
        // Tuple/Some/None/Deref destructuring in member-transform
        // params is uncommon in P1 fixtures; keep a stable binder
        // name and let the validator catch genuinely unsupported
        // shapes.
        Pattern::Tuple(_) => "_".to_string(),
        Pattern::Some(_) => "_".to_string(),
        Pattern::None => "_".to_string(),
        Pattern::Deref(inner) => pattern_binder(inner),
    }
}

// Tiny extension trait so we can inline `lower_type` calls without
// repeating the LeanTypeCtx construction at every call site.
trait IntoLeanString {
    fn into_lean_string(self, program: &Program, entity: &Entity, profile: LeanProfile) -> String;
}

impl IntoLeanString for crate::ast::Type {
    fn into_lean_string(self, program: &Program, entity: &Entity, profile: LeanProfile) -> String {
        let ctx = LeanTypeCtx::for_entity(
            program,
            &entity.name,
            &entity.records,
            &entity.enums,
            &entity.type_aliases,
            profile,
        );
        lower_type(&self, &ctx)
    }
}

#[allow(dead_code)]
fn _route_signature_lookup<'a>(entity: &'a Entity, name: &str) -> Option<&'a Route> {
    entity.routes.iter().find(|r| r.name == name)
}

