// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Shared Lean test/property step emission (B-17).
//!
//! Post-call `expect *` groups are flushed while `_result_N` / `w` are live,
//! each as `let __eK := <prop>`, and the leaf goal is their conjunction
//! ([`conjoin_props`]). Used by `lean_test` and `lean_property`.

use std::collections::{HashMap, HashSet};

use cambrian_core::U256;
use crate::ast::{Entity, Expr, FieldPath, PathSegment, Program, TestStep, Type};
use crate::graph::dfs_topo;

use super::super::core::emitter::push_indent;
use super::super::expr::{
    bitvec_width_of_type, collection_shape_of_type, gen_expr, sys_block_field, LeanExprCtx,
};
use super::world::{entity_field_name, resolve_event_ctor};
use super::spec_prefix::{format_init_instance_named, state_expr_named};
use super::super::route::{route_fail_mode, route_is_view};
use crate::codegen::evm_test_codegen::{find_init_route, is_init_route_call};

// ---------------------------------------------------------------------------
// Prop conjunction (shared with invariant `render_checks`)
// ---------------------------------------------------------------------------

/// Join already-rendered Prop strings with ` ∧ `; empty → `True`.
pub(crate) fn conjoin_props(pieces: &[String]) -> String {
    if pieces.is_empty() {
        "True".to_string()
    } else {
        pieces.join(" ∧ ")
    }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) enum SpecEvent {
    Let {
        name: String,
        ty: Option<Type>,
        value: Expr,
    },
    SetMsgCtx {
        fields: Vec<(String, Expr)>,
    },
    SetSysCtx {
        fields: Vec<(String, Expr)>,
    },
    RawCall {
        route_name: String,
        args: Vec<Expr>,
        is_view: bool,
        callee_entity: Option<String>,
        callee_inst: Option<String>,
    },
    WrappedCall {
        route_name: String,
        args: Vec<Expr>,
        is_view: bool,
        callee_entity: Option<String>,
        callee_inst: Option<String>,
    },
    /// `deploy peer = Peer(...)` — install a second entity into `w`.
    InstallPeer {
        peer_entity: String,
        inst_var: String,
        args: Vec<Expr>,
        init_state: Vec<(String, Expr)>,
    },
    /// `expect emit Event(args)` before the next route call.
    ExpectEmit {
        event_name: String,
        args: Vec<Expr>,
    },
    ExpectState(Vec<(FieldPath, Expr)>),
    ExpectThrow {
        code: u32,
    },
    ExpectReturn {
        value: Expr,
    },
    ExpectReturnTuple {
        values: Vec<Expr>,
    },
    ExpectReturnLens {
        path: FieldPath,
        value: Expr,
    },
    /// Boolean `expect <expr>`. `Ident("result")` is the preceding call's return.
    ExpectPred {
        cond: Expr,
    },
    /// Modelled raw value-transfers from `expect effects [~> dest, …]`
    /// (PM-005). Renders a real Prop over `w.balances`, never vacuous `True`.
    ExpectTransfers {
        dests: Vec<Expr>,
    },
    /// Effects were present but none are Lean-modelable (TVM platform /
    /// typed sends without a transfer log). Goal becomes `False` so lake
    /// cannot "prove" the test.
    ExpectEffectsUnmodelable,
    Comment(String),
}

/// Lower `expect effects [...]` into modelled transfer props (PM-005).
///
/// Raw `~> dest` entries become [`SpecEvent::ExpectTransfers`]. When the
/// list is non-empty but contains only unmodelable effects (typed sends /
/// platform / deploy), emit [`SpecEvent::ExpectEffectsUnmodelable`] so the
/// theorem goal is `False` rather than vacuous `True`. Empty `[]` stays a
/// comment (trivially holds).
pub(crate) fn lower_expect_effects(
    elements: &[crate::ast::TestEffectElement],
    diags: &mut Vec<super::spec::SpecDiag>,
    label: &str,
) -> Vec<SpecEvent> {
    use crate::ast::{TestEffect, TestEffectElement};

    if elements.is_empty() {
        return vec![SpecEvent::Comment(
            "expect effects []  ⇒  trivially holds (no effects asserted)".to_string(),
        )];
    }

    let mut dests = Vec::new();
    let mut unmodelable = 0usize;
    for el in elements {
        match el {
            TestEffectElement::Wildcard => {}
            TestEffectElement::Effect(TestEffect::Send {
                message: None,
                dest,
                ..
            }) => {
                // Raw value transfer `~> dest`.
                dests.push(dest.clone());
            }
            TestEffectElement::Effect(_) => {
                unmodelable += 1;
            }
        }
    }

    if !dests.is_empty() {
        if unmodelable > 0 {
            diags.push(super::spec::SpecDiag::warn(format!(
                "{label}: {unmodelable} non-transfer effect(s) in `expect effects` not yet modelled on Lean; transfer props still emitted",
            )));
        }
        return vec![SpecEvent::ExpectTransfers { dests }];
    }

    diags.push(super::spec::SpecDiag::warn(format!(
        "{label}: `expect effects [...]` has no Lean-modelable raw transfers — refusing vacuous True (goal := False)",
    )));
    vec![SpecEvent::ExpectEffectsUnmodelable]
}

// ---------------------------------------------------------------------------
// Emit context
// ---------------------------------------------------------------------------

pub(crate) struct SpecStepCtx<'a> {
    pub program: &'a Program,
    pub entity: &'a Entity,
    pub params: HashSet<String>,
    pub lets: HashSet<String>,
    pub deploy_bindings: HashMap<String, (String, String)>,
    pub profile: super::super::LeanProfile,
}

/// Collect `deploy binding = Entity(...)` handles for spec lowering.
pub(crate) fn collect_deploy_bindings(steps: &[TestStep]) -> HashMap<String, (String, String)> {
    let mut map = HashMap::new();
    for step in steps {
        if let TestStep::DeployPeer { binding, entity, .. } = step {
            map.insert(binding.clone(), (entity.clone(), alloc_peer_inst(binding)));
        }
    }
    map
}

/// EVM `gen_deploy_peer` forwards `deploy peer = Entity(args…)` positional
/// args into the factory deploy / init route. Lean `InstallPeer` must run
/// the matching init route when args are present (LG-013 deploy-positional).
pub(crate) fn deploy_positional_init_event(
    program: &Program,
    peer_entity: &Entity,
    inst_var: &str,
    args: &[Expr],
) -> Option<SpecEvent> {
    if args.is_empty() {
        return None;
    }
    let route = find_init_route(peer_entity)?;
    let fail = route_fail_mode(program, peer_entity, route);
    let view = route_is_view(route);
    Some(if fail {
        SpecEvent::WrappedCall {
            route_name: route.name.clone(),
            args: args.to_vec(),
            is_view: view,
            callee_entity: Some(peer_entity.name.clone()),
            callee_inst: Some(inst_var.to_string()),
        }
    } else {
        SpecEvent::RawCall {
            route_name: route.name.clone(),
            args: args.to_vec(),
            is_view: view,
            callee_entity: Some(peer_entity.name.clone()),
            callee_inst: Some(inst_var.to_string()),
        }
    })
}

pub(crate) fn init_call_folded_into_deploy(
    peer_entity: &Entity,
    route: &str,
    binding: &str,
    deploy_init_applied: &HashSet<String>,
) -> bool {
    deploy_init_applied.contains(binding) && is_init_route_call(peer_entity, route)
}

