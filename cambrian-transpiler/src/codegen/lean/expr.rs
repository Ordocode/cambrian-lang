// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean codegen — expression lowering (P1.3).
//!
//! Mirrors the structural intent of [`evm_expr.rs`](evm_expr.rs) but
//! targets Lean 4 syntax. The entry point is [`gen_expr`], which folds
//! a Cambrian [`Expr`] into a Lean term, threading a [`LeanExprCtx`]
//! that knows about:
//!
//! * the enclosing entity (for `m_<name>` → `s.m_<name>` resolution and
//!   `<E>.<CONST>` / `<E>.<pure_fn>` qualification);
//! * in-scope `let`-bindings and route params (for shadowing);
//! * the current `(route, phase)` so `^m_a` can resolve to a call into
//!   `<E>.Members.M<a>.<route>_<phase>` (see [docs/PLAN_LEAN_TARGET.md](../../docs/PLAN_LEAN_TARGET.md) §P1.3).
//!
//! Unsupported expression shapes (closures, iterator chains, raw
//! collections) are gated by validator rules `L1`/`L2` upstream; if
//! one slips through we emit a `/- TODO(P4): … -/ Cambrian.Unsupported`
//! placeholder so Lean refuses to build at the offending site rather
//! than silently miscompiling.

use std::collections::{HashMap, HashSet};

use crate::ast::{
    BinOp, Entity, EnumDecl, EnumVariant, Expr, MatchArm, MatchPattern, Pattern, Program, PureFn,
    RouteAction, Type, UnaryOp,
};
use crate::ir::{CoerceKind, ResolvedType, TypedExpr, TypedExprKind};

use super::core::hashmap::{gen_empty_map, try_gen_map_index, try_gen_map_method_call};
use super::core::iter::{gen_for_expr, try_gen_fold_method, try_gen_iter_expr};
use super::core::map_analysis::member_is_hashmap;
use super::core::types::{
    is_generated_type_name, lean_local_type_name, lean_string_literal, lower_type,
    program_type_ref, sanitize_variant, type_is_signed,
    LeanTypeCtx,
};
use super::evm::domain::{EvmDomain, LeanDomain, SysFieldCtx};

/// Active domain adapter for expression lowering. Today always
/// [`EvmDomain`]; a future non-EVM Lean adapter swaps this singleton.
fn domain() -> EvmDomain {
    EvmDomain
}

/// Context threaded through expression lowering. Holds everything
/// needed to resolve identifiers and emit per-phase member calls.
pub struct LeanExprCtx<'a> {
    pub type_ctx: LeanTypeCtx<'a>,
    pub entity: &'a Entity,
    pub local_enums: &'a [EnumDecl],
    /// Pure fns visible at the top level (entity-scope helpers are
    /// elsewhere in P1; we only emit top-level `pure fn`s).
    pub pure_fns: &'a [PureFn],
    /// Names currently in scope from `let` bindings.
    pub lets: HashSet<String>,
    /// Names currently in scope as route parameters.
    pub route_params: HashSet<String>,
    /// The active route name when lowering inside a route body or a
    /// member transform — drives `^m_a` resolution. `None` for
    /// top-level pure-fn / constant bodies.
    pub route_name: Option<String>,
    /// The active phase name when lowering inside a phased route body
    /// or a member transform tied to a phase. `None` for unphased
    /// contexts.
    pub phase_name: Option<String>,
    /// Per-route, per-phase: the route argument names (positional).
    /// Used so `^m_a` can synthesise the same argument tuple as the
    /// generated `<E>.Members.M<a>.<route>[_<phase>](s, ctx, args...)`
    /// wrapper expects.
    pub route_arg_names: Vec<String>,
    /// True when lowering a transform body — `m_x` in this context
    /// refers to the *pre-phase* state member, while `^m_x` is the
    /// post-phase value of another member.
    pub in_transform: bool,
    /// Lean variable name to which bare member references (`m_x` →
    /// `<state_var>.m_x`) and `^m_a` fall-backs resolve. Routes use
    /// `"s"`; spec emitters override (e.g. `"s₀"` for fuzz `bound`
    /// chains, `"s_final"` for invariant `check` bodies).
    pub state_var: String,
    /// Names of `derived foo() -> T { … }` helpers visible from this
    /// scope. A bare call `foo(args)` to one of these gets rewritten
    /// at lowering time as `(foo <state_var> args)` so the helper's
    /// implicit state parameter is supplied. Empty in non-invariant
    /// contexts.
    pub derived_helpers: HashSet<String>,
    /// Name of the Lean variable holding the per-program `World` value
    /// when lowering inside a route entry-point or a spec body (P3.6).
    /// `None` ⇒ legacy mode (no world access); `sys::balance` and
    /// cross-instance reads fall back to the P1 placeholder. Routes set
    /// this to `"w"`; spec emitters override as needed.
    pub world_var: Option<String>,
    /// Name of the Lean variable holding the active `<E>.Identity`
    /// value when lowering inside a route entry-point or a spec body
    /// (P3.6). `None` ⇒ identity-member references stay on the
    /// `state_var` projection (legacy behaviour). Route entry-points
    /// set this to `"inst"`.
    pub instance_var: Option<String>,
    /// Multi-entity invariant: map instance alias (`v`) to
    /// `(entity_name, lean_identity_var)`.
    pub qualified_instances: HashMap<String, (String, String)>,
    /// Property/test `deploy binding = Entity(...)`: map binding name to
    /// `(entity_name, lean_inst_var)` for address lowering (`Entity.address inst`).
    pub deploy_bindings: HashMap<String, (String, String)>,
    /// Names of in-scope identifiers (route / pure-fn params, `let`
    /// bindings) whose Cambrian type is `HashMap<K, V>`. Used by the
    /// HashMap method / index lowering so receivers other than state
    /// members (e.g. pure-fn parameters of map type) also dispatch
    /// through `Cambrian.AddressMap.*`.
    /// Names of `HashMap` values currently in scope (params / `let` binders)
    /// so nested-map method calls resolve correctly.
    pub hashmap_idents: HashSet<String>,
    /// Names bound by a `List.range` / range-fold element binder (UPSTREAM
    /// B-19). These elaborate as `Nat`; coercing them to BitVec must use
    /// `BitVec.ofNat`, not `Cambrian.castWidth`.
    pub nat_idents: HashSet<String>,
    /// When lowering invariant `assume` / `check` expressions that
    /// reference `trace::` accessors, the Lean variable name holding the
    /// trace accumulator (`TraceAcc`). `None` outside trace-aware
    /// invariant contexts; a stray `trace::` ref then lowers to a
    /// `Cambrian.Unsupported` placeholder so Lean refuses to build.
    pub trace_acc_var: Option<String>,
    /// Expected `BitVec` width for literal / match-arm ascription (B-7).
    /// Set from the enclosing route return type, pure-fn return type, or
    /// member type when known; `None` defaults to Cam's `u64` width (64).
    /// Without this, a `match` whose arms are small integers always
    /// elaborates as `BitVec 64` and fails when the consumer expects
    /// `BitVec 8` (e.g. `payment_channel.getChannelState -> u8`).
    pub expected_bitvec_width: Option<u32>,
    /// When `Some(true)`, ambient numeric context is a signed Cambrian
    /// type (`i8`…`i128`): literals ascribe as `Int` under `numerics: nat`,
    /// and BitVec widen uses `signExtend`. `Some(false)` forces unsigned;
    /// `None` means "infer from operands / identifiers".
    pub expected_signed: Option<bool>,
    /// Expected collection shape for bare `{}` (`EmptyCollection`) (B-14).
    /// Member transforms set this from the member's `Vec` / `HashMap` type
    /// so `{}` lowers to `[]` or `AddressMap.empty` respectively.
    pub expected_collection: Option<ExpectedCollection>,
    /// When set, `msg::sender` lowers to this term instead of `ctx.sender`
    /// (deploy child init uses the factory address — W2-BC-03).
    pub msg_sender_override: Option<String>,
    /// The `pure fn` whose body is being lowered; its parameters type bare
    /// identifiers (B-34). `None` outside pure-fn bodies.
    pub pure_fn: Option<&'a PureFn>,
}

/// Shape hint for lowering `Expr::EmptyCollection` (`{}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedCollection {
    List,
    Map,
}

/// Best-effort collection shape for a Cambrian type (aliases resolved).
pub(crate) fn collection_shape_of_type(
    ty: &Type,
    type_ctx: &LeanTypeCtx<'_>,
) -> Option<ExpectedCollection> {
    match ty {
        Type::Generic(name, _) if name == "Vec" => Some(ExpectedCollection::List),
        Type::Generic(name, _) if name == "HashMap" => Some(ExpectedCollection::Map),
        Type::Simple(name) => {
            let alias = type_ctx
                .local_aliases
                .iter()
                .find(|a| a.name == *name)
                .or_else(|| {
                    type_ctx
                        .program
                        .type_aliases
                        .iter()
                        .find(|a| a.name == *name)
                })?;
            collection_shape_of_type(&alias.ty, type_ctx)
        }
        _ => None,
    }
}

/// Best-effort `BitVec n` width for a Cambrian type (aliases resolved).
pub(crate) fn bitvec_width_of_type(ty: &Type, type_ctx: &LeanTypeCtx<'_>) -> Option<u32> {
    match ty {
        Type::Simple(name) => match name.as_str() {
            "u8" | "i8" => Some(8),
            "u16" | "i16" => Some(16),
            "u32" | "i32" => Some(32),
            "u64" | "i64" => Some(64),
            "u128" | "i128" => Some(128),
            "U256" | "uint256" => Some(256),
            "address" => Some(160),
            "pubkey" => Some(256),
            other => {
                let alias = type_ctx
                    .local_aliases
                    .iter()
                    .find(|a| a.name == other)
                    .or_else(|| {
                        type_ctx
                            .program
                            .type_aliases
                            .iter()
                            .find(|a| a.name == other)
                    });
                alias.and_then(|a| bitvec_width_of_type(&a.ty, type_ctx))
            }
        },
        _ => None,
    }
}

/// True when `expr` is known to inhabit a signed Cambrian integer type.
pub(crate) fn expr_is_signed(expr: &Expr, ctx: &LeanExprCtx<'_>) -> bool {
    if ctx.expected_signed == Some(true) {
        return true;
    }
    match expr {
        Expr::Ident(name) => {
            if let Some(p) = ctx
                .entity
                .routes
                .iter()
                .filter(|r| Some(&r.name) == ctx.route_name.as_ref())
                .flat_map(|r| r.params.iter())
                .find(|p| &p.name == name)
            {
                return type_is_signed(&p.ty, &ctx.type_ctx);
            }
            if let Some(p) = pure_fn_param(name, ctx) {
                return type_is_signed(&p.ty, &ctx.type_ctx);
            }
            if ctx.route_params.contains(name) {
                // Spec / fuzz binders: look up entity members only below.
            }
            if let Some(m) = ctx.entity.members.iter().find(|m| &m.name == name) {
                return type_is_signed(&m.ty, &ctx.type_ctx);
            }
            false
        }
        Expr::TemporalRef(member_name) => ctx
            .entity
            .members
            .iter()
            .find(|m| &m.name == member_name)
            .is_some_and(|m| type_is_signed(&m.ty, &ctx.type_ctx)),
        Expr::FieldAccess(base, field) => {
            if let Expr::Ident(inst) = base.as_ref() {
                if let Some((ent, _)) = ctx.qualified_instances.get(inst) {
                    if let Some(e) = ctx.type_ctx.program.entities.iter().find(|e| &e.name == ent)
                    {
                        return e
                            .members
                            .iter()
                            .find(|m| &m.name == field)
                            .is_some_and(|m| type_is_signed(&m.ty, &ctx.type_ctx));
                    }
                }
            }
            false
        }
        Expr::UnaryOp(UnaryOp::Neg, inner) => expr_is_signed(inner, ctx) || ctx.expected_signed != Some(false),
        Expr::Cast(_, target) => type_is_signed(target, &ctx.type_ctx),
        Expr::BinOp(lhs, op, rhs) => {
            matches!(
                op,
                BinOp::Add
                    | BinOp::Sub
                    | BinOp::Mul
                    | BinOp::Div
                    | BinOp::Mod
                    | BinOp::WrappingAdd
                    | BinOp::WrappingSub
                    | BinOp::WrappingMul
            ) && (expr_is_signed(lhs, ctx) || expr_is_signed(rhs, ctx))
        }
        _ => false,
    }
}

impl<'a> LeanExprCtx<'a> {
    pub fn push_pattern(&mut self, pat: &Pattern) {
        match pat {
            Pattern::Ident(name) => {
                self.lets.insert(name.clone());
            }
            Pattern::Tuple(parts) => {
                for p in parts {
                    self.push_pattern(p);
                }
            }
            Pattern::Some(inner) | Pattern::Deref(inner) => {
                self.push_pattern(inner);
            }
            Pattern::Wildcard | Pattern::None => {}
        }
    }

    /// Build a context tailored for **spec** lowering — `test`, `fuzz`,
    /// or `invariant` bodies (P2). Spec-side expressions never appear
    /// inside member transforms or phased routes (no `^m_a`, no
    /// `in_transform`), but they do bind:
    ///
    /// * `state_var` — the Lean variable name to which bare `m_<x>`
    ///   resolves (`s` for tests / step bodies, `s₀` for fuzz `bound`s,
    ///   `s_final` for invariant `check` bodies, etc.).
    /// * `params` — fuzz / route / `derived` parameters in scope.
    /// * `lets` — `let`-bindings already visible at this point in the
    ///   spec body (test `let`s, invariant `track {}` bindings).
    pub fn for_spec(
        program: &'a Program,
        entity: &'a Entity,
        state_var: impl Into<String>,
        params: HashSet<String>,
        lets: HashSet<String>,
        profile: super::LeanProfile,
    ) -> Self {
        LeanExprCtx {
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
            lets,
            route_params: params,
            route_name: None,
            phase_name: None,
            route_arg_names: Vec::new(),
            in_transform: false,
            state_var: state_var.into(),
            derived_helpers: HashSet::new(),
            world_var: None,
            instance_var: None,
            qualified_instances: HashMap::new(),
            deploy_bindings: HashMap::new(),
            hashmap_idents: HashSet::new(),
            nat_idents: HashSet::new(),
            trace_acc_var: None,
            expected_bitvec_width: None,
            expected_signed: None,
            expected_collection: None,
            msg_sender_override: None,
            pure_fn: None,
        }
    }

    /// Override `msg::sender` lowering (e.g. deploy factory address).
    pub fn with_msg_sender_override(mut self, term: impl Into<String>) -> Self {
        self.msg_sender_override = Some(term.into());
        self
    }

    /// Builder shortcut: set `trace_acc_var` so `trace::` accessors lower
    /// against the named accumulator. Returns the modified context for
    /// chaining (`for_spec(...).with_trace_acc("acc")`).
    pub fn with_trace_acc(mut self, acc_var: impl Into<String>) -> Self {
        self.trace_acc_var = Some(acc_var.into());
        self
    }

    /// Builder shortcut: set `world_var` / `instance_var` for a spec
    /// context that runs in P3 World mode. Returns the modified
    /// context so call sites can chain (`for_spec(...).with_world(...)`).
    pub fn with_world(
        mut self,
        world_var: impl Into<String>,
        instance_var: impl Into<String>,
    ) -> Self {
        self.world_var = Some(world_var.into());
        self.instance_var = Some(instance_var.into());
        self
    }

    /// Variant of [`for_spec`] that also installs a set of `derived`
    /// helper names. P2 invariants supply this so calls like
    /// `utilization()` lower as `(utilization s)`.
    pub fn for_spec_with_derived(
        program: &'a Program,
        entity: &'a Entity,
        state_var: impl Into<String>,
        params: HashSet<String>,
        lets: HashSet<String>,
        derived_helpers: HashSet<String>,
        profile: super::LeanProfile,
    ) -> Self {
        let mut ctx = Self::for_spec(program, entity, state_var, params, lets, profile);
        ctx.derived_helpers = derived_helpers;
        ctx
    }

    /// Builder: mark additional names as `Nat`-typed range binders (B-19).
    pub fn with_extra_nat_idents(mut self, names: impl IntoIterator<Item = String>) -> Self {
        self.nat_idents.extend(names);
        self
    }

    /// Builder: map `deploy binding = Entity(...)` names to `(entity, inst_var)`.
    pub fn with_deploy_bindings(
        mut self,
        map: HashMap<String, (String, String)>,
    ) -> Self {
        self.deploy_bindings = map;
        self
    }

    /// Clone this context (shared references, owned collections cloned).
    pub fn dup(&self) -> Self {
        Self {
            type_ctx: clone_type_ctx(&self.type_ctx),
            entity: self.entity,
            local_enums: self.local_enums,
            pure_fns: self.pure_fns,
            lets: self.lets.clone(),
            route_params: self.route_params.clone(),
            route_name: self.route_name.clone(),
            phase_name: self.phase_name.clone(),
            route_arg_names: self.route_arg_names.clone(),
            in_transform: self.in_transform,
            state_var: self.state_var.clone(),
            derived_helpers: self.derived_helpers.clone(),
            world_var: self.world_var.clone(),
            instance_var: self.instance_var.clone(),
            qualified_instances: self.qualified_instances.clone(),
            deploy_bindings: self.deploy_bindings.clone(),
            hashmap_idents: self.hashmap_idents.clone(),
            nat_idents: self.nat_idents.clone(),
            trace_acc_var: self.trace_acc_var.clone(),
            expected_bitvec_width: self.expected_bitvec_width,
            expected_signed: self.expected_signed,
            expected_collection: self.expected_collection,
            msg_sender_override: self.msg_sender_override.clone(),
            pure_fn: self.pure_fn,
        }
    }
}

/// Emit `lo..hi` as a `List Nat` (Lean's `List.range` is `Nat`-only).
/// `BitVec` / `UInt` bounds are converted via `.toNat`; `Nat` and
/// literal bounds pass through untouched. The resulting list shifts
/// by `lo` so the iteration variable still ranges over `[lo, hi)`.
pub fn gen_range_list(lo: &Expr, hi: &Expr, ctx: &LeanExprCtx<'_>) -> String {
    let lo_n = gen_nat_bound(lo, ctx);
    let hi_n = gen_nat_bound(hi, ctx);
    format!("((List.range ({} - {})).map (· + {}))", hi_n, lo_n, lo_n,)
}

/// Lower a range bound to a `Nat` term for `List.range` / list offset.
///
/// Must **not** inherit [`LeanExprCtx::expected_bitvec_width`]: that hint
/// comes from route/pure return types (e.g. `U256` → 256) and would
/// ascribe `(7 : BitVec 256)` into a `Nat`-only position, breaking
/// `(0..7).fold` / `(0..n).fold` under BitVec numerics.
fn gen_nat_bound(e: &Expr, ctx: &LeanExprCtx<'_>) -> String {
    let mut nat_ctx = dup_expr_ctx(ctx);
    nat_ctx.expected_bitvec_width = None;
    match e {
        // Bare literals / Nat trees elaborate as `Nat` against `List.range`.
        _ if is_bare_numeric_literal(e) || is_literal_nat_tree(e) => gen_expr(e, &nat_ctx),
        // Under `numerics: nat` scalars are already `Nat`.
        _ if ctx.type_ctx.use_nat_numerics => gen_expr(e, &nat_ctx),
        // BitVec / UInt params and members — coerce.
        _ => format!("({}).toNat", gen_expr(e, &nat_ctx)),
    }
}

