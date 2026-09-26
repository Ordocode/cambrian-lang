// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean codegen — `InvariantDecl` → `Action` inductive + `step` +
//! `runTrace` + theorem (P2.3).
//!
//! For each *single-entity* `InvariantDecl` whose `_self` instance
//! belongs to `entity`, we emit a sub-namespace
//! `<E>.Spec.Invariants.<inv_slug>` containing:
//!
//!   1. `inductive Action` — one constructor per declared route action,
//!      with a literal-bound refinement parameter when the action's
//!      `bound` clauses are over literal endpoints. State-dependent
//!      bounds (the `LendingPair` kitchen-sink) are *not* refined on
//!      the constructor; instead, the `step` arm wraps its body in an
//!      `if` guard that no-ops on violation (skip-if-violated, matching
//!      `vm.assume` semantics).
//!   2. Optional `def utilization (s : E.State) : T := …` view helpers
//!      from `derived` blocks.
//!   3. `def senders : List Cambrian.Address := [...]` (positive set
//!      after `exclude senders` subtraction); when empty *and* no
//!      excludes, we drop the sender parameter entirely.
//!   4. `def step` — single dispatch on `Action`, threading `(s, ctx)`
//!      as needed. Mixed routes lift to `Cambrian.RouteResult`.
//!   5. `def runTrace` — fold of `step` over a `List Action`.
//!   6. `theorem <inv_slug>` — `∀ trace, … → <checks>`.
//!
//! Multi-entity invariants (`for { v: Vault, t: Treasury } …`) emit a
//! single harness in the **owner** entity's spec file (the first
//! instance in the `instances { … }` block). Other entities only pull
//! in cross-entity `Routes` imports via [`lean_spec`](super::spec).

use std::collections::{HashMap, HashSet};

use crate::ast::{
    ContextSpec, Entity, Expr, ForallSpec, ForallTarget, FuzzDecl, InvariantDecl, Param, Program,
    Route, TestStep, Type,
};
use crate::codegen::evm_test_codegen::{InitRouteArg, InitRouteFill, resolve_init_route_args};
use crate::codegen::invariant_harness;

use super::super::core::emitter::{doc_comment, line_comment, push_deriving, push_indent};
use super::super::core::types::{default_for_type, lower_type, LeanTypeCtx};
use super::super::expr::{gen_expr, LeanExprCtx};
use super::super::route::{route_fail_mode, route_is_view};
use super::invariant_shape::InvariantProofShape;
use super::spec::{slugify, SpecBlock, SpecDiag};
use super::spec_prefix;
use super::world::entity_field_name;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// A single invariant's emitted output. Under the default profile `model` is
/// `None` and `proof` carries the whole namespace block (byte-identical to
/// pre-B1). Under the predictable profile the invariant's *model* machinery
/// (`Action` / `step` / `runTrace` / `senders` / …) **and** its statement
/// `abbrev` land in `model` (destined for `<E>Statements.lean`, since the
/// statement quantifies over that machinery), while `proof` keeps only the
/// `invByCases` scaffolding + the `theorem … : <slug>.statement := by …`
/// shell (destined for `<E>Spec.lean`). Both wrap the same `namespace <slug>`.
struct SpecItem {
    model: Option<String>,
    proof: String,
}

/// The separable pieces of an invariant `theorem`: any leading doc comment
/// (`#[fail_on_revert]` note) and the statement `type` body (ending *without*
/// a trailing newline). The caller wraps them as an inline `theorem` (legacy)
/// or splits them into an `abbrev … : Prop` + `theorem … : <slug>.statement`
/// (predictable). Under predictable profile the `type` carries `RouteResult.okImplies` in place
/// of the permissive `match … | .error _ => True | .ok w => …`.
struct InvTheorem {
    lead_comment: String,
    ty: String,
}