/// After `deploy binding = <owner>(...)`, bare `call route()` on the spec owner
/// must target the deployed instance (`pair_inst`), not the prefix `inst`
/// (INT-004 / LG-013 follow-up).
pub(crate) fn note_sut_deploy_instance(
    active: &mut Option<String>,
    owner_entity: &str,
    peer_entity: &str,
    inst_var: &str,
) {
    if peer_entity == owner_entity {
        *active = Some(inst_var.to_string());
    }
}

pub(crate) fn sut_self_call_instance(active: &Option<String>) -> Option<String> {
    active.clone()
}

impl SpecStepCtx<'_> {
    pub(crate) fn expr_ctx(&self) -> LeanExprCtx<'_> {
        LeanExprCtx::for_spec(
            self.program,
            self.entity,
            spec_state_var(self.entity),
            self.params.clone(),
            self.lets.clone(),
            self.profile,
        )
        .with_world("w", "inst")
        .with_deploy_bindings(self.deploy_bindings.clone())
    }
}

pub(crate) fn spec_state_var(entity: &Entity) -> String {
    format!(
        "(Cambrian.Generated.World.{} w inst)",
        entity_field_name(&entity.name),
    )
}

/// Stable `let <binding>_inst` name for a `deploy <binding> = …` step.
pub(crate) fn alloc_peer_inst(binding: &str) -> String {
    format!(
        "{}_inst",
        super::super::core::types::lean_safe_ident(binding)
    )
}

/// Resolve the callee entity for a spec route call (`None` → theorem owner).
pub(crate) fn resolve_callee_entity<'a>(
    program: &'a Program,
    owner: &'a Entity,
    callee_entity: Option<&str>,
) -> &'a Entity {
    if let Some(name) = callee_entity {
        program
            .entities
            .iter()
            .find(|e| e.name == name)
            .unwrap_or(owner)
    } else {
        owner
    }
}

struct EmitState {
    result_idx: usize,
    expect_idx: usize,
    throw_arm_idx: usize,
    /// Names of `__eK` binders already emitted (final goal joins these).
    expect_names: Vec<String>,
    /// Binder for the most recent view return (`_result_N`), if any.
    current_result: Option<String>,
    /// Declared return type of the route behind `current_result`.
    current_result_ty: Option<Type>,
    /// One-shot `msg { value: N }` for the next route call only (W2-BC-04a).
    pending_msg_value: Option<String>,
    /// Pre-call `expect emit` consumed after the next route call (W2-BC-06).
    pending_expect_emit: Option<(String, Vec<Expr>)>,
}

impl EmitState {
    fn new() -> Self {
        Self {
            result_idx: 0,
            expect_idx: 0,
            throw_arm_idx: 0,
            expect_names: Vec::new(),
            current_result: None,
            current_result_ty: None,
            pending_msg_value: None,
            pending_expect_emit: None,
        }
    }

    fn alloc_throw_arm(&mut self) -> String {
        let name = format!("n_throw_{}", self.throw_arm_idx);
        self.throw_arm_idx += 1;
        name
    }

    fn alloc_result(&mut self, ty: Option<Type>) -> String {
        let name = format!("_result_{}", self.result_idx);
        self.result_idx += 1;
        self.current_result = Some(name.clone());
        self.current_result_ty = ty;
        name
    }

    fn alloc_expect(&mut self) -> String {
        let name = format!("__e{}", self.expect_idx);
        self.expect_idx += 1;
        self.expect_names.push(name.clone());
        name
    }
}

// ---------------------------------------------------------------------------
// Throw target
// ---------------------------------------------------------------------------

/// Pair each `WrappedCall` with the next `expect throw N` on the linear spine
/// (success expects between call and throw are skipped).
pub(crate) fn collect_throw_targets(events: &[SpecEvent]) -> HashMap<usize, u32> {
    let mut map = HashMap::new();
    let mut pending_wrapped: Option<usize> = None;
    for (i, ev) in events.iter().enumerate() {
        match ev {
            SpecEvent::WrappedCall { .. } => pending_wrapped = Some(i),
            SpecEvent::ExpectThrow { code } => {
                if let Some(idx) = pending_wrapped {
                    map.insert(idx, *code);
                }
                pending_wrapped = None;
            }
            SpecEvent::RawCall { .. } => pending_wrapped = None,
            _ => {}
        }
    }
    map
}

/// `Some((wrapped_call_index, throw_code))` for the **last** throw pair — used
/// by legacy shape heuristics. Multi-throw bodies use [`collect_throw_targets`].
#[allow(dead_code)] // retained for cambrian-predict mirror; use collect_throw_targets in emitters.
pub(crate) fn find_throw_target(events: &[SpecEvent]) -> Option<(usize, u32)> {
    collect_throw_targets(events)
        .into_iter()
        .max_by_key(|(idx, _)| *idx)
}

/// Index after the `expect throw` paired with `call_idx`, if any.
fn index_after_throw_pair(events: &[SpecEvent], call_idx: usize) -> Option<usize> {
    let mut i = call_idx + 1;
    while i < events.len() {
        match &events[i] {
            SpecEvent::ExpectState(_)
            | SpecEvent::ExpectReturn { .. }
            | SpecEvent::ExpectReturnTuple { .. }
            | SpecEvent::ExpectReturnLens { .. }
            | SpecEvent::ExpectPred { .. }
            | SpecEvent::ExpectTransfers { .. }
            | SpecEvent::ExpectEffectsUnmodelable => i += 1,
            SpecEvent::ExpectThrow { .. } => return Some(i + 1),
            _ => return None,
        }
    }
    None
}

fn has_throw_continuation(events: &[SpecEvent], call_idx: usize) -> bool {
    index_after_throw_pair(events, call_idx).is_some_and(|start| start < events.len())
}

// ---------------------------------------------------------------------------
// Proof shape (algebraic — mirrors `emit_body_rec`, no Lean text heuristics)
// ---------------------------------------------------------------------------

/// Proof-relevant structure of a spec statement, derived from lowered
/// [`SpecEvent`]s before emission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpecStatementShape {
    /// Deepest stack of [`SpecEvent::WrappedCall`] bindings on the success
    /// path (each opens a `match` / escrow `okAnd`). Depth ≥ 2 ⇒ nested
    /// control flow that the simp ladder cannot discharge (LG-007b class A).
    pub max_route_depth: usize,
    /// `expect throw N` after a wrapped call — specialized error arm.
    pub has_throw_expect: bool,
    /// `expect effects` lowered to [`SpecEvent::ExpectEffectsUnmodelable`].
    pub has_unmodelable_effects: bool,
    /// The statement calls a route that rewrites a `HashMap` member behind a
    /// conditional (see [`branching_map_rewrite_routes`]). `simp` has to
    /// case-split the guard and normalize both map terms per call, which
    /// overflows the elaborator stack rather than the heartbeat budget —
    /// `maxRecDepth` is **not** recoverable, so the `first | … | sorry`
    /// fail-safe never fires and the whole file fails to compile.
    pub has_branching_map_rewrite: bool,
}

impl SpecStatementShape {
    pub(crate) fn from_events(events: &[SpecEvent], entity: &Entity) -> Self {
        let throw_targets = collect_throw_targets(events);
        let branching = branching_map_rewrite_routes(entity);
        Self {
            max_route_depth: route_depth_on_success_path(events, 0, &throw_targets),
            has_throw_expect: !throw_targets.is_empty(),
            has_unmodelable_effects: events
                .iter()
                .any(|e| matches!(e, SpecEvent::ExpectEffectsUnmodelable)),
            has_branching_map_rewrite: events.iter().any(|e| match e {
                SpecEvent::RawCall { route_name, .. }
                | SpecEvent::WrappedCall { route_name, .. } => {
                    branching.contains(route_name.as_str())
                }
                _ => false,
            }),
        }
    }

