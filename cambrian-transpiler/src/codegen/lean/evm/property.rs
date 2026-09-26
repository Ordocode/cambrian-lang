// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean codegen — `PropertyDecl` → Lean theorem.
//!
//! A `property` is the abstract, parameterised statement; the Lean
//! backend lowers it directly (it never sees the desugared `fuzz`/`test`
//! instances). The theorem:
//!   1. introduces the property parameters as universally quantified
//!      variables (`∀ (amount : BitVec 64), …`);
//!   2. lifts each `assume` precondition to a hypothesis in the `→`-chain
//!      between the `∀` and the goal.
//!
//! Crucially, *sampling bounds never reach Lean*: they live only in the
//! nested `fuzz` instances (consumed by the Rust backend after the
//! desugaring pass), so the theorem quantifies over the full parameter
//! range with only the genuine logical preconditions attached.
//!
//! The body lowering of `Let` / `SetContext` / `Call` / `Expect*` mirrors
//! `lean_test.rs`, except calls and member references see property
//! parameters as in-scope locals.

use std::collections::{HashMap, HashSet};

use crate::ast::{Entity, Expr, ForallTarget, Param, Program, PropertyDecl, TestStep, Type};

use super::super::core::emitter::{line_comment, push_indent};
use super::super::core::types::{lower_type, LeanTypeCtx};
use super::super::expr::{gen_expr, LeanExprCtx};
use super::super::route::{route_fail_mode, route_is_view};
use super::spec::{slugify, SpecBlock, SpecDiag};
use super::spec_prefix::{self, alloc_unique};
use super::spec_steps::{
    alloc_peer_inst, dropped_step_comment, emit_body, emit_prefix_let_bindings,
    collect_throw_targets,
    filter_property_body_after_hoist,
    hoist_lets_for_assume_hypotheses, lower_expect_effects, stale_result_comment,
    spec_state_var, collect_deploy_bindings, deploy_positional_init_event,
    init_call_folded_into_deploy, note_sut_deploy_instance, sut_self_call_instance,
    SpecEvent, SpecStatementShape, SpecStepCtx,
};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub(crate) fn gen_property_block(
    program: &Program,
    entity: &Entity,
    diags: &mut Vec<SpecDiag>,
    profile: super::super::LeanProfile,
) -> Option<SpecBlock> {
    let props: Vec<&PropertyDecl> = program
        .properties
        .iter()
        .filter(|p| p.entity_name == entity.name)
        .collect();
    if props.is_empty() {
        return None;
    }
    let predictable = profile.predictable;
    let ns = format!("{}.Spec.Properties", entity.name);
    let mut proofs = format!("namespace {}\n\n", ns);
    let mut stmts = if predictable {
        format!("namespace {}\n\n", ns)
    } else {
        String::new()
    };
    for p in props {
        let th = gen_property_theorem(program, entity, p, profile, diags);
        if predictable {
            stmts.push_str(&format!(
                "abbrev {}.statement : Prop :=\n{}\n\n",
                th.slug, th.ty,
            ));
            proofs.push_str(&th.comments);
            proofs.push_str(&format!("theorem {} : {}.statement", th.slug, th.slug));
            proofs.push_str(&super::spec::emit_proof_tail(
                &th.proof_shape,
                entity,
                true,
                profile.proof_helpers,
                &th.slug,
            ));
        } else {
            proofs.push_str(&th.comments);
            proofs.push_str(&format!("theorem {} :\n{}", th.slug, th.ty));
            proofs.push_str(&super::spec::emit_proof_tail(
                &th.proof_shape,
                entity,
                false,
                profile.proof_helpers,
                &th.slug,
            ));
        }
        proofs.push('\n');
    }
    proofs.push_str(&format!("end {}\n\n", ns));
    if predictable {
        stmts.push_str(&format!("end {}\n\n", ns));
    }
    Some(SpecBlock {
        proofs,
        statements: if predictable { Some(stmts) } else { None },
    })
}

// ---------------------------------------------------------------------------
// Per-property theorem emission
// ---------------------------------------------------------------------------

