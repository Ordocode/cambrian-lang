// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use super::evm_harness_factory::{
    emit_cambrian_factory_field, emit_cambrian_factory_new, emit_deterministic_project_import,
    emit_factory_deploy_entity, emit_factory_predict_assert,
    factory_deploy_fn_name, format_factory_predict_call, CAMBRIAN_FACTORY_VAR,
};
use super::solidity::{
    active_ctx, gen_expr_test, gen_expr_test_ir, has_non_identity_init_params, is_mapping_type,
    reset_active_ctx, reset_active_scratch, set_active_ctx, sol_type_entity, EvmCtx,
};
use super::solidity::core::types::{
    hex_literal_to_sol_for_ty, infer_int_literal_sol_ty, sol_sanitize_ident,
};
use crate::ast::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

mod invariant_predicate_lower;

/// A contract deployed by a `deploy <binding> = <Entity>(...)` step, held
/// for the rest of the enclosing test body so that `call <binding>.route(...)`
/// resolves to the right entity and the right Solidity local.
struct Peer<'a> {
    entity: &'a Entity,
    /// The typed Solidity local (`_peer_asset`), used to make calls.
    var: String,
}

/// Bindings visible at a given point in one test body.
type PeerMap<'a> = std::collections::HashMap<String, Peer<'a>>;

/// Drop `address <b> = address(_peer_<b>);` lines for bindings nothing else
/// mentions.
///
/// A peer is worth binding to a name so it can be *passed* somewhere
/// (`call t0.transfer(pair, 400)`); plenty of tests only ever call on the
/// peer and never pass it, and for those Solidity warns about an unused
/// local. Deciding this up front would mean walking every expression shape
/// in the remaining steps; deciding it here needs only the text that was
/// actually emitted, and the text is the thing the warning is about.
fn drop_unused_peer_addresses(body: &str, bindings: &[String]) -> String {
    let mut out = body.to_string();
    for b in bindings {
        let decl = format!("        address {} = address(_peer_{});\n", b, b);
        if !out.contains(&decl) {
            continue;
        }
        let without = out.replace(&decl, "");
        // Word-boundary count, so `pair` is not found inside `_peer_pair`.
        let mentioned = without
            .match_indices(b.as_str())
            .any(|(i, _)| {
                let before = without[..i].chars().next_back();
                let after = without[i + b.len()..].chars().next();
                !before.is_some_and(is_ident_char) && !after.is_some_and(is_ident_char)
            });
        if !mentioned {
            out = without;
        }
    }
    out
}

/// Emit a `deploy <binding> = <Entity>(args) with { ... }` step.
///
/// Two locals come out of this: the typed one that calls are made through,
/// and an `address` under the binding's own name so the binding can be
/// passed as an argument (`call constructor(asset)`), which is the whole
/// point of deploying a peer in the first place.
fn gen_deploy_peer<'a>(
    peer_entity: &'a Entity,
    binding: &str,
    args: &[Expr],
    init_state: &[(String, Expr)],
    program: &Program,
    peers: &mut PeerMap<'a>,
    out: &mut String,
) {
    let addr_local = sol_sanitize_ident(binding);
    let peer_var = format!("_peer_{}", addr_local);

    // Identity members are CREATE2 salt ingredients: the real factory fixes
    // them at deploy time and no setter exists, so a `with { }` entry naming
    // one has to become a constructor argument rather than a storage poke.
    // Anything not named in the clause keeps the type's default, matching
    // what `setUp()` does for the entity under test.
    let identity_members: Vec<&Member> =
        peer_entity.members.iter().filter(|m| m.is_identity).collect();
    let identity_arg = |m: &Member| -> String {
        init_state
            .iter()
            .find(|(field, _)| field == &m.name)
            .map(|(_, value)| gen_expr_test_typed(value, peer_entity, &m.ty, None))
            .unwrap_or_else(|| default_value_for_type(&m.ty))
    };

    let mut deploy_args: Vec<String> = identity_members.iter().map(|m| identity_arg(m)).collect();
    if !args.is_empty() {
        deploy_args.extend(init_route_arg_exprs_sol(peer_entity, args));
    } else {
        deploy_args.extend(init_route_ctor_arg_defaults(peer_entity));
    }
    out.push_str(&format!(
        "        {} {} = {}(address({}.{}({})));\n",
        peer_entity.name,
        peer_var,
        peer_entity.name,
        CAMBRIAN_FACTORY_VAR,
        factory_deploy_fn_name(&peer_entity.name),
        deploy_args.join(", ")
    ));
    out.push_str(&format!(
        "        address {} = address({});\n",
        addr_local, peer_var
    ));

    // Non-identity seeds are ordinary storage, written the same way a
    // test-level `with { ... }` writes them.
    let storage_seeds: Vec<(String, Expr)> = init_state.iter()
        .filter(|(field, _)| !identity_members.iter().any(|m| &m.name == field))
        .cloned()
        .collect();
    if !storage_seeds.is_empty() {
        gen_state_init(peer_entity, &storage_seeds, &peer_var, program, out);
    }

    peers.insert(binding.to_string(), Peer { entity: peer_entity, var: peer_var });
}

/// Per test/fuzz-function counter for `_ret_<n>` Solidity locals.
struct RetCounter(u32);

impl RetCounter {
    fn new() -> Self {
        Self(0)
    }

    fn bump(&mut self) -> u32 {
        self.0 += 1;
        self.0
    }

    fn current(&self) -> u32 {
        self.0
    }
}

pub(crate) fn find_init_route<'a>(entity: &'a Entity) -> Option<&'a Route> {
    entity
        .routes
        .iter()
        .find(|r| r.is_init || r.name == "constructor")
}

pub(crate) fn is_init_route(route: &Route) -> bool {
    route.is_init || route.name == "constructor"
}

/// `call constructor(...)` or `call <init-route>(...)` in a lowered test body.
pub(crate) fn is_init_route_call(entity: &Entity, route: &str) -> bool {
    if route == "constructor" {
        return true;
    }
    find_init_route(entity).is_some_and(|r| r.name == route)
}

/// True when any generated `test` / `fuzz` body calls the entity init route
/// (per-test `factory.deploy*` in the body, not `setUp`).
fn suite_deploys_via_constructor(
    entity: &Entity,
    tests: &[&TestDecl],
    fuzzes: &[&FuzzDecl],
) -> bool {
    fn body_calls_init(entity: &Entity, steps: &[TestStep]) -> bool {
        steps.iter().any(|step| {
            matches!(
                step,
                TestStep::Call { route, .. } if is_init_route_call(entity, route)
            )
        })
    }
    tests
        .iter()
        .any(|t| body_calls_init(entity, &t.body))
        || fuzzes.iter().any(|f| body_calls_init(entity, &f.body))
}

/// Foundry invariant handler wrapper name for a route. Solidity reserves
/// `constructor` for contract creation, so init routes are prefixed.
pub(crate) fn invariant_handler_action_fn(entity: &Entity, route: &str) -> String {
    if is_init_route_call(entity, route) {
        format!("cam_init_{}", route)
    } else {
        route.to_string()
    }
}

/// Dummy holder used when an address init-route slot has no `senders { ... }`
/// and no handler yet. Matches RehearsalToken's first sender (`0xa01`).
pub(crate) const DUMMY_HOLDER_SOL: &str =
    "address(uint160(uint256(0x0000000000000000000000000000000000000000000000000000000000000a01)))";

/// How to fill one init-route / `initialize` parameter from `init { ... }`.
pub(crate) enum InitRouteArg<'a> {
    Expr(&'a Expr),
    DummyHolder,
    /// Foundry `address(_handler)` / revm `DEFAULT_SENDER`.
    Handler,
    /// Multi-entity sibling instance (`address(_tok)` / `_addr_tok`).
    SiblingInstance(&'a str),
    Default,
}

/// Inputs for resolving init-route / `initialize` arguments.
#[derive(Clone, Copy)]
pub(crate) struct InitRouteFill<'a> {
    pub entity: &'a Entity,
    pub init_state: &'a [(String, Expr)],
    pub deploy: &'a [Expr],
    pub senders: &'a [Expr],
    /// Fill unmatched address slots with dummy `0xa01` (or senders, unary).
    pub dummy_unmatched_address: bool,
    /// Fill unmatched address slots with the invariant handler / tester sender.
    pub handler_unmatched_address: bool,
    /// `(instance_name, entity_name)` available in this invariant.
    pub siblings: &'a [(&'a str, &'a str)],
}

impl<'a> InitRouteFill<'a> {
    pub(crate) fn unit_defaults(entity: &'a Entity) -> Self {
        Self {
            entity,
            init_state: &[],
            deploy: &[],
            senders: &[],
            dummy_unmatched_address: false,
            handler_unmatched_address: false,
            siblings: &[],
        }
    }
}

/// Multi-entity factory deploy loop: tracks which instances are already on-chain
/// so forward sibling refs use `predict*` instead of `address(0)` (U4-4c Step 1c).
pub(crate) struct MultiFactoryDeployCtx<'a> {
    pub entity_for_inst: &'a [(&'a str, &'a Entity)],
    pub inits: &'a [&'a [(String, Expr)]],
    pub siblings: &'a [(&'a str, &'a str)],
    pub deployed: std::collections::HashSet<String>,
}

impl<'a> MultiFactoryDeployCtx<'a> {
    fn sibling_entity(&self, inst: &str) -> Option<&'a Entity> {
        self.entity_for_inst
            .iter()
            .find(|(n, _)| *n == inst)
            .map(|(_, e)| *e)
    }

    fn sibling_init(&self, inst: &str) -> &'a [(String, Expr)] {
        self.entity_for_inst
            .iter()
            .zip(self.inits.iter())
            .find(|((n, _), _)| *n == inst)
            .map(|(_, init)| *init)
            .unwrap_or(&[])
    }

    fn sibling_instance_ref_sol(&self, inst: &str) -> String {
        if self.deployed.contains(inst) {
            return format!("address(_{inst})");
        }
        let entity = self
            .sibling_entity(inst)
            .unwrap_or_else(|| panic!("unknown sibling instance '{inst}'"));
        let init = self.sibling_init(inst);
        let predict_args = identity_ctor_args_sol(entity, init, self.siblings, Some(self));
        format!(
            "address({})",
            format_factory_predict_call(&entity.name, &predict_args)
        )
    }
}

fn pattern_ident(pat: &Pattern) -> Option<&str> {
    match pat {
        Pattern::Ident(s) => Some(s.as_str()),
        Pattern::Deref(inner) => pattern_ident(inner),
        _ => None,
    }
}

/// True when `expr` mentions a bare identifier (used to detect `in ctor(x) => x`).
fn expr_mentions_ident(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::Ident(s) => s == name,
        Expr::BinOp(l, _, r) | Expr::Index(l, r) | Expr::Range(l, r) | Expr::Let(_, l, r) => {
            expr_mentions_ident(l, name) || expr_mentions_ident(r, name)
        }
        Expr::UnaryOp(_, e)
        | Expr::Some(e)
        | Expr::Cast(e, _)
        | Expr::FieldAccess(e, _)
        | Expr::Closure(_, e) => expr_mentions_ident(e, name),
        Expr::MethodCall(recv, _, args) => {
            expr_mentions_ident(recv, name) || args.iter().any(|a| expr_mentions_ident(a, name))
        }
        Expr::FnCall(_, args) | Expr::NamespacedCall { args, .. } => {
            args.iter().any(|a| expr_mentions_ident(a, name))
        }
        Expr::If(c, t, e) => {
            expr_mentions_ident(c, name)
                || expr_mentions_ident(t, name)
                || e.as_ref().is_some_and(|x| expr_mentions_ident(x, name))
        }
        Expr::Block(items) => items.iter().any(|e| expr_mentions_ident(e, name)),
        _ => false,
    }
}

/// Member `init { m_x: v }` may fill ctor slot `i` only when the transform at
/// that slot maps the route parameter into the member (`in ctor(x) => x`), not
/// when the param is only a pattern name on a constant default (`=> 0`).
fn transform_maps_ctor_param_to_init_pin(transform: &MemberTransform, param_idx: usize) -> bool {
    let Some(param_name) = transform.params.get(param_idx).and_then(pattern_ident) else {
        return false;
    };
    expr_mentions_ident(&transform.body, param_name)
}

fn param_is_address(ty: &Type) -> bool {
    matches!(ty, Type::Simple(s) if s == "address") || typed_address_entity(ty).is_some()
}

fn typed_address_entity(ty: &Type) -> Option<&str> {
    match ty {
        Type::TypedAddress(name) => Some(name.as_str()),
        Type::Generic(name, params) if name == "Address" => match params.as_slice() {
            [Type::Simple(entity)] => Some(entity.as_str()),
            _ => None,
        },
        _ => None,
    }
}

fn init_route_is_unary_address(entity: &Entity) -> bool {
    match find_init_route(entity).map(|r| r.params.as_slice()) {
        Some([p]) => param_is_address(&p.ty),
        _ => false,
    }
}

fn sibling_for_typed_address<'a>(
    ty: &Type,
    siblings: &'a [(&'a str, &'a str)],
) -> Option<&'a str> {
    let ename = typed_address_entity(ty)?;
    siblings
        .iter()
        .find(|(_, e)| *e == ename)
        .map(|(n, _)| *n)
}

/// Map each init-route parameter to an `init { ... }` pin, a sibling instance,
/// the invariant handler, a dummy holder, or a per-type default.
/// Constructor transform `Ident` patterns (non-mapping members) are the
/// primary source: `init { m_quorum: 1000 }` fills the `quorum` slot of
/// `constructor(_, _, _, _, quorum)`.
pub(crate) fn resolve_init_route_args<'a>(
    fill: InitRouteFill<'a>,
) -> Vec<(&'a Type, InitRouteArg<'a>)> {
    let Some(route) = find_init_route(fill.entity) else {
        return Vec::new();
    };
    let route_name = route.name.as_str();
    let unary = init_route_is_unary_address(fill.entity);
    route
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut from_init: Option<&Expr> = None;
            for m in &fill.entity.members {
                if is_mapping_type(&m.ty) {
                    continue;
                }
                for t in &m.transforms {
                    if t.route_name != route_name {
                        continue;
                    }
                    if t.params.get(i).and_then(pattern_ident).is_none() {
                        continue;
                    }
                    if let Some((_, expr)) = fill.init_state.iter().find(|(n, _)| n == &m.name) {
                        if transform_maps_ctor_param_to_init_pin(t, i) {
                            from_init = Some(expr);
                            break;
                        }
                    }
                }
                if from_init.is_some() {
                    break;
                }
            }
            if from_init.is_none() {
                if let Some(m) = fill
                    .entity
                    .members
                    .iter()
                    .find(|m| m.is_identity && m.name == p.name)
                {
                    if let Some((_, expr)) = fill.init_state.iter().find(|(n, _)| n == &m.name) {
                        from_init = Some(expr);
                    }
                }
            }
            let arg = if let Some(expr) = from_init {
                // A pin that names a sibling instance resolves to that
                // instance's deployed address (both renderers), not to the
                // bare name.
                if let Expr::Ident(n) = expr {
                    if let Some((inst, _)) =
                        fill.siblings.iter().find(|(inst, _)| *inst == n.as_str())
                    {
                        return (&p.ty, InitRouteArg::SiblingInstance(inst));
                    }
                }
                InitRouteArg::Expr(expr)
            } else if let Some(inst) = sibling_for_typed_address(&p.ty, fill.siblings) {
                InitRouteArg::SiblingInstance(inst)
            } else if param_is_address(&p.ty) {
                unmatched_address_arg(fill, unary)
            } else {
                InitRouteArg::Default
            };
            (&p.ty, arg)
        })
        .collect()
}

fn unmatched_address_arg<'a>(fill: InitRouteFill<'a>, unary: bool) -> InitRouteArg<'a> {
    let deployer = fill
        .deploy
        .first()
        .or_else(|| fill.senders.first());
    if unary {
        if let Some(s) = deployer {
            return InitRouteArg::Expr(s);
        }
    }
    if fill.handler_unmatched_address {
        return InitRouteArg::Handler;
    }
    if fill.dummy_unmatched_address {
        if let Some(s) = deployer {
            return InitRouteArg::Expr(s);
        }
        return InitRouteArg::DummyHolder;
    }
    InitRouteArg::Default
}

/// `init { ... }` fields the constructor / `initialize` already owns.
/// When the init route has no parameters, constructor-owned pins still
/// go through `vm.store` / slot writes (the deploy path cannot take them).
pub(crate) fn leftover_init_state(
    entity: &Entity,
    init_state: &[(String, Expr)],
) -> Vec<(String, Expr)> {
    if !has_non_identity_init_params(entity) {
        return init_state.to_vec();
    }
    let ctor = members_set_in_init_route(entity);
    init_state
        .iter()
        .filter(|(n, _)| !ctor.contains(n))
        .cloned()
        .collect()
}

fn render_init_route_arg_sol(
    entity: &Entity,
    arg: InitRouteArg<'_>,
    ty: &Type,
    deploy_ctx: Option<&MultiFactoryDeployCtx<'_>>,
) -> String {
    match arg {
        InitRouteArg::Expr(expr) => {
            if is_address_type(ty) {
                return gen_address_expr_test(expr, entity);
            }
            gen_expr_test(expr, entity).unwrap_or_else(|| default_value_for_type(ty))
        }
        InitRouteArg::DummyHolder => DUMMY_HOLDER_SOL.to_string(),
        InitRouteArg::Handler => "address(_handler)".to_string(),
        InitRouteArg::SiblingInstance(inst) => deploy_ctx
            .map(|ctx| ctx.sibling_instance_ref_sol(inst))
            .unwrap_or_else(|| format!("address(_{inst})")),
        InitRouteArg::Default => default_value_for_type(ty),
    }
}

fn render_init_route_args_sol(
    fill: InitRouteFill<'_>,
    deploy_ctx: Option<&MultiFactoryDeployCtx<'_>>,
) -> Vec<String> {
    resolve_init_route_args(fill)
        .into_iter()
        .map(|(ty, arg)| render_init_route_arg_sol(fill.entity, arg, ty, deploy_ctx))
        .collect()
}

fn identity_ctor_args_sol(
    entity: &Entity,
    init_state: &[(String, Expr)],
    siblings: &[(&str, &str)],
    deploy_ctx: Option<&MultiFactoryDeployCtx<'_>>,
) -> Vec<String> {
    entity
        .members
        .iter()
        .filter(|m| m.is_identity)
        .map(|m| {
            if let Some((_, expr)) = init_state.iter().find(|(n, _)| n == &m.name) {
                if let Expr::Ident(n) = expr {
                    if siblings.iter().any(|(inst, _)| *inst == n.as_str()) {
                        return deploy_ctx
                            .map(|ctx| ctx.sibling_instance_ref_sol(n))
                            .unwrap_or_else(|| format!("address(_{n})"));
                    }
                }
                gen_expr_test_typed(expr, entity, &m.ty, None)
            } else {
                default_value_for_type(&m.ty)
            }
        })
        .collect()
}

fn identity_deploy_args_default(entity: &Entity) -> Vec<String> {
    entity
        .members
        .iter()
        .filter(|m| m.is_identity)
        .map(|m| default_value_for_type(&m.ty))
        .collect()
}

fn factory_deploy_args_default(entity: &Entity) -> Vec<String> {
    let mut args = identity_deploy_args_default(entity);
    args.extend(init_route_ctor_arg_defaults(entity));
    args
}

fn invariant_init_fill<'a>(
    entity: &'a Entity,
    init_state: &'a [(String, Expr)],
    deploy: &'a [Expr],
    senders: &'a [Expr],
    siblings: &'a [(&'a str, &'a str)],
    handler_unmatched_address: bool,
) -> InitRouteFill<'a> {
    InitRouteFill {
        entity,
        init_state,
        deploy,
        senders,
        dummy_unmatched_address: true,
        handler_unmatched_address,
        siblings,
    }
}

fn invariant_factory_deploy_args(
    entity: &Entity,
    init_state: &[(String, Expr)],
    inv: &InvariantDecl,
    siblings: &[(&str, &str)],
    deploy_ctx: Option<&MultiFactoryDeployCtx<'_>>,
) -> Vec<String> {
    let mut args = identity_ctor_args_sol(entity, init_state, siblings, deploy_ctx);
    let fill = invariant_init_fill(
        entity,
        init_state,
        &inv.deploy,
        &inv.senders,
        siblings,
        true,
    );
    args.extend(render_init_route_args_sol(fill, deploy_ctx));
    args
}

