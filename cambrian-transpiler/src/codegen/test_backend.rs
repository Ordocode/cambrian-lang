// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Shared scaffolding for the test-codegen backends.
//!
//! Three backends emit test code from `TestDecl` / `FuzzDecl` /
//! `InvariantDecl`:
//!
//!   * [`super::evm_test_codegen`] — Foundry (`forge test` / `forge invariant`)
//!     Solidity test contracts.
//!   * [`super::evm_revm_test_codegen`] — `revm-tests/` Rust crate driving
//!     the compiled Solidity through `revm` in-memory.
//!   * [`super::test_codegen`] — Acki Nacki host-Rust tests against the
//!     WASM crate's `execute_impl`.
//!
//! Each backend has its own host-language idioms and code shape, so most of
//! the lowering remains backend-private. What lives here is the *truly
//! shared* shape:
//!
//!   * [`PostCallAssert`] / [`collect_post_call_assertions`] — the regular
//!     pattern of "a `call route(args)` step is followed by zero or more
//!     `expect_*` steps that bind to that call's outcome".
//!   * [`PendingReturn`] — the three return-shape variants every backend
//!     has to handle (scalar, tuple, lens).
//!   * [`lower_check_expr_multi`] / [`substitute_member_accessors`] — the
//!     two utilities the Foundry backend exports for use by extension
//!     points (the old Echidna pipeline relied on these too; future
//!     test-emitter additions can reuse them).
//!   * [`TestStepLowerer`] / [`InvariantLowerer`] traits — the API surface
//!     new backends and Foundry-side feature additions implement.
//!
//! The traits are intentionally narrow: they describe *step lowering*
//! (one method per `TestStep` variant, plus the new `skip if`,
//! `advance_time` extensions) and *invariant emission* (the surrounding
//! handler / setup / check structure). Backends are free to implement only
//! the variants they support and `unimplemented!` or no-op the rest.

use crate::ast::*;

// ===========================================================================
// Trace-aware invariant accessors (`trace::length` / `count` / `lastWas`)
// ===========================================================================

/// Which `trace::*` accessors an invariant actually uses. Computed by
/// scanning the surviving actions' `assume` conditions and the
/// invariant's `check` expressions. Backends use this to emit only the
/// counters / flags that are referenced.
#[derive(Default, Clone)]
pub struct TraceFeatures {
    pub uses_length: bool,
    pub counted: Vec<String>,
    pub uses_last: bool,
}

impl TraceFeatures {
    pub fn is_empty(&self) -> bool {
        !self.uses_length && self.counted.is_empty() && !self.uses_last
    }
    fn add_count(&mut self, route: &str) {
        if !self.counted.iter().any(|r| r == route) {
            self.counted.push(route.to_string());
        }
    }
}

fn collect_trace_features(expr: &Expr, f: &mut TraceFeatures) {
    match expr {
        Expr::TraceField(field) => {
            if field == "length" {
                f.uses_length = true;
            }
        }
        Expr::TraceCall { name, route } => match name.as_str() {
            "count" => f.add_count(route),
            "lastWas" => f.uses_last = true,
            _ => {}
        },
        Expr::BinOp(l, _, r) | Expr::Index(l, r) | Expr::Range(l, r) => {
            collect_trace_features(l, f);
            collect_trace_features(r, f);
        }
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) | Expr::Some(e) => {
            collect_trace_features(e, f)
        }
        Expr::FnCall(_, args)
        | Expr::ArrayLit(args)
        | Expr::Tuple(args)
        | Expr::EnumVariantWithData(_, _, args)
        | Expr::MacroRef(_, args)
        | Expr::NamespacedCall { args, .. } => {
            for a in args {
                collect_trace_features(a, f);
            }
        }
        Expr::MethodCall(recv, _, args) => {
            collect_trace_features(recv, f);
            for a in args {
                collect_trace_features(a, f);
            }
        }
        Expr::If(c, t, e) => {
            collect_trace_features(c, f);
            collect_trace_features(t, f);
            if let Some(e) = e {
                collect_trace_features(e, f);
            }
        }
        Expr::Let(_, v, b) => {
            collect_trace_features(v, f);
            collect_trace_features(b, f);
        }
        Expr::Block(items) => {
            for it in items {
                collect_trace_features(it, f);
            }
        }
        Expr::Match(s, arms) => {
            collect_trace_features(s, f);
            for arm in arms {
                collect_trace_features(&arm.body, f);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                collect_trace_features(v, f);
            }
        }
        Expr::RecordUpdate(b, fields) => {
            collect_trace_features(b, f);
            for (_, v) in fields {
                collect_trace_features(v, f);
            }
        }
        Expr::Closure(_, b) => collect_trace_features(b, f),
        Expr::For(_, it, b) => {
            collect_trace_features(it, f);
            collect_trace_features(b, f);
        }
        _ => {}
    }
}