    /// Whether the statement should ship `:= by sorry` instead of the simp
    /// ladder (compile-safe under nested goals / throw arms).
    pub(crate) fn needs_sorry_stub(&self) -> bool {
        self.has_throw_expect
            || self.max_route_depth >= 2
            || self.has_unmodelable_effects
            || self.has_branching_map_rewrite
    }
}

/// Routes whose member transforms rewrite a `HashMap` member behind a
/// conditional.
///
/// A guarded map rewrite (`if to == old_del { m_votes } else { m_votes.update
/// (old_del, …).update(to, …) }`) is the one transform shape that makes the
/// reflection ladder abort the *build* instead of falling through to `sorry`:
/// `simp` case-splits the symbolic guard and normalizes a distinct
/// association-list term per branch, per call in the trace, and exhausts
/// `maxRecDepth` — a non-recoverable error `first` cannot catch. Raising
/// `maxRecDepth` is not an option either; the limit guards the native stack
/// and Lean aborts (SIGABRT) well before the goal closes.
///
/// Unguarded map updates (`m_votes.update(to, v + amount)`) and guarded
/// *scalar* transforms stay on the ladder: they normalize to a single term and
/// are bounded by `maxHeartbeats`, which the ladder recovers from.
pub(crate) fn branching_map_rewrite_routes(entity: &Entity) -> HashSet<&str> {
    let mut routes = HashSet::new();
    for member in &entity.members {
        if !is_map_type(&member.ty) {
            continue;
        }
        for transform in &member.transforms {
            if expr_has_conditional(&transform.body) {
                routes.insert(transform.route_name.as_str());
            }
        }
    }
    routes
}

fn is_map_type(ty: &crate::ast::Type) -> bool {
    matches!(ty, crate::ast::Type::Generic(name, _) if name == "HashMap" || name == "mapping")
}

fn expr_has_conditional(expr: &Expr) -> bool {
    if matches!(expr, Expr::If(..) | Expr::Match(..)) {
        return true;
    }
    let mut found = false;
    crate::analysis::route_facts::walk_expr_children(expr, &mut |child| {
        found = found || expr_has_conditional(child);
    });
    found
}

/// Walk the same control-flow spine as [`emit_body_rec`]: linear prefix, then
/// at most one [`SpecEvent::WrappedCall`] whose `.ok` arm continues at `i + 1`.
fn route_depth_on_success_path(
    events: &[SpecEvent],
    start: usize,
    throw_targets: &HashMap<usize, u32>,
) -> usize {
    let mut i = skip_leading_success_expects(events, start);

    while i < events.len() {
        match &events[i] {
            SpecEvent::WrappedCall { .. } => break,
            SpecEvent::RawCall { .. } => {
                i += 1;
                i = skip_leading_success_expects(events, i);
            }
            SpecEvent::Let { .. }
            | SpecEvent::SetMsgCtx { .. }
            | SpecEvent::SetSysCtx { .. }
            | SpecEvent::InstallPeer { .. }
            | SpecEvent::ExpectEmit { .. }
            | SpecEvent::Comment(_) => {
                i += 1;
            }
            SpecEvent::ExpectThrow { .. } => {
                i += 1;
            }
            SpecEvent::ExpectState(_)
            | SpecEvent::ExpectReturn { .. }
            | SpecEvent::ExpectReturnTuple { .. }
            | SpecEvent::ExpectReturnLens { .. }
            | SpecEvent::ExpectPred { .. }
            | SpecEvent::ExpectTransfers { .. }
            | SpecEvent::ExpectEffectsUnmodelable => {
                i = skip_leading_success_expects(events, i);
            }
        }
    }

    if i >= events.len() {
        return 0;
    }

    if throw_targets.contains_key(&i) {
        if has_throw_continuation(events, i) {
            let cont = index_after_throw_pair(events, i).unwrap_or(i + 1);
            return 1 + route_depth_on_success_path(events, cont, throw_targets);
        }
        return 1;
    }
    1 + route_depth_on_success_path(events, i + 1, throw_targets)
}

/// Success expects flushed before the next action — same grouping as
/// [`flush_success_expects`], without rendering.
fn skip_leading_success_expects(events: &[SpecEvent], start: usize) -> usize {
    let mut i = start;
    while i < events.len() && is_success_expect(&events[i]) {
        i += 1;
    }
    i
}

fn is_expect_event(ev: &SpecEvent) -> bool {
    matches!(
        ev,
        SpecEvent::ExpectState(_)
            | SpecEvent::ExpectThrow { .. }
            | SpecEvent::ExpectReturn { .. }
            | SpecEvent::ExpectReturnTuple { .. }
            | SpecEvent::ExpectReturnLens { .. }
            | SpecEvent::ExpectPred { .. }
            | SpecEvent::ExpectTransfers { .. }
            | SpecEvent::ExpectEffectsUnmodelable
    )
}

fn is_success_expect(ev: &SpecEvent) -> bool {
    matches!(
        ev,
        SpecEvent::ExpectState(_)
            | SpecEvent::ExpectReturn { .. }
            | SpecEvent::ExpectReturnTuple { .. }
            | SpecEvent::ExpectReturnLens { .. }
            | SpecEvent::ExpectPred { .. }
            | SpecEvent::ExpectTransfers { .. }
            | SpecEvent::ExpectEffectsUnmodelable
    )
}

// ---------------------------------------------------------------------------
// LG-F05: hoist `let` bindings referenced by `assume` preconditions
// ---------------------------------------------------------------------------

/// `let` bindings that must appear in the theorem prefix (before `→`
/// hypotheses), in dependency order.
pub(crate) struct HoistLetsForAssumes {
    pub prefix_lets: Vec<(String, Option<Type>, Expr)>,
    pub hoisted_names: HashSet<String>,
}

/// Render a spec `let` RHS with optional declared type (TYPED-LIT-3).
pub(crate) fn gen_expr_for_spec_let(
    value: &Expr,
    ctx: &LeanExprCtx<'_>,
    decl_ty: Option<&Type>,
) -> String {
    let mut ec = ctx.dup();
    if let Some(ty) = decl_ty {
        ec.expected_bitvec_width = bitvec_width_of_type(ty, &ec.type_ctx);
        ec.expected_collection = collection_shape_of_type(ty, &ec.type_ctx);
    }
    gen_expr(value, &ec)
}