/// U4-4c: handler-first invariant — empty ctor + `cam_wire` after entity deploy.
fn emit_handler_cam_wire_single(
    entity: &Entity,
    var_name: &str,
    inv: &InvariantDecl,
    handler_state_idents: &[String],
    out: &mut String,
) {
    out.push_str("    constructor() {}\n\n");
    out.push_str(&format!(
        "    function cam_wire({} __target) public {{\n",
        entity.name
    ));
    out.push_str(&format!(
        "        require(address({}) == address(0), \"cam_wire: already wired\");\n",
        var_name
    ));
    out.push_str(&format!("        {} = __target;\n", var_name));
    for binding in &inv.track {
        let raw = gen_expr_test(&binding.value, entity).unwrap_or_else(|| "0".to_string());
        let lowered = substitute_member_accessors(&raw, entity, var_name, handler_state_idents);
        out.push_str(&format!("        {} = {};\n", binding.name, lowered));
    }
    out.push_str("    }\n");
}

/// U4-4c: multi-entity handler — empty ctor + `cam_wire` with one param per instance.
fn emit_handler_cam_wire_multi(entity_for_inst: &[(&str, &Entity)], out: &mut String) {
    out.push_str("    constructor() {}\n\n");
    out.push_str("    function cam_wire(");
    let params: Vec<String> = entity_for_inst
        .iter()
        .map(|(n, e)| format!("{} __{}", e.name, n))
        .collect();
    out.push_str(&params.join(", "));
    out.push_str(") public {\n");
    for (inst_name, _) in entity_for_inst {
        out.push_str(&format!(
            "        require(address(_{}) == address(0), \"cam_wire: already wired\");\n",
            inst_name
        ));
        out.push_str(&format!("        _{} = __{};\n", inst_name, inst_name));
    }
    out.push_str("    }\n");
}

/// Deterministic `new Entity(factory, identity…)` — `initialize` is a later phase.
/// Deploy instances so that `Address<E>` ctor params see already-deployed
/// siblings. `inits` carries each instance's `init { ... }` pins (parallel to
/// `entity_for_inst`): a pin that names a sibling instance (`init vlt {
/// m_asset: tok }`) is a deploy-order edge the type graph cannot see — the
/// identity may be typed against an `extern entity` interface
/// (`Address<VaultAsset>`) while the instance is a concrete peer
/// (`ERC20Multi`).
/// Result of topo-sorting multi-entity invariant instances for deploy order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstanceDeployOrderReport {
    pub order: Vec<usize>,
    /// `false` when sibling/typed-address edges contain a cycle — codegen falls
    /// back to declaration order `(0..n)` (U4-4c Step 0 / Step 1c).
    pub topological: bool,
}

pub(crate) fn instance_deploy_order_report(
    entity_for_inst: &[(String, &Entity)],
    inits: &[&[(String, Expr)]],
) -> InstanceDeployOrderReport {
    let n = entity_for_inst.len();
    let mut indeg = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for i in 0..n {
        if let Some(pins) = inits.get(i) {
            for (_, expr) in pins.iter() {
                let Expr::Ident(name) = expr else { continue };
                if let Some(j) = entity_for_inst
                    .iter()
                    .position(|(inst, _)| inst == name)
                {
                    if i != j {
                        adj[j].push(i);
                        indeg[i] += 1;
                    }
                }
            }
        }
        let Some(route) = find_init_route(entity_for_inst[i].1) else {
            continue;
        };
        for p in &route.params {
            let Some(ename) = typed_address_entity(&p.ty) else {
                continue;
            };
            if let Some(j) = entity_for_inst.iter().position(|(_, e)| e.name == ename) {
                if i != j {
                    adj[j].push(i);
                    indeg[i] += 1;
                }
            }
        }
    }
    let mut q: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut order = Vec::with_capacity(n);
    while let Some(i) = q.pop() {
        order.push(i);
        for &k in &adj[i] {
            indeg[k] -= 1;
            if indeg[k] == 0 {
                q.push(k);
            }
        }
    }
    if order.len() == n {
        InstanceDeployOrderReport {
            order,
            topological: true,
        }
    } else {
        InstanceDeployOrderReport {
            order: (0..n).collect(),
            topological: false,
        }
    }
}

pub(crate) fn instance_deploy_order(
    entity_for_inst: &[(String, &Entity)],
    inits: &[&[(String, Expr)]],
) -> Vec<usize> {
    instance_deploy_order_report(entity_for_inst, inits).order
}

/// One row of the U4-4c Step 0 multi-invariant deploy-order inventory (T8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiInvariantDeployInventoryRow {
    pub invariant_name: String,
    pub instance_names: Vec<String>,
    pub deploy_order: Vec<String>,
    pub topological: bool,
    /// `(instance, sibling)` pairs where deploy needs a sibling that sorts
    /// after it in the resolved order — Step 1c must emit `predict*` for these.
    pub forward_sibling_refs: Vec<(String, String)>,
}

pub fn multi_invariant_deploy_inventory_row(
    program: &Program,
    inv: &InvariantDecl,
) -> Option<MultiInvariantDeployInventoryRow> {
    if inv.is_single_entity() {
        return None;
    }
    let entity_for_inst: Vec<(String, &Entity)> = inv
        .instances
        .iter()
        .map(|inst| {
            let entity = program
                .entities
                .iter()
                .find(|e| e.name == inst.entity)
                .unwrap_or_else(|| panic!("unknown entity '{}' in invariant '{}'", inst.entity, inv.name));
            (inst.name.clone(), entity)
        })
        .collect();
    let inits: Vec<&[(String, Expr)]> = inv
        .instances
        .iter()
        .map(|i| i.init.as_slice())
        .collect();
    let report = instance_deploy_order_report(&entity_for_inst, &inits);
    let deploy_order: Vec<String> = report
        .order
        .iter()
        .map(|&idx| entity_for_inst[idx].0.clone())
        .collect();
    let pos: std::collections::HashMap<&str, usize> = deploy_order
        .iter()
        .enumerate()
        .map(|(p, name)| (name.as_str(), p))
        .collect();
    let mut forward_sibling_refs = Vec::new();
    let instance_set: std::collections::HashSet<&str> = entity_for_inst
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    for (idx, (inst_name, entity)) in entity_for_inst.iter().enumerate() {
        let inst_init = inits.get(idx).copied().unwrap_or(&[]);
        for (_, expr) in inst_init {
            if let Expr::Ident(sib) = expr {
                if !instance_set.contains(sib.as_str()) {
                    continue;
                }
                let my_pos = pos[inst_name.as_str()];
                let sib_pos = pos[sib.as_str()];
                if sib_pos > my_pos {
                    forward_sibling_refs.push((inst_name.clone(), sib.clone()));
                }
            }
        }
        let Some(route) = find_init_route(entity) else {
            continue;
        };
        for p in &route.params {
            let Some(ename) = typed_address_entity(&p.ty) else {
                continue;
            };
            if let Some(j) = entity_for_inst.iter().position(|(_, e)| e.name == ename) {
                let sib_name = &entity_for_inst[j].0;
                let my_pos = pos[inst_name.as_str()];
                let sib_pos = pos[sib_name.as_str()];
                if sib_pos > my_pos {
                    forward_sibling_refs.push((inst_name.clone(), sib_name.clone()));
                }
            }
        }
    }
    forward_sibling_refs.sort();
    forward_sibling_refs.dedup();
    Some(MultiInvariantDeployInventoryRow {
        invariant_name: inv.name.clone(),
        instance_names: entity_for_inst.iter().map(|(n, _)| n.clone()).collect(),
        deploy_order,
        topological: report.topological,
        forward_sibling_refs,
    })
}

fn init_route_ctor_arg_defaults(entity: &Entity) -> Vec<String> {
    render_init_route_args_sol(InitRouteFill::unit_defaults(entity), None)
}

fn init_route_arg_exprs_sol(entity: &Entity, args: &[Expr]) -> Vec<String> {
    let Some(route) = find_init_route(entity) else {
        return args
            .iter()
            .map(|a| gen_expr_test(a, entity).unwrap_or_else(|| "0".to_string()))
            .collect();
    };
    args.iter()
        .zip(route.params.iter())
        .map(|(a, p)| gen_expr_test_typed(a, entity, &p.ty, None))
        .collect()
}

fn members_set_in_init_route(entity: &Entity) -> HashSet<String> {
    let route_name = find_init_route(entity)
        .map(|r| r.name.as_str())
        .unwrap_or("constructor");
    entity
        .members
        .iter()
        .filter(|m| {
            m.transforms
                .iter()
                .any(|t| t.route_name == route_name)
        })
        .map(|m| m.name.clone())
        .collect()
}

/// When every unit test shares the same lowered `call constructor` / default
/// `msg { sender }` prefix, hoist it into `setUp()` once.
struct CtorHarness {
    init_args: String,
    prank_sender: Option<String>,
}

fn test_body_calls_constructor(entity: &Entity, body: &[TestStep]) -> bool {
    body.iter().any(|s| {
        matches!(s, TestStep::Call { route, .. } if is_init_route_call(entity, route))
    })
}

fn resolve_test_expr(
    expr: &Expr,
    lets: &std::collections::HashMap<String, String>,
    entity: &Entity,
) -> String {
    if let Expr::Ident(name) = expr {
        if let Some(v) = lets.get(name) {
            return v.clone();
        }
    }
    gen_expr_test(expr, entity).unwrap_or_else(|| "0".to_string())
}

/// Lower init-route ctor args for harness hoist / factory deploy with correct
/// `address` typing (bare small integer literals are invalid as `address`).
fn typed_test_expr_with_lets(
    expr: &Expr,
    entity: &Entity,
    ty: &Type,
    let_exprs: &std::collections::HashMap<String, Expr>,
) -> String {
    if let Expr::Ident(name) = expr {
        if let Some(val) = let_exprs.get(name) {
            return gen_expr_test_typed(val, entity, ty, None);
        }
    }
    gen_expr_test_typed(expr, entity, ty, None)
}