/// Lower an expression to a Lean term string. The returned text is
/// always a single Lean term (parenthesised where needed) so callers
/// can drop it into any expression position.
pub fn gen_expr(expr: &Expr, ctx: &LeanExprCtx<'_>) -> String {
    match expr {
        // Numeric literals stay polymorphic via Lean's `OfNat` unless the
        // surrounding context pinned an expected BitVec width (B-12/3a).
        Expr::IntLiteral(n) => {
            if ctx.type_ctx.use_nat_numerics && ctx.expected_signed == Some(true) {
                return format!("({} : Int)", n);
            }
            let lit = if !n.fits_u128() {
                format!("0x{}", crate::ast::u256_hex_digits(n))
            } else {
                n.to_display_decimal()
            };
            if let Some(w) = ctx.expected_bitvec_width {
                if !ctx.type_ctx.use_nat_numerics {
                    return format!("({} : BitVec {})", lit, w);
                }
            }
            lit
        }
        Expr::StringLiteral(s) => lean_string_literal(s),
        Expr::BytesLiteral(bytes) => gen_bytes_literal(bytes),
        Expr::BoolLiteral(b) => if *b { "true" } else { "false" }.to_string(),
        Expr::ArrayLit(items) => {
            let parts: Vec<String> = items.iter().map(|e| gen_expr(e, ctx)).collect();
            format!("[{}]", parts.join(", "))
        }
        Expr::EmptyCollection => match ctx.expected_collection {
            Some(ExpectedCollection::List) => "[]".to_string(),
            Some(ExpectedCollection::Map) | None => gen_empty_map(),
        },
        Expr::Ident(name) => gen_ident(name, ctx),
        Expr::TemporalRef(member_name) => gen_temporal_ref(member_name, ctx),
        Expr::MacroRef(name, args) => gen_macro_ref(name, args, ctx),
        Expr::MsgField(field) => gen_msg_field(field, ctx),
        Expr::SysField(field) => gen_sys_field(field, ctx),
        Expr::TraceField(field) => gen_trace_field(field, ctx),
        Expr::TraceCall { name, route } => gen_trace_call(name, route, ctx),
        Expr::BinOp(lhs, op, rhs) => gen_binop(lhs, op, rhs, ctx),
        Expr::UnaryOp(op, inner) => gen_unaryop(op, inner, ctx),
        Expr::FieldAccess(inner, field) => {
            // `inst.address` — this contract's CREATE2 address (LEAN-H11).
            // Identity has no `.address` field; the per-entity helper is
            // `<E>.address (id : Identity)`.
            if field == "address" {
                if let Some(term) = try_instance_address_accessor(inner, ctx) {
                    return term;
                }
                if let Expr::Ident(binding) = inner.as_ref() {
                    if let Some((entity, inst_var)) = ctx.deploy_bindings.get(binding) {
                        return format!("({}.address {})", entity, inst_var);
                    }
                }
            }
            if let Expr::Ident(ent_name) = inner.as_ref() {
                if let Some(peer) = ctx
                    .type_ctx
                    .program
                    .entities
                    .iter()
                    .find(|e| &e.name == ent_name)
                {
                    if peer.constants.iter().any(|c| &c.name == field) {
                        return format!(
                            "{}.{}",
                            ent_name,
                            super::core::types::lean_safe_ident(field)
                        );
                    }
                }
            }
            if let Expr::Ident(inst) = inner.as_ref() {
                if let Some((ent, inst_var)) = ctx.qualified_instances.get(inst) {
                    let ef = super::evm::world::entity_field_name(ent);
                    return format!(
                        "(w.storage.{} {}).{}",
                        ef,
                        inst_var,
                        super::core::types::lean_safe_ident(field),
                    );
                }
            }
            if let Some(projection) = lean_tuple_projection(field) {
                return format!("({}){}", gen_expr(inner, ctx), projection);
            }
            format!(
                "({}).{}",
                gen_expr(inner, ctx),
                super::core::types::lean_safe_ident(field)
            )
        }
        Expr::Index(base, idx) => {
            if let Some(term) = try_gen_map_index(base, idx, ctx) {
                term
            } else {
                // Vec indexing. `List.getD` expects a `Nat` index; route
                // params / members carrying numeric indices arrive as
                // `BitVec n`, so project through `.toNat`. We use `getD …
                // default` (rather than the removed-in-v4.26 `List.get!`):
                // it is total, reduces cleanly in proofs, and an
                // out-of-bounds read yields the element's zero value —
                // matching EVM array semantics and the `AddressMap` reads.
                // Under `numerics: nat` the index is already `Nat`, so the
                // `.toNat` projection is dropped.
                let idx_term = if ctx.type_ctx.use_nat_numerics {
                    format!("({})", gen_expr(idx, ctx))
                } else {
                    format!("({}).toNat", gen_expr(idx, ctx))
                };
                format!("(List.getD ({}) {} default)", gen_expr(base, ctx), idx_term,)
            }
        }
        Expr::MethodCall(base, name, args) => {
            if let Some(term) = try_gen_iter_expr(expr, ctx) {
                return term;
            }
            if name == "fold" && args.len() == 2 {
                if let Some(term) = try_gen_fold_method(base, &args[0], &args[1], ctx) {
                    return term;
                }
            }
            if let Some(term) = try_gen_map_method_call(base, name, args, ctx) {
                // Parenthesize: map ops lower to space-applied terms
                // (`Cambrian.AddressMap.insert m k v`) that must be
                // wrapped when used as an argument — e.g. the nested
                // `m.update(k, inner.update(k2, v))` value position.
                return format!("({})", term);
            }
            // `Entity.address(args...)` — deterministic-address helper
            // emitted per-entity by `lean_entity`. The base must be a
            // bare `Ident` matching a known in-program entity name.
            // Also `inst.address()` (no args) — same as field form (LEAN-H11).
            if name == "address" {
                if let Expr::Ident(entity_name) = base.as_ref() {
                    if let Some(term) = create2_address_term(entity_name, args, ctx) {
                        return term;
                    }
                }
                if args.is_empty() {
                    if let Some(term) = try_instance_address_accessor(base, ctx) {
                        return term;
                    }
                }
            }
            // `vec.push(x)` and `vec.len()` for `List`-backed `Vec<T>`.
            if name == "push" && args.len() == 1 {
                let base_term = gen_expr(base, ctx);
                let item_term = gen_expr(&args[0], ctx);
                return format!("({} ++ [{}])", base_term, item_term);
            }
            if name == "len" && args.is_empty() {
                // `List.length` is already `Nat`; under `numerics: nat` keep it
                // there, otherwise widen to the fixed-width `Cambrian.U256`.
                let base_term = gen_expr(base, ctx);
                let ascription = if ctx.type_ctx.use_nat_numerics {
                    "Nat"
                } else {
                    "Cambrian.U256"
                };
                return format!("(({}).length : {})", base_term, ascription);
            }
            // `String.split(delim)` → Lean `String.splitOn` (T-LEAN-LCC-001).
            // `.collect()` peels via `try_gen_iter_expr` and re-enters here.
            if name == "split" && args.len() == 1 {
                let base_term = gen_expr(base, ctx);
                let delim = gen_expr(&args[0], ctx);
                return format!("(({}).splitOn {})", base_term, delim);
            }
            format!(
                "(/- TODO(P4): method call '.{}' not supported in Lean P1 -/ Cambrian.Unsupported)",
                name,
            )
        }
        Expr::FnCall(name, args) => gen_fn_call(name, args, ctx),
        Expr::If(cond, then_e, else_e) => gen_if(cond, then_e, else_e.as_deref(), ctx),
        Expr::Let(pat, value, body) => gen_let(pat, value, body, ctx),
        Expr::Block(exprs) => gen_block(exprs, ctx),
        Expr::RecordConstruct(name, fields) => gen_record_ctor(name, fields, ctx),
        Expr::RecordUpdate(base, updates) => gen_record_update(base, updates, ctx),
        Expr::Closure(_, _) => {
            "(/- L2: closures deferred to P4 -/ Cambrian.Unsupported)".to_string()
        }
        Expr::Cast(inner, target) => gen_cast(inner, target, ctx),
        Expr::Tuple(items) => {
            let parts: Vec<String> = items.iter().map(|e| gen_expr(e, ctx)).collect();
            format!("({})", parts.join(", "))
        }
        Expr::Match(scrutinee, arms) => gen_match(scrutinee, arms, ctx),
        Expr::EnumVariant(en, variant) => {
            format!(
                "{}.{}",
                qualify_enum_ref(en, ctx),
                sanitize_variant(variant)
            )
        }
        Expr::EnumVariantWithData(en, variant, args) => {
            // Disambiguation: the parser produces `EnumVariantWithData`
            // for both `EnumName::Variant(args)` and unparenthesised
            // namespaced calls like `evm::keccak256Packed(...)`. The
            // latter must reach `gen_namespaced_call` so it dispatches
            // to the opaque `Cambrian.*` axioms in the prelude.
            if is_namespace(en) {
                return gen_namespaced_call(en, variant, args, ctx);
            }
            // `HashMap::new()` parses as an enum variant too, but HashMap is a
            // builtin container, not a user enum. The EVM backend already
            // special-cases it (evm_transform.rs); without the same treatment
            // here the Lean target emits `HashMap.new`, which the Prelude does
            // not define, and `lake build` fails on every contract holding a
            // mapping.
            if en == "HashMap" && variant == "new" && args.is_empty() {
                return gen_empty_map();
            }
            // `Lib::fn(args)` parses as `EnumVariantWithData` when `Lib` is a
            // declared library, not an enum (mirror of EVM `library_qualifies`).
            if lean_library_pure_call_supported(ctx.type_ctx.program, en, variant, args.len()) {
                return gen_namespaced_call(en, variant, args, ctx);
            }
            let parts: Vec<String> = args.iter().map(|a| gen_expr(a, ctx)).collect();
            format!(
                "({}.{} {})",
                qualify_enum_ref(en, ctx),
                sanitize_variant(variant),
                parts.join(" "),
            )
        }
        Expr::Some(inner) => format!("(Option.some {})", gen_expr(inner, ctx)),
        Expr::None => "Option.none".to_string(),
        Expr::Range(lo, hi) => gen_range_list(lo, hi, ctx),
        Expr::For(pat, iter, body) => gen_for_expr(pat, iter, body, ctx),
        Expr::NamespacedCall {
            namespace,
            name,
            args,
            ..
        } => gen_namespaced_call(namespace, name, args, ctx),
        Expr::AddressOf {
            entity_name, args, ..
        } => {
            // `address_of <E>(args)` / `addressOf <E>(args)` keyword
            // form — same CREATE2 address as `<E>.address(args)`. The
            // `with { … }` params (code / value / stateInit) do not
            // affect the derived address, so they are ignored here.
            create2_address_term(entity_name, args, ctx).unwrap_or_else(|| {
                format!(
                    "(/- TODO: address_of {} (unknown entity) -/ Cambrian.Unsupported)",
                    entity_name,
                )
            })
        }
        Expr::Encode { .. } => {
            "(/- TODO(P5): encode<T>(...) is test-only -/ Cambrian.Unsupported)".to_string()
        }
    }
}

// ---------------------------------------------------------------------------
// Identifier and temporal-reference resolution
// ---------------------------------------------------------------------------

fn gen_ident(name: &str, ctx: &LeanExprCtx<'_>) -> String {
    if ctx.lets.contains(name) || ctx.route_params.contains(name) {
        return super::core::types::lean_safe_ident(name);
    }
    if let Some((entity, inst_var)) = ctx.deploy_bindings.get(name) {
        return format!("({}.address {})", entity, inst_var);
    }
    if let Some(member) = ctx.entity.members.iter().find(|m| m.name == name) {
        // P3.6: identity members projected from `inst` when available.
        // Both `inst.m_id` and `s.m_id` carry the same value (because
        // `State.identity` projects the identity members verbatim), but
        // the former composes more naturally with `<E>.address inst`
        // and survives spec rewrites that case-split on `inst`.
        if member.is_identity {
            if let Some(inst) = &ctx.instance_var {
                return format!("{}.{}", inst, name);
            }
        }
        return format!("{}.{}", ctx.state_var, name);
    }
    if ctx.entity.constants.iter().any(|c| c.name == name) {
        return format!("{}.{}", ctx.entity.name, name);
    }
    // T-LEAN-EX-010 companion: sanitize reserved names even when the
    // binder was not registered in `lets` / `route_params` (route
    // sequential lets often leave `lets` empty). Cambrian source never
    // means the codegen-owned State/World binders by writing bare
    // `s`/`w`/`inst`/`ctx`.
    super::core::types::lean_safe_ident(name)
}

/// Lower `^m_a` at the active `(route, phase)` to either:
///
/// * the post-phase member-transform call
///   `<E>.Members.M_<a>.<route>_<phase>(s, ctx, args)` when `m_a`
///   *has* a transform at `phase` (its post-phase value is the
///   transform applied to the pre-phase state); or
/// * the bare `s.<a>` projection when `m_a` has *no* transform at
///   `phase` (its post-phase value equals its pre-phase value, and
///   `s` here is the state at the start of `phase` because each
///   phase function pipes through the prior phase's output).
///
/// The two-arm rule is what makes Cambrian's per-(member, route, phase)
/// decomposition compose correctly for multi-member fixtures like
/// `phased_two_member.cam` (see [docs/PLAN_LEAN_TARGET.md](../../docs/PLAN_LEAN_TARGET.md) §P1.4).
fn gen_temporal_ref(member_name: &str, ctx: &LeanExprCtx<'_>) -> String {
    let route = ctx.route_name.as_deref().unwrap_or("<unknown_route>");
    let entity = &ctx.entity.name;
    let target_phase = ctx.phase_name.as_deref();

    let has_transform_at_phase = ctx
        .entity
        .members
        .iter()
        .find(|m| m.name == member_name)
        .map(|m| {
            m.transforms
                .iter()
                .any(|t| t.route_name == route && t.phase.as_deref() == target_phase)
        })
        .unwrap_or(false);

    if !has_transform_at_phase {
        return format!("{}.{}", ctx.state_var, member_name);
    }

    let suffix = match target_phase {
        Some(phase) => format!("_{}", phase),
        None => String::new(),
    };
    let args = ctx.route_arg_names.join(" ");
    let sv = &ctx.state_var;
    let inst = ctx.instance_var.as_deref().unwrap_or("inst");
    if args.is_empty() {
        format!(
            "({}.Members.M_{}.{}{} {} ctx {})",
            entity, member_name, route, suffix, sv, inst
        )
    } else {
        format!(
            "({}.Members.M_{}.{}{} {} ctx {} {})",
            entity, member_name, route, suffix, sv, inst, args
        )
    }
}

// ---------------------------------------------------------------------------
// msg::* and sys::* — routed through the Lean-EVM domain adapter
// ---------------------------------------------------------------------------

fn gen_msg_field(field: &str, ctx: &LeanExprCtx<'_>) -> String {
    if field == "sender" {
        if let Some(term) = &ctx.msg_sender_override {
            return term.clone();
        }
    }
    domain().msg_field(field, ctx.type_ctx.use_nat_numerics)
}

/// Lower a `trace::length` accessor against the in-scope `TraceAcc`.
fn gen_trace_field(field: &str, ctx: &LeanExprCtx<'_>) -> String {
    domain().trace_field(field, ctx.trace_acc_var.as_deref())
}

/// Lower a `trace::count(route)` / `trace::lastWas(route)` accessor.
fn gen_trace_call(name: &str, route: &str, ctx: &LeanExprCtx<'_>) -> String {
    domain().trace_call(name, route, ctx.trace_acc_var.as_deref())
}

fn gen_sys_field(field: &str, ctx: &LeanExprCtx<'_>) -> String {
    let sys = SysFieldCtx {
        world_var: ctx.world_var.as_deref(),
        instance_var: ctx.instance_var.as_deref(),
        entity_name: &ctx.entity.name,
        nat_numerics: ctx.type_ctx.use_nat_numerics,
    };
    domain().sys_field(field, &sys)
}

/// True when a `sys::*` field has no State/MsgCtx fallback and must be
/// lowered against a World binder. Re-export of the analysis helper so
/// Lean codegen / `_pre_*` stay in lockstep with route world-threading.
pub(crate) fn sys_field_needs_world(field: &str) -> bool {
    crate::analysis::sys_field_needs_world(field)
}

/// True when `expr` (or a `@macro` body it invokes) reads a world-only
/// `sys::*` field (`balance`, `blockNumber`, `chainid`), which only
/// lowers against a `World` binder.
pub(crate) fn expr_needs_world(entity: &Entity, expr: &Expr) -> bool {
    match expr {
        Expr::SysField(f) if sys_field_needs_world(f) => true,
        Expr::MacroRef(name, args) => {
            args.iter().any(|a| expr_needs_world(entity, a))
                || entity
                    .macros
                    .iter()
                    .find(|m| m.name == *name)
                    .is_some_and(|m| expr_needs_world(entity, &m.body))
        }
        Expr::ArrayLit(items) | Expr::Tuple(items) | Expr::Block(items) => {
            items.iter().any(|e| expr_needs_world(entity, e))
        }
        Expr::BinOp(l, _, r) | Expr::Index(l, r) | Expr::Range(l, r) | Expr::For(_, l, r) => {
            expr_needs_world(entity, l) || expr_needs_world(entity, r)
        }
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) | Expr::Some(e) => {
            expr_needs_world(entity, e)
        }
        Expr::RecordUpdate(base, fields) => {
            expr_needs_world(entity, base)
                || fields.iter().any(|(_, e)| expr_needs_world(entity, e))
        }
        Expr::If(c, t, e) => {
            expr_needs_world(entity, c)
                || expr_needs_world(entity, t)
                || e.as_ref().is_some_and(|x| expr_needs_world(entity, x))
        }
        Expr::Let(_, v, b) => expr_needs_world(entity, v) || expr_needs_world(entity, b),
        Expr::RecordConstruct(_, fields) => fields.iter().any(|(_, e)| expr_needs_world(entity, e)),
        Expr::MethodCall(base, _, args) => {
            expr_needs_world(entity, base) || args.iter().any(|a| expr_needs_world(entity, a))
        }
        Expr::FnCall(_, args) | Expr::EnumVariantWithData(_, _, args) => {
            args.iter().any(|a| expr_needs_world(entity, a))
        }
        Expr::Closure(_, b) => expr_needs_world(entity, b),
        Expr::Match(s, arms) => {
            expr_needs_world(entity, s) || arms.iter().any(|a| expr_needs_world(entity, &a.body))
        }
        Expr::NamespacedCall { args, .. } => args.iter().any(|a| expr_needs_world(entity, a)),
        Expr::AddressOf {
            args, with_params, ..
        } => {
            args.iter().any(|a| expr_needs_world(entity, a))
                || with_params.iter().any(|(_, e)| expr_needs_world(entity, e))
        }
        Expr::Encode { value, .. } => expr_needs_world(entity, value),
        _ => false,
    }
}

fn gen_macro_ref(name: &str, args: &[Expr], ctx: &LeanExprCtx<'_>) -> String {
    let Some(mac) = ctx.entity.macros.iter().find(|m| m.name == name) else {
        return format!("(/- unknown macro `{name}` -/ Cambrian.Unsupported)");
    };
    let needs_world = expr_needs_world(ctx.entity, &mac.body);
    let world = if needs_world {
        match &ctx.world_var {
            Some(w) => format!("{w} "),
            None => {
                return "(/- TODO(P3): sys::balance outside World mode -/ Cambrian.Unsupported)"
                    .to_string();
            }
        }
    } else {
        String::new()
    };
    let inst = ctx.instance_var.as_deref().unwrap_or("inst");
    let mut call = format!(
        "{}.Local.macro_{} {}{} ctx {}",
        ctx.entity.name, name, world, ctx.state_var, inst
    );
    for a in args {
        call.push(' ');
        call.push_str(&gen_expr(a, ctx));
    }
    format!("({call})")
}

/// Map a `sys::<field>` alias to its `Cambrian.BlockEnv` field name and
/// Lean type, for World-mode block seeding (`{ w with block := { … } }`).
///
/// `sys::*` is not threaded through a `SysCtx` parameter in World mode —
/// route bodies read it straight off `w.block` (see [`gen_sys_field`]).
/// Seeding the initial world's `BlockEnv` is therefore the *only* way to
/// give `sys::*` a quantified (`∀`) or pinned starting value in specs.
/// `BlockEnv` carries every field as `BitVec 64` (note: its block-number
/// field is named `number`, not `blockNumber`). Returns `None` for
/// `sys::*` fields that don't live on `BlockEnv` (e.g. `balance`).
pub(crate) fn sys_block_field(field: &str) -> Option<(&'static str, &'static str)> {
    match field {
        "now" | "timestamp" => Some(("timestamp", "BitVec 64")),
        "block_number" | "blocknumber" | "blockNumber" | "number" => Some(("number", "BitVec 64")),
        "chainid" | "chain_id" | "chainId" => Some(("chainId", "BitVec 64")),
        _ => None,
    }
}

/// Build the RHS of a world re-bind that seeds `sys::balance` for *this*
/// contract instance. Delegates to [`LeanDomain::balance_world_update`].
pub(crate) fn sys_balance_world_update(
    world: &str,
    entity: &str,
    inst: &str,
    value: &str,
) -> String {
    domain().balance_world_update(world, entity, inst, value)
}

/// Lower a Cambrian byte-string literal (`b"\x19\x01"`) to a Lean
/// `Cambrian.Bytes`. Empty literal collapses to `Cambrian.Bytes.empty`.
fn gen_bytes_literal(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "Cambrian.Bytes.empty".to_string();
    }
    let parts: Vec<String> = bytes.iter().map(|b| format!("{}#8", b)).collect();
    format!("([{}] : Cambrian.Bytes)", parts.join(", "))
}

/// Lower a Cambrian `<ns>::<name>(args…)` namespaced call to a Lean
/// term. Only the `evm::*` intrinsics referenced by the in-repo
/// projects are handled here; anything else falls back to the legacy
/// `Cambrian.Unsupported` placeholder so Lean refuses to build at the
/// offending site.
/// Returns `true` iff `name` is one of the recognised SDK / target
/// namespaces (parsed by the grammar with the same `<ns>::<member>`
/// shape as enum-variant references). Used to disambiguate
/// `Expr::EnumVariantWithData` between an actual enum constructor
/// and a namespaced call (`evm::keccak256(args)`, `gosh::decode(...)`,
/// `msg::value`, …).
fn is_namespace(name: &str) -> bool {
    matches!(name, "evm" | "gosh" | "msg" | "sys" | "tvm" | "address_of")
}

/// Wrap `arg_term` in `Cambrian.castWidth` when `ty` is a fixed-width
/// bit-vector type the param needs. Cambrian's source language admits
/// implicit numeric widening across function-call boundaries; Lean's
/// `BitVec n` is monomorphic so we splice the cast in here.
///
/// True when `expr` is built only from numeric literals and arithmetic /
/// bitwise ops — i.e. Lean elaborates it as `Nat` without an expected type.
/// Used to inject `BitVec.ofNat` / ascriptions so `castWidth` / `.zeroExtend`
/// never see a bare `Nat` source (UPSTREAM B-7).
pub(crate) fn is_literal_nat_tree(expr: &Expr) -> bool {
    match expr {
        Expr::IntLiteral(_) => true,
        Expr::UnaryOp(UnaryOp::Neg, inner) => is_literal_nat_tree(inner),
        Expr::BinOp(lhs, op, rhs) => {
            matches!(
                op,
                BinOp::Add
                    | BinOp::Sub
                    | BinOp::Mul
                    | BinOp::Div
                    | BinOp::Mod
                    | BinOp::BitAnd
                    | BinOp::BitOr
                    | BinOp::BitXor
                    | BinOp::Shl
                    | BinOp::Shr
                    | BinOp::WrappingAdd
                    | BinOp::WrappingSub
                    | BinOp::WrappingMul
            ) && is_literal_nat_tree(lhs)
                && is_literal_nat_tree(rhs)
        }
        Expr::Cast(inner, _) => is_literal_nat_tree(inner),
        _ => false,
    }
}

fn is_bare_numeric_literal(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::IntLiteral(_)
    )
}

/// Literal divisor that cannot be zero — `/ 10_000` / `% 100` stay total
/// (Solidity 0.8 would not revert). Variable or zero literals still force
/// fail-mode `checkedDiv` (PW3-O-006).
fn expr_is_nonzero_int_literal(expr: &Expr) -> bool {
    match expr {
        Expr::IntLiteral(n) => *n != cambrian_core::U256::ZERO,
        Expr::Cast(inner, _) => expr_is_nonzero_int_literal(inner),
        _ => false,
    }
}

/// Coerce a lowered term to `BitVec width` with an explicit source width
/// whenever possible (B-7 / B-12). Avoids `castWidth N ?m` and
/// `.zeroExtend` on bare `Nat`.
pub(crate) fn coerce_term_to_width(
    expr: &Expr,
    term: &str,
    width: u32,
    ctx: &LeanExprCtx<'_>,
) -> String {
    if ctx.type_ctx.use_nat_numerics {
        return term.to_string();
    }
    if is_bare_numeric_literal(expr) {
        return format!("(({} : BitVec {}))", term, width);
    }
    if is_literal_nat_tree(expr) {
        if expr_is_signed(expr, ctx) {
            // Negated literal trees: inject via `ofInt` so the sign bit is kept.
            return format!("(BitVec.ofInt {} {})", width, term);
        }
        return format!("(BitVec.ofNat {} {})", width, term);
    }
    // UPSTREAM B-19: range-loop binders are `Nat` (from `List.range`).
    if is_nat_ident(expr, ctx) {
        return format!("(BitVec.ofNat {} {})", width, term);
    }
    if let Some(src) = bitvec_width(expr, ctx) {
        if src == width {
            return term.to_string();
        }
        if expr_is_signed(expr, ctx) && !source_is_unsigned(expr, ctx) && width > src {
            return format!("(({}).signExtend {})", term, width);
        }
        // Ascribe the source width so `castWidth`'s input metavar is
        // concrete (B-12 wave 3d). Unsigned widen / any narrow.
        return format!("(Cambrian.castWidth {} ({} : BitVec {}))", width, term, src);
    }
    // Unknown source width: still emit castWidth (historical); callers
    // that know a better strategy (ofNat / ascription) should use those
    // branches above.
    format!("(Cambrian.castWidth {} {})", width, term)
}

/// Lean width of a body built from typed operands: plain arithmetic promotes
/// to the widest operand, so `a + b` over `u8` params is `BitVec 8` even when
/// the member / `pure fn` result is `u16`.
pub(crate) fn arith_result_width(expr: &Expr, ctx: &LeanExprCtx<'_>) -> Option<u32> {
    arith_width(expr, ctx, true)
}