/// Collect identifier names referenced in `expr` (for dependency analysis).
fn expr_free_idents(expr: &Expr, out: &mut HashSet<String>) {
    match expr {
        Expr::Ident(name) => {
            out.insert(name.clone());
        }
        Expr::FnCall(name, args) => {
            out.insert(name.clone());
            for a in args {
                expr_free_idents(a, out);
            }
        }
        Expr::BinOp(l, _, r) | Expr::Range(l, r) => {
            expr_free_idents(l, out);
            expr_free_idents(r, out);
        }
        Expr::UnaryOp(_, e)
        | Expr::FieldAccess(e, _)
        | Expr::Cast(e, _)
        | Expr::Some(e)
        | Expr::Closure(_, e) => {
            expr_free_idents(e, out);
        }
        Expr::Index(e, idx) => {
            expr_free_idents(e, out);
            expr_free_idents(idx, out);
        }
        Expr::MethodCall(e, _, args) => {
            expr_free_idents(e, out);
            for a in args {
                expr_free_idents(a, out);
            }
        }
        Expr::If(cond, t, e) => {
            expr_free_idents(cond, out);
            expr_free_idents(t, out);
            if let Some(el) = e {
                expr_free_idents(el, out);
            }
        }
        Expr::Let(_, val, body) | Expr::For(_, val, body) => {
            expr_free_idents(val, out);
            expr_free_idents(body, out);
        }
        Expr::Block(stmts) | Expr::ArrayLit(stmts) | Expr::Tuple(stmts) => {
            for s in stmts {
                expr_free_idents(s, out);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                expr_free_idents(v, out);
            }
        }
        Expr::RecordUpdate(base, fields) => {
            expr_free_idents(base, out);
            for (_, v) in fields {
                expr_free_idents(v, out);
            }
        }
        Expr::Match(subject, arms) => {
            expr_free_idents(subject, out);
            for arm in arms {
                expr_free_idents(&arm.body, out);
            }
        }
        Expr::MacroRef(_, args) | Expr::EnumVariantWithData(_, _, args) => {
            for a in args {
                expr_free_idents(a, out);
            }
        }
        Expr::NamespacedCall { args, .. } => {
            for a in args {
                expr_free_idents(a, out);
            }
        }
        Expr::AddressOf { args, with_params, .. } => {
            for a in args {
                expr_free_idents(a, out);
            }
            for (_, v) in with_params {
                expr_free_idents(v, out);
            }
        }
        Expr::Encode { value, .. } => {
            expr_free_idents(value, out);
        }
        Expr::BoolLiteral(_)
        | Expr::IntLiteral(_)
       
        | Expr::StringLiteral(_)
       
        | Expr::BytesLiteral(_)
        | Expr::EmptyCollection
        | Expr::MsgField(_)
        | Expr::SysField(_)
        | Expr::TraceField(_)
        | Expr::None
        | Expr::EnumVariant(_, _) => {}
        Expr::TemporalRef(name) => {
            out.insert(name.clone());
        }
        Expr::TraceCall { route, .. } => {
            out.insert(route.clone());
        }
    }
}

/// True when `expr` mentions any identifier in `names`.
pub(crate) fn expr_refs_any_ident(expr: &Expr, names: &HashSet<String>) -> bool {
    if names.is_empty() {
        return false;
    }
    let mut idents = HashSet::new();
    expr_free_idents(expr, &mut idents);
    idents.iter().any(|n| names.contains(n))
}

/// True when any expression in `exprs` mentions an identifier in `names`.
pub(crate) fn exprs_refs_any_ident(exprs: &[Expr], names: &HashSet<String>) -> bool {
    exprs.iter().any(|e| expr_refs_any_ident(e, names))
}

/// Comment standing in for an expectation whose call was dropped.
pub(crate) fn stale_result_comment(kind: &str) -> SpecEvent {
    SpecEvent::Comment(format!(
        "WARNING: `{kind}` dropped with the cross-contract call it asserts on \
         — `_result` still holds the previous call's value",
    ))
}

/// Comment standing in for a step that references an undeclared deploy binding.
pub(crate) fn dropped_step_comment(kind: &str, detail: &str) -> SpecEvent {
    SpecEvent::Comment(format!("WARNING: `{kind}` dropped — {detail}",))
}

/// Property `assume` steps become `→` hypotheses **before** the goal body.
/// Any `let` referenced (directly or through other `let`s) by an `assume`
/// must be hoisted into the prefix first, in topological order so each
/// binding appears after its dependencies.
pub(crate) fn hoist_lets_for_assume_hypotheses(steps: &[TestStep]) -> HoistLetsForAssumes {
    let mut let_bindings: Vec<(String, Option<Type>, Expr)> = Vec::new();
    let mut let_map: HashMap<String, (Option<Type>, Expr)> = HashMap::new();
    for step in steps {
        if let TestStep::Let { name, ty, value } = step {
            let_bindings.push((name.clone(), ty.clone(), value.clone()));
            let_map.insert(name.clone(), (ty.clone(), value.clone()));
        }
    }
    let let_names: HashSet<String> = let_map.keys().cloned().collect();
    if let_names.is_empty() {
        return HoistLetsForAssumes {
            prefix_lets: Vec::new(),
            hoisted_names: HashSet::new(),
        };
    }

    let mut needed: HashSet<String> = HashSet::new();
    for step in steps {
        if let TestStep::Assume { cond } = step {
            let mut refs = HashSet::new();
            expr_free_idents(cond, &mut refs);
            refs.retain(|n| let_names.contains(n));
            needed.extend(refs);
        }
    }

    let mut changed = true;
    while changed {
        changed = false;
        for name in needed.clone() {
            if let Some((_, value)) = let_map.get(&name) {
                let mut deps = HashSet::new();
                expr_free_idents(value, &mut deps);
                deps.retain(|d| let_names.contains(d));
                for dep in deps {
                    if needed.insert(dep) {
                        changed = true;
                    }
                }
            }
        }
    }

    if needed.is_empty() {
        return HoistLetsForAssumes {
            prefix_lets: Vec::new(),
            hoisted_names: HashSet::new(),
        };
    }

    let needed_vec: Vec<String> = let_bindings
        .iter()
        .map(|(n, _, _)| n.clone())
        .filter(|n| needed.contains(n))
        .collect();
    let index_of: HashMap<String, usize> = needed_vec
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), i))
        .collect();

    let order = dfs_topo(needed_vec.len(), |i| {
        let name = &needed_vec[i];
        let mut deps = HashSet::new();
        if let Some((_, value)) = let_map.get(name) {
            expr_free_idents(value, &mut deps);
        }
        deps.retain(|d| needed.contains(d) && d != name);
        deps.iter()
            .filter_map(|d| index_of.get(d).copied())
            .collect()
    });

    let prefix_lets: Vec<(String, Option<Type>, Expr)> = order
        .iter()
        .map(|&i| {
            let n = &needed_vec[i];
            let (ty, value) = let_map[n].clone();
            (n.clone(), ty, value)
        })
        .collect();

    HoistLetsForAssumes {
        prefix_lets,
        hoisted_names: needed,
    }
}

/// Filter property body steps after hoisting: drop `assume` (→ hypotheses)
/// and `let` bindings already emitted in the prefix.
pub(crate) fn filter_property_body_after_hoist(
    steps: &[TestStep],
    hoisted: &HashSet<String>,
) -> Vec<TestStep> {
    steps
        .iter()
        .filter(|step| match step {
            TestStep::Assume { .. } => false,
            TestStep::Let { name, .. } => !hoisted.contains(name),
            _ => true,
        })
        .cloned()
        .collect()
}

pub(crate) fn emit_prefix_let_bindings(
    out: &mut String,
    indent: usize,
    lets: &[(String, Option<Type>, Expr)],
    ctx: &LeanExprCtx,
) {
    for (name, ty, value) in lets {
        push_indent(out, indent);
        out.push_str(&format!(
            "let {} := {}\n",
            super::super::core::types::lean_safe_ident(name),
            gen_expr_for_spec_let(value, ctx, ty.as_ref()),
        ));
    }
}

// ---------------------------------------------------------------------------
// Body emission
// ---------------------------------------------------------------------------

pub(crate) fn emit_body(
    events: &[SpecEvent],
    start: usize,
    throw_targets: &HashMap<usize, u32>,
    indent: usize,
    ctx: &SpecStepCtx<'_>,
    out: &mut String,
) {
    let mut state = EmitState::new();
    emit_body_rec(events, start, throw_targets, indent, ctx, &mut state, out);
}