fn resolve_init_route_ctor_args_for_test(
    entity: &Entity,
    args: &[Expr],
    lets: &std::collections::HashMap<String, String>,
    let_exprs: &std::collections::HashMap<String, Expr>,
) -> String {
    let route = find_init_route(entity);
    let empty_params: &[crate::ast::Param] = &[];
    let params = route
        .map(|r| r.params.as_slice())
        .unwrap_or(empty_params);
    args.iter()
        .enumerate()
        .map(|(i, arg)| {
            if let Some(p) = params.get(i) {
                if let Expr::Ident(name) = arg {
                    if let Some(val) = let_exprs.get(name) {
                        return gen_expr_test_typed(val, entity, &p.ty, None);
                    }
                }
                return gen_expr_test_typed(arg, entity, &p.ty, None);
            }
            resolve_test_expr(arg, lets, entity)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Mirror entity `const` declarations into the Foundry test contract so
/// catalog `property` / `invariant` bodies can reference `INITIAL_SUPPLY`
/// and similar names (they are not in scope on the test contract otherwise).
fn gen_msg_sender_start_prank(sender_sol: &str, out: &mut String) {
    out.push_str("        vm.stopPrank();\n");
    out.push_str(&format!("        vm.startPrank({});\n", sender_sol));
}

fn msg_ctor_pair_matches(
    entity: &Entity,
    fields: &[(String, Expr)],
    args: &[Expr],
    lets: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let sender_expr = fields.iter().find(|(k, _)| k == "sender").map(|(_, v)| v)?;
    if args.is_empty() {
        return None;
    }
    let sender_sol = resolve_test_expr(sender_expr, lets, entity);
    let first_sol = resolve_test_expr(&args[0], lets, entity);
    if sender_sol == first_sol {
        Some(sender_sol)
    } else {
        None
    }
}

/// Emit deterministic `initialize` as the factory, then `vm.startPrank(sender)`.
fn emit_det_initialize_then_prank(
    entity: &Entity,
    route: &str,
    args: &[Expr],
    var_name: &str,
    sender_expr: &Expr,
    let_types: Option<&HashMap<String, String>>,
    factory_deploy: bool,
    ctor_hoisted: bool,
    out: &mut String,
) {
    gen_call(
        entity,
        route,
        args,
        var_name,
        false,
        "init",
        true,
        factory_deploy,
        ctor_hoisted,
        &mut RetCounter::new(),
        let_types,
        None,
        out,
    );
    let sender_sol = gen_expr_test_typed(
        sender_expr,
        entity,
        &Type::Simple("address".to_string()),
        let_types,
    );
    gen_msg_sender_start_prank(&sender_sol, out);
}

fn gen_entity_test_constants(entity: &Entity, out: &mut String) {
    if entity.constants.is_empty() {
        return;
    }
    for c in &entity.constants {
        let ty = sol_type_entity(entity, &c.ty, true, &super::solidity::active_ctx());
        let val = gen_expr_test_typed(&c.value, entity, &c.ty, None);
        out.push_str(&format!(
            "    {} private constant {} = {};\n",
            ty, c.name, val
        ));
    }
    out.push('\n');
}

fn extract_ctor_msg_pattern(entity: &Entity, body: &[TestStep]) -> Option<(String, Option<String>)> {
    let mut lets: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut let_exprs: std::collections::HashMap<String, Expr> = std::collections::HashMap::new();
    let mut i = 0;
    while i < body.len() {
        match &body[i] {
            TestStep::Let { name, ty: _, value } => {
                let_exprs.insert(name.clone(), value.clone());
                lets.insert(
                    name.clone(),
                    gen_expr_test(value, entity).unwrap_or_else(|| "0".to_string()),
                );
                i += 1;
            }
            TestStep::SetContext { namespace, fields } if namespace == "msg" => {
                if let Some(TestStep::Call { route, args, .. }) = body.get(i + 1) {
                    if is_init_route_call(entity, route) && !args.is_empty() {
                        let init_args = resolve_init_route_ctor_args_for_test(
                            entity,
                            args,
                            &lets,
                            &let_exprs,
                        );
                        let addr_ty = Type::Simple("address".to_string());
                        let sender_sol = fields
                            .iter()
                            .find(|(k, _)| k == "sender")
                            .map(|(_, sender_expr)| {
                                typed_test_expr_with_lets(
                                    sender_expr,
                                    entity,
                                    &addr_ty,
                                    &let_exprs,
                                )
                            });
                        let prank = sender_sol.filter(|s| s == &init_args);
                        return Some((init_args, prank));
                    }
                }
                i += 1;
            }
            TestStep::Call { route, args, .. } if is_init_route_call(entity, route) => {
                if args.is_empty() {
                    return None;
                }
                let init_args =
                    resolve_init_route_ctor_args_for_test(entity, args, &lets, &let_exprs);
                let prank = if let Some(TestStep::SetContext { namespace, fields }) = body.get(i + 1) {
                    if namespace == "msg" {
                        let addr_ty = Type::Simple("address".to_string());
                        let sender_sol = fields
                            .iter()
                            .find(|(k, _)| k == "sender")
                            .map(|(_, sender_expr)| {
                                typed_test_expr_with_lets(
                                    sender_expr,
                                    entity,
                                    &addr_ty,
                                    &let_exprs,
                                )
                            });
                        sender_sol.filter(|s| s == &init_args)
                    } else {
                        None
                    }
                } else {
                    None
                };
                return Some((init_args, prank));
            }
            TestStep::Assume { .. } | TestStep::Bound { .. } => i += 1,
            _ => return None,
        }
    }
    None
}

fn try_hoist_ctor_harness(
    entity: &Entity,
    tests: &[&TestDecl],
    fuzzes: &[&FuzzDecl],
    deterministic: bool,
) -> Option<CtorHarness> {
    if !deterministic || !has_non_identity_init_params(entity) || tests.is_empty() {
        return None;
    }
    let mut pattern: Option<(String, Option<String>)> = None;
    for t in tests {
        let p = extract_ctor_msg_pattern(entity, &t.body)?;
        match &pattern {
            None => pattern = Some(p),
            Some(prev) if prev == &p => {}
            _ => return None,
        }
    }
    for f in fuzzes {
        if !test_body_calls_constructor(entity, &f.body) {
            continue;
        }
        let p = extract_ctor_msg_pattern(entity, &f.body)?;
        match &pattern {
            None => pattern = Some(p),
            Some(prev) if prev == &p => {}
            _ => return None,
        }
    }
    let (init_args, prank_sender) = pattern?;
    Some(CtorHarness {
        init_args,
        prank_sender,
    })
}

fn should_skip_hoisted_msg(
    harness: Option<&CtorHarness>,
    namespace: &str,
    fields: &[(String, Expr)],
    entity: &Entity,
    lets: &std::collections::HashMap<String, String>,
) -> bool {
    let Some(h) = harness else {
        return false;
    };
    if namespace != "msg" {
        return false;
    }
    let Some(expected) = &h.prank_sender else {
        return false;
    };
    fields.iter().any(|(k, expr)| {
        k == "sender" && &resolve_test_expr(expr, lets, entity) == expected
    })
}

/// Solidity type of a `bound` parameter (falls back to `uint256`).
/// Foundry's `bound(...)` always returns `uint256`; we must cast the result
/// to the parameter's declared type so assignments like
/// `uint64 amount = uint256(bound(...))` do not fail solc 7407 (T-X-008 / T-G-001).
fn bound_param_sol_ty(entity: &Entity, params: &[Param], var: &str) -> String {
    params
        .iter()
        .find(|p| p.name == var)
        .map(|p| sol_type_entity(entity, &p.ty, true, &super::solidity::active_ctx()))
        .unwrap_or_else(|| "uint256".to_string())
}

/// One `bound x in lo..hi` step, as a Solidity assignment.
///
/// Addresses round-trip through `uint160` because `bound` is numeric and an
/// address is a number. That case is not a convenience: without it an
/// invariant cannot confine an actor to a finite set at all. The alternative
/// spelling, `assume p == <literal>`, rejects the call at odds of 2^-160, so
/// the action never executes and the check passes over a trace in which
/// nothing happened — a green suite that tested nothing. Bounding maps every
/// draw into the range instead of discarding it.
fn emit_bound_step(var: &str, ty: &str, lo: &str, hi: &str) -> String {
    if ty == "address" {
        // The bounds are address-shaped literals too — `gen_expr` has
        // already wrapped them as `address(uint160(…))` — so they need the
        // same trip to a number before `bound` will accept them.
        format!(
            "        {} = address(uint160(bound(uint256(uint160({})), uint256(uint160({})), uint256(uint160({})))));\n",
            var, var, lo, hi,
        )
    } else {
        format!("        {} = {}(bound({}, {}, {}));\n", var, ty, var, lo, hi)
    }
}

pub fn generate_evm_tests(
    program: &Program,
    _deterministic: bool,
    inv_cfg: &crate::project::InvariantConfig,
) -> Vec<(String, String)> {
    // U4-6 Step 11: harness always uses CambrianFactory (project import stub).
    generate_evm_tests_for_project(program, true, inv_cfg, Some("Harness"), true)
}

pub fn generate_evm_tests_for_project(
    program: &Program,
    deterministic: bool,
    inv_cfg: &crate::project::InvariantConfig,
    project_name: Option<&str>,
    emit_harness_project_sol: bool,
) -> Vec<(String, String)> {
    let expanded = crate::codegen::solidity::core::alias::expand_type_aliases(program);
    let program: &Program = &expanded;
    reset_active_ctx();
    set_active_ctx(EvmCtx::build(program, deterministic, true));
    reset_active_scratch();

    if program.tests.is_empty() && program.fuzz_tests.is_empty() && program.invariants.is_empty() {
        reset_active_ctx();
        return vec![];
    }

    let mut files = Vec::new();
    if emit_harness_project_sol {
        let pname = project_name.unwrap_or("Harness");
        let project_file = format!("_{}_project.sol", pname);
        let code = super::solidity::evm::gen_evm_solidity(program, deterministic);
        files.push((format!("src/{}", project_file), code));
        for entity in &program.entities {
            let stub = format!(
                "// SPDX-License-Identifier: UNLICENSED\npragma solidity ^0.8.24;\n// Auto-generated stub: re-exports {} from the combined harness project file.\nimport \"./{}\";\n",
                entity.name, project_file,
            );
            files.push((format!("src/{}.sol", entity.name), stub));
        }
    }
    let mut entities_with_tests = std::collections::HashSet::new();
    for t in &program.tests {
        entities_with_tests.insert(t.entity_name.clone());
    }
    for f in &program.fuzz_tests {
        entities_with_tests.insert(f.entity_name.clone());
    }

    for entity_name in &entities_with_tests {
        let entity = match program.entities.iter().find(|e| &e.name == entity_name) {
            Some(e) => e,
            None => continue,
        };
        let tests: Vec<&TestDecl> = program
            .tests
            .iter()
            .filter(|t| &t.entity_name == entity_name)
            .collect();
        let fuzzes: Vec<&FuzzDecl> = program
            .fuzz_tests
            .iter()
            .filter(|f| &f.entity_name == entity_name)
            .collect();
        if tests.is_empty() && fuzzes.is_empty() {
            continue;
        }

        let all_referenced_entities: Vec<&str> = tests
            .iter()
            .flat_map(|t| t.body.iter())
            .chain(fuzzes.iter().flat_map(|f| f.body.iter()))
            .filter_map(|step| {
                if let TestStep::SetRegistry { entity_name, .. } = step {
                    Some(entity_name.as_str())
                } else {
                    None
                }
            })
            .collect();
        let extra_entities: Vec<&Entity> = program
            .entities
            .iter()
            .filter(|e| {
                all_referenced_entities.contains(&e.name.as_str()) && e.name != *entity_name
            })
            .collect();

        let code = gen_test_contract(
            entity,
            &tests,
            &fuzzes,
            program,
            deterministic,
            project_name,
            &extra_entities,
        );
        files.push((format!("test/{}.t.sol", entity_name), code));
    }

    // Generate one Solidity file per invariant declaration: Handler + Invariant_<name>Test.
    // Single-entity invariants emit the legacy single-handler form. Multi-entity
    // (`for system`) invariants emit a Handler holding all instance handles.
    for inv in &program.invariants {
        if inv.emit_policy == crate::ast::InvariantEmitPolicy::Superseded {
            continue;
        }
        if inv.is_single_entity() {
            let entity = match program
                .entities
                .iter()
                .find(|e| e.name == inv.entity_name())
            {
                Some(e) => e,
                None => continue,
            };
            // The file is keyed by entity as well as by invariant name: two
            // entities may carry the same invariant title (a derived component
            // and its base routinely do), and a name-only key made the second
            // write clobber the first — the lost invariant was never run, and
            // nothing in the build output said so.
            let code = gen_invariant_file(
                entity,
                inv,
                inv_cfg,
                deterministic,
                program,
                project_name,
            );
            let fn_name = sanitize_test_name(&inv.name);
            files.push((
                format!("test/Invariant_{}_{}.t.sol", entity.name, fn_name),
                code,
            ));
        } else {
            let code = gen_invariant_multi_file(
                program,
                inv,
                inv_cfg,
                deterministic,
                project_name,
            );
            let fn_name = sanitize_test_name(&inv.name);
            files.push((format!("test/Invariant_{}.t.sol", fn_name), code));
        }
    }

    reset_active_ctx();
    files
}

/// Default tuning per generated profile. A project can do
/// `FOUNDRY_PROFILE=cambrian forge test` for fast iteration and
/// `FOUNDRY_PROFILE=cambrian_night forge test` for overnight stress
/// without writing a single line of toml.
const CAMBRIAN_PROFILE_DEFAULTS: &[(&str, u32, u32, u32)] = &[
    // (name, fuzz_runs, invariant_runs, invariant_depth)
    ("cambrian", 100, 50, 50),
    ("cambrian_night", 10_000, 5_000, 250),
];

pub fn generate_foundry_toml(
    config: Option<&crate::project::FoundryConfig>,
    fuzz: Option<&crate::project::FuzzConfig>,
    invariant: Option<&crate::project::InvariantConfig>,
) -> String {
    let solc_version = config
        .and_then(|c| c.solc_version.as_deref())
        .unwrap_or("0.8.24");
    let evm_version = config
        .and_then(|c| c.evm_version.as_deref())
        .unwrap_or("prague");
    let optimizer = config.and_then(|c| c.optimizer).unwrap_or(false);
    let optimizer_runs = config.and_then(|c| c.optimizer_runs).unwrap_or(200);
    let via_ir = config.and_then(|c| c.via_ir).unwrap_or(false);
    // Phase Library-4: serialise `foundry.remappings = [...]` into the
    // foundry.toml `remappings = [...]` array. Mirrored into every
    // tiered profile so `FOUNDRY_PROFILE=cambrian forge test` sees the
    // same library paths as the default profile.
    let remappings_lit = config
        .and_then(|c| c.remappings.as_ref())
        .map(|r| {
            let items: Vec<String> = r
                .iter()
                .map(|s| format!("\"{}\"", s.replace('"', "\\\"")))
                .collect();
            format!("remappings = [{}]\n", items.join(", "))
        })
        .unwrap_or_default();
    let fuzz_runs = config
        .and_then(|c| c.fuzz_runs)
        .or_else(|| fuzz.map(|f| f.runs))
        .unwrap_or(256);
    let fuzz_seed_line = fuzz
        .and_then(|f| {
            if f.seed != 0 {
                Some(format!("seed = \"0x{:x}\"\n", f.seed))
            } else {
                None
            }
        })
        .unwrap_or_default();
    let max_local_rejects_line = fuzz
        .map(|f| format!("max_test_rejects = {}\n", f.max_local_rejects))
        .unwrap_or_default();
    let invariant_section = invariant
        .map(|i| {
            let seed_line = if i.seed != 0 {
                format!("seed = \"0x{:x}\"\n", i.seed)
            } else {
                String::new()
            };
            format!(
                "\n[profile.default.invariant]\nruns = {}\ndepth = {}\nfail_on_revert = {}\n{}",
                i.runs, i.depth, i.fail_on_revert, seed_line,
            )
        })
        .unwrap_or_default();

    let mut tiered = String::new();
    for &(name, default_fuzz, default_inv, default_depth) in CAMBRIAN_PROFILE_DEFAULTS {
        let user_tuning = config
            .and_then(|c| c.profiles.as_ref())
            .and_then(|m| m.get(name));
        let p_fuzz = user_tuning
            .and_then(|t| t.fuzz_runs)
            .unwrap_or(default_fuzz);
        let p_inv_runs = user_tuning
            .and_then(|t| t.invariant_runs)
            .unwrap_or(default_inv);
        let p_inv_depth = user_tuning
            .and_then(|t| t.invariant_depth)
            .unwrap_or(default_depth);
        let p_fail = user_tuning
            .and_then(|t| t.fail_on_revert)
            .or_else(|| invariant.map(|i| i.fail_on_revert))
            .unwrap_or(false);
        tiered.push_str(&format!(
            "\n[profile.{name}]\nsrc = \"src\"\nout = \"out\"\nlibs = [\"lib\"]\n\
             solc_version = \"{solc_version}\"\n\
             evm_version = \"{evm_version}\"\n\
             optimizer = {optimizer}\noptimizer_runs = {optimizer_runs}\nvia_ir = {via_ir}\n\
             {remappings_lit}\
             \n[profile.{name}.fuzz]\nruns = {p_fuzz}\n\
             \n[profile.{name}.invariant]\nruns = {p_inv_runs}\ndepth = {p_inv_depth}\nfail_on_revert = {p_fail}\n",
        ));
    }

    format!(
        r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "{solc_version}"
evm_version = "{evm_version}"
optimizer = {optimizer}
optimizer_runs = {optimizer_runs}
via_ir = {via_ir}
{remappings_lit}
[profile.default.fuzz]
runs = {fuzz_runs}
{fuzz_seed_line}{max_local_rejects_line}{invariant_section}{tiered}"#
    )
}

/// `forge install` args that copy forge-std as plain files, not a git submodule.
///
/// Default `forge install` (Foundry 1.7) runs `git submodule add`, which
/// force-stages a mode-160000 gitlink in the parent Cambrian repo even when
/// the path is gitignored. Do not add `--root .`: Foundry still walks up to
/// this checkout and errors with "Library directory is not relative to the
/// repository root".
pub const FORGE_STD_INSTALL_ARGS: &[&str] = &["install", "foundry-rs/forge-std", "--no-git"];

/// Install forge-std under `out_dir/lib/forge-std` without touching `.gitmodules`.
///
/// `cargo test` runs many of these at once. Parallel `git clone`s of the same
/// repo get connection resets, so installs take a cross-process directory lock
/// and retry a partial tree.
pub fn install_forge_std(out_dir: &Path) -> Result<(), String> {
    if out_dir.join("lib/forge-std").is_dir() {
        return Ok(());
    }
    let _lock = ForgeStdInstallLock::acquire();
    let mut last = String::from("forge install forge-std failed");
    for attempt in 0..4 {
        if attempt > 0 {
            let _ = std::fs::remove_dir_all(out_dir.join("lib/forge-std"));
            std::thread::sleep(std::time::Duration::from_millis(250 * attempt));
        }
        let output = Command::new("forge")
            .args(FORGE_STD_INSTALL_ARGS)
            .current_dir(out_dir)
            .output()
            .map_err(|e| format!("forge install: {e}"))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        last = format!("forge install forge-std failed:\n{stdout}{stderr}");
    }
    Err(last)
}

/// `mkdir` lock in the temp dir. Drop removes it. A crashed holder is stolen
/// after two minutes so a later test is not stuck.
struct ForgeStdInstallLock {
    path: Option<PathBuf>,
}

impl ForgeStdInstallLock {
    fn acquire() -> Self {
        let path = std::env::temp_dir().join("cambrian-forge-std-install.lock");
        for _ in 0..240 {
            match std::fs::create_dir(&path) {
                Ok(()) => return Self { path: Some(path) },
                Err(_) => {
                    if let Ok(meta) = std::fs::metadata(&path) {
                        if meta
                            .modified()
                            .ok()
                            .and_then(|t| t.elapsed().ok())
                            .is_some_and(|age| age.as_secs() > 120)
                        {
                            let _ = std::fs::remove_dir(&path);
                            continue;
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
        Self { path: None }
    }
}

impl Drop for ForgeStdInstallLock {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_dir(path);
        }
    }
}

pub fn generate_setup_sh() -> String {
    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

if [ ! -d "lib/forge-std" ]; then
    forge {}
fi

cat <<'EOM'
Setup complete.

Run the default suite:
    forge test

Run with the tiered Cambrian profiles:
    FOUNDRY_PROFILE=cambrian       forge test    # cheap CI cycle
    FOUNDRY_PROFILE=cambrian_night forge test    # overnight stress
EOM
"#,
        FORGE_STD_INSTALL_ARGS.join(" ")
    )
}

fn gen_test_contract(
    entity: &Entity,
    tests: &[&TestDecl],
    fuzzes: &[&FuzzDecl],
    program: &Program,
    deterministic: bool,
    project_name: Option<&str>,
    _extra_entities: &[&Entity],
) -> String {
    let mut out = String::new();
    let det_factory = project_name.is_some();
    let project_name = project_name.unwrap_or("project");

    out.push_str("// SPDX-License-Identifier: UNLICENSED\n");
    out.push_str("pragma solidity ^0.8.24;\n\n");
    out.push_str("import \"forge-std/Test.sol\";\n");
    if det_factory {
        emit_deterministic_project_import(project_name, &mut out);
    }
    out.push_str(&format!("import \"../src/{}.sol\";\n", entity.name));

    for e in program.entities.iter().filter(|e| e.name != entity.name) {
        let names_entity = |step: &TestStep| matches!(
            step,
            TestStep::SetRegistry { entity_name, .. } | TestStep::DeployPeer { entity: entity_name, .. }
                if entity_name == &e.name
        );
        let referenced = tests.iter().any(|t| t.body.iter().any(names_entity))
            || fuzzes.iter().any(|f| f.body.iter().any(names_entity));
        if referenced {
            out.push_str(&format!("import \"../src/{}.sol\";\n", e.name));
        }
    }

    out.push_str(&format!("\ncontract {}Test is Test {{\n", entity.name));

    let var_name = entity_var_name(&entity.name);
    out.push_str(&format!("    {} internal {};\n", entity.name, var_name));
    if det_factory {
        emit_cambrian_factory_field(&mut out);
    }
    gen_entity_test_constants(entity, &mut out);

    let ctor_harness = if det_factory {
        try_hoist_ctor_harness(entity, tests, fuzzes, deterministic)
    } else {
        None
    };
    let setup_deploys = ctor_harness.is_some() || !suite_deploys_via_constructor(entity, tests, fuzzes);
    // `setUp` skips the default instance when some bodies deploy their own;
    // the other bodies still need one before they seed state or call.
    let needs_default_deploy = |steps: &[TestStep]| {
        det_factory
            && !setup_deploys
            && !steps.iter().any(|s| match s {
                TestStep::Call { route, .. } => is_init_route_call(entity, route),
                TestStep::DeployPeer { entity: e, .. } => e == &entity.name,
                _ => false,
            })
    };

    out.push_str("    function setUp() public {\n");
    if det_factory {
        // BUG-U4 U4-4: deploy through the real CambrianFactory (production path).
        emit_cambrian_factory_new(&mut out);
        if let Some(h) = &ctor_harness {
            let mut deploy_args = identity_deploy_args_default(entity);
            if !h.init_args.is_empty() {
                deploy_args.extend(h.init_args.split(", ").map(str::to_string));
            }
            emit_factory_deploy_entity(&entity.name, &var_name, &deploy_args, &mut out);
            if let Some(sender) = &h.prank_sender {
                gen_msg_sender_start_prank(sender, &mut out);
            }
        } else if !suite_deploys_via_constructor(entity, tests, fuzzes) {
            // View-only / route tests with no per-body `call constructor` still
            // need a CREATE2 instance (BUG-U4 U4-4). Suites where every test
            // deploys its own args keep `setUp` factory-only (stdlib ERC20).
            let deploy_args = factory_deploy_args_default(entity);
            emit_factory_deploy_entity(&entity.name, &var_name, &deploy_args, &mut out);
        }
    }
    out.push_str("    }\n");

    for test in tests {
        out.push('\n');
        out.push_str(&gen_test_fn(
            entity,
            test,
            program,
            &var_name,
            deterministic,
            det_factory,
            ctor_harness.as_ref(),
            needs_default_deploy(&test.body),
        ));
    }

    for fuzz in fuzzes {
        out.push('\n');
        out.push_str(&gen_fuzz_fn(
            entity,
            fuzz,
            program,
            &var_name,
            deterministic,
            det_factory,
            ctor_harness.as_ref(),
            needs_default_deploy(&fuzz.body),
        ));
    }

    out.push_str("}\n");
    out
}

fn gen_test_fn(
    entity: &Entity,
    test: &TestDecl,
    program: &Program,
    var_name: &str,
    deterministic: bool,
    factory_deploy: bool,
    ctor_harness: Option<&CtorHarness>,
    default_deploy: bool,
) -> String {
    let mut peers: PeerMap = PeerMap::new();
    // `deploy x = <entity under test>(...)` rebinds the subject: from that
    // step on, unqualified `call` and `expect state` address the instance
    // the test wired itself, not the default-seeded one from `setUp()`.
    // Without this an `expect state` after such a deploy would read the
    // wrong contract and pass for the wrong reason.
    let mut subject_var: String = var_name.to_string();
    let mut ret_cnt = RetCounter::new();
    let fn_name_base = sanitize_test_name(&test.name);
    let fn_name = match &test.tag {
        Some(t) => format!("{}_{}", fn_name_base, sanitize_tag_for_fn(t)),
        None => fn_name_base.clone(),
    };
    let mut out = String::new();
    if let Some(tag) = &test.tag {
        out.push_str(&format!("    /// @dev tag: {}\n", tag));
    }
    out.push_str(&format!("    function test_{}() public {{\n", fn_name));

    if default_deploy {
        emit_factory_deploy_entity(&entity.name, var_name, &factory_deploy_args_default(entity), &mut out);
    }
    if !test.init_state.is_empty() {
        gen_state_init(entity, &test.init_state, var_name, program, &mut out);
    }

    if test.skip_from {
        out.push_str("        // skip from: prank as address(1) to pass from-check\n");
        out.push_str("        vm.prank(address(1));\n");
    }

    let body = &test.body;
    let addr_lets = address_typed_lets(entity, program, body);
    let mut lets: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut let_types: HashMap<String, String> = HashMap::new();
    let mut pending_msg_value: Option<String> = None;
    let mut mapping_pins_emitted = false;
    let mut i = 0;
    while i < body.len() {
        match &body[i] {
            TestStep::SetContext { namespace, fields } => {
                if namespace == "msg" {
                    if let Some(TestStep::Call { route, args, .. }) = body.get(i + 1) {
                        if is_init_route_call(entity, route) {
                            if msg_ctor_pair_matches(entity, fields, args, &lets).is_some() {
                                if ctor_harness.is_some() {
                                    i += 2;
                                    continue;
                                }
                                if deterministic {
                                    if let Some((_, sender_expr)) =
                                        fields.iter().find(|(k, _)| k == "sender")
                                    {
                                        emit_det_initialize_then_prank(
                                            entity,
                                            route,
                                            args,
                                            var_name,
                                            sender_expr,
                                            Some(&let_types),
                                            factory_deploy,
                                            false,
                                            &mut out,
                                        );
                                    }
                                    i += 2;
                                    continue;
                                }
                            }
                        }
                    }
                }
                gen_context_block(
                    namespace,
                    fields,
                    entity,
                    var_name,
                    Some(&let_types),
                    &mut pending_msg_value,
                    &mut out,
                );
                i += 1;
            }
            TestStep::SetRegistry { entity_name, .. } => {
                out.push_str(&format!(
                    "        // registry {} -- skipped (TVM-specific)\n",
                    entity_name
                ));
                i += 1;
            }
            TestStep::Let { name, ty, value } => {
                let decl_ty = let_decl_type(ty, name, &addr_lets);
                let sol_ty = gen_let_binding(entity, name, decl_ty.as_ref(), value, &mut out);
                let_types.insert(name.clone(), sol_ty);
                lets.insert(
                    name.clone(),
                    gen_expr_test(value, entity).unwrap_or_else(|| "0".to_string()),
                );
                i += 1;
            }
            TestStep::DeployPeer { binding, entity: peer_name, args, init_state } => {
                match program.entities.iter().find(|e| &e.name == peer_name) {
                    Some(peer_entity) => {
                        gen_deploy_peer(
                            peer_entity,
                            binding,
                            args,
                            init_state,
                            program,
                            &mut peers,
                            &mut out,
                        );
                        if peer_entity.name == entity.name {
                            if let Some(p) = peers.get(binding) {
                                subject_var = p.var.clone();
                            }
                        }
                    }
                    // Validator T24 rejects this; the comment keeps the
                    // generated file readable if it ever slips through.
                    None => out.push_str(&format!(
                        "        // deploy {} = {}(...): entity not found\n",
                        binding, peer_name
                    )),
                }
                i += 1;
            }
            TestStep::Call { target, route, args } => {
                if !mapping_pins_emitted && !test.init_state.is_empty() {
                    gen_mapping_pin_init(
                        entity,
                        &test.init_state,
                        var_name,
                        program,
                        &let_types,
                        &mut out,
                    );
                    mapping_pins_emitted = true;
                }
                // A qualified call retargets the callee only: `expect state`
                // stays bound to the entity under test, which is what
                // `for <Entity>` in the header promises.
                let (call_entity, call_var) = match target {
                    Some(binding) => match peers.get(binding) {
                        Some(p) => (p.entity, p.var.clone()),
                        None => (entity, var_name.to_string()),
                    },
                    None => (entity, subject_var.clone()),
                };
                if is_init_route_call(entity, route) && ctor_harness.is_some() {
                    i += 1;
                    if let Some(TestStep::SetContext { namespace, fields }) = body.get(i) {
                        if should_skip_hoisted_msg(ctor_harness, namespace, fields, entity, &lets)
                        {
                            i += 1;
                        }
                    }
                    continue;
                }
                let call_var = call_var.as_str();
                let assertions = collect_post_call_assertions(body, i + 1);
                let has_throw = assertions
                    .iter()
                    .any(|a| matches!(a, PostCallAssert::Throw(_)));
                let has_return = post_call_needs_return(&assertions);
                let has_effects = assertions
                    .iter()
                    .any(|a| matches!(a, PostCallAssert::Effects(_)));

                if has_throw {
                    if let Some(PostCallAssert::Throw(code)) = assertions
                        .iter()
                        .find(|a| matches!(a, PostCallAssert::Throw(_)))
                    {
                        out.push_str(&format!(
                            "        vm.expectRevert(bytes(\"throw({})\"));\n",
                            code
                        ));
                    }
                }

                if has_effects {
                    for a in &assertions {
                        if let PostCallAssert::Effects(elements) = a {
                            gen_pre_call_effect_setup(elements, entity, &subject_var, &mut out);
                        }
                    }
                }

                gen_call(
                    call_entity,
                    route,
                    args,
                    call_var,
                    has_return,
                    &test.name,
                    deterministic,
                    factory_deploy,
                    ctor_harness.is_some(),
                    &mut ret_cnt,
                    Some(&let_types),
                    pending_msg_value.as_deref(),
                    &mut out,
                );
                pending_msg_value = None;

                for a in &assertions {
                    match a {
                        PostCallAssert::Return(value) => {
                            gen_expect_return(
                                call_entity,
                                &PendingReturn::Scalar(value.clone()),
                                route,
                                program,
                                &test.name,
                                ret_cnt.current(),
                                Some(&let_types),
                                &mut out,
                            );
                        }
                        PostCallAssert::ReturnTuple(values) => {
                            gen_expect_return(
                                call_entity,
                                &PendingReturn::Tuple(values.clone()),
                                route,
                                program,
                                &test.name,
                                ret_cnt.current(),
                                Some(&let_types),
                                &mut out,
                            );
                        }
                        PostCallAssert::ReturnLens(path, value) => {
                            gen_expect_return(
                                call_entity,
                                &PendingReturn::Lens(path.clone(), value.clone()),
                                route,
                                program,
                                &test.name,
                                ret_cnt.current(),
                                Some(&let_types),
                                &mut out,
                            );
                        }
                        PostCallAssert::State(fields) => {
                            gen_expect_state(entity, fields, &subject_var, &test.name, Some(&let_types), &mut out);
                        }
                        PostCallAssert::Effects(elements) => {
                            gen_post_call_effect_checks(elements, entity, &mut out);
                        }
                        PostCallAssert::Pred(cond) => {
                            gen_expect_pred(
                                program,
                                entity,
                                call_entity,
                                route,
                                cond,
                                &subject_var,
                                &test.name,
                                ret_cnt.current(),
                                &mut out,
                            );
                        }
                        PostCallAssert::Throw(_) => {}
                    }
                }

                i = i + 1 + assertions.len();
            }
            TestStep::ExpectEmit { event_name, args } => {
                gen_expect_emit(entity, program, event_name, args, Some(&let_types), &mut out);
                i += 1;
            }
            TestStep::ExpectState { fields } => {
                gen_expect_state(entity, fields, &subject_var, &test.name, Some(&let_types), &mut out);
                i += 1;
            }
            TestStep::ExpectThrow { .. }
            | TestStep::ExpectReturn { .. }
            | TestStep::ExpectReturnTuple { .. }
            | TestStep::ExpectReturnLens { .. }
            | TestStep::ExpectPred { .. }
            | TestStep::ExpectEffects { .. } => {
                i += 1;
            }
            TestStep::Assume { .. } | TestStep::Bound { .. } => {
                i += 1;
            }
            TestStep::SkipIf { .. } | TestStep::AdvanceTime { .. } => {
                // Invariant-action-only steps; ignored in regular `test`
                // bodies. Validator T15/T16 catches misuse.
                i += 1;
            }
        }
    }

    if !peers.is_empty() {
        let bindings: Vec<String> = peers.keys().cloned().collect();
        out = drop_unused_peer_addresses(&out, &bindings);
    }
    out.push_str("        vm.stopPrank();\n");
    out.push_str("    }\n");
    out
}

fn expr_idents_for_assume_hoist(expr: &Expr, out: &mut HashSet<String>) {
    match expr {
        Expr::Ident(name) => {
            out.insert(name.clone());
        }
        Expr::BinOp(l, _, r) | Expr::Range(l, r) => {
            expr_idents_for_assume_hoist(l, out);
            expr_idents_for_assume_hoist(r, out);
        }
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) | Expr::Some(e) => {
            expr_idents_for_assume_hoist(e, out);
        }
        Expr::If(cond, t, e) => {
            expr_idents_for_assume_hoist(cond, out);
            expr_idents_for_assume_hoist(t, out);
            if let Some(el) = e {
                expr_idents_for_assume_hoist(el, out);
            }
        }
        Expr::MethodCall(b, _, args) => {
            expr_idents_for_assume_hoist(b, out);
            for a in args {
                expr_idents_for_assume_hoist(a, out);
            }
        }
        Expr::FnCall(_, args) | Expr::MacroRef(_, args) => {
            for a in args {
                expr_idents_for_assume_hoist(a, out);
            }
        }
        Expr::Index(b, k) => {
            expr_idents_for_assume_hoist(b, out);
            expr_idents_for_assume_hoist(k, out);
        }
        _ => {}
    }
}

/// LG-F05 (EVM): hoist only `let` bindings referenced by `assume` steps so
/// preconditions like `to != deployer` compile, without evaluating unrelated
/// leading lets (e.g. `amount - 1`) before `vm.assume` rejects bad fuzz inputs.
fn lets_to_hoist_before_assumes(body: &[TestStep]) -> HashSet<String> {
    let let_names: HashSet<String> = body
        .iter()
        .filter_map(|step| {
            if let TestStep::Let { name, .. } = step {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();
    if let_names.is_empty() {
        return HashSet::new();
    }
    let mut needed: HashSet<String> = HashSet::new();
    for step in body {
        if let TestStep::Assume { cond } = step {
            let mut refs = HashSet::new();
            expr_idents_for_assume_hoist(cond, &mut refs);
            refs.retain(|n| let_names.contains(n));
            needed.extend(refs);
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for name in needed.clone() {
            if let Some(TestStep::Let { value, .. }) = body.iter().find(|s| {
                matches!(s, TestStep::Let { name: n, .. } if n == &name)
            }) {
                let mut deps = HashSet::new();
                expr_idents_for_assume_hoist(value, &mut deps);
                deps.retain(|d| let_names.contains(d));
                for dep in deps {
                    if needed.insert(dep) {
                        changed = true;
                    }
                }
            }
        }
    }
    needed
}

fn gen_fuzz_fn(
    entity: &Entity,
    fuzz: &FuzzDecl,
    program: &Program,
    var_name: &str,
    deterministic: bool,
    factory_deploy: bool,
    ctor_harness: Option<&CtorHarness>,
    default_deploy: bool,
) -> String {
    let mut ret_cnt = RetCounter::new();
    let mut peers: PeerMap = PeerMap::new();
    let mut subject_var: String = var_name.to_string();
    let fn_name_base = sanitize_test_name(&fuzz.name);
    let fn_name = match &fuzz.tag {
        Some(t) => format!("{}_{}", fn_name_base, sanitize_tag_for_fn(t)),
        None => fn_name_base.clone(),
    };
    let params: Vec<String> = fuzz
        .params
        .iter()
        .map(|p| {
            format!(
                "{} {}",
                sol_type_entity(entity, &p.ty, true, &super::solidity::active_ctx()),
                p.name
            )
        })
        .collect();
    let mut out = String::new();
    if let Some(tag) = &fuzz.tag {
        out.push_str(&format!("    /// @dev tag: {}\n", tag));
    }
    if let Some(n) = fuzz.runs {
        out.push_str(&format!(
            "    /// forge-config: default.fuzz.runs = {}\n",
            n
        ));
    }
    out.push_str(&format!(
        "    function testFuzz_{}({}) public {{\n",
        fn_name,
        params.join(", ")
    ));

    if default_deploy {
        emit_factory_deploy_entity(&entity.name, var_name, &factory_deploy_args_default(entity), &mut out);
    }
    if !fuzz.init_state.is_empty() {
        gen_state_init(entity, &fuzz.init_state, var_name, program, &mut out);
    }

    if fuzz.skip_from {
        out.push_str("        // skip from: prank as address(1) to pass from-check\n");
        out.push_str("        vm.prank(address(1));\n");
    }

    let param_types: HashMap<String, String> = fuzz
        .params
        .iter()
        .map(|p| {
            (
                p.name.clone(),
                sol_type_entity(entity, &p.ty, true, &super::solidity::active_ctx()),
            )
        })
        .collect();
    let body = &fuzz.body;
    let addr_lets = address_typed_lets(entity, program, body);
    let hoisted_for_assumes = lets_to_hoist_before_assumes(body);
    let mut lets: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut let_types = param_types;
    for step in body {
        if let TestStep::Let { name, ty, value } = step {
            if !hoisted_for_assumes.contains(name) {
                continue;
            }
            let decl_ty = let_decl_type(ty, name, &addr_lets);
                let sol_ty = gen_let_binding(entity, name, decl_ty.as_ref(), value, &mut out);
            let_types.insert(name.clone(), sol_ty);
            lets.insert(
                name.clone(),
                gen_expr_test(value, entity).unwrap_or_else(|| "0".to_string()),
            );
        }
    }
    // Forge `bound` must run before `assume` so cross-parameter preconditions
    // (e.g. `amount > 1000 - frz`) are checked on the narrowed values, not
    // the raw fuzz draw that `bound` would otherwise overwrite afterward.
    for step in body {
        if let TestStep::Bound {
            var,
            lo,
            hi,
            inclusive,
        } = step
        {
            let lo_s = gen_expr_test(lo, entity).unwrap_or_else(|| "0".to_string());
            let hi_s_raw = gen_expr_test(hi, entity).unwrap_or_else(|| "0".to_string());
            let hi_s = if *inclusive {
                hi_s_raw
            } else {
                format!("({}) - 1", hi_s_raw)
            };
            let ty = bound_param_sol_ty(entity, &fuzz.params, var);
            out.push_str(&emit_bound_step(var, &ty, &lo_s, &hi_s));
        }
    }
    // Property `assume` steps may follow other `let` bindings; emit all
    // assumes before those later locals so underflowing preconditions
    // (e.g. `amount - 1`) are not evaluated on rejected fuzz inputs.
    for step in body {
        if let TestStep::Assume { cond } = step {
            let c = gen_expr_test_with_param_types(cond, entity, &let_types);
            out.push_str(&format!("        vm.assume({});\n", c));
        }
    }

    let mut pending_msg_value: Option<String> = None;
    let mut i = 0;
    while i < body.len() {
        match &body[i] {
            TestStep::Bound { .. } | TestStep::Assume { .. } => {
                i += 1;
            }
            TestStep::SetContext { namespace, fields } => {
                if namespace == "msg" {
                    if let Some(TestStep::Call { route, args, .. }) = body.get(i + 1) {
                        if is_init_route_call(entity, route) {
                            if msg_ctor_pair_matches(entity, fields, args, &lets).is_some() {
                                if ctor_harness.is_some() {
                                    i += 2;
                                    continue;
                                }
                            }
                            if deterministic && !args.is_empty() {
                                if let Some((_, sender_expr)) =
                                    fields.iter().find(|(k, _)| k == "sender")
                                {
                                    emit_det_initialize_then_prank(
                                        entity,
                                        route,
                                        args,
                                        var_name,
                                        sender_expr,
                                        Some(&let_types),
                                        factory_deploy,
                                        false,
                                        &mut out,
                                    );
                                }
                                i += 2;
                                continue;
                            }
                        }
                    }
                }
                gen_context_block(
                    namespace,
                    fields,
                    entity,
                    var_name,
                    Some(&let_types),
                    &mut pending_msg_value,
                    &mut out,
                );
                i += 1;
            }
            TestStep::SetRegistry { entity_name, .. } => {
                out.push_str(&format!(
                    "        // registry {} -- skipped (TVM-specific)\n",
                    entity_name
                ));
                i += 1;
            }
            TestStep::Let { name, ty, value } => {
                if hoisted_for_assumes.contains(name) {
                    i += 1;
                    continue;
                }
                let decl_ty = let_decl_type(ty, name, &addr_lets);
                let sol_ty = gen_let_binding(entity, name, decl_ty.as_ref(), value, &mut out);
                let_types.insert(name.clone(), sol_ty);
                lets.insert(
                    name.clone(),
                    gen_expr_test(value, entity).unwrap_or_else(|| "0".to_string()),
                );
                i += 1;
            }
            TestStep::DeployPeer { binding, entity: peer_name, args, init_state } => {
                match program.entities.iter().find(|e| &e.name == peer_name) {
                    Some(peer_entity) => {
                        gen_deploy_peer(
                            peer_entity,
                            binding,
                            args,
                            init_state,
                            program,
                            &mut peers,
                            &mut out,
                        );
                        if peer_entity.name == entity.name {
                            if let Some(p) = peers.get(binding) {
                                subject_var = p.var.clone();
                            }
                        }
                    }
                    // Validator T24 rejects this; the comment keeps the
                    // generated file readable if it ever slips through.
                    None => out.push_str(&format!(
                        "        // deploy {} = {}(...): entity not found\n",
                        binding, peer_name
                    )),
                }
                i += 1;
            }
            TestStep::Call { target, route, args } => {
                // A qualified call retargets the callee only: `expect state`
                // stays bound to the entity under test, which is what
                // `for <Entity>` in the header promises.
                let (call_entity, call_var) = match target {
                    Some(binding) => match peers.get(binding) {
                        Some(p) => (p.entity, p.var.clone()),
                        None => (entity, var_name.to_string()),
                    },
                    None => (entity, subject_var.clone()),
                };
                if is_init_route_call(entity, route) && ctor_harness.is_some() {
                    i += 1;
                    if let Some(TestStep::SetContext { namespace, fields }) = body.get(i) {
                        if should_skip_hoisted_msg(ctor_harness, namespace, fields, entity, &lets) {
                            i += 1;
                        }
                    }
                    continue;
                }
                let call_var = call_var.as_str();
                let assertions = collect_post_call_assertions(body, i + 1);
                let has_throw = assertions
                    .iter()
                    .any(|a| matches!(a, PostCallAssert::Throw(_)));
                let has_return = post_call_needs_return(&assertions);
                let has_effects = assertions
                    .iter()
                    .any(|a| matches!(a, PostCallAssert::Effects(_)));

                if has_throw {
                    if let Some(PostCallAssert::Throw(code)) = assertions
                        .iter()
                        .find(|a| matches!(a, PostCallAssert::Throw(_)))
                    {
                        out.push_str(&format!(
                            "        vm.expectRevert(bytes(\"throw({})\"));\n",
                            code
                        ));
                    }
                }

                if has_effects {
                    for a in &assertions {
                        if let PostCallAssert::Effects(elements) = a {
                            gen_pre_call_effect_setup(elements, entity, &subject_var, &mut out);
                        }
                    }
                }

                gen_call(
                    call_entity,
                    route,
                    args,
                    call_var,
                    has_return,
                    &fuzz.name,
                    deterministic,
                    factory_deploy,
                    ctor_harness.is_some(),
                    &mut ret_cnt,
                    Some(&let_types),
                    pending_msg_value.as_deref(),
                    &mut out,
                );
                pending_msg_value = None;

                for a in &assertions {
                    match a {
                        PostCallAssert::Return(value) => {
                            gen_expect_return(
                                call_entity,
                                &PendingReturn::Scalar(value.clone()),
                                route,
                                program,
                                &fuzz.name,
                                ret_cnt.current(),
                                Some(&let_types),
                                &mut out,
                            );
                        }
                        PostCallAssert::ReturnTuple(values) => {
                            gen_expect_return(
                                call_entity,
                                &PendingReturn::Tuple(values.clone()),
                                route,
                                program,
                                &fuzz.name,
                                ret_cnt.current(),
                                Some(&let_types),
                                &mut out,
                            );
                        }
                        PostCallAssert::ReturnLens(path, value) => {
                            gen_expect_return(
                                call_entity,
                                &PendingReturn::Lens(path.clone(), value.clone()),
                                route,
                                program,
                                &fuzz.name,
                                ret_cnt.current(),
                                Some(&let_types),
                                &mut out,
                            );
                        }
                        PostCallAssert::State(fields) => {
                            gen_expect_state(entity, fields, &subject_var, &fuzz.name, Some(&let_types), &mut out);
                        }
                        PostCallAssert::Effects(elements) => {
                            gen_post_call_effect_checks(elements, entity, &mut out);
                        }
                        PostCallAssert::Pred(cond) => {
                            gen_expect_pred(
                                program,
                                entity,
                                call_entity,
                                route,
                                cond,
                                &subject_var,
                                &fuzz.name,
                                ret_cnt.current(),
                                &mut out,
                            );
                        }
                        PostCallAssert::Throw(_) => {}
                    }
                }

                i = i + 1 + assertions.len();
            }
            TestStep::ExpectState { fields } => {
                gen_expect_state(entity, fields, &subject_var, &fuzz.name, Some(&let_types), &mut out);
                i += 1;
            }
            TestStep::ExpectEmit { .. }
            | TestStep::ExpectThrow { .. }
            | TestStep::ExpectReturn { .. }
            | TestStep::ExpectReturnTuple { .. }
            | TestStep::ExpectReturnLens { .. }
            | TestStep::ExpectPred { .. }
            | TestStep::ExpectEffects { .. } => {
                i += 1;
            }
            TestStep::SkipIf { .. } | TestStep::AdvanceTime { .. } => {
                // Invariant-action-only steps; ignored in `fuzz` bodies.
                i += 1;
            }
        }
    }

    if !peers.is_empty() {
        let bindings: Vec<String> = peers.keys().cloned().collect();
        out = drop_unused_peer_addresses(&out, &bindings);
    }
    out.push_str("    }\n");
    out
}

enum PostCallAssert {
    State(Vec<(FieldPath, Expr)>),
    Throw(u32),
    Return(Expr),
    ReturnTuple(Vec<Expr>),
    ReturnLens(FieldPath, Expr),
    Effects(Vec<TestEffectElement>),
    Pred(Expr),
}

fn post_call_needs_return(assertions: &[PostCallAssert]) -> bool {
    assertions.iter().any(|a| match a {
        PostCallAssert::Return(_)
        | PostCallAssert::ReturnTuple(_)
        | PostCallAssert::ReturnLens(_, _) => true,
        PostCallAssert::Pred(cond) => expr_mentions_ident(cond, EXPECT_RESULT_NAME),
        _ => false,
    })
}

fn collect_post_call_assertions(body: &[TestStep], start: usize) -> Vec<PostCallAssert> {
    let mut result = Vec::new();
    let mut i = start;
    while i < body.len() {
        match &body[i] {
            TestStep::ExpectState { fields } => {
                result.push(PostCallAssert::State(fields.clone()));
            }
            TestStep::ExpectThrow { code } => {
                result.push(PostCallAssert::Throw(*code));
            }
            TestStep::ExpectReturn { value } => {
                result.push(PostCallAssert::Return(value.clone()));
            }
            TestStep::ExpectReturnTuple { values } => {
                result.push(PostCallAssert::ReturnTuple(values.clone()));
            }
            TestStep::ExpectReturnLens { path, value } => {
                result.push(PostCallAssert::ReturnLens(path.clone(), value.clone()));
            }
            TestStep::ExpectEffects { elements } => {
                result.push(PostCallAssert::Effects(elements.clone()));
            }
            TestStep::ExpectPred { cond } => {
                result.push(PostCallAssert::Pred(cond.clone()));
            }
            _ => break,
        }
        i += 1;
    }
    result
}

enum PendingReturn {
    Scalar(Expr),
    Tuple(Vec<Expr>),
    Lens(FieldPath, Expr),
}

/// Normalize a rendered Solidity expression for `bytes32(uint256(...))`
/// slot writes in Foundry `vm.store` seeding. `bool` literals must become
/// `0`/`1` (solc rejects `uint256(false)`), and `address` values route
/// through `uint160`.
fn vm_store_slot_value(rendered: &str, member: Option<&Member>) -> String {
    let is_address = member
        .map(|m| matches!(&m.ty, crate::ast::Type::Simple(n) if n == "address"))
        .unwrap_or(false);
    if is_address || rendered.starts_with("address(") {
        return format!("uint160({})", rendered);
    }
    match rendered {
        "true" => "1".to_string(),
        "false" => "0".to_string(),
        other => other.to_string(),
    }
}

fn is_hashmap_literal(expr: &Expr) -> bool {
    matches!(expr, Expr::RecordConstruct(name, _) if name == "__HashMap")
}

/// Seed `mapping(K => V)` slots via `vm.store` after test `let` bindings
/// that supply map keys are in scope (T-ARCH-014 / W2-BC-07b).
pub(crate) fn gen_mapping_pin_init(
    entity: &Entity,
    init_state: &[(String, Expr)],
    var_name: &str,
    program: &Program,
    let_types: &HashMap<String, String>,
    out: &mut String,
) {
    let layout = super::storage_layout::compute_layout_with_program(entity, false, program);
    for (field_name, value) in init_state {
        if !is_hashmap_literal(value) {
            continue;
        }
        let member = entity.members.iter().find(|m| m.name == *field_name);
        let Some(info) = layout.lookup(field_name) else {
            continue;
        };
        if !matches!(info.kind, super::storage_layout::SlotKind::Mapping) {
            continue;
        }
        let Expr::RecordConstruct(_, entries) = value else {
            continue;
        };
        for (key, val) in entries {
            let key_rendered = gen_expr_test_with_param_types(
                &Expr::Ident(key.clone()),
                entity,
                let_types,
            );
            let val_rendered = vm_store_slot_value(
                &gen_expr_test_with_param_types(val, entity, let_types),
                member,
            );
            out.push_str(&format!(
                "        vm.store(address({}), bytes32(uint256(keccak256(abi.encode({}, uint256({}))))), bytes32(uint256({})));\n",
                var_name, key_rendered, info.slot, val_rendered,
            ));
        }
    }
}

pub(crate) fn gen_state_init(
    entity: &Entity,
    init_state: &[(String, Expr)],
    var_name: &str,
    program: &Program,
    out: &mut String,
) {
    // Use the shared Solidity layout (including packing) so `vm.store`
    // writes land on the same slots the contract actually uses. A naive
    // "one member = one slot" counter under-counts when Solidity packs
    // e.g. `address` + `uint64` into a single word (T-G-001 / Governor
    // `m_quorum` lives at slot 3, not 4). Program-scope unit enums pack
    // as uint8 (PW3-O-003 / O-014).
    let layout = super::storage_layout::compute_layout_with_program(entity, false, program);

    for (field_name, value) in init_state {
        let member = entity.members.iter().find(|m| m.name == *field_name);

        if let Some(m) = member {
            if m.is_identity {
                out.push_str(&format!(
                    "        // with {{ {}: ... }} -- identity member, set via constructor\n",
                    field_name
                ));
                continue;
            }
        }

        let Some(info) = layout.lookup(field_name) else {
            out.push_str(&format!(
                "        // with {{ {}: ... }} -- unknown member\n",
                field_name
            ));
            continue;
        };

        if matches!(info.kind, super::storage_layout::SlotKind::DynamicArray) {
            out.push_str(&format!(
                "        // with {{ {}: ... }} -- dynamic array init via vm.store not yet supported\n",
                field_name
            ));
            continue;
        }
        if matches!(info.kind, super::storage_layout::SlotKind::Mapping) {
            if is_hashmap_literal(value) {
                continue;
            }
            out.push_str(&format!(
                "        // with {{ {}: ... }} -- mapping init requires `{{ key => val }}` literal\n",
                field_name
            ));
            continue;
        }

        let val = vm_store_slot_value(
            &gen_expr_test(value, entity).unwrap_or_else(|| "0".to_string()),
            member,
        );
        if layout.is_exclusive(info) {
            out.push_str(&format!(
                "        vm.store(address({}), bytes32(uint256({})), bytes32(uint256({})));\n",
                var_name, info.slot, val
            ));
            continue;
        }

        // Packed field: read-modify-write so we don't clobber neighbors
        // that share the slot. Solidity packs from the low-order bytes,
        // so the field occupies bits `[offset*8, offset*8 + size*8)`.
        let bit_offset = info.offset as u32 * 8;
        let bit_size = info.size as u32 * 8;
        // `bit_size` is in 1..=248 for packed values (full-slot values
        // take the branch above), so `(1 << bit_size) - 1` always fits
        // in `u128` and the Solidity `uint256(...)` literal is fine.
        let low_mask: u128 = if bit_size >= 128 {
            u128::MAX
        } else {
            (1u128 << bit_size) - 1
        };
        out.push_str("        {\n");
        out.push_str(&format!(
            "            bytes32 __cam_word = vm.load(address({}), bytes32(uint256({})));\n",
            var_name, info.slot
        ));
        out.push_str(&format!(
            "            uint256 __cam_mask = ~(uint256({:#x}) << {});\n",
            low_mask, bit_offset
        ));
        out.push_str("            uint256 __cam_cleared = uint256(__cam_word) & __cam_mask;\n");
        out.push_str(&format!(
            "            uint256 __cam_placed = (uint256({}) & uint256({:#x})) << {};\n",
            val, low_mask, bit_offset
        ));
        out.push_str(&format!(
            "            vm.store(address({}), bytes32(uint256({})), bytes32(__cam_cleared | __cam_placed));\n",
            var_name, info.slot
        ));
        out.push_str("        }\n");
    }
}

/// Non-identity, non-mapping state members forall-ized (`field: *` / bare
/// `*`) for an invariant instance, minus pinned and unsupported members.
/// These drive the randomized initial state on the dynamic (Foundry / revm)
/// targets. Identity / mapping members and context (`msg::`/`sys::`) targets
/// are not seeded on the dynamic backends (Lean-only); they're filtered out.
pub(crate) fn forall_dynamic_fields<'a>(
    entity: &'a Entity,
    inst: &InvariantInstance,
) -> Vec<&'a Member> {
    use std::collections::HashSet;
    let spec = &inst.forall_state;
    let pinned: HashSet<&str> = inst.init.iter().map(|(n, _)| n.as_str()).collect();

    // Candidate field names, in declaration order for `*`, then explicit
    // `field: *` targets.
    let mut candidates: Vec<&str> = Vec::new();
    if spec.all_state {
        for m in &entity.members {
            candidates.push(m.name.as_str());
        }
    }
    for t in &spec.targets {
        if let ForallTarget::StateField(f) = t {
            candidates.push(f.as_str());
        }
    }

    let mut out: Vec<&Member> = Vec::new();
    for name in candidates {
        if let Some(m) = entity.members.iter().find(|m| m.name == name) {
            if m.is_identity || is_mapping_type(&m.ty) {
                continue;
            }
            if pinned.contains(name) || out.iter().any(|x: &&Member| x.name == name) {
                continue;
            }
            out.push(m);
        }
    }
    out
}

/// Solidity expression producing a fuzzer-seeded random initial value for a
/// forall-ized member on the Foundry target (`vm.random*` cheatcodes). The
/// result is always a `uint256` so it can flow through `gen_state_init`'s
/// `vm.store(..., bytes32(uint256(<value>)))` write. The sample is shaped to
/// the member's type so the seeded slot is canonical: `bool` collapses to
/// `0/1` and `address` masks to 160 bits (a raw 32-byte word would leave the
/// slot "dirty", which Solidity bool reads in particular mishandle).
fn foundry_random_init_value(ty: &Type) -> String {
    match ty {
        Type::Simple(n) if n == "bool" => "vm.randomUint() % 2".to_string(),
        Type::Simple(n) if n == "address" => "uint256(uint160(vm.randomUint()))".to_string(),
        _ => "vm.randomUint()".to_string(),
    }
}

/// Emit the `uint256 __fa_<prefix><field> = vm.random*();` locals and the
/// `vm.store` writes that seed an invariant's forall-ized initial state on
/// the Foundry target. `prefix` disambiguates per-instance locals in the
/// multi-entity form (empty for single-entity).
fn gen_forall_state_init(
    entity: &Entity,
    fields: &[&Member],
    var_name: &str,
    prefix: &str,
    program: &Program,
    out: &mut String,
) {
    if fields.is_empty() {
        return;
    }
    out.push_str("        // forall init: randomized starting state (vm.random* cheatcodes)\n");
    let mut pins: Vec<(String, Expr)> = Vec::new();
    for m in fields {
        let local = format!("__fa_{}{}", prefix, m.name);
        out.push_str(&format!(
            "        uint256 {} = {};\n",
            local,
            foundry_random_init_value(&m.ty),
        ));
        pins.push((m.name.clone(), Expr::Ident(local)));
    }
    gen_state_init(entity, &pins, var_name, program, out);
}

/// Emit the `ctx { ... }` setup for a Foundry invariant `setUp`. Concrete pins
/// map to `vm.warp` / `targetSender`; a forall (`*`) on `sys::now` randomizes
/// the initial timestamp, while a forall on `msg::sender` simply leaves the
/// sender pool unrestricted so Foundry fuzzes `msg.sender` per call.
fn gen_invariant_foundry_ctx(
    ctx: &ContextSpec,
    entity: &Entity,
    skip_ctx_sender_target_pool: bool,
    out: &mut String,
) {
    for e in &ctx.entries {
        match (e.namespace.as_str(), e.field.as_str()) {
            ("sys", "now") | ("sys", "timestamp") => match &e.value {
                Some(v) => {
                    let s = gen_expr_test(v, entity).unwrap_or_else(|| "0".to_string());
                    out.push_str(&format!("        vm.warp({});\n", s));
                }
                None => out
                    .push_str("        vm.warp(vm.randomUint()); // ctx { sys::now: * }\n"),
            },
            ("msg", "sender") => match &e.value {
                Some(_) if skip_ctx_sender_target_pool => {
                    out.push_str(
                        "        // ctx { msg::sender }: deploy { } present — not added to targetSender pool (PL-F-INV-04-CTX)\n",
                    );
                }
                Some(v) => {
                    let s = gen_address_expr_test(v, entity);
                    out.push_str(&format!("        targetSender({});\n", s));
                }
                None => out.push_str(
                    "        // ctx { msg::sender: * }: senders left unrestricted (Foundry fuzzes msg.sender per call)\n",
                ),
            },
            (ns, f) => out.push_str(&format!(
                "        // ctx {{ {}::{} }}: not lowered on the Foundry invariant target\n",
                ns, f,
            )),
        }
    }
}

fn gen_context_block(
    namespace: &str,
    fields: &[(String, Expr)],
    entity: &Entity,
    var_name: &str,
    let_types: Option<&HashMap<String, String>>,
    pending_msg_value: &mut Option<String>,
    out: &mut String,
) {
    match namespace {
        "msg" => {
            for (field, value) in fields {
                match field.as_str() {
                    "sender" => {
                        let val = gen_expr_test_typed(
                            value,
                            entity,
                            &Type::Simple("address".to_string()),
                            let_types,
                        );
                        gen_msg_sender_start_prank(&val, out);
                    }
                    "value" => {
                        let val = gen_expr_test(value, entity).unwrap_or_else(|| "0".to_string());
                        out.push_str(&format!("        vm.deal(address(this), {});\n", val));
                        *pending_msg_value = Some(val);
                    }
                    "timestamp" => {
                        let val = gen_expr_test(value, entity).unwrap_or_else(|| "0".to_string());
                        out.push_str(&format!("        vm.warp({});\n", val));
                    }
                    _ => {
                        out.push_str(&format!(
                            "        // msg.{} -- not directly supported on EVM\n",
                            field
                        ));
                    }
                }
            }
        }
        "sys" => {
            for (field, value) in fields {
                let val = gen_expr_test(value, entity).unwrap_or_else(|| "0".to_string());
                match field.as_str() {
                    "now" | "timestamp" => {
                        out.push_str(&format!("        vm.warp({});\n", val));
                    }
                    "balance" => {
                        out.push_str(&format!(
                            "        vm.deal(address({}), {});\n",
                            var_name, val
                        ));
                    }
                    "blockNumber" | "logicaltime" => {
                        out.push_str(&format!("        vm.roll({});\n", val));
                    }
                    _ => {
                        out.push_str(&format!(
                            "        // sys.{} -- not directly supported on EVM\n",
                            field
                        ));
                    }
                }
            }
        }
        _ => {
            out.push_str(&format!(
                "        // {} context -- not supported on EVM\n",
                namespace
            ));
        }
    }
}

fn spec_let_sol_type(entity: &Entity, decl_ty: Option<&Type>, value: &Expr) -> String {
    if let Some(t) = decl_ty {
        return sol_type_entity(entity, t, true, &super::solidity::active_ctx());
    }
    let rendered = gen_expr_test(value, entity).unwrap_or_default();
    if rendered.starts_with("address(") {
        return "address".to_string();
    }
    infer_sol_type(value)
}

/// Untyped test `let`s whose every typed use is an address slot (route
/// argument, `msg::sender`, event argument, map key, member value). They are
/// declared `address` from those uses, never from the literal's shape
/// (TYPED-LIT-0); a let with any non-address typed use keeps its default.
fn address_typed_lets(entity: &Entity, program: &Program, body: &[TestStep]) -> HashSet<String> {
    let untyped: HashSet<&str> = body
        .iter()
        .filter_map(|s| match s {
            TestStep::Let { name, ty: None, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    let mut addr: HashSet<String> = HashSet::new();
    let mut other: HashSet<String> = HashSet::new();
    let mut note = |e: &Expr, ty: &Type| {
        if let Expr::Ident(n) = e {
            if untyped.contains(n.as_str()) {
                if is_address_type(ty) {
                    addr.insert(n.clone());
                } else {
                    other.insert(n.clone());
                }
            }
        }
    };
    fn walk_state(
        entity: &Entity,
        ty: &Type,
        rest: &[PathSegment],
        value: &Expr,
        note: &mut dyn FnMut(&Expr, &Type),
    ) {
        let ctx = active_ctx();
        match (map_kv(ty), rest.first()) {
            (Some((k, v)), Some(PathSegment::Index(ix))) => {
                note(ix, k);
                walk_state(entity, v, &rest[1..], value, note);
            }
            (Some((k, v)), None) => {
                if let Expr::RecordConstruct(n, pairs) = value {
                    if n == "__HashMap" {
                        for (key, val) in pairs {
                            note(&map_literal_key(key), k);
                            walk_state(entity, v, &[], val, note);
                        }
                    }
                }
            }
            (None, Some(PathSegment::Field(f))) => {
                let fty = state_record(entity, &ctx, ty)
                    .and_then(|r| r.fields.iter().find(|x| &x.name == f))
                    .map(|x| x.ty.clone());
                if let Some(fty) = fty {
                    walk_state(entity, &fty, &rest[1..], value, note);
                }
            }
            (None, None) => match (option_inner(ty), value) {
                (Some(inner), Expr::Some(v)) => walk_state(entity, inner, &[], v, note),
                _ => note(value, ty),
            },
            _ => {}
        }
    }
    for step in body {
        match step {
            TestStep::Call { target: None, route, args } => {
                if let Some(r) = entity.routes.iter().find(|r| &r.name == route) {
                    for (p, a) in r.params.iter().zip(args) {
                        note(a, &p.ty);
                    }
                }
            }
            TestStep::SetContext { namespace, fields } if namespace == "msg" => {
                for (f, v) in fields {
                    if f == "sender" {
                        note(v, &Type::Simple("address".to_string()));
                    }
                }
            }
            TestStep::ExpectEmit { event_name, args } => {
                let decl = entity
                    .events
                    .iter()
                    .chain(program.events.iter())
                    .find(|e| &e.name == event_name);
                if let Some(d) = decl {
                    for (p, a) in d.params.iter().zip(args) {
                        note(a, &p.ty);
                    }
                }
            }
            TestStep::ExpectState { fields } => {
                for (path, value) in fields {
                    let Some(PathSegment::Field(m)) = path.first() else { continue };
                    if let Some(member) = entity.members.iter().find(|x| &x.name == m) {
                        walk_state(entity, &member.ty, &path[1..], value, &mut note);
                    }
                }
            }
            _ => {}
        }
    }
    addr.retain(|n| !other.contains(n));
    addr
}

fn let_decl_type(ty: &Option<Type>, name: &str, addr_lets: &HashSet<String>) -> Option<Type> {
    ty.clone()
        .or_else(|| addr_lets.contains(name).then(|| Type::Simple("address".to_string())))
}

fn gen_let_binding(
    entity: &Entity,
    name: &str,
    decl_ty: Option<&Type>,
    value: &Expr,
    out: &mut String,
) -> String {
    let ty = spec_let_sol_type(entity, decl_ty, value);
    let val = if let Some(t) = decl_ty {
        gen_expr_test_typed(value, entity, t, None)
    } else {
        gen_expr_test(value, entity).unwrap_or_else(|| "0".to_string())
    };
    out.push_str(&format!("        {} {} = {};\n", ty, name, val));
    ty
}

pub(crate) fn is_address_type(ty: &Type) -> bool {
    matches!(ty, Type::Simple(s) if s == "address") || matches!(ty, Type::TypedAddress(_))
}

fn gen_address_expr_test(expr: &Expr, entity: &Entity) -> String {
    if let Expr::IntLiteral(v) = expr {
        // The slot is an address, so any literal that fits 160 bits is one
        // (a full 40-digit address is not "address-shaped" by the 64-bit rule).
        let digits = crate::ast::u256_hex_digits(v);
        if crate::ast::u256_is_address_shaped(v) || digits.as_bytes()[..24].iter().all(|c| *c == b'0') {
            return format!("address(uint160(uint256(0x{digits})))");
        }
    }
    gen_expr_test(expr, entity).unwrap_or_else(|| "address(0)".to_string())
}

/// Lower a test/fuzz expression with fuzz/property parameter types registered
/// on the emit scratch so `actual_sol_type` sees `address` params during
/// `assume holder != 0x00…00` lowering.
fn gen_expr_test_with_param_types(
    expr: &Expr,
    entity: &Entity,
    param_types: &HashMap<String, String>,
) -> String {
    super::solidity::core::scratch::with_active_scratch(|scratch| {
        for (name, ty) in param_types {
            scratch.borrow_mut().push_let_binding(name, ty);
        }
        let out = gen_expr_test(expr, entity).unwrap_or_else(|| "true".to_string());
        for name in param_types.keys() {
            scratch.borrow_mut().pop_let_binding(name);
        }
        out
    })
}

pub(crate) fn gen_expr_test_typed(
    expr: &Expr,
    entity: &Entity,
    ty: &Type,
    let_types: Option<&HashMap<String, String>>,
) -> String {
    if matches!(ty, Type::Simple(s) if s == "bytes4") {
        return match expr {
            Expr::IntLiteral(v) => {
                if v.hi == 0 && v.lo <= u64::MAX as u128 {
                    format!("bytes4(uint32({}))", v.lo)
                } else {
                    format!(
                        "bytes4(uint32(uint256(0x{})))",
                        crate::ast::u256_hex_digits(v)
                    )
                }
            }
            _ => {
                let s = gen_expr_test(expr, entity).unwrap_or_else(|| "0".to_string());
                if s.starts_with("bytes4(") {
                    s
                } else {
                    format!("bytes4(uint32({}))", s)
                }
            }
        };
    }
    if is_address_type(ty) {
        return match expr {
            Expr::IntLiteral(v) => {
                hex_literal_to_sol_for_ty(&crate::ast::u256_hex_digits(v), Some("address"))
            }
            Expr::Ident(name) => {
                let n = sol_sanitize_ident(name);
                match let_types.and_then(|m| m.get(name)) {
                    Some(t) if t != "address" => format!("address(uint160(uint256({})))", n),
                    _ => n,
                }
            }
            _ => gen_address_expr_test(expr, entity),
        };
    }
    gen_expr_test(expr, entity).unwrap_or_else(|| "0".to_string())
}

/// Wrap a word-sized Solidity expression so it lands in a `bytes32` position.
///
/// Both conversions are explicit and lossless, so this is correct whatever the
/// expression already is — including a `bytes32`, where `uint256(...)` is the
/// identity. An address-typed expression is left alone: `uint256(address)` is
/// not a legal conversion, and an address in a `bytes32` slot is a source
/// error worth surfacing as one rather than papering over.
fn coerce_to_bytes32(sol: String) -> String {
    if sol.starts_with("address(") || sol.starts_with("bytes32(") {
        return sol;
    }
    format!("bytes32(uint256({}))", sol)
}

fn solidity_call_modifier(msg_value: Option<&str>) -> String {
    msg_value
        .map(|v| format!("{{value: {}}}", v))
        .unwrap_or_default()
}

fn format_solidity_route_call(
    var_name: &str,
    route: &str,
    value_modifier: &str,
    args_joined: &str,
) -> String {
    if args_joined.is_empty() {
        format!("{}.{}{}()", var_name, route, value_modifier)
    } else {
        format!("{}.{}{}({})", var_name, route, value_modifier, args_joined)
    }
}

fn gen_call(
    entity: &Entity,
    route: &str,
    args: &[Expr],
    var_name: &str,
    needs_return: bool,
    _test_name: &str,
    _deterministic: bool,
    _factory_deploy: bool,
    ctor_hoisted: bool,
    ret_cnt: &mut RetCounter,
    let_types: Option<&HashMap<String, String>>,
    msg_value: Option<&str>,
    out: &mut String,
) {
    // G-U7: `gen_expr` already lowers address-shaped hex literals as
    // `address(uint160(0x...))`, so a bare `call setFeeTo(0x000…d03)`
    // lowers to a valid Solidity call without an extra wrapper here.
    //
    // A `bytes32` parameter is the exception. Solidity refuses an integer
    // literal there, and a test that writes `call permit(..., 1, 2)` for two
    // signature halves is writing the ordinary thing — the halves are opaque
    // words and a test rarely cares which. Coerce by the declared parameter
    // type so the source stays readable.
    let declared = entity
        .routes
        .iter()
        .find(|r| r.name == route)
        .map(|r| r.params.iter().map(|p| p.ty.clone()).collect::<Vec<_>>())
        .unwrap_or_default();
    let arg_strs: Vec<String> = match entity.routes.iter().find(|r| r.name == route) {
        Some(r) => r
            .params
            .iter()
            .zip(args.iter())
            .map(|(p, a)| gen_expr_test_typed(a, entity, &p.ty, let_types))
            .collect(),
        None => args
            .iter()
            .map(|a| gen_expr_test(a, entity).unwrap_or_else(|| "0".to_string()))
            .collect(),
    };
    let arg_strs: Vec<String> = arg_strs
        .into_iter()
        .enumerate()
        .map(|(idx, sol)| {
            match declared.get(idx) {
                Some(Type::Simple(t)) if t == "bytes32" => coerce_to_bytes32(sol),
                _ => sol,
            }
        })
        .collect();
    let args_joined = arg_strs.join(", ");
    let value_modifier = solidity_call_modifier(msg_value);

    // `call constructor(args)` redeploys via CambrianFactory (BUG-U4 U4-4).
    let effective_route: &str = if is_init_route_call(entity, route) {
        if ctor_hoisted && has_non_identity_init_params(entity) {
            return;
        }
        let mut deploy_args = identity_deploy_args_default(entity);
        if has_non_identity_init_params(entity) {
            deploy_args.extend(arg_strs.clone());
        }
        emit_factory_deploy_entity(&entity.name, var_name, &deploy_args, out);
        return;
    } else {
        route
    };

    let route_obj = entity.routes.iter().find(|r| r.name == route);
    let ret_type = route_obj.and_then(|r| r.return_type.as_ref());
    let has_return = ret_type
        .map(|t| !matches!(t, Type::Simple(ref s) if s == "()"))
        .unwrap_or(false);

    if needs_return && has_return {
        let ret_type = ret_type.unwrap();
        // Use a fresh `_ret_<n>` so consecutive calls in the same test
        // body don't redeclare the same Solidity local. The caller
        // (`gen_expect_return`) reads back via the same counter.
        let n = ret_cnt.bump();
        // `in_param: true` is what appends the data location. A Solidity
        // local holding a reference type needs one exactly as a parameter
        // does — `string _ret_1 = t.name();` does not compile — and the flag
        // is the only thing that distinguishes `string` from
        // `string memory`. Passing `false` here produced a test file that
        // failed to compile for every entity carrying a `String` view, which
        // is why `ERC20.test.cam` never asserts on `name()`.
        match ret_type {
            Type::Tuple(items) => {
                let type_strs: Vec<String> = items.iter().enumerate()
                    .map(|(i, t)| {
                        format!(
                            "{} _ret_{}_{}",
                            sol_type_entity(entity, t, true, &super::solidity::active_ctx()),
                            n,
                            i
                        )
                    })
                    .collect();
                let call = format_solidity_route_call(
                    var_name,
                    effective_route,
                    &value_modifier,
                    &args_joined,
                );
                out.push_str(&format!(
                    "        ({}) = {};\n",
                    type_strs.join(", "),
                    call,
                ));
            }
            _ => {
                let ret_sol =
                    sol_type_entity(entity, ret_type, true, &super::solidity::active_ctx());
                let call = format_solidity_route_call(
                    var_name,
                    effective_route,
                    &value_modifier,
                    &args_joined,
                );
                out.push_str(&format!("        {} _ret_{} = {};\n", ret_sol, n, call));
            }
        }
    } else {
        let call = format_solidity_route_call(
            var_name,
            effective_route,
            &value_modifier,
            &args_joined,
        );
        out.push_str(&format!("        {};\n", call));
    }
}

/// `return.0` on a tuple-returning route is the component local `_ret_N_0`.
fn rewrite_result_tuple_slots(expr: &mut Expr, ret_n: u32) {
    map_expr(expr, &mut |node| {
        let slot = match node {
            Expr::FieldAccess(inner, field)
                if matches!(inner.as_ref(), Expr::Ident(name) if name == EXPECT_RESULT_NAME)
                    && field.parse::<usize>().is_ok() =>
            {
                Some(format!("_ret_{ret_n}_{field}"))
            }
            _ => None,
        };
        if let Some(name) = slot {
            *node = Expr::Ident(name);
        }
    });
}

fn gen_expect_pred(
    program: &Program,
    entity: &Entity,
    callee: &Entity,
    route: &str,
    cond: &Expr,
    var_name: &str,
    test_name: &str,
    ret_n: u32,
    out: &mut String,
) {
    let mut cond = cond.clone();
    if expr_mentions_ident(&cond, EXPECT_RESULT_NAME) {
        let tuple_return = callee
            .routes
            .iter()
            .find(|r| r.name == route)
            .and_then(|r| r.return_type.as_ref())
            .is_some_and(|t| matches!(t, Type::Tuple(_)));
        if tuple_return {
            // Tuple captures are `_ret_N_0`, `_ret_N_1`, …, matching `gen_call`.
            rewrite_result_tuple_slots(&mut cond, ret_n);
        }
        rename_ident(&mut cond, EXPECT_RESULT_NAME, &format!("_ret_{ret_n}"));
    }
    let ctx = active_ctx();
    let sol = lower_invariant_check_inline(program, entity, &cond, &ctx, var_name, &[]);
    out.push_str(&format!(
        "        assertTrue({sol}, \"test '{test_name}': expect predicate failed\");\n"
    ));
}

fn gen_expect_return(
    entity: &Entity,
    pending: &PendingReturn,
    route_name: &str,
    program: &Program,
    test_name: &str,
    ret_n: u32,
    let_types: Option<&HashMap<String, String>>,
    out: &mut String,
) {
    let route = entity.routes.iter().find(|route| route.name == route_name);
    let ret_ty = route.and_then(|r| r.return_type.as_ref());

    let n = ret_n;
    match pending {
        PendingReturn::Scalar(value) => {
            // Bare lets / literals: coerce to the route return type (TYPED-LIT).
            // Complex oracles still use the route IR path.
            let expected = if ret_ty.is_some()
                && matches!(value, Expr::Ident(_) | Expr::IntLiteral(_))
            {
                gen_expr_test_typed(value, entity, ret_ty.unwrap(), let_types)
            } else {
                route
                    .and_then(|route| gen_expr_test_ir(value, program, entity, route))
                    .or_else(|| {
                        ret_ty.map(|ty| gen_expr_test_typed(value, entity, ty, let_types))
                    })
                    .or_else(|| gen_expr_test(value, entity))
                    .unwrap_or_else(|| "0".to_string())
            };
            // forge-std `assertEq` has no enum overload, so when the
            // expected value is `EnumName.Variant` we cast both sides
            // to uint256 to land on the integer overload.
            let (lhs, rhs) = if matches!(value, Expr::EnumVariant(_, _)) {
                (
                    format!("uint256(_ret_{})", n),
                    format!("uint256({})", expected),
                )
            } else {
                (format!("_ret_{}", n), expected)
            };
            out.push_str(&format!(
                "        assertEq({}, {}, \"test '{}': return value mismatch\");\n",
                lhs, rhs, test_name
            ));
        }
        PendingReturn::Tuple(values) => {
            let elem_tys: Vec<Option<&Type>> = match ret_ty {
                Some(Type::Tuple(items)) => items.iter().map(Some).collect(),
                _ => vec![ret_ty],
            };
            for (i, value) in values.iter().enumerate() {
                let expected = elem_tys
                    .get(i)
                    .and_then(|ty| ty.map(|ty| gen_expr_test_typed(value, entity, ty, let_types)))
                    .or_else(|| gen_expr_test(value, entity))
                    .unwrap_or_else(|| "0".to_string());
                let (lhs, rhs) = if matches!(value, Expr::EnumVariant(_, _)) {
                    (
                        format!("uint256(_ret_{}_{})", n, i),
                        format!("uint256({})", expected),
                    )
                } else {
                    (format!("_ret_{}_{}", n, i), expected)
                };
                out.push_str(&format!(
                    "        assertEq({}, {}, \"test '{}': return[{}] mismatch\");\n",
                    lhs, rhs, test_name, i
                ));
            }
        }
        PendingReturn::Lens(path, value) => {
            // A tuple return is already destructured into `_ret_N_i` locals
            // by `gen_call`; the lens head picks one of them.
            let (local, rest, base_ty) = match (path.first(), ret_ty) {
                (Some(PathSegment::TupleIndex(i)), Some(Type::Tuple(items))) => {
                    (format!("_ret_{}_{}", n, i), &path[1..], items.get(*i))
                }
                _ => (format!("_ret_{}", n), &path[..], ret_ty),
            };
            let lens_ty = if rest.is_empty() { base_ty } else { None };
            let expected = lens_ty
                .map(|ty| gen_expr_test_typed(value, entity, ty, let_types))
                .or_else(|| gen_expr_test(value, entity))
                .unwrap_or_else(|| "0".to_string());
            let accessor = gen_path_accessor_typed(&local, base_ty, rest, entity);
            let (lhs, rhs) = if matches!(value, Expr::EnumVariant(_, _)) {
                (format!("uint256({})", accessor), format!("uint256({})", expected))
            } else {
                (accessor, expected)
            };
            out.push_str(&format!(
                "        assertEq({}, {}, \"test '{}': return lens mismatch\");\n",
                lhs, rhs, test_name
            ));
        }
    }
}

fn gen_expect_emit(
    entity: &Entity,
    program: &Program,
    event_name: &str,
    args: &[Expr],
    let_types: Option<&HashMap<String, String>>,
    out: &mut String,
) {
    // Entity events live inside the contract (`T.Moved`); program-scope
    // events are file-level declarations and are emitted unqualified.
    let (qualifier, decl) = match (
        entity.events.iter().find(|e| e.name == event_name),
        program.events.iter().find(|e| e.name == event_name),
    ) {
        (Some(d), _) => (format!("{}.", entity.name), Some(d)),
        (None, Some(d)) => (String::new(), Some(d)),
        (None, None) => (format!("{}.", entity.name), None),
    };
    let rendered: Vec<String> = args
        .iter()
        .enumerate()
        .map(|(i, a)| match decl.and_then(|d| d.params.get(i)) {
            Some(p) => gen_expr_test_typed(a, entity, &p.ty, let_types),
            None => match let_types {
                Some(types) => gen_expr_test_with_param_types(a, entity, types),
                None => gen_expr_test(a, entity).unwrap_or_else(|| "0".to_string()),
            },
        })
        .collect();
    out.push_str("        vm.expectEmit(true, true, true, true);\n");
    out.push_str(&format!(
        "        emit {}{}({});\n",
        qualifier,
        event_name,
        rendered.join(", ")
    ));
}

fn state_record<'a>(entity: &'a Entity, ctx: &'a EvmCtx, ty: &Type) -> Option<&'a Record> {
    let Type::Simple(name) = ty else { return None };
    entity
        .records
        .iter()
        .find(|r| &r.name == name)
        .or_else(|| ctx.lookup_record(name))
}

fn state_is_plain_enum(entity: &Entity, ctx: &EvmCtx, ty: &Type) -> bool {
    let Type::Simple(name) = ty else { return false };
    match entity.enums.iter().find(|e| &e.name == name) {
        Some(d) => !super::solidity::core::state::is_payload_enum(d),
        None => ctx.lookup_enum(name).is_some() && !ctx.is_payload_enum_named(name),
    }
}

fn option_inner(ty: &Type) -> Option<&Type> {
    match ty {
        Type::Generic(n, ps) if n == "Option" && ps.len() == 1 => Some(&ps[0]),
        _ => None,
    }
}

fn map_kv(ty: &Type) -> Option<(&Type, &Type)> {
    match ty {
        Type::Generic(n, ps) if n == "HashMap" && ps.len() == 2 => Some((&ps[0], &ps[1])),
        _ => None,
    }
}

/// Whole-map literal keys are stored as source text: a decimal integer or
/// an identifier.
fn map_literal_key(key: &str) -> Expr {
    match cambrian_core::U256::from_decimal_str(key) {
        Ok(v) => Expr::IntLiteral(v),
        Err(_) => Expr::Ident(key.to_string()),
    }
}

/// One Solidity read of a member position: either a public-getter call
/// (whose struct results come back as a tuple) or a plain value.
enum StateRead {
    Getter(String),
    Value(String),
}

struct StateCmp<'a> {
    entity: &'a Entity,
    ctx: EvmCtx,
    test_name: &'a str,
    member: &'a str,
    let_types: Option<&'a HashMap<String, String>>,
}

impl StateCmp<'_> {
    fn fresh(&self, out: &str) -> String {
        format!("_st_{}", out.matches(" _st_").count())
    }

    fn sol_ty(&self, ty: &Type) -> String {
        sol_type_entity(self.entity, ty, true, &self.ctx)
    }

    fn assert_eq(&self, lhs: &str, rhs: &str, out: &mut String) {
        out.push_str(&format!(
            "        assertEq({}, {}, \"test '{}': {} mismatch\");\n",
            lhs, rhs, self.test_name, self.member
        ));
    }

    /// Bind field `want` of a struct getter to a local. The getter returns
    /// one tuple slot per field except array and mapping fields.
    fn destructure(&self, call: &str, rec: &Record, want: &str, out: &mut String) -> String {
        let local = format!("{}_{}", self.fresh(out), want);
        let slots: Vec<String> = rec
            .fields
            .iter()
            .filter(|f| !matches!(&f.ty, Type::Generic(n, _) if n == "Vec" || n == "HashMap"))
            .map(|f| {
                if f.name == want {
                    format!("{} {}", self.sol_ty(&f.ty), local)
                } else {
                    String::new()
                }
            })
            .collect();
        out.push_str(&format!("        ({}) = {};\n", slots.join(", "), call));
        local
    }

    fn emit(&self, read: StateRead, ty: &Type, rest: &[PathSegment], expected: &Expr, out: &mut String) {
        if let Some((key_ty, val_ty)) = map_kv(ty) {
            let StateRead::Getter(call) = &read else {
                self.assert_eq(&read_text(&read), &gen_expr_test(expected, self.entity).unwrap_or_default(), out);
                return;
            };
            let with_key = |k: &Expr| {
                let key = gen_expr_test_typed(k, self.entity, key_ty, self.let_types);
                if call.ends_with("()") {
                    format!("{}{})", &call[..call.len() - 1], key)
                } else {
                    format!("{}, {})", &call[..call.len() - 1], key)
                }
            };
            match (rest.first(), expected) {
                (Some(PathSegment::Index(k)), _) => {
                    self.emit(StateRead::Getter(with_key(k)), val_ty, &rest[1..], expected, out)
                }
                (None, Expr::RecordConstruct(n, pairs)) if n == "__HashMap" => {
                    for (k, v) in pairs {
                        self.emit(StateRead::Getter(with_key(&map_literal_key(k))), val_ty, &[], v, out);
                    }
                }
                _ => self.assert_eq(call, &gen_expr_test(expected, self.entity).unwrap_or_default(), out),
            }
            return;
        }
        if let Some(rec) = state_record(self.entity, &self.ctx, ty) {
            if let Some(PathSegment::Field(f)) = rest.first() {
                let fty = rec.fields.iter().find(|x| &x.name == f).map(|x| x.ty.clone());
                if let Some(fty) = fty {
                    let value = match read {
                        StateRead::Getter(call) => self.destructure(&call, rec, f, out),
                        StateRead::Value(v) => format!("{}.{}", v, f),
                    };
                    self.emit(StateRead::Value(value), &fty, &rest[1..], expected, out);
                    return;
                }
            }
        }
        if !rest.is_empty() {
            let base = read_text(&read);
            let accessor = gen_path_accessor(&base, rest, self.entity);
            let rhs = gen_expr_test(expected, self.entity).unwrap_or_else(|| "0".to_string());
            self.assert_eq(&accessor, &rhs, out);
            return;
        }
        if let Some(inner) = option_inner(ty) {
            let (tag, payload) = match read {
                StateRead::Getter(call) => {
                    let base = self.fresh(out);
                    let some_slot = if matches!(expected, Expr::None) {
                        String::new()
                    } else {
                        format!("{} {base}_some", self.sol_ty(inner))
                    };
                    out.push_str(&format!(
                        "        ({} {base}_tag, {some_slot}) = {};\n",
                        super::solidity::core::option::option_tag_name(inner),
                        call
                    ));
                    (format!("{base}_tag"), format!("{base}_some"))
                }
                StateRead::Value(v) => (format!("{v}.tag"), format!("{v}.some_0")),
            };
            match expected {
                Expr::None => self.assert_eq(&format!("uint256({tag})"), "0", out),
                Expr::Some(v) => {
                    self.assert_eq(&format!("uint256({tag})"), "1", out);
                    self.emit(StateRead::Value(payload), inner, &[], v, out);
                }
                other => {
                    let rhs = gen_expr_test(other, self.entity).unwrap_or_else(|| "0".to_string());
                    self.assert_eq(&payload, &rhs, out);
                }
            }
            return;
        }
        let actual = read_text(&read);
        let expected_sol = gen_expr_test_typed(expected, self.entity, ty, self.let_types);
        if state_is_plain_enum(self.entity, &self.ctx, ty) || matches!(expected, Expr::EnumVariant(_, _)) {
            self.assert_eq(&format!("uint256({actual})"), &format!("uint256({expected_sol})"), out);
        } else {
            self.assert_eq(&actual, &expected_sol, out);
        }
    }
}

fn read_text(read: &StateRead) -> String {
    match read {
        StateRead::Getter(s) | StateRead::Value(s) => s.clone(),
    }
}

/// Lower `expect state { path: v, ... }` against the contract's public
/// getters: map keys become getter arguments, struct getters (records,
/// `Option`) are destructured, and expected values are typed from the
/// member's declared type.
fn gen_expect_state(
    entity: &Entity,
    fields: &[(FieldPath, Expr)],
    var_name: &str,
    test_name: &str,
    let_types: Option<&HashMap<String, String>>,
    out: &mut String,
) {
    for (path, expected_value) in fields {
        let Some(PathSegment::Field(field_name)) = path.first() else {
            continue;
        };
        let cmp = StateCmp {
            entity,
            ctx: active_ctx(),
            test_name,
            member: field_name,
            let_types,
        };
        match entity.members.iter().find(|m| &m.name == field_name) {
            Some(member) => cmp.emit(
                StateRead::Getter(format!("{}.{}()", var_name, field_name)),
                &member.ty,
                &path[1..],
                expected_value,
                out,
            ),
            None => {
                let expected =
                    gen_expr_test(expected_value, entity).unwrap_or_else(|| "0".to_string());
                let accessor = gen_path_accessor(&format!("{}.{}()", var_name, field_name), &path[1..], entity);
                cmp.assert_eq(&accessor, &expected, out);
            }
        }
    }
}

fn gen_post_call_effect_checks(elements: &[TestEffectElement], entity: &Entity, out: &mut String) {
    for elem in elements {
        if let TestEffectElement::Effect(TestEffect::Send { dest, .. }) = elem {
            let dest_expr = gen_expr_test(dest, entity).unwrap_or_else(|| "address(0)".to_string());
            let sanitized = sanitize_var_name(&dest_expr);
            out.push_str(&format!(
                "        assertTrue({}.balance >= _bal_before_{}, \"effect: balance should increase for {}\");\n",
                dest_expr, sanitized, dest_expr
            ));
        }
    }
}

fn gen_pre_call_effect_setup(
    elements: &[TestEffectElement],
    entity: &Entity,
    _var_name: &str,
    out: &mut String,
) {
    for elem in elements {
        match elem {
            TestEffectElement::Effect(TestEffect::Send { dest, .. }) => {
                let dest_expr =
                    gen_expr_test(dest, entity).unwrap_or_else(|| "address(0)".to_string());
                out.push_str(&format!(
                    "        uint256 _bal_before_{} = {}.balance;\n",
                    sanitize_var_name(&dest_expr),
                    dest_expr
                ));
            }
            TestEffectElement::Effect(TestEffect::Deploy { entity, .. }) => {
                out.push_str(&format!(
                    "        // expect deploy {} -- tracked via event/address\n",
                    entity
                ));
            }
            TestEffectElement::Effect(TestEffect::PlatformEffect { name, .. }) => {
                out.push_str(&format!(
                    "        // expect {} -- TVM-specific, skipped on EVM\n",
                    name
                ));
            }
            TestEffectElement::Wildcard => {}
        }
    }
}

/// `gen_path_accessor` with the base type known: `.len` on a `Vec` is the
/// array's `.length`.
fn gen_path_accessor_typed(
    base: &str,
    ty: Option<&Type>,
    path: &[PathSegment],
    entity: &Entity,
) -> String {
    let mut acc = base.to_string();
    let mut cur = ty.cloned();
    for (i, seg) in path.iter().enumerate() {
        match (seg, &cur) {
            (PathSegment::Field(f), Some(Type::Generic(g, _))) if f == "len" && g == "Vec" => {
                acc = format!("{}.length", acc);
                cur = None;
            }
            (PathSegment::Index(_), Some(Type::Generic(g, ps))) if g == "Vec" && ps.len() == 1 => {
                acc = gen_path_accessor(&acc, &path[i..=i], entity);
                cur = Some(ps[0].clone());
            }
            _ => return gen_path_accessor(&acc, &path[i..], entity),
        }
    }
    acc
}

fn gen_path_accessor(base: &str, path: &[PathSegment], entity: &Entity) -> String {
    let mut result = base.to_string();
    for seg in path {
        match seg {
            PathSegment::Field(name) => {
                result = format!("{}.{}", result, name);
            }
            PathSegment::Index(expr) => {
                let idx = gen_expr_test(expr, entity).unwrap_or_else(|| "0".to_string());
                result = format!("{}[{}]", result, idx);
            }
            PathSegment::TupleIndex(i) => {
                result = format!("{}.{}", result, i);
            }
        }
    }
    result
}

/// Handler-storage identifiers for `trace::*` accessors (Foundry checks call
/// them via `_handler._traceLen()` etc.).
fn trace_handler_state_idents(
    trace_feat: &crate::codegen::test_backend::TraceFeatures,
    action_routes: &[String],
) -> Vec<String> {
    let mut idents = Vec::new();
    if trace_feat.uses_length {
        idents.push("_traceLen".to_string());
    }
    for r in &trace_feat.counted {
        idents.push(format!("_traceCount_{}", r));
    }
    if trace_feat.uses_last {
        for r in action_routes {
            idents.push(format!("_traceLast_{}", r));
        }
    }
    idents
}

/// Emit the per-call trace-counter updates for a Foundry handler action.
/// `route` is `Some(name)` for a real action wrapper, `None` for the
/// synthetic `advanceTime` action (which advances `_traceLen` and clears
/// every `lastWas` flag, since `advanceTime` is not a declared route).
fn emit_trace_counter_bump(
    trace_feat: &crate::codegen::test_backend::TraceFeatures,
    action_routes: &[String],
    route: Option<&str>,
    out: &mut String,
) {
    if trace_feat.uses_length {
        out.push_str("        _traceLen++;\n");
    }
    if let Some(route) = route {
        if trace_feat.counted.iter().any(|r| r == route) {
            out.push_str(&format!("        _traceCount_{}++;\n", route));
        }
    }
    if trace_feat.uses_last {
        for r in action_routes {
            let val = if route == Some(r.as_str()) {
                "true"
            } else {
                "false"
            };
            out.push_str(&format!("        _traceLast_{} = {};\n", r, val));
        }
    }
}

fn gen_invariant_file(
    entity: &Entity,
    inv: &InvariantDecl,
    _inv_cfg: &crate::project::InvariantConfig,
    deterministic: bool,
    program: &Program,
    project_name: Option<&str>,
) -> String {
    if project_name.is_none() {
        return format!(
            "// SPDX-License-Identifier: UNLICENSED\npragma solidity ^0.8.24;\n// invariant '{}' skipped: harness requires project_name (CambrianFactory deploy)\n",
            inv.name
        );
    }
    assert!(
        deterministic,
        "invariant factory harness requires deterministic_addresses"
    );
    let inv_name = sanitize_test_name(&inv.name);
    let handler_name = format!("{}Handler_{}", entity.name, inv_name);
    let test_name = format!("Invariant_{}_{}_Test", entity.name, inv_name);
    let var_name = entity_var_name(&entity.name);

    let mut out = String::new();
    out.push_str("// SPDX-License-Identifier: UNLICENSED\n");
    out.push_str("pragma solidity ^0.8.24;\n\n");
    out.push_str("import \"forge-std/Test.sol\";\n");
    out.push_str("import \"forge-std/StdInvariant.sol\";\n");
    let init_state = inv.init_state();
    let siblings: &[(&str, &str)] = &[];
    emit_deterministic_project_import(project_name.unwrap(), &mut out);
    out.push_str(&format!("import \"../src/{}.sol\";\n\n", entity.name));

    // Names exposed on the handler itself (track bindings + derived
    // queries). When `substitute_member_accessors` later rewrites
    // bare identifiers in `check` clauses, these names must remain bare
    // (they live on the handler, not on the SUT).
    let mut handler_state_idents: Vec<String> = inv
        .track
        .iter()
        .map(|s| s.name.clone())
        .chain(inv.derived.iter().map(|q| q.name.clone()))
        .collect();

    // Trace-aware accessors (`trace::length` / `count` / `lastWas`) —
    // backed by handler-storage counters maintained per action call.
    let trace_feat = crate::codegen::test_backend::scan_invariant_trace_features(inv);
    let action_routes: Vec<String> = {
        let mut v: Vec<String> = Vec::new();
        for a in &inv.actions {
            if !v.contains(&a.route) {
                v.push(a.route.clone());
            }
        }
        v
    };
    handler_state_idents.extend(trace_handler_state_idents(&trace_feat, &action_routes));

    // Handler contract — wraps each action and applies bound/assume.
    out.push_str(&format!("contract {} is Test {{\n", handler_name));
    out.push_str(&format!("    {} public {};\n", entity.name, var_name));
    gen_entity_test_constants(entity, &mut out);

    // `track { let name = expr; ... }` -- handler-state snapshot fields.
    for binding in &inv.track {
        // Snapshot bindings reference SUT state; use uint256 as the
        // safe default until we have an explicit type-inference hook.
        out.push_str(&format!("    uint256 public {};\n", binding.name));
    }

    // Trace-state counters / flags (only the referenced ones).
    if trace_feat.uses_length {
        out.push_str("    uint256 public _traceLen;\n");
    }
    for r in &trace_feat.counted {
        out.push_str(&format!("    uint256 public _traceCount_{};\n", r));
    }
    if trace_feat.uses_last {
        for r in &action_routes {
            out.push_str(&format!("    bool public _traceLast_{};\n", r));
        }
    }

    emit_handler_cam_wire_single(entity, &var_name, inv, &handler_state_idents, &mut out);

    // `derived` view queries -- pure helpers callable from `check`.
    for q in &inv.derived {
        let q_params: Vec<String> = q
            .params
            .iter()
            .map(|p| {
                format!(
                    "{} {}",
                    sol_type_entity(entity, &p.ty, true, &super::solidity::active_ctx()),
                    p.name
                )
            })
            .collect();
        let ret_ty = sol_type_entity(
            entity,
            &q.return_type,
            false,
            &super::solidity::active_ctx(),
        );
        out.push_str(&format!(
            "\n    function {}({}) public view returns ({}) {{\n",
            q.name,
            q_params.join(", "),
            ret_ty,
        ));
        for step in &q.body {
            if let TestStep::Let { name, ty, value } = step {
                let v = if let Some(t) = ty {
                    gen_expr_test_typed(value, entity, t, None)
                } else {
                    gen_expr_test(value, entity).unwrap_or_else(|| "0".to_string())
                };
                let v_subst =
                    substitute_member_accessors(&v, entity, &var_name, &handler_state_idents);
                out.push_str(&format!("        {} {} = {};\n", ret_ty, name, v_subst));
            }
        }
        let retval = gen_expr_test(&q.return_value, entity).unwrap_or_else(|| "0".to_string());
        let retval_subst =
            substitute_member_accessors(&retval, entity, &var_name, &handler_state_idents);
        out.push_str(&format!("        return {};\n", retval_subst));
        out.push_str("    }\n");
    }

    for action in &inv.actions {
        let route = entity.routes.iter().find(|r| r.name == action.route);
        let params: Vec<String> = action
            .params
            .iter()
            .map(|p| {
                format!(
                    "{} {}",
                    sol_type_entity(entity, &p.ty, true, &super::solidity::active_ctx()),
                    p.name
                )
            })
            .collect();
        let handler_fn = invariant_handler_action_fn(entity, &action.route);
        out.push_str(&format!(
            "\n    function {}({}) public {{\n",
            handler_fn,
            params.join(", "),
        ));

        // Apply per-action body steps. Computed bounds (`bound x in
        // 0..pair.totalAssets()`) thread through `substitute_member_accessors`
        // so a bound expression can reference SUT members or handler
        // snapshot bindings.
        for step in &action.body {
            match step {
                TestStep::Bound {
                    var,
                    lo,
                    hi,
                    inclusive,
                } => {
                    let lo_raw = gen_expr_test(lo, entity).unwrap_or_else(|| "0".to_string());
                    let hi_raw = gen_expr_test(hi, entity).unwrap_or_else(|| "0".to_string());
                    let lo_s = substitute_member_accessors(
                        &lo_raw,
                        entity,
                        &var_name,
                        &handler_state_idents,
                    );
                    let hi_s_in = substitute_member_accessors(
                        &hi_raw,
                        entity,
                        &var_name,
                        &handler_state_idents,
                    );
                    let hi_s = if *inclusive {
                        hi_s_in
                    } else {
                        format!("({}) - 1", hi_s_in)
                    };
                    let ty = bound_param_sol_ty(entity, &action.params, var);
                    out.push_str(&emit_bound_step(var, &ty, &lo_s, &hi_s));
                }
                TestStep::Assume { cond } => {
                    let c = gen_expr_test(cond, entity).unwrap_or_else(|| "true".to_string());
                    let c_subst =
                        substitute_member_accessors(&c, entity, &var_name, &handler_state_idents);
                    out.push_str(&format!("        vm.assume({});\n", c_subst));
                }
                TestStep::SkipIf { cond } => {
                    let c = gen_expr_test(cond, entity).unwrap_or_else(|| "false".to_string());
                    let c_subst =
                        substitute_member_accessors(&c, entity, &var_name, &handler_state_idents);
                    out.push_str(&format!("        if ({}) return;\n", c_subst));
                }
                _ => {}
            }
        }

        // Forward call. `fail_on_revert == false` wraps the call in
        // `try/catch` so reverts terminate this action without taking
        // down the run (the same behaviour the runner config asks for,
        // but enforced at the call site for clarity).
        let arg_names: Vec<String> = action.params.iter().map(|p| p.name.clone()).collect();
        let _ = route;
        // Forward the rotated sender into the entity. `targetSender`
        // only chooses who calls the *handler*; without a prank the
        // entity sees the handler's own address as `msg.sender`, so
        // `senders { … }` names a set the contract never observes. Any
        // guard of the shape `msg::sender == m_owner` then rejects every
        // call, every action reverts into the `try/catch` above, and the
        // invariant holds over a run in which nothing happened.
        //
        // Emitted only when the invariant declares a sender set: that is
        // the case where the author stated an intent the lowering has to
        // honour. With no `senders { … }` Foundry picks arbitrary
        // addresses and the handler's own identity is as good as any.
        if !inv.senders.is_empty() {
            out.push_str("        vm.prank(msg.sender);\n");
        }
        if is_init_route_call(entity, &action.route) {
            out.push_str("        // init route: entity already constructed in setUp\n");
        } else if inv.fail_on_revert {
            out.push_str(&format!(
                "        {}.{}({});\n",
                var_name,
                action.route,
                arg_names.join(", "),
            ));
        } else {
            out.push_str(&format!(
                "        try {}.{}({}) {{}} catch {{ return; }}\n",
                var_name,
                action.route,
                arg_names.join(", "),
            ));
        }
        emit_trace_counter_bump(&trace_feat, &action_routes, Some(&action.route), &mut out);
        out.push_str("    }\n");
    }

    // `with_time` -- synthetic `advanceTime(uint256 secs)` action.
    if inv.with_time {
        out.push_str("\n    function advanceTime(uint256 secs) public {\n");
        out.push_str("        vm.warp(block.timestamp + secs);\n");
        out.push_str("        vm.roll(block.number + 1);\n");
        emit_trace_counter_bump(&trace_feat, &action_routes, None, &mut out);
        out.push_str("    }\n");
    }

    out.push_str("}\n\n");

    // Test contract.
    out.push_str(&format!(
        "contract {} is StdInvariant, Test {{\n",
        test_name
    ));
    out.push_str(&format!("    {} internal {};\n", entity.name, var_name));
    out.push_str(&format!("    {} internal _handler;\n", handler_name));
    emit_cambrian_factory_field(&mut out);
    gen_entity_test_constants(entity, &mut out);
    out.push_str("    function setUp() public {\n");
    // U4-4c: handler-first — `address(_handler)` is known before factory.deploy*.
    emit_cambrian_factory_new(&mut out);
    out.push_str(&format!("        _handler = new {}();\n", handler_name));
    let deploy_args = invariant_factory_deploy_args(entity, init_state, inv, siblings, None);
    emit_factory_deploy_entity(&entity.name, &var_name, &deploy_args, &mut out);

    let leftover = leftover_init_state(entity, init_state);
    if !leftover.is_empty() {
        gen_state_init(entity, &leftover, &var_name, program, &mut out);
    }

    // Forall-ized starting state (`init { m_count: * }`): seed each
    // forall field with a fuzzer-random value so the invariant is checked
    // from an arbitrary start, mirroring the Lean `∀`.
    let forall_fields = forall_dynamic_fields(entity, &inv.instances[0]);
    gen_forall_state_init(entity, &forall_fields, &var_name, "", program, &mut out);

    out.push_str(&format!("        _handler.cam_wire({});\n", var_name));

    out.push_str("        targetContract(address(_handler));\n");

    // Restrict targetSelector to the action wrappers (plus the
    // synthetic `advanceTime(uint256)` when the invariant is declared
    // `#[with_time]`).
    if !inv.actions.is_empty() || inv.with_time {
        let total = inv.actions.len() + if inv.with_time { 1 } else { 0 };
        out.push_str(&format!(
            "        bytes4[] memory selectors = new bytes4[]({});\n",
            total,
        ));
        for (i, action) in inv.actions.iter().enumerate() {
            let sig_params: Vec<String> = action
                .params
                .iter()
                .map(|p| sol_type_entity(entity, &p.ty, false, &super::solidity::active_ctx()))
                .collect();
            let handler_fn = invariant_handler_action_fn(entity, &action.route);
            out.push_str(&format!(
                "        selectors[{}] = bytes4(keccak256(\"{}({})\"));\n",
                i,
                handler_fn,
                sig_params.join(","),
            ));
        }
        if inv.with_time {
            out.push_str(&format!(
                "        selectors[{}] = bytes4(keccak256(\"advanceTime(uint256)\"));\n",
                inv.actions.len(),
            ));
        }
        out.push_str("        targetSelector(FuzzSelector({addr: address(_handler), selectors: selectors}));\n");
    }

    // Optional sender pool. G-U7: `gen_expr` already lowers
    // address-shaped hex literals as `address(uint160(...))`, so the
    // raw expression is a valid `targetSender(...)` argument.
    if !inv.senders.is_empty() {
        for s in &inv.senders {
            let s_str = gen_address_expr_test(s, entity);
            out.push_str(&format!("        targetSender({});\n", s_str));
        }
    }

    // `ctx { ... }` -- blockchain/context params (warp / targetSender).
    gen_invariant_foundry_ctx(
        &inv.context,
        entity,
        super::invariant_harness::has_explicit_deploy(inv),
        &mut out,
    );

    // `exclude senders { ... }` -> excludeSender(...).
    for s in &inv.exclude_senders {
        let s_str = gen_address_expr_test(s, entity);
        out.push_str(&format!("        excludeSender({});\n", s_str));
    }

    // `exclude selectors { name1, name2 }` -> excludeSelector via a
    // FuzzSelector targeting the handler. We emit one
    // FuzzSelector with all the bytes4 signatures of the named action
    // wrappers.
    if !inv.exclude_selectors.is_empty() {
        out.push_str(&format!(
            "        bytes4[] memory excluded = new bytes4[]({});\n",
            inv.exclude_selectors.len(),
        ));
        for (i, name) in inv.exclude_selectors.iter().enumerate() {
            // Resolve to the action's parameter signature (or the
            // builtin `advanceTime(uint256)` when `with_time` is on).
            let sig_params = if let Some(action) = inv.actions.iter().find(|a| &a.route == name) {
                action
                    .params
                    .iter()
                    .map(|p| sol_type_entity(entity, &p.ty, false, &super::solidity::active_ctx()))
                    .collect::<Vec<_>>()
                    .join(",")
            } else if name == "advanceTime" {
                "uint256".to_string()
            } else {
                String::new()
            };
            let handler_fn = if let Some(action) = inv.actions.iter().find(|a| &a.route == name) {
                invariant_handler_action_fn(entity, &action.route)
            } else {
                name.clone()
            };
            out.push_str(&format!(
                "        excluded[{}] = bytes4(keccak256(\"{}({})\"));\n",
                i, handler_fn, sig_params,
            ));
        }
        out.push_str("        excludeSelector(FuzzSelector({addr: address(_handler), selectors: excluded}));\n");
    }

    out.push_str("    }\n");

    let tag_suffix = inv
        .tag
        .as_deref()
        .map(|t| format!("_{}", sanitize_tag_for_fn(t)))
        .unwrap_or_default();
    let revert_prefix = inv
        .tag
        .as_deref()
        .map(|t| format!("[{}] ", t))
        .unwrap_or_default();

    // One invariant_<name>_<n>() function per check clause.
    let evm_ctx = EvmCtx::build(program, deterministic, true);
    for (idx, check) in inv.checks.iter().enumerate() {
        let suffix = if inv.checks.len() == 1 {
            String::new()
        } else {
            format!("_{}", idx)
        };
        let cond_s = crate::codegen::predicate_expr::lower_invariant_check_require_expr(
            program,
            entity,
            inv,
            idx,
            check,
            &var_name,
            &evm_ctx,
            &handler_state_idents,
        );
        if let Some(tag) = &inv.tag {
            out.push_str(&format!("\n    /// @dev tag: {}", tag));
        } else {
            out.push('\n');
        }
        if let Some(n) = inv.runs {
            out.push_str(&format!(
                "\n    /// forge-config: default.invariant.runs = {}",
                n
            ));
        }
        if let Some(n) = inv.depth {
            out.push_str(&format!(
                "\n    /// forge-config: default.invariant.depth = {}",
                n
            ));
        }
        out.push_str(&format!(
            "\n    function invariant_{}{}{}() public view {{\n",
            inv_name, suffix, tag_suffix,
        ));
        // Rewrite track-binding / derived-query references as
        // `_handler.<name>()` first, then rewrite remaining SUT members
        // as `<var>.<name>()`.
        let cond_handler = rewrite_handler_idents(&cond_s, &handler_state_idents);
        let cond_subst = match crate::codegen::predicate_expr::classify_invariant_check(
            program, inv, idx, &evm_ctx,
        ) {
            crate::codegen::predicate_expr::CheckKind::Helper { .. } => cond_s,
            crate::codegen::predicate_expr::CheckKind::Inline => cond_handler,
            _ => substitute_member_accessors(
                &cond_handler,
                entity,
                &var_name,
                &handler_state_idents,
            ),
        };
        out.push_str(&format!(
            "        require({}, \"{}invariant '{}' violated\");\n",
            cond_subst, revert_prefix, inv.name,
        ));
        out.push_str("    }\n");
    }

    if inv.fail_on_revert {
        out.push_str("\n    // #[fail_on_revert]: Foundry treats handler reverts as failures via foundry.toml\n");
    } else {
        out.push_str("\n    // default: handler reverts are silently skipped\n");
    }

    out.push_str("}\n");
    out
}

/// Generate a Solidity test contract for a multi-instance (`for system`)
/// invariant. The Handler stores N entity handles (one per instance), and
/// `setUp()` deploys each instance + applies its per-instance `init { ... }`.
fn gen_invariant_multi_file(
    program: &Program,
    inv: &InvariantDecl,
    _inv_cfg: &crate::project::InvariantConfig,
    deterministic: bool,
    project_name: Option<&str>,
) -> String {
    if project_name.is_none() {
        return format!(
            "// SPDX-License-Identifier: UNLICENSED\npragma solidity ^0.8.24;\n// invariant '{}' skipped: multi-entity harness requires project_name (CambrianFactory deploy)\n",
            inv.name
        );
    }
    assert!(
        deterministic,
        "multi-entity invariant factory harness requires deterministic_addresses"
    );
    let inv_name = sanitize_test_name(&inv.name);
    let handler_name = format!("Handler_{}", inv_name);
    let test_name = format!("Invariant_{}_Test", inv_name);

    // Resolve instance -> entity.
    let mut entity_for_inst: Vec<(String, &Entity)> = Vec::new();
    for inst in &inv.instances {
        if let Some(e) = program.entities.iter().find(|e| e.name == inst.entity) {
            entity_for_inst.push((inst.name.clone(), e));
        }
    }
    if entity_for_inst.len() != inv.instances.len() {
        return format!(
            "// SPDX-License-Identifier: UNLICENSED\npragma solidity ^0.8.24;\n// invariant '{}' skipped: at least one instance references an unknown entity\n",
            inv.name,
        );
    }

    // Distinct entities referenced.
    let mut seen_entities: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let mut distinct_entities: Vec<&Entity> = Vec::new();
    for (_, e) in &entity_for_inst {
        if seen_entities.insert(e.name.as_str()) {
            distinct_entities.push(*e);
        }
    }

    let mut out = String::new();
    out.push_str("// SPDX-License-Identifier: UNLICENSED\n");
    out.push_str("pragma solidity ^0.8.24;\n\n");
    out.push_str("import \"forge-std/Test.sol\";\n");
    out.push_str("import \"forge-std/StdInvariant.sol\";\n");
    emit_deterministic_project_import(project_name.unwrap(), &mut out);
    for e in &distinct_entities {
        out.push_str(&format!("import \"../src/{}.sol\";\n", e.name));
    }
    out.push('\n');

    let entity_for_inst_refs: Vec<(&str, &Entity)> = entity_for_inst
        .iter()
        .map(|(n, e)| (n.as_str(), *e))
        .collect();

    let trace_feat = crate::codegen::test_backend::scan_invariant_trace_features(inv);
    let action_routes: Vec<String> = {
        let mut v: Vec<String> = Vec::new();
        for a in &inv.actions {
            if !v.contains(&a.route) {
                v.push(a.route.clone());
            }
        }
        v
    };
    let handler_state_idents: Vec<String> = inv
        .track
        .iter()
        .map(|s| s.name.clone())
        .chain(inv.derived.iter().map(|q| q.name.clone()))
        .chain(trace_handler_state_idents(&trace_feat, &action_routes))
        .collect();

    // ---- Handler ----
    out.push_str(&format!("contract {} is Test {{\n", handler_name));
    for (inst_name, e) in &entity_for_inst {
        out.push_str(&format!("    {} public _{};\n", e.name, inst_name));
    }
    if trace_feat.uses_length {
        out.push_str("    uint256 public _traceLen;\n");
    }
    for r in &trace_feat.counted {
        out.push_str(&format!("    uint256 public _traceCount_{};\n", r));
    }
    if trace_feat.uses_last {
        for r in &action_routes {
            out.push_str(&format!("    bool public _traceLast_{};\n", r));
        }
    }
    emit_handler_cam_wire_multi(&entity_for_inst_refs, &mut out);

    // Wrappers.
    for action in &inv.actions {
        let entity = entity_for_inst
            .iter()
            .find(|(n, _)| n == &action.instance)
            .map(|(_, e)| *e)
            .unwrap();
        let wrapper_name = format!("{}_{}", action.instance, action.route);
        let params: Vec<String> = action
            .params
            .iter()
            .map(|p| {
                format!(
                    "{} {}",
                    sol_type_entity(entity, &p.ty, true, &super::solidity::active_ctx()),
                    p.name
                )
            })
            .collect();
        out.push_str(&format!(
            "\n    function {}({}) public {{\n",
            wrapper_name,
            params.join(", "),
        ));
        for step in &action.body {
            match step {
                TestStep::Bound {
                    var,
                    lo,
                    hi,
                    inclusive,
                } => {
                    let lo_s = gen_expr_test(lo, entity).unwrap_or_else(|| "0".to_string());
                    let hi_s_raw = gen_expr_test(hi, entity).unwrap_or_else(|| "0".to_string());
                    let hi_s = if *inclusive {
                        hi_s_raw
                    } else {
                        format!("({}) - 1", hi_s_raw)
                    };
                    let ty = bound_param_sol_ty(entity, &action.params, var);
                    out.push_str(&emit_bound_step(var, &ty, &lo_s, &hi_s));
                }
                TestStep::Assume { cond } => {
                    let c = gen_expr_test(cond, entity).unwrap_or_else(|| "true".to_string());
                    out.push_str(&format!("        vm.assume({});\n", c));
                }
                _ => {}
            }
        }
        let arg_names: Vec<String> = action.params.iter().map(|p| p.name.clone()).collect();
        out.push_str(&format!(
            "        _{}.{}({});\n",
            action.instance,
            action.route,
            arg_names.join(", "),
        ));
        emit_trace_counter_bump(&trace_feat, &action_routes, Some(&action.route), &mut out);
        out.push_str("    }\n");
    }
    out.push_str("}\n\n");

    // ---- Test contract ----
    out.push_str(&format!(
        "contract {} is StdInvariant, Test {{\n",
        test_name
    ));
    for (inst_name, e) in &entity_for_inst {
        out.push_str(&format!("    {} internal _{};\n", e.name, inst_name));
    }
    out.push_str(&format!("    {} internal _handler;\n", handler_name));
    emit_cambrian_factory_field(&mut out);
    for (_, e) in &entity_for_inst {
        gen_entity_test_constants(e, &mut out);
    }
    out.push_str("    function setUp() public {\n");
    let siblings: Vec<(&str, &str)> = entity_for_inst
        .iter()
        .map(|(n, e)| (n.as_str(), e.name.as_str()))
        .collect();
    let inits: Vec<&[(String, Expr)]> = entity_for_inst
        .iter()
        .map(|(inst_name, _)| {
            inv.instances
                .iter()
                .find(|i| &i.name == inst_name)
                .map(|i| i.init.as_slice())
                .unwrap_or(&[])
        })
        .collect();
    let order = instance_deploy_order(&entity_for_inst, &inits);
    emit_cambrian_factory_new(&mut out);
    out.push_str(&format!("        _handler = new {}();\n", handler_name));
    let mut deployed = std::collections::HashSet::new();
    for &idx in &order {
        let (inst_name, e) = &entity_for_inst[idx];
        let inst_init: &[(String, Expr)] = inv
            .instances
            .iter()
            .find(|i| i.name == *inst_name)
            .map(|i| i.init.as_slice())
            .unwrap_or(&[]);
        let var_name = format!("_{}", inst_name);
        let entity_for_inst_refs: Vec<(&str, &Entity)> = entity_for_inst
            .iter()
            .map(|(n, ent)| (n.as_str(), *ent))
            .collect();
        let deploy_ctx = MultiFactoryDeployCtx {
            entity_for_inst: &entity_for_inst_refs,
            inits: &inits,
            siblings: &siblings,
            deployed: deployed.clone(),
        };
        let deploy_args =
            invariant_factory_deploy_args(e, inst_init, inv, &siblings, Some(&deploy_ctx));
        emit_factory_deploy_entity(&e.name, &var_name, &deploy_args, &mut out);
        let predict_args = identity_ctor_args_sol(e, inst_init, &siblings, Some(&deploy_ctx));
        emit_factory_predict_assert(&e.name, &var_name, &predict_args, &mut out);
        deployed.insert(inst_name.clone());
    }

    // Per-instance leftover init writes (pinned + forall-randomized).
    // Ctor-owned fields were already supplied via factory.deploy* / `initialize`.
    for inst in &inv.instances {
        let entity = entity_for_inst
            .iter()
            .find(|(n, _)| n == &inst.name)
            .map(|(_, e)| *e)
            .unwrap();
        let var_name = format!("_{}", inst.name);
        let leftover = leftover_init_state(entity, &inst.init);
        if !leftover.is_empty() {
            gen_state_init(entity, &leftover, &var_name, program, &mut out);
        }
        let forall_fields = forall_dynamic_fields(entity, inst);
        let prefix = format!("{}_", inst.name);
        gen_forall_state_init(entity, &forall_fields, &var_name, &prefix, program, &mut out);
    }

    let cam_wire_args: Vec<String> = entity_for_inst
        .iter()
        .map(|(n, _)| format!("_{}", n))
        .collect();
    out.push_str(&format!(
        "        _handler.cam_wire({});\n",
        cam_wire_args.join(", ")
    ));

    out.push_str("        targetContract(address(_handler));\n");

    // Selector list.
    if !inv.actions.is_empty() {
        out.push_str(&format!(
            "        bytes4[] memory selectors = new bytes4[]({});\n",
            inv.actions.len(),
        ));
        for (i, action) in inv.actions.iter().enumerate() {
            let entity = entity_for_inst
                .iter()
                .find(|(n, _)| n == &action.instance)
                .map(|(_, e)| *e)
                .unwrap();
            let sig_params: Vec<String> = action
                .params
                .iter()
                .map(|p| sol_type_entity(entity, &p.ty, false, &super::solidity::active_ctx()))
                .collect();
            let wrapper_name = format!("{}_{}", action.instance, action.route);
            out.push_str(&format!(
                "        selectors[{}] = bytes4(keccak256(\"{}({})\"));\n",
                i,
                wrapper_name,
                sig_params.join(","),
            ));
        }
        out.push_str("        targetSelector(FuzzSelector({addr: address(_handler), selectors: selectors}));\n");
    }

    let ctx_entity = entity_for_inst
        .first()
        .map(|(_, e)| *e)
        .expect("invariant has instances");
    if !inv.senders.is_empty() {
        for s in &inv.senders {
            let s_str = gen_address_expr_test(s, ctx_entity);
            out.push_str(&format!("        targetSender({});\n", s_str));
        }
    }

    // `ctx { ... }` -- blockchain/context params (warp / targetSender).
    gen_invariant_foundry_ctx(
        &inv.context,
        ctx_entity,
        super::invariant_harness::has_explicit_deploy(inv),
        &mut out,
    );

    out.push_str("    }\n");

    let tag_suffix = inv
        .tag
        .as_deref()
        .map(|t| format!("_{}", sanitize_tag_for_fn(t)))
        .unwrap_or_default();
    let revert_prefix = inv
        .tag
        .as_deref()
        .map(|t| format!("[{}] ", t))
        .unwrap_or_default();

    // Invariant assertions.
    let evm_ctx = EvmCtx::build(program, deterministic, true);
    for (idx, check) in inv.checks.iter().enumerate() {
        let suffix = if inv.checks.len() == 1 {
            String::new()
        } else {
            format!("_{}", idx)
        };
        let cond_subst = match crate::codegen::predicate_expr::classify_invariant_check(
            program, inv, idx, &evm_ctx,
        ) {
            crate::codegen::predicate_expr::CheckKind::Helper {
                fn_name, instance, ..
            } => crate::codegen::predicate_expr::helper_call_foundry(
                instance.as_deref(),
                "",
                &fn_name,
            ),
            crate::codegen::predicate_expr::CheckKind::Inline => rewrite_handler_idents(
                &lower_check_expr_multi(check, &entity_for_inst),
                &handler_state_idents,
            ),
            crate::codegen::predicate_expr::CheckKind::Unsupported => {
                unreachable!(
                    "I17 skipped (bug): {}",
                    crate::codegen::predicate_expr::i17_message(&inv.name, idx)
                )
            }
        };
        if let Some(tag) = &inv.tag {
            out.push_str(&format!("\n    /// @dev tag: {}", tag));
        } else {
            out.push('\n');
        }
        if let Some(n) = inv.runs {
            out.push_str(&format!(
                "\n    /// forge-config: default.invariant.runs = {}",
                n
            ));
        }
        if let Some(n) = inv.depth {
            out.push_str(&format!(
                "\n    /// forge-config: default.invariant.depth = {}",
                n
            ));
        }
        out.push_str(&format!(
            "\n    function invariant_{}{}{}() public view {{\n",
            inv_name, suffix, tag_suffix,
        ));
        out.push_str(&format!(
            "        require({}, \"{}invariant '{}' violated\");\n",
            cond_subst, revert_prefix, inv.name,
        ));
        out.push_str("    }\n");
    }

    if inv.fail_on_revert {
        out.push_str("\n    // #[fail_on_revert]: Foundry treats handler reverts as failures via foundry.toml\n");
    } else {
        out.push_str("\n    // default: handler reverts are silently skipped\n");
    }

    out.push_str("}\n");
    out
}

/// Lower a single-entity invariant `check` for Foundry `require(...)`.
pub(crate) fn lower_invariant_check_inline(
    program: &Program,
    entity: &Entity,
    expr: &Expr,
    ctx: &super::solidity::core::ctx::EvmCtx,
    sut_var: &str,
    handler_state_idents: &[String],
) -> String {
    invariant_predicate_lower::lower_single_entity_check(
        program,
        entity,
        expr,
        ctx,
        sut_var,
        handler_state_idents,
    )
}

/// Lower a multi-instance invariant `check` for Foundry `require(...)`.
pub(crate) fn lower_check_expr_multi(expr: &Expr, entity_for_inst: &[(String, &Entity)]) -> String {
    invariant_predicate_lower::lower_multi_entity_check(expr, entity_for_inst)
}

/// Rewrite bare references to handler-state identifiers (`track`
/// bindings + `derived` query names) to call them on `_handler`.
/// Track bindings appear as `name` (public state -> auto-getter), so
/// they lower to `_handler.name()`. Derived queries appear as
/// `name(args)`, so they lower to `_handler.name(args)`.
///
/// Both rewrites happen in a single pass per name so a name that
/// appears as both a bare reference and a call site doesn't get
/// double-substituted (`utilization` -> `_handler.utilization` ->
/// `_handler.utilization()` is the single-pass shape; the
/// previous two-pass implementation produced `_handler._handler.…()`).
pub(crate) fn rewrite_handler_idents(expr_src: &str, state_idents: &[String]) -> String {
    let mut result = expr_src.to_string();
    for n in state_idents {
        result = rewrite_one_handler_ident(&result, n);
    }
    result
}

fn rewrite_one_handler_ident(haystack: &str, needle: &str) -> String {
    if needle.is_empty() {
        return haystack.to_string();
    }
    let chars: Vec<char> = haystack.chars().collect();
    let n_chars: Vec<char> = needle.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let end = i + n_chars.len();
        let prev_is_ident = i > 0 && is_ident_char(chars[i - 1]);
        let prev_is_dot = i > 0 && chars[i - 1] == '.';
        if end <= chars.len() && chars[i..end] == n_chars[..] && !prev_is_ident && !prev_is_dot {
            // Look ahead: is this a call site (`name(...)`) or a bare
            // reference?
            let next_is_paren = end < chars.len() && chars[end] == '(';
            let next_is_ident = end < chars.len() && is_ident_char(chars[end]);
            if next_is_ident {
                out.push(chars[i]);
                i += 1;
            } else if next_is_paren {
                // `name(...)` -> `_handler.name(...)` (do not add `()`).
                out.push_str(&format!("_handler.{}", needle));
                i = end;
            } else {
                // bare `name` -> `_handler.name()` (auto-getter).
                out.push_str(&format!("_handler.{}()", needle));
                i = end;
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Best-effort rewrite of bare member identifiers to `var.member()` accessors so
/// invariant `check` expressions can reference entity state by name. Also
/// rewrites bare references to public route names (e.g. `allPairsLength`) to
/// `var.allPairsLength` so a `check allPairsLength() == 0` clause binds to
/// the entity instance rather than emitting an unbound free-function call.
///
/// `state_idents` lists names exposed on the *handler* itself (track
/// snapshot bindings + `derived` query helpers). These are NOT rewritten —
/// they live on the handler, not on the SUT.
pub(crate) fn substitute_member_accessors(
    expr_src: &str,
    entity: &Entity,
    var_name: &str,
    state_idents: &[String],
) -> String {
    let mut result = expr_src.to_string();
    for m in &entity.members {
        if m.is_identity {
            continue;
        }
        if state_idents.iter().any(|s| s == &m.name) {
            continue;
        }
        // Inline lowerer may already emit `{var}.{member}(…)` (HashMap getter).
        if result.contains(&format!("{}.{member}(", var_name, member = m.name)) {
            continue;
        }
        let needle = m.name.clone();
        let replacement = format!("{}.{}()", var_name, m.name);
        result = whole_word_replace(&result, &needle, &replacement);
        // Parallel `_keys` sidecars emitted for iterable HashMap members
        // (pure-fn aggregate args) must bind to the SUT instance too.
        let keys_needle = format!("{}_keys", m.name);
        let keys_replacement = format!("{}.{}_keys()", var_name, m.name);
        result = whole_word_replace(&result, &keys_needle, &keys_replacement);
    }
    for r in &entity.routes {
        if r.is_init || r.name == "constructor" {
            continue;
        }
        if state_idents.iter().any(|s| s == &r.name) {
            continue;
        }
        let needle = r.name.clone();
        let replacement = format!("{}.{}", var_name, r.name);
        // Only rewrite when the route name is followed by `(` (i.e. a call
        // site) — bare identifier matches are already covered by the member
        // pass above.
        result = whole_word_replace_before_paren(&result, &needle, &replacement);
    }
    rewrite_entity_pure_fn_calls(&result, var_name)
}

/// After member substitution, Cambrian pure helpers like
/// `balance_of(m_balances, owner)` become ill-typed Solidity
/// `balance_of(_rHT.m_balances(), owner)` — rewrite to route calls.
fn rewrite_entity_pure_fn_calls(expr_src: &str, var_name: &str) -> String {
    use regex::Regex;
    let mut result = expr_src.to_string();
    let patterns: &[(&str, &str)] = &[
        (
            &format!(r"balance_of\(\s*{var_name}\.m_balances\(\)\s*,\s*([^)]+)\)"),
            &format!("{var_name}.balanceOf($1)"),
        ),
        (
            &format!(
                r"allowance_of\(\s*{var_name}\.m_allowances\(\)\s*,\s*([^,]+)\s*,\s*([^)]+)\)"
            ),
            &format!("{var_name}.allowance($1, $2)"),
        ),
    ];
    for (pat, repl) in patterns {
        if let Ok(re) = Regex::new(pat) {
            let replacement = repl.to_string();
            result = re.replace_all(&result, replacement.as_str()).into_owned();
        }
    }
    result
}

fn whole_word_replace_before_paren(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return haystack.to_string();
    }
    let chars: Vec<char> = haystack.chars().collect();
    let n_chars: Vec<char> = needle.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let end = i + n_chars.len();
        if end < chars.len()
            && chars[i..end] == n_chars[..]
            && (i == 0 || !is_ident_char(chars[i - 1]))
            && chars[end] == '('
        {
            out.push_str(replacement);
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn whole_word_replace(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return haystack.to_string();
    }
    let chars: Vec<char> = haystack.chars().collect();
    let n_chars: Vec<char> = needle.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let end = i + n_chars.len();
        if end <= chars.len()
            && chars[i..end] == n_chars[..]
            && (i == 0 || !is_ident_char(chars[i - 1]))
            && (end == chars.len() || !is_ident_char(chars[end]))
        {
            out.push_str(replacement);
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

pub(crate) fn sanitize_test_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Turn `INV-FEE-001` into `INV_FEE_001` so it can be appended to a
/// generated test function name and discovered with
/// `forge test --match-test '*_INV_FEE_001'`.
pub(crate) fn sanitize_tag_for_fn(tag: &str) -> String {
    tag.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn sanitize_var_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

pub(crate) fn entity_var_name(entity_name: &str) -> String {
    let mut name = String::from("_");
    let mut first = true;
    for c in entity_name.chars() {
        if first {
            name.extend(c.to_lowercase());
            first = false;
        } else {
            name.push(c);
        }
    }
    name
}

pub(crate) fn default_value_for_type(ty: &Type) -> String {
    match ty {
        Type::Simple(name) => match name.as_str() {
            "bool" => "false".to_string(),
            "String" => "\"\"".to_string(),
            "address" => "address(0)".to_string(),
            "pubkey" => "bytes32(0)".to_string(),
            _ => "0".to_string(),
        },
        Type::TypedAddress(_) => "address(0)".to_string(),
        _ => "0".to_string(),
    }
}

fn infer_sol_type(expr: &Expr) -> String {
    match expr {
        Expr::IntLiteral(v) => infer_int_literal_sol_ty(v),
        Expr::BoolLiteral(_) => "bool".to_string(),
        Expr::StringLiteral(_) => "string memory".to_string(),
        Expr::BytesLiteral(_) => "bytes memory".to_string(),
        _ => "uint256".to_string(),
    }
}