fn arith_width(expr: &Expr, ctx: &LeanExprCtx<'_>, top: bool) -> Option<u32> {
    match expr {
        Expr::BinOp(l, op, r)
            if matches!(
                op,
                BinOp::Add
                    | BinOp::Sub
                    | BinOp::Mul
                    | BinOp::Div
                    | BinOp::Mod
                    | BinOp::BitAnd
                    | BinOp::BitOr
                    | BinOp::BitXor
            ) =>
        {
            if let Some(w) = mixed_sign_widening_width(l, r, ctx) {
                return Some(w);
            }
            // Same call visibility as `operand_widths`.
            let (lw, rw) =
                with_call_widths(l, r, arith_width(l, ctx, false), arith_width(r, ctx, false), ctx);
            match (lw, rw) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) if matches!(**r, Expr::IntLiteral(_)) => Some(a),
                (None, Some(b)) if matches!(**l, Expr::IntLiteral(_)) => Some(b),
                _ => None,
            }
        }
        _ if top => bitvec_width(expr, ctx).or_else(|| pure_fn_call_width(expr, ctx)),
        _ => bitvec_width(expr, ctx),
    }
}

fn is_nat_ident(expr: &Expr, ctx: &LeanExprCtx<'_>) -> bool {
    matches!(expr, Expr::Ident(n) if ctx.nat_idents.contains(n))
}

pub(crate) fn coerce_arg(arg: &Expr, arg_term: &str, ty: &Type, ctx: &LeanExprCtx<'_>) -> String {
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
    match target_bits {
        Some(n) => coerce_term_to_width(arg, arg_term, n, ctx),
        None => arg_term.to_string(),
    }
}

fn gen_namespaced_call(
    namespace: &str,
    name: &str,
    args: &[Expr],
    ctx: &LeanExprCtx<'_>,
) -> String {
    let nat = ctx.type_ctx.use_nat_numerics;
    let arg_terms: Vec<String> = args.iter().map(|a| gen_expr(a, ctx)).collect();
    let arg_str = arg_terms.join(" ");
    let cam_std = |bitvec: &str, nat_name: &str| -> String {
        let sym = if nat { nat_name } else { bitvec };
        format!("(Cambrian.{} {})", sym, arg_terms.join(" "))
    };
    // `keccak256*` return a fixed-width `U256`; under `numerics: nat` their
    // result is a `U256` value (== `Nat`), so project it at the boundary. The
    // opaque hashes' chunk args are polymorphic, so `Nat` chunks pass through.
    let hashw = |call: String| {
        if nat {
            format!("({}).toNat", call)
        } else {
            call
        }
    };
    match (namespace, name) {
        ("evm", "keccak256") if args.len() == 1 => {
            hashw(format!("(Cambrian.keccak256 {})", arg_str))
        }
        ("evm", "keccak256Packed") => match args.len() {
            1 => hashw(format!("(Cambrian.keccak256Packed1 {})", arg_str)),
            2 => hashw(format!("(Cambrian.keccak256Packed2 {})", arg_str)),
            3 => hashw(format!("(Cambrian.keccak256Packed3 {})", arg_str)),
            4 => hashw(format!("(Cambrian.keccak256Packed4 {})", arg_str)),
            n => format!(
                "(/- TODO: evm::keccak256Packed with arity {} not modelled -/ Cambrian.Unsupported)",
                n,
            ),
        },
        ("evm", "ecrecover") if args.len() == 4 => {
            // `ecrecover (digest : U256) (v : BitVec 8) (r s : U256) : Address`.
            // Under `numerics: nat` the numeric args arrive as `Nat`, so inject
            // them into their fixed widths; the `Address` result stays fixed.
            if nat {
                format!(
                    "(Cambrian.ecrecover (BitVec.ofNat 256 ({})) (BitVec.ofNat 8 ({})) (BitVec.ofNat 256 ({})) (BitVec.ofNat 256 ({})))",
                    arg_terms[0], arg_terms[1], arg_terms[2], arg_terms[3],
                )
            } else {
                format!("(Cambrian.ecrecover {})", arg_str)
            }
        }
        ("std::math", "min") if args.len() == 2 => {
            let signed = expr_is_signed(&args[0], ctx) || expr_is_signed(&args[1], ctx);
            if signed && !nat {
                format!(
                    "(if ({}.slt {}) then {} else {})",
                    arg_terms[0], arg_terms[1], arg_terms[0], arg_terms[1]
                )
            } else {
                format!(
                    "(if {} < {} then {} else {})",
                    arg_terms[0], arg_terms[1], arg_terms[0], arg_terms[1]
                )
            }
        }
        ("std::math", "max") if args.len() == 2 => {
            let signed = expr_is_signed(&args[0], ctx) || expr_is_signed(&args[1], ctx);
            if signed && !nat {
                format!(
                    "(if ({}.slt {}) then {} else {})",
                    arg_terms[1], arg_terms[0], arg_terms[0], arg_terms[1]
                )
            } else {
                format!(
                    "(if {} > {} then {} else {})",
                    arg_terms[0], arg_terms[1], arg_terms[0], arg_terms[1]
                )
            }
        }
        ("std::math", "abs") if args.len() == 1 => {
            let signed = expr_is_signed(&args[0], ctx);
            let sym = if nat {
                if signed {
                    "absInt"
                } else {
                    "absNat"
                }
            } else if signed {
                "abs"
            } else {
                "absU"
            };
            format!("(Cambrian.{} {})", sym, arg_terms[0])
        }
        ("std::math", "clamp") if args.len() == 3 => {
            let signed = expr_is_signed(&args[0], ctx)
                || expr_is_signed(&args[1], ctx)
                || expr_is_signed(&args[2], ctx);
            let sym = if nat {
                if signed {
                    "clampInt"
                } else {
                    "clampNat"
                }
            } else if signed {
                "clampS"
            } else {
                "clamp"
            };
            format!("(Cambrian.{} {})", sym, arg_terms.join(" "))
        }
        ("std::math", "muldiv") if args.len() == 3 => cam_std("muldiv", "muldivNat"),
        ("std::math", "sign") if args.len() == 1 => {
            let signed = expr_is_signed(&args[0], ctx);
            let sym = if nat {
                if signed {
                    "signInt"
                } else {
                    "signNat"
                }
            } else if signed {
                "sign"
            } else {
                "signU"
            };
            format!("(Cambrian.{} {})", sym, arg_terms[0])
        }
        ("std::math", "divmod") if args.len() == 2 => cam_std("divmod", "divmodNat"),
        ("std::math", "divc") if args.len() == 2 => cam_std("divc", "divcNat"),
        ("std::math", "divr") if args.len() == 2 => cam_std("divr", "divrNat"),
        ("std::math", "minmax") if args.len() == 2 => cam_std("minmax", "minmaxNat"),
        ("std::math", "modpow2") if args.len() == 2 => cam_std("modpow2", "modpow2Nat"),
        ("std::math", "pow") if args.len() == 2 => cam_std("pow", "powNat"),
        ("std::str", name) if args.len() == 2 && crate::codegen::std_str::parse_str_meta(name).is_some() => {
            let meta = crate::codegen::std_str::parse_str_meta(name).expect("parse_str_meta");
            let lean_fn = if meta.signed {
                "parseRadixSignedNat?"
            } else {
                "parseRadixNat?"
            };
            // `parseRadixNat?` / `parseRadixSignedNat?` take `radix : Nat`.
            // Under BitVec numerics the ambient expected width (from the
            // parse return type) ascribes the radix literal as
            // `(10 : BitVec W)`, which does not unify with `Nat`. Clear the
            // ascription for literal/Nat trees; coerce other BitVec terms.
            let radix = if nat {
                arg_terms[1].clone()
            } else if is_bare_numeric_literal(&args[1]) || is_literal_nat_tree(&args[1]) {
                let mut nat_ctx = dup_expr_ctx(ctx);
                nat_ctx.expected_bitvec_width = None;
                gen_expr(&args[1], &nat_ctx)
            } else {
                format!("({}).toNat", arg_terms[1])
            };
            let core = format!(
                "(Cambrian.{} {} {} {})",
                lean_fn,
                arg_terms[0],
                radix,
                meta.bits
            );
            let inject = if meta.signed {
                format!("Cambrian.signedNatToBitVec {}", meta.bits)
            } else {
                format!("BitVec.ofNat {}", meta.bits)
            };
            if nat {
                core
            } else if ctx.type_ctx.use_predictable_profile {
                // Escrow (campaign defect L5): the legacy `match` on the
                // parser's `Option` mints `<decl>.match_1` (`Option.casesOn`
                // under a matcher wrapper) straight into the digest zone. Emit
                // the eliminator explicitly instead. As with the zone-enum
                // `match` (`try_enum_cases_on`) the RESULT is ascribed rather
                // than the motive spelled: both make the eliminator elaborate,
                // but a spelled motive leaves an enclosing binder's type as the
                // unreduced `(fun _ => <ty>) <scrut>` (commit fb91f23).
                format!(
                    "((Option.casesOn {} Option.none (fun v => Option.some ({} v))) : Option (BitVec {}))",
                    core, inject, meta.bits
                )
            } else if meta.signed {
                format!(
                    "(match {} with | Option.some v => Option.some (Cambrian.signedNatToBitVec {} v) | Option.none => Option.none)",
                    core, meta.bits
                )
            } else {
                format!(
                    "(match {} with | Option.some v => Option.some (BitVec.ofNat {} v) | Option.none => Option.none)",
                    core, meta.bits
                )
            }
        }
        ("std::str", "format") if args.len() == 2 => {
            let fmt = &arg_terms[0];
            let val = &args[1];
            if nat || is_bare_numeric_literal(val) || is_literal_nat_tree(val) {
                format!("(Cambrian.formatNat {} {})", fmt, arg_terms[1])
            } else {
                let a = format!("(Cambrian.castWidth 64 {})", arg_terms[1]);
                format!("(Cambrian.format {} {})", fmt, a)
            }
        }
        ("std::str", "format") if args.len() == 1 => {
            format!("(Cambrian.formatNat {})", arg_terms[0])
        }
        ("std::crypto", _) => {
            format!(
                "(/- std::crypto::{} has no Lean lowering -/ Cambrian.Unsupported)",
                name
            )
        }
        _ => {
            if let Some(lib) = ctx
                .type_ctx
                .program
                .libraries
                .iter()
                .find(|l| l.name == namespace)
            {
                if lib.pure_fns.iter().any(|f| f.name == name) {
                    let sym = super::core::pure::library_pure_lean_symbol(namespace, name);
                    if arg_terms.is_empty() {
                        return format!("(Cambrian.Generated.Pure.{sym})");
                    }
                    return format!("(Cambrian.Generated.Pure.{sym} {})", arg_terms.join(" "));
                }
            }
            format!(
                "(/- {}::{} has no Lean lowering -/ Cambrian.Unsupported)",
                namespace, name,
            )
        }
    }
}

/// True when `Lib::fn` resolves to a library-scoped `pure fn` emitted in
/// `Cambrian.Generated.Pure`.
pub(crate) fn lean_library_pure_call_supported(
    program: &crate::ast::Program,
    namespace: &str,
    name: &str,
    arity: usize,
) -> bool {
    program.libraries.iter().any(|lib| {
        lib.name == namespace
            && lib
                .pure_fns
                .iter()
                .any(|f| f.name == name && f.params.len() == arity)
    })
}

/// True when `namespace::name` with the given arity has a Lean lowering in
/// [`gen_namespaced_call`]. Used by validator **L16** so unsupported calls
/// fail at transpile time instead of emitting `Cambrian.Unsupported` into a
/// value slot (UPSTREAM B-33). `std::crypto::*` is rejected separately as L13.
pub(crate) fn lean_namespaced_call_supported(namespace: &str, name: &str, arity: usize) -> bool {
    match (namespace, name) {
        ("evm", "keccak256") => arity == 1,
        ("evm", "keccak256Packed") => matches!(arity, 1 | 2 | 3 | 4),
        ("evm", "ecrecover") => arity == 4,
        (
            "std::math",
            "min" | "max" | "divmod" | "divc" | "divr" | "minmax" | "modpow2" | "pow",
        ) => arity == 2,
        ("std::math", "abs" | "sign") => arity == 1,
        ("std::math", "clamp" | "muldiv") => arity == 3,
        // `muldivmod` has an EVM lowering but no Lean arm.
        ("std::math", _) => false,
        ("std::str", "format") => matches!(arity, 1 | 2),
        ("std::str", n) => arity == 2 && crate::codegen::std_str::parse_str_meta(n).is_some(),
        ("std::crypto", _) => false,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Operators
// ---------------------------------------------------------------------------

fn lean_operand_shell(expr: &Expr) -> TypedExpr {
    TypedExpr {
        ty: ResolvedType::simple("unknown"),
        kind: TypedExprKind::AstPassthrough(Box::new(expr.clone())),
    }
}

fn lean_wrap_width_cast(te: TypedExpr, width: u32) -> TypedExpr {
    let ty = ResolvedType::Simple(format!("BitVec{}", width));
    TypedExpr {
        ty: ty.clone(),
        kind: TypedExprKind::Coerce {
            kind: CoerceKind::WidthCast,
            expr: Box::new(te),
            to: ty,
        },
    }
}

/// Lean-specific width coercion on binop operands (P6 C3 residual).
///
/// Portable [`crate::ir::coerce_binop_operands`] targets Rust CamCast rules;
/// this delegates printing to [`coerce_term_to_width`] so goldens stay byte-identical.
pub(crate) fn print_coerced_lean_operand(
    te: &TypedExpr,
    rendered: &str,
    expr: &Expr,
    ctx: &LeanExprCtx<'_>,
) -> String {
    if let TypedExprKind::Coerce {
        kind: CoerceKind::WidthCast,
        to,
        ..
    } = &te.kind
    {
        if let ResolvedType::Simple(name) = to {
            if let Some(w) = name
                .strip_prefix("BitVec")
                .and_then(|s| s.parse::<u32>().ok())
            {
                return coerce_term_to_width(expr, rendered, w, ctx);
            }
        }
    }
    rendered.to_string()
}

fn lean_coerce_operand_to_width(
    expr: &Expr,
    rendered: &str,
    width: u32,
    ctx: &LeanExprCtx<'_>,
) -> String {
    let te = lean_wrap_width_cast(lean_operand_shell(expr), width);
    print_coerced_lean_operand(&te, rendered, expr, ctx)
}

/// Mixed-width binop operand: a `pure fn` call is cast from its declared
/// return type; any other operand keeps the historical shell coercion.
fn coerce_binop_operand(expr: &Expr, rendered: &str, width: u32, ctx: &LeanExprCtx<'_>) -> String {
    if bitvec_width(expr, ctx).is_none() && pure_fn_call_width(expr, ctx).is_some() {
        return coerce_result_to_width(expr, rendered, width, ctx);
    }
    lean_coerce_operand_to_width(expr, rendered, width, ctx)
}

/// `LANGUAGE.md` widening rule 2: a signed and an unsigned operand meet at a
/// signed width of `max(2 × unsigned, signed)`. `None` unless both widths are
/// known and the signs differ (rule 3 pairs are rejected by V69).
pub(crate) fn mixed_sign_widening_width(lhs: &Expr, rhs: &Expr, ctx: &LeanExprCtx<'_>) -> Option<u32> {
    let mut sign_ctx = dup_expr_ctx(ctx);
    sign_ctx.expected_signed = None;
    let (lw, rw) = (bitvec_width(lhs, ctx)?, bitvec_width(rhs, ctx)?);
    let (ls, rs) = (expr_is_signed(lhs, &sign_ctx), expr_is_signed(rhs, &sign_ctx));
    if ls == rs {
        return None;
    }
    let (uw, sw) = if ls { (rw, lw) } else { (lw, rw) };
    Some((2 * uw).max(sw).min(256))
}

/// EVM `SignPromote` for mixed-sign compares: unsigned operands widen via
/// `zeroExtend`, signed via `signExtend` / `castWidth` (T-ARCH-017).
fn lean_promote_mixed_sign_compare_operand(
    expr: &Expr,
    rendered: &str,
    width: u32,
    signed: bool,
    ctx: &LeanExprCtx<'_>,
) -> String {
    if signed {
        coerce_term_to_width(expr, rendered, width, ctx)
    } else if let Some(src) = bitvec_width(expr, ctx) {
        if src == width {
            rendered.to_string()
        } else {
            format!("(({}).zeroExtend {})", rendered, width)
        }
    } else {
        coerce_term_to_width(expr, rendered, width, ctx)
    }
}

/// True when `e` contains `/` or `%`, or a `std::math` call that panics on
/// a zero divisor (`divc`/`divr`/`divmod`/`muldiv`/`muldivmod`).
pub fn expr_has_div0_op(e: &Expr) -> bool {
    match e {
        Expr::BinOp(lhs, op, rhs) => {
            (matches!(op, BinOp::Div | BinOp::Mod) && !expr_is_nonzero_int_literal(rhs))
                || expr_has_div0_op(lhs)
                || expr_has_div0_op(rhs)
        }
        Expr::NamespacedCall {
            namespace, name, args: _, ..
        } if namespace == "std::math"
            && matches!(
                name.as_str(),
                "divc" | "divr" | "divmod" | "muldiv" | "muldivmod"
            ) =>
        {
            true
        }
        Expr::UnaryOp(_, expr) => expr_has_div0_op(expr),
        Expr::FnCall(_, args) | Expr::EnumVariantWithData(_, _, args) => {
            args.iter().any(expr_has_div0_op)
        }
        Expr::MethodCall(recv, _, args) => {
            expr_has_div0_op(recv) || args.iter().any(expr_has_div0_op)
        }
        Expr::Index(base, index) => expr_has_div0_op(base) || expr_has_div0_op(index),
        Expr::FieldAccess(base, _) => expr_has_div0_op(base),
        Expr::If(condition, then_branch, else_branch) => {
            expr_has_div0_op(condition)
                || expr_has_div0_op(then_branch)
                || else_branch.as_ref().is_some_and(|e| expr_has_div0_op(e))
        }
        Expr::Match(scrutinee, arms) => {
            expr_has_div0_op(scrutinee) || arms.iter().any(|a| expr_has_div0_op(&a.body))
        }
        Expr::Let(_, value, body) => expr_has_div0_op(value) || expr_has_div0_op(body),
        Expr::Block(stmts) => stmts.iter().any(expr_has_div0_op),
        Expr::Tuple(items) | Expr::ArrayLit(items) => items.iter().any(expr_has_div0_op),
        Expr::RecordConstruct(_, fields) => fields.iter().any(|(_, v)| expr_has_div0_op(v)),
        Expr::RecordUpdate(base, fields) => {
            expr_has_div0_op(base) || fields.iter().any(|(_, v)| expr_has_div0_op(v))
        }
        Expr::Cast(expr, _) | Expr::Some(expr) | Expr::Closure(_, expr) => expr_has_div0_op(expr),
        Expr::Range(a, b) | Expr::For(_, a, b) => expr_has_div0_op(a) || expr_has_div0_op(b),
        Expr::MacroRef(_, args) | Expr::NamespacedCall { args, .. } => {
            args.iter().any(expr_has_div0_op)
        }
        Expr::AddressOf {
            args, with_params, ..
        } => {
            args.iter().any(expr_has_div0_op)
                || with_params.iter().any(|(_, v)| expr_has_div0_op(v))
        }
        Expr::Encode { value, .. } => expr_has_div0_op(value),
        _ => false,
    }
}

/// True when `e` contains a narrowing `as uN`/`as iN` of a non-literal
/// (PW3-O-007). Literals are treated as in-range.
pub fn expr_has_narrow_cast(e: &Expr) -> bool {
    match e {
        Expr::Cast(inner, ty) if is_narrow_int_target(ty) && !is_bare_numeric_literal(inner) => true,
        Expr::Cast(inner, _) => expr_has_narrow_cast(inner),
        Expr::BinOp(lhs, _, rhs) => expr_has_narrow_cast(lhs) || expr_has_narrow_cast(rhs),
        Expr::UnaryOp(_, expr) => expr_has_narrow_cast(expr),
        Expr::FnCall(_, args) | Expr::EnumVariantWithData(_, _, args) => {
            args.iter().any(expr_has_narrow_cast)
        }
        Expr::MethodCall(recv, _, args) => {
            expr_has_narrow_cast(recv) || args.iter().any(expr_has_narrow_cast)
        }
        Expr::Index(base, index) => expr_has_narrow_cast(base) || expr_has_narrow_cast(index),
        Expr::FieldAccess(base, _) => expr_has_narrow_cast(base),
        Expr::If(condition, then_branch, else_branch) => {
            expr_has_narrow_cast(condition)
                || expr_has_narrow_cast(then_branch)
                || else_branch.as_ref().is_some_and(|e| expr_has_narrow_cast(e))
        }
        Expr::Match(scrutinee, arms) => {
            expr_has_narrow_cast(scrutinee) || arms.iter().any(|a| expr_has_narrow_cast(&a.body))
        }
        Expr::Let(_, value, body) => expr_has_narrow_cast(value) || expr_has_narrow_cast(body),
        Expr::Block(stmts) => stmts.iter().any(expr_has_narrow_cast),
        Expr::Tuple(items) | Expr::ArrayLit(items) => items.iter().any(expr_has_narrow_cast),
        Expr::RecordConstruct(_, fields) => fields.iter().any(|(_, v)| expr_has_narrow_cast(v)),
        Expr::RecordUpdate(base, fields) => {
            expr_has_narrow_cast(base) || fields.iter().any(|(_, v)| expr_has_narrow_cast(v))
        }
        Expr::Some(expr) | Expr::Closure(_, expr) => expr_has_narrow_cast(expr),
        Expr::Range(a, b) | Expr::For(_, a, b) => {
            expr_has_narrow_cast(a) || expr_has_narrow_cast(b)
        }
        Expr::MacroRef(_, args) | Expr::NamespacedCall { args, .. } => {
            args.iter().any(expr_has_narrow_cast)
        }
        Expr::AddressOf {
            args, with_params, ..
        } => {
            args.iter().any(expr_has_narrow_cast)
                || with_params.iter().any(|(_, v)| expr_has_narrow_cast(v))
        }
        Expr::Encode { value, .. } => expr_has_narrow_cast(value),
        _ => false,
    }
}

fn is_narrow_int_target(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Simple(n) if matches!(
            n.as_str(),
            "u8" | "u16" | "u32" | "u64" | "u128"
                | "i8" | "i16" | "i32" | "i64" | "i128"
                | "uint8" | "uint16" | "uint32" | "uint64" | "uint128"
                | "int8" | "int16" | "int32" | "int64" | "int128"
        )
    )
}

/// True when `e` contains a checked-sensitive binop (`+`/`-`/`*`).
/// Under `lean.numerics: overflow-panic` those lower to `Cambrian.checked*`.
/// True when `e` indexes a `Vec` member (`m_x[i]`), not a `HashMap` slot.
pub fn expr_has_vec_list_index(e: &Expr, entity: &Entity) -> bool {
    fn is_vec_member_base(base: &Expr, entity: &Entity) -> bool {
        if let Expr::Ident(n) = base {
            return entity.members.iter().any(|m| {
                m.name == *n && matches!(&m.ty, Type::Generic(name, _) if name == "Vec")
            });
        }
        false
    }
    fn rec(e: &Expr, entity: &Entity) -> bool {
        match e {
            Expr::Index(base, _) if is_vec_member_base(base, entity) => true,
            Expr::BinOp(l, _, r) => rec(l, entity) || rec(r, entity),
            Expr::Cast(inner, _) | Expr::Some(inner) => rec(inner, entity),
            Expr::If(c, t, el) => {
                rec(c, entity)
                    || rec(t, entity)
                    || el.as_ref().is_some_and(|e| rec(e, entity))
            }
            Expr::Let(_, v, b) => rec(v, entity) || rec(b, entity),
            Expr::Match(_, arms) => arms.iter().any(|arm| rec(&arm.body, entity)),
            Expr::FnCall(_, args) | Expr::MethodCall(_, _, args) => {
                args.iter().any(|a| rec(a, entity))
            }
            _ => false,
        }
    }
    rec(e, entity)
}

fn gen_list_index_route_result(base: &Expr, idx: &Expr, ctx: &LeanExprCtx<'_>) -> String {
    let idx_term = if ctx.type_ctx.use_nat_numerics {
        format!("({})", gen_expr(idx, ctx))
    } else {
        format!("({}).toNat", gen_expr(idx, ctx))
    };
    format!(
        "(Cambrian.checkedListGet ({}) {})",
        gen_expr(base, ctx),
        idx_term,
    )
}

pub fn expr_has_checked_binop(e: &Expr) -> bool {
    match e {
        Expr::BinOp(lhs, op, rhs) => {
            matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul)
                || expr_has_checked_binop(lhs)
                || expr_has_checked_binop(rhs)
        }
        Expr::UnaryOp(_, expr) => expr_has_checked_binop(expr),
        Expr::FnCall(_, args) | Expr::EnumVariantWithData(_, _, args) => {
            args.iter().any(expr_has_checked_binop)
        }
        Expr::MethodCall(recv, _, args) => {
            expr_has_checked_binop(recv) || args.iter().any(expr_has_checked_binop)
        }
        Expr::Index(base, index) => expr_has_checked_binop(base) || expr_has_checked_binop(index),
        Expr::FieldAccess(base, _) => expr_has_checked_binop(base),
        Expr::If(condition, then_branch, else_branch) => {
            expr_has_checked_binop(condition)
                || expr_has_checked_binop(then_branch)
                || else_branch
                    .as_ref()
                    .is_some_and(|e| expr_has_checked_binop(e))
        }
        Expr::Match(scrutinee, arms) => {
            expr_has_checked_binop(scrutinee) || arms.iter().any(|a| expr_has_checked_binop(&a.body))
        }
        Expr::Let(_, value, body) => expr_has_checked_binop(value) || expr_has_checked_binop(body),
        Expr::Block(stmts) => stmts.iter().any(expr_has_checked_binop),
        Expr::Tuple(items) | Expr::ArrayLit(items) => items.iter().any(expr_has_checked_binop),
        Expr::RecordConstruct(_, fields) => fields.iter().any(|(_, v)| expr_has_checked_binop(v)),
        Expr::RecordUpdate(base, fields) => {
            expr_has_checked_binop(base) || fields.iter().any(|(_, v)| expr_has_checked_binop(v))
        }
        Expr::Cast(expr, _) | Expr::Some(expr) | Expr::Closure(_, expr) => {
            expr_has_checked_binop(expr)
        }
        Expr::Range(a, b) | Expr::For(_, a, b) => {
            expr_has_checked_binop(a) || expr_has_checked_binop(b)
        }
        Expr::MacroRef(_, args) | Expr::NamespacedCall { args, .. } => {
            args.iter().any(expr_has_checked_binop)
        }
        Expr::AddressOf {
            args, with_params, ..
        } => {
            args.iter().any(expr_has_checked_binop)
                || with_params.iter().any(|(_, v)| expr_has_checked_binop(v))
        }
        Expr::Encode { value, .. } => expr_has_checked_binop(value),
        _ => false,
    }
}