fn emit_body_rec(
    events: &[SpecEvent],
    start: usize,
    throw_targets: &HashMap<usize, u32>,
    indent: usize,
    ctx: &SpecStepCtx<'_>,
    state: &mut EmitState,
    out: &mut String,
) {
    // Expects at the start of a wrapped-call continuation belong to that call.
    let mut i = flush_success_expects(events, start, indent, ctx, state, out);

    while i < events.len() {
        match &events[i] {
            SpecEvent::WrappedCall { .. } => break,
            SpecEvent::InstallPeer {
                peer_entity,
                inst_var,
                args,
                init_state,
            } => {
                emit_install_peer(
                    ctx,
                    peer_entity,
                    inst_var,
                    args,
                    init_state,
                    indent,
                    out,
                );
                i += 1;
            }
            SpecEvent::ExpectEmit { event_name, args } => {
                state.pending_expect_emit = Some((event_name.clone(), args.clone()));
                i += 1;
            }
            SpecEvent::RawCall {
                route_name,
                args,
                is_view,
                callee_entity,
                callee_inst,
            } => {
                emit_raw_call(
                    ctx,
                    state,
                    route_name,
                    args,
                    *is_view,
                    callee_entity.as_deref(),
                    callee_inst.as_deref(),
                    indent,
                    out,
                );
                i += 1;
                i = flush_success_expects(events, i, indent, ctx, state, out);
            }
            SpecEvent::Let { name, ty, value } => {
                push_indent(out, indent);
                let ec = ctx.expr_ctx();
                out.push_str(&format!(
                    "let {} := {}\n",
                    super::super::core::types::lean_safe_ident(name),
                    gen_expr_for_spec_let(value, &ec, ty.as_ref()),
                ));
                i += 1;
            }
            SpecEvent::SetMsgCtx { fields } => {
                let ec = ctx.expr_ctx();
                let mut sticky: Vec<(String, Expr)> = Vec::new();
                for (name, expr) in fields {
                    if name == "value" {
                        state.pending_msg_value = Some(gen_expr(expr, &ec));
                    } else {
                        sticky.push((name.clone(), expr.clone()));
                    }
                }
                if !sticky.is_empty() {
                    push_indent(out, indent);
                    out.push_str(&format_ctx_update_inline(ctx, "ctx", &sticky));
                    out.push('\n');
                }
                i += 1;
            }
            SpecEvent::SetSysCtx { fields } => {
                let upd = format_block_update_inline(ctx, fields);
                if !upd.is_empty() {
                    push_indent(out, indent);
                    out.push_str(&upd);
                    out.push('\n');
                }
                i += 1;
            }
            SpecEvent::Comment(text) => {
                push_indent(out, indent);
                out.push_str("-- ");
                out.push_str(text);
                out.push('\n');
                i += 1;
            }
            SpecEvent::ExpectThrow { .. } => {
                // Consumed only via throw-target wrapping; skip past.
                i += 1;
            }
            SpecEvent::ExpectState(_)
            | SpecEvent::ExpectReturn { .. }
            | SpecEvent::ExpectReturnTuple { .. }
            | SpecEvent::ExpectReturnLens { .. }
            | SpecEvent::ExpectPred { .. }
            | SpecEvent::ExpectTransfers { .. }
            | SpecEvent::ExpectEffectsUnmodelable => {
                // Orphan success expects (no preceding call in this segment).
                i = flush_success_expects(events, i, indent, ctx, state, out);
            }
        }
    }

    if i < events.len() {
        let (route_name, args, is_view, callee_entity, callee_inst) = match &events[i] {
            SpecEvent::WrappedCall {
                route_name,
                args,
                is_view,
                callee_entity,
                callee_inst,
            } => (
                route_name.clone(),
                args.clone(),
                *is_view,
                callee_entity.clone(),
                callee_inst.clone(),
            ),
            _ => unreachable!(),
        };
        let throw_code = throw_targets.get(&i);
        let is_throw_target = throw_code.is_some();
        let ctx_var = bind_one_shot_msg_ctx(state, indent, out);
        let route_call = format_route_call(
            ctx,
            &route_name,
            &args,
            callee_entity.as_deref(),
            callee_inst.as_deref(),
            &ctx_var,
        );

        if ctx.profile.predictable {
            if let Some(code) = throw_code {
                if has_throw_continuation(events, i) {
                    let cont = index_after_throw_pair(events, i).unwrap_or(i + 1);
                    push_indent(out, indent);
                    out.push_str("(\n");
                    push_indent(out, indent + 1);
                    out.push_str(&format!(
                        "Cambrian.RouteResult.errCodeIs ({}) {}",
                        route_call, *code,
                    ));
                    push_indent(out, indent + 1);
                    out.push_str("&&\n");
                    push_indent(out, indent + 1);
                    out.push_str("(\n");
                    emit_body_rec(events, cont, throw_targets, indent + 2, ctx, state, out);
                    if out.ends_with('\n') {
                        out.pop();
                    }
                    push_indent(out, indent + 1);
                    out.push_str(")\n");
                    push_indent(out, indent);
                    out.push_str(")\n");
                } else {
                    push_indent(out, indent);
                    out.push_str(&format!(
                        "Cambrian.RouteResult.errCodeIs ({}) {}\n",
                        route_call, *code,
                    ));
                }
                return;
            }
            push_indent(out, indent);
            let binder = if is_view { "pair" } else { "w" };
            out.push_str(&format!(
                "Cambrian.RouteResult.okAnd ({}) (fun {} =>\n",
                route_call, binder,
            ));
            if is_view {
                let rname =
                    state.alloc_result(route_return_ty(ctx, &route_name, callee_entity.as_deref()));
                push_indent(out, indent + 1);
                out.push_str("let w := pair.fst\n");
                push_indent(out, indent + 1);
                out.push_str(&format!("let {} := pair.snd\n", rname));
            }
            emit_body_rec(events, i + 1, throw_targets, indent + 1, ctx, state, out);
            if out.ends_with('\n') {
                out.pop();
            }
            out.push_str(")\n");
            return;
        }

        push_indent(out, indent);
        out.push_str(&format!("match {} with\n", route_call));

        push_indent(out, indent);
        if let Some(code) = throw_code {
            let n_name = state.alloc_throw_arm();
            out.push_str(&format!("| .error (Cambrian.ThrowCode.ofNat {}) =>\n", n_name));
            push_indent(out, indent + 1);
            if has_throw_continuation(events, i) {
                let cont = index_after_throw_pair(events, i).unwrap_or(i + 1);
                out.push_str(&format!("({} = {}) && (\n", n_name, *code));
                emit_body_rec(events, cont, throw_targets, indent + 2, ctx, state, out);
                if out.ends_with('\n') {
                    out.pop();
                }
                push_indent(out, indent + 1);
                out.push_str(")\n");
            } else {
                out.push_str(&format!("{} = {}\n", n_name, *code));
            }
        } else {
            out.push_str("| .error _ => False\n");
        }

        push_indent(out, indent);
        if is_view {
            let rname =
                state.alloc_result(route_return_ty(ctx, &route_name, callee_entity.as_deref()));
            out.push_str("| .ok pair =>\n");
            push_indent(out, indent + 1);
            if is_throw_target {
                out.push_str("False\n");
            } else {
                push_indent(out, indent + 1);
                out.push_str("let w := pair.fst\n");
                push_indent(out, indent + 1);
                out.push_str(&format!("let {} := pair.snd\n", rname));
                emit_body_rec(events, i + 1, throw_targets, indent + 1, ctx, state, out);
            }
        } else {
            out.push_str("| .ok w =>\n");
            push_indent(out, indent + 1);
            if is_throw_target {
                out.push_str("False\n");
            } else {
                emit_body_rec(events, i + 1, throw_targets, indent + 1, ctx, state, out);
            }
        }
        return;
    }

    // Leaf: conjoin all flushed expect binders.
    let goal = conjoin_props(&state.expect_names);
    push_indent(out, indent);
    out.push_str(&goal);
    out.push('\n');
}