/// Scan an invariant's `assume` conditions and `check` expressions for
/// `trace::*` usage. `actions` is the (possibly exclude-filtered) action
/// set the backend will actually emit.
pub fn scan_invariant_trace_features(inv: &InvariantDecl) -> TraceFeatures {
    let mut f = TraceFeatures::default();
    for action in &inv.actions {
        if inv.exclude_selectors.iter().any(|s| s == &action.route) {
            continue;
        }
        for step in &action.body {
            if let TestStep::Assume { cond } = step {
                collect_trace_features(cond, &mut f);
            }
        }
    }
    for c in &inv.checks {
        collect_trace_features(c, &mut f);
    }
    f
}

// ===========================================================================
// PostCallAssert / PendingReturn — shared post-call shape
// ===========================================================================

/// Captures the right-hand side of a `call route(args) ; expect_*` block:
/// every backend processes the trailing `expect_*` steps as belonging to
/// the preceding `Call`.
#[derive(Debug, Clone)]
pub enum PostCallAssert {
    State(Vec<(FieldPath, Expr)>),
    Throw(u32),
    Return(Expr),
    ReturnTuple(Vec<Expr>),
    ReturnLens(FieldPath, Expr),
    Effects(Vec<TestEffectElement>),
    /// `expect <bool>` grouped with the other post-call expects.
    Pred(Expr),
}