/// Lower `e` to a Lean term of type `RouteResult T`.
/// Checked `+`/`-`/`*` (overflow-panic), `/`/`%`/`std::math::divc` (always),
/// and narrowing `as uN` become `Cambrian.checked*` fail surfaces.
pub fn gen_expr_as_route_result(expr: &Expr, ctx: &LeanExprCtx<'_>) -> String {
    let overflow = ctx.type_ctx.overflow_panic && !ctx.type_ctx.use_nat_numerics;
    let nat = ctx.type_ctx.use_nat_numerics;
    match expr {
        Expr::BinOp(lhs, op, rhs)
            if overflow && matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) =>
        {
            let signed = expr_is_signed(lhs, ctx) || expr_is_signed(rhs, ctx);
            let checked = match (op, signed) {
                (BinOp::Add, true) => "checkedSAdd",
                (BinOp::Sub, true) => "checkedSSub",
                (BinOp::Mul, true) => "checkedSMul",
                (BinOp::Add, false) => "checkedAdd",
                (BinOp::Sub, false) => "checkedSub",
                (BinOp::Mul, false) => "checkedMul",
                _ => unreachable!(),
            };
            bind_checked_binop(lhs, rhs, checked, ctx)
        }
        Expr::BinOp(lhs, op, rhs)
            if !nat
                && matches!(op, BinOp::Div | BinOp::Mod)
                && !expr_is_nonzero_int_literal(rhs) =>
        {
            let signed = expr_is_signed(lhs, ctx) || expr_is_signed(rhs, ctx);
            let checked = match (op, signed) {
                (BinOp::Div, true) => "checkedSDiv",
                (BinOp::Mod, true) => "checkedSRem",
                (BinOp::Div, false) => "checkedDiv",
                (BinOp::Mod, false) => "checkedMod",
                _ => unreachable!(),
            };
            bind_checked_binop(lhs, rhs, checked, ctx)
        }
        Expr::NamespacedCall {
            namespace,
            name,
            args,
            ..
        } if !nat
            && namespace == "std::math"
            && matches!(
                name.as_str(),
                "divc" | "divr" | "divmod" | "muldiv" | "muldivmod"
            ) =>
        {
            let checked = match name.as_str() {
                "divc" => "checkedDivc",
                "divr" => "checkedDivr",
                "divmod" => "checkedDivmod",
                "muldiv" | "muldivmod" => "checkedMuldiv",
                _ => unreachable!(),
            };
            bind_checked_call(args, checked, ctx)
        }
        Expr::Cast(inner, ty) if !nat && is_narrow_int_target(ty) && !is_bare_numeric_literal(inner)
        => {
            let bits = bitvec_width_of_type(ty, &ctx.type_ctx).unwrap_or(64);
            // Widen / same-width cannot overflow (Solidity 0.8). Signed
            // uses `signExtend` (B-34); unsigned uses `castWidth`. Only
            // actual narrowing panics 0x11 via `checkedCastWidth`.
            if bitvec_width(inner, ctx).is_some_and(|src| src <= bits) {
                let term = gen_cast(inner, ty, ctx);
                format!("(pure {term})")
            } else {
                // Compute the inner at U256 (Solidity mixed-arith promotion),
                // then panic 0x11 if the value does not fit in `ty`.
                let mut inner_ctx = dup_expr_ctx(ctx);
                inner_ctx.expected_bitvec_width = Some(256);
                let inner_rr = gen_expr_as_route_result(inner, &inner_ctx);
                if let Some(inner_term) = strip_pure_wrap(&inner_rr) {
                    format!("(Cambrian.checkedCastWidth {} {})", bits, inner_term)
                } else {
                    format!("({inner_rr} >>= fun __c => Cambrian.checkedCastWidth {bits} __c)")
                }
            }
        }
        Expr::FnCall(name, args) if ctx.pure_fns.iter().any(|f| f.name == *name && pure_fn_forces_fail(f, ctx.pure_fns)) =>
        {
            bind_pure_fn_call(name, args, ctx)
        }
        Expr::FnCall(name, args)
            if ctx.pure_fns.iter().any(|f| f.name == *name && !pure_fn_forces_fail(f, ctx.pure_fns)) =>
        {
            bind_total_pure_fn_call(name, args, ctx)
        }
        Expr::NamespacedCall {
            namespace,
            name,
            args,
            ..
        } if namespace == "std::math" && matches!(name.as_str(), "min" | "max") && args.len() == 2 =>
        {
            bind_std_math_minmax_route_result(name, args, ctx)
        }
        Expr::BinOp(lhs, op, rhs)
            if expr_forces_fail_surface(lhs, ctx.pure_fns)
                || expr_forces_fail_surface(rhs, ctx.pure_fns) =>
        {
            bind_total_binop(lhs, op, rhs, ctx)
        }
        Expr::MethodCall(base, method, args)
            if method == "fold" && args.len() == 2 =>
        {
            if let Some(term) =
                super::core::iter::try_gen_fold_method_fail(base, &args[0], &args[1], ctx)
            {
                term
            } else {
                format!("(pure {})", gen_expr(expr, ctx))
            }
        }
        Expr::If(condition, then_branch, Some(else_branch)) => {
            let c = gen_expr(condition, ctx);
            let t = gen_expr_as_route_result(then_branch, ctx);
            let e = gen_expr_as_route_result(else_branch, ctx);
            format!("(if {c} then {t} else {e})")
        }
        Expr::Let(pat, value, body) => {
            let name = pattern_to_lean(pat);
            let v = gen_expr_as_route_result(value, ctx);
            // Same map-binder tracking as `gen_let`: fail-mode `>>=`
            // otherwise forgets `let inner = m[k]` and nested
            // `inner.exists` / `inner.update` fall through to
            // `Cambrian.Unsupported` (contracts/dex.cam).
            let mut body_ctx = dup_expr_ctx(ctx);
            body_ctx.push_pattern(pat);
            if let Pattern::Ident(n) = pat {
                if map_type_of(value, ctx).is_some() {
                    body_ctx.hashmap_idents.insert(n.clone());
                }
            }
            let b = gen_expr_as_route_result(body, &body_ctx);
            if let Some(vt) = strip_pure_wrap(&v) {
                format!("(let {name} := {vt}; {b})")
            } else {
                format!("({v} >>= fun {name} => {b})")
            }
        }
        Expr::RecordUpdate(base, fields)
            if expr_forces_fail_surface(base, ctx.pure_fns)
                || fields
                    .iter()
                    .any(|(_, e)| expr_forces_fail_surface(e, ctx.pure_fns)) =>
        {
            bind_record_update_fail(base, fields, ctx)
        }
        Expr::Match(scrutinee, arms) => gen_match_as_route_result(scrutinee, arms, ctx),
        Expr::Index(base, idx) if try_gen_map_index(base, idx, ctx).is_none() => {
            gen_list_index_route_result(base, idx, ctx)
        }
        _ => format!("(pure {})", gen_expr(expr, ctx)),
    }
}

/// Lean projection suffix for a Cambrian tuple index, or `None` for a
/// record field.
///
/// Cambrian indexes tuples from zero; Lean's `Prod` projections start at one
/// and only ever run `.1`/`.2`, because `α × β × γ` is `α × (β × γ)`. So the
/// index walks the right spine: `.2` per step, `.1` to take the head.
/// Emitting `.0` verbatim was a parse error, which is how this surfaced —
/// a Uniswap pair's binary-search `fold(...).0` failed `lake build` while
/// the same source transpiled and ran green on EVM.
///
/// The tuple's arity is not known at this point. `.2`-per-step is exact for
/// pairs at both indices and for the last component of any tuple; a middle
/// component of a wider tuple lands on a `Prod` where a scalar is expected
/// and fails the build, which is the outcome to prefer over a silent one.
fn lean_tuple_projection(field: &str) -> Option<String> {
    let index: usize = field.parse().ok()?;
    Some(if index == 0 {
        ".1".to_string()
    } else {
        ".2".repeat(index)
    })
}

pub(crate) fn pure_fn_forces_fail(f: &PureFn, pures: &[PureFn]) -> bool {
    expr_forces_fail_surface(&f.body, pures)
}

/// True when `e` itself, or a `pure fn` it (transitively) calls, is a fail
/// surface (`/`, narrowing `as`, `std::math::divc`, …). Uses TLS
/// `use_nat_numerics()` — prefer [`expr_forces_fail_surface_with_nat`] for
/// external mirrors (T-ARCH-020 / RV-2).
pub fn expr_forces_fail_surface(e: &Expr, pures: &[PureFn]) -> bool {
    expr_forces_fail_surface_with_nat(e, pures, super::use_nat_numerics())
}

/// Like [`expr_forces_fail_surface`], but `nat_numerics` comes from the project
/// under analysis instead of TLS.
pub fn expr_forces_fail_surface_with_nat(
    e: &Expr,
    pures: &[PureFn],
    nat_numerics: bool,
) -> bool {
    fn rec(
        e: &Expr,
        pures: &[PureFn],
        nat_numerics: bool,
        stack: &mut HashSet<String>,
    ) -> bool {
        if !nat_numerics && (expr_has_div0_op(e) || expr_has_narrow_cast(e)) {
            return true;
        }
        match e {
            Expr::FnCall(name, args) => {
                let callee_fail = pures.iter().any(|f| {
                    if f.name != *name {
                        return false;
                    }
                    if !stack.insert(f.name.clone()) {
                        return false;
                    }
                    let r = rec(&f.body, pures, nat_numerics, stack);
                    stack.remove(&f.name);
                    r
                });
                callee_fail || args.iter().any(|a| rec(a, pures, nat_numerics, stack))
            }
            Expr::MethodCall(recv, _, args) => {
                rec(recv, pures, nat_numerics, stack)
                    || args.iter().any(|a| rec(a, pures, nat_numerics, stack))
            }
            Expr::BinOp(l, _, r) => {
                rec(l, pures, nat_numerics, stack) || rec(r, pures, nat_numerics, stack)
            }
            Expr::Cast(inner, _) | Expr::Some(inner) | Expr::Closure(_, inner) => {
                rec(inner, pures, nat_numerics, stack)
            }
            Expr::If(c, t, el) => {
                rec(c, pures, nat_numerics, stack)
                    || rec(t, pures, nat_numerics, stack)
                    || el
                        .as_ref()
                        .is_some_and(|e| rec(e, pures, nat_numerics, stack))
            }
            Expr::Let(_, v, b) => {
                rec(v, pures, nat_numerics, stack) || rec(b, pures, nat_numerics, stack)
            }
            Expr::Match(_, arms) => arms
                .iter()
                .any(|arm| rec(&arm.body, pures, nat_numerics, stack)),
            Expr::Block(stmts) | Expr::Tuple(stmts) | Expr::ArrayLit(stmts) => {
                stmts.iter().any(|s| rec(s, pures, nat_numerics, stack))
            }
            Expr::RecordUpdate(base, fields) => {
                rec(base, pures, nat_numerics, stack)
                    || fields
                        .iter()
                        .any(|(_, e)| rec(e, pures, nat_numerics, stack))
            }
            Expr::RecordConstruct(_, fields) => fields
                .iter()
                .any(|(_, e)| rec(e, pures, nat_numerics, stack)),
            Expr::NamespacedCall { args, .. } => {
                args.iter().any(|a| rec(a, pures, nat_numerics, stack))
            }
            _ => false,
        }
    }
    rec(e, pures, nat_numerics, &mut HashSet::new())
}

fn bind_total_binop(lhs: &Expr, op: &BinOp, rhs: &Expr, ctx: &LeanExprCtx<'_>) -> String {
    let l = gen_expr_as_route_result(lhs, ctx);
    let r = gen_expr_as_route_result(rhs, ctx);
    let op_str = match op {
        BinOp::Add | BinOp::WrappingAdd => "+",
        BinOp::Sub | BinOp::WrappingSub => "-",
        BinOp::Mul | BinOp::WrappingMul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::BitAnd => "&&&",
        BinOp::BitOr => "|||",
        BinOp::BitXor => "^^^",
        BinOp::Shl => "<<<",
        BinOp::Shr => ">>>",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "≤",
        BinOp::Gt => ">",
        BinOp::Ge => "≥",
        BinOp::And => "&&",
        BinOp::Or => "||",
    };
    match (strip_pure_wrap(&l), strip_pure_wrap(&r)) {
        (Some(l), Some(r)) => format!("(pure ({l} {op_str} {r}))"),
        (Some(l), None) => format!("({r} >>= fun __r => pure ({l} {op_str} __r))"),
        (None, Some(r)) => format!("({l} >>= fun __l => pure (__l {op_str} {r}))"),
        (None, None) => format!("({l} >>= fun __l => {r} >>= fun __r => pure (__l {op_str} __r))"),
    }
}

fn bind_record_update_fail(
    base: &Expr,
    fields: &[(String, Expr)],
    ctx: &LeanExprCtx<'_>,
) -> String {
    let base_rr = gen_expr_as_route_result(base, ctx);
    let field_rrs: Vec<(String, String)> = fields
        .iter()
        .map(|(n, e)| (n.clone(), gen_expr_as_route_result(e, ctx)))
        .collect();
    let mut binds: Vec<(String, String)> = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    for (i, (name, rr)) in field_rrs.iter().enumerate() {
        if let Some(t) = strip_pure_wrap(rr) {
            parts.push(format!("{name} := {t}"));
        } else {
            let bn = format!("__u{i}");
            binds.push((bn.clone(), rr.clone()));
            parts.push(format!("{name} := {bn}"));
        }
    }
    let with_fields = parts.join(", ");
    let (ctor, nested_binds) = if let Some(bt) = strip_pure_wrap(&base_rr) {
        (
            format!("({{ {bt} with {with_fields} }})"),
            binds,
        )
    } else {
        binds.push(("__b".to_string(), base_rr));
        (
            format!("({{ __b with {with_fields} }})"),
            binds,
        )
    };
    let mut nested = format!("(pure {ctor})");
    for (bn, rr) in nested_binds.into_iter().rev() {
        nested = format!("({rr} >>= fun {bn} => {nested})");
    }
    nested
}

fn bind_checked_binop(lhs: &Expr, rhs: &Expr, checked: &str, ctx: &LeanExprCtx<'_>) -> String {
    let l = gen_expr_as_route_result(lhs, ctx);
    let r = gen_expr_as_route_result(rhs, ctx);
    if strip_pure_wrap(&l).is_some() && strip_pure_wrap(&r).is_some() {
        let mut raw_ctx = dup_expr_ctx(ctx);
        raw_ctx.expected_bitvec_width = None;
        let l_term = gen_expr(lhs, &raw_ctx);
        let r_term = gen_expr(rhs, &raw_ctx);
        let (l_out, r_out) = coerce_arith_operands(lhs, rhs, &l_term, &r_term, ctx);
        format!("(Cambrian.{} {} {})", checked, l_out, r_out)
    } else {
        format!("({l} >>= fun __l => {r} >>= fun __r => Cambrian.{checked} __l __r)")
    }
}

fn bind_checked_call(args: &[Expr], checked: &str, ctx: &LeanExprCtx<'_>) -> String {
    bind_args_route_result(
        args,
        ctx,
        || {
            let mut raw_ctx = dup_expr_ctx(ctx);
            raw_ctx.expected_bitvec_width = None;
            let terms: Vec<String> = args.iter().map(|a| gen_expr(a, &raw_ctx)).collect();
            format!("(Cambrian.{} {})", checked, terms.join(" "))
        },
        |binders, _arg_exprs| format!("(Cambrian.{} {})", checked, binders.join(" ")),
    )
}

/// Sequence fail-surface message arguments before embedding them in a total
/// callee (`Dispatch.*`, `Routes.*`, extern axiom, …) that expects unwrapped
/// `U256` / record values, not `RouteResult`.
pub(crate) fn bind_message_args_for_total_call(
    args: &[Expr],
    ctx: &LeanExprCtx<'_>,
    build_call: impl Fn(&[String]) -> String,
) -> String {
    bind_args_route_result(
        args,
        ctx,
        || {
            let terms: Vec<String> = args.iter().map(|a| gen_expr(a, ctx)).collect();
            build_call(&terms)
        },
        |binders, _arg_exprs| build_call(binders),
    )
}

/// Sequence `args` left-to-right with `>>=` when any arg is not `(pure …)`.
fn bind_args_route_result(
    args: &[Expr],
    ctx: &LeanExprCtx<'_>,
    all_pure_tail: impl FnOnce() -> String,
    bind_tail: impl FnOnce(&[String], &[&Expr]) -> String,
) -> String {
    let rrs: Vec<String> = args
        .iter()
        .map(|a| gen_expr_as_route_result(a, ctx))
        .collect();
    if rrs.iter().all(|t| strip_pure_wrap(t).is_some()) {
        return all_pure_tail();
    }
    let binders: Vec<String> = (0..args.len()).map(|i| format!("__p{i}")).collect();
    let arg_refs: Vec<&Expr> = args.iter().collect();
    let mut nested = bind_tail(&binders, &arg_refs);
    for (i, rr) in rrs.iter().enumerate().rev() {
        nested = format!("({rr} >>= fun __p{i} => {nested})");
    }
    nested
}

fn bind_pure_fn_call(name: &str, args: &[Expr], ctx: &LeanExprCtx<'_>) -> String {
    let pf = ctx
        .pure_fns
        .iter()
        .find(|f| f.name == name)
        .expect("bind_pure_fn_call: missing pure fn");
    bind_args_route_result(
        args,
        ctx,
        || {
            let terms: Vec<String> = args.iter().map(|a| gen_expr(a, ctx)).collect();
            if terms.is_empty() {
                format!("(Cambrian.Generated.Pure.{name})")
            } else {
                format!("(Cambrian.Generated.Pure.{} {})", name, terms.join(" "))
            }
        },
        |binders, arg_exprs| {
            let coerced: Vec<String> = binders
                .iter()
                .zip(pf.params.iter().zip(arg_exprs.iter()))
                .map(|(bn, (p, arg))| coerce_arg(arg, bn, &p.ty, ctx))
                .collect();
            if coerced.is_empty() {
                format!("(Cambrian.Generated.Pure.{name})")
            } else {
                format!(
                    "(Cambrian.Generated.Pure.{} {})",
                    name,
                    coerced.join(" ")
                )
            }
        },
    )
}

/// Total `pure fn` callee with fail-surface arguments (FARC SEQ-2).
fn bind_total_pure_fn_call(name: &str, args: &[Expr], ctx: &LeanExprCtx<'_>) -> String {
    let pf = ctx
        .pure_fns
        .iter()
        .find(|f| f.name == name)
        .expect("bind_total_pure_fn_call: missing pure fn");
    let call = Expr::FnCall(name.to_string(), args.to_vec());
    bind_args_route_result(
        args,
        ctx,
        || format!("(pure {})", gen_expr(&call, ctx)),
        |binders, arg_exprs| {
            let coerced: Vec<String> = binders
                .iter()
                .zip(pf.params.iter().zip(arg_exprs.iter()))
                .map(|(bn, (p, arg))| coerce_arg(arg, bn, &p.ty, ctx))
                .collect();
            if coerced.is_empty() {
                format!("(pure (Cambrian.Generated.Pure.{name}))")
            } else {
                format!(
                    "(pure (Cambrian.Generated.Pure.{} {}))",
                    name,
                    coerced.join(" ")
                )
            }
        },
    )
}

fn std_math_minmax_ite(name: &str, a: &str, b: &str, signed: bool, nat: bool) -> String {
    match name {
        "min" if signed && !nat => {
            format!("(if ({}.slt {}) then {} else {})", a, b, a, b)
        }
        "min" => format!("(if {} < {} then {} else {})", a, b, a, b),
        "max" if signed && !nat => {
            format!("(if ({}.slt {}) then {} else {})", b, a, a, b)
        }
        "max" => format!("(if {} > {} then {} else {})", a, b, a, b),
        _ => unreachable!("std_math_minmax_ite"),
    }
}