pub(crate) fn gen_invariants_block(
    program: &Program,
    entity: &Entity,
    diags: &mut Vec<SpecDiag>,
    profile: super::super::LeanProfile,
) -> Option<SpecBlock> {
    // Single-entity invariants targeting this entity, plus multi-entity
    // ones that *mention* this entity (so we still emit the skip
    // comment in the appropriate per-entity file).
    let single: Vec<&InvariantDecl> = program
        .invariants
        .iter()
        .filter(|inv| {
            inv.emit_policy != crate::ast::InvariantEmitPolicy::Superseded
                && inv.is_single_entity()
                && inv.entity_name() == entity.name
        })
        .collect();
    let multi: Vec<&InvariantDecl> = program
        .invariants
        .iter()
        .filter(|inv| {
            inv.emit_policy != crate::ast::InvariantEmitPolicy::Superseded
                && !inv.is_single_entity()
                && inv.instances.iter().any(|i| i.entity == entity.name)
        })
        .collect();

    if single.is_empty() && multi.is_empty() {
        return None;
    }

    let predictable = profile.predictable;
    let ns = format!("{}.Spec.Invariants", entity.name);
    let mut proofs = format!("namespace {}\n\n", ns);
    let mut stmts = if predictable {
        format!("namespace {}\n\n", ns)
    } else {
        String::new()
    };

    let push_item = |item: SpecItem, proofs: &mut String, stmts: &mut String| {
        proofs.push_str(&item.proof);
        proofs.push('\n');
        if let Some(model) = item.model {
            stmts.push_str(&model);
            stmts.push('\n');
        }
    };

    for inv in &single {
        let item = gen_single_entity_invariant(program, entity, inv, diags, profile);
        push_item(item, &mut proofs, &mut stmts);
    }

    for inv in &multi {
        let owner = inv
            .instances
            .first()
            .map(|i| i.entity.as_str())
            .unwrap_or("");
        if owner == entity.name {
            let item = gen_multi_entity_invariant(program, inv, diags, profile);
            push_item(item, &mut proofs, &mut stmts);
        }
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
// Single-entity invariant emission
// ---------------------------------------------------------------------------

fn gen_single_entity_invariant(
    program: &Program,
    entity: &Entity,
    inv: &InvariantDecl,
    diags: &mut Vec<SpecDiag>,
    profile: super::super::LeanProfile,
) -> SpecItem {
    let slug = slugify(&inv.name);
    // `out` accumulates the invariant model machinery (namespace open, senders,
    // `Action`, `step`, `runTrace`, …). Under legacy it *is* the whole block;
    // under predictable profile (B1) it becomes the `<E>Statements.lean` model, capped with
    // the statement abbrev, while the proof file gets the theorem shell.
    let mut out = String::new();
    out.push_str(&format!("namespace {}\n\n", slug));

    if let Some(tag) = &inv.tag {
        line_comment(&mut out, 0, &format!("tag: {}", tag));
    }
    if let Some(runs) = inv.runs {
        line_comment(&mut out, 0, &format!("runs hint: {}", runs));
    }
    if let Some(depth) = inv.depth {
        line_comment(&mut out, 0, &format!("depth hint: {}", depth));
    }
    if inv.skip_from {
        line_comment(
            &mut out,
            0,
            "WARNING: 'skip from' is not yet honoured by the Lean target — `from`-checks remain in place. (L7)",
        );
        diags.push(SpecDiag::warn(format!(
            "invariant '{}': 'skip from' is not honoured on Lean (L7)",
            inv.name,
        )));
    }

    // Resolve action -> route and analyse fail mode.
    let actions: Vec<ResolvedAction> = inv
        .actions
        .iter()
        .filter_map(|a| {
            entity
                .routes
                .iter()
                .find(|r| r.name == a.route)
                .map(|r| ResolvedAction {
                    ast: a,
                    route: r,
                    bounds: collect_bounds(a, entity),
                })
        })
        .filter(|a| !inv.exclude_selectors.iter().any(|s| s == &a.ast.route))
        .collect();

    let proof_shape =
        InvariantProofShape::from_route_names(actions.iter().map(|a| a.route.name.clone()));

    let any_throwing = actions
        .iter()
        .any(|a| route_fail_mode(program, entity, a.route));
    let with_time = inv.with_time;

    // Sender selection — `senders` minus `exclude_senders`. The
    // positive set drives the action constructor's `sender_idx`
    // parameter; when empty after subtraction, `step` keeps ambient
    // `ctx.sender` (any sender allowed — mirrors Foundry without
    // `targetSender`).
    let senders_ctx = LeanExprCtx::for_spec(
        program,
        entity,
        spec_state_var(entity),
        HashSet::new(),
        HashSet::new(),
        profile,
    )
    .with_world("w", "inst");
    let senders: Vec<String> = inv
        .senders
        .iter()
        .map(|e| gen_expr(e, &senders_ctx))
        .collect();
    let exclude_senders: Vec<String> = inv
        .exclude_senders
        .iter()
        .map(|e| gen_expr(e, &senders_ctx))
        .collect();
    let senders = filter_excluded_senders(senders, &exclude_senders);
    let has_senders = !senders.is_empty();

    doc_comment(
        &mut out,
        0,
        "Authorised senders for this invariant. Empty list ⇒ ambient `ctx.sender` (no `sender_idx` on `Action`).",
    );
    out.push_str("def senders : List Cambrian.Address :=\n");
    push_indent(&mut out, 1);
    if has_senders {
        out.push_str(&format!("[{}]\n\n", senders.join(", ")));
    } else {
        out.push_str("[]\n\n");
    }

    // `derived` views.
    for q in &inv.derived {
        emit_derived_query(program, entity, q, &mut out, profile);
    }

    // `track` bindings — not emitted as a top-level def; baked into the
    // theorem body. We do, however, render a doc comment listing them.
    if !inv.track.is_empty() {
        doc_comment(
            &mut out,
            0,
            &format!(
                "track bindings (captured against `s₀` in the theorem body): {}",
                inv.track
                    .iter()
                    .map(|b| b.name.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        );
    }

    // Action inductive.
    emit_action_inductive(
        program,
        entity,
        &actions,
        &mut out,
        with_time,
        has_senders,
        profile,
    );

    // Trace-aware machinery — emitted only when `assume` / `trace::*`
    // are actually used, so unused invariants stay byte-identical.
    let trace = scan_trace_usage(inv, &actions);
    let needs_acc = trace.needs_acc();
    if trace.shape.uses_last {
        emit_last_action(&actions, with_time, &mut out, profile);
    }
    if needs_acc {
        emit_trace_acc(&trace.shape, &mut out);
        emit_acc_update(
            &actions,
            &trace.shape,
            with_time,
            has_senders,
            &mut out,
            profile,
        );
    }

    // step + runTrace.
    emit_step(
        program,
        entity,
        &actions,
        with_time,
        any_throwing,
        has_senders,
        &mut out,
        profile,
    );

    // stepValid + traceValid (exclude semantics for `assume`).
    if trace.has_assume {
        emit_step_valid(
            program,
            entity,
            inv,
            &actions,
            with_time,
            has_senders,
            needs_acc,
            &mut out,
            profile,
        );
        emit_trace_valid(
            entity,
            with_time,
            any_throwing,
            needs_acc,
            &mut out,
            profile,
        );
    }
    if trace.checks_use_trace {
        emit_final_acc(&mut out, profile);
    }

    emit_run_trace(
        entity,
        with_time,
        any_throwing,
        inv.fail_on_revert,
        &mut out,
        profile,
    );

    // Theorem type + proof pieces (escrow rewrites the permissive `match … |
    // .error _ => True | .ok w => …` to a `RouteResult.okImplies` call).
    let thm = build_invariant_theorem(
        program,
        entity,
        inv,
        &actions,
        any_throwing,
        with_time,
        &trace,
        diags,
        profile,
    );

    if !profile.predictable {
        // Legacy: scaffolding tactic + inline theorem + close, all in `out`
        // (byte-identical to pre-B1).
        if !proof_shape.needs_sorry_stub() {
            emit_inv_by_cases_macro(entity, &mut out, profile);
        }
        out.push_str(&thm.lead_comment);
        out.push_str(&format!("theorem {} :\n{}", slug, thm.ty));
        emit_invariant_proof_tail(&mut out, profile, &proof_shape);
        out.push_str(&format!("end {}\n\n", slug));
        return SpecItem {
            model: None,
            proof: out,
        };
    }

    // Escrow: `out` holds the relocated model machinery — cap it with the
    // statement abbrev; the proof file keeps the scaffolding tactic + the
    // theorem shell that references that abbrev.
    out.push_str(&format!(
        "abbrev {}.statement : Prop :=\n{}\n\n",
        slug, thm.ty,
    ));
    out.push_str(&format!("end {}\n\n", slug));

    let mut proof = format!("namespace {}\n\n", slug);
    if !proof_shape.needs_sorry_stub() {
        emit_inv_by_cases_macro(entity, &mut proof, profile);
    }
    proof.push_str(&thm.lead_comment);
    proof.push_str(&format!("theorem {} : {}.statement", slug, slug));
    emit_invariant_proof_tail(&mut proof, profile, &proof_shape);
    proof.push_str(&format!("end {}\n\n", slug));

    SpecItem {
        model: Some(out),
        proof,
    }
}

// ---------------------------------------------------------------------------
// Multi-entity invariant emission (P4b.5)
// ---------------------------------------------------------------------------

struct MultiResolvedAction<'a> {
    instance: String,
    target: &'a Entity,
    route: &'a Route,
    #[allow(dead_code)]
    ast: &'a crate::ast::InvariantAction,
}

fn multi_action_variant(instance: &str, route: &str) -> String {
    format!(
        "{}_{}",
        super::super::core::types::sanitize_variant(instance),
        super::super::core::types::sanitize_variant(route),
    )
}

fn gen_multi_entity_invariant(
    program: &Program,
    inv: &InvariantDecl,
    diags: &mut Vec<SpecDiag>,
    profile: super::super::LeanProfile,
) -> SpecItem {
    let owner_name = inv
        .instances
        .first()
        .map(|i| i.entity.as_str())
        .unwrap_or("");
    let owner = program
        .entities
        .iter()
        .find(|e| e.name == owner_name)
        .expect("multi-entity invariant owner entity must exist");

    let slug = slugify(&inv.name);
    let mut out = String::new();
    out.push_str(&format!("namespace {}\n\n", slug));

    if let Some(tag) = &inv.tag {
        line_comment(&mut out, 0, &format!("tag: {}", tag));
    }
    if inv.skip_from {
        line_comment(
            &mut out,
            0,
            "WARNING: 'skip from' is not yet honoured by the Lean target — `from`-checks remain in place. (L7)",
        );
        diags.push(SpecDiag::warn(format!(
            "invariant '{}': 'skip from' is not honoured on Lean (L7)",
            inv.name,
        )));
    }

    let mut qualified_instances: HashMap<String, (String, String)> = HashMap::new();
    for inst in &inv.instances {
        qualified_instances.insert(
            inst.name.clone(),
            (inst.entity.clone(), format!("inst_{}", inst.name)),
        );
    }

    let actions: Vec<MultiResolvedAction<'_>> = inv
        .actions
        .iter()
        .filter_map(|a| {
            let slot = inv.instances.iter().find(|i| i.name == a.instance)?;
            let target = program.entities.iter().find(|e| e.name == slot.entity)?;
            target
                .routes
                .iter()
                .find(|r| r.name == a.route)
                .map(|r| MultiResolvedAction {
                    instance: a.instance.clone(),
                    target,
                    route: r,
                    ast: a,
                })
        })
        .filter(|a| {
            !inv.exclude_selectors
                .iter()
                .any(|s| s == &multi_action_variant(&a.instance, &a.route.name))
        })
        .collect();

    let proof_shape =
        InvariantProofShape::from_route_names(actions.iter().map(|a| a.route.name.clone()));

    let any_throwing = actions
        .iter()
        .any(|a| route_fail_mode(program, a.target, a.route));
    let with_time = inv.with_time;

    let senders_ctx = multi_spec_ctx(
        program,
        owner,
        &qualified_instances,
        HashSet::new(),
        HashSet::new(),
        profile,
    );
    let senders: Vec<String> = inv
        .senders
        .iter()
        .map(|e| gen_expr(e, &senders_ctx))
        .collect();
    let exclude_senders: Vec<String> = inv
        .exclude_senders
        .iter()
        .map(|e| gen_expr(e, &senders_ctx))
        .collect();
    let senders = filter_excluded_senders(senders, &exclude_senders);
    let has_senders = !senders.is_empty();

    doc_comment(
        &mut out,
        0,
        "Authorised senders for this multi-entity invariant. Empty list ⇒ ambient `ctx.sender`.",
    );
    out.push_str("def senders : List Cambrian.Address :=\n");
    push_indent(&mut out, 1);
    if has_senders {
        out.push_str(&format!("[{}]\n\n", senders.join(", ")));
    } else {
        out.push_str("[]\n\n");
    }

    emit_multi_action_inductive(program, &actions, &mut out, with_time, has_senders, profile);
    emit_multi_step(
        program,
        &actions,
        &inv.instances,
        with_time,
        any_throwing,
        has_senders,
        &mut out,
        profile,
    );
    // Upstream (4982b9d): `emit_multi_run_trace` gained `fail_on_revert` for the
    // T-X-007 / LEAN-H9 continue-past-revert semantics. Our B1 split relocated
    // the scaffolding tactic + theorem out of this linear position — legacy
    // emits `emit_inv_by_cases_macro` + inline theorem below, predictable routes them
    // into the `proof` half — so upstream's inline `emit_inv_by_cases_macro` /
    // `emit_multi_invariant_theorem` calls are intentionally dropped here.
    emit_multi_run_trace(
        &inv.instances,
        with_time,
        any_throwing,
        inv.fail_on_revert,
        &mut out,
        profile,
    );

    let ty = build_multi_invariant_theorem(
        program,
        owner,
        inv,
        &actions,
        any_throwing,
        with_time,
        &qualified_instances,
        profile,
    );

    if !profile.predictable {
        // Legacy: scaffolding tactic + inline theorem + close (byte-identical).
        if !proof_shape.needs_sorry_stub() {
            emit_inv_by_cases_macro(owner, &mut out, profile);
        }
        out.push_str(&format!("theorem {} :\n{}", slug, ty));
        emit_invariant_proof_tail(&mut out, profile, &proof_shape);
        out.push_str(&format!("end {}\n\n", slug));
        return SpecItem {
            model: None,
            proof: out,
        };
    }

    // Escrow: `out` holds the relocated model machinery — cap it with the
    // statement abbrev; the proof file gets the scaffolding + theorem shell.
    out.push_str(&format!("abbrev {}.statement : Prop :=\n{}\n\n", slug, ty,));
    out.push_str(&format!("end {}\n\n", slug));

    let mut proof = format!("namespace {}\n\n", slug);
    if !proof_shape.needs_sorry_stub() {
        emit_inv_by_cases_macro(owner, &mut proof, profile);
    }
    proof.push_str(&format!("theorem {} : {}.statement", slug, slug));
    emit_invariant_proof_tail(&mut proof, profile, &proof_shape);
    proof.push_str(&format!("end {}\n\n", slug));

    SpecItem {
        model: Some(out),
        proof,
    }
}

fn multi_spec_ctx<'a>(
    program: &'a Program,
    owner: &'a Entity,
    qualified_instances: &HashMap<String, (String, String)>,
    params: HashSet<String>,
    lets: HashSet<String>,
    profile: super::super::LeanProfile,
) -> LeanExprCtx<'a> {
    let mut ctx = LeanExprCtx::for_spec(program, owner, "s_unused", params, lets, profile)
        .with_world("w", "inst");
    ctx.qualified_instances = qualified_instances.clone();
    ctx
}

fn emit_multi_action_inductive(
    program: &Program,
    actions: &[MultiResolvedAction<'_>],
    out: &mut String,
    with_time: bool,
    has_senders: bool,
    profile: super::super::LeanProfile,
) {
    out.push_str("inductive Action where\n");
    for a in actions {
        let type_ctx = LeanTypeCtx::for_entity(
            program,
            &a.target.name,
            &a.target.records,
            &a.target.enums,
            &a.target.type_aliases,
            profile,
        );
        push_indent(out, 1);
        out.push_str(&format!(
            "| {}",
            multi_action_variant(&a.instance, &a.route.name)
        ));
        for p in &a.route.params {
            out.push_str(&format!(" ({} : {})", p.name, lower_type(&p.ty, &type_ctx)));
        }
        if has_senders {
            out.push_str(" (sender_idx : Nat)");
        }
        // Sampling `bound`s are technical hints for the Rust fuzzer, not
        // logical preconditions; they are intentionally dropped from the
        // Lean Action so theorems quantify over the full parameter range.
        out.push('\n');
    }
    if with_time {
        push_indent(out, 1);
        out.push_str("| advanceTime (delta : BitVec 64)\n");
    }
    push_deriving(out, profile.predictable, &["Repr"]);
}

fn multi_inst_sig(instances: &[crate::ast::InvariantInstance]) -> String {
    instances
        .iter()
        .map(|i| format!(" (inst_{} : {}.Identity)", i.name, i.entity))
        .collect::<Vec<_>>()
        .join("")
}

fn multi_inst_args(instances: &[crate::ast::InvariantInstance]) -> String {
    instances
        .iter()
        .map(|i| format!("inst_{}", i.name))
        .collect::<Vec<_>>()
        .join(" ")
}

fn emit_multi_step(
    program: &Program,
    actions: &[MultiResolvedAction<'_>],
    instances: &[crate::ast::InvariantInstance],
    with_time: bool,
    any_throwing: bool,
    has_senders: bool,
    out: &mut String,
    profile: super::super::LeanProfile,
) {
    let result_ty = if with_time {
        "Cambrian.Generated.World × Cambrian.MsgCtx".to_string()
    } else {
        "Cambrian.Generated.World".to_string()
    };
    let wrapped = if any_throwing {
        format!("Cambrian.RouteResult ({})", result_ty)
    } else {
        result_ty
    };

    // B8 (predictable): dispatch through `Action.casesOn` instead of a fun-match —
    // symmetric with the single-entity `emit_step`. No `step.match_1` matcher.
    // Legacy keeps the byte-identical fun-match.
    let predictable = profile.predictable;

    out.push_str("def step (w : Cambrian.Generated.World) (ctx : Cambrian.MsgCtx)");
    out.push_str(&multi_inst_sig(instances));
    out.push_str(" :\n");
    push_indent(out, 2);
    if predictable {
        out.push_str(&format!("Action → {} :=\n", wrapped));
        out.push_str(&format!(
            "  fun a => Action.casesOn (motive := fun _ => {}) a\n",
            wrapped,
        ));
    } else {
        out.push_str(&format!("Action → {}\n", wrapped));
    }

    for a in actions {
        let variant = multi_action_variant(&a.instance, &a.route.name);
        let inst_var = format!("inst_{}", a.instance);

        // Arm body: optional sender update + the (fail-mode-shaped) route call.
        // Identical bytes in both profiles; only the dispatch scaffolding
        // around it differs.
        let mut body = String::new();
        if has_senders {
            push_indent(&mut body, 2);
            body.push_str(
                "let ctx := { ctx with sender := senders.getD (sender_idx % senders.length) default }\n",
            );
        }
        push_indent(&mut body, 2);
        if route_is_init(a.route) {
            emit_step_noop(with_time, any_throwing, &mut body);
        } else {
            let route_args: String = a
                .route
                .params
                .iter()
                .map(|p| p.name.clone())
                .collect::<Vec<_>>()
                .join(" ");
            let arg_suffix = if route_args.is_empty() {
                String::new()
            } else {
                format!(" {}", route_args)
            };
            let call = format!(
                "{}.Routes.{} w {} ctx{}",
                a.target.name, a.route.name, inst_var, arg_suffix,
            );
            let route_fail = route_fail_mode(program, a.target, a.route);
            // L10 (15a7bc8): the send/var-call elision note must land inside the arm
            // `body` (shared by both profiles, dispatch-wrapped below), mirroring
            // single-entity `emit_step_call` — NOT the dispatch stream `out`.
            if route_has_send_effect(a.route) {
                body.push_str(&format!(
                    "-- L10: invariant step for '{}' elides send / var-call interleavings (atomic WorldState → WorldState)\n",
                    a.route.name,
                ));
                push_indent(&mut body, 2);
            }
            body.push_str(&format_step_result(
                &call,
                with_time,
                any_throwing,
                route_fail,
                route_is_view(a.route),
                predictable,
            ));
        }

        if predictable {
            // One `Action.casesOn` minor premise (nullary ctor ⇒ plain term,
            // otherwise a `(fun <fields> => …)` lambda).
            push_cases_arm(out, &a.route.params, has_senders, &body);
        } else {
            push_indent(out, 1);
            out.push_str(&format!("| .{}", variant));
            for p in &a.route.params {
                out.push_str(&format!(" {}", p.name));
            }
            if has_senders {
                out.push_str(" sender_idx");
            }
            // Literal-bound refinement hypotheses were dropped from the Action
            // constructor, so there is nothing to bind here.
            out.push_str(" =>\n");
            out.push_str(&body);
        }
    }
    if with_time {
        if predictable {
            push_indent(out, 2);
            out.push_str("(fun delta =>\n");
            push_indent(out, 3);
            if any_throwing {
                out.push_str(".ok (w, { ctx with timestamp := ctx.timestamp + delta }))\n");
            } else {
                out.push_str("(w, { ctx with timestamp := ctx.timestamp + delta }))\n");
            }
        } else {
            push_indent(out, 1);
            out.push_str("| .advanceTime delta =>\n");
            push_indent(out, 2);
            if any_throwing {
                out.push_str(".ok (w, { ctx with timestamp := ctx.timestamp + delta })\n");
            } else {
                out.push_str("(w, { ctx with timestamp := ctx.timestamp + delta })\n");
            }
        }
    }
    out.push('\n');
}

fn emit_multi_run_trace(
    instances: &[crate::ast::InvariantInstance],
    with_time: bool,
    any_throwing: bool,
    fail_on_revert: bool,
    out: &mut String,
    profile: super::super::LeanProfile,
) {
    let inst_args = multi_inst_args(instances);
    let result_ty = if with_time {
        "Cambrian.Generated.World × Cambrian.MsgCtx".to_string()
    } else {
        "Cambrian.Generated.World".to_string()
    };
    let wrapped = if any_throwing {
        format!("Cambrian.RouteResult ({})", result_ty)
    } else {
        result_ty
    };
    // B5 (predictable): non-recursive fold — symmetric with the single-entity
    // `emit_run_trace` (see that function for the three-way fold-form split and
    // the `fail_on_revert:false` revert-continue rationale). `step` takes
    // `(w) (ctx) <inst…>`, so the fold lambda threads `w`/`ctx` and passes the
    // fixed instances. Legacy keeps the byte-identical structural recursion.
    let predictable = profile.predictable;
    if predictable {
        out.push_str("def runTrace (w₀ : Cambrian.Generated.World) (ctx : Cambrian.MsgCtx)");
        out.push_str(&multi_inst_sig(instances));
        out.push_str(" (trace : List Action) :\n");
        push_indent(out, 2);
        out.push_str(&format!("{} :=\n", wrapped));
        push_indent(out, 1);
        let inst_pass = if inst_args.is_empty() {
            String::new()
        } else {
            format!("{} ", inst_args)
        };
        if any_throwing && !fail_on_revert {
            // Revert-continue: catch each `.error` and keep the accumulator.
            if with_time {
                out.push_str(&format!(
                    ".ok (trace.foldl (fun p a => (step p.fst p.snd {}a).toOption.getD p) (w₀, ctx))\n\n",
                    inst_pass,
                ));
            } else {
                out.push_str(&format!(
                    ".ok (trace.foldl (fun w a => (step w ctx {}a).toOption.getD w) w₀)\n\n",
                    inst_pass,
                ));
            }
        } else {
            // Total (`foldl`) or propagate-stop (`foldlM`). Ascribe the
            // accumulator (B-13) so the elaborator does not pick `PUnit`.
            let fold = if any_throwing { "foldlM" } else { "foldl" };
            if with_time {
                out.push_str(&format!(
                    "trace.{} (fun (p : Cambrian.Generated.World × Cambrian.MsgCtx) a => step p.fst p.snd {}a) (w₀, ctx)\n\n",
                    fold, inst_pass,
                ));
            } else {
                out.push_str(&format!(
                    "trace.{} (fun (w : Cambrian.Generated.World) a => step w ctx {}a) w₀\n\n",
                    fold, inst_pass,
                ));
            }
        }
        return;
    }

    out.push_str("def runTrace (w₀ : Cambrian.Generated.World) (ctx : Cambrian.MsgCtx)");
    out.push_str(&multi_inst_sig(instances));
    out.push_str(" :\n");
    push_indent(out, 2);
    out.push_str(&format!("List Action → {}\n", wrapped));
    let inst_pass = if inst_args.is_empty() {
        String::new()
    } else {
        format!(" {} ", inst_args)
    };
    push_indent(out, 1);
    match (with_time, any_throwing) {
        (false, false) => {
            out.push_str("| []        => w₀\n");
            push_indent(out, 1);
            out.push_str(&format!(
                "| a :: rest => runTrace (step w₀ ctx{}a) ctx{}rest\n\n",
                inst_pass, inst_pass
            ));
        }
        (false, true) => {
            out.push_str("| []        => .ok w₀\n");
            push_indent(out, 1);
            out.push_str(&format!(
                "| a :: rest => match step w₀ ctx{}a with\n",
                inst_pass
            ));
            push_indent(out, 2);
            // See `emit_run_trace` — `fail_on_revert: false` continues the
            // trace past a reverted action (T-X-007 / LEAN-H9).
            if fail_on_revert {
                out.push_str("| .error e => .error e\n");
            } else {
                out.push_str(&format!("| .error _ => runTrace w₀ ctx{}rest\n", inst_pass));
            }
            push_indent(out, 2);
            out.push_str(&format!(
                "| .ok w'   => runTrace w' ctx{}rest\n\n",
                inst_pass
            ));
        }
        (true, false) => {
            out.push_str("| []        => (w₀, ctx)\n");
            push_indent(out, 1);
            out.push_str(&format!(
                "| a :: rest => let p := step w₀ ctx{}a; runTrace p.fst p.snd{}rest\n\n",
                inst_pass, inst_pass
            ));
        }
        (true, true) => {
            out.push_str("| []        => .ok (w₀, ctx)\n");
            push_indent(out, 1);
            out.push_str(&format!(
                "| a :: rest => match step w₀ ctx{}a with\n",
                inst_pass
            ));
            push_indent(out, 2);
            if fail_on_revert {
                out.push_str("| .error e => .error e\n");
            } else {
                out.push_str(&format!("| .error _ => runTrace w₀ ctx{}rest\n", inst_pass));
            }
            push_indent(out, 2);
            out.push_str(&format!(
                "| .ok p    => runTrace p.fst p.snd{}rest\n\n",
                inst_pass
            ));
        }
    }
}

/// Build the multi-entity invariant theorem's *type* (no header, no proof
/// tail, no trailing newline). Multi-entity invariants carry no leading doc
/// comment, so — unlike [`build_invariant_theorem`] — this returns the bare
/// type string. Escrow rewrites the permissive `match … | .error _ => True |
/// .ok w => …` to `RouteResult.okImplies`.
fn build_multi_invariant_theorem(
    program: &Program,
    owner: &Entity,
    inv: &InvariantDecl,
    actions: &[MultiResolvedAction<'_>],
    any_throwing: bool,
    with_time: bool,
    qualified_instances: &HashMap<String, (String, String)>,
    profile: super::super::LeanProfile,
) -> String {
    let _ = actions;
    let predictable = profile.predictable;
    let mut ty = String::new();
    let out = &mut ty;
    let slug = slugify(&inv.name);
    let _ = &slug;
    let check_ctx = multi_spec_ctx(
        program,
        owner,
        qualified_instances,
        HashSet::new(),
        HashSet::new(),
        profile,
    );
    let checks_text = render_checks(&inv.checks, &check_ctx);

    // Resolve per-instance forall init. Variables are prefixed with the
    // instance name to stay unique across instances.
    let mut all_quant: Vec<(String, String)> = Vec::new();
    let mut all_vars: HashSet<String> = HashSet::new();
    let mut eff_inits: HashMap<String, Vec<(String, Expr)>> = HashMap::new();
    for inst in &inv.instances {
        let ent = program
            .entities
            .iter()
            .find(|e| e.name == inst.entity)
            .expect("instance entity");
        let tcx = type_ctx_for(program, ent, profile);
        let (q, pins, vars) = resolve_forall_init(
            ent,
            &inst.init,
            &inst.forall_state,
            &tcx,
            &format!("{}_", inst.name),
        );
        all_quant.extend(q);
        all_vars.extend(vars);
        let mut eff = inst.init.clone();
        eff.extend(pins);
        eff_inits.insert(inst.name.clone(), eff);
    }

    // Initial context from the invariant's `ctx { ... }` block: `msg::` seeds
    // the `MsgCtx`, `sys::` seeds the world (`sys::balance` is single-entity
    // only — multi-entity has no single "self", so pass `None`).
    let mut ctx_used: HashSet<String> = all_vars.clone();
    let (ctx_quant, ctx_line, block_line, _balance_line) =
        invariant_ctx_setup(&inv.context, &check_ctx, &mut ctx_used, None);
    all_quant.extend(ctx_quant);

    push_indent(out, 1);
    out.push_str("∀ (trace : List Action),\n");
    emit_forall_quant(out, &all_quant);
    // Bind `ctx` before init instances / world (UPSTREAM B-26).
    push_indent(out, 1);
    out.push_str(&ctx_line);
    // Sibling-pinned instances (`init vlt { m_asset: tok }`) read the pinned
    // sibling's binder, so declare binders in pin-dependency order.
    for idx in instance_decl_order(&inv.instances) {
        let inst = &inv.instances[idx];
        let ent = program
            .entities
            .iter()
            .find(|e| e.name == inst.entity)
            .expect("instance entity");
        // UPSTREAM B-26: init seeds without world binding (see single-entity).
        let mut init_ctx = LeanExprCtx::for_spec(
            program,
            ent,
            "s_unused",
            all_vars.clone(),
            HashSet::new(),
            profile,
        );
        init_ctx.qualified_instances = qualified_instances.clone();
        let eff = eff_inits.get(&inst.name).expect("eff init");
        let siblings: Vec<(&str, &str)> = inv
            .instances
            .iter()
            .filter(|other| other.name != inst.name)
            .map(|other| (other.name.as_str(), other.entity.as_str()))
            .collect();
        push_indent(out, 1);
        out.push_str(&format_init_instance_named(
            ent,
            eff,
            &format!("inst_{}", inst.name),
            &init_ctx,
            &siblings,
        ));
        out.push('\n');
    }
    push_indent(out, 1);
    // World assembly may still reference sys fields for balance/block overlays
    // via `world_ctx`; keep world binding there. Init state seeds above are
    // world-free.
    let world_ctx = multi_spec_ctx(
        program,
        owner,
        qualified_instances,
        all_vars.clone(),
        HashSet::new(),
        profile,
    );
    out.push_str(&format_init_multi_world(
        program, inv, &eff_inits, &world_ctx,
    ));
    out.push('\n');
    if let Some(bl) = &block_line {
        push_indent(out, 1);
        out.push_str(bl);
    }

    let inst_a = multi_inst_args(&inv.instances);
    let run_trace_call = if inst_a.is_empty() {
        "runTrace w ctx trace".to_string()
    } else {
        format!("runTrace w ctx {} trace", inst_a)
    };
    if any_throwing {
        // Executable shape (Option A): compute the world so only `trace` is
        // sampled. `okImplies` = permissive (default); `okAnd` when
        // `#[fail_on_revert]` (PN-104). Both profiles use the Core helper —
        // no raw `| .error _ => True` match in the statement.
        let wrapper = if inv.fail_on_revert {
            "Cambrian.RouteResult.okAnd"
        } else {
            "Cambrian.RouteResult.okImplies"
        };
        let _ = predictable;
        push_indent(out, 1);
        out.push_str(&format!("{wrapper} ({run_trace_call}) (fun "));
        if with_time {
            // L7 (§53): bracket + align the `let` body — see the
            // single-entity `build_invariant_theorem` for why an
            // over-indented body is an escrow-only LAKE failure.
            out.push_str(&format!("p =>\n   (let w := p.fst\n    {}))", checks_text));
        } else {
            out.push_str(&format!("w =>\n   {})", checks_text));
        }
    } else if with_time {
        push_indent(out, 1);
        out.push_str(&format!("let p := {}\n", run_trace_call));
        push_indent(out, 1);
        out.push_str("let w := p.fst\n");
        push_indent(out, 1);
        out.push_str(&checks_text);
    } else {
        push_indent(out, 1);
        out.push_str(&format!("let w := {}\n", run_trace_call));
        push_indent(out, 1);
        out.push_str(&checks_text);
    }
    ty
}

/// Resolve an instance's `*` forall spec into quantifier declarations and
/// synthetic init pins. State fields turn into `∀`-bound variables that seed
/// the instance's initial state; `all_state` expands to every non-pinned
/// member. Context (`msg::`/`sys::`) forall targets are not modelled in the
/// stateful-invariant initial state and are ignored here.
///
/// Returns `(quantifiers, pins, var_names)` where `quantifiers` is a list of
/// `(var, lean_type)`, `pins` map member names to `Ident(var)`, and
/// `var_names` is the set of introduced variable names (for the lowering
/// context's locals).
fn resolve_forall_init(
    entity: &Entity,
    init: &[(String, Expr)],
    spec: &ForallSpec,
    type_ctx: &LeanTypeCtx,
    var_prefix: &str,
) -> (Vec<(String, String)>, Vec<(String, Expr)>, HashSet<String>) {
    let pinned: HashSet<&str> = init.iter().map(|(n, _)| n.as_str()).collect();
    let mut fields: Vec<(String, Type)> = Vec::new();
    if spec.all_state {
        for m in &entity.members {
            if !pinned.contains(m.name.as_str()) {
                fields.push((m.name.clone(), m.ty.clone()));
            }
        }
    }
    for t in &spec.targets {
        if let ForallTarget::StateField(f) = t {
            if pinned.contains(f.as_str()) || fields.iter().any(|(n, _)| n == f) {
                continue;
            }
            if let Some(m) = entity.members.iter().find(|m| &m.name == f) {
                fields.push((f.clone(), m.ty.clone()));
            }
        }
    }
    let mut quant = Vec::new();
    let mut pins = Vec::new();
    let mut vars = HashSet::new();
    for (f, ty) in &fields {
        let var = format!("{}{}", var_prefix, f);
        quant.push((var.clone(), lower_type(ty, type_ctx)));
        pins.push((f.clone(), Expr::Ident(var.clone())));
        vars.insert(var.clone());
    }
    (quant, pins, vars)
}

/// Emit an indented `∀ (v : T) …,` line for the resolved forall quantifiers,
/// or nothing when there are none.
fn emit_forall_quant(out: &mut String, quant: &[(String, String)]) {
    if quant.is_empty() {
        return;
    }
    push_indent(out, 1);
    out.push('∀');
    for (name, ty) in quant {
        out.push_str(&format!(" ({} : {})", name, ty));
    }
    out.push_str(",\n");
}

/// Build the initial context setup for an invariant from its `ctx { ... }`
/// block. Shared with tests/properties via [`spec_prefix::ctx_setup`];
/// `sys::now` is also mirrored onto `ctx.timestamp` so init seeds without
/// `world_var` observe the seeded clock (B-26).
fn invariant_ctx_setup(
    ctx_spec: &ContextSpec,
    lower_ctx: &LeanExprCtx<'_>,
    used: &mut HashSet<String>,
    self_balance: Option<(&str, &str)>,
) -> (
    Vec<(String, String)>,
    String,
    Option<String>,
    Option<String>,
) {
    let setup = spec_prefix::ctx_setup(
        ctx_spec,
        lower_ctx,
        used,
        self_balance,
        spec_prefix::alloc_ctx_var,
    );
    (
        setup.quant,
        setup.ctx_line,
        setup.block_line,
        setup.balance_line,
    )
}

/// Build a `LeanTypeCtx` for an entity (used to lower forall var types).
fn type_ctx_for<'a>(
    program: &'a Program,
    entity: &'a Entity,
    profile: super::super::LeanProfile,
) -> LeanTypeCtx<'a> {
    LeanTypeCtx::for_entity(
        program,
        &entity.name,
        &entity.records,
        &entity.enums,
        &entity.type_aliases,
        profile,
    )
}

/// A pin whose value is a bare sibling-instance name (`init vlt { m_asset:
/// tok }`) means "the address of that instance": render it as
/// `(<SiblingEntity>.address inst_<name>)`. Lowering it through `gen_expr`
/// instead would emit the raw ident `tok`, which is unbound in the theorem
/// statement and fails at `lake build` — the Lean twin of the Foundry-harness
/// sibling-pin bug (`identity_ctor_args_sol` rendering `tok` instead of
/// `address(_tok)`).
fn sibling_pin_term(e: &Expr, siblings: &[(&str, &str)]) -> Option<String> {
    let Expr::Ident(n) = e else { return None };
    siblings
        .iter()
        .find(|(name, _)| name == n)
        .map(|(name, entity)| format!("({}.address inst_{})", entity, name))
}

/// Declaration order for the per-instance `let inst_<n> : <E>.Identity`
/// binders: an instance whose init pins name a sibling must be declared after
/// it — the pin renders as `<E>.address inst_<sib>`, which reads the
/// sibling's binder. Stable (declaration order) except where a pin forces an
/// edge; the Lean mirror of the Foundry harness's `instance_deploy_order`.
fn instance_decl_order(instances: &[crate::ast::InvariantInstance]) -> Vec<usize> {
    let names: Vec<&str> = instances.iter().map(|i| i.name.as_str()).collect();
    let deps: Vec<Vec<usize>> = instances
        .iter()
        .map(|inst| {
            inst.init
                .iter()
                .filter_map(|(_, e)| match e {
                    Expr::Ident(n) => names.iter().position(|s| s == n),
                    _ => None,
                })
                .collect()
        })
        .collect();
    let mut order: Vec<usize> = Vec::with_capacity(instances.len());
    let mut placed = vec![false; instances.len()];
    while order.len() < instances.len() {
        let next = (0..instances.len())
            .find(|&i| !placed[i] && deps[i].iter().all(|&d| placed[d]))
            // A pin cycle cannot render either way; keep declaration order
            // and let `lake` report the unbound reference.
            .unwrap_or_else(|| (0..instances.len()).find(|&i| !placed[i]).unwrap());
        placed[next] = true;
        order.push(next);
    }
    order
}

fn format_init_instance_named(
    entity: &Entity,
    init: &[(String, Expr)],
    inst_var: &str,
    ctx: &LeanExprCtx<'_>,
    siblings: &[(&str, &str)],
) -> String {
    let pieces = identity_field_inits(entity, init, ctx, siblings);
    match pieces {
        None => format!("let {} : {}.Identity := {{}}", inst_var, entity.name),
        Some(pieces) => format!(
            "let {} : {}.Identity := {{ {} }}",
            inst_var,
            entity.name,
            pieces.join(", "),
        ),
    }
}

/// Every identity field, in declaration order, as `name := <term>`.
///
/// `None` when the entity declares no identity members — then `Identity` is a
/// singleton structure and `{}` is the only legal literal.
///
/// A field the `init { … }` block does not mention falls back to
/// `State.default`, matching what `lean_test` and `lean_property` do. Emitting
/// only the mentioned fields would be shorter and wrong: `{}` and a partial
/// literal are both rejected by Lean for a structure with required fields, so
/// an invariant that names no identity slot — the common case, since a
/// single-instance invariant has no reason to care which id it runs under —
/// would not compile at all.
fn identity_field_inits(
    entity: &Entity,
    init: &[(String, Expr)],
    ctx: &LeanExprCtx<'_>,
    siblings: &[(&str, &str)],
) -> Option<Vec<String>> {
    let id_members: Vec<&crate::ast::Member> =
        entity.members.iter().filter(|m| m.is_identity).collect();
    if id_members.is_empty() {
        return None;
    }
    Some(
        id_members
            .iter()
            .map(|m| match init.iter().find(|(n, _)| n == &m.name) {
                Some((_, e)) => match sibling_pin_term(e, siblings) {
                    Some(term) => format!("{} := {}", m.name, term),
                    None => format!("{} := {}", m.name, gen_expr(e, ctx)),
                },
                None => format!("{} := ({}.State.default).{}", m.name, entity.name, m.name),
            })
            .collect(),
    )
}

fn format_init_multi_world(
    program: &Program,
    inv: &InvariantDecl,
    eff_inits: &HashMap<String, Vec<(String, Expr)>>,
    ctx: &LeanExprCtx<'_>,
) -> String {
    let mut w_expr = "Cambrian.Generated.World.default".to_string();
    for inst in &inv.instances {
        let ent = program
            .entities
            .iter()
            .find(|e| e.name == inst.entity)
            .expect("instance entity");
        let inst_var = format!("inst_{}", inst.name);
        let id_names: HashSet<String> = ent
            .members
            .iter()
            .filter(|m| m.is_identity)
            .map(|m| m.name.clone())
            .collect();
        let inst_init = eff_inits
            .get(&inst.name)
            .map(|v| v.as_slice())
            .unwrap_or(&inst.init);
        let siblings: Vec<(&str, &str)> = inv
            .instances
            .iter()
            .filter(|other| other.name != inst.name)
            .map(|other| (other.name.as_str(), other.entity.as_str()))
            .collect();
        let non_id_inits: Vec<&(String, Expr)> = inst_init
            .iter()
            .filter(|(n, _)| !id_names.contains(n))
            .collect();
        let state_expr = if non_id_inits.is_empty() {
            format!("{}.State.default", ent.name)
        } else {
            let updates: Vec<String> = non_id_inits
                .iter()
                .map(|(name, expr)| match sibling_pin_term(expr, &siblings) {
                    Some(term) => format!("{} := {}", name, term),
                    None => format!("{} := {}", name, gen_expr(expr, ctx)),
                })
                .collect();
            format!(
                "({{ {}.State.default with {} }})",
                ent.name,
                updates.join(", "),
            )
        };
        w_expr = format!(
            "Cambrian.Generated.World.with{} ({}) {} {}",
            ent.name, w_expr, inst_var, state_expr,
        );
    }
    format!("let w : Cambrian.Generated.World := {}", w_expr)
}

// ---------------------------------------------------------------------------
// invByCases scaffolding tactic + invariant proof ladder
// ---------------------------------------------------------------------------

/// Emit a per-invariant `scoped macro "invByCases"`. It lives inside the
/// invariant's namespace (so the unqualified `step` / `runTrace` references
/// resolve, via macro hygiene, to *this* invariant's definitions) and is the
/// scaffolding the user reaches for when an obligation is genuinely
/// inductive: it peels `trace`, inducts on it, splits on every `Action`
/// constructor, and hits each arm with the reflection `simp` set + `omega`.
///
/// Trace-independent checks (the common case — e.g. an unsigned `≥ 0` bound)
/// close outright; anything that needs a strengthened induction hypothesis is
/// left as an honest subgoal for the user to finish.
fn emit_inv_by_cases_macro(entity: &Entity, out: &mut String, profile: super::super::LeanProfile) {
    // `lean.proof_helpers: false` ⇒ no scaffolding tactic at all.
    if !profile.proof_helpers {
        return;
    }
    let base = super::spec::reflection_simp_base(entity);
    doc_comment(
        out,
        0,
        "Scaffolding tactic: peel `trace`, induct, split on each `Action` \
constructor, and apply the reflection `simp` set + `omega`. Trace-independent \
checks close outright; genuinely inductive obligations are left as subgoals.",
    );
    out.push_str("scoped macro \"invByCases\" : tactic =>\n");
    out.push_str("  `(tactic|\n");
    out.push_str("    (intro trace;\n");
    out.push_str("     induction trace <;>\n");
    out.push_str("       intros <;>\n");
    out.push_str(&format!(
        "       (try simp_all [runTrace, step, {base}, cambrian_bitvec_simp] <;>\n"
    ));
    out.push_str("       (try omega))))\n\n");
}

/// Append the invariant proof ladder: try the `invByCases` scaffolding (and
/// require it to fully close via `done`), otherwise fall through to `sorry`
/// so the build never regresses for invariants that need a hand-written
/// strengthening.
fn emit_invariant_proof_tail(
    out: &mut String,
    profile: super::super::LeanProfile,
    shape: &InvariantProofShape,
) {
    // `lean.proof_helpers: false` ⇒ statement-only stub (no `invByCases`
    // is emitted in that mode, so the ladder cannot reference it anyway).
    if !profile.proof_helpers || shape.needs_sorry_stub() {
        out.push_str(" := by sorry\n\n");
        return;
    }
    // Heartbeat-bounded so a non-inductive obligation (the common case for a
    // genuinely interesting invariant) gives up quickly and falls through to
    // `sorry` instead of burning the declaration's full default budget. The
    // bound stays under Lean's 200000 default so the global timeout never
    // fires.
    out.push_str(" := by\n");
    out.push_str("  first\n");
    out.push_str("    | (set_option maxHeartbeats 80000 in (invByCases; done))\n");
    out.push_str("    | sorry\n\n");
}

// ---------------------------------------------------------------------------
// Action analysis
// ---------------------------------------------------------------------------

struct ResolvedAction<'a> {
    ast: &'a crate::ast::InvariantAction,
    route: &'a Route,
    bounds: Vec<BoundClause>,
}

#[derive(Clone)]
struct BoundClause {
    var: String,
    lo: Expr,
    hi: Expr,
    inclusive: bool,
    /// `true` when `lo`/`hi` reference entity members or use `derived`/
    /// `track` symbols — i.e. they cannot be resolved to a literal at
    /// `Action`-construction time. Forces the bound into the `step`
    /// arm as a skip-if-violated guard.
    state_dependent: bool,
}

fn collect_bounds(action: &crate::ast::InvariantAction, entity: &Entity) -> Vec<BoundClause> {
    let mut out = Vec::new();
    for step in &action.body {
        if let TestStep::Bound {
            var,
            lo,
            hi,
            inclusive,
        } = step
        {
            let state_dep = expr_references_member_or_derived(lo, entity)
                || expr_references_member_or_derived(hi, entity);
            out.push(BoundClause {
                var: var.clone(),
                lo: lo.clone(),
                hi: hi.clone(),
                inclusive: *inclusive,
                state_dependent: state_dep,
            });
        }
    }
    out
}

/// Conservative state-dependence check: any reference to an entity
/// member or a "derived" identifier (which we approximate by any
/// non-literal `Ident`) marks the bound as state-dependent.
fn expr_references_member_or_derived(expr: &Expr, entity: &Entity) -> bool {
    match expr {
        Expr::IntLiteral(_)
       
       
        | Expr::BoolLiteral(_)
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_) => false,
        Expr::Ident(name) => entity.members.iter().any(|m| &m.name == name),
        Expr::TemporalRef(_) => true,
        Expr::BinOp(l, _, r) => {
            expr_references_member_or_derived(l, entity)
                || expr_references_member_or_derived(r, entity)
        }
        Expr::UnaryOp(_, e) | Expr::Cast(e, _) | Expr::FieldAccess(e, _) => {
            expr_references_member_or_derived(e, entity)
        }
        Expr::FnCall(_, args) => args
            .iter()
            .any(|a| expr_references_member_or_derived(a, entity)),
        // Anything else is conservatively flagged.
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Trace-aware conditions (assume + trace::* accessors)
// ---------------------------------------------------------------------------

/// Minimal accumulator shape derived by scanning the invariant's
/// `assume` / `check` expressions. Only the fields actually referenced
/// are threaded through `traceValid` / `finalAcc`, so the heavy
/// machinery stays out of invariants that don't use it.
#[derive(Default)]
struct AccShape {
    uses_length: bool,
    counted: Vec<String>,
    uses_last: bool,
}

impl AccShape {
    fn is_empty(&self) -> bool {
        !self.uses_length && self.counted.is_empty() && !self.uses_last
    }
    fn add_count(&mut self, route: &str) {
        if !self.counted.iter().any(|r| r == route) {
            self.counted.push(route.to_string());
        }
    }
}

/// Result of scanning an invariant for trace-aware usage.
struct TraceUsage {
    /// Any surviving action declares an `assume` step.
    has_assume: bool,
    /// Union of trace accessors over `assume` + `check` expressions.
    shape: AccShape,
    /// At least one `check` expression references a `trace::` accessor.
    checks_use_trace: bool,
}

impl TraceUsage {
    /// The accumulator must be threaded when any `trace::` accessor is used.
    fn needs_acc(&self) -> bool {
        !self.shape.is_empty()
    }
}

fn collect_trace_in_expr(expr: &Expr, shape: &mut AccShape) {
    match expr {
        Expr::TraceField(field) => {
            if field == "length" {
                shape.uses_length = true;
            }
        }
        Expr::TraceCall { name, route } => match name.as_str() {
            "count" => shape.add_count(route),
            "lastWas" => shape.uses_last = true,
            _ => {}
        },
        Expr::BinOp(l, _, r) | Expr::Index(l, r) | Expr::Range(l, r) => {
            collect_trace_in_expr(l, shape);
            collect_trace_in_expr(r, shape);
        }
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) | Expr::Some(e) => {
            collect_trace_in_expr(e, shape)
        }
        Expr::FnCall(_, args)
        | Expr::ArrayLit(args)
        | Expr::Tuple(args)
        | Expr::EnumVariantWithData(_, _, args)
        | Expr::MacroRef(_, args)
        | Expr::NamespacedCall { args, .. } => {
            for a in args {
                collect_trace_in_expr(a, shape);
            }
        }
        Expr::MethodCall(recv, _, args) => {
            collect_trace_in_expr(recv, shape);
            for a in args {
                collect_trace_in_expr(a, shape);
            }
        }
        Expr::If(c, t, e) => {
            collect_trace_in_expr(c, shape);
            collect_trace_in_expr(t, shape);
            if let Some(e) = e {
                collect_trace_in_expr(e, shape);
            }
        }
        Expr::Let(_, v, b) => {
            collect_trace_in_expr(v, shape);
            collect_trace_in_expr(b, shape);
        }
        Expr::Block(items) => {
            for it in items {
                collect_trace_in_expr(it, shape);
            }
        }
        Expr::Match(s, arms) => {
            collect_trace_in_expr(s, shape);
            for arm in arms {
                collect_trace_in_expr(&arm.body, shape);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                collect_trace_in_expr(v, shape);
            }
        }
        Expr::RecordUpdate(b, fields) => {
            collect_trace_in_expr(b, shape);
            for (_, v) in fields {
                collect_trace_in_expr(v, shape);
            }
        }
        Expr::Closure(_, b) => collect_trace_in_expr(b, shape),
        Expr::For(_, it, b) => {
            collect_trace_in_expr(it, shape);
            collect_trace_in_expr(b, shape);
        }
        _ => {}
    }
}

/// Scan the surviving actions' `assume` conditions and the invariant's
/// `check` expressions for trace-aware usage.
fn scan_trace_usage(inv: &InvariantDecl, actions: &[ResolvedAction<'_>]) -> TraceUsage {
    let mut has_assume = false;
    let mut shape = AccShape::default();
    for a in actions {
        for step in &a.ast.body {
            if let TestStep::Assume { cond } = step {
                has_assume = true;
                collect_trace_in_expr(cond, &mut shape);
            }
        }
    }
    let mut checks_use_trace = false;
    for c in &inv.checks {
        let mut cshape = AccShape::default();
        collect_trace_in_expr(c, &mut cshape);
        if !cshape.is_empty() {
            checks_use_trace = true;
        }
        collect_trace_in_expr(c, &mut shape);
    }
    TraceUsage {
        has_assume,
        shape,
        checks_use_trace,
    }
}

/// `inductive LastAction` — a nullary tag per action (plus `none` and
/// `advanceTime`) used by `trace::lastWas(route)`.
fn emit_last_action(
    actions: &[ResolvedAction<'_>],
    with_time: bool,
    out: &mut String,
    profile: super::super::LeanProfile,
) {
    out.push_str("inductive LastAction where\n");
    push_indent(out, 1);
    out.push_str("| none\n");
    for a in actions {
        push_indent(out, 1);
        out.push_str(&format!("| {}\n", a.route.name));
    }
    if with_time {
        push_indent(out, 1);
        out.push_str("| advanceTime\n");
    }
    let predictable = profile.predictable;
    let derives =
        super::super::core::deceq::deceq_filtered(predictable, true, &["DecidableEq", "BEq", "Repr"]);
    push_deriving(out, predictable, &derives);
    if predictable {
        // LastAction is all-nullary (`none` + one tag per action + optional
        // `advanceTime`) → enum matrix template (arity 0 for every ctor).
        let mut ctors: Vec<(String, usize)> = vec![("none".to_string(), 0)];
        for a in actions {
            ctors.push((a.route.name.clone(), 0));
        }
        if with_time {
            ctors.push(("advanceTime".to_string(), 0));
        }
        super::super::core::deceq::emit_enum_deceq(out, "LastAction", &ctors);
    }
}

/// `structure TraceAcc` + `def accInit` — only the referenced fields.
fn emit_trace_acc(shape: &AccShape, out: &mut String) {
    out.push_str("structure TraceAcc where\n");
    if shape.uses_length {
        push_indent(out, 1);
        out.push_str("len : Nat\n");
    }
    for r in &shape.counted {
        push_indent(out, 1);
        out.push_str(&format!("count_{} : Nat\n", r));
    }
    if shape.uses_last {
        push_indent(out, 1);
        out.push_str("last : LastAction\n");
    }
    out.push('\n');

    out.push_str("def accInit : TraceAcc :=\n");
    push_indent(out, 1);
    let mut fields = Vec::new();
    if shape.uses_length {
        fields.push("len := 0".to_string());
    }
    for r in &shape.counted {
        fields.push(format!("count_{} := 0", r));
    }
    if shape.uses_last {
        fields.push("last := LastAction.none".to_string());
    }
    out.push_str(&format!("{{ {} }}\n\n", fields.join(", ")));
}

/// The per-step accumulator delta for one action `route`: the `{ acc with … }`
/// record-update term (or bare `acc` when this action touches no accumulator
/// field). Shared by the escrow `casesOn` premise and the legacy fun-match arm
/// so both profiles emit byte-identical arm bodies.
fn acc_update_delta(route: &str, shape: &AccShape) -> String {
    let mut updates = Vec::new();
    if shape.uses_length {
        updates.push("len := acc.len + 1".to_string());
    }
    if shape.counted.iter().any(|r| r == route) {
        updates.push(format!("count_{} := acc.count_{} + 1", route, route));
    }
    if shape.uses_last {
        updates.push(format!("last := LastAction.{}", route));
    }
    if updates.is_empty() {
        "acc".to_string()
    } else {
        format!("{{ acc with {} }}", updates.join(", "))
    }
}

/// `def accUpdate (acc) : Action → TraceAcc` — folds the per-step delta.
///
/// B8 (predictable): dispatch through the `Action.casesOn` auto-companion instead of
/// a fun-match, so no `accUpdate.match_1` matcher is minted (the same
/// equation-compiler artefact `emit_step` sidesteps). Legacy keeps the
/// byte-identical fun-match.
fn emit_acc_update(
    actions: &[ResolvedAction<'_>],
    shape: &AccShape,
    with_time: bool,
    has_senders: bool,
    out: &mut String,
    profile: super::super::LeanProfile,
) {
    let predictable = profile.predictable;
    if predictable {
        out.push_str("def accUpdate (acc : TraceAcc) : Action → TraceAcc :=\n");
        push_indent(out, 1);
        out.push_str("fun a => Action.casesOn (motive := fun _ => TraceAcc) a\n");
        for a in actions {
            let delta = acc_update_delta(&a.route.name, shape);
            push_cases_arm(out, &a.route.params, has_senders, &delta);
        }
        if with_time {
            // `advanceTime` — the last `Action` ctor, hence the last premise.
            let mut updates = Vec::new();
            if shape.uses_length {
                updates.push("len := acc.len + 1".to_string());
            }
            if shape.uses_last {
                updates.push("last := LastAction.advanceTime".to_string());
            }
            let delta = if updates.is_empty() {
                "acc".to_string()
            } else {
                format!("{{ acc with {} }}", updates.join(", "))
            };
            // Only the binder NAME is read by `push_cases_arm`; the type is
            // irrelevant (the `advanceTime` field is `BitVec 64`).
            let delta_param = [Param {
                name: "delta".to_string(),
                ty: Type::Simple("uint64".to_string()),
            }];
            push_cases_arm(out, &delta_param, false, &delta);
        }
        out.push('\n');
        return;
    }

    out.push_str("def accUpdate (acc : TraceAcc) : Action → TraceAcc\n");
    for a in actions {
        push_indent(out, 1);
        out.push_str(&format!("| .{}", a.route.name));
        for _ in &a.route.params {
            out.push_str(" _");
        }
        if has_senders {
            out.push_str(" _");
        }
        out.push_str(&format!(" => {}\n", acc_update_delta(&a.route.name, shape)));
    }
    if with_time {
        push_indent(out, 1);
        out.push_str("| .advanceTime _");
        let mut updates = Vec::new();
        if shape.uses_length {
            updates.push("len := acc.len + 1".to_string());
        }
        if shape.uses_last {
            updates.push("last := LastAction.advanceTime".to_string());
        }
        if updates.is_empty() {
            out.push_str(" => acc\n");
        } else {
            out.push_str(&format!(" => {{ acc with {} }}\n", updates.join(", ")));
        }
    }
    out.push('\n');
}

/// `def finalAcc (acc) : List Action → TraceAcc` — fold for `check`
/// expressions that reference trace state after the whole trace.
///
/// B5 (predictable): a non-recursive `List.foldl accUpdate acc` spine, so no
/// `finalAcc.brecOn`/`.match_N`/well-founded machinery is minted — the same
/// matcher-free rewrite `emit_run_trace` applies to `runTrace`. Legacy keeps
/// the byte-identical structural recursion.
fn emit_final_acc(out: &mut String, profile: super::super::LeanProfile) {
    if profile.predictable {
        out.push_str("def finalAcc (acc : TraceAcc) : List Action → TraceAcc :=\n");
        push_indent(out, 1);
        out.push_str("fun trace => trace.foldl accUpdate acc\n\n");
        return;
    }
    out.push_str("def finalAcc (acc : TraceAcc) : List Action → TraceAcc\n");
    push_indent(out, 1);
    out.push_str("| []        => acc\n");
    push_indent(out, 1);
    out.push_str("| a :: rest => finalAcc (accUpdate acc a) rest\n\n");
}

/// `def stepValid` — the per-action well-formedness predicate built from
/// each action's `assume` conditions (exclude semantics). Actions with
/// no `assume` are trivially valid.
fn emit_step_valid(
    program: &Program,
    entity: &Entity,
    inv: &InvariantDecl,
    actions: &[ResolvedAction<'_>],
    with_time: bool,
    has_senders: bool,
    needs_acc: bool,
    out: &mut String,
    profile: super::super::LeanProfile,
) {
    let derived_names: HashSet<String> = inv.derived.iter().map(|q| q.name.clone()).collect();
    out.push_str(&format!(
        "def stepValid (w : Cambrian.Generated.World) (inst : {}.Identity) (ctx : Cambrian.MsgCtx)",
        entity.name,
    ));
    if needs_acc {
        out.push_str(" (acc : TraceAcc)");
    }
    // B8 (predictable): dispatch through `Action.casesOn` instead of a fun-match, so
    // no `stepValid`/`accUpdate` matcher is minted. Legacy keeps the fun-match.
    let predictable = profile.predictable;
    if predictable {
        out.push_str(" : Action → Bool :=\n");
        push_indent(out, 1);
        out.push_str("fun a => Action.casesOn (motive := fun _ => Bool) a\n");
    } else {
        out.push_str(" : Action → Bool\n");
    }

    // Build the well-formedness body (the `assume` conjunction) for one action;
    // byte-identical in both profiles — only the dispatch scaffolding differs.
    let arm_body = |a: &ResolvedAction<'_>| -> String {
        let mut body = String::new();
        if has_senders {
            body.push_str(
                "let ctx := { ctx with sender := senders.getD (sender_idx % senders.length) default }\n",
            );
        }
        let params: HashSet<String> = a.route.params.iter().map(|p| p.name.clone()).collect();
        let mut ectx = LeanExprCtx::for_spec_with_derived(
            program,
            entity,
            spec_state_var(entity),
            params,
            HashSet::new(),
            derived_names.clone(),
            profile,
        )
        .with_world("w", "inst");
        if needs_acc {
            ectx = ectx.with_trace_acc("acc");
        }
        let conds: Vec<String> = a
            .ast
            .body
            .iter()
            .filter_map(|s| match s {
                TestStep::Assume { cond } => Some(format!("({})", gen_expr(cond, &ectx))),
                _ => None,
            })
            .collect();
        if conds.is_empty() {
            body.push_str("true");
        } else {
            body.push_str(&conds.join(" && "));
        }
        body
    };

    for a in actions {
        let body = arm_body(a);
        if predictable {
            push_cases_arm(out, &a.route.params, has_senders, &body);
        } else {
            push_indent(out, 1);
            out.push_str(&format!("| .{}", a.route.name));
            for p in &a.route.params {
                out.push_str(&format!(" {}", p.name));
            }
            if has_senders {
                out.push_str(" sender_idx");
            }
            out.push_str(" =>\n");
            push_indent(out, 2);
            // `body` may carry a leading `let ctx := …\n` for the sender case,
            // then the conjunction; re-indent the whole thing to level 2.
            out.push_str(&body.replace('\n', &format!("\n{}", "  ".repeat(2))));
            out.push('\n');
        }
    }
    if with_time {
        if predictable {
            let delta_param = [Param {
                name: "delta".to_string(),
                ty: Type::Simple("uint64".to_string()),
            }];
            push_cases_arm(out, &delta_param, false, "true");
        } else {
            push_indent(out, 1);
            out.push_str("| .advanceTime _ =>\n");
            push_indent(out, 2);
            out.push_str("true\n");
        }
    }
    out.push('\n');
}

/// The escrow (B5/L6) `traceValid` body: one non-recursive `List.foldl` over a
/// `(carrier, [acc,] valid)` tuple, `.2[.2]`-projected. Returned WITHOUT a
/// trailing newline.
///
/// The four `(with_time, any_throwing)` cells differ only in the *carrier* — the
/// component `step` threads — and therefore only in four strings: its type, the
/// per-step advance, how `stepValid` reads `(w, ctx)` out of it, and the seed:
///
/// | cell | carrier | advance | seed |
/// |---|---|---|---|
/// | `(f, f)` | `World` | `step s.1 inst ctx a` | `w₀` |
/// | `(t, f)` | `World × MsgCtx` | `step s.1.1 inst s.1.2 a` | `(w₀, ctx)` |
/// | `(f, t)` | `RouteResult World` | `s.1.bind (fun w => step w inst ctx a)` | `.ok w₀` |
/// | `(t, t)` | `RouteResult (World × MsgCtx)` | `s.1.bind (fun p => step p.1 inst p.2 a)` | `.ok (w₀, ctx)` |
///
/// Value-identity with the legacy structural recursion:
///
/// * TOTAL cells — `Bool.and` is associative with `true` as unit, and the
///   carrier advances on the PRE-step tuple exactly as the recursion's argument
///   does, so the accumulated conjunction is the recursion's `&&`-chain.
/// * THROWING cells — the recursion STOPS at the first `step` error and yields
///   `true` for the remaining suffix (`| .error _ => true`), keeping the prefix
///   conjunction *including* the erroring action's own `stepValid`. The fold
///   reproduces that exactly: `Except.bind` makes the carrier absorbing once it
///   is `.error` (so no later `step` runs), and the contribution of a step whose
///   carrier is already `.error` is `(none.map …).getD true = true`, the unit of
///   `&&`. The erroring action itself still contributes, because the carrier is
///   still `.ok` when its own contribution is computed. The accumulator keeps
///   folding past the error, which is unobservable: every later contribution is
///   the `true` unit regardless of `acc`.
///
/// No `match` / structural recursion anywhere in the result, hence no
/// `traceValid.brecOn` / `.match_N` in the digest zone. Legacy keeps the
/// byte-identical recursion (see [`emit_trace_valid`]'s tail).
fn predictable_trace_valid_fold(with_time: bool, any_throwing: bool, needs_acc: bool) -> String {
    // Accumulator component of the state tuple (present iff some `assume` /
    // trace `check` reads `trace::*`).
    let (acc_comp, acc_seed, bool_proj, acc_arg, tail_proj) = if needs_acc {
        ("accUpdate s.2.1 a, ", "acc, ", "s.2.2", " s.2.1", "2.2")
    } else {
        ("", "", "s.2", "", "2")
    };

    let world = "Cambrian.Generated.World";
    let pair = "Cambrian.Generated.World × Cambrian.MsgCtx";
    let (carrier_ty, advance, contrib, seed) = match (with_time, any_throwing) {
        (false, false) => (
            world.to_string(),
            "step s.1 inst ctx a".to_string(),
            format!("stepValid s.1 inst ctx{} a", acc_arg),
            "w₀".to_string(),
        ),
        (true, false) => (
            format!("({})", pair),
            "step s.1.1 inst s.1.2 a".to_string(),
            format!("stepValid s.1.1 inst s.1.2{} a", acc_arg),
            "(w₀, ctx)".to_string(),
        ),
        (false, true) => (
            format!("Cambrian.RouteResult {}", world),
            "s.1.bind (fun w => step w inst ctx a)".to_string(),
            format!(
                "(s.1.toOption.map (fun w => stepValid w inst ctx{} a)).getD true",
                acc_arg,
            ),
            "(.ok w₀)".to_string(),
        ),
        (true, true) => (
            format!("Cambrian.RouteResult ({})", pair),
            "s.1.bind (fun p => step p.1 inst p.2 a)".to_string(),
            format!(
                "(s.1.toOption.map (fun p => stepValid p.1 inst p.2{} a)).getD true",
                acc_arg,
            ),
            "(.ok (w₀, ctx))".to_string(),
        ),
    };

    format!(
        "fun trace => (trace.foldl (fun (s : {} × {}Bool) a => \
         ({}, {}{} && {})) ({}, {}true)).{}",
        carrier_ty,
        if needs_acc { "TraceAcc × " } else { "" },
        advance,
        acc_comp,
        bool_proj,
        contrib,
        seed,
        acc_seed,
        tail_proj,
    )
}

/// `def traceValid` — fold `stepValid` over the trace, advancing the
/// world via `step` and the accumulator via `accUpdate`. The
/// theorem's hypothesis `traceValid … = true` restricts quantification
/// to well-formed traces (exclude semantics).
fn emit_trace_valid(
    entity: &Entity,
    with_time: bool,
    any_throwing: bool,
    needs_acc: bool,
    out: &mut String,
    profile: super::super::LeanProfile,
) {
    out.push_str(&format!(
        "def traceValid (w₀ : Cambrian.Generated.World) (inst : {}.Identity) (ctx : Cambrian.MsgCtx)",
        entity.name,
    ));
    if needs_acc {
        out.push_str(" (acc : TraceAcc)");
    }

    // B5 (predictable): a non-recursive `List.foldl` spine over a `(carrier, acc,
    // valid)` tuple, so no `traceValid.brecOn`/`.match_N` matcher is minted (the
    // same rewrite `emit_run_trace`/`emit_final_acc` apply). `Bool.and` is
    // associative with `true` as unit, so the accumulated conjunction is
    // value-identical to the recursive `&&`-fold, with the carrier / accumulator
    // threaded identically (`step`/`accUpdate` on the PRE-step tuple, exactly as
    // the recursion advances them).
    //
    // L6 (§53): the fold now covers ALL FOUR `(with_time, any_throwing)` cells.
    // The three non-total ones used to fall through to the legacy structural
    // recursion, which is reachable by a one-liner (`assume` over a route with a
    // `where … : throw N` guard) and mints BOTH `List.brecOn` and
    // `traceValid.match_N` into the digest zone — silently (`rc = 0`, `lake`
    // green). See [`predictable_trace_valid_fold`] for the per-cell carrier algebra.
    if profile.predictable {
        out.push_str(" : List Action → Bool :=\n");
        push_indent(out, 1);
        out.push_str(&predictable_trace_valid_fold(with_time, any_throwing, needs_acc));
        out.push_str("\n\n");
        return;
    }

    out.push_str(" : List Action → Bool\n");
    push_indent(out, 1);
    out.push_str("| []        => true\n");
    push_indent(out, 1);

    let acc_arg = if needs_acc { " acc" } else { "" };
    let acc_upd = if needs_acc { " (accUpdate acc a)" } else { "" };
    let sv = format!("stepValid w₀ inst ctx{} a", acc_arg);
    match (with_time, any_throwing) {
        (false, false) => {
            out.push_str(&format!(
                "| a :: rest => {} && traceValid (step w₀ inst ctx a) inst ctx{} rest\n\n",
                sv, acc_upd,
            ));
        }
        (false, true) => {
            out.push_str(&format!("| a :: rest => {} &&\n", sv));
            push_indent(out, 2);
            out.push_str("(match step w₀ inst ctx a with\n");
            push_indent(out, 3);
            out.push_str("| .error _ => true\n");
            push_indent(out, 3);
            out.push_str(&format!(
                "| .ok w'   => traceValid w' inst ctx{} rest)\n\n",
                acc_upd,
            ));
        }
        (true, false) => {
            out.push_str(&format!("| a :: rest => {} &&\n", sv));
            push_indent(out, 2);
            out.push_str(&format!(
                "(let p := step w₀ inst ctx a; traceValid p.fst inst p.snd{} rest)\n\n",
                acc_upd,
            ));
        }
        (true, true) => {
            out.push_str(&format!("| a :: rest => {} &&\n", sv));
            push_indent(out, 2);
            out.push_str("(match step w₀ inst ctx a with\n");
            push_indent(out, 3);
            out.push_str("| .error _ => true\n");
            push_indent(out, 3);
            out.push_str(&format!(
                "| .ok p    => traceValid p.fst inst p.snd{} rest)\n\n",
                acc_upd,
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Action inductive
// ---------------------------------------------------------------------------

fn emit_action_inductive(
    program: &Program,
    entity: &Entity,
    actions: &[ResolvedAction<'_>],
    out: &mut String,
    with_time: bool,
    has_senders: bool,
    profile: super::super::LeanProfile,
) {
    let type_ctx = LeanTypeCtx::for_entity(
        program,
        &entity.name,
        &entity.records,
        &entity.enums,
        &entity.type_aliases,
        profile,
    );
    out.push_str("inductive Action where\n");
    for a in actions {
        push_indent(out, 1);
        out.push_str(&format!("| {}", a.route.name));
        for p in &a.route.params {
            out.push_str(&format!(" ({} : {})", p.name, lower_type(&p.ty, &type_ctx)));
        }
        if has_senders {
            out.push_str(" (sender_idx : Nat)");
        }
        // Sampling `bound`s are technical fuzzing hints, not logical
        // preconditions; they are dropped from the Lean Action so the
        // theorem quantifies over the full parameter range. State-dependent
        // bounds still survive as runtime guards inside `step`.
        out.push('\n');
    }
    if with_time {
        push_indent(out, 1);
        out.push_str("| advanceTime (delta : BitVec 64)\n");
    }
    push_deriving(out, type_ctx.use_predictable_profile, &["Repr"]);
}

/// True when `e` is a numeric literal whose lowered Lean form is a
/// `Nat`. Such forms have no `.toNat`, so callers must not append it
/// when comparing against a `BitVec.toNat`-derived value.
fn is_numeric_literal(e: &Expr) -> bool {
    matches!(
        e,
        Expr::IntLiteral(_),
    )
}

/// Emit `<expr>.toNat` for a non-literal expression and the bare
/// rendering for a literal (since literals lower to `Nat` directly).
fn render_bound_endpoint(e: &Expr, ctx: &LeanExprCtx<'_>) -> String {
    let s = gen_expr(e, ctx);
    if is_numeric_literal(e) {
        s
    } else {
        format!("({}).toNat", s)
    }
}

// ---------------------------------------------------------------------------
// Derived helpers
// ---------------------------------------------------------------------------

fn emit_derived_query(
    program: &Program,
    entity: &Entity,
    q: &crate::ast::InvariantQuery,
    out: &mut String,
    profile: super::super::LeanProfile,
) {
    let type_ctx = LeanTypeCtx::for_entity(
        program,
        &entity.name,
        &entity.records,
        &entity.enums,
        &entity.type_aliases,
        profile,
    );
    let params: HashSet<String> = q.params.iter().map(|p| p.name.clone()).collect();
    let lets: HashSet<String> = q
        .body
        .iter()
        .filter_map(|s| match s {
            TestStep::Let { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();
    let ec = LeanExprCtx::for_spec(program, entity, "s", params, lets, profile);

    doc_comment(out, 0, &format!("Derived view helper `{}`.", q.name));
    out.push_str(&format!("def {} (s : {}.State)", q.name, entity.name));
    for p in &q.params {
        out.push_str(&format!(
            " ({} : {})",
            super::super::core::types::lean_safe_ident(&p.name),
            lower_type(&p.ty, &type_ctx),
        ));
    }
    out.push_str(&format!(
        " : {} :=\n",
        lower_type(&q.return_type, &type_ctx)
    ));
    // Emit any `let`-bindings, then the `return` expression.
    for step in &q.body {
        if let TestStep::Let { name, ty, value } = step {
            push_indent(out, 1);
            out.push_str(&format!(
                "let {} := {}\n",
                super::super::core::types::lean_safe_ident(name),
                super::spec_steps::gen_expr_for_spec_let(value, &ec, ty.as_ref()),
            ));
        }
    }
    push_indent(out, 1);
    out.push_str(&gen_expr(&q.return_value, &ec));
    out.push_str("\n\n");
}

// ---------------------------------------------------------------------------
// step
// ---------------------------------------------------------------------------

/// Emit one `Action.casesOn` minor premise (predictable profile, B8). A
/// constructor WITH fields (route params and/or `sender_idx`) yields a
/// `(fun <fields> => <body>)` lambda, fields in constructor order (route
/// params, then `sender_idx`). A NULLARY constructor (no route params AND no
/// `sender_idx`) yields a PLAIN TERM `(<body>)`: a `casesOn` minor premise for
/// a zero-field constructor takes no binders, and `(fun => …)` is an
/// empty-binder parse error in Lean. The `(` sits on its own line so a leading
/// `-- L10` comment inside `body` is never glued into `(--`.
fn push_cases_arm(out: &mut String, params: &[Param], has_senders: bool, body: &str) {
    let trimmed = body.trim_end_matches('\n');
    push_indent(out, 2);
    if params.is_empty() && !has_senders {
        out.push_str("(\n");
        out.push_str(trimmed);
        out.push_str(")\n");
    } else {
        out.push_str("(fun");
        for p in params {
            out.push_str(&format!(" {}", p.name));
        }
        if has_senders {
            out.push_str(" sender_idx");
        }
        out.push_str(" =>\n");
        out.push_str(trimmed);
        out.push_str(")\n");
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_step(
    program: &Program,
    entity: &Entity,
    actions: &[ResolvedAction<'_>],
    with_time: bool,
    any_throwing: bool,
    has_senders: bool,
    out: &mut String,
    profile: super::super::LeanProfile,
) {
    // Signature. P3: `step` operates on `(w : World) (inst : Identity)
    // (ctx : MsgCtx)`. Route entry-points now produce
    // `Cambrian.Generated.World`, so `step`'s result type swaps from
    // `<E>.State[ × MsgCtx]` to `Cambrian.Generated.World[ × MsgCtx]`.
    let result_ty = if with_time {
        "Cambrian.Generated.World × Cambrian.MsgCtx".to_string()
    } else {
        "Cambrian.Generated.World".to_string()
    };
    let wrapped_result = if any_throwing {
        format!("Cambrian.RouteResult ({})", result_ty)
    } else {
        result_ty.clone()
    };

    // B8 (predictable): dispatch through the `Action.casesOn` auto-companion instead
    // of a fun-match, so no `step.match_1` matcher is minted (the last
    // equation-compiler artefact of the zone disappears — closed structurally,
    // not by predicting `Match.mkMatcher`). Legacy keeps the byte-identical
    // fun-match.
    let predictable = profile.predictable;

    out.push_str(&format!(
        "def step (w : Cambrian.Generated.World) (inst : {}.Identity) (ctx : Cambrian.MsgCtx) :\n",
        entity.name,
    ));
    push_indent(out, 2);
    if predictable {
        out.push_str(&format!("Action → {} :=\n", wrapped_result));
        // Motive spelled explicitly: the result type is non-dependent, so
        // inference *can* recover `fun _ => <result>`, but the explicit form is
        // deterministic (predictor-friendly) and never enters motive HO-unif.
        out.push_str(&format!(
            "  fun a => Action.casesOn (motive := fun _ => {}) a\n",
            wrapped_result,
        ));
    } else {
        out.push_str(&format!("Action → {}\n", wrapped_result));
    }

    // Each arm.
    for a in actions {
        // Arm body (sender update + optional state guard + call). Identical
        // bytes in both profiles; only the dispatch scaffolding around it —
        // `| .ctor … =>` (legacy) vs a `(fun … =>)` minor premise (predictable) —
        // differs.
        let mut body = String::new();

        // Sender update.
        if has_senders {
            push_indent(&mut body, 2);
            body.push_str(
                "let ctx := { ctx with sender := senders.getD (sender_idx % senders.length) default }\n",
            );
        }

        // State-dependent guards.
        let state_bounds: Vec<&BoundClause> =
            a.bounds.iter().filter(|b| b.state_dependent).collect();
        let skip_if_conds: Vec<String> = a
            .ast
            .body
            .iter()
            .filter_map(|step| match step {
                TestStep::SkipIf { cond } => {
                    Some(gen_expr(cond, &spec_ctx_for_step(program, entity, profile)))
                }
                _ => None,
            })
            .collect();
        let needs_guard = !state_bounds.is_empty() || !skip_if_conds.is_empty();

        if needs_guard {
            push_indent(&mut body, 2);
            body.push_str("if ");
            // Skip-if conditions are inverted: the action runs when NONE of them are true.
            let mut conds: Vec<String> = Vec::new();
            for c in &skip_if_conds {
                conds.push(format!("!({})", c));
            }
            for b in &state_bounds {
                let ctx = spec_ctx_for_step(program, entity, profile);
                let lo = render_bound_endpoint(&b.lo, &ctx);
                let hi = render_bound_endpoint(&b.hi, &ctx);
                let cmp = if b.inclusive { "≤" } else { "<" };
                conds.push(format!(
                    "({} ≤ {}.toNat ∧ {}.toNat {} {})",
                    lo, b.var, b.var, cmp, hi,
                ));
            }
            // B8: arm body is built into `body` (dispatch-wrapped later); the
            // guarded call sits at indent 3. Upstream (15a7bc8) added the
            // `indent` arg so `emit_step_call` re-indents its L10 note correctly.
            body.push_str(&conds.join(" ∧ "));
            body.push_str(" then\n");
            push_indent(&mut body, 3);
            emit_step_call(program, entity, a, with_time, any_throwing, 3, &mut body);
            push_indent(&mut body, 2);
            body.push_str("else ");
            emit_step_noop(with_time, any_throwing, &mut body);
            body.push('\n');
        } else {
            push_indent(&mut body, 2);
            emit_step_call(program, entity, a, with_time, any_throwing, 2, &mut body);
        }

        if predictable {
            // One `Action.casesOn` minor premise (nullary ctor ⇒ plain term,
            // otherwise a `(fun <fields> => …)` lambda).
            push_cases_arm(out, &a.route.params, has_senders, &body);
        } else {
            push_indent(out, 1);
            out.push_str(&format!("| .{}", a.route.name));
            for p in &a.route.params {
                out.push_str(&format!(" {}", p.name));
            }
            if has_senders {
                out.push_str(" sender_idx");
            }
            // Literal-bound refinement hypotheses were dropped from the Action
            // constructor, so there is nothing to bind here.
            out.push_str(" =>\n");
            // B8: the (upstream-fixed, L10-annotated) arm `body` — already built
            // via `emit_step_call` above — is emitted here under the fun-match arm.
            out.push_str(&body);
        }
    }
    if with_time {
        // `advanceTime` is the last `Action` constructor, hence the last
        // `casesOn` minor premise under predictable profile.
        if predictable {
            push_indent(out, 2);
            out.push_str("(fun delta =>\n");
            push_indent(out, 3);
            if any_throwing {
                out.push_str(".ok (w, { ctx with timestamp := ctx.timestamp + delta }))\n");
            } else {
                out.push_str("(w, { ctx with timestamp := ctx.timestamp + delta }))\n");
            }
        } else {
            push_indent(out, 1);
            out.push_str("| .advanceTime delta =>\n");
            push_indent(out, 2);
            if any_throwing {
                out.push_str(".ok (w, { ctx with timestamp := ctx.timestamp + delta })\n");
            } else {
                out.push_str("(w, { ctx with timestamp := ctx.timestamp + delta })\n");
            }
        }
    }
    out.push('\n');
}

fn spec_ctx_for_step<'a>(
    program: &'a Program,
    entity: &'a Entity,
    profile: super::super::LeanProfile,
) -> LeanExprCtx<'a> {
    // `inst` is in scope from the `step` signature; bound/skip-if
    // expressions resolve `m_<x>` against the active instance's state
    // (via `Cambrian.Generated.World.<e> w inst`). Route params are
    // added by callers when needed.
    LeanExprCtx::for_spec(
        program,
        entity,
        spec_state_var(entity),
        HashSet::new(),
        HashSet::new(),
        profile,
    )
    .with_world("w", "inst")
}

/// Lower one `step` arm's route call onto the invariant carrier (`World`,
/// `World × MsgCtx`, or `RouteResult` of either). Valued (`route_is_view`)
/// routes return `World × T` (or `RouteResult (World × T)`); the stepper
/// threads only `World`, so those calls project `.fst`.
fn format_step_result(
    call: &str,
    with_time: bool,
    any_throwing: bool,
    route_fail: bool,
    is_view: bool,
    predictable: bool,
) -> String {
    let world = if is_view {
        format!("({}).fst", call)
    } else {
        call.to_string()
    };
    match (with_time, any_throwing, route_fail) {
        (false, false, _) => format!("{}\n", world),
        (false, true, true) => {
            if is_view {
                format!("({}).map (fun p => p.fst)\n", call)
            } else {
                format!("{}\n", call)
            }
        }
        (false, true, false) => format!(".ok ({})\n", world),
        (true, false, _) => format!("({}, ctx)\n", world),
        // L7 (§53): the LAST inline `match` of the invariant model. It is
        // reachable by `#[with_time]` over a route with a `where … : throw N`
        // guard, and mints `step.match_1` into the digest zone — silently.
        // `Except.map` IS this match (`.error e => .error e | .ok a =>
        // .ok (f a)`, core `Except.map`), so the escrow arm expresses it as
        // an explicit combinator application. Legacy keeps the byte-identical
        // `match`. A valued fallible route maps `p.fst` instead of `w'`.
        (true, true, true) => {
            if is_view {
                if predictable {
                    format!("({}).map (fun p => (p.fst, ctx))\n", call)
                } else {
                    format!(
                        "match {} with | .error e => .error e | .ok p => .ok (p.fst, ctx)\n",
                        call,
                    )
                }
            } else if predictable {
                format!("({}).map (fun w' => (w', ctx))\n", call)
            } else {
                format!(
                    "match {} with | .error e => .error e | .ok w' => .ok (w', ctx)\n",
                    call,
                )
            }
        }
        (true, true, false) => format!(".ok ({}, ctx)\n", world),
    }
}

fn route_is_init(route: &Route) -> bool {
    route.is_init || route.name == "constructor"
}

fn filter_excluded_senders(senders: Vec<String>, exclude: &[String]) -> Vec<String> {
    if exclude.is_empty() {
        return senders;
    }
    senders
        .into_iter()
        .filter(|s| !exclude.contains(s))
        .collect()
}

fn emit_step_call(
    program: &Program,
    entity: &Entity,
    a: &ResolvedAction<'_>,
    with_time: bool,
    any_throwing: bool,
    indent: usize,
    out: &mut String,
) {
    if route_is_init(a.route) {
        emit_step_noop(with_time, any_throwing, out);
        return;
    }
    let route_args: String = a
        .route
        .params
        .iter()
        .map(|p| p.name.clone())
        .collect::<Vec<_>>()
        .join(" ");
    let arg_suffix = if route_args.is_empty() {
        String::new()
    } else {
        format!(" {}", route_args)
    };
    let call = format!(
        "{}.Routes.{} w inst ctx{}",
        entity.name, a.route.name, arg_suffix,
    );
    let route_fail = route_fail_mode(program, entity, a.route);

    if route_has_send_effect(a.route) {
        // L10: atomic invariant step elides sub-call interleavings.
        // Caller already wrote `indent` spaces for the first line.
        out.push_str(&format!(
            "-- L10: invariant step for '{}' elides send / var-call interleavings (atomic WorldState → WorldState)\n",
            a.route.name,
        ));
        push_indent(out, indent);
    }

    out.push_str(&format_step_result(
        &call,
        with_time,
        any_throwing,
        route_fail,
        route_is_view(a.route),
        crate::codegen::lean::use_predictable_profile(),
    ));
}

/// True when a route body contains a `Send` / `VarCall` (L10 paper trail).
fn route_has_send_effect(route: &Route) -> bool {
    use crate::ast::RouteAction;
    route
        .body
        .all_actions()
        .into_iter()
        .any(|a| matches!(a, RouteAction::Send { .. } | RouteAction::VarCall { .. }))
}

fn emit_step_noop(with_time: bool, any_throwing: bool, out: &mut String) {
    match (with_time, any_throwing) {
        (false, false) => out.push('w'),
        (false, true) => out.push_str(".ok w"),
        (true, false) => out.push_str("(w, ctx)"),
        (true, true) => out.push_str(".ok (w, ctx)"),
    }
}

// ---------------------------------------------------------------------------
// runTrace
// ---------------------------------------------------------------------------

fn emit_run_trace(
    entity: &Entity,
    with_time: bool,
    any_throwing: bool,
    fail_on_revert: bool,
    out: &mut String,
    profile: super::super::LeanProfile,
) {
    let result_ty = if with_time {
        "Cambrian.Generated.World × Cambrian.MsgCtx".to_string()
    } else {
        "Cambrian.Generated.World".to_string()
    };
    let wrapped = if any_throwing {
        format!("Cambrian.RouteResult ({})", result_ty)
    } else {
        result_ty.clone()
    };

    // B5 (predictable): non-recursive fold. `runTrace` becomes a plain
    // `List.foldl(M)` app-spine over `step` — no structural recursion, no
    // `runTrace.match_*`/`.brecOn` machinery, no fuel. Semantics are IDENTICAL
    // to the legacy recursion (dump-verified). The fold form splits three ways,
    // mirroring the legacy `match` arms exactly:
    //   • total (¬throwing)             → `foldl` (no monad); `step` returns a
    //     bare `World`, the recursion is a plain accumulate.
    //   • throwing, fail_on_revert:true → `foldlM` (over the `Except`/
    //     `RouteResult` monad): the first `.error` propagates and aborts the
    //     whole trace — the legacy `| .error e => .error e` arm.
    //   • throwing, fail_on_revert:false (Foundry default) → the revert-continue
    //     fold `.ok (trace.foldl (fun w a => (step w inst ctx a).toOption.getD w)
    //     w₀)`: a reverted action is caught (`Except.toOption` → `Option.getD`
    //     recovers the UNCHANGED accumulator) and the campaign proceeds, so the
    //     trace never aborts — the legacy `| .error _ => runTrace w₀ … rest` arm
    //     (T-X-007 / LEAN-H9). `RouteResult = Except`, so `.toOption.getD`
    //     resolves; the whole fold is wrapped back in `.ok` to keep the
    //     `RouteResult World` return type.
    // The `with_time` pair-threaded accumulator (`let p := step …; runTrace
    // p.fst … p.snd …`) becomes the fold seed `(w₀, ctx)`. `trace` moves to an
    // explicit parameter so the result type is the bare `wrapped`, not
    // `List Action →`. Legacy keeps the byte-identical structural recursion.
    let predictable = profile.predictable;
    if predictable {
        out.push_str(&format!(
            "def runTrace (w₀ : Cambrian.Generated.World) (inst : {}.Identity) (ctx : Cambrian.MsgCtx) (trace : List Action) :\n",
            entity.name,
        ));
        push_indent(out, 2);
        out.push_str(&format!("{} :=\n", wrapped));
        push_indent(out, 1);
        if any_throwing && !fail_on_revert {
            // Revert-continue: catch each `.error` and keep the accumulator.
            if with_time {
                out.push_str(
                    ".ok (trace.foldl (fun p a => (step p.fst inst p.snd a).toOption.getD p) (w₀, ctx))\n\n",
                );
            } else {
                out.push_str(
                    ".ok (trace.foldl (fun w a => (step w inst ctx a).toOption.getD w) w₀)\n\n",
                );
            }
        } else {
            // Total (`foldl`) or propagate-stop (`foldlM`). Ascribe the
            // accumulator (B-13) so the elaborator does not pick `PUnit`.
            let fold = if any_throwing { "foldlM" } else { "foldl" };
            if with_time {
                out.push_str(&format!(
                    "trace.{} (fun (p : Cambrian.Generated.World × Cambrian.MsgCtx) a => step p.fst inst p.snd a) (w₀, ctx)\n\n",
                    fold,
                ));
            } else {
                out.push_str(&format!(
                    "trace.{} (fun (w : Cambrian.Generated.World) a => step w inst ctx a) w₀\n\n",
                    fold,
                ));
            }
        }
        return;
    }

    out.push_str(&format!(
        "def runTrace (w₀ : Cambrian.Generated.World) (inst : {}.Identity) (ctx : Cambrian.MsgCtx) :\n",
        entity.name,
    ));
    push_indent(out, 2);
    out.push_str(&format!("List Action → {}\n", wrapped));
    push_indent(out, 1);
    match (with_time, any_throwing) {
        (false, false) => {
            out.push_str("| []        => w₀\n");
            push_indent(out, 1);
            out.push_str("| a :: rest => runTrace (step w₀ inst ctx a) inst ctx rest\n\n");
        }
        (false, true) => {
            out.push_str("| []        => .ok w₀\n");
            push_indent(out, 1);
            out.push_str("| a :: rest => match step w₀ inst ctx a with\n");
            push_indent(out, 2);
            // `fail_on_revert: false` (Foundry default) — a reverted action is
            // caught (try/catch) and the campaign proceeds against the unchanged
            // world; only `fail_on_revert: true` aborts the whole trace
            // (T-X-007 / LEAN-H9).
            if fail_on_revert {
                out.push_str("| .error e => .error e\n");
            } else {
                out.push_str("| .error _ => runTrace w₀ inst ctx rest\n");
            }
            push_indent(out, 2);
            out.push_str("| .ok w'   => runTrace w' inst ctx rest\n\n");
        }
        (true, false) => {
            out.push_str("| []        => (w₀, ctx)\n");
            push_indent(out, 1);
            out.push_str(
                "| a :: rest => let p := step w₀ inst ctx a; runTrace p.fst inst p.snd rest\n\n",
            );
        }
        (true, true) => {
            out.push_str("| []        => .ok (w₀, ctx)\n");
            push_indent(out, 1);
            out.push_str("| a :: rest => match step w₀ inst ctx a with\n");
            push_indent(out, 2);
            if fail_on_revert {
                out.push_str("| .error e => .error e\n");
            } else {
                out.push_str("| .error _ => runTrace w₀ inst ctx rest\n");
            }
            push_indent(out, 2);
            out.push_str("| .ok p    => runTrace p.fst inst p.snd rest\n\n");
        }
    }
}

// ---------------------------------------------------------------------------
// Theorem
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn build_invariant_theorem(
    program: &Program,
    entity: &Entity,
    inv: &InvariantDecl,
    actions: &[ResolvedAction<'_>],
    any_throwing: bool,
    with_time: bool,
    trace: &TraceUsage,
    diags: &mut Vec<SpecDiag>,
    profile: super::super::LeanProfile,
) -> InvTheorem {
    let predictable = profile.predictable;
    let slug = slugify(&inv.name);
    let mut lead_comment = String::new();
    if inv.fail_on_revert && !any_throwing {
        doc_comment(
            &mut lead_comment,
            0,
            "#[fail_on_revert]: tautological — all action routes are total.",
        );
    }
    // The theorem *type* is assembled into `ty`; the caller wraps it as
    // `theorem <slug> :\n<ty> := by …` (legacy) or `abbrev <slug>.statement :
    // Prop :=\n<ty>` + `theorem <slug> : <slug>.statement := by …` (predictable).
    let mut ty = String::new();
    let out = &mut ty;
    let _ = &slug;

    let derived_names: HashSet<String> = inv.derived.iter().map(|q| q.name.clone()).collect();

    // Forall-ized initial state (`init { m_count: * }`). The quantified vars
    // seed the initial world; they must be visible as locals to the
    // init-state lowering.
    let tcx = type_ctx_for(program, entity, profile);
    let self_spec = &inv.instances[0].forall_state;
    let (mut forall_quant, forall_pins, forall_vars) =
        resolve_forall_init(entity, inv.init_state(), self_spec, &tcx, "");
    let mut effective_init = inv.init_state().to_vec();
    effective_init.extend(forall_pins.clone());

    // UPSTREAM B-26: init seeds must not see `world_var` — otherwise
    // `sys::now` lowers to `w.block.timestamp` inside `let w := … with …`,
    // referencing `w` in its own definition. Without world, `sys::now` is
    // `ctx.timestamp` (MsgCtx is bound in the theorem).
    let init_ctx = LeanExprCtx::for_spec_with_derived(
        program,
        entity,
        spec_state_var(entity),
        forall_vars.clone(),
        HashSet::new(),
        derived_names.clone(),
        profile,
    );

    // For checks/track/derived references, members lower against either
    // the post-trace world (`w` = `w_final` aliased) or the initial
    // world; both expose the same projection shape, so the spec_state
    // expression resolves uniformly.
    let setup_lets: HashSet<String> = inv.track.iter().map(|b| b.name.clone()).collect();
    let mut check_ctx = LeanExprCtx::for_spec_with_derived(
        program,
        entity,
        spec_state_var(entity),
        HashSet::new(),
        setup_lets.clone(),
        derived_names.clone(),
        profile,
    )
    .with_world("w", "inst");
    let action_param_refs = action_param_names_in_checks(inv, entity, actions);
    if !action_param_refs.is_empty() {
        check_ctx.route_params.extend(action_param_refs.iter().cloned());
    }
    if trace.checks_use_trace {
        // `check` expressions reference `trace::*`; bind `acc` to the
        // post-trace accumulator (see the `let acc := finalAcc …` below).
        check_ctx = check_ctx.with_trace_acc("acc");
    }

    // Initial context from the invariant's `ctx { ... }` block. `msg::` seeds
    // the `MsgCtx`; `sys::` seeds the world block / balances. Forall ctx vars
    // extend the theorem's `∀`-prefix.
    let mut ctx_used: HashSet<String> = forall_vars.clone();
    let (ctx_quant, ctx_line, block_line, balance_line) = invariant_ctx_setup(
        &inv.context,
        &check_ctx,
        &mut ctx_used,
        Some((entity.name.as_str(), "inst")),
    );
    forall_quant.extend(ctx_quant);

    push_indent(out, 1);
    out.push_str("∀ (trace : List Action),\n");
    emit_forall_quant(out, &forall_quant);

    // Bind `ctx` before the init world so init seeds may reference
    // `ctx.timestamp` (UPSTREAM B-26 — `sys::now` without world_var).
    push_indent(out, 1);
    out.push_str(&ctx_line);

    // inst + initial world.
    push_indent(out, 1);
    out.push_str(&format_init_instance(entity, &effective_init, &init_ctx));
    out.push('\n');
    push_indent(out, 1);
    out.push_str(&format_init_world(entity, &effective_init, &init_ctx));
    out.push('\n');
    let init_pin_names: std::collections::HashSet<String> =
        effective_init.iter().map(|(n, _)| n.clone()).collect();
    if let Some(bootstrap) = format_invariant_constructor_bootstrap(
        entity,
        inv,
        &effective_init,
        &init_pin_names,
        &init_ctx,
    ) {
        for line in bootstrap.lines() {
            push_indent(out, 1);
            out.push_str(line);
            out.push('\n');
        }
    }
    if let Some(bl) = &block_line {
        push_indent(out, 1);
        out.push_str(bl);
    }
    if let Some(bl) = &balance_line {
        push_indent(out, 1);
        out.push_str(bl);
    }

    // track lets — captured against the initial world.
    for b in &inv.track {
        let track_ctx = LeanExprCtx::for_spec_with_derived(
            program,
            entity,
            spec_state_var(entity),
            HashSet::new(),
            HashSet::new(),
            derived_names.clone(),
            profile,
        )
        .with_world("w", "inst");
        push_indent(out, 1);
        out.push_str(&format!(
            "let {} := {}\n",
            super::super::core::types::lean_safe_ident(&b.name),
            gen_expr(&b.value, &track_ctx),
        ));
    }

    // `assume` exclude semantics: restrict the theorem to well-formed
    // traces. Emitted only when some action declares an `assume`.
    if trace.has_assume {
        let acc_arg = if trace.needs_acc() { " accInit" } else { "" };
        push_indent(out, 1);
        out.push_str(&format!(
            "traceValid w inst ctx{} trace = true →\n",
            acc_arg,
        ));
    }

    let mut checks_text = render_checks(&inv.checks, &check_ctx);
    if !action_param_refs.is_empty() {
        let has_senders = !inv.senders.is_empty();
        checks_text = wrap_checks_over_trace(
            actions,
            &action_param_refs,
            with_time,
            has_senders,
            &checks_text,
        );
    }
    if trace.checks_use_trace {
        // Bind the post-trace accumulator for `trace::*` references in
        // `check` expressions. `finalAcc` only folds over the trace.
        checks_text = format!("let acc := finalAcc accInit trace;\n {}", checks_text);
    }

    if any_throwing {
        // Executable shape (Option A): *compute* the final world instead of
        // quantifying over it. Default policy (`okImplies`) accepts a revert;
        // `#[fail_on_revert]` uses `okAnd` (`.error => False`). Both profiles
        // emit the Core helper — no raw `| .error _ => True` match (PN-104).
        let wrapper = if inv.fail_on_revert {
            "Cambrian.RouteResult.okAnd"
        } else {
            "Cambrian.RouteResult.okImplies"
        };
        let _ = predictable;
        push_indent(out, 1);
        out.push_str(&format!("{wrapper} (runTrace w inst ctx trace) (fun "));
        if with_time {
            // L7 (§53): the `let` body MUST NOT start at a column strictly
            // greater than the `let` keyword's — Lean would then glue it on
            // as an APPLICATION argument of the `let` value (`p.fst <checks>`)
            // and reject the closing paren with `expected ';' or line break`.
            out.push_str("p =>\n");
            push_indent(out, 1);
            out.push_str("   (let w := p.fst\n");
            push_indent(out, 1);
            out.push_str(&format!("    {}))", checks_text));
        } else {
            out.push_str("w =>\n");
            push_indent(out, 1);
            out.push_str(&format!("   {})", checks_text));
        }
    } else if with_time {
        push_indent(out, 1);
        out.push_str("let p := runTrace w inst ctx trace\n");
        push_indent(out, 1);
        out.push_str("let w := p.fst\n");
        push_indent(out, 1);
        out.push_str(&checks_text);
    } else {
        push_indent(out, 1);
        out.push_str("let w := runTrace w inst ctx trace\n");
        push_indent(out, 1);
        out.push_str(&checks_text);
    }

    // No `_revert_ok` companion: the permissive `.error _ => True` /
    // `okImplies`-on-error arm already encodes the default policy (reverts are
    // accepted), so a separate `runTrace … = .error e → True` lemma is
    // vacuously redundant.

    let _ = diags;
    InvTheorem { lead_comment, ty }
}

fn render_checks(checks: &[Expr], ctx: &LeanExprCtx<'_>) -> String {
    let pieces: Vec<String> = checks.iter().map(|c| gen_expr(c, ctx)).collect();
    super::spec_steps::conjoin_props(&pieces)
}

/// Action-parameter names that appear free in `inv.checks` (and are not
/// entity members / `derived` / `track` binders). Those names are fields
/// of `Action` constructors, not of the post-trace `World`; the theorem
/// must bind them by folding over `trace`.
fn action_param_names_in_checks(
    inv: &InvariantDecl,
    entity: &Entity,
    actions: &[ResolvedAction<'_>],
) -> HashSet<String> {
    let params: HashSet<String> = actions
        .iter()
        .flat_map(|a| a.route.params.iter().map(|p| p.name.clone()))
        .collect();
    if params.is_empty() {
        return HashSet::new();
    }
    let mut idents = HashSet::new();
    for c in &inv.checks {
        collect_expr_idents(c, &mut idents);
    }
    idents.retain(|n| {
        params.contains(n)
            && !entity.members.iter().any(|m| &m.name == n)
            && !inv.derived.iter().any(|d| &d.name == n)
            && !inv.track.iter().any(|t| &t.name == n)
    });
    idents
}

fn collect_expr_idents(expr: &Expr, out: &mut HashSet<String>) {
    match expr {
        Expr::Ident(n) => {
            out.insert(n.clone());
        }
        Expr::BinOp(l, _, r) | Expr::Index(l, r) | Expr::Range(l, r) => {
            collect_expr_idents(l, out);
            collect_expr_idents(r, out);
        }
        Expr::UnaryOp(_, e)
        | Expr::Cast(e, _)
        | Expr::FieldAccess(e, _)
        | Expr::Some(e)
        | Expr::Encode { value: e, .. } => collect_expr_idents(e, out),
        Expr::FnCall(_, args)
        | Expr::MacroRef(_, args)
        | Expr::Tuple(args)
        | Expr::ArrayLit(args)
        | Expr::EnumVariantWithData(_, _, args)
        | Expr::Block(args) => {
            for a in args {
                collect_expr_idents(a, out);
            }
        }
        Expr::MethodCall(recv, _, args) => {
            collect_expr_idents(recv, out);
            for a in args {
                collect_expr_idents(a, out);
            }
        }
        Expr::If(c, t, e) => {
            collect_expr_idents(c, out);
            collect_expr_idents(t, out);
            if let Some(el) = e {
                collect_expr_idents(el, out);
            }
        }
        Expr::Let(_, v, b) => {
            collect_expr_idents(v, out);
            collect_expr_idents(b, out);
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, e) in fields {
                collect_expr_idents(e, out);
            }
        }
        Expr::RecordUpdate(base, fields) => {
            collect_expr_idents(base, out);
            for (_, e) in fields {
                collect_expr_idents(e, out);
            }
        }
        Expr::Closure(_, b) => collect_expr_idents(b, out),
        Expr::For(_, iter, b) => {
            collect_expr_idents(iter, out);
            collect_expr_idents(b, out);
        }
        Expr::Match(scrut, arms) => {
            collect_expr_idents(scrut, out);
            for arm in arms {
                collect_expr_idents(&arm.body, out);
            }
        }
        Expr::NamespacedCall { args, .. } => {
            for a in args {
                collect_expr_idents(a, out);
            }
        }
        Expr::AddressOf {
            args, with_params, ..
        } => {
            for a in args {
                collect_expr_idents(a, out);
            }
            for (_, e) in with_params {
                collect_expr_idents(e, out);
            }
        }
        Expr::IntLiteral(_)
       
       
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::BoolLiteral(_)
        | Expr::EmptyCollection
        | Expr::TemporalRef(_)
        | Expr::MsgField(_)
        | Expr::SysField(_)
        | Expr::TraceField(_)
        | Expr::TraceCall { .. }
        | Expr::EnumVariant(_, _)
        | Expr::None => {}
    }
}

/// Fold `checks` over `trace`, binding each `Action` constructor's fields
/// so action-parameter names in the Cam `check` are in scope. Constructors
/// that do not carry every referenced name (and `advanceTime`) get `True`.
fn wrap_checks_over_trace(
    actions: &[ResolvedAction<'_>],
    refs: &HashSet<String>,
    with_time: bool,
    has_senders: bool,
    checks: &str,
) -> String {
    let mut arms = String::new();
    for a in actions {
        let mut pat = format!(".{}", a.route.name);
        for p in &a.route.params {
            pat.push(' ');
            pat.push_str(&p.name);
        }
        if has_senders {
            pat.push_str(" sender_idx");
        }
        let covers = refs.iter().all(|n| a.route.params.iter().any(|p| &p.name == n));
        let body = if covers { checks } else { "True" };
        arms.push_str(&format!(" | {} => {}", pat, body));
    }
    if with_time {
        arms.push_str(" | .advanceTime delta => True");
    }
    format!("(∀ (a : Action), a ∈ trace → (match a with{}))", arms)
}

/// Render a `let inst : <E>.Identity := …` binding from the
/// invariant's `init { … }` block. Only identity members contribute;
/// non-identity members are folded into the world state separately by
/// [`format_init_world`].
fn format_init_instance(entity: &Entity, init: &[(String, Expr)], ctx: &LeanExprCtx<'_>) -> String {
    match identity_field_inits(entity, init, ctx, &[]) {
        None => format!("let inst : {}.Identity := {{}}", entity.name),
        Some(pieces) => format!(
            "let inst : {}.Identity := {{ {} }}",
            entity.name,
            pieces.join(", "),
        ),
    }
}

fn init_route(entity: &Entity) -> Option<&crate::ast::Route> {
    entity
        .routes
        .iter()
        .find(|r| r.is_init)
        .or_else(|| entity.routes.iter().find(|r| r.name == "constructor"))
}

/// True when `init { … }` pins some members but leaves others that the
/// constructor route still initializes (PL-F-INV-02 / Run #7b cp_002 class).
fn invariant_needs_constructor_bootstrap(
    entity: &Entity,
    init_pins: &std::collections::HashSet<String>,
) -> bool {
    let route = match init_route(entity) {
        Some(r) => r,
        None => return false,
    };
    entity.members.iter().any(|m| {
        !m.is_identity
            && !init_pins.contains(&m.name)
            && m.transforms
                .iter()
                .any(|t| t.route_name == route.name)
    })
}

/// Lower one init-route argument for Lean invariant ctor bootstrap (parity with
/// `render_init_route_arg_sol` / `render_init_route_arg_rust`).
fn render_init_route_arg_lean(
    arg: InitRouteArg<'_>,
    ty: &Type,
    ctx: &LeanExprCtx<'_>,
    inv: &InvariantDecl,
) -> String {
    match arg {
        InitRouteArg::Expr(expr) => gen_expr(expr, ctx),
        InitRouteArg::DummyHolder => inv
            .senders
            .first()
            .map(|s| gen_expr(s, ctx))
            .unwrap_or_else(|| default_for_type(ty, &ctx.type_ctx)),
        // Foundry uses `address(_handler)`; Lean has no handler contract — W13 seeds
        // `ctx.sender` from `senders[0]` immediately before the bootstrap call.
        InitRouteArg::Handler => "ctx.sender".to_string(),
        InitRouteArg::SiblingInstance(_) => default_for_type(ty, &ctx.type_ctx),
        InitRouteArg::Default => default_for_type(ty, &ctx.type_ctx),
    }
}

/// Seed `ctx.sender` from the first `senders { … }` literal, then run the
/// init/constructor route so member transforms (e.g. mint into `m_balances`)
/// are reflected before `trace := []` checks.
fn format_invariant_constructor_bootstrap(
    entity: &Entity,
    inv: &InvariantDecl,
    effective_init: &[(String, Expr)],
    init_pins: &std::collections::HashSet<String>,
    ctx: &LeanExprCtx<'_>,
) -> Option<String> {
    if !invariant_needs_constructor_bootstrap(entity, init_pins) {
        return None;
    }
    let route = init_route(entity)?;
    let mut out = String::new();
    let ctx_has_sender = inv
        .context
        .entries
        .iter()
        .any(|e| e.namespace == "msg" && e.field == "sender");
    let explicit_deploy = invariant_harness::has_explicit_deploy(inv);
    if !ctx_has_sender && !explicit_deploy {
        let sender = invariant_harness::effective_deploy(inv)?;
        out.push_str(&format!(
            "let ctx := {{ ctx with sender := {} }}\n",
            gen_expr(sender, ctx),
        ));
    }
    let fill = InitRouteFill {
        entity,
        init_state: effective_init,
        deploy: &inv.deploy,
        senders: &inv.senders,
        dummy_unmatched_address: true,
        // Match Foundry `initialize(...)` (see `emit_foundry_invariant_initialize`).
        handler_unmatched_address: true,
        siblings: &[],
    };
    let arg_strs: Vec<String> = resolve_init_route_args(fill)
        .into_iter()
        .map(|(ty, arg)| render_init_route_arg_lean(arg, ty, ctx, inv))
        .collect();
    let args = if arg_strs.is_empty() {
        String::new()
    } else {
        format!(" {}", arg_strs.join(" "))
    };
    let ctx_arg = if explicit_deploy && !ctx_has_sender {
        let sender = invariant_harness::effective_deploy(inv)?;
        format!("{{ ctx with sender := {} }}", gen_expr(sender, ctx))
    } else {
        "ctx".to_string()
    };
    let ctor_call = format!(
        "{}.Routes.{} w inst {}{}",
        entity.name,
        route.name,
        ctx_arg,
        args,
    );
    // PL-F-INV-02 bootstrap: total init routes return `World`; fail-mode
    // routes return `RouteResult World`. After `format_init_world` pins
    // `w : World`, a bare fail-mode call is ill-typed (SMAFD LG-F07 / RHT).
    // Do not `match .ok` on total routes — Lean expects `World` as the match
    // result type and resolves `.ok` to `WorldState.ok` (lake error).
    if route_fail_mode(ctx.type_ctx.program, entity, route) {
        out.push_str(&format!("let w := Cambrian.exceptGetD ({ctor_call}) w\n"));
    } else {
        out.push_str(&format!("let w := {ctor_call}\n"));
    }
    Some(out)
}

/// Render a `let w : Cambrian.Generated.World := …` binding. Folds
/// the non-identity `init { … }` members into a `State.default`-based
/// record, then stamps it into the world via
/// `Cambrian.Generated.World.with<E>`.
fn format_init_world(entity: &Entity, init: &[(String, Expr)], ctx: &LeanExprCtx<'_>) -> String {
    let id_names: HashSet<String> = entity
        .members
        .iter()
        .filter(|m| m.is_identity)
        .map(|m| m.name.clone())
        .collect();
    let non_id_inits: Vec<&(String, Expr)> =
        init.iter().filter(|(n, _)| !id_names.contains(n)).collect();
    let state_expr = if non_id_inits.is_empty() {
        format!("{}.State.default", entity.name)
    } else {
        let updates: Vec<String> = non_id_inits
            .iter()
            .map(|(name, expr)| format!("{} := {}", name, gen_expr(expr, ctx)))
            .collect();
        format!(
            "({{ {}.State.default with {} }})",
            entity.name,
            updates.join(", "),
        )
    };
    let setter = format!("Cambrian.Generated.World.with{}", entity.name);
    format!(
        "let w : Cambrian.Generated.World := {} Cambrian.Generated.World.default inst {}",
        setter, state_expr,
    )
}

/// The Lean-side expression that projects the active instance's
/// `<E>.State` out of the world. Identical across `lean_test`,
/// `lean_fuzz`, and `lean_invariant` so members lower against a
/// uniform "state lens".
fn spec_state_var(entity: &Entity) -> String {
    format!(
        "(Cambrian.Generated.World.{} w inst)",
        entity_field_name(&entity.name),
    )
}

#[allow(dead_code)]
fn _silence_unused(_: &Param, _: &FuzzDecl) {}