/// The three separable pieces of a property theorem (see the `lean_test`
/// twin): leading comments, the slug, and the statement `type` body ending
/// *without* a trailing newline.
struct PropertyTheorem {
    comments: String,
    slug: String,
    ty: String,
    proof_shape: SpecStatementShape,
}

fn gen_property_theorem(
    program: &Program,
    entity: &Entity,
    p: &PropertyDecl,
    profile: super::super::LeanProfile,
    diags: &mut Vec<SpecDiag>,
) -> PropertyTheorem {
    let mut comments = String::new();
    if let Some(tag) = &p.tag {
        line_comment(&mut comments, 0, &format!("tag: {}", tag));
    }

    let slug = slugify(&p.name);
    // The statement type body is assembled into `out`; the caller wraps it as
    // either `theorem <slug> :\n<out> := by …` (legacy) or `abbrev
    // <slug>.statement : Prop :=\n<out>` + `theorem <slug> : <slug>.statement`
    // (predictable).
    let mut out = String::new();

    // ∀-prefix
    let type_ctx = LeanTypeCtx::for_entity(
        program,
        &entity.name,
        &entity.records,
        &entity.enums,
        &entity.type_aliases,
        profile,
    );

    // Resolve the forall spec into quantified state fields and context
    // params. State fields turn into extra `∀`-bound variables that seed the
    // initial state; context params seed the initial `ctx`/`sys`.
    let pinned: HashSet<&str> = p.init_state.iter().map(|(n, _)| n.as_str()).collect();
    let mut forall_state: Vec<(String, Type)> = Vec::new();
    if p.forall_state.all_state {
        for m in &entity.members {
            if !pinned.contains(m.name.as_str()) {
                forall_state.push((m.name.clone(), m.ty.clone()));
            }
        }
    }
    for t in &p.forall_state.targets {
        if let ForallTarget::StateField(f) = t {
            if pinned.contains(f.as_str()) || forall_state.iter().any(|(n, _)| n == f) {
                continue;
            }
            if let Some(m) = entity.members.iter().find(|m| &m.name == f) {
                forall_state.push((f.clone(), m.ty.clone()));
            }
        }
    }
    let forall_field_names: Vec<String> = forall_state.iter().map(|(n, _)| n.clone()).collect();

    // Collect all quantified variables: property params, then forall state
    // fields, then forall context params.
    let mut quant: Vec<(String, String)> = Vec::new();
    let mut used: HashSet<String> = HashSet::new();
    for param in &p.params {
        quant.push((param.name.clone(), lower_type(&param.ty, &type_ctx)));
        used.insert(param.name.clone());
    }
    for (f, ty) in &forall_state {
        quant.push((f.clone(), lower_type(ty, &type_ctx)));
        used.insert(f.clone());
    }
    let params: HashSet<String> = p.params.iter().map(|p| p.name.clone()).collect();
    let mut init_params = params.clone();
    for f in &forall_field_names {
        init_params.insert(f.clone());
    }
    // Pin/forall ctx lowering happens before `let w`, so no `world_var`.
    let pin_ctx = LeanExprCtx::for_spec(
        program,
        entity,
        spec_state_var(entity),
        init_params.clone(),
        HashSet::new(),
        profile,
    );
    let setup = spec_prefix::ctx_setup(
        &p.context,
        &pin_ctx,
        &mut used,
        Some((entity.name.as_str(), "inst")),
        alloc_unique,
    );
    quant.extend(setup.quant.iter().cloned());

    if !quant.is_empty() {
        push_indent(&mut out, 1);
        out.push('∀');
        for (name, ty) in &quant {
            out.push_str(&format!(
                " ({} : {})",
                super::super::core::types::lean_safe_ident(name),
                ty,
            ));
        }
        out.push_str(",\n");
    }

    let mut all_lets = params.clone();
    for step in &p.body {
        if let TestStep::Let { name, .. } = step {
            all_lets.insert(name.clone());
        }
    }

    // The initial state pins the property defaults plus a synthetic pin per
    // forall state field, referencing the quantified variable.
    let mut effective_init = p.init_state.clone();
    for f in &forall_field_names {
        effective_init.push((f.clone(), Expr::Ident(f.clone())));
    }

    // Init seeds must not see `world_var` (B-26): `sys::now` → `ctx.timestamp`.
    let init_ctx = LeanExprCtx::for_spec(
        program,
        entity,
        spec_state_var(entity),
        init_params.clone(),
        HashSet::new(),
        profile,
    );
    let mut init_world_ctx = LeanExprCtx::for_spec(
        program,
        entity,
        spec_state_var(entity),
        init_params.clone(),
        HashSet::new(),
        profile,
    );
    init_world_ctx.instance_var = Some("inst".into());
    let world_ctx = LeanExprCtx::for_spec(
        program,
        entity,
        spec_state_var(entity),
        init_params,
        HashSet::new(),
        profile,
    )
    .with_world("w", "inst");

    let inst_line = spec_prefix::format_init_instance(entity, &effective_init, &init_ctx);
    let world_lets =
        spec_prefix::format_world_lets(entity, &effective_init, &init_world_ctx, &world_ctx);
    spec_prefix::emit_prefix_lets(&mut out, 1, &setup, &inst_line, &world_lets);

    let hoist = hoist_lets_for_assume_hypotheses(&p.body);
    let prefix_lets = hoist.prefix_lets;
    let mut bound_params = params.clone();
    for (name, _, _) in &prefix_lets {
        bound_params.insert(name.clone());
    }

    let deploy_bindings = collect_deploy_bindings(&p.body);

    // Hypotheses — ONLY `assume` preconditions (the logical content).
    // Sampling `bound`s are not part of the property body, so nothing
    // technical leaks into the theorem. `ctx` / `w` are already bound.
    let bound_ctx = LeanExprCtx::for_spec(
        program,
        entity,
        spec_state_var(entity),
        bound_params,
        HashSet::new(),
        profile,
    )
    .with_world("w", "inst")
    .with_deploy_bindings(deploy_bindings.clone());
    emit_prefix_let_bindings(&mut out, 1, &prefix_lets, &bound_ctx);
    let mut hypotheses: Vec<String> = Vec::new();
    for step in &p.body {
        if let TestStep::Assume { cond } = step {
            hypotheses.push(gen_expr(cond, &bound_ctx));
        }
    }
    for h in &hypotheses {
        push_indent(&mut out, 1);
        out.push_str(h);
        out.push_str(" →\n");
    }

    let lower_ctx = SpecStepCtx {
        program,
        entity,
        params,
        lets: all_lets,
        deploy_bindings,
        profile,
    };

    let body_steps = filter_property_body_after_hoist(&p.body, &hoist.hoisted_names);
    let events = lower_steps(program, entity, p, &body_steps, diags);
    let throw_targets = collect_throw_targets(&events);
    let proof_shape = SpecStatementShape::from_events(&events, entity);
    emit_body(&events, 0, &throw_targets, 1, &lower_ctx, &mut out);
    PropertyTheorem {
        comments,
        slug,
        ty: out,
        proof_shape,
    }
}