fn bind_std_math_minmax_route_result(name: &str, args: &[Expr], ctx: &LeanExprCtx<'_>) -> String {
    bind_args_route_result(
        args,
        ctx,
        || format!("(pure {})", gen_namespaced_call("std::math", name, args, ctx)),
        |binders, arg_exprs| {
            let signed =
                expr_is_signed(arg_exprs[0], ctx) || expr_is_signed(arg_exprs[1], ctx);
            let nat = ctx.type_ctx.use_nat_numerics;
            let c0 = coerce_arg(
                arg_exprs[0],
                &binders[0],
                &coerce_type_for_expr(arg_exprs[0], ctx),
                ctx,
            );
            let c1 = coerce_arg(
                arg_exprs[1],
                &binders[1],
                &coerce_type_for_expr(arg_exprs[1], ctx),
                ctx,
            );
            format!(
                "(pure {})",
                std_math_minmax_ite(name, &c0, &c1, signed, nat)
            )
        },
    )
}

fn coerce_type_for_expr(arg: &Expr, ctx: &LeanExprCtx<'_>) -> Type {
    if let Some(w) = bitvec_width(arg, ctx) {
        return width_to_coerce_type(w);
    }
    Type::Simple("U256".to_string())
}

fn width_to_coerce_type(w: u32) -> Type {
    Type::Simple(match w {
        8 => "u8".to_string(),
        16 => "u16".to_string(),
        32 => "u32".to_string(),
        64 => "u64".to_string(),
        128 => "u128".to_string(),
        160 => "address".to_string(),
        256 => "U256".to_string(),
        _ => "U256".to_string(),
    })
}

pub(crate) fn strip_pure_wrap(term: &str) -> Option<&str> {
    let t = term.trim();
    if let Some(inner) = t.strip_prefix("(pure ") {
        if let Some(stripped) = inner.strip_suffix(')') {
            return Some(stripped.trim());
        }
    }
    None
}

/// Declared result width of a `pure fn` call (`f(args)` with a fixed-width
/// `Simple` return type). A fail-mode call is bound before use, so its
/// operand is the payload of that width too.
fn pure_fn_call_width(expr: &Expr, ctx: &LeanExprCtx<'_>) -> Option<u32> {
    let Expr::FnCall(name, _) = expr else { return None };
    let f = ctx.pure_fns.iter().find(|f| &f.name == name)?;
    type_bitvec_width(&f.return_type)
}

/// Coerce `term` (lowered from `expr`) to `BitVec width`. A width-visible
/// total `pure fn` call is cast from its declared return type (sign-aware,
/// ascribed); everything else goes through [`coerce_term_to_width`].
pub(crate) fn coerce_result_to_width(
    expr: &Expr,
    term: &str,
    width: u32,
    ctx: &LeanExprCtx<'_>,
) -> String {
    if bitvec_width(expr, ctx).is_none() {
        if let (Some(src), Expr::FnCall(name, _)) = (pure_fn_call_width(expr, ctx), expr) {
            let signed = ctx
                .pure_fns
                .iter()
                .find(|f| &f.name == name)
                .is_some_and(|f| matches!(&f.return_type, Type::Simple(s) if s.starts_with('i')));
            return if src == width {
                term.to_string()
            } else if signed && width > src {
                format!("(({}).signExtend {})", term, width)
            } else {
                format!("(Cambrian.castWidth {} ({} : BitVec {}))", width, term, src)
            };
        }
    }
    coerce_term_to_width(expr, term, width, ctx)
}

/// Operand widths for the mixed-width promotion of a binop. A `pure fn`
/// call is width-visible only against a typed peer of a different width —
/// the one case where the operands would otherwise not type-check.
fn operand_widths(lhs: &Expr, rhs: &Expr, ctx: &LeanExprCtx<'_>) -> (Option<u32>, Option<u32>) {
    with_call_widths(lhs, rhs, bitvec_width(lhs, ctx), bitvec_width(rhs, ctx), ctx)
}

/// `(lw, rw)` with `pure fn` calls made width-visible iff both sides then
/// have known, different widths; otherwise the widths are returned as given.
fn with_call_widths(
    lhs: &Expr,
    rhs: &Expr,
    lw: Option<u32>,
    rw: Option<u32>,
    ctx: &LeanExprCtx<'_>,
) -> (Option<u32>, Option<u32>) {
    let l = lw.or_else(|| pure_fn_call_width(lhs, ctx));
    let r = rw.or_else(|| pure_fn_call_width(rhs, ctx));
    match (l, r) {
        (Some(a), Some(b)) if a != b => (l, r),
        _ => (lw, rw),
    }
}

/// Shared BitVec width promotion for arithmetic operands (mirrors
/// the total `gen_binop` path).
fn coerce_arith_operands(
    lhs: &Expr,
    rhs: &Expr,
    l: &str,
    r: &str,
    ctx: &LeanExprCtx<'_>,
) -> (String, String) {
    if ctx.type_ctx.use_nat_numerics {
        return (l.to_string(), r.to_string());
    }
    let (lw, rw) = operand_widths(lhs, rhs, ctx);
    match (lw, rw) {
        (Some(a), Some(b)) if a != b => {
            let target = a.max(b);
            (
                coerce_binop_operand(lhs, l, target, ctx),
                coerce_binop_operand(rhs, r, target, ctx),
            )
        }
        (Some(a), None)
            if is_literal_nat_tree(rhs)
                || is_bare_numeric_literal(rhs)
                || is_nat_ident(rhs, ctx) =>
        {
            (l.to_string(), lean_coerce_operand_to_width(rhs, r, a, ctx))
        }
        (None, Some(b))
            if is_literal_nat_tree(lhs)
                || is_bare_numeric_literal(lhs)
                || is_nat_ident(lhs, ctx) =>
        {
            (lean_coerce_operand_to_width(lhs, l, b, ctx), r.to_string())
        }
        (None, None) => {
            if let Some(w) = ctx.expected_bitvec_width {
                (
                    lean_coerce_operand_to_width(lhs, l, w, ctx),
                    lean_coerce_operand_to_width(rhs, r, w, ctx),
                )
            } else {
                (l.to_string(), r.to_string())
            }
        }
        _ => (l.to_string(), r.to_string()),
    }
}

fn gen_binop(lhs: &Expr, op: &BinOp, rhs: &Expr, ctx: &LeanExprCtx<'_>) -> String {
    // Lower operands without the ambient expected width so literal
    // ascription happens once in `coerce_term_to_width` (avoids
    // `((n : BitVec W) : BitVec W)`).
    let mut raw_ctx = dup_expr_ctx(ctx);
    raw_ctx.expected_bitvec_width = None;
    let l = gen_expr(lhs, &raw_ctx);
    let r = gen_expr(rhs, &raw_ctx);
    // Wrapping arithmetic always promotes both operands to U256.
    // Solidity 0.8 allows e.g. `block.timestamp - uint64(...)` thanks
    // to implicit widening; Lean's `BitVec n` is monomorphic so we
    // splice in `Cambrian.castWidth 256`. The resulting `BitVec.sub`
    // already wraps modulo 2^256 — the explicit `-%` operator just
    // documents intent.
    if matches!(
        op,
        BinOp::WrappingAdd | BinOp::WrappingSub | BinOp::WrappingMul
    ) {
        let op_str = match op {
            BinOp::WrappingAdd => "+",
            BinOp::WrappingSub => "-",
            BinOp::WrappingMul => "*",
            _ => unreachable!(),
        };
        // Under `numerics: nat` there is no fixed width to wrap around;
        // wrapping ops degrade to plain `Nat` arithmetic (overflow
        // ignored, matching the mode's "simple numbers" contract).
        if ctx.type_ctx.use_nat_numerics {
            return format!("({} {} {})", l, op_str, r);
        }
        let wide = format!(
            "({} {} {})",
            lean_coerce_operand_to_width(lhs, &l, 256, ctx),
            op_str,
            lean_coerce_operand_to_width(rhs, &r, 256, ctx),
        );
        // Truncating the 256-bit result wraps modulo the narrower width,
        // matching the EVM `uintN(_wadd(..))` lowering.
        return match ctx.expected_bitvec_width {
            Some(w) if w != 256 => format!("(Cambrian.castWidth {} ({} : BitVec 256))", w, wide),
            _ => wide,
        };
    }
    // Arithmetic / comparison binops require matching `BitVec n`
    // widths in Lean. When one side's source type is a wider numeric
    // than the other (e.g. `deadline : U256` vs `sys::timestamp :
    // BitVec 64`), promote the narrower side to the wider width via
    // `Cambrian.castWidth`. Also coerce bare Nat literal trees against
    // a typed peer / expected width (B-12 waves 3a/3b).
    let arith_or_cmp = matches!(
        op,
        BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Mod
            | BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::BitXor
            | BinOp::Shl
            | BinOp::Shr
            | BinOp::Eq
            | BinOp::Ne
            | BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge
    );
    // Under `numerics: nat` integer scalars are `Nat` (no fixed width),
    // so width promotion via `castWidth` neither applies nor type-checks;
    // emit the operands verbatim. `address`/`pubkey` comparisons keep
    // matching widths intrinsically, so they need no promotion either.
    let mixed_sign_arith_width = if matches!(
        op,
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
    ) && !ctx.type_ctx.use_nat_numerics
    {
        let mut sign_ctx = dup_expr_ctx(ctx);
        sign_ctx.expected_signed = None;
        mixed_sign_widening_width(lhs, rhs, ctx)
            .map(|w| (w, expr_is_signed(lhs, &sign_ctx), expr_is_signed(rhs, &sign_ctx)))
    } else {
        None
    };
    let (l_out, r_out) = if let Some((target, ls, rs)) = mixed_sign_arith_width {
        (
            lean_promote_mixed_sign_compare_operand(lhs, &l, target, ls, ctx),
            lean_promote_mixed_sign_compare_operand(rhs, &r, target, rs, ctx),
        )
    } else if arith_or_cmp && !ctx.type_ctx.use_nat_numerics {
        let (lw, rw) = operand_widths(lhs, rhs, ctx);
        match (lw, rw) {
            (Some(a), Some(b)) if a != b => {
                let target = a.max(b);
                (
                    coerce_binop_operand(lhs, &l, target, ctx),
                    coerce_binop_operand(rhs, &r, target, ctx),
                )
            }
            (Some(a), None)
                if is_literal_nat_tree(rhs)
                    || is_bare_numeric_literal(rhs)
                    || is_nat_ident(rhs, ctx) =>
            {
                (l, lean_coerce_operand_to_width(rhs, &r, a, ctx))
            }
            (None, Some(b))
                if is_literal_nat_tree(lhs)
                    || is_bare_numeric_literal(lhs)
                    || is_nat_ident(lhs, ctx) =>
            {
                (lean_coerce_operand_to_width(lhs, &l, b, ctx), r)
            }
            (None, None) => {
                if let Some(w) = ctx.expected_bitvec_width {
                    (
                        lean_coerce_operand_to_width(lhs, &l, w, ctx),
                        lean_coerce_operand_to_width(rhs, &r, w, ctx),
                    )
                } else {
                    (l, r)
                }
            }
            _ => (l, r),
        }
    } else {
        (l, r)
    };
    let lhs_signed = expr_is_signed(lhs, ctx);
    let rhs_signed = expr_is_signed(rhs, ctx);
    let mixed_sign_cmp = matches!(
        op,
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne
    ) && lhs_signed != rhs_signed
        && !ctx.type_ctx.use_nat_numerics;
    if mixed_sign_cmp {
        let target = 256u32;
        let l_prom = lean_promote_mixed_sign_compare_operand(lhs, &l_out, target, lhs_signed, ctx);
        let r_prom = lean_promote_mixed_sign_compare_operand(rhs, &r_out, target, rhs_signed, ctx);
        return match op {
            BinOp::Lt => format!("({}.slt {})", l_prom, r_prom),
            BinOp::Le => format!("({}.sle {})", l_prom, r_prom),
            BinOp::Gt => format!("({}.slt {})", r_prom, l_prom),
            BinOp::Ge => format!("({}.sle {})", r_prom, l_prom),
            BinOp::Eq => format!("({} == {})", l_prom, r_prom),
            BinOp::Ne => format!("({} != {})", l_prom, r_prom),
            _ => unreachable!(),
        };
    }
    let signed = lhs_signed || rhs_signed;
    if signed && !ctx.type_ctx.use_nat_numerics {
        match op {
            BinOp::Lt => return format!("({}.slt {})", l_out, r_out),
            BinOp::Le => return format!("({}.sle {})", l_out, r_out),
            BinOp::Gt => return format!("({}.slt {})", r_out, l_out),
            BinOp::Ge => return format!("({}.sle {})", r_out, l_out),
            BinOp::Div => return format!("({}.sdiv {})", l_out, r_out),
            BinOp::Mod => return format!("({}.srem {})", l_out, r_out),
            BinOp::Shr => {
                // Shift amount is `Nat` (same as unsigned `>>>`).
                return format!("({}.sshiftRight ({}).toNat)", l_out, r_out);
            }
            _ => {}
        }
    }
    let op_str = match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::BitAnd => "&&&",
        BinOp::BitOr => "|||",
        BinOp::BitXor => "^^^",
        BinOp::Shl => "<<<",
        BinOp::Shr => ">>>",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "≤",
        BinOp::Gt => ">",
        BinOp::Ge => "≥",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::WrappingAdd | BinOp::WrappingSub | BinOp::WrappingMul => unreachable!(),
    };
    format!("({} {} {})", l_out, op_str, r_out)
}

/// Best-effort: infer the `BitVec n` width of `expr` from local
/// context. Returns `Some(n)` for numerics whose width is statically
/// derivable from the entity / route signature; `None` for anything
/// else (arithmetic results, function calls, etc.) so the caller can
/// fall back to emitting the operands verbatim.
pub(crate) fn bitvec_width(expr: &Expr, ctx: &LeanExprCtx<'_>) -> Option<u32> {
    match expr {
        Expr::Ident(name) => {
            // Route params / let-bound vars (typed by signature).
            if let Some(p) = ctx
                .entity
                .routes
                .iter()
                .filter(|r| Some(&r.name) == ctx.route_name.as_ref())
                .flat_map(|r| r.params.iter())
                .find(|p| &p.name == name)
            {
                return type_bitvec_width(&p.ty);
            }
            if let Some(p) = pure_fn_param(name, ctx) {
                return type_bitvec_width(&p.ty);
            }
            // Entity members (typed by declaration).
            if let Some(m) = ctx.entity.members.iter().find(|m| &m.name == name) {
                return type_bitvec_width(&m.ty);
            }
            None
        }
        Expr::FieldAccess(_, _) => None,
        Expr::SysField(name) => match name.as_str() {
            "timestamp" | "now" => Some(64),
            "chainid" | "chainId" => Some(256),
            "blockNumber" | "block_number" | "number" => Some(256),
            "balance" => Some(256),
            "address" => Some(160),
            _ => None,
        },
        Expr::MsgField(name) => match name.as_str() {
            "value" => Some(256),
            "sender" => Some(160),
            _ => None,
        },
        Expr::NamespacedCall {
            namespace, name, ..
        }
        | Expr::EnumVariantWithData(namespace, name, _) => {
            match (namespace.as_str(), name.as_str()) {
                ("evm", "keccak256") => Some(256),
                ("evm", "keccak256Packed") => Some(256),
                ("evm", "ecrecover") => Some(160),
                _ => None,
            }
        }
        _ => None,
    }
}

fn type_bitvec_width(ty: &Type) -> Option<u32> {
    if let Type::Simple(s) = ty {
        match s.as_str() {
            "u8" | "i8" => Some(8),
            "u16" | "i16" => Some(16),
            "u32" | "i32" => Some(32),
            "u64" | "i64" => Some(64),
            "u128" | "i128" => Some(128),
            "U256" | "uint256" => Some(256),
            "address" => Some(160),
            "pubkey" => Some(256),
            _ => None,
        }
    } else {
        None
    }
}

fn gen_unaryop(op: &UnaryOp, inner: &Expr, ctx: &LeanExprCtx<'_>) -> String {
    match op {
        UnaryOp::Not => {
            let i = gen_expr(inner, ctx);
            format!("(!{})", i)
        }
        UnaryOp::Neg => {
            // Propagate signed expectation so `-5` elaborates as `Int` /
            // signed BitVec rather than failing on `Nat`.
            let mut neg_ctx = dup_expr_ctx(ctx);
            if ctx.expected_signed != Some(false) {
                neg_ctx.expected_signed = Some(true);
            }
            if ctx.type_ctx.use_nat_numerics && is_bare_numeric_literal(inner) {
                let n = gen_expr(inner, &neg_ctx);
                return format!("(-{})", n);
            }
            if !ctx.type_ctx.use_nat_numerics && is_bare_numeric_literal(inner) {
                if let Some(w) = ctx
                    .expected_bitvec_width
                    .or_else(|| bitvec_width(inner, &neg_ctx))
                {
                    // `ofInt` expects an `Int` magnitude; do not ascribe the
                    // inner literal as `BitVec w` (F6 / signed_arith `-5`).
                    let mut int_ctx = dup_expr_ctx(&neg_ctx);
                    int_ctx.expected_bitvec_width = None;
                    let n = gen_expr(inner, &int_ctx);
                    return format!("(BitVec.ofInt {} (-({} : Int)))", w, n);
                }
            }
            let i = gen_expr(inner, &neg_ctx);
            format!("(-{})", i)
        }
        UnaryOp::Deref => gen_expr(inner, ctx), // Deref has no Lean analogue in P1; transparent.
    }
}

// ---------------------------------------------------------------------------
// Function calls
// ---------------------------------------------------------------------------

fn gen_fn_call(name: &str, args: &[Expr], ctx: &LeanExprCtx<'_>) -> String {
    // `addressOf(<E>.state(args))` — the function-call spelling of a
    // CREATE2 address derivation. Equivalent to `<E>.address(args)`;
    // both must produce the identical `(<E>.address (<E>.Identity.mk …))`
    // term. The inner `.state(...)` is consumed here (it is not a
    // standalone value), so it never reaches the unsupported-method path.
    if name == "addressOf" && args.len() == 1 {
        if let Expr::MethodCall(base, m, state_args) = &args[0] {
            if m == "state" {
                if let Expr::Ident(entity_name) = base.as_ref() {
                    if let Some(term) = create2_address_term(entity_name, state_args, ctx) {
                        return term;
                    }
                }
            }
        }
    }

    let parts: Vec<String> = args.iter().map(|a| gen_expr(a, ctx)).collect();
    let arg_str = parts.join(" ");

    // Prelude-provided helpers come first so user code can't shadow
    // them. Currently empty in P1; populated in tandem with the
    // Prelude file.
    if let Some(prelude) = prelude_helper(name, args.len(), ctx.type_ctx.use_nat_numerics) {
        let call = if arg_str.is_empty() {
            prelude.to_string()
        } else {
            format!("({} {})", prelude, arg_str)
        };
        // `hashOf*` returns a fixed-width `BitVec 256`. Under `numerics: nat`
        // its result is a `U256` value (== `Nat`) — typically a map key — so
        // project it to `Nat` at this boundary (the hash itself stays
        // deterministic and fixed-width).
        if name == "hashOf" && ctx.type_ctx.use_nat_numerics {
            return format!("({}).toNat", call);
        }
        return call;
    }

    // Type-name "constructor" calls (`address(0)`, `U256(amount)`, …)
    // are parsed as `FnCall("<typename>", [arg])`. These have no
    // matching `fn` declaration; they're conversion / literal casts.
    // We catch them before falling through to bare-call emission.
    if let Some(term) = type_constructor_call(name, args, ctx) {
        return term;
    }

    // P2: `derived foo() -> T` invariant helpers receive `s` as their
    // first argument. The user-visible call site reads `foo(args)`;
    // we splice in the state-var.
    if ctx.derived_helpers.contains(name) {
        let state_var = ctx.state_var.as_str();
        return if arg_str.is_empty() {
            format!("({} {})", name, state_var)
        } else {
            format!("({} {} {})", name, state_var, arg_str)
        };
    }

    if let Some(pf) = ctx.pure_fns.iter().find(|f| f.name == name) {
        // Auto-widen bit-vector args to match the declared param width.
        // Cambrian has implicit numeric widening across the call
        // boundary; Lean's `BitVec n` is monomorphic in `n`, so we
        // splice in `Cambrian.castWidth` whenever the param has a
        // known fixed width (`u8` … `i128`, `U256`, `address`,
        // `pubkey`). Other types (`bool`, `String`, records, enums,
        // generics) pass through verbatim.
        let coerced: Vec<String> = pf
            .params
            .iter()
            .zip(args.iter().zip(parts.iter()))
            .map(|(p, (arg_expr, arg_term))| coerce_arg(arg_expr, arg_term, &p.ty, ctx))
            .collect();
        let arg_str = coerced.join(" ");
        return if arg_str.is_empty() {
            format!("Cambrian.Generated.Pure.{}", name)
        } else {
            format!("(Cambrian.Generated.Pure.{} {})", name, arg_str)
        };
    }

    // A non-throwing `view` route of the current entity used in an
    // *expression* (an invariant `check`, a `derived` body, or a `track`
    // binding): lower to the route's returned value. `<E>.Routes.<name> w inst
    // ctx args : World × Ret`, so project `.snd`. Only in World/spec mode
    // (`world_var` set) — route *bodies* lower a view `call` as a statement.
    // Without this, a bare `totalSupply()` fell through to the emit below and
    // became an auto-bound implicit, silently making the theorem vacuous.
    if let (Some(w), Some(inst)) = (ctx.world_var.clone(), ctx.instance_var.clone()) {
        if let Some(r) = ctx.entity.routes.iter().find(|r| r.name == name) {
            if super::route::route_is_view(r)
                && !super::route::route_fail_mode(ctx.type_ctx.program, ctx.entity, r)
            {
                let coerced: Vec<String> = r
                    .params
                    .iter()
                    .zip(args.iter().zip(parts.iter()))
                    .map(|(p, (arg_expr, arg_term))| coerce_arg(arg_expr, arg_term, &p.ty, ctx))
                    .collect();
                let call_args = coerced.join(" ");
                return if call_args.is_empty() {
                    format!(
                        "({}.Routes.{} {} {} ctx).snd",
                        ctx.entity.name, name, w, inst
                    )
                } else {
                    format!(
                        "({}.Routes.{} {} {} ctx {}).snd",
                        ctx.entity.name, name, w, inst, call_args
                    )
                };
            }
        }
    }

    // Bare call: assume an in-namespace function. Emit bare so Lean
    // can resolve it via the active `namespace` declaration.
    if arg_str.is_empty() {
        name.to_string()
    } else {
        format!("({} {})", name, arg_str)
    }
}