/// Consume a contiguous run of success `expect *` events starting at `start`,
/// emitting `let __eK := <prop>` for each. Skips `expect throw` (handled by
/// throw-target wrapping). Returns the index after the consumed run.
fn flush_success_expects(
    events: &[SpecEvent],
    start: usize,
    indent: usize,
    ctx: &SpecStepCtx<'_>,
    state: &mut EmitState,
    out: &mut String,
) -> usize {
    flush_pending_expect_emit(ctx, state, indent, out);
    let mut i = start;
    while i < events.len() && is_expect_event(&events[i]) {
        if matches!(&events[i], SpecEvent::ExpectThrow { .. }) {
            // Throw is handled by the wrapped-call wrapper; stop the group.
            break;
        }
        if !is_success_expect(&events[i]) {
            break;
        }
        let prop = render_expect_prop(&events[i], ctx, state);
        let ename = state.alloc_expect();
        push_indent(out, indent);
        out.push_str(&format!("let {} := {}\n", ename, prop));
        i += 1;
    }
    i
}

fn render_expect_prop(ev: &SpecEvent, ctx: &SpecStepCtx<'_>, state: &EmitState) -> String {
    let ec = ctx.expr_ctx();
    let result = state.current_result.as_deref().unwrap_or("_result");
    match ev {
        SpecEvent::ExpectReturn { value } => {
            format!("{} = {}", result, gen_expr(value, &ec))
        }
        SpecEvent::ExpectReturnTuple { values } => {
            let parts: Vec<String> = values.iter().map(|v| gen_expr(v, &ec)).collect();
            let tup = if parts.len() == 1 {
                parts[0].clone()
            } else {
                format!("({})", parts.join(", "))
            };
            format!("{} = {}", result, tup)
        }
        SpecEvent::ExpectReturnLens { path, value } => {
            let lhs = render_lens(result, state.current_result_ty.clone(), path, &ec, ctx);
            format!("{} = {}", lhs, gen_expr(value, &ec))
        }
        SpecEvent::ExpectPred { cond } => {
            let mut cond = cond.clone();
            let mut ec = ec;
            if crate::ast::expr_mentions_ident(&cond, crate::ast::EXPECT_RESULT_NAME) {
                let binder = state
                    .current_result
                    .clone()
                    .unwrap_or_else(|| "_result".to_string());
                crate::ast::rename_ident(&mut cond, crate::ast::EXPECT_RESULT_NAME, &binder);
                ec.lets.insert(binder);
            }
            gen_expr(&cond, &ec)
        }
        SpecEvent::ExpectState(fields) => {
            let state_root = spec_state_var(ctx.entity);
            let pieces: Vec<String> = fields
                .iter()
                .map(|(path, expr)| {
                    let lhs = match path.split_first() {
                        Some((PathSegment::Field(m), rest)) => {
                            match ctx.entity.members.iter().find(|x| &x.name == m) {
                                Some(member) => render_lens(
                                    &format!("({}).{}", state_root, m),
                                    Some(member.ty.clone()),
                                    &rest.to_vec(),
                                    &ec,
                                    ctx,
                                ),
                                None => render_lens(&state_root, None, path, &ec, ctx),
                            }
                        }
                        _ => render_lens(&state_root, None, path, &ec, ctx),
                    };
                    let rhs = gen_expr(expr, &ec);
                    format!("{} = {}", lhs, rhs)
                })
                .collect();
            conjoin_props(&pieces)
        }
        SpecEvent::ExpectTransfers { dests } => {
            // PM-005: raw `~> dest` effects — assert each destination's
            // post-balance is at least the inbound `ctx.value` transferred
            // from the entity (when value is 0 this is still a real Prop
            // binder, not the vacuous leaf `True`).
            let pieces: Vec<String> = dests
                .iter()
                .map(|dest| {
                    let d = gen_expr(dest, &ec);
                    format!("(Cambrian.WorldState.balanceOf w ({})) ≥ ctx.value", d)
                })
                .collect();
            conjoin_props(&pieces)
        }
        SpecEvent::ExpectEffectsUnmodelable => {
            // Non-empty effects with no Lean model — fail-loud, not vacuous.
            "False".to_string()
        }
        _ => "True".to_string(),
    }
}

fn route_return_ty(
    ctx: &SpecStepCtx<'_>,
    route_name: &str,
    callee_entity: Option<&str>,
) -> Option<Type> {
    resolve_callee_entity(ctx.program, ctx.entity, callee_entity)
        .routes
        .iter()
        .find(|r| r.name == route_name)
        .and_then(|r| r.return_type.clone())
}

fn lens_unfold(ty: &Type, ctx: &SpecStepCtx<'_>) -> Type {
    let mut cur = ty.clone();
    for _ in 0..32 {
        let Type::Simple(name) = &cur else { break };
        match ctx
            .entity
            .type_aliases
            .iter()
            .chain(ctx.program.type_aliases.iter())
            .find(|a| &a.name == name)
        {
            Some(a) => cur = a.ty.clone(),
            None => break,
        }
    }
    cur
}

fn lens_field_ty(ty: &Type, field: &str, ctx: &SpecStepCtx<'_>) -> Option<Type> {
    let Type::Simple(name) = ty else { return None };
    ctx.entity
        .records
        .iter()
        .chain(ctx.program.records.iter())
        .find(|r| &r.name == name)
        .and_then(|r| r.fields.iter().find(|f| f.name == field))
        .map(|f| f.ty.clone())
}

/// Lower an `expect return` / `expect state` lens. `root_ty` is the type of
/// `root` when known; it selects the projection for each segment (`List`
/// vs `AddressMap` indexing, `.len` on a `List`, the last slot of a
/// right-nested tuple).
fn render_lens(
    root: &str,
    root_ty: Option<Type>,
    path: &FieldPath,
    ec: &LeanExprCtx<'_>,
    ctx: &SpecStepCtx<'_>,
) -> String {
    let mut acc = root.to_string();
    let mut cur = root_ty.map(|t| lens_unfold(&t, ctx));
    for seg in path {
        let next = match (seg, &cur) {
            (PathSegment::Field(name), Some(Type::Generic(g, _))) if name == "len" && g == "Vec" => {
                let ascription = if ec.type_ctx.use_nat_numerics {
                    "Nat"
                } else {
                    "Cambrian.U256"
                };
                acc = format!("(({}).length : {})", acc, ascription);
                None
            }
            (PathSegment::Field(name), ty) => {
                acc = format!("({}).{}", acc, name);
                ty.as_ref().and_then(|t| lens_field_ty(t, name, ctx))
            }
            (PathSegment::TupleIndex(0), ty) => {
                acc = format!("({}).fst", acc);
                match ty {
                    Some(Type::Tuple(items)) => items.first().cloned(),
                    _ => None,
                }
            }
            (PathSegment::TupleIndex(n), ty) => {
                for _ in 0..*n - 1 {
                    acc = format!("({}).snd", acc);
                }
                let items = match ty {
                    Some(Type::Tuple(items)) => Some(items),
                    _ => None,
                };
                if items.is_some_and(|items| *n + 1 == items.len()) {
                    acc = format!("({}).snd", acc);
                } else {
                    acc = format!("({}).snd.fst", acc);
                }
                items.and_then(|items| items.get(*n).cloned())
            }
            (PathSegment::Index(key), Some(Type::Generic(g, ps))) if g == "Vec" && ps.len() == 1 => {
                let idx = match key {
                    Expr::IntLiteral(_) => gen_expr(key, ec),
                    _ if ec.type_ctx.use_nat_numerics => format!("({})", gen_expr(key, ec)),
                    _ => format!("({}).toNat", gen_expr(key, ec)),
                };
                acc = format!("(List.getD ({}) {} default)", acc, idx);
                Some(ps[0].clone())
            }
            (PathSegment::Index(key), ty) => {
                let k = gen_expr(key, ec);
                acc = format!("(Cambrian.AddressMap.lookup {} {} |>.get!)", acc, k);
                match ty {
                    Some(Type::Generic(g, ps)) if g == "HashMap" && ps.len() == 2 => {
                        Some(ps[1].clone())
                    }
                    _ => None,
                }
            }
        };
        cur = next.map(|t| lens_unfold(&t, ctx));
    }
    acc
}