/// Walk the body starting at `start` and collect every contiguous
/// `expect_*` step into a list of [`PostCallAssert`]s. The first
/// non-expect step (or end of body) terminates the run.
///
/// The number of consumed steps is `result.len()`; callers should advance
/// their cursor by `1 + result.len()` (1 for the originating `Call`).
pub fn collect_post_call_assertions(body: &[TestStep], start: usize) -> Vec<PostCallAssert> {
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

/// The three return-binding shapes every backend lowers `expect_return`
/// into. `Lens` allows asserting on a nested field of a record/tuple
/// returned by the route.
#[derive(Debug, Clone)]
pub enum PendingReturn {
    Scalar(Expr),
    Tuple(Vec<Expr>),
    Lens(FieldPath, Expr),
}

// ===========================================================================
// TestStepLowerer trait
// ===========================================================================

/// Per-backend lowerer for a single `TestStep`. Implementors emit into a
/// backend-specific buffer (passed as `&mut self` state) and return when
/// done.
///
/// Callers drive this trait via [`walk_test_body`].
///
/// The trait is intentionally generous: every method has a default no-op
/// body so backends only override the ones they support. New steps added
/// here (e.g. [`TestStepLowerer::emit_skip_if`]) won't break existing
/// backends — they'll just no-op the new step until upgraded.
#[allow(unused_variables)]
pub trait TestStepLowerer {
    fn emit_bound(&mut self, var: &str, lo: &Expr, hi: &Expr, inclusive: bool) {}
    fn emit_assume(&mut self, cond: &Expr) {}
    fn emit_let(&mut self, name: &str, value: &Expr) {}
    fn emit_set_context(&mut self, namespace: &str, fields: &[(String, Expr)]) {}
    fn emit_set_registry(
        &mut self,
        entity_name: &str,
        code_hash: &Expr,
        code_depth: &Expr,
        wasm_hash: &Expr,
    ) {
    }
    fn emit_call(&mut self, route: &str, args: &[Expr], assertions: &[PostCallAssert]) {}
    fn emit_expect_state(&mut self, fields: &[(FieldPath, Expr)]) {}

    /// `skip if <cond>;` — early-return-without-rejection. Lowers to
    /// `if (!cond) return;` on Foundry handler actions; no-op elsewhere.
    fn emit_skip_if(&mut self, cond: &Expr) {}

    /// `advanceTime(secs)` builtin — lowers to `vm.warp + vm.roll` on
    /// Foundry; no-op elsewhere.
    fn emit_advance_time(&mut self, secs: &Expr) {}
}

/// Drive a `TestStep` body through a [`TestStepLowerer`]. Handles the
/// `Call` + trailing `expect_*` aggregation: the lowerer's
/// [`TestStepLowerer::emit_call`] receives the assertions list directly,
/// so it doesn't have to re-walk the body.
///
/// Stand-alone `expect_*` steps (without a preceding `call`) are
/// tolerated and silently skipped — the validator already rejects them
/// in regular `test` blocks; they appear inside `gen_invariant_*` action
/// bodies as no-ops there.
pub fn walk_test_body(body: &[TestStep], lowerer: &mut dyn TestStepLowerer) {
    let mut i = 0;
    while i < body.len() {
        match &body[i] {
            TestStep::Bound {
                var,
                lo,
                hi,
                inclusive,
            } => {
                lowerer.emit_bound(var, lo, hi, *inclusive);
                i += 1;
            }
            TestStep::Assume { cond } => {
                lowerer.emit_assume(cond);
                i += 1;
            }
            TestStep::Let { name, ty: _, value } => {
                lowerer.emit_let(name, value);
                i += 1;
            }
            TestStep::SetContext { namespace, fields } => {
                lowerer.emit_set_context(namespace, fields);
                i += 1;
            }
            TestStep::SetRegistry {
                entity_name,
                code_hash,
                code_depth,
                wasm_hash,
            } => {
                lowerer.emit_set_registry(entity_name, code_hash, code_depth, wasm_hash);
                i += 1;
            }
            TestStep::Call { target: Some(_), .. } => {
                // Peer calls need a second deployed contract, which this
                // lowerer has no notion of. The validator rejects the pair
                // for non-EVM targets, so reaching here means a new backend
                // opted in without wiring it.
                i += 1;
            }
            TestStep::Call { target: None, route, args } => {
                let assertions = collect_post_call_assertions(body, i + 1);
                lowerer.emit_call(route, args, &assertions);
                i += 1 + assertions.len();
            }
            TestStep::DeployPeer { .. } => {
                i += 1;
            }
            TestStep::ExpectState { fields } => {
                lowerer.emit_expect_state(fields);
                i += 1;
            }
            TestStep::SkipIf { cond } => {
                lowerer.emit_skip_if(cond);
                i += 1;
            }
            TestStep::AdvanceTime { secs } => {
                lowerer.emit_advance_time(secs);
                i += 1;
            }
            TestStep::ExpectThrow { .. }
            | TestStep::ExpectReturn { .. }
            | TestStep::ExpectReturnTuple { .. }
            | TestStep::ExpectReturnLens { .. }
            | TestStep::ExpectPred { .. }
            | TestStep::ExpectEffects { .. }
            | TestStep::ExpectEmit { .. } => {
                // Stand-alone expect_*: no-op (the preceding-call form is
                // consumed under TestStep::Call above).
                i += 1;
            }
        }
    }
}

// ===========================================================================
// InvariantLowerer trait
// ===========================================================================

/// Per-backend lowerer for an `invariant { ... }` declaration.
///
/// Backends emit a handler-style structure (regardless of host language):
///
///   * a *constructor* that captures the system-under-test's handle(s),
///     applies any `setup { ... }` snapshot bindings, and remembers them
///     for the lifetime of the test;
///   * one *action wrapper* per declared `action` clause, optionally
///     guarded by the per-backend "swallow reverts when not
///     `fail_on_revert`" rule;
///   * zero or more *queries* (Foundry-only today) — pure view helpers
///     callable from `check` clauses;
///   * one *check* function per `check` clause.
///
/// The trait surface mirrors that structure. Backends can implement only
/// the methods they support; the orchestration order is the responsibility
/// of the per-backend caller (see Foundry's `gen_invariant_file`).
#[allow(unused_variables)]
pub trait InvariantLowerer {
    fn emit_setup_snapshot(&mut self, items: &[InvariantSetupBinding]) {}
    fn emit_query(&mut self, q: &InvariantQuery) {}
    fn emit_action_wrapper(&mut self, action: &InvariantAction, fail_on_revert: bool) {}
    fn emit_check(&mut self, idx: usize, expr: &Expr) {}
}

// ===========================================================================
// Shared expression-rewriting utilities
// ===========================================================================

/// Lower a Cambrian `check` expression in the *multi-instance* invariant
/// scope to a Solidity-compatible expression by walking the AST. References
/// of the form `<inst>.<member>` (parsed as `FieldAccess(Ident(inst), member)`)
/// lower to `_<inst>.<member>()`. Any other sub-expression is delegated to
/// the generic `gen_expr` (whose Rust-flavoured output happens to be a
/// valid Solidity expression for the operators we use here — arithmetic,
/// comparison, logical connectors, integer/hex/bool literals).
pub fn lower_check_expr_multi(expr: &Expr, entity_for_inst: &[(String, &Entity)]) -> String {
    use crate::ast::{BinOp, UnaryOp};
    fn is_inst(name: &str, ents: &[(String, &Entity)]) -> bool {
        ents.iter().any(|(n, _)| n == name)
    }
    match expr {
        Expr::FieldAccess(inner, member) => {
            if let Expr::Ident(name) = inner.as_ref() {
                if is_inst(name, entity_for_inst) {
                    return format!("_{}.{}()", name, member);
                }
            }
            let recv = lower_check_expr_multi(inner, entity_for_inst);
            format!("{}.{}", recv, member)
        }
        Expr::BinOp(l, op, r) => {
            let ls = lower_check_expr_multi(l, entity_for_inst);
            let rs = lower_check_expr_multi(r, entity_for_inst);
            let op_str = match op {
                BinOp::Add | BinOp::WrappingAdd => "+",
                BinOp::Sub | BinOp::WrappingSub => "-",
                BinOp::Mul | BinOp::WrappingMul => "*",
                BinOp::Div => "/",
                BinOp::Mod => "%",
                BinOp::Eq => "==",
                BinOp::Ne => "!=",
                BinOp::Lt => "<",
                BinOp::Le => "<=",
                BinOp::Gt => ">",
                BinOp::Ge => ">=",
                BinOp::And => "&&",
                BinOp::Or => "||",
                BinOp::BitAnd => "&",
                BinOp::BitOr => "|",
                BinOp::BitXor => "^",
                BinOp::Shl => "<<",
                BinOp::Shr => ">>",
            };
            format!("({} {} {})", ls, op_str, rs)
        }
        Expr::UnaryOp(op, inner) => {
            let s = lower_check_expr_multi(inner, entity_for_inst);
            match op {
                UnaryOp::Not => format!("!({})", s),
                UnaryOp::Neg => format!("-({})", s),
                UnaryOp::Deref => s,
            }
        }
        Expr::Index(base, key) => {
            let k = lower_check_expr_multi(key, entity_for_inst);
            if let Expr::FieldAccess(inner, member) = base.as_ref() {
                if let Expr::Ident(name) = inner.as_ref() {
                    if is_inst(name, entity_for_inst) {
                        // Public mapping getter: `_inst.member(key)`.
                        return format!("_{}.{}({})", name, member, k);
                    }
                }
            }
            format!("{}[{}]", lower_check_expr_multi(base, entity_for_inst), k)
        }
        _ => {
            let ctx = super::solidity::EvmCtx::empty();
            let scope = super::solidity::EmitScope::none();
            let scratch = std::cell::RefCell::new(super::solidity::EmitScratch::new());
            super::solidity::gen_expr(expr, &ctx, &scope, &scratch).unwrap_or_else(|| {
                unreachable!(
                    "I17 skipped (bug): {}",
                    super::predicate_expr::i17_message("multi-entity", 0)
                )
            })
        }
    }
}

/// Best-effort rewrite of bare member identifiers to `var.member()`
/// accessors so invariant `check` expressions can reference entity state
/// by name. Also rewrites bare references to public route names to
/// `var.route` so a `check allPairsLength() == 0` clause binds to the
/// entity instance rather than emitting an unbound free-function call.
///
/// `state_idents` is the set of already-bound state-snapshot / query
/// identifiers exposed by the handler — these are *not* rewritten (they
/// reside on the handler itself, not on the SUT). When empty, behaves the
/// same as the legacy two-pass rewriter.
pub fn substitute_member_accessors(
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
            // Shadowed by a handler-side snapshot/query.
            continue;
        }
        let needle = m.name.clone();
        let replacement = format!("{}.{}()", var_name, m.name);
        result = whole_word_replace(&result, &needle, &replacement);
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
        result = whole_word_replace_before_paren(&result, &needle, &replacement);
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