/// Helpers that ship in `Cambrian.Core` (re-exported via Prelude). Keep
/// this small and self-documenting — every entry corresponds to a concrete
/// `def Cambrian.<name>` in the vendored Core.
fn prelude_helper(name: &str, arity: usize, use_nat: bool) -> Option<&'static str> {
    match (name, arity) {
        ("divmod", _) => {
            if use_nat {
                Some("Cambrian.divmodNat")
            } else {
                Some("Cambrian.divmod")
            }
        }
        // `hashOf(a, b, c, …)` — polymorphic opaque hash family. We
        // dispatch on arity so each call site stays well-typed.
        ("hashOf", 2) => Some("Cambrian.hashOf2"),
        ("hashOf", 3) => Some("Cambrian.hashOf3"),
        ("hashOf", 4) => Some("Cambrian.hashOf4"),
        ("hashOf", 5) => Some("Cambrian.hashOf5"),
        ("hashOf", 6) => Some("Cambrian.hashOf6"),
        _ => None,
    }
}

/// Recognise Cambrian type-name "constructors" used as conversion /
/// literal casts (e.g. `address(0)`, `U256(amount)`, `u64(timestamp)`).
/// Returns the Lean term to emit. `None` ⇒ not a recognised cast.
fn type_constructor_call(name: &str, args: &[Expr], ctx: &LeanExprCtx<'_>) -> Option<String> {
    if args.len() != 1 {
        return None;
    }
    let arg = &args[0];
    // Under `numerics: nat` an integer-scalar constructor (`u64(x)`,
    // `U256(x)`, …) is identity on `Nat` (width ignored). `address` /
    // `pubkey` stay fixed-width and fall through to the BitVec path.
    if ctx.type_ctx.use_nat_numerics
        && matches!(
            name,
            "u8" | "u16"
                | "u32"
                | "u64"
                | "u128"
                | "i8"
                | "i16"
                | "i32"
                | "i64"
                | "i128"
                | "U256"
                | "uint256"
        )
    {
        return Some(format!("({})", gen_expr(arg, ctx)));
    }
    let lit_or_widen = |target_bits: u32| -> String {
        let term = gen_expr(arg, ctx);
        coerce_term_to_width(arg, &term, target_bits, ctx)
    };
    match name {
        "address" => Some(lit_or_widen(160)),
        "pubkey" => Some(lit_or_widen(256)),
        "U256" | "uint256" => Some(lit_or_widen(256)),
        "u8" | "i8" => Some(lit_or_widen(8)),
        "u16" | "i16" => Some(lit_or_widen(16)),
        "u32" | "i32" => Some(lit_or_widen(32)),
        "u64" | "i64" => Some(lit_or_widen(64)),
        "u128" | "i128" => Some(lit_or_widen(128)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// if / let / block
// ---------------------------------------------------------------------------

fn gen_if(cond: &Expr, then_e: &Expr, else_e: Option<&Expr>, ctx: &LeanExprCtx<'_>) -> String {
    let c = gen_expr(cond, ctx);
    let t = gen_expr(then_e, ctx);
    let body = match else_e {
        Some(e) => format!("(if {} then {} else {})", c, t, gen_expr(e, ctx)),
        None => {
            // Lean's `if … then … else …` requires both branches.
            // Without an explicit else, the source value is implicitly
            // unit-equivalent — we surface it as `()` and let the
            // surrounding context type-check.
            format!("(if {} then {} else ())", c, t)
        }
    };
    // B-12/3e: pin the ite result width when the surrounding context
    // knows it, so Lean does not invent `BitVec ?m`.
    if let Some(w) = ctx.expected_bitvec_width {
        if !ctx.type_ctx.use_nat_numerics && else_e.is_some() {
            return format!("({} : BitVec {})", body, w);
        }
    }
    body
}

/// Lower a deterministic (CREATE2) address derivation to
/// `(<E>.address (<E>.Identity.mk <id_args>))` — the canonical form
/// emitted by `lean_entity` per entity. Returns `None` when `entity`
/// is not an in-program entity (extern / unknown targets have no
/// `.address` helper). Shared by the three surface spellings:
/// `<E>.address(args)`, `addressOf(<E>.state(args))`, and the
/// `Expr::AddressOf` keyword form. Formatting is domain-owned via
/// [`LeanDomain::entity_address_term`].
fn create2_address_term(entity: &str, id_args: &[Expr], ctx: &LeanExprCtx<'_>) -> Option<String> {
    if !ctx
        .type_ctx
        .program
        .entities
        .iter()
        .any(|e| e.name == entity)
    {
        return None;
    }
    let arg_terms: Vec<String> = id_args.iter().map(|a| gen_expr(a, ctx)).collect();
    Some(domain().entity_address_term(entity, &arg_terms))
}

/// Lower `inst.address` / `inst.address()` to `(<E>.address inst)` when
/// `inner` names the active route identity (`instance_var`). Matches
/// `sys::address` and the CREATE2 helper; Identity itself has no field
/// named `address` (LEAN-H11 / T-LEAN-009).
fn try_instance_address_accessor(inner: &Expr, ctx: &LeanExprCtx<'_>) -> Option<String> {
    let inst_var = ctx.instance_var.as_deref()?;
    match inner {
        Expr::Ident(name) if name == inst_var => {
            Some(format!("({}.address {})", ctx.entity.name, inst_var))
        }
        _ => None,
    }
}

/// Lower `expr` against a known `expected` type, applying the few
/// type-directed coercions Lean needs that an untyped [`gen_expr`]
/// cannot infer. Currently: an empty string literal in an `address`
/// position is Cambrian's spelling of the zero address, which on the
/// Lean (`BitVec 160`) model is `0` — a bare `""` there would otherwise
/// be a `String` and fail to type-check.
pub(crate) fn gen_expr_as_type(expr: &Expr, expected: &Type, ctx: &LeanExprCtx<'_>) -> String {
    if let Expr::StringLiteral(s) = expr {
        if s.is_empty() && lower_type(expected, &ctx.type_ctx) == "Cambrian.Address" {
            return "(0 : Cambrian.Address)".to_string();
        }
    }
    // B-14: thread Vec vs HashMap so bare `{}` picks `[]` / AddressMap.empty.
    let mut typed = dup_expr_ctx(ctx);
    if let Some(shape) = collection_shape_of_type(expected, &ctx.type_ctx) {
        typed.expected_collection = Some(shape);
    }
    if let Some(w) = bitvec_width_of_type(expected, &ctx.type_ctx) {
        typed.expected_bitvec_width = Some(w);
    }
    typed.expected_signed = Some(type_is_signed(expected, &ctx.type_ctx));
    gen_expr(expr, &typed)
}

fn gen_let(pat: &Pattern, value: &Expr, body: &Expr, ctx: &LeanExprCtx<'_>) -> String {
    let v = gen_expr(value, ctx);
    let mut new_ctx = LeanExprCtx {
        type_ctx: clone_type_ctx(&ctx.type_ctx),
        entity: ctx.entity,
        local_enums: ctx.local_enums,
        pure_fns: ctx.pure_fns,
        lets: ctx.lets.clone(),
        route_params: ctx.route_params.clone(),
        route_name: ctx.route_name.clone(),
        phase_name: ctx.phase_name.clone(),
        route_arg_names: ctx.route_arg_names.clone(),
        in_transform: ctx.in_transform,
        state_var: ctx.state_var.clone(),
        derived_helpers: ctx.derived_helpers.clone(),
        world_var: ctx.world_var.clone(),
        instance_var: ctx.instance_var.clone(),
        qualified_instances: ctx.qualified_instances.clone(),
        deploy_bindings: ctx.deploy_bindings.clone(),
        hashmap_idents: ctx.hashmap_idents.clone(),
        nat_idents: ctx.nat_idents.clone(),
        trace_acc_var: ctx.trace_acc_var.clone(),
        expected_bitvec_width: ctx.expected_bitvec_width,
        expected_signed: ctx.expected_signed,
        expected_collection: ctx.expected_collection,
        msg_sender_override: ctx.msg_sender_override.clone(),
        pure_fn: ctx.pure_fn,
    };
    new_ctx.push_pattern(pat);
    // Track `let`-bound *map* values so a nested-map operation on the
    // binder (`let inner = m[k]; inner.update(k2, v)`) lowers via the
    // HashMap path instead of falling through to `Cambrian.Unsupported`.
    // Without this, every nested `HashMap<_, HashMap<_, _>>` write loses
    // its inner update.
    if let Pattern::Ident(name) = pat {
        if map_type_of(value, &new_ctx).is_some() {
            new_ctx.hashmap_idents.insert(name.clone());
        }
    }
    let body_str = gen_expr(body, &new_ctx);
    match pat {
        Pattern::Some(inner) => {
            let binder = pattern_to_lean(inner);
            format!("(let {} := ({}).get!\n{})", binder, v, body_str)
        }
        // Escrow (campaign defect L4): the legacy guard below is VACUOUS in
        // Lean's pure model — both arms have type `Unit`, `panic!` reduces to
        // `default = ()`, and the binder is `_`, so the whole `let` zeta-reduces
        // to its body. What it does contribute is (a) a `<decl>.match_1` matcher
        // in the digest zone and (b) `panicWithPosWithDecl <file> <decl> <line>
        // <col>` — the emitted file's LINE and COLUMN baked into the sold kernel
        // term. Under the predictable profile the statement therefore contributes no
        // term at all; the comment keeps the drop visible in the emission.
        Pattern::None if ctx.type_ctx.use_predictable_profile => {
            format!("({} {})", LET_NONE_DROPPED_COMMENT, body_str)
        }
        Pattern::None => format!(
            "(let _ := match {} with | none => () | some _ => (panic! \"let none matched some\" : Unit)\n{})",
            v, body_str
        ),
        Pattern::Deref(inner) => {
            // Transparent: re-lower as if the inner pattern were written.
            // Recurse via a synthetic call to keep binder handling unified.
            let _ = inner;
            let binding = lower_let_binding(pat, &v);
            // `lower_let_binding` returns `let … := …`; wrap as expr-let.
            format!("({}\n{})", binding, body_str)
        }
        other => {
            let pat_str = pattern_to_lean(other);
            format!("(let {} := {}\n{})", pat_str, v, body_str)
        }
    }
}

/// Best-effort static `HashMap` type of `expr` within `ctx` — `Some(ty)`
/// when `expr` provably denotes a map value, else `None`. Used by
/// [`gen_let`] to propagate map-ness onto `let` binders so nested-map
/// reads/writes (`let inner = m[k]; inner.update(…)`) keep lowering via
/// the `Cambrian.AddressMap` path. Handles the value-type projection of
/// indexing a nested map (`m[k] : HashMap<…>` when `m :
/// HashMap<_, HashMap<…>>`), map-returning method chains
/// (`.insert`/`.update`/`.remove`), and `if`/`block`/`let` tails.
/// `expr` has a declared unsigned integer type, whatever the ambient
/// `expected_signed` says: `u8 as i16` widens with zeros, not the sign bit.
fn source_is_unsigned(expr: &Expr, ctx: &LeanExprCtx<'_>) -> bool {
    let ty = match expr {
        Expr::Ident(name) => declared_ident_type(name, ctx),
        Expr::Cast(_, ty) => Some(ty.clone()),
        _ => None,
    };
    ty.is_some_and(|t| matches!(&t, Type::Simple(s) if !s.starts_with('i')) && type_bitvec_width(&t).is_some())
}

/// Parameter `name` of the enclosing `pure fn` (B-34). Other names in a
/// pure-fn body (fold / closure binders) keep the historical fallback: the fn
/// whose parameter-name set matches, then any pure fn with such a parameter.
fn pure_fn_param<'a>(name: &str, ctx: &LeanExprCtx<'a>) -> Option<&'a crate::ast::Param> {
    if let Some(p) = ctx.pure_fn.and_then(|f| f.params.iter().find(|p| p.name == name)) {
        return Some(p);
    }
    if ctx.route_name.is_some() {
        return None;
    }
    let pure_fns: &'a [PureFn] = ctx.pure_fns;
    if let Some(f) = pure_fns.iter().find(|f| {
        f.params.len() == ctx.route_params.len()
            && f.params.iter().all(|p| ctx.route_params.contains(&p.name))
    }) {
        if let Some(p) = f.params.iter().find(|p| p.name == name) {
            return Some(p);
        }
    }
    pure_fns.iter().flat_map(|f| f.params.iter()).find(|p| p.name == name)
}

fn declared_ident_type(name: &str, ctx: &LeanExprCtx<'_>) -> Option<Type> {
    if let Some(p) = ctx
        .entity
        .routes
        .iter()
        .filter(|r| Some(&r.name) == ctx.route_name.as_ref())
        .flat_map(|r| r.params.iter())
        .find(|p| p.name == name)
    {
        return Some(p.ty.clone());
    }
    if let Some(p) = pure_fn_param(name, ctx) {
        return Some(p.ty.clone());
    }
    ctx.entity
        .members
        .iter()
        .find(|m| m.name == name)
        .map(|m| m.ty.clone())
}

/// Best-effort: `expr` has type `HashMap<_, _>`.
pub(crate) fn map_type_of(expr: &Expr, ctx: &LeanExprCtx<'_>) -> Option<Type> {
    let type_is_map = |t: &Type| matches!(t, Type::Generic(n, p) if n == "HashMap" && p.len() == 2);
    match expr {
        Expr::Ident(n) => ctx
            .entity
            .members
            .iter()
            .find(|m| &m.name == n && member_is_hashmap(m))
            .map(|m| m.ty.clone())
            .or_else(|| declared_ident_type(n, ctx).filter(type_is_map)),
        Expr::FieldAccess(base, field) => {
            let rec = match base.as_ref() {
                Expr::Ident(n) => match declared_ident_type(n, ctx)? {
                    Type::Simple(s) => s,
                    _ => return None,
                },
                _ => return None,
            };
            record_field_type(&rec, field, ctx).filter(type_is_map)
        }
        Expr::Index(base, _) => match map_type_of(base, ctx) {
            Some(Type::Generic(_, params)) if params.len() == 2 && type_is_map(&params[1]) => {
                Some(params[1].clone())
            }
            _ => None,
        },
        Expr::MethodCall(base, m, _) if matches!(m.as_str(), "insert" | "update" | "remove") => {
            map_type_of(base, ctx)
        }
        Expr::If(_, t, e) => {
            map_type_of(t, ctx).or_else(|| e.as_ref().and_then(|x| map_type_of(x, ctx)))
        }
        Expr::Block(items) => items.last().and_then(|e| map_type_of(e, ctx)),
        Expr::Let(_, _, body) => map_type_of(body, ctx),
        _ => None,
    }
}

fn gen_block(exprs: &[Expr], ctx: &LeanExprCtx<'_>) -> String {
    if exprs.is_empty() {
        return "()".to_string();
    }
    if exprs.len() == 1 {
        return gen_expr(&exprs[0], ctx);
    }
    // Lean has no statement-block; we chain `let _ := …` for side-effect
    // expressions. In P1 entity-body blocks are always pure, so this
    // shape is rare. We emit a `do`-flavoured chain.
    let mut out = String::from("(do\n");
    for (i, e) in exprs.iter().enumerate() {
        let line = gen_expr(e, ctx);
        if i + 1 == exprs.len() {
            out.push_str(&format!("  pure {}\n", line));
        } else {
            out.push_str(&format!("  let _ := {}\n", line));
        }
    }
    out.push(')');
    out
}

// ---------------------------------------------------------------------------
// records and casts
// ---------------------------------------------------------------------------

fn gen_record_ctor(name: &str, fields: &[(String, Expr)], ctx: &LeanExprCtx<'_>) -> String {
    let qualified = qualify_user_type(name, &ctx.type_ctx);
    let parts: Vec<String> = fields
        .iter()
        .map(|(fname, fexpr)| {
            format!(
                "{} := {}",
                super::core::types::lean_safe_ident(fname),
                gen_expr(fexpr, ctx)
            )
        })
        .collect();
    if parts.is_empty() {
        return format!("({{}} : {})", qualified);
    }
    format!("({{ {} }} : {})", parts.join(", "), qualified)
}

fn gen_record_update(base: &Expr, updates: &[(String, Expr)], ctx: &LeanExprCtx<'_>) -> String {
    let b = gen_expr(base, ctx);
    let parts: Vec<String> = updates
        .iter()
        .map(|(fname, fexpr)| {
            format!(
                "{} := {}",
                super::core::types::lean_safe_ident(fname),
                gen_expr(fexpr, ctx)
            )
        })
        .collect();
    format!("({{ {} with {} }})", b, parts.join(", "))
}

fn gen_cast(inner: &Expr, target: &Type, ctx: &LeanExprCtx<'_>) -> String {
    let mut inner_ctx = dup_expr_ctx(ctx);
    // Preserve source signedness when casting away from a signed type so
    // widen uses `signExtend` rather than unsigned `castWidth`.
    if expr_is_signed(inner, ctx) {
        inner_ctx.expected_signed = Some(true);
    }
    let i = gen_expr(inner, &inner_ctx);
    let target_lean = lower_type(target, &ctx.type_ctx);
    // Under `numerics: nat` integer scalars lower to `Nat` / `Int`.
    if ctx.type_ctx.use_nat_numerics {
        match target_lean.as_str() {
            "Nat" => {
                if expr_is_signed(inner, ctx) || matches!(inner, Expr::UnaryOp(UnaryOp::Neg, _)) {
                    // Negative Int → Nat: Lean's `Int.toNat` maps negatives to 0.
                    return format!("(Int.toNat ({}))", i);
                }
                return format!("({})", i);
            }
            "Int" => {
                if expr_is_signed(inner, ctx)
                    || matches!(
                        inner,
                        Expr::UnaryOp(UnaryOp::Neg, _) | Expr::IntLiteral(_)
                    )
                {
                    return format!("({})", i);
                }
                return format!("(Int.ofNat ({}))", i);
            }
            _ => {}
        }
    }
    fn bits_for(ty: &Type, ctx: &LeanExprCtx<'_>) -> Option<u32> {
        match ty {
            Type::Simple(name) => match name.as_str() {
                "u8" | "i8" => Some(8u32),
                "u16" | "i16" => Some(16),
                "u32" | "i32" => Some(32),
                "u64" | "i64" => Some(64),
                "u128" | "i128" => Some(128),
                "U256" | "uint256" => Some(256),
                "address" => Some(160),
                "pubkey" => Some(256),
                other => {
                    // Resolve type aliases (e.g. `type Amount = U256`).
                    let alias = ctx
                        .type_ctx
                        .local_aliases
                        .iter()
                        .find(|a| a.name == other)
                        .or_else(|| {
                            ctx.type_ctx
                                .program
                                .type_aliases
                                .iter()
                                .find(|a| a.name == other)
                        });
                    alias.and_then(|a| bits_for(&a.ty, ctx))
                }
            },
            _ => None,
        }
    }
    let bits = bits_for(target, ctx);
    if let Some(n) = bits {
        let mut cast_ctx = dup_expr_ctx(ctx);
        cast_ctx.expected_signed = Some(type_is_signed(target, &ctx.type_ctx));
        // Prefer source signedness for widen so `i8 as i16` sign-extends.
        if expr_is_signed(inner, ctx) {
            cast_ctx.expected_signed = Some(true);
        }
        return coerce_term_to_width(inner, &i, n, &cast_ctx);
    }
    match target {
        Type::Simple(name) if name == "bool" => {
            format!("(/- TODO(P4): cast to bool -/ {})", i)
        }
        _ => {
            format!("(/- TODO(P4): cast to {} -/ {})", target_lean, i)
        }
    }
}

// ---------------------------------------------------------------------------
// match
// ---------------------------------------------------------------------------

/// Fail-mode [`gen_match`]: every arm lowers via [`gen_expr_as_route_result`].
fn gen_match_as_route_result(scrutinee: &Expr, arms: &[MatchArm], ctx: &LeanExprCtx<'_>) -> String {
    if ctx.type_ctx.use_predictable_profile {
        return format!("(pure {})", gen_match(scrutinee, arms, ctx));
    }
    let mut scrut_ctx = dup_expr_ctx(ctx);
    scrut_ctx.expected_bitvec_width = None;
    let mut s = gen_expr(scrutinee, &scrut_ctx);
    if !ctx.type_ctx.use_nat_numerics {
        if let Some(w) = bitvec_width(scrutinee, ctx) {
            if !s.contains(": BitVec") {
                s = format!("({} : BitVec {})", s, w);
            }
        }
    }
    let mut out = format!("(match {} with", s);
    for arm in live_match_arms(arms, ctx) {
        let mut arm_ctx = dup_expr_ctx(ctx);
        push_match_pattern_bindings(&arm.pattern, &mut arm_ctx);
        // Keep `(pure …)` arms as RouteResult so mixed fail/total arms share a type.
        let body = gen_expr_as_route_result(&arm.body, &arm_ctx);
        out.push_str(&format!(
            " | {} => {}",
            match_pattern_to_lean(&arm.pattern, ctx),
            body,
        ));
    }
    out.push(')');
    out
}

/// Drop a trailing catch-all whose preceding arms already cover every case
/// (V47 warns about it): Lean rejects it as a redundant alternative.
fn live_match_arms<'m>(arms: &'m [MatchArm], ctx: &LeanExprCtx<'_>) -> &'m [MatchArm] {
    let Some(i) = arms
        .iter()
        .position(|a| matches!(a.pattern, MatchPattern::Wildcard | MatchPattern::Ident(_)))
    else {
        return arms;
    };
    let enums: Vec<&EnumDecl> = ctx
        .local_enums
        .iter()
        .chain(ctx.type_ctx.program.enums.iter())
        .collect();
    if crate::validate::arms_cover_all(&arms[..i], &enums) {
        &arms[..i]
    } else {
        arms
    }
}

fn gen_match(scrutinee: &Expr, arms: &[MatchArm], ctx: &LeanExprCtx<'_>) -> String {
    let arms = live_match_arms(arms, ctx);
    // Lower the scrutinee without the route/return expected width: that
    // width applies to arm *bodies* (via `ascribe_literal_match_arm`), not
    // to the scrutinee. Falling back to `expected_bitvec_width` here wrongly
    // ascribes `: BitVec W` onto `Option`/`String`/enum scrutinees (e.g.
    // `match std::str::parse_uint(...) { … }` in a `-> u64` route — STD-LEAN;
    // enum match B-20).
    let mut scrut_ctx = dup_expr_ctx(ctx);
    scrut_ctx.expected_bitvec_width = None;
    let mut s = gen_expr(scrutinee, &scrut_ctx);
    // B-12/3e: ascribe scrutinee width only when the scrutinee itself is
    // BitVec-shaped so pattern variables do not carry `BitVec ?m`.
    if !ctx.type_ctx.use_nat_numerics {
        if let Some(w) = bitvec_width(scrutinee, ctx) {
            if !s.contains(": BitVec") {
                s = format!("({} : BitVec {})", s, w);
            }
        }
    }
    // Predictable profile (campaign slice Э2 / `local_changes.md` §19): a `match`
    // expression makes the elaborator mint a matcher (`<owner>.match_N`) — a
    // zone declaration with unpredictable numbering the R3 digest predictor
    // cannot reconstruct (see B4 `lean_predictable.rs`). Lower every match to an
    // explicit eliminator so no matcher is minted. FAIL-LOUD on any shape
    // outside the handled grammar (parity with the B4 route-body contract).
    if ctx.type_ctx.use_predictable_profile {
        return gen_match_predictable(&s, arms, ctx);
    }
    // Parenthesize so the term embeds cleanly inside `do` / `let`
    // (C-2): bare `match … with\n  | …` can confuse the parser when the
    // surrounding fold uses `(do …) : Except _ _` ascription.
    let mut out = format!("(match {} with", s);
    for arm in arms {
        let mut arm_ctx = dup_expr_ctx(ctx);
        push_match_pattern_bindings(&arm.pattern, &mut arm_ctx);
        let body = gen_expr(&arm.body, &arm_ctx);
        // Literal arm bodies elaborate as `Nat` without an expected type
        // (B-7 form b). Ascribe only when the enclosing context pinned a
        // BitVec width (route/pure/member return type). Defaulting to 64
        // here mixed `BitVec 64` literals with `Nat` range binders inside
        // foldlM (C-2 lake: `HAdd Nat Nat (BitVec 64)`).
        let body = ascribe_literal_match_arm(&arm.body, &body, &arm_ctx);
        out.push_str(&format!(
            " | {} => {}",
            match_pattern_to_lean(&arm.pattern, ctx),
            body,
        ));
    }
    out.push(')');
    out
}

/// Ascribe a match-arm body that is a literal / literal-arithmetic tree
/// so it elaborates as `BitVec N` rather than `Nat` (B-7). Width comes
/// from [`LeanExprCtx::expected_bitvec_width`] only — never guess `64`,
/// or Nat range binders in the same match (C-2 / `0..n`) disagree with
/// ascribed arms and break `List.range` elaboration.
///
/// UPSTREAM B-25: for literal-arithmetic trees, re-lower under a nat ctx
/// (no ambient BitVec ascription) before wrapping in `BitVec.ofNat` —
/// otherwise operands are already `: BitVec W` and `ofNat` sees BitVec.
fn ascribe_literal_match_arm(body: &Expr, term: &str, ctx: &LeanExprCtx<'_>) -> String {
    let Some(width) = ctx.expected_bitvec_width else {
        return term.to_string();
    };
    if is_bare_numeric_literal(body) {
        let mut nat_ctx = dup_expr_ctx(ctx);
        nat_ctx.expected_bitvec_width = None;
        let nat_term = gen_expr(body, &nat_ctx);
        format!("(({} : BitVec {}))", nat_term, width)
    } else if is_literal_nat_tree(body) {
        let mut nat_ctx = dup_expr_ctx(ctx);
        nat_ctx.expected_bitvec_width = None;
        let nat_term = gen_expr(body, &nat_ctx);
        format!("(BitVec.ofNat {} {})", width, nat_term)
    } else {
        term.to_string()
    }
}

/// Deep-clone an expression context (references are shared; owned collections
/// are cloned) so an arm/branch body can be lowered with extra pattern
/// bindings pushed without disturbing the caller's context.
fn dup_expr_ctx<'a>(ctx: &LeanExprCtx<'a>) -> LeanExprCtx<'a> {
    LeanExprCtx {
        type_ctx: clone_type_ctx(&ctx.type_ctx),
        entity: ctx.entity,
        local_enums: ctx.local_enums,
        pure_fns: ctx.pure_fns,
        lets: ctx.lets.clone(),
        route_params: ctx.route_params.clone(),
        route_name: ctx.route_name.clone(),
        phase_name: ctx.phase_name.clone(),
        route_arg_names: ctx.route_arg_names.clone(),
        in_transform: ctx.in_transform,
        state_var: ctx.state_var.clone(),
        derived_helpers: ctx.derived_helpers.clone(),
        world_var: ctx.world_var.clone(),
        instance_var: ctx.instance_var.clone(),
        qualified_instances: ctx.qualified_instances.clone(),
        deploy_bindings: ctx.deploy_bindings.clone(),
        hashmap_idents: ctx.hashmap_idents.clone(),
        nat_idents: ctx.nat_idents.clone(),
        trace_acc_var: ctx.trace_acc_var.clone(),
        expected_bitvec_width: ctx.expected_bitvec_width,
        expected_signed: ctx.expected_signed,
        expected_collection: ctx.expected_collection,
        msg_sender_override: ctx.msg_sender_override.clone(),
        pure_fn: ctx.pure_fn,
    }
}