fn flush_pending_expect_emit(
    ctx: &SpecStepCtx<'_>,
    state: &mut EmitState,
    indent: usize,
    out: &mut String,
) {
    if let Some((event_name, args)) = state.pending_expect_emit.take() {
        let prop = render_expect_emit_prop(ctx.entity, &event_name, &args, ctx);
        let ename = state.alloc_expect();
        push_indent(out, indent);
        out.push_str(&format!("let {} := {}\n", ename, prop));
    }
}

fn render_expect_emit_prop(
    owner: &Entity,
    event_name: &str,
    args: &[Expr],
    ctx: &SpecStepCtx<'_>,
) -> String {
    let ec = ctx.expr_ctx();
    let payload = match resolve_event_ctor(ctx.program, owner, event_name) {
        Some(ctor) => {
            let arg_terms: Vec<String> = args.iter().map(|a| gen_expr(a, &ec)).collect();
            if arg_terms.is_empty() {
                format!("Cambrian.Generated.Event.{}", ctor)
            } else {
                format!(
                    "(Cambrian.Generated.Event.{} {})",
                    ctor,
                    arg_terms.join(" ")
                )
            }
        }
        None => return "False".to_string(),
    };
    format!(
        "match w.events.getLast? with | some e => e = {} | none => False",
        payload
    )
}

fn emit_install_peer(
    ctx: &SpecStepCtx<'_>,
    peer_entity: &str,
    inst_var: &str,
    _args: &[Expr],
    init_state: &[(String, Expr)],
    indent: usize,
    out: &mut String,
) {
    let peer = ctx
        .program
        .entities
        .iter()
        .find(|e| e.name == peer_entity)
        .expect("InstallPeer: unknown entity");
    let init_ctx = LeanExprCtx::for_spec(
        ctx.program,
        peer,
        spec_state_var(peer),
        HashSet::new(),
        HashSet::new(),
        ctx.profile,
    )
    .with_deploy_bindings(ctx.deploy_bindings.clone());
    let world_ctx = LeanExprCtx::for_spec(
        ctx.program,
        peer,
        spec_state_var(peer),
        HashSet::new(),
        HashSet::new(),
        ctx.profile,
    )
    .with_world("w", inst_var)
    .with_deploy_bindings(ctx.deploy_bindings.clone());
    push_indent(out, indent);
    out.push_str(&format_init_instance_named(peer, init_state, inst_var, &init_ctx));
    out.push('\n');
    let state_val = state_expr_named(peer, init_state, inst_var, &world_ctx);
    push_indent(out, indent);
    out.push_str(&format!(
        "let w := Cambrian.Generated.World.with{} w {} ({})\n",
        peer.name,
        inst_var,
        state_val,
    ));
}

fn call_inst(
    _ctx: &SpecStepCtx<'_>,
    _callee_entity: Option<&str>,
    callee_inst: Option<&str>,
) -> String {
    callee_inst
        .map(|s| s.to_string())
        .unwrap_or_else(|| "inst".to_string())
}

fn format_route_call(
    ctx: &SpecStepCtx<'_>,
    route_name: &str,
    args: &[Expr],
    callee_entity: Option<&str>,
    callee_inst: Option<&str>,
    ctx_var: &str,
) -> String {
    let callee = resolve_callee_entity(ctx.program, ctx.entity, callee_entity);
    let inst = call_inst(ctx, callee_entity, callee_inst);
    let arg_str = lower_call_args(ctx, args, Some(callee), &inst);
    format!(
        "{}.Routes.{} w {} {}{}",
        callee.name,
        route_name,
        inst,
        ctx_var,
        space_prefixed(&arg_str),
    )
}

fn bind_one_shot_msg_ctx(state: &mut EmitState, indent: usize, out: &mut String) -> String {
    if let Some(v) = state.pending_msg_value.take() {
        push_indent(out, indent);
        out.push_str(&format!("let ctx_call := {{ ctx with value := {} }}\n", v));
        "ctx_call".to_string()
    } else {
        "ctx".to_string()
    }
}

fn emit_raw_call(
    ctx: &SpecStepCtx<'_>,
    state: &mut EmitState,
    route_name: &str,
    args: &[Expr],
    is_view: bool,
    callee_entity: Option<&str>,
    callee_inst: Option<&str>,
    indent: usize,
    out: &mut String,
) {
    let callee = resolve_callee_entity(ctx.program, ctx.entity, callee_entity);
    let inst = call_inst(ctx, callee_entity, callee_inst);
    let ctx_var = bind_one_shot_msg_ctx(state, indent, out);
    if is_view {
        let rname = state.alloc_result(route_return_ty(ctx, route_name, callee_entity));
        push_indent(out, indent);
        out.push_str(&format!(
            "let w_view := {}\n",
            format_route_call(ctx, route_name, args, callee_entity, callee_inst, &ctx_var),
        ));
        push_indent(out, indent);
        out.push_str(&format!("let {} := w_view.snd\n", rname));
        push_indent(out, indent);
        out.push_str("let w := w_view.fst\n");
    } else {
        state.current_result = None;
        push_indent(out, indent);
        out.push_str(&format!(
            "let w := {}\n",
            format_route_call(ctx, route_name, args, callee_entity, callee_inst, &ctx_var),
        ));
    }
}