/// Render the property proof tail (the `:= by …` string) glued to the
/// preceding declaration head.
///
/// A `property` quantifies over its parameters (and any forall-ized state /
/// context), with `assume` preconditions threaded as `→`-hypotheses. The
/// ladder runs `simp` *contextually* — so the `assume` hypotheses in the
/// goal's implication chain are available to discharge a fallible route's
/// guard — over the shared reflection set (see
/// [`reflection_simp_base`](super::spec::reflection_simp_base)). The
/// `simp` traverses the `∀`/`let`/`→` prefix (no manual `intro` needed,
/// which avoids the let-becomes-opaque-local pitfall), reflecting each route
/// call into its arithmetic. The first rung settles exact equalities; the
/// second adds `cambrian_bitvec_simp` + `omega` for `BitVec` arithmetic;
/// anything left falls through to `sorry`.
///
// ---------------------------------------------------------------------------
// Event lowering (body emission shared via lean_spec_steps — B-17)
// ---------------------------------------------------------------------------

fn lower_steps(
    program: &Program,
    entity: &Entity,
    p: &PropertyDecl,
    body: &[TestStep],
    diags: &mut Vec<SpecDiag>,
) -> Vec<SpecEvent> {
    let mut events = Vec::new();
    // A dropped cross-contract call leaves `_result` bound to the previous
    // call. Any return expectation that follows is then asserted against the
    // wrong value — a type error when the shapes differ, a false theorem when
    // they happen to match.
    let mut result_is_stale = false;
    let mut peer_bindings: HashMap<String, (String, String)> = HashMap::new();
    let mut deploy_init_applied: HashSet<String> = HashSet::new();
    let mut active_sut_inst: Option<String> = None;
    for step in body {
        match step {
            TestStep::Let { name, ty, value } => events.push(SpecEvent::Let {
                name: name.clone(),
                ty: ty.clone(),
                value: value.clone(),
            }),
            TestStep::SetContext { namespace, fields } => match namespace.as_str() {
                "msg" => events.push(SpecEvent::SetMsgCtx {
                    fields: fields.clone(),
                }),
                "sys" => events.push(SpecEvent::SetSysCtx {
                    fields: fields.clone(),
                }),
                other => {
                    events.push(SpecEvent::Comment(format!(
                        "WARNING: unknown context namespace '{}' (T8) — skipped",
                        other,
                    )));
                    diags.push(SpecDiag::warn(format!(
                        "property '{}': unknown context namespace '{}' (T8)",
                        p.name, other,
                    )));
                }
            },
            TestStep::SetRegistry { entity_name, .. } => {
                events.push(SpecEvent::Comment(format!(
                    "TODO: SetRegistry for '{}' skipped on Lean target (TVM-only, mirrors E10)",
                    entity_name,
                )));
                diags.push(SpecDiag::warn(format!(
                    "property '{}': SetRegistry for '{}' skipped on Lean target",
                    p.name, entity_name,
                )));
            }
            TestStep::DeployPeer {
                binding,
                entity: peer,
                args,
                init_state,
            } => {
                let inst_var = alloc_peer_inst(binding);
                peer_bindings.insert(
                    binding.clone(),
                    (peer.clone(), inst_var.clone()),
                );
                events.push(SpecEvent::InstallPeer {
                    peer_entity: peer.clone(),
                    inst_var: inst_var.clone(),
                    args: args.clone(),
                    init_state: init_state.clone(),
                });
                if let Some(peer_ent) = program.entities.iter().find(|e| e.name == *peer) {
                    if let Some(init_ev) =
                        deploy_positional_init_event(program, peer_ent, &inst_var, args)
                    {
                        events.push(init_ev);
                        deploy_init_applied.insert(binding.clone());
                    }
                }
                note_sut_deploy_instance(
                    &mut active_sut_inst,
                    &entity.name,
                    peer,
                    &inst_var,
                );
            }
            TestStep::Call {
                target: Some(binding),
                route,
                args,
            } => {
                let (peer_entity, inst_var) = match peer_bindings.get(binding) {
                    Some(p) => p.clone(),
                    None => {
                        events.push(dropped_step_comment(
                            &format!("call {}.{}", binding, route),
                            "undeclared deploy binding",
                        ));
                        result_is_stale = true;
                        continue;
                    }
                };
                let peer_ent = program
                    .entities
                    .iter()
                    .find(|e| e.name == peer_entity);
                if let Some(ent) = peer_ent {
                    if init_call_folded_into_deploy(
                        ent,
                        route,
                        binding,
                        &deploy_init_applied,
                    ) {
                        events.push(SpecEvent::Comment(format!(
                            "call {}.{} folded into deploy positional init",
                            binding, route
                        )));
                        result_is_stale = false;
                        continue;
                    }
                }
                let r = match peer_ent.and_then(|e| e.routes.iter().find(|r| r.name == *route)) {
                    Some(r) => r,
                    None => {
                        events.push(SpecEvent::Comment(format!(
                            "WARNING: call to unknown route '{}' on peer '{}' (T2)",
                            route, peer_entity,
                        )));
                        continue;
                    }
                };
                let fail = route_fail_mode(program, peer_ent.unwrap(), r);
                let view = route_is_view(r);
                let call = if fail {
                    SpecEvent::WrappedCall {
                        route_name: route.clone(),
                        args: args.clone(),
                        is_view: view,
                        callee_entity: Some(peer_entity),
                        callee_inst: Some(inst_var),
                    }
                } else {
                    SpecEvent::RawCall {
                        route_name: route.clone(),
                        args: args.clone(),
                        is_view: view,
                        callee_entity: Some(peer_entity),
                        callee_inst: Some(inst_var),
                    }
                };
                events.push(call);
                result_is_stale = false;
            }
            TestStep::Call { target: None, route, args } => {
                let resolved = entity.routes.iter().find(|r| &r.name == route);
                let r = match resolved {
                    Some(r) => r,
                    None => {
                        events.push(SpecEvent::Comment(format!(
                            "WARNING: call to unknown route '{}' (T2)",
                            route,
                        )));
                        continue;
                    }
                };
                let fail = route_fail_mode(program, entity, r);
                let view = route_is_view(r);
                let self_inst = sut_self_call_instance(&active_sut_inst);
                let ev = if fail {
                    SpecEvent::WrappedCall {
                        route_name: route.clone(),
                        args: args.clone(),
                        is_view: view,
                        callee_entity: None,
                        callee_inst: self_inst.clone(),
                    }
                } else {
                    SpecEvent::RawCall {
                        route_name: route.clone(),
                        args: args.clone(),
                        is_view: view,
                        callee_entity: None,
                        callee_inst: self_inst,
                    }
                };
                events.push(ev);
                result_is_stale = false;
            }
            TestStep::ExpectState { fields } => {
                events.push(SpecEvent::ExpectState(fields.clone()));
            }
            TestStep::ExpectThrow { code } => {
                if result_is_stale {
                    events.push(stale_result_comment("expect throw"));
                } else {
                    events.push(SpecEvent::ExpectThrow { code: *code });
                }
            }
            TestStep::ExpectReturn { value } => {
                if result_is_stale {
                    events.push(stale_result_comment("expect return"));
                } else {
                    events.push(SpecEvent::ExpectReturn {
                        value: value.clone(),
                    });
                }
            }
            TestStep::ExpectReturnTuple { values } => {
                if result_is_stale {
                    events.push(stale_result_comment("expect return tuple"));
                } else {
                    events.push(SpecEvent::ExpectReturnTuple {
                        values: values.clone(),
                    });
                }
            }
            TestStep::ExpectReturnLens { path, value } => {
                if result_is_stale {
                    events.push(stale_result_comment("expect return lens"));
                } else {
                    events.push(SpecEvent::ExpectReturnLens {
                        path: path.clone(),
                        value: value.clone(),
                    });
                }
            }
            TestStep::ExpectPred { cond } => {
                let mentions_result = crate::ast::expr_mentions_ident(cond, crate::ast::EXPECT_RESULT_NAME);
                if mentions_result && result_is_stale {
                    events.push(stale_result_comment("expect"));
                } else {
                    events.push(SpecEvent::ExpectPred { cond: cond.clone() });
                }
            }
            TestStep::ExpectEffects { elements } => {
                events.extend(lower_expect_effects(
                    elements,
                    diags,
                    &format!("property '{}'", p.name),
                ));
            }
            TestStep::ExpectEmit { event_name, args } => {
                events.push(SpecEvent::ExpectEmit {
                    event_name: event_name.clone(),
                    args: args.clone(),
                });
            }
            TestStep::Assume { .. } => {}
            TestStep::Bound { .. } | TestStep::SkipIf { .. } | TestStep::AdvanceTime { .. } => {
                events.push(SpecEvent::Comment(
                    "TODO: bound/skipIf/advanceTime not allowed in property body".to_string(),
                ));
            }
        }
    }
    events
}

#[allow(dead_code)]
fn _silence_unused(_: &Param) {}