/// Escrow-profile `match` lowering (campaign slice Э2). Emits an explicit,
/// matcher-free term for the two shapes observed across the campaign corpora,
/// and FAILS LOUD for anything else:
///
///  * **zone enum, exhaustive, no wildcard** — arms cover every constructor of
///    the scrutinee's enum exactly once. Lowered to `<Enum>.casesOn
///    (motive := fun _ => <result>) <scrut> <premise…>` with the minor premises
///    in CONSTRUCTOR declaration order (regardless of the source arm order). A
///    nullary constructor's premise is a plain term; a data constructor's is
///    `(fun <binders> => <body>)` — the `push_cases_arm` convention (B8 /
///    `local_changes.md` §10, §14). The motive is spelled EXPLICITLY: the
///    eliminator sits in `let v := (…)`, a position with no expected type, so
///    an elided motive is not inferable and the elaborator gives up with
///    "failed to elaborate eliminator, expected type is not available"
///    (campaign defect E1). `<result>` comes from
///    [`enum_match_motive_type`]; when it cannot be determined the lowering
///    fails loud rather than guessing a motive that would silently change the
///    kernel term.
///  * **integer literals + trailing catch-all** — the non-final arms are
///    integer-literal patterns and the final arm is a wildcard. Lowered to a
///    right-nested `ite` cascade `if <scrut> = <lit> then <body> else …`.
///
/// Option/Bool/tuple scrutinees, guarded arms, non-exhaustive enum matches,
/// mid-match wildcards, and `Ident` catch-alls are NOT handled → panic with a
/// diagnostic, so the gap surfaces at transpile time rather than as a matcher
/// (or a digest mismatch) later.
fn gen_match_predictable(scrut: &str, arms: &[MatchArm], ctx: &LeanExprCtx<'_>) -> String {
    if let Some(t) = try_enum_cases_on(scrut, arms, ctx) {
        return t;
    }
    if let Some(t) = try_int_literal_ite(scrut, arms, ctx) {
        return t;
    }
    panic!(
        "predictable profile: match expression falls outside the lowering grammar \
         (only exhaustive zone-enum `casesOn` and integer-literal `ite` are \
         supported; Option/Bool/tuple/guarded/non-exhaustive are not). Extend \
         gen_match_predictable or fix the source. Scrutinee: {}\n  arms: {}",
        scrut,
        arms.iter()
            .map(describe_match_pattern)
            .collect::<Vec<_>>()
            .join(" | "),
    );
}

/// Lower an exhaustive, wildcard-free match on a zone enum to `casesOn`.
/// Returns `None` (so the caller can try the next shape / fail loud) if the
/// scrutinee is not a known enum, an arm is not a variant of that enum, a
/// wildcard is present, or the arms do not cover every constructor once.
fn try_enum_cases_on(scrut: &str, arms: &[MatchArm], ctx: &LeanExprCtx<'_>) -> Option<String> {
    let en = match &arms.first()?.pattern {
        MatchPattern::EnumVariant(en, _) | MatchPattern::EnumVariantWithData(en, _, _) => {
            en.clone()
        }
        _ => return None,
    };
    let variants = lookup_enum_variants(&en, ctx)?;
    // Map each covered constructor name to its arm; reject wildcards, foreign
    // enums, and duplicate constructors.
    let mut arm_by_ctor: HashMap<&str, &MatchArm> = HashMap::new();
    for arm in arms {
        let (aen, avar) = match &arm.pattern {
            MatchPattern::EnumVariant(e, v) | MatchPattern::EnumVariantWithData(e, v, _) => (e, v),
            _ => return None,
        };
        if aen != &en {
            return None;
        }
        if arm_by_ctor.insert(avar.as_str(), arm).is_some() {
            return None;
        }
    }
    if arm_by_ctor.len() != variants.len() {
        return None;
    }
    let qual = qualify_enum_ref(&en, ctx);
    // E1: resolve the motive BEFORE lowering the arms — the result type also
    // drives the arm bodies' ambient literal width (Lean elaborates every minor
    // premise AT the motive's type, so the emitted term must agree with it).
    let (motive_ty, motive_width) = match enum_match_motive_type(&variants, &arm_by_ctor, ctx) {
        Some(m) => m,
        None => panic!(
            "predictable profile: cannot determine the result type of the `match` on \
             enum `{}`, so the `casesOn` motive is not spellable. Without an \
             explicit `(motive := fun _ => <ty>)` the eliminator does not \
             elaborate in a `let` position (campaign defect E1). Annotate an arm \
             (e.g. `<expr> as u64`) or extend `arm_result_type`.\n  arms: {}",
            en,
            arms.iter()
                .map(describe_match_pattern)
                .collect::<Vec<_>>()
                .join(" | "),
        ),
    };
    let mut body_ctx = dup_expr_ctx(ctx);
    if let Some(w) = motive_width {
        body_ctx.expected_bitvec_width = Some(w);
    }
    let ctx = &body_ctx;
    let mut premises: Vec<String> = Vec::with_capacity(variants.len());
    for v in &variants {
        let arm = arm_by_ctor.get(v.name.as_str())?;
        let prem = match &arm.pattern {
            MatchPattern::EnumVariant(_, _) => {
                // Nullary constructor: minor premise is a plain term.
                let body = gen_expr(&arm.body, ctx);
                format!("({})", ascribe_literal_match_arm(&arm.body, &body, ctx))
            }
            MatchPattern::EnumVariantWithData(_, _, parts) => {
                // Data constructor: minor premise is `(fun <binders> => body)`.
                let mut arm_ctx = dup_expr_ctx(ctx);
                push_match_pattern_bindings(&arm.pattern, &mut arm_ctx);
                let binders: Vec<String> = parts.iter().map(pattern_to_lean).collect();
                let body = gen_expr(&arm.body, &arm_ctx);
                format!(
                    "(fun {} => {})",
                    binders.join(" "),
                    ascribe_literal_match_arm(&arm.body, &body, &arm_ctx)
                )
            }
            _ => return None,
        };
        premises.push(prem);
    }
    // Variant (b): ascribe the ELIMINATOR's result instead of spelling the
    // motive. Both make the eliminator elaborate (the ascription supplies the
    // expected type the `let` position lacks), but the motive spelling leaves
    // the binder's type as the UNREDUCED `(fun _ => <ty>) <scrut>`, which the
    // predictor does not model; the ascription leaves it as the reduced `<ty>`.
    Some(format!(
        "(({}.casesOn {} {}) : {})",
        qual,
        scrut,
        premises.join(" "),
        motive_ty,
    ))
}

/// Result type of an exhaustive enum `match` lowered to `casesOn` — the body of
/// the eliminator's motive (`fun _ => <ty>`), plus its `BitVec` width when it
/// has one. `None` ⇒ the shape is not lowerable and the caller must fail loud
/// (a guessed motive would silently change the kernel term, i.e. the R3 digest).
///
/// The arms are walked in CONSTRUCTOR-DECLARATION order and the first one whose
/// type [`arm_result_type`] can see wins — the same source (and the same order)
/// the R3 predictor uses for its motive (`cambrian-predict`
/// `try_enum_cases_on_lower`: `expected` first, else the first arm's inferred
/// type). Since a well-typed `match` gives every arm the SAME type, skipping an
/// arm whose type the emitter cannot see is type-preserving; it only widens the
/// set of shapes that lower at all.
///
/// A body that is a bare numeric literal (or a literal-only arithmetic tree)
/// carries no type of its own — it inherits the ambient expected width
/// (`ascribe_literal_match_arm`), so it is skipped. When EVERY arm is such a
/// literal the ambient width is the answer, spelled exactly as
/// `ascribe_literal_match_arm` spells it (`BitVec <w>`, defaulting to Cam's
/// `u64` width) so the motive cannot contradict the premises.
///
/// The arms are scanned TWICE (campaign defect P-D):
///
///  1. with the resolution every shape that already lowers uses — pattern
///     binders, route params, entity members. Unchanged and FIRST, so no motive
///     the emitter spells today can shift;
///  2. only if the first scan saw nothing, again with the active route's local
///     `let` binders visible ([`collect_route_lets`]): an arm that forwards a
///     `let`-bound value is typed by re-reading the expression that `let` binds.
///
/// Both scans skip literal arms and walk in constructor order, so the second one
/// only widens the set of shapes that lower at all.
fn enum_match_motive_type<'a>(
    variants: &[EnumVariant],
    arm_by_ctor: &HashMap<&str, &MatchArm>,
    ctx: &LeanExprCtx<'a>,
) -> Option<(String, Option<u32>)> {
    let spell = |ty: &Type| {
        (
            lower_type(ty, &ctx.type_ctx),
            bitvec_width_of_type(ty, &ctx.type_ctx),
        )
    };
    // Scan 1 — no local `let`s.
    let (ty, all_literal) = scan_arms_for_result_type(variants, arm_by_ctor, ctx, None)?;
    if let Some(ty) = ty {
        return Some(spell(&ty));
    }
    if all_literal {
        let w = ctx.expected_bitvec_width.unwrap_or(64);
        return Some((format!("BitVec {}", w), Some(w)));
    }
    // Scan 2 — route-local `let` binders.
    let lets = collect_route_lets(ctx);
    if lets.is_empty() {
        return None;
    }
    let (ty, _) = scan_arms_for_result_type(variants, arm_by_ctor, ctx, Some(&lets))?;
    ty.as_ref().map(spell)
}

/// One constructor-order walk of the arms looking for the first one whose result
/// type [`arm_result_type`] can see. Returns `(type, all_arms_were_literal)`;
/// `None` only when an arm is missing (the caller has already checked coverage,
/// so this cannot fire in practice — it mirrors the original `?`).
fn scan_arms_for_result_type<'a>(
    variants: &[EnumVariant],
    arm_by_ctor: &HashMap<&str, &MatchArm>,
    ctx: &LeanExprCtx<'a>,
    lets: Option<&RouteLets<'a>>,
) -> Option<(Option<Type>, bool)> {
    let mut all_literal = true;
    for v in variants {
        let arm = arm_by_ctor.get(v.name.as_str())?;
        if is_bare_numeric_literal(&arm.body) || is_literal_nat_tree(&arm.body) {
            continue;
        }
        all_literal = false;
        // A data constructor's binders are typed by the constructor's declared
        // field types, so an arm that just forwards a payload resolves.
        let binders: Vec<(&str, &Type)> = match &arm.pattern {
            MatchPattern::EnumVariantWithData(_, _, parts) => parts
                .iter()
                .enumerate()
                .filter_map(|(i, p)| match p {
                    Pattern::Ident(n) => Some((n.as_str(), v.fields.get(i)?)),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        if let Some(ty) = arm_result_type(&arm.body, ctx, &binders, lets, 0) {
            return Some((Some(ty), all_literal));
        }
    }
    Some((None, all_literal))
}

/// Best-effort Cambrian type of a `casesOn` minor-premise body — the source of
/// the eliminator's motive. `binders` carries the arm's data-constructor
/// pattern binders with their DECLARED field types (source spelling, not the
/// `lean_safe_ident` rendering), so `Op::Add(a) => a` resolves.
///
/// Deliberately PARTIAL, and deliberately separate from [`bitvec_width`] (which
/// answers a different question — the width of a coercion operand — and is on
/// the legacy path): `None` means "the emitter cannot see this type", never
/// "this expression has no type". A numeric literal returns `None` because it
/// has no type of its own. Callers must fail loud, not default.
///
/// `lets` carries the active route's local `let` binders (P-D) and is `Some`
/// only on the SECOND scan of [`enum_match_motive_type`]. With it, an identifier
/// that [`LeanExprCtx::lets`] can only confirm by NAME is typed by re-reading
/// the expression its `let` binds; without it such an identifier stays `None`,
/// which is what keeps the first scan (and therefore every motive the emitter
/// spells today) byte-identical. `depth` counts `let`-hops, not expression
/// depth.
///
/// The `sys::`/`msg::` widths are the same table [`bitvec_width`] uses, so a
/// motive can never contradict an operand coercion in the same arm.
fn arm_result_type<'a>(
    expr: &Expr,
    ctx: &LeanExprCtx<'a>,
    binders: &[(&str, &Type)],
    lets: Option<&RouteLets<'a>>,
    depth: u32,
) -> Option<Type> {
    let simple = |n: &str| Some(Type::Simple(n.to_string()));
    let sub = |e: &Expr| arm_result_type(e, ctx, binders, lets, depth);
    match expr {
        Expr::Ident(name) => {
            if let Some((_, ty)) = binders.iter().find(|(n, _)| n == name) {
                return Some((*ty).clone());
            }
            // Route params of the active route (typed by signature), then
            // entity members (typed by declaration) — the `bitvec_width` order.
            if let Some(p) = ctx
                .entity
                .routes
                .iter()
                .filter(|r| Some(&r.name) == ctx.route_name.as_ref())
                .flat_map(|r| r.params.iter())
                .find(|p| &p.name == name)
            {
                return Some(p.ty.clone());
            }
            if let Some(m) = ctx.entity.members.iter().find(|m| &m.name == name) {
                return Some(m.ty.clone());
            }
            // P-D: a route-local `let` binder — typed by its bound value. Last,
            // because reaching here means the name is neither a pattern binder,
            // nor a param, nor a member.
            local_let_type(name, ctx, lets, depth)
        }
        // `^m_x` is the pre-phase value of member `m_x` — same declared type.
        Expr::TemporalRef(name) => ctx
            .entity
            .members
            .iter()
            .find(|m| &m.name == name)
            .map(|m| m.ty.clone()),
        Expr::Cast(_, ty) => Some(ty.clone()),
        Expr::BoolLiteral(_) => simple("bool"),
        Expr::StringLiteral(_) => simple("String"),
        Expr::SysField(name) => match name.as_str() {
            "timestamp" | "now" => simple("u64"),
            "chainid" | "chainId" | "blockNumber" | "block_number" | "number" | "balance" => {
                simple("U256")
            }
            "address" => simple("address"),
            _ => None,
        },
        Expr::MsgField(name) => match name.as_str() {
            "value" => simple("U256"),
            "sender" => simple("address"),
            "timestamp" => simple("u64"),
            _ => None,
        },
        // `m[k]` → the map's value type; `v[i]` → the vector's element type.
        Expr::Index(base, _) => match resolve_alias_type(sub(base)?, ctx) {
            Type::Generic(n, params) if n == "HashMap" && params.len() == 2 => {
                Some(params[1].clone())
            }
            Type::Generic(n, params) if n == "Vec" && params.len() == 1 => Some(params[0].clone()),
            _ => None,
        },
        Expr::FieldAccess(base, field) => match resolve_alias_type(sub(base)?, ctx) {
            Type::Simple(rec) => record_field_type(&rec, field, ctx),
            _ => None,
        },
        // `vec.len()` is emitted with a hardcoded `U256` ascription.
        Expr::MethodCall(_, name, args) if name == "len" && args.is_empty() => simple("U256"),
        Expr::FnCall(name, _) => ctx
            .pure_fns
            .iter()
            .find(|f| &f.name == name)
            .map(|f| f.return_type.clone()),
        Expr::BinOp(lhs, op, rhs) => match op {
            BinOp::Eq
            | BinOp::Ne
            | BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge
            | BinOp::And
            | BinOp::Or => simple("bool"),
            // Arithmetic / bitwise: the result carries the operands' type. Mixed
            // widths widen to the larger one (`gen_binop`), and a literal
            // operand has no type — so take the first operand that resolves.
            _ => sub(lhs).or_else(|| sub(rhs)),
        },
        Expr::UnaryOp(op, inner) => match op {
            UnaryOp::Not => simple("bool"),
            UnaryOp::Neg | UnaryOp::Deref => sub(inner),
        },
        Expr::If(_, then_e, Some(else_e)) => sub(then_e).or_else(|| sub(else_e)),
        // A nested `match` — SECOND SCAN ONLY, so the first scan (and with it
        // every motive the emitter spells today) is left untouched.
        Expr::Match(_, inner) if lets.is_some() => {
            nested_match_type(inner, ctx, binders, lets, depth)
        }
        // Block form `{ let r = …; r }` (how a record literal reaches a `let`)
        // and its tail — second scan only, same reason. The block's own binder
        // shadows any route-local binding of that name, so it is poisoned
        // before the tail is read.
        Expr::Let(pat, _, body) if lets.is_some() => {
            let mut inner = lets?.clone();
            poison_pattern(pat, &mut inner);
            arm_result_type(body, ctx, binders, Some(&inner), depth)
        }
        Expr::Block(items) if lets.is_some() => {
            arm_result_type(items.last()?, ctx, binders, lets, depth)
        }
        Expr::EnumVariant(en, _) | Expr::EnumVariantWithData(en, _, _)
            if !is_namespace(en) && lookup_enum_variants(en, ctx).is_some() =>
        {
            simple(en)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Route-local `let` binders for the motive scan (campaign defect P-D)
// ---------------------------------------------------------------------------

/// Where a route-local binder gets its type from.
#[derive(Clone, Copy)]
enum LetSource<'a> {
    /// `let <x> = <expr>` — the binder has the expression's type.
    Value(&'a Expr),
    /// `for <x> in <expr>` — the binder has the ELEMENT type of the iterated
    /// collection. A range (`0..n`) lowers to `List Nat`, and a map iteration
    /// binds a pair, so only a `Vec<T>` answers here (see [`local_let_type`]).
    Element(&'a Expr),
}

/// The active route's `let` binders, keyed by binder name and mapped to the
/// source of their type.
///
/// A `None` value POISONS the name: it is bound, but by a form whose type is not
/// readable from this route's source — a `var` cross-contract call (the type
/// lives in the callee's signature), a refutable / non-tuple destructuring — or
/// it is bound more than once, so no single binding answers for it. A missing
/// key and a `None` value are the same answer to [`arm_result_type`]: "cannot
/// see", i.e. fail loud, never a guess.
///
/// Collection is POSITION-BLIND — the whole route body (both branches of every
/// conditional, loop bodies, `rescue`d actions, every phase) is scanned, because
/// the motive scan has no statement cursor. The poison-on-rebinding rule is what
/// makes that safe: a name whose type could depend on where you stand is exactly
/// a name bound twice.
type RouteLets<'a> = HashMap<String, Option<LetSource<'a>>>;

/// Depth cap on `let`-chasing (`let a = b + 1; let b = <expr>`). Position-blind
/// collection means a cyclic pair of bindings — impossible in source the
/// validator accepts (`V8`, undefined identifier), but cheap to guard — cannot
/// spin the resolver.
const LET_CHAIN_DEPTH: u32 = 8;

/// Type of a route-local binder. `None` when the second scan is not running
/// (`lets = None`), when the name is poisoned/unknown, or when the chain is too
/// deep.
fn local_let_type<'a>(
    name: &str,
    ctx: &LeanExprCtx<'a>,
    lets: Option<&RouteLets<'a>>,
    depth: u32,
) -> Option<Type> {
    if depth >= LET_CHAIN_DEPTH {
        return None;
    }
    // The bound expression lives OUTSIDE the match, so the arm's pattern binders
    // are not in scope for it.
    match (*lets?.get(name)?)? {
        LetSource::Value(e) => arm_result_type(e, ctx, &[], lets, depth + 1),
        LetSource::Element(iter) => {
            match resolve_alias_type(arm_result_type(iter, ctx, &[], lets, depth + 1)?, ctx) {
                Type::Generic(n, params) if n == "Vec" && params.len() == 1 => {
                    Some(params[0].clone())
                }
                // A `HashMap` iteration binds a PAIR and a range binds a `Nat`
                // (`gen_range_list` emits `List Nat`) — neither is a Cambrian
                // type this scan can spell, so both stay fail-loud.
                _ => None,
            }
        }
    }
}

/// Type of a nested `match` used as a binder's value (P-D, second scan only).
/// Both escrow lowerings carry the arms' common type — the `casesOn` ascribed to
/// its own motive and the `ite` cascade alike — so the first arm that has a type
/// of its own answers for the whole `match`.
///
/// Arms whose pattern BINDS a name are skipped on purpose: the binder would
/// shadow a route-local binding of the same name, and the constructor's declared
/// field types are not threaded down here.
fn nested_match_type<'a>(
    arms: &[MatchArm],
    ctx: &LeanExprCtx<'a>,
    binders: &[(&str, &Type)],
    lets: Option<&RouteLets<'a>>,
    depth: u32,
) -> Option<Type> {
    arms.iter()
        .filter(|a| {
            matches!(
                a.pattern,
                MatchPattern::EnumVariant(_, _)
                    | MatchPattern::Wildcard
                    | MatchPattern::IntLiteral(_)
                    | MatchPattern::BoolLiteral(_)
                    | MatchPattern::None
            )
        })
        .filter(|a| !is_bare_numeric_literal(&a.body) && !is_literal_nat_tree(&a.body))
        .find_map(|a| arm_result_type(&a.body, ctx, binders, lets, depth))
}

/// Collect [`RouteLets`] for the route named by `ctx.route_name`. Empty when
/// there is no active route (top-level pure-fn / constant bodies) or the route
/// binds nothing.
fn collect_route_lets<'a>(ctx: &LeanExprCtx<'a>) -> RouteLets<'a> {
    let mut out: RouteLets<'a> = HashMap::new();
    let Some(route) = ctx
        .route_name
        .as_ref()
        .and_then(|n| ctx.entity.routes.iter().find(|r| &r.name == n))
    else {
        return out;
    };
    for action in route.body.all_actions() {
        collect_action_lets(action, &mut out);
    }
    out
}

/// Walk one route action (and everything nested in it) for `let` binders.
fn collect_action_lets<'a>(action: &'a RouteAction, out: &mut RouteLets<'a>) {
    match action {
        RouteAction::Let { pattern, value } => bind_let_pattern(pattern, value, out),
        // `var v = msg(…) ~> dest` binds a cross-contract result typed by the
        // CALLEE's signature — poisoned rather than guessed.
        RouteAction::VarCall { name, .. } => insert_route_let(out, name, None),
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            for a in then_actions.iter().chain(else_actions.iter()) {
                collect_action_lets(a, out);
            }
        }
        RouteAction::For {
            pattern,
            iter,
            body,
        } => {
            match pattern {
                Pattern::Ident(n) => insert_route_let(out, n, Some(LetSource::Element(iter))),
                // A tuple binder is a map iteration (`for (k, v) in m`): its
                // components are not read here.
                _ => poison_pattern(pattern, out),
            }
            for a in body {
                collect_action_lets(a, out);
            }
        }
        RouteAction::Rescue { action, .. } => collect_action_lets(action, out),
        _ => {}
    }
}