fn lower_call_args(
    ctx: &SpecStepCtx<'_>,
    args: &[Expr],
    callee: Option<&Entity>,
    inst_var: &str,
) -> String {
    let ec = match callee {
        Some(e) => LeanExprCtx::for_spec(
            ctx.program,
            e,
            spec_state_var(e),
            ctx.params.clone(),
            ctx.lets.clone(),
            ctx.profile,
        )
        .with_world("w", inst_var)
        .with_deploy_bindings(ctx.deploy_bindings.clone()),
        None => ctx.expr_ctx(),
    };
    args.iter()
        .map(|a| {
            let s = gen_expr(a, &ec);
            if needs_parens(&s) {
                format!("({})", s)
            } else {
                s
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn needs_parens(s: &str) -> bool {
    let t = s.trim();
    if t.starts_with('(') && t.ends_with(')') {
        return false;
    }
    t.contains(' ')
}

fn space_prefixed(s: &str) -> String {
    if s.is_empty() {
        String::new()
    } else {
        format!(" {}", s)
    }
}

fn format_ctx_update_inline(ctx: &SpecStepCtx<'_>, var: &str, fields: &[(String, Expr)]) -> String {
    let ec = ctx.expr_ctx();
    let updates: Vec<String> = fields
        .iter()
        .map(|(name, expr)| {
            let lean_field = remap_ctx_field(var, name);
            format!("{} := {}", lean_field, gen_expr(expr, &ec))
        })
        .collect();
    format!("let {} := {{ {} with {} }}", var, var, updates.join(", "))
}

fn format_block_update_inline(ctx: &SpecStepCtx<'_>, fields: &[(String, Expr)]) -> String {
    let ec = ctx.expr_ctx();
    let updates: Vec<String> = fields
        .iter()
        .filter_map(|(name, expr)| {
            sys_block_field(name).map(|(bf, _)| format!("{} := {}", bf, gen_expr(expr, &ec)))
        })
        .collect();
    if updates.is_empty() {
        String::new()
    } else {
        format!(
            "let w := {{ w with block := {{ w.block with {} }} }}",
            updates.join(", ")
        )
    }
}

fn remap_ctx_field(var: &str, name: &str) -> String {
    match (var, name) {
        ("sys", "now") | ("sys", "timestamp") => "timestamp".to_string(),
        ("sys", "chainid") => "chainId".to_string(),
        ("sys", "block_number") | ("sys", "blocknumber") => "blockNumber".to_string(),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;

    fn raw(name: &str) -> SpecEvent {
        SpecEvent::RawCall {
            route_name: name.to_string(),
            args: vec![],
            is_view: false,
            callee_entity: None,
            callee_inst: None,
        }
    }

    fn wrapped(name: &str) -> SpecEvent {
        SpecEvent::WrappedCall {
            route_name: name.to_string(),
            args: vec![],
            is_view: false,
            callee_entity: None,
            callee_inst: None,
        }
    }

    fn span() -> crate::ast::Span {
        crate::ast::Span {
            start: 0,
            end: 0,
            file_id: 0,
        }
    }

    fn entity_with_members(members: Vec<crate::ast::Member>) -> Entity {
        Entity {
            name: "Token".to_string(),
            records: vec![],
            enums: vec![],
            type_aliases: vec![],
            constants: vec![],
            macros: vec![],
            routes: vec![],
            members,
            events: vec![],
            errors: vec![],
            span: span(),
        }
    }

    /// `<name>: <ty> { in <route>(…) => <body> }`
    fn member(name: &str, ty: crate::ast::Type, route: &str, body: Expr) -> crate::ast::Member {
        crate::ast::Member {
            name: name.to_string(),
            ty,
            is_identity: false,
            default_value: None,
            transforms: vec![crate::ast::MemberTransform {
                route_name: route.to_string(),
                params: vec![],
                body,
                phase: None,
                span: span(),
            }],
            span: span(),
        }
    }

    fn map_ty() -> crate::ast::Type {
        crate::ast::Type::Generic(
            "HashMap".to_string(),
            vec![
                crate::ast::Type::Simple("address".to_string()),
                crate::ast::Type::Simple("uint256".to_string()),
            ],
        )
    }

    /// `if <cond> { m } else { m.update(k, v) }` — nested one level down so the
    /// walker, not just the top-level match, has to find it.
    fn guarded_update() -> Expr {
        Expr::Block(vec![Expr::If(
            Box::new(Expr::BoolLiteral(true)),
            Box::new(Expr::Ident("m".to_string())),
            Some(Box::new(Expr::MethodCall(
                Box::new(Expr::Ident("m".to_string())),
                "update".to_string(),
                vec![Expr::IntLiteral(U256::from(0)), Expr::IntLiteral(U256::from(1))],
            ))),
        )])
    }

    fn plain_update() -> Expr {
        Expr::MethodCall(
            Box::new(Expr::Ident("m".to_string())),
            "update".to_string(),
            vec![Expr::IntLiteral(U256::from(0)), Expr::IntLiteral(U256::from(1))],
        )
    }

    fn bare(name: &str) -> Entity {
        let _ = name;
        entity_with_members(vec![])
    }

    #[test]
    fn shape_linear_raw_calls_use_ladder() {
        let events = vec![
            raw("mint"),
            raw("burn"),
            SpecEvent::ExpectState(vec![]),
        ];
        let shape = SpecStatementShape::from_events(&events, &bare("Token"));
        assert_eq!(shape.max_route_depth, 0);
        assert!(!shape.has_throw_expect);
        assert!(!shape.needs_sorry_stub());
    }

    #[test]
    fn shape_throw_expect_needs_sorry() {
        let events = vec![raw("ok"), wrapped("fail"), SpecEvent::ExpectThrow { code: 1 }];
        let shape = SpecStatementShape::from_events(&events, &bare("Token"));
        assert!(shape.has_throw_expect);
        assert_eq!(shape.max_route_depth, 1);
        assert!(shape.needs_sorry_stub());
    }

    #[test]
    fn shape_nested_wrapped_needs_sorry() {
        let events = vec![
            wrapped("a"),
            wrapped("b"),
            SpecEvent::ExpectState(vec![]),
        ];
        let shape = SpecStatementShape::from_events(&events, &bare("Token"));
        assert_eq!(shape.max_route_depth, 2);
        assert!(!shape.has_throw_expect);
        assert!(shape.needs_sorry_stub());
    }

    #[test]
    fn shape_unmodelable_effects_needs_sorry() {
        let events = vec![SpecEvent::ExpectEffectsUnmodelable];
        let shape = SpecStatementShape::from_events(&events, &bare("Token"));
        assert!(shape.has_unmodelable_effects);
        assert!(shape.needs_sorry_stub());
    }

    #[test]
    fn shape_guarded_map_rewrite_needs_sorry() {
        let entity = entity_with_members(vec![member(
            "m_votes",
            map_ty(),
            "delegate",
            guarded_update(),
        )]);
        assert_eq!(
            branching_map_rewrite_routes(&entity),
            HashSet::from(["delegate"]),
        );

        let events = vec![raw("delegate"), SpecEvent::ExpectState(vec![])];
        let shape = SpecStatementShape::from_events(&events, &entity);
        assert!(shape.has_branching_map_rewrite);
        assert_eq!(shape.max_route_depth, 0);
        assert!(shape.needs_sorry_stub());
    }

    #[test]
    fn shape_unguarded_map_rewrite_keeps_ladder() {
        let entity =
            entity_with_members(vec![member("m_votes", map_ty(), "mint", plain_update())]);
        assert!(branching_map_rewrite_routes(&entity).is_empty());

        let events = vec![raw("mint"), SpecEvent::ExpectState(vec![])];
        let shape = SpecStatementShape::from_events(&events, &entity);
        assert!(!shape.has_branching_map_rewrite);
        assert!(!shape.needs_sorry_stub());
    }

    /// A guarded *scalar* transform normalizes to one term and stays bounded by
    /// `maxHeartbeats`, which the ladder recovers from — no stub.
    #[test]
    fn shape_guarded_scalar_rewrite_keeps_ladder() {
        let entity = entity_with_members(vec![member(
            "m_total_supply",
            crate::ast::Type::Simple("uint256".to_string()),
            "settle",
            guarded_update(),
        )]);
        assert!(branching_map_rewrite_routes(&entity).is_empty());

        let events = vec![raw("settle"), SpecEvent::ExpectState(vec![])];
        let shape = SpecStatementShape::from_events(&events, &entity);
        assert!(!shape.needs_sorry_stub());
    }

    /// Only the routes actually called are consulted — an entity with a heavy
    /// route does not stub statements that never touch it.
    #[test]
    fn shape_uncalled_guarded_route_keeps_ladder() {
        let entity = entity_with_members(vec![member(
            "m_votes",
            map_ty(),
            "delegate",
            guarded_update(),
        )]);
        let events = vec![raw("constructor"), SpecEvent::ExpectState(vec![])];
        let shape = SpecStatementShape::from_events(&events, &entity);
        assert!(!shape.has_branching_map_rewrite);
        assert!(!shape.needs_sorry_stub());
    }
}