/// Bind one `let <pat> = <value>` into [`RouteLets`].
fn bind_let_pattern<'a>(pat: &Pattern, value: &'a Expr, out: &mut RouteLets<'a>) {
    match pat {
        Pattern::Ident(n) => insert_route_let(out, n, Some(LetSource::Value(value))),
        // `let (a, b) = (e₁, e₂)` is positional, so each binder is typed by its
        // own component. Destructuring anything else (a call, a member, a tuple
        // of a different arity) is not read component-wise here.
        Pattern::Tuple(parts) => match value {
            Expr::Tuple(items) if items.len() == parts.len() => {
                for (p, v) in parts.iter().zip(items.iter()) {
                    bind_let_pattern(p, v, out);
                }
            }
            _ => poison_pattern(pat, out),
        },
        // Refutable / deref patterns bind the payload of an `Option`, whose type
        // is the option's parameter — not read here.
        Pattern::Some(_) | Pattern::Deref(_) => poison_pattern(pat, out),
        Pattern::Wildcard | Pattern::None => {}
    }
}

fn poison_pattern(pat: &Pattern, out: &mut RouteLets<'_>) {
    for n in pattern_binder_names(pat) {
        insert_route_let(out, &n, None);
    }
}

/// Insert a binder, POISONING it (`None`) when the name is already bound —
/// see [`RouteLets`].
fn insert_route_let<'a>(out: &mut RouteLets<'a>, name: &str, value: Option<LetSource<'a>>) {
    use std::collections::hash_map::Entry;
    match out.entry(name.to_string()) {
        Entry::Occupied(mut e) => {
            e.insert(None);
        }
        Entry::Vacant(e) => {
            e.insert(value);
        }
    }
}

/// Every identifier a pattern binds, in source order.
fn pattern_binder_names(pat: &Pattern) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(p: &Pattern, out: &mut Vec<String>) {
        match p {
            Pattern::Ident(n) => out.push(n.clone()),
            Pattern::Tuple(parts) => {
                for x in parts {
                    walk(x, out);
                }
            }
            Pattern::Some(inner) | Pattern::Deref(inner) => walk(inner, out),
            Pattern::Wildcard | Pattern::None => {}
        }
    }
    walk(pat, &mut out);
    out
}

/// Inline a `type Alias = T` chain (entity-local first, then program scope) so
/// structural lookups see the underlying type. Non-alias types pass through.
fn resolve_alias_type(ty: Type, ctx: &LeanExprCtx<'_>) -> Type {
    let Type::Simple(name) = &ty else { return ty };
    let alias = ctx
        .type_ctx
        .local_aliases
        .iter()
        .find(|a| &a.name == name)
        .or_else(|| {
            ctx.type_ctx
                .program
                .type_aliases
                .iter()
                .find(|a| &a.name == name)
        });
    match alias {
        Some(a) => resolve_alias_type(a.ty.clone(), ctx),
        None => ty,
    }
}

/// Declared type of `<record>.<field>`, entity-local records shadowing
/// program-scope ones (mirrors `lean_types::resolve_user_type`).
fn record_field_type(rec: &str, field: &str, ctx: &LeanExprCtx<'_>) -> Option<Type> {
    ctx.type_ctx
        .local_records
        .iter()
        .find(|r| r.name == rec)
        .or_else(|| ctx.type_ctx.program.records.iter().find(|r| r.name == rec))?
        .fields
        .iter()
        .find(|f| f.name == field)
        .map(|f| f.ty.clone())
}

/// Lower a match whose non-final arms are integer literals and whose final arm
/// is a wildcard catch-all to a right-nested `ite` cascade. Returns `None` for
/// any other shape.
fn try_int_literal_ite(scrut: &str, arms: &[MatchArm], ctx: &LeanExprCtx<'_>) -> Option<String> {
    if arms.len() < 2 {
        return None;
    }
    let (last, lits) = arms.split_last()?;
    // The final arm must be a bare wildcard (the `else`). An `Ident` catch-all
    // would need the scrutinee bound into the branch — not observed, kept
    // fail-loud.
    let default_body = match &last.pattern {
        MatchPattern::Wildcard => gen_expr(&last.body, ctx),
        _ => return None,
    };
    let mut clauses: Vec<(u128, String)> = Vec::with_capacity(lits.len());
    for arm in lits {
        match &arm.pattern {
            MatchPattern::IntLiteral(n) => {
                if !n.fits_u128() {
                    return None;
                }
                clauses.push((n.lo, gen_expr(&arm.body, ctx)));
            }
            _ => return None,
        }
    }
    if clauses.is_empty() {
        return None;
    }
    let mut out = default_body;
    for (n, body) in clauses.iter().rev() {
        out = format!("if {} = {} then {} else {}", scrut, n, body, out);
    }
    Some(out)
}

/// Look up an enum's constructors (in declaration order) by name, preferring
/// an entity-local enum over a program-scope one (mirrors `qualify_enum_ref`'s
/// shadowing).
fn lookup_enum_variants(en: &str, ctx: &LeanExprCtx<'_>) -> Option<Vec<EnumVariant>> {
    if let Some(e) = ctx.local_enums.iter().find(|e| e.name == en) {
        return Some(e.variants.clone());
    }
    if let Some(e) = ctx.type_ctx.program.enums.iter().find(|e| e.name == en) {
        return Some(e.variants.clone());
    }
    None
}

/// One-line description of a match arm's pattern, for the fail-loud diagnostic.
fn describe_match_pattern(arm: &MatchArm) -> String {
    match &arm.pattern {
        MatchPattern::EnumVariant(e, v) => format!("{}::{}", e, v),
        MatchPattern::EnumVariantWithData(e, v, p) => format!("{}::{}({})", e, v, p.len()),
        MatchPattern::Wildcard => "_".to_string(),
        MatchPattern::IntLiteral(n) => n.to_string(),
        MatchPattern::BoolLiteral(b) => b.to_string(),
        MatchPattern::Ident(n) => n.clone(),
        MatchPattern::Some(_) => "some(..)".to_string(),
        MatchPattern::None => "none".to_string(),
    }
}

/// Requalify a bare enum name for the active emission profile (B6).
///
/// Mirrors `resolve_user_type`'s shadowing: an entity-local enum shadows a
/// program-scope one and keeps its existing (bare) spelling — B6 only
/// relocates *program-scope* (top-level, non-entity) types. A program-scope
/// enum nests under `Cambrian.Generated.Types` under predictable profile and stays bare
/// under legacy (byte-identical). Applied to enum-variant literals and
/// match patterns so they resolve to the type's new home.
fn qualify_enum_ref(en: &str, ctx: &LeanExprCtx<'_>) -> String {
    if ctx.local_enums.iter().any(|e| e.name == en) {
        let local = lean_local_type_name(en);
        return match ctx.type_ctx.entity_name {
            Some(entity) if is_generated_type_name(&local) => format!("{}.{}", entity, local),
            _ => local,
        };
    }
    if ctx.type_ctx.program.enums.iter().any(|e| e.name == en) {
        return program_type_ref(en, ctx.type_ctx.use_predictable_profile);
    }
    en.to_string()
}

fn match_pattern_to_lean(pat: &MatchPattern, ctx: &LeanExprCtx<'_>) -> String {
    match pat {
        MatchPattern::EnumVariant(en, variant) => {
            format!(
                "{}.{}",
                qualify_enum_ref(en, ctx),
                sanitize_variant(variant)
            )
        }
        MatchPattern::EnumVariantWithData(en, variant, parts) => {
            let bound: Vec<String> = parts.iter().map(pattern_to_lean).collect();
            format!(
                "{}.{} {}",
                qualify_enum_ref(en, ctx),
                sanitize_variant(variant),
                bound.join(" "),
            )
        }
        MatchPattern::Wildcard => "_".to_string(),
        MatchPattern::IntLiteral(n) => n.to_string(),
        MatchPattern::BoolLiteral(b) => if *b { "true" } else { "false" }.to_string(),
        MatchPattern::Ident(name) => super::core::types::lean_safe_ident(name),
        MatchPattern::Some(inner) => {
            format!("(Option.some {})", pattern_to_lean(inner))
        }
        MatchPattern::None => "Option.none".to_string(),
    }
}

fn push_match_pattern_bindings(pat: &MatchPattern, ctx: &mut LeanExprCtx<'_>) {
    match pat {
        MatchPattern::Ident(name) => {
            ctx.lets.insert(name.clone());
        }
        MatchPattern::EnumVariantWithData(_, _, parts) => {
            for p in parts {
                ctx.push_pattern(p);
            }
        }
        MatchPattern::Some(inner) => ctx.push_pattern(inner),
        _ => {}
    }
}

pub(crate) fn pattern_to_lean(pat: &Pattern) -> String {
    match pat {
        // T-LEAN-EX-010: sanitize reserved binders (`s`/`w`/`inst`/`ctx`)
        // so route `let s = …` does not shadow the State binder used in
        // writeback (`withExprProbe w inst s`).
        Pattern::Ident(name) => super::core::types::lean_safe_ident(name),
        Pattern::Wildcard => "_".to_string(),
        Pattern::Tuple(parts) => {
            let inner: Vec<String> = parts.iter().map(pattern_to_lean).collect();
            format!("({})", inner.join(", "))
        }
        // Refutable Option patterns are not valid Lean `let` binders
        // (exhaustiveness). Use [`lower_let_binding`] for statement lets;
        // keep the structural rendering for match-arm / nested contexts.
        Pattern::Some(inner) => format!("(Option.some {})", pattern_to_lean(inner)),
        Pattern::None => "Option.none".to_string(),
        Pattern::Deref(inner) => pattern_to_lean(inner),
    }
}

/// The predictable-profile replacement for a `let none = <e>;` statement (campaign
/// defect L4). A BLOCK comment, so it is safe both on a line of its own
/// (statement position, [`lower_let_binding`]) and inline inside a term
/// (expression position, [`gen_let`]) — a `--` comment would swallow the rest
/// of the line in the latter. It deliberately spells NEITHER of the tokens the
/// slice gates grep for (`match` / `panic!` / `panicWithPos`), so the comment
/// can never turn a future forbidden-token scan into a false positive.
const LET_NONE_DROPPED_COMMENT: &str =
    "/- predictable: `let none = …` contributes no term (vacuous guard: `_` binder, both arms `()`) -/";

/// Lower `let pat = value` in a route body. When `fail_mode` is set, the
/// RHS uses [`gen_expr_as_route_result`] (PW3-O-006 / T-ARCH-001) so
/// checked div/overflow in intermediate lets propagate like `return`.
pub(crate) fn lower_fail_mode_let_stmts(
    pat: &Pattern,
    value: &Expr,
    ctx: &LeanExprCtx<'_>,
    fail_mode: bool,
) -> Vec<String> {
    if !fail_mode {
        return vec![lower_let_binding(pat, &gen_expr(value, ctx))];
    }
    let rr = gen_expr_as_route_result(value, ctx);
    if let Some(inner) = strip_pure_wrap(&rr) {
        vec![lower_let_binding(pat, inner)]
    } else {
        vec![
            format!("let __cam_let_rhs ← {}", rr.trim()),
            lower_let_binding(pat, "__cam_let_rhs"),
        ]
    }
}

/// Lower a Cam `let <pat> = <value>` statement to a Lean binding (no
/// trailing newline). Refutable `some`/`none` patterns cannot be Lean
/// `let` binders; they become fail-loud match/`get!` forms. Irrefutable
/// patterns (`ident` / `*` / tuple) stay ordinary `let`.
pub(crate) fn lower_let_binding(pat: &Pattern, value_term: &str) -> String {
    match pat {
        Pattern::Some(inner) => {
            let binder = pattern_to_lean(inner);
            format!("let {} := ({}).get!", binder, value_term)
        }
        // Escrow (campaign defect L4) — see the twin arm in [`gen_let`]: the
        // legacy guard is a no-op in the pure model, so under the escrow
        // profile it contributes no term (and neither a `match_1` matcher nor
        // the `panicWithPosWithDecl` file/line/column literals it drags in).
        Pattern::None if crate::codegen::lean::use_predictable_profile() => {
            LET_NONE_DROPPED_COMMENT.to_string()
        }
        Pattern::None => format!(
            "let _ := match {} with | none => () | some _ => (panic! \"let none matched some\" : Unit)",
            value_term
        ),
        Pattern::Deref(inner) => lower_let_binding(inner, value_term),
        other => format!("let {} := {}", pattern_to_lean(other), value_term),
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn qualify_user_type(name: &str, ctx: &LeanTypeCtx<'_>) -> String {
    if ctx.local_records.iter().any(|r| r.name == name)
        || ctx.local_enums.iter().any(|e| e.name == name)
    {
        match ctx.entity_name {
            Some(entity) => format!("{}.{}", entity, lean_local_type_name(name)),
            None => lean_local_type_name(name),
        }
    } else if ctx.program.records.iter().any(|r| r.name == name)
        || ctx.program.enums.iter().any(|e| e.name == name)
    {
        // Program-scope (top-level, non-entity) user type. Under the escrow
        // emission profile (B6, spec §15.11) it nests under
        // `Cambrian.Generated.Types`; under legacy it stays bare
        // (byte-identical, prefix is empty). Mirrors `resolve_user_type`
        // (type position) and `qualify_enum_ref` (enum literals) so the
        // record-construct ascription emitted by `gen_record_ctor` resolves
        // to the type's relocated home instead of `Unknown identifier`.
        program_type_ref(name, ctx.use_predictable_profile)
    } else {
        name.to_string()
    }
}

fn clone_type_ctx<'a>(src: &LeanTypeCtx<'a>) -> LeanTypeCtx<'a> {
    LeanTypeCtx {
        program: src.program,
        proof_helpers: src.proof_helpers,
        use_nat_numerics: src.use_nat_numerics,
        overflow_panic: src.overflow_panic,
        use_predictable_profile: src.use_predictable_profile,
        deterministic_addresses: src.deterministic_addresses,
        entity_name: src.entity_name,
        local_records: src.local_records,
        local_enums: src.local_enums,
        local_aliases: src.local_aliases,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Entity, Program, Span};
    use cambrian_core::U256;

    fn dummy_program() -> Program {
        Program {
            imports: vec![],
            file_imports: vec![],
            pure_fns: vec![],
            type_aliases: vec![],
            records: vec![],
            enums: vec![],
            entities: vec![],
            extern_entities: vec![],
            tests: vec![],
            properties: vec![],
            fuzz_tests: vec![],
            invariants: vec![],
            events: vec![],
            errors: vec![],
            libraries: vec![],
            using_decls: vec![],
        }
    }

    fn dummy_entity(name: &str) -> Entity {
        Entity {
            name: name.into(),
            records: vec![],
            enums: vec![],
            type_aliases: vec![],
            constants: vec![],
            macros: vec![],
            routes: vec![],
            members: vec![],
            events: vec![],
            errors: vec![],
            span: Span::none(),
        }
    }

    fn ctx_for<'a>(p: &'a Program, e: &'a Entity) -> LeanExprCtx<'a> {
        LeanExprCtx {
            type_ctx: LeanTypeCtx::for_entity(
                p,
                &e.name,
                &e.records,
                &e.enums,
                &e.type_aliases,
                crate::codegen::lean::LeanProfile::DEFAULT,
            ),
            entity: e,
            local_enums: &e.enums,
            pure_fns: &p.pure_fns,
            lets: HashSet::new(),
            route_params: HashSet::new(),
            route_name: None,
            phase_name: None,
            route_arg_names: vec![],
            in_transform: false,
            state_var: "s".to_string(),
            derived_helpers: HashSet::new(),
            world_var: None,
            instance_var: None,
            qualified_instances: HashMap::new(),
            deploy_bindings: HashMap::new(),
            hashmap_idents: HashSet::new(),
            nat_idents: HashSet::new(),
            trace_acc_var: None,
            expected_bitvec_width: None,
            expected_signed: None,
            expected_collection: None,
            msg_sender_override: None,
            pure_fn: None,
        }
    }

    #[test]
    fn binop_addition_lowers_to_lean_plus() {
        let p = dummy_program();
        let e = dummy_entity("E");
        let ctx = ctx_for(&p, &e);
        let expr = Expr::BinOp(
            Box::new(Expr::IntLiteral(U256::ONE)),
            BinOp::Add,
            Box::new(Expr::IntLiteral(U256::from_u128(2))),
        );
        let s = gen_expr(&expr, &ctx);
        assert!(s.contains(" + "), "expected '+' in {}", s);
    }

    #[test]
    fn member_ident_resolves_to_state_field() {
        let p = dummy_program();
        let mut e = dummy_entity("Counter");
        e.members.push(crate::ast::Member {
            name: "m_count".into(),
            ty: Type::Simple("u64".into()),
            is_identity: false,
            default_value: None,
            transforms: vec![],
            span: Span::none(),
        });
        let ctx = ctx_for(&p, &e);
        let s = gen_expr(&Expr::Ident("m_count".into()), &ctx);
        assert_eq!(s, "s.m_count");
    }

    #[test]
    fn route_param_takes_precedence_over_member() {
        let p = dummy_program();
        let mut e = dummy_entity("Counter");
        e.members.push(crate::ast::Member {
            name: "amount".into(),
            ty: Type::Simple("u64".into()),
            is_identity: false,
            default_value: None,
            transforms: vec![],
            span: Span::none(),
        });
        let mut ctx = ctx_for(&p, &e);
        ctx.route_params.insert("amount".into());
        let s = gen_expr(&Expr::Ident("amount".into()), &ctx);
        assert_eq!(s, "amount");
    }

    #[test]
    fn temporal_ref_emits_member_call_when_member_has_transform_at_phase() {
        let p = dummy_program();
        let mut e = dummy_entity("TwoMember");
        e.members.push(crate::ast::Member {
            name: "m_a".into(),
            ty: Type::Simple("u64".into()),
            is_identity: false,
            default_value: None,
            transforms: vec![crate::ast::MemberTransform {
                route_name: "bump".into(),
                params: vec![Pattern::Ident("amount".into())],
                body: Expr::IntLiteral(U256::ZERO),
                phase: Some("inc".into()),
                span: Span::none(),
            }],
            span: Span::none(),
        });
        let mut ctx = ctx_for(&p, &e);
        ctx.route_name = Some("bump".into());
        ctx.phase_name = Some("inc".into());
        ctx.route_arg_names = vec!["amount".into()];
        let s = gen_expr(&Expr::TemporalRef("m_a".into()), &ctx);
        assert!(s.contains("TwoMember.Members.M_m_a.bump_inc"), "got {}", s);
        assert!(s.contains("amount"), "got {}", s);
    }

    #[test]
    fn temporal_ref_falls_back_to_state_field_when_no_transform_at_phase() {
        let p = dummy_program();
        let mut e = dummy_entity("TwoMember");
        // m_a only has a transform in the `inc` phase.
        e.members.push(crate::ast::Member {
            name: "m_a".into(),
            ty: Type::Simple("u64".into()),
            is_identity: false,
            default_value: None,
            transforms: vec![crate::ast::MemberTransform {
                route_name: "bump".into(),
                params: vec![Pattern::Ident("amount".into())],
                body: Expr::IntLiteral(U256::ZERO),
                phase: Some("inc".into()),
                span: Span::none(),
            }],
            span: Span::none(),
        });
        // Active phase is `mirror` — m_a has no transform here.
        let mut ctx = ctx_for(&p, &e);
        ctx.route_name = Some("bump".into());
        ctx.phase_name = Some("mirror".into());
        ctx.route_arg_names = vec!["amount".into()];
        let s = gen_expr(&Expr::TemporalRef("m_a".into()), &ctx);
        assert_eq!(s, "s.m_a");
    }

    #[test]
    fn msg_sender_lowers_to_ctx_field() {
        let p = dummy_program();
        let e = dummy_entity("E");
        let ctx = ctx_for(&p, &e);
        let s = gen_expr(&Expr::MsgField("sender".into()), &ctx);
        assert_eq!(s, "ctx.sender");
    }
}
