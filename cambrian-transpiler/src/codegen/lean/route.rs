// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean-EVM adapter — route assemblers (P1.4 / P4 step F / Local transitions).
//!
//! Orchestrates `<Entity>.Local.*` + `<Entity>.Routes.*` emission: named
//! state-local transitions, thin World wrappers, send/deploy interleaving,
//! fuel/mutual SCC policy, and proof lemmas that mention `World`. State-local
//! builders (`_pre_*`, `Local.<route>` / `Local.<route>_<phase>`, transform
//! application) live in [`super::core::route_local`].
//!
//! Return-type policy:
//! * a route or phase that *cannot* fail (no `where`, no `throw`,
//!   no `throw CustomErr`) returns the raw type (`State`, or
//!   `(State × T)` for view routes);
//! * any failure surface wraps the result in `Cambrian.RouteResult`.
//!
//! See [docs/PLAN_LEAN_TARGET.md](../../docs/PLAN_LEAN_TARGET.md) §P1.4 for
//! the fail-mode predicate, the per-`where`-clause throw-code mapping,
//! and the `view`-route return shape.

use std::collections::{HashMap, HashSet};

use crate::ast::{
    Entity, Expr, FromClauseKind, PhaseBlock, Program, Route, RouteAction, RouteBody,
};
use crate::ir::{self, IrStmt, RouteIr};

use super::core::emitter::push_indent;
use super::core::stmt::{Carrier, LeanStmt};
use super::core::types::{lower_type, LeanTypeCtx};
use super::evm::deploy::lower_deploy;
use super::evm::send::{
    classify_dest, lower_call_route, lower_cross_var_call, lower_extern_call,
    lower_raw_transfer, lower_raw_value_receive,
    lower_self_var_call, resolve_route, resolve_self_route, route_has_unphased_sends,
    route_phased_needs_world_thread, SendTarget,
};
use super::evm::world::entity_field_name;
use super::expr::{expr_needs_world, gen_expr, LeanExprCtx};
use super::LeanProfile;
use crate::codegen::{
    collect_transforms, order_transforms_temporally, precommit_snapshots,
    rewrite_actions_precommit, ResolvedTransform,
};

use super::core::route_local::{
    action_can_return, apply_transforms_term, build_route_body, custom_error_code,
    describe_skipped, emit_local_route_def, emit_macro_helpers, emit_pre_guard,
    emit_where_predicate, expr_ctx_for, find_return_payload, gen_from_checks,
    lower_route_param_type, lower_view_payload_term, pre_apply, render_return_payload,
    route_arg_list, route_args_with_space, write_indented,
};

/// One Routes-namespace definition, ready to render under either
/// [`RouteRenderMode`].
#[derive(Clone, Debug)]
pub struct RouteItem {
    pub entity: String,
    pub name: String,
    /// Everything after `def <name>` (binders, return type, `:=`, body).
    pub after_name: String,
    pub attrs: String,
    pub lemmas: String,
}

/// How to assemble [`RouteItem`]s into Lean source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteRenderMode {
    /// `namespace <E>.Routes` with short names; recursive SCCs wrap in
    /// `mutual` / `partial def`; attributes and lemmas kept.
    Standalone,
    /// FQN `partial def <E>.Routes.<r>` only — for an outer cross-entity
    /// `mutual` (B-15). No `namespace` / `attribute` (Lean rejects both).
    MergedMutual,
}

/// A renderable chunk of the Routes section.
enum RoutesGroup {
    /// Predictable-profile fuel-fixpoint blob (already formatted).
    Predictable(String),
    /// Same-entity recursive SCC → one `mutual` of partial defs.
    Mutual(Vec<RouteItem>),
    /// Single non-recursive route def.
    Single(RouteItem),
}

/// Emit `<Entity>.Local.*` (state transitions) then `<Entity>.Routes.*`
/// (World wrappers) for every route on `entity`. Returns an empty string
/// when the entity has no routes.
#[allow(dead_code)]
pub fn gen_routes_module(program: &Program, entity: &Entity, profile: LeanProfile) -> String {
    let graphs = crate::analysis::ProgramGraphs::build(program);
    gen_routes_module_with_graphs(program, entity, profile, &graphs)
}

pub(crate) fn gen_routes_module_with_graphs(
    program: &Program,
    entity: &Entity,
    profile: LeanProfile,
    graphs: &crate::analysis::ProgramGraphs,
) -> String {
    if entity.routes.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str(&gen_local_module(program, entity, profile));
    out.push_str(&render_routes_groups(
        &collect_routes_groups(program, entity, profile, graphs),
        entity,
        RouteRenderMode::Standalone,
    ));
    out
}

/// Named `<E>.Local.*` transitions + `_pre_*` predicates (state-only).
/// Safe to emit outside any `mutual` block — Local defs do not participate
/// in cross-entity recursion.
pub fn gen_local_module(program: &Program, entity: &Entity, profile: LeanProfile) -> String {
    let mut local = String::new();
    emit_macro_helpers(&mut local, program, entity, profile, true);
    if !entity.routes.is_empty() {
        let graphs = crate::analysis::ProgramGraphs::build(program);
        for route in &entity.routes {
            emit_local_artifacts(&mut local, program, entity, route, profile, &graphs);
        }
    }
    if local.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str(&format!("namespace {}.Local\n\n", entity.name));
    out.push_str(&local);
    out.push_str(&format!("end {}.Local\n\n", entity.name));
    out
}

/// Collect Routes defs as structured groups (no string post-processing).
fn collect_routes_groups(
    program: &Program,
    entity: &Entity,
    profile: LeanProfile,
    graphs: &crate::analysis::ProgramGraphs,
) -> Vec<RoutesGroup> {
    let mut groups = Vec::new();
    for scc in order_route_sccs(graphs, entity) {
        if profile.predictable && scc_is_recursive(graphs, entity, &scc) {
            let mut blob = String::new();
            emit_fuel_fix_scc(&mut blob, program, entity, &scc, profile, graphs);
            if profile.proof_helpers {
                for route in &scc {
                    emit_route_simp_attrs(&mut blob, route);
                    emit_route_pre_lemmas(&mut blob, program, entity, route, profile);
                }
            }
            groups.push(RoutesGroup::Predictable(blob));
            continue;
        }
        if scc_is_recursive(graphs, entity, &scc) {
            let items: Vec<RouteItem> = scc
                .iter()
                .map(|route| {
                    make_route_item(
                        program, entity, route, profile, graphs, /*attrs*/ false,
                    )
                })
                .collect();
            groups.push(RoutesGroup::Mutual(items));
        } else {
            let route = scc[0];
            groups.push(RoutesGroup::Single(make_route_item(
                program, entity, route, profile, graphs, /*attrs*/ true,
            )));
        }
    }
    groups
}

fn make_route_item(
    program: &Program,
    entity: &Entity,
    route: &Route,
    profile: LeanProfile,
    graphs: &crate::analysis::ProgramGraphs,
    with_attrs: bool,
) -> RouteItem {
    let mut chunk = String::new();
    emit_route_entry_body(&mut chunk, program, entity, route, profile, graphs);
    let after_name = split_def_after_name(&chunk, &route.name);
    let mut attrs = String::new();
    let mut lemmas = String::new();
    if profile.proof_helpers {
        if with_attrs {
            emit_route_simp_attrs(&mut attrs, route);
        }
        emit_route_pre_lemmas(&mut lemmas, program, entity, route, profile);
    }
    RouteItem {
        entity: entity.name.clone(),
        name: route.name.clone(),
        after_name,
        attrs,
        lemmas,
    }
}

/// Strip the leading `def <name> ` from an entry-body chunk.
fn split_def_after_name(chunk: &str, name: &str) -> String {
    let prefix = format!("def {} ", name);
    if let Some(rest) = chunk.strip_prefix(&prefix) {
        return rest.to_string();
    }
    if let Some(idx) = chunk.find(&prefix) {
        return chunk[idx + prefix.len()..].to_string();
    }
    panic!(
        "route entry chunk for `{}` missing leading `def {} `:\n{}",
        name, name, chunk
    );
}

fn render_routes_groups(groups: &[RoutesGroup], entity: &Entity, mode: RouteRenderMode) -> String {
    match mode {
        RouteRenderMode::Standalone => {
            let mut out = String::new();
            out.push_str(&format!("namespace {}.Routes\n\n", entity.name));
            for g in groups {
                match g {
                    RoutesGroup::Predictable(blob) => out.push_str(blob),
                    RoutesGroup::Mutual(items) => {
                        out.push_str("mutual\n\n");
                        for item in items {
                            out.push_str("partial def ");
                            out.push_str(&item.name);
                            out.push(' ');
                            out.push_str(&item.after_name);
                            if !item.after_name.ends_with('\n') {
                                out.push('\n');
                            }
                            out.push('\n');
                        }
                        out.push_str("end\n\n");
                        for item in items {
                            out.push_str(&item.lemmas);
                        }
                    }
                    RoutesGroup::Single(item) => {
                        out.push_str("def ");
                        out.push_str(&item.name);
                        out.push(' ');
                        out.push_str(&item.after_name);
                        if !item.after_name.ends_with('\n') {
                            out.push('\n');
                        }
                        out.push('\n');
                        out.push_str(&item.attrs);
                        out.push_str(&item.lemmas);
                    }
                }
            }
            out.push_str(&format!("end {}.Routes\n\n", entity.name));
            out
        }
        RouteRenderMode::MergedMutual => {
            // Flatten every route to FQN `partial def`; attrs/lemmas omitted
            // (Lean rejects attributes inside mutual; lemmas stay on stubs).
            let mut out = String::new();
            for g in groups {
                let items: &[RouteItem] = match g {
                    RoutesGroup::Predictable(_) => continue,
                    RoutesGroup::Mutual(items) => items.as_slice(),
                    RoutesGroup::Single(item) => std::slice::from_ref(item),
                };
                for item in items {
                    out.push_str("partial def ");
                    out.push_str(&item.entity);
                    out.push_str(".Routes.");
                    out.push_str(&item.name);
                    out.push(' ');
                    out.push_str(&item.after_name);
                    if !item.after_name.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push('\n');
                }
            }
            out
        }
    }
}

/// Emit every Routes entry-point as FQN `partial def <E>.Routes.<r>` for an
/// outer cross-entity `mutual` (B-15).
#[allow(dead_code)]
pub fn gen_routes_wrappers_fqn_for_mutual(
    program: &Program,
    entity: &Entity,
    profile: LeanProfile,
) -> String {
    let graphs = crate::analysis::ProgramGraphs::build(program);
    gen_routes_wrappers_fqn_for_mutual_with_graphs(program, entity, profile, &graphs)
}

pub(crate) fn gen_routes_wrappers_fqn_for_mutual_with_graphs(
    program: &Program,
    entity: &Entity,
    profile: LeanProfile,
    graphs: &crate::analysis::ProgramGraphs,
) -> String {
    render_routes_groups(
        &collect_routes_groups(program, entity, profile, graphs),
        entity,
        RouteRenderMode::MergedMutual,
    )
}

// ---------------------------------------------------------------------------
// Predictable profile: total fuel-bounded emission of recursive route SCCs
// ---------------------------------------------------------------------------
//
// The default profile keeps upstream's B-8 shape (`mutual` + `partial def`).
// That shape is unusable for an predictable emission: Lean marks a `partial def`
// constant **opaque**, so the exported NDJSON carries an `Inhabited.default`
// witness instead of the route logic — the thing the deal sells never leaves
// the kernel. Under `lean.emission_profile: predictable` a recursive SCC is
// therefore emitted as a TOTAL, fuel-bounded fixpoint:
//
// ```lean
// def <E>.Routes.fuelFix_<r1>_…_<rk> : Nat → ((T_r1) × … × (T_rk)) :=
//   Nat.rec (motive := fun _ => ((T_r1) × … × (T_rk)))
//     (Prod.mk (fun … => Except.error (Cambrian.ThrowCode.ofNat <OOF>)) …)
//     (fun _ ih => Prod.mk (fun … => <body r1, SCC calls → ih projections>) …)
//
// def <E>.Routes.<ri> (w : …) … : Cambrian.RouteResult … :=
//   (fuelFix_<r1>_…_<rk> 1000000).<proj i> w inst ctx …
// ```
//
// The only recursion is the explicit `Nat.rec` application: no equation
// compiler, no `match`, no `termination_by`, no `partial`, no `mutual` — so
// no `.brecOn` / `.match_1` / `._unary` companions appear in the digest zone
// and every body is a real, kernel-visible term. Fuel exhaustion is a normal
// revert, which is also what the EVM does when it runs out of gas.

/// Fuel budget baked into every escrow recursive-route wrapper. Inlined as a
/// literal at each use site on purpose — it is part of the wrapper's hashed
/// body, so it must not hide behind a separate constant declaration.
const FUEL_BUDGET: u64 = 1_000_000;

/// `ThrowCode` raised when a recursive route SCC exhausts its fuel. `2^32` is
/// provably disjoint from every code the emitter can otherwise produce: both
/// sources ([`where_clause_throw_code`]'s `throw N` literal and
/// [`custom_error_code`]'s FNV-1a hash) are `u32`.
const OUT_OF_FUEL_CODE: u64 = 4_294_967_296;

/// True when `scc` needs a recursive emission: either mutual recursion
/// (more than one route) or a single route that calls itself.
fn scc_is_recursive(
    graphs: &crate::analysis::ProgramGraphs,
    entity: &Entity,
    scc: &[&Route],
) -> bool {
    if scc.len() > 1 {
        return true;
    }
    let route = scc[0];
    graphs
        .callees(&entity.name, &route.name)
        .iter()
        .any(|callee| callee == &route.name)
}

/// Emit the fuel fixpoint for a recursive SCC plus one thin wrapper `def`
/// per route (predictable profile only). Route order inside the fixpoint is the
/// entity's declaration order, which [`order_route_sccs`] already imposes.
fn emit_fuel_fix_scc(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    scc: &[&Route],
    profile: LeanProfile,
    graphs: &crate::analysis::ProgramGraphs,
) {
    let k = scc.len();
    let fix_name = format!(
        "fuelFix_{}",
        scc.iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<&str>>()
            .join("_"),
    );

    // Per-route shape, with the fail-loud gates for SCC members that have
    // no place to report fuel exhaustion.
    let mut tys: Vec<String> = Vec::with_capacity(k);
    let mut binders: Vec<String> = Vec::with_capacity(k);
    let mut bodies: Vec<String> = Vec::with_capacity(k);
    for route in scc {
        let return_ty = view_return_type(program, entity, route, profile);
        let fail_mode = route_fail_mode(program, entity, route);
        if !fail_mode {
            panic!(
                "predictable profile: recursive route SCC [{}] contains route '{}' of \
                 entity '{}', which has no failure surface (it returns \
                 `{}` rather than `Cambrian.RouteResult …`), so fuel exhaustion \
                 has no value to return. Give the cycle a `where … : throw N` \
                 guard or a `throw`, or break the recursion.",
                scc.iter()
                    .map(|r| r.name.as_str())
                    .collect::<Vec<&str>>()
                    .join(", "),
                route.name,
                entity.name,
                match &return_ty {
                    Some(rt) => format!("Cambrian.Generated.World × {}", rt),
                    None => "Cambrian.Generated.World".to_string(),
                },
            );
        }
        tys.push(fuel_route_arrow_type(
            program,
            entity,
            route,
            return_ty.as_deref(),
            fail_mode,
            profile,
        ));
        binders.push(pre_lemma_args(route));
        bodies.push(fuel_route_body(program, entity, route, profile, graphs));
    }

    // SCC-internal qualified references rewrite to `ih` projections; every
    // other `<E>.Routes.*` reference (cross-SCC, helper `_pre_i` / `_phase`)
    // is left exactly as the ordinary emitters produced it.
    let mut ih_of: HashMap<&str, String> = HashMap::new();
    for (i, route) in scc.iter().enumerate() {
        ih_of.insert(route.name.as_str(), format!("ih{}", fuel_projection(i, k)));
    }
    let bodies: Vec<String> = bodies
        .iter()
        .map(|b| substitute_scc_calls(b, &entity.name, &ih_of))
        .collect();

    let prod_ty = tys
        .iter()
        .map(|t| format!("({})", t))
        .collect::<Vec<String>>()
        .join(" × ");

    // `Nat.rec` base: every projection reverts with the out-of-fuel code.
    let base = render_nested_prod(k, 0, 2, &|i, _indent| {
        format!(
            "(fun {} => Except.error (Cambrian.ThrowCode.ofNat {}))",
            binders[i], OUT_OF_FUEL_CODE,
        )
    });
    // `Nat.rec` step: the real route bodies, one fuel unit deeper.
    let step = render_nested_prod(k, 0, 3, &|i, indent| {
        format!(
            "(fun {} =>\n{})",
            binders[i],
            indent_block(&bodies[i], indent + 1),
        )
    });

    out.push_str(&format!("def {} : Nat → ({}) :=\n", fix_name, prod_ty));
    out.push_str(&format!("  Nat.rec (motive := fun _ => ({}))\n", prod_ty));
    out.push_str(&format!("    {}\n", base));
    out.push_str("    (fun _ ih =>\n");
    out.push_str(&format!("      {})\n\n", step));

    // Thin wrappers: the public route names stay ordinary `def`s with the
    // usual signature, so callers, `Pre` lemmas and `cambrian_route_simp`
    // see no difference from a non-recursive route.
    for (i, route) in scc.iter().enumerate() {
        emit_route_signature(
            out,
            program,
            entity,
            route,
            view_return_type(program, entity, route, profile).as_deref(),
            route_fail_mode(program, entity, route),
            None,
            profile,
        );
        out.push_str(" :=\n");
        out.push_str(&format!(
            "  ({} {}){} {}\n\n",
            fix_name,
            FUEL_BUDGET,
            fuel_projection(i, k),
            binders[i],
        ));
    }
}

/// The route's type as a plain arrow chain — the component type the fuel
/// fixpoint's product carries. Mirrors [`emit_route_signature`]'s binder and
/// return-type spelling exactly.
fn fuel_route_arrow_type(
    program: &Program,
    entity: &Entity,
    route: &Route,
    return_ty: Option<&str>,
    fail_mode: bool,
    profile: LeanProfile,
) -> String {
    let mut parts: Vec<String> = vec![
        "Cambrian.Generated.World".to_string(),
        format!("{}.Identity", entity.name),
        "Cambrian.MsgCtx".to_string(),
    ];
    for p in &route.params {
        parts.push(lower_route_param_type(program, entity, &p.ty, profile));
    }
    let core = match return_ty {
        Some(rt) => format!("Cambrian.Generated.World × {}", rt),
        None => "Cambrian.Generated.World".to_string(),
    };
    parts.push(if fail_mode {
        format!("Cambrian.RouteResult ({})", core)
    } else {
        core
    });
    parts.join(" → ")
}

/// The body term of a route's entry `def`, dedented to column zero. Reuses
/// [`emit_route_entry_body`] verbatim so a recursive route's body is emitted
/// by exactly the same machinery as a non-recursive one; all three entry
/// paths produce a single `def … :=` header line followed by a body indented
/// by one level.
fn fuel_route_body(
    program: &Program,
    entity: &Entity,
    route: &Route,
    profile: LeanProfile,
    graphs: &crate::analysis::ProgramGraphs,
) -> String {
    let mut chunk = String::new();
    emit_route_entry_body(&mut chunk, program, entity, route, profile, graphs);
    let body = match chunk.split_once('\n') {
        Some((header, rest)) if header.trim_end().ends_with(":=") => rest,
        _ => panic!(
            "predictable profile: route '{}' of entity '{}' did not emit the expected \
             `def … :=` header — cannot lift it into a fuel fixpoint. Chunk head:\n{}",
            route.name,
            entity.name,
            chunk.lines().take(3).collect::<Vec<&str>>().join("\n"),
        ),
    };
    body.trim_end_matches('\n')
        .lines()
        .map(|l| l.strip_prefix("  ").unwrap_or(l))
        .collect::<Vec<&str>>()
        .join("\n")
}

/// `Prod` projection path for element `i` of a right-nested `k`-tuple:
/// `""` (k = 1), `.1` / `.2` (k = 2), `.1` / `.2.1` / `.2.2` (k = 3), …
fn fuel_projection(i: usize, k: usize) -> String {
    if k == 1 {
        return String::new();
    }
    let mut s = String::new();
    for _ in 0..i {
        s.push_str(".2");
    }
    if i + 1 < k {
        s.push_str(".1");
    }
    s
}

/// Render a right-nested `Prod.mk` chain over `n` elements, starting at
/// `start`, with the opening token at indentation `indent`. `render_elem`
/// yields a parenthesized term whose first line carries no indentation and
/// whose continuation lines are indented relative to its `indent` argument.
/// A one-element "tuple" is the element itself (no `Prod.mk`).
fn render_nested_prod(
    n: usize,
    start: usize,
    indent: usize,
    render_elem: &dyn Fn(usize, usize) -> String,
) -> String {
    if start + 1 == n {
        return render_elem(start, indent);
    }
    let pad = "  ".repeat(indent + 1);
    format!(
        "(Prod.mk\n{pad}{head}\n{pad}{tail})",
        pad = pad,
        head = render_elem(start, indent + 1),
        tail = render_nested_prod(n, start + 1, indent + 1, render_elem),
    )
}

/// Indent every non-empty line of `body` by `indent` levels.
fn indent_block(body: &str, indent: usize) -> String {
    let pad = "  ".repeat(indent);
    body.lines()
        .map(|l| {
            if l.trim().is_empty() {
                l.to_string()
            } else {
                format!("{}{}", pad, l)
            }
        })
        .collect::<Vec<String>>()
        .join("\n")
}

/// Byte index just past the Lean string literal that opens at `start`
/// (`text[start]` must be `"`), or `text.len()` if the literal is never closed.
///
/// PM-037. The escrow fuel path rewrites the ALREADY-EMITTED Lean text, so it
/// must know where the code stops and data begins: a Cambrian `String` literal
/// whose content happens to read `<Entity>.Routes.<r>` is a string, not a call,
/// and rewriting it to `ih.N` would both corrupt the value and diverge from the
/// predictor's AST-level mirror (`subst_scc_calls`, which leaves `Expr::StrLit`
/// alone). Only the plain form is recognised, because that is the only form the
/// emitter can print: [`lean_string_literal`](crate::codegen::lean::core::types)
/// wraps the content in `"…"` and escapes `\` `"` `\n` `\t` `\r` with a
/// backslash — no raw strings (`r#"…"#`) and no interpolation (`s!"…"`) ever
/// reach this text. Scanning is UTF-8-safe: the two bytes that matter (`\` and
/// `"`) are ASCII, so a multi-byte scalar can never be mistaken for either.
fn lean_string_literal_end(text: &str, start: usize) -> usize {
    let bytes = text.as_bytes();
    debug_assert_eq!(bytes[start], b'"');
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return i + 1,
            // A backslash consumes itself plus the next SCALAR (not byte), so a
            // `\"` never terminates the literal.
            b'\\' => {
                i += 1;
                match text[i..].chars().next() {
                    Some(c) => i += c.len_utf8(),
                    None => break,
                }
            }
            _ => {
                let c = text[i..].chars().next().expect("in-bounds char boundary");
                i += c.len_utf8();
            }
        }
    }
    text.len()
}

/// Rewrite SCC-internal references `<Entity>.Routes.<r>` into the matching
/// `ih` projection. Catches every call form uniformly (`call`, same-entity
/// `send` / `VarCall`, view-route calls in expression position) because they
/// all spell the callee with the same qualified prefix. Matching is
/// identifier-exact: a longer name that merely starts with `<r>` (e.g. the
/// `<r>_pre_0` guard or a `<r>_<phase>` helper) never matches, and neither
/// does a reference to a route outside the SCC.
///
/// Lean string literals are copied through VERBATIM and are invisible to both
/// the rewrite and the E6 residual guard — see [`lean_string_literal_end`]
/// (PM-037, owner decision D4(a)).
fn substitute_scc_calls(body: &str, entity: &str, ih_of: &HashMap<&str, String>) -> String {
    fn is_ident_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_' || c == '\'' || c == '!' || c == '?'
    }

    /// Find a still-unsubstituted SCC-internal call, using the SAME boundary
    /// rules as the rewrite below (so the guard can never disagree with it) —
    /// string-literal spans included, so a literal the rewrite deliberately
    /// left alone cannot be reported as a residual call.
    fn residual_call(text: &str, prefix: &str, ih_of: &HashMap<&str, String>) -> Option<String> {
        let mut i = 0usize;
        while i < text.len() {
            if text.as_bytes()[i] == b'"' {
                i = lean_string_literal_end(text, i);
                continue;
            }
            if text[i..].starts_with(prefix) {
                let prev_breaks = text[..i]
                    .chars()
                    .next_back()
                    .map(|c| !is_ident_char(c) && c != '.')
                    .unwrap_or(true);
                if prev_breaks {
                    let rest = &text[i + prefix.len()..];
                    let end = rest.find(|c| !is_ident_char(c)).unwrap_or(rest.len());
                    if rest[end..].chars().next() != Some('.') && ih_of.contains_key(&rest[..end]) {
                        return Some(format!("{}{}", prefix, &rest[..end]));
                    }
                }
            }
            let c = text[i..].chars().next().expect("in-bounds char boundary");
            i += c.len_utf8();
        }
        None
    }

    let prefix = format!("{}.Routes.", entity);
    let mut out = String::with_capacity(body.len());
    let mut i = 0usize;
    while i < body.len() {
        // Data, not code: copy the whole literal and resume scanning after it.
        if body.as_bytes()[i] == b'"' {
            let end = lean_string_literal_end(body, i);
            out.push_str(&body[i..end]);
            i = end;
            continue;
        }
        if body[i..].starts_with(&prefix) {
            let prev_breaks = body[..i]
                .chars()
                .next_back()
                .map(|c| !is_ident_char(c) && c != '.')
                .unwrap_or(true);
            if prev_breaks {
                let rest = &body[i + prefix.len()..];
                let end = rest.find(|c| !is_ident_char(c)).unwrap_or(rest.len());
                let terminator_breaks = rest[end..].chars().next() != Some('.');
                if terminator_breaks {
                    if let Some(ih) = ih_of.get(&rest[..end]) {
                        out.push_str(ih);
                        i += prefix.len() + end;
                        continue;
                    }
                }
            }
        }
        let c = body[i..].chars().next().expect("in-bounds char boundary");
        out.push(c);
        i += c.len_utf8();
    }

    // E6 guard (fail-loud, mirrors the `def … :=` guard in `fuel_route_body`).
    //
    // The escrow fuel path parses the shared emitter's output AS TEXT and rests
    // on two assumptions; this is the second one — that an SCC-internal call
    // literally reads `<Entity>.Routes.<route>`. If the emitter ever changes
    // that spelling (upstream's named local transitions would), the loop above
    // simply substitutes nothing and hands back the body unchanged. The
    // fixpoint would still assemble, `Nat.rec` and all, but the recursive call
    // would remain a REAL call — so the totality the whole predictable profile
    // exists to guarantee would be silently gone, and the defect would surface
    // either as an opaque elaboration failure or, worse, as something that
    // builds and ships into the sold NDJSON. Refuse loudly instead.
    if let Some(residual) = residual_call(&out, &prefix, ih_of) {
        panic!(
            "predictable profile: fuel fixpoint for entity '{}' left an unsubstituted \
             SCC-internal call `{}` in the body — the recursive call was NOT \
             rewritten to an `ih` projection, so the fixpoint is not total. \
             The emitter's call spelling changed; teach `substitute_scc_calls` \
             the new form. SCC routes: {:?}",
            entity,
            residual,
            {
                let mut names: Vec<&str> = ih_of.keys().copied().collect();
                names.sort_unstable();
                names
            },
        );
    }

    out
}

/// Strongly connected components of the same-entity `call` / self-send
/// graph, ordered so that if A calls B and they are in different SCCs,
/// B's component is emitted before A's (callees first).
fn order_route_sccs<'a>(
    graphs: &crate::analysis::ProgramGraphs,
    entity: &'a Entity,
) -> Vec<Vec<&'a Route>> {
    let Some(sccs) = graphs.route_sccs.get(&entity.name) else {
        return entity.routes.iter().map(|r| vec![r]).collect();
    };
    sccs.iter()
        .map(|comp| {
            comp.iter()
                .filter_map(|n| entity.routes.iter().find(|r| r.name == *n))
                .collect()
        })
        .collect()
}

/// Emit state-local artifacts into `<E>.Local`: `_pre_*` predicates,
/// named `Local.<route>` defs (state-only unphased), and `Local.<route>_<phase>`
/// helpers (non-world-threaded phased). World-threaded routes keep their
/// bodies in `Routes` but still get `_pre_*` here.
fn emit_local_artifacts(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    route: &Route,
    profile: LeanProfile,
    graphs: &crate::analysis::ProgramGraphs,
) {
    let route_ir = ir::lower_route(program, entity, route, graphs);
    for (i, w) in route.where_clauses.iter().enumerate() {
        emit_where_predicate(out, program, entity, route, i, w, None, profile);
    }
    if let RouteBody::Phased(phases) = &route.body {
        for ph in phases {
            for (i, w) in ph.where_clauses.iter().enumerate() {
                emit_where_predicate(out, program, entity, route, i, w, Some(&ph.name), profile);
            }
        }
        if !route_ir.needs_world {
            let route_ret = view_return_type(program, entity, route, profile);
            let returning = returning_phase_name(phases);
            for ph in phases {
                let phase_ret = if returning.as_deref() == Some(ph.name.as_str()) {
                    route_ret.as_deref()
                } else {
                    None
                };
                emit_phase_fn(out, program, entity, route, ph, profile, phase_ret);
            }
        }
    }
    // Named Local transition for state-only unphased / Mixed bodies.
    if matches!(&route.body, RouteBody::Unphased(_) | RouteBody::Mixed(_, _))
        && !route_has_unphased_sends(route)
    {
        let return_ty = view_return_type(program, entity, route, profile);
        let fail_mode = route_fail_mode(program, entity, route);
        let actions = match &route.body {
            RouteBody::Unphased(a) | RouteBody::Mixed(_, a) => a.as_slice(),
            RouteBody::Phased(_) => unreachable!(),
        };
        let body = build_route_body(
            program,
            entity,
            route,
            actions,
            None,
            return_ty.as_deref(),
            Carrier::State { fail: fail_mode },
            profile,
        );
        let body = super::evm::rewrite_route_body(&body, profile.predictable);
        emit_local_route_def(
            out,
            program,
            entity,
            route,
            return_ty.as_deref(),
            fail_mode,
            profile,
            &body,
        );
    }
    if profile.proof_helpers {
        emit_local_simp_attrs(out, route);
    }
}

/// The route entry-point `def` itself (may sit inside a `mutual` block).
fn emit_route_entry_body(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    route: &Route,
    profile: LeanProfile,
    graphs: &crate::analysis::ProgramGraphs,
) {
    let route_ir = ir::lower_route(program, entity, route, graphs);
    let return_ty = view_return_type(program, entity, route, profile);
    // Adapter fail-mode (kernel + PN-106 extern overlay + overflow-panic),
    // not IR's kernel-only fail_closure — IR stays domain-neutral.
    let fail_mode = route_fail_mode(program, entity, route);
    match &route.body {
        RouteBody::Phased(phases) => {
            if route_ir.needs_world {
                emit_phased_route_entry_world_threaded(
                    out,
                    program,
                    entity,
                    route,
                    phases,
                    return_ty.as_deref(),
                    fail_mode,
                    profile,
                    graphs,
                );
            } else {
                emit_phased_route_entry(
                    out,
                    program,
                    entity,
                    route,
                    phases,
                    return_ty.as_deref(),
                    fail_mode,
                    profile,
                );
            }
        }
        RouteBody::Unphased(actions) | RouteBody::Mixed(_, actions) => {
            emit_unphased_route(
                out,
                program,
                entity,
                route,
                actions,
                return_ty.as_deref(),
                fail_mode,
                profile,
                graphs,
            );
        }
    }
}

/// Design B — emit a per-route success characterization on top of the
/// design-A `isOk` reflection: a decidable `<route>.Pre` proposition and
/// a `<route>_isOk_iff` theorem. Only fail-mode routes get them (a route
/// that cannot fail always succeeds, so `Pre = True` adds nothing).
///
/// Two shapes, picked by [`route_pre_is_flat`]:
///
/// * **Flat** (unphased, no sends, body is only `let`/`return`, **and**
///   no checked member/`as uN` fail surface): `Pre` is the guard-level
///   conjunction of `_pre_<i>` / `from` in **evaluation order**
///   (world-dependent `where` on the `Routes` wrapper, then `from`, then
///   state-only `where` inside `Local`), each in `= true` Prop form, with
///   the per-instance state bound via `let s := w.storage.<field> inst`.
///   `isOk_iff` is discharged by `simp [cambrian_route_simp, …]` (design-A
///   reduces `isOk` to the same Bool conjunction; the bridge lemmas split
///   it into the Prop conjunction).
/// * **Abstract** (phased / world-threaded / send-bearing / loops /
///   checked member transform or narrowing `as`): `Pre := (<route> …).isOk
///   = true`, a decidable named handle that `simp [cambrian_route_simp]`
///   still expands into guards on demand; `isOk_iff` is `Iff.rfl`. Flat
///   `where`-only Pre is not equivalent to `isOk` when a member can still
///   revert (B-11 `msg::value as u128`).
///
/// `isOk_iff` is a plain named theorem (not a global `@[simp]`) so it
/// never auto-rewrites `isOk` everywhere; callers opt in.
fn emit_route_pre_lemmas(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    route: &Route,
    profile: LeanProfile,
) {
    if !route_fail_mode(program, entity, route) {
        return;
    }

    let sig = pre_lemma_binders(program, entity, route, profile);
    let args = pre_lemma_args(route);
    let name = &route.name;
    let flat = route_pre_is_flat(route);
    // `where`/`from` alone do not characterize success when a member
    // transform can still fail (`as uN` / checked `+`). `simp` is then
    // left with `pre → Except.isOk member` (B-11 Payee.deposit).
    let members_can_fail = route_has_div0_or_narrow(program, entity, route)
        || (profile.overflow_panic
            && !profile.nat_numerics
            && route_has_checked_arith(entity, route));

    // `<route>.Pre : Prop`
    out.push_str(&format!("def {}.Pre{} : Prop :=\n", name, sig));
    let use_flat = flat && !members_can_fail && {
        let conjuncts = flat_pre_conjuncts(program, entity, route, profile);
        !conjuncts.is_empty()
    };
    if use_flat {
        let field = entity_field_name(&entity.name);
        out.push_str(&format!("  let s := w.storage.{} inst\n", field));
        let conjuncts = flat_pre_conjuncts(program, entity, route, profile);
        out.push_str(&format!("  {}\n", conjuncts.join(" ∧ ")));
    } else {
        // Abstract handle — also used when fail-mode has no where/from
        // (e.g. `lean.numerics: overflow-panic` with checked member arith).
        out.push_str(&format!("  ({} {}).isOk = true\n", name, args));
    }
    out.push('\n');

    // `instance : Decidable (<route>.Pre …)`
    out.push_str(&format!(
        "instance{} : Decidable ({}.Pre {}) := by\n",
        sig, name, args,
    ));
    out.push_str(&format!("  unfold {}.Pre; infer_instance\n\n", name));

    // `theorem <route>_isOk_iff : (<route> …).isOk = true ↔ <route>.Pre …`
    out.push_str(&format!("theorem {}_isOk_iff{} :\n", name, sig));
    out.push_str(&format!(
        "    ({} {}).isOk = true ↔ {}.Pre {} := ",
        name, args, name, args,
    ));
    if use_flat {
        out.push_str("by\n");
        out.push_str(&format!("  unfold {} {}.Pre\n", name, name));
        if profile.predictable {
            // Predictable profile: the thin `Routes.<r>` wrapper is an explicit
            // `Except.bind (Local.<r> …) (fun … => Except.ok …)` (no
            // `do`/`←`), so the legacy one-shot `simp` cannot see through the
            // bind of an `ite` cascade. Split the bind's match, then the
            // guard `if` inside its equation, and close each cell by
            // `simp_all`. Proof territory only — the digest zone is untouched.
            //
            // Two ladders, picked by [`route_pre_is_mixed_world`]:
            //
            // * **Single-round** (state-only guards, or world-only guards
            //   over a guardless `Local`): one goal split reaches the bind's
            //   match; every remaining `ite` lives inside the captured
            //   equation `heq`, so `repeat' split at heq` + `simp_all`
            //   closes.
            // * **Fixpoint** (mixed: world-dependent wrapper guard(s) AND
            //   guard(s) inside `Local`): the first split consumes the
            //   wrapper's world `if`, and its else-branch still holds
            //   `match (Local's ite cascade) with …` in the GOAL — the
            //   single-round ladder never splits the goal again and
            //   `simp_all` cannot case on the inner guard (`unsolved goals
            //   case isFalse`, uniswap ERC20 `permit`). The fixpoint form
            //   re-runs (goal split → capture the newest equation as `heq`
            //   → split inside it) to saturation, then closes every leaf
            //   with `simp_all`.
            //
            // Emission is conditional (not a global swap) so the escrow
            // emission of every project without the mixed form stays
            // byte-for-byte.
            out.push_str("  simp only [cambrian_route_simp, Except.bind]\n");
            if route_pre_is_mixed_world(entity, route) {
                out.push_str(
                    "  repeat' (split <;> rename_i heq <;> (repeat' split at heq))\n",
                );
                out.push_str("  all_goals simp_all\n\n");
            } else {
                out.push_str(
                    "  split <;> rename_i heq <;> (repeat' split at heq) <;> simp_all\n\n",
                );
            }
        } else {
            out.push_str(
                "  simp [cambrian_route_simp, Bool.and_eq_true, Bool.or_eq_true, decide_eq_true_eq]\n\n",
            );
        }
    } else {
        out.push_str("Iff.rfl\n\n");
    }
}

/// Shared binder list for the design-B `Pre` def, its `Decidable`
/// instance, and the `isOk_iff` theorem — identical to the route
/// signature's leading binders (`emit_route_signature`).
fn pre_lemma_binders(
    program: &Program,
    entity: &Entity,
    route: &Route,
    profile: LeanProfile,
) -> String {
    let mut sig = format!(
        " (w : Cambrian.Generated.World) (inst : {}.Identity) (ctx : Cambrian.MsgCtx)",
        entity.name,
    );
    for p in &route.params {
        sig.push_str(&format!(
            " ({} : {})",
            super::core::types::lean_safe_ident(&p.name),
            lower_route_param_type(program, entity, &p.ty, profile),
        ));
    }
    sig
}

/// Argument spelling for a route call inside the `Pre` artifacts:
/// `w inst ctx <param…>`.
fn pre_lemma_args(route: &Route) -> String {
    let mut s = String::from("w inst ctx");
    for p in &route.params {
        s.push(' ');
        s.push_str(&super::core::types::lean_safe_ident(&p.name));
    }
    s
}

/// True when `route`'s success condition is a flat guard conjunction:
/// unphased body, no world-threaded sends, and every action is a `let`
/// or `return` (so the `isOk` reflection collapses to `from`-check `&&`
/// `_pre_<i>` with no intermediate state threading). Everything else
/// (phased, send-bearing, loops, conditionals, explicit `throw`) uses
/// the abstract `Pre` fallback.
fn route_pre_is_flat(route: &Route) -> bool {
    if route_has_unphased_sends(route) {
        return false;
    }
    match &route.body {
        RouteBody::Unphased(actions) => actions
            .iter()
            .all(|a| matches!(a, RouteAction::Let { .. } | RouteAction::Return { .. })),
        RouteBody::Phased(_) | RouteBody::Mixed(_, _) => false,
    }
}

/// True when a flat fail-mode route mixes **wrapper** and **Local**
/// guards: at least one world-dependent `where` clause (post-B-31 that
/// includes `sys::chainid` / `sys::blockNumber` / `sys::balance`
/// conditions), emitted as `if !(_pre_<i> w …)` on the `Routes` wrapper,
/// AND at least one guard left inside `Local.<r>` (a state-only `where`
/// or a `from` check). Exactly this combination defeats the escrow
/// single-round `isOk_iff` ladder (see [`emit_route_pre_lemmas`]);
/// world-only guards over a guardless `Local` reduce on the constructor
/// and stay on the single-round ladder (pinned by the
/// `sys_chainid_blocknum` corpus).
fn route_pre_is_mixed_world(entity: &Entity, route: &Route) -> bool {
    let world_guards = route
        .where_clauses
        .iter()
        .filter(|w| expr_needs_world(entity, &w.condition))
        .count();
    let local_guards = route
        .where_clauses
        .iter()
        .filter(|w| !expr_needs_world(entity, &w.condition))
        .count()
        + usize::from(!route.from_clauses.is_empty());
    world_guards >= 1 && local_guards >= 1
}

/// Build the guard-level conjuncts of a flat route's `Pre`, in Prop
/// `= true` form, assuming `s : <Entity>.State` is in scope (bound by the
/// `Pre` body's `let s := w.storage.<field> inst`).
///
/// Conjunction order must match the Local-wrapper evaluation order in
/// [`wrap_calling_local`]: world-dependent `_pre_*` run on the
/// `Routes` wrapper **before** the `Local` call, which then checks `from`
/// and the remaining state-only `_pre_*`. Source order of mixed
/// world/local `where` clauses (e.g. ERC20 `permit`: timestamp then
/// `sys::chainid`) would leave `isOk_iff` with `P ∧ Q ↔ Q ∧ P`, which
/// `simp` does not close (`And.comm` is not a default simp lemma).
fn flat_pre_conjuncts(
    program: &Program,
    entity: &Entity,
    route: &Route,
    profile: LeanProfile,
) -> Vec<String> {
    let route_args = route_arg_list(route);
    let mut cs = Vec::new();
    for (i, w) in route.where_clauses.iter().enumerate() {
        if expr_needs_world(entity, &w.condition) {
            cs.push(format!(
                "{} = true",
                pre_apply(
                    Some(&entity.name),
                    &route.name,
                    None,
                    i,
                    true,
                    &route_args,
                    "",
                ),
            ));
        }
    }
    if let Some(fc) = from_check_prop_conjunct(program, entity, route, profile) {
        cs.push(fc);
    }
    for (i, w) in route.where_clauses.iter().enumerate() {
        if !expr_needs_world(entity, &w.condition) {
            cs.push(format!(
                "{} = true",
                pre_apply(
                    Some(&entity.name),
                    &route.name,
                    None,
                    i,
                    false,
                    &route_args,
                    "",
                ),
            ));
        }
    }
    cs
}

/// Prop-form mirror of [`gen_from_checks`]'s condition: `ctx.sender =
/// <addr>` per `from` clause, joined by `∨`. Returns `None` when the
/// route has no `from` clauses. Mirrors the member / entity lowering in
/// `gen_from_checks` but uses `=` / `∨` (Prop) rather than `==` / `||`
/// (Bool) so the conjunct reads as a clean proposition; the `isOk_iff`
/// proof bridges the two via `decide_eq_true_eq`.
fn from_check_prop_conjunct(
    program: &Program,
    entity: &Entity,
    route: &Route,
    profile: LeanProfile,
) -> Option<String> {
    if route.from_clauses.is_empty() {
        return None;
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
        checks.push(super::core::route_local::from_clause_sender_check(
            program,
            entity,
            clause,
            &ctx,
            profile,
            "=",
        ));
    }
    Some(if checks.len() == 1 {
        checks.pop().unwrap()
    } else {
        format!("({})", checks.join(" ∨ "))
    })
}

/// Register Local transition defs and `_pre_*` guards into the Prelude's
/// reflection simp sets. Emitted inside `namespace <Entity>.Local`.
fn emit_local_simp_attrs(out: &mut String, route: &Route) {
    let mut route_names: Vec<String> = Vec::new();
    if matches!(&route.body, RouteBody::Unphased(_) | RouteBody::Mixed(_, _))
        && !route_has_unphased_sends(route)
    {
        route_names.push(route.name.clone());
    }
    if let RouteBody::Phased(phases) = &route.body {
        if !route_phased_needs_world_thread(route) {
            for ph in phases {
                route_names.push(format!("{}_{}", route.name, ph.name));
            }
        }
    }
    if !route_names.is_empty() {
        out.push_str(&format!(
            "attribute [cambrian_route_simp] {}\n",
            route_names.join(" "),
        ));
    }

    let mut pre_names: Vec<String> = Vec::new();
    for (i, _) in route.where_clauses.iter().enumerate() {
        pre_names.push(format!("{}_pre_{}", route.name, i));
    }
    if let RouteBody::Phased(phases) = &route.body {
        for ph in phases {
            for (i, _) in ph.where_clauses.iter().enumerate() {
                pre_names.push(format!("{}_{}_pre_{}", route.name, ph.name, i));
            }
        }
    }
    if !pre_names.is_empty() {
        out.push_str(&format!(
            "attribute [cambrian_pre_simp] {}\n",
            pre_names.join(" "),
        ));
    }
    if !route_names.is_empty() || !pre_names.is_empty() {
        out.push('\n');
    }
}

/// Register the World-wrapper route assembler into `cambrian_route_simp`.
/// Local defs / `_pre_*` are attributed in [`emit_local_simp_attrs`].
fn emit_route_simp_attrs(out: &mut String, route: &Route) {
    out.push_str(&format!(
        "attribute [cambrian_route_simp] {}\n\n",
        route.name,
    ));
}

fn emit_unphased_route(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    route: &Route,
    actions: &[RouteAction],
    return_ty: Option<&str>,
    fail_mode: bool,
    profile: LeanProfile,
    graphs: &crate::analysis::ProgramGraphs,
) {
    // P3.8: when the unphased body contains any `Send` / `VarCall`
    // action, switch to a world-threaded body that produces the
    // final `World` (or `RouteResult World`) directly.
    // State-only routes call the named `<E>.Local.<route>` transition.
    let assembled = if route_has_unphased_sends(route) {
        build_route_body_world_threaded(
            program, entity, route, actions, return_ty, fail_mode, profile, graphs,
        )
    } else {
        wrap_calling_local(entity, route, return_ty, fail_mode)
    };
    emit_route_signature(
        out, program, entity, route, return_ty, fail_mode, None, profile,
    );
    out.push_str(" :=\n");
    let assembled = super::evm::rewrite_route_body(&assembled, profile.predictable);
    write_indented(out, &assembled, 1);
    out.push_str("\n\n");
}

fn emit_phase_fn(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    route: &Route,
    ph: &PhaseBlock,
    profile: LeanProfile,
    phase_return_ty: Option<&str>,
) {
    let phase_fail = phase_fail_mode(program, entity, route, ph);
    let body = super::core::route_local::build_route_body(
        program,
        entity,
        route,
        &ph.actions,
        Some(ph),
        phase_return_ty,
        Carrier::State { fail: phase_fail },
        profile,
    );
    let body = super::evm::rewrite_route_body(&body, profile.predictable);
    super::core::route_local::emit_phase_def(
        out,
        program,
        entity,
        route,
        ph,
        profile,
        phase_fail,
        phase_return_ty,
        &body,
    );
}

/// Name of the phase that produces a view route's `return(...)` payload
/// (last phase that contains a `return`), if any. Used so that phase's
/// Local helper returns `State × T` (UPSTREAM B-29).
fn returning_phase_name(phases: &[PhaseBlock]) -> Option<String> {
    phases
        .iter()
        .rev()
        .find(|ph| ph.actions.iter().any(action_can_return))
        .map(|ph| ph.name.clone())
}

fn emit_phased_route_entry(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    route: &Route,
    phases: &[PhaseBlock],
    return_ty: Option<&str>,
    fail_mode: bool,
    profile: LeanProfile,
) {
    emit_route_signature(
        out, program, entity, route, return_ty, fail_mode, None, profile,
    );
    out.push_str(" :=\n");

    let field = entity_field_name(&entity.name);
    let read_state = format!("w.storage.{} inst", field);
    let setter_fn = format!("Cambrian.Generated.World.with{}", entity.name);

    // Build the phase chain. Each phase fn is either raw (returns State)
    // or wrapped (returns RouteResult State). When mixed, we lift raw
    // phases into the Except monad.
    let mut chain = String::new();
    let route_args: String = route
        .params
        .iter()
        .map(|p| p.name.as_str())
        .collect::<Vec<&str>>()
        .join(" ");

    if !fail_mode {
        // All phases are raw: simple sequential composition.
        chain.push_str("Id.run do\n");
        chain.push_str(&format!("    let mut s := {}\n", read_state));
        let returning = returning_phase_name(phases);
        let mut payload_var: Option<String> = None;
        for ph in phases {
            let is_ret = returning.as_deref() == Some(ph.name.as_str()) && return_ty.is_some();
            if is_ret {
                // Do not shadow `mut s` (Lean rejects `let (s, …) :=` over
                // a mutable binder). Bind through temps then assign.
                chain.push_str(&format!(
                    "    let (__s_ret, __payload) := {}.Local.{}_{} s ctx inst{}\n",
                    entity.name,
                    route.name,
                    ph.name,
                    if route_args.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", route_args)
                    },
                ));
                chain.push_str("    s := __s_ret\n");
                payload_var = Some("__payload".to_string());
            } else {
                chain.push_str(&format!(
                    "    s := {}.Local.{}_{} s ctx inst{}\n",
                    entity.name,
                    route.name,
                    ph.name,
                    if route_args.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", route_args)
                    },
                ));
            }
        }
        // route-level where-checks would have wrapped fail_mode; they
        // can't appear in the all-raw branch.
        if return_ty.is_some() {
            let payload = payload_var
                .unwrap_or_else(|| phased_return_payload(program, entity, route, phases, profile));
            chain.push_str(&format!("    pure ({} w inst s, {})\n", setter_fn, payload,));
        } else {
            chain.push_str(&format!("    pure ({} w inst s)\n", setter_fn,));
        }
    } else {
        // Wrapped: assemble via Except.bind. Route-level guards run
        // first — `from`-checks then `where`-checks (consistent with
        // every other route-assembly path) — then the phases, lifting
        // raw ones into `pure`.
        chain.push_str("do\n");
        push_indent(&mut chain, 1);
        chain.push_str(&format!("let s := {}\n", read_state));
        chain.push_str(&gen_from_checks(program, entity, route, 1, profile));
        for (i, w) in route.where_clauses.iter().enumerate() {
            emit_pre_guard(
                &mut chain,
                1,
                Some(&entity.name),
                entity,
                route,
                None,
                i,
                w,
                &route_args,
                "",
                false,
            );
        }
        push_indent(&mut chain, 1);
        chain.push_str("let mut s := s\n");
        let returning = returning_phase_name(phases);
        let mut payload_var: Option<String> = None;
        for ph in phases {
            let captured = super::member::phase_captured_vars_before(
                program,
                entity,
                route,
                Some(&ph.name),
                profile,
            );
            let captured_suffix = if captured.is_empty() {
                String::new()
            } else {
                let names: Vec<String> = captured
                    .iter()
                    .map(|(n, _)| super::core::types::lean_safe_ident(n))
                    .collect();
                format!(" {}", names.join(" "))
            };
            for (i, w) in ph.where_clauses.iter().enumerate() {
                emit_pre_guard(
                    &mut chain,
                    1,
                    Some(&entity.name),
                    entity,
                    route,
                    Some(&ph.name),
                    i,
                    w,
                    &route_args,
                    &captured_suffix,
                    false,
                );
            }
            push_indent(&mut chain, 1);
            let is_ret = returning.as_deref() == Some(ph.name.as_str()) && return_ty.is_some();
            if phase_fail_mode(program, entity, route, ph) {
                if is_ret {
                    chain.push_str(&format!(
                        "let (__s_ret, __payload) ← {}.Local.{}_{} s ctx inst{}\n",
                        entity.name,
                        route.name,
                        ph.name,
                        route_args_with_space(&route_args),
                    ));
                    push_indent(&mut chain, 1);
                    chain.push_str("s := __s_ret\n");
                    payload_var = Some("__payload".to_string());
                } else {
                    chain.push_str(&format!(
                        "s ← {}.Local.{}_{} s ctx inst{}\n",
                        entity.name,
                        route.name,
                        ph.name,
                        route_args_with_space(&route_args),
                    ));
                }
            } else if is_ret {
                // Avoid shadowing `mut s` — see non-fail branch above.
                chain.push_str(&format!(
                    "let (__s_ret, __payload) := {}.Local.{}_{} s ctx inst{}\n",
                    entity.name,
                    route.name,
                    ph.name,
                    route_args_with_space(&route_args),
                ));
                push_indent(&mut chain, 1);
                chain.push_str("s := __s_ret\n");
                payload_var = Some("__payload".to_string());
            } else {
                chain.push_str(&format!(
                    "s := {}.Local.{}_{} s ctx inst{}\n",
                    entity.name,
                    route.name,
                    ph.name,
                    route_args_with_space(&route_args),
                ));
            }
        }
        push_indent(&mut chain, 1);
        if return_ty.is_some() {
            let payload = payload_var
                .unwrap_or_else(|| phased_return_payload(program, entity, route, phases, profile));
            chain.push_str(&format!("pure ({} w inst s, {})\n", setter_fn, payload,));
        } else {
            chain.push_str(&format!("pure ({} w inst s)\n", setter_fn,));
        }
    }
    let chain = super::evm::rewrite_route_body(chain.trim_end(), profile.predictable);
    write_indented(out, &chain, 1);
    out.push_str("\n\n");
    let _ = program;
}

/// Phased route entry that threads `World` through phases containing
/// sends (P4a.3). Skips per-phase `def` emission — inlines transforms
/// + actions per phase instead.
fn emit_phased_route_entry_world_threaded(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    route: &Route,
    phases: &[PhaseBlock],
    return_ty: Option<&str>,
    fail_mode: bool,
    profile: LeanProfile,
    graphs: &crate::analysis::ProgramGraphs,
) {
    emit_route_signature(
        out, program, entity, route, return_ty, fail_mode, None, profile,
    );
    out.push_str(" :=\n");
    let body = build_phased_route_body_world_threaded(
        program, entity, route, phases, return_ty, fail_mode, profile, graphs,
    );
    let body = super::evm::rewrite_route_body(&body, profile.predictable);
    write_indented(out, &body, 1);
    out.push_str("\n\n");
}

/// Bind the pre-commit snapshots of `actions` — reads of members the
/// enclosing phase's `transforms` are about to commit over, performed by the
/// phase's outbound effects — into `let _pre_<m>_<i> := …` lines ahead of the
/// `let s := { s with … }` commit, and return the actions rewritten to read
/// those locals. `None` when there is nothing to snapshot (the caller then
/// lowers the original actions — byte-identical output for untouched routes).
///
/// Lean mirror of the EVM backend's `gen_updates_with_snapshots_from_phase_ir`
/// (see [`crate::codegen::PrecommitSnapshot`] for the shared semantics). The
/// motivating divergence: `UniswapV2Pair.burn` pays out
/// `mul_div(m_balances[this], bal0, m_total_supply)` in the phase that zeroes
/// both, so the Lean model transferred 0 while the fixed EVM contract pays
/// the pre-burn share.
fn emit_precommit_snapshot_lets(
    body: &mut String,
    indent: usize,
    program: &Program,
    entity: &Entity,
    route: &Route,
    phase: Option<&str>,
    transforms: &[ResolvedTransform],
    actions: &[RouteAction],
    fail_mode: bool,
    profile: LeanProfile,
) -> Option<Vec<RouteAction>> {
    if transforms.is_empty() {
        return None;
    }
    let committed: std::collections::HashSet<String> =
        transforms.iter().map(|t| t.member.name.clone()).collect();
    let snaps = precommit_snapshots(entity, &committed, actions);
    if snaps.is_empty() {
        return None;
    }
    let ctx = expr_ctx_for(
        Carrier::World { fail: fail_mode },
        program,
        entity,
        route,
        phase,
        false,
        profile,
    );
    for snap in &snaps {
        for line in super::expr::lower_fail_mode_let_stmts(
            &crate::ast::Pattern::Ident(snap.local.clone()),
            &snap.read,
            &ctx,
            fail_mode,
        ) {
            push_indent(body, indent);
            body.push_str(&line);
            body.push('\n');
        }
    }
    Some(rewrite_actions_precommit(actions, &snaps))
}

fn build_phased_route_body_world_threaded(
    program: &Program,
    entity: &Entity,
    route: &Route,
    phases: &[PhaseBlock],
    return_ty: Option<&str>,
    fail_mode: bool,
    profile: LeanProfile,
    graphs: &crate::analysis::ProgramGraphs,
) -> String {
    let field = entity_field_name(&entity.name);
    let setter_fn = format!("Cambrian.Generated.World.with{}", entity.name);
    let mut body = String::new();
    if fail_mode {
        body.push_str("do\n");
        push_indent(&mut body, 1);
        body.push_str(&format!("let s := w.storage.{} inst\n", field));
        // `from`-checks then `where`-checks (consistent with every
        // other route-assembly path).
        body.push_str(&gen_from_checks(program, entity, route, 1, profile));
        for (i, w) in route.where_clauses.iter().enumerate() {
            emit_pre_guard(
                &mut body,
                1,
                Some(&entity.name),
                entity,
                route,
                None,
                i,
                w,
                &route_arg_list(route),
                "",
                false,
            );
        }
    } else {
        body.push_str(&gen_from_checks(program, entity, route, 0, profile));
        body.push_str(&format!("let s := w.storage.{} inst\n", field));
    }
    let (orders, _) = crate::validate::build_temporal_orders(entity);
    for ph in phases {
        let transforms = order_transforms_temporally(
            collect_transforms(entity, &route.name, Some(&ph.name)),
            &orders,
            &route.name,
        );
        let captured = super::member::phase_captured_vars_before(
            program,
            entity,
            route,
            Some(&ph.name),
            profile,
        );
        let captured_suffix = if captured.is_empty() {
            String::new()
        } else {
            let names: Vec<String> = captured
                .iter()
                .map(|(n, _)| super::core::types::lean_safe_ident(n))
                .collect();
            format!(" {}", names.join(" "))
        };
        // Per-phase `where` clauses run *before* the phase's transforms
        // and side-effect actions: matches the Cambrian semantics and
        // mirrors `build_route_body`'s ordering. Captured vars from
        // strictly earlier phases are in scope; the pre-fn signature
        // already accepts them.
        for (i, w) in ph.where_clauses.iter().enumerate() {
            let indent = if fail_mode { 1 } else { 0 };
            emit_pre_guard(
                &mut body,
                indent,
                Some(&entity.name),
                entity,
                route,
                Some(&ph.name),
                i,
                w,
                &route_arg_list(route),
                &captured_suffix,
                false,
            );
        }
        // The state the phase's effects read is captured ahead of the commit
        // (pre-commit snapshots); the actions below then lower against the
        // `_pre_*` locals instead of the committed `s`.
        let rewritten_actions = emit_precommit_snapshot_lets(
            &mut body,
            if fail_mode { 1 } else { 0 },
            program,
            entity,
            route,
            Some(&ph.name),
            &transforms,
            &ph.actions,
            fail_mode,
            profile,
        );
        if !transforms.is_empty() {
            let indent = if fail_mode { 1 } else { 0 };
            push_indent(&mut body, indent);
            let bind = if crate::codegen::lean::core::route_local::transforms_use_checked_arith(
                &transforms,
                profile,
                &program.pure_fns,
            ) {
                "←"
            } else {
                ":="
            };
            body.push_str(&format!(
                "let s {bind} {}\n",
                apply_transforms_term(program, entity, route, &transforms, profile),
            ));
        }
        let _ = emit_world_threaded_actions(
            &mut body,
            program,
            entity,
            route,
            rewritten_actions.as_deref().unwrap_or(&ph.actions),
            Some(ph.name.as_str()),
            fail_mode,
            if fail_mode { 1 } else { 0 },
            profile,
            graphs,
        );
    }
    // A trailing `return(...)` lives in the final phase that declares
    // one; lower it into the `(w, <payload>)` tail.
    let nested_phase_actions: &[RouteAction] = phases
        .iter()
        .rev()
        .find(|ph| ph.actions.iter().any(action_can_return))
        .map(|ph| ph.actions.as_slice())
        .unwrap_or(&[]);
    let payload_str = world_threaded_payload(
        program,
        entity,
        route,
        phases
            .iter()
            .rev()
            .find_map(|ph| find_return_payload(&ph.actions))
            .as_deref(),
        nested_phase_actions,
        profile,
    );

    let indent = if fail_mode { 1 } else { 0 };
    push_indent(&mut body, indent);
    body.push_str(&format!("let w := {} w inst s\n", setter_fn));
    push_indent(&mut body, indent);
    if return_ty.is_some() {
        if fail_mode {
            body.push_str(&format!("pure (w, {})\n", payload_str));
        } else {
            body.push_str(&format!("(w, {})\n", payload_str));
        }
    } else if fail_mode {
        body.push_str("pure w\n");
    } else {
        body.push_str("w\n");
    }
    body
}

// ---------------------------------------------------------------------------
// World wrap around a named Local transition
//
// * Reading `s := w.storage.<entity> inst`.
// * Calling `<E>.Local.<route> s ctx inst …`.
// * Writing the resulting state back via `with<Entity>`.
// ---------------------------------------------------------------------------

/// Thin Routes wrapper: load state, call the named Local transition, store.
fn wrap_calling_local(
    entity: &Entity,
    route: &Route,
    return_ty: Option<&str>,
    fail_mode: bool,
) -> String {
    let field = entity_field_name(&entity.name);
    let setter_fn = format!("Cambrian.Generated.World.with{}", entity.name);
    let read_state = format!("w.storage.{} inst", field);
    let mut call = format!("{}.Local.{} s ctx inst", entity.name, route.name);
    for p in &route.params {
        call.push(' ');
        call.push_str(&super::core::types::lean_safe_ident(&p.name));
    }

    let mut out = String::new();
    let route_args = route_arg_list(route);
    match (fail_mode, return_ty.is_some()) {
        (false, false) => {
            out.push_str(&format!("let s := {}\n", read_state));
            out.push_str(&format!("let s := {}\n", call));
            out.push_str(&format!("{} w inst s\n", setter_fn));
        }
        (false, true) => {
            out.push_str(&format!("let s := {}\n", read_state));
            out.push_str(&format!("let (s, payload) := {}\n", call));
            out.push_str(&format!("({} w inst s, payload)\n", setter_fn));
        }
        (true, false) => {
            out.push_str("do\n");
            out.push_str(&format!("  let s := {}\n", read_state));
            for (i, w) in route.where_clauses.iter().enumerate() {
                if !expr_needs_world(entity, &w.condition) {
                    continue;
                }
                emit_pre_guard(
                    &mut out,
                    1,
                    Some(&entity.name),
                    entity,
                    route,
                    None,
                    i,
                    w,
                    &route_args,
                    "",
                    false,
                );
            }
            out.push_str(&format!("  let s ← {}\n", call));
            out.push_str(&format!("  pure ({} w inst s)\n", setter_fn));
        }
        (true, true) => {
            out.push_str("do\n");
            out.push_str(&format!("  let s := {}\n", read_state));
            for (i, w) in route.where_clauses.iter().enumerate() {
                if !expr_needs_world(entity, &w.condition) {
                    continue;
                }
                emit_pre_guard(
                    &mut out,
                    1,
                    Some(&entity.name),
                    entity,
                    route,
                    None,
                    i,
                    w,
                    &route_args,
                    "",
                    false,
                );
            }
            out.push_str(&format!("  let (s, payload) ← {}\n", call));
            out.push_str(&format!("  pure ({} w inst s, payload)\n", setter_fn,));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Route signature emission
// ---------------------------------------------------------------------------

fn emit_route_signature(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    route: &Route,
    return_ty: Option<&str>,
    fail_mode: bool,
    _phase: Option<&str>,
    profile: LeanProfile,
) {
    out.push_str(&format!(
        "def {} (w : Cambrian.Generated.World) (inst : {}.Identity) (ctx : Cambrian.MsgCtx)",
        route.name, entity.name,
    ));
    for p in &route.params {
        out.push_str(&format!(
            " ({} : {})",
            super::core::types::lean_safe_ident(&p.name),
            lower_route_param_type(program, entity, &p.ty, profile),
        ));
    }
    let core = match return_ty {
        Some(rt) => format!("Cambrian.Generated.World × {}", rt),
        None => "Cambrian.Generated.World".to_string(),
    };
    let full = if fail_mode {
        format!("Cambrian.RouteResult ({})", core)
    } else {
        core
    };
    out.push_str(&format!(" : {}", full));
}

// ---------------------------------------------------------------------------
// World-threaded body assembly (P3.8) — Lean-EVM adapter
//
// Routes containing `Send` / `VarCall` actions need to thread `w`
// alongside `s` so sends can mutate the world (raw transfers update
// the balance ledger; same-entity self-calls go through
// `<Self>.Routes.<msg>`). State-local pieces (from-checks, `_pre_*`
// throws, `{ s with … }` transforms) are still built via
// [`super::core::route_local`]; this path owns the World read/writeback
// and send/deploy interleaving.
//
// Follow-up (local-transitions): peel remaining World arms of
// `lower_action` into `evm/route_world.rs` once core exposes a pure
// State-carrier statement emitter without the unified Carrier match.
// ---------------------------------------------------------------------------

fn build_route_body_world_threaded(
    program: &Program,
    entity: &Entity,
    route: &Route,
    actions: &[RouteAction],
    return_ty: Option<&str>,
    fail_mode: bool,
    profile: LeanProfile,
    graphs: &crate::analysis::ProgramGraphs,
) -> String {
    let field = entity_field_name(&entity.name);
    let setter_fn = format!("Cambrian.Generated.World.with{}", entity.name);
    let (orders, _) = crate::validate::build_temporal_orders(entity);
    let transforms = order_transforms_temporally(
        collect_transforms(entity, &route.name, None),
        &orders,
        &route.name,
    );

    // A trailing `return(...)` in a send-bearing view route is lowered
    // into the final `(w, <payload>)` tail (see [`world_threaded_payload`]);
    // the `Return` action itself is a no-op in `emit_world_threaded_action`.
    let payload_str = world_threaded_payload(
        program,
        entity,
        route,
        find_return_payload(actions).as_deref(),
        actions,
        profile,
    );

    let mut body = String::new();

    if fail_mode {
        body.push_str("do\n");
        push_indent(&mut body, 1);
        body.push_str(&format!("let s := w.storage.{} inst\n", field));
        // `from`-checks then `where`-checks (consistent with every
        // other route-assembly path).
        body.push_str(&gen_from_checks(program, entity, route, 1, profile));
        for (i, w) in route.where_clauses.iter().enumerate() {
            let route_args = route_arg_list(route);
            emit_pre_guard(
                &mut body,
                1,
                Some(&entity.name),
                entity,
                route,
                None,
                i,
                w,
                &route_args,
                "",
                false,
            );
        }
        let rewritten_actions = emit_precommit_snapshot_lets(
            &mut body, 1, program, entity, route, None, &transforms, actions, fail_mode, profile,
        );
        if !transforms.is_empty() {
            push_indent(&mut body, 1);
            let bind = if crate::codegen::lean::core::route_local::transforms_use_checked_arith(
                &transforms,
                profile,
                &program.pure_fns,
            ) {
                "←"
            } else {
                ":="
            };
            body.push_str(&format!(
                "let s {bind} {}\n",
                apply_transforms_term(program, entity, route, &transforms, profile),
            ));
        }
        let terminated = emit_world_threaded_actions(
            &mut body,
            program,
            entity,
            route,
            rewritten_actions.as_deref().unwrap_or(actions),
            None,
            fail_mode,
            1,
            profile,
            graphs,
        );
        // A `throw` already emitted its writeback + `throw …`, closing the `do`
        // block; the success tail below is dead code (T-X-009 / LEAN-H1).
        if !terminated {
            push_indent(&mut body, 1);
            body.push_str(&format!("let w := {} w inst s\n", setter_fn));
            push_indent(&mut body, 1);
            if return_ty.is_some() {
                body.push_str(&format!("pure (w, {})\n", payload_str));
            } else {
                body.push_str("pure w\n");
            }
        }
    } else {
        body.push_str(&format!("let s := w.storage.{} inst\n", field));
        body.push_str(&gen_from_checks(program, entity, route, 0, profile));
        let rewritten_actions = emit_precommit_snapshot_lets(
            &mut body, 0, program, entity, route, None, &transforms, actions, fail_mode, profile,
        );
        if !transforms.is_empty() {
            let bind = if crate::codegen::lean::core::route_local::transforms_use_checked_arith(
                &transforms,
                profile,
                &program.pure_fns,
            ) {
                "←"
            } else {
                ":="
            };
            body.push_str(&format!(
                "let s {bind} {}\n",
                apply_transforms_term(program, entity, route, &transforms, profile),
            ));
        }
        let _ = emit_world_threaded_actions(
            &mut body,
            program,
            entity,
            route,
            rewritten_actions.as_deref().unwrap_or(actions),
            None,
            fail_mode,
            0,
            profile,
            graphs,
        );
        body.push_str(&format!("let w := {} w inst s\n", setter_fn));
        if return_ty.is_some() {
            body.push_str(&format!("(w, {})\n", payload_str));
        } else {
            body.push_str("w\n");
        }
    }
    body
}

/// Render the payload term for a world-threaded route's `(w, <payload>)`
/// tail. When the route declares a top-level `return(...)`, lower its
/// values against the entry context (member reads resolve to `s`,
/// `let`/`var` bindings stay in scope). When the only `return` is nested
/// inside a conditional, lift the conditional into a value-producing
/// `if … then … else …` term via [`lower_view_payload_term`]; fall back
/// to `default` only when the route never returns a value.
fn world_threaded_payload(
    program: &Program,
    entity: &Entity,
    route: &Route,
    return_values: Option<&[crate::ast::Expr]>,
    actions: &[RouteAction],
    profile: LeanProfile,
) -> String {
    match return_values {
        Some(values) => {
            let mut ctx = expr_ctx_for(
                Carrier::World { fail: false },
                program,
                entity,
                route,
                None,
                false,
                profile,
            );
            // Phase-local `let` binders are already in the world-threaded
            // do-block; mark them so Ident payloads resolve as locals
            // rather than State fields (UPSTREAM B-29).
            for a in actions {
                if let RouteAction::Let { pattern, .. } = a {
                    for n in super::core::route_local::pattern_binders(pattern) {
                        ctx.lets.insert(n);
                    }
                }
                if matches!(a, RouteAction::Return { .. }) {
                    break;
                }
            }
            render_return_payload(values, &ctx, route.return_type.as_ref())
        }
        // No top-level `return`: a view route may still return from
        // inside a conditional. Lift that into a value-producing term so
        // the payload survives (mirrors the non-threaded path); fall back
        // to `default` only when there is no `return` anywhere.
        None if actions.iter().any(action_can_return) => lower_view_payload_term(
            program,
            entity,
            route,
            None,
            actions,
            "default",
            Carrier::World { fail: false },
            &HashSet::new(),
            profile,
        ),
        None => "default".to_string(),
    }
}

/// Returns `true` when the action sequence short-circuited via a `throw`
/// (world carrier), so the caller can suppress the dead writeback + success
/// tail (T-X-009 / LEAN-H1).
fn emit_world_threaded_actions(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    route: &Route,
    actions: &[RouteAction],
    phase: Option<&str>,
    fail_mode: bool,
    indent: usize,
    profile: LeanProfile,
    graphs: &crate::analysis::ProgramGraphs,
) -> bool {
    let route_ir = ir::lower_route(program, entity, route, graphs);
    let lc = LowerCtx {
        program,
        entity,
        route,
        phase,
        return_ty: None,
        profile,
        route_ir: Some(&route_ir),
        nat_idents: HashSet::new(),
    };
    let (stmts, terminated) = lower_actions(&lc, actions, Carrier::World { fail: fail_mode });
    super::core::stmt::render_into(out, &stmts, indent);
    terminated
}

fn extract_send_value(send_options: Option<&crate::ast::Expr>) -> Option<crate::ast::Expr> {
    super::evm::send::extract_send_value(send_options)
}

/// Extract the `return(...)` payload from a phased route's phase bodies
/// (same logic as the world-threaded phased assembler).
fn phased_return_payload(
    program: &Program,
    entity: &Entity,
    route: &Route,
    phases: &[PhaseBlock],
    profile: LeanProfile,
) -> String {
    let nested_phase_actions: &[RouteAction] = phases
        .iter()
        .rev()
        .find(|ph| ph.actions.iter().any(action_can_return))
        .map(|ph| ph.actions.as_slice())
        .unwrap_or(&[]);
    world_threaded_payload(
        program,
        entity,
        route,
        phases
            .iter()
            .rev()
            .find_map(|ph| find_return_payload(&ph.actions))
            .as_deref(),
        nested_phase_actions,
        profile,
    )
}

/// True when every action in `actions` is a comment-only stub at the
/// current target (sends, deploys, rescues, custom throws in non-fail
/// mode, etc.). Used by the `Conditional` emitter to detect empty
/// branches that would produce an ill-formed `if … then <nothing>
/// else …`.
fn all_actions_are_skipped(actions: &[RouteAction]) -> bool {
    actions.iter().all(|a| {
        matches!(
            a,
            RouteAction::Send { .. }
                | RouteAction::VarCall { .. }
                | RouteAction::Deploy { .. }
                | RouteAction::Effect { .. }
                | RouteAction::Rescue { .. }
                | RouteAction::CallRoute { .. }
                | RouteAction::Emit { .. }
                | RouteAction::UpdateCode { .. }
        )
    })
}

// ===========================================================================
// Unified route-action statement lowering (Lean statement IR, Layer 3)
//
// Still lives in the adapter because `Carrier::World` arms call into
// `lean_send` / `lean_deploy` / World writeback. Core's `build_route_body`
// reaches the State arms via [`emit_actions`] (`Carrier::State` only).
// Future local-transitions follow-up: split World arms into
// `evm/route_world.rs` and leave a State-only emitter in core.
//
// `lower_action` is the *single* exhaustive `match` over `RouteAction`,
// branching internally on the [`Carrier`]. It produces a [`LeanStmt`] tree
// that owns no indentation; [`super::core::stmt::render_into`] applies it.
// ===========================================================================

/// Bundle of the per-route context every lowering arm needs, so the IR
/// functions don't thread eight arguments each.
#[derive(Clone)]
struct LowerCtx<'a> {
    program: &'a Program,
    entity: &'a Entity,
    route: &'a Route,
    phase: Option<&'a str>,
    return_ty: Option<&'a str>,
    profile: LeanProfile,
    /// When present, send targets and route facts come from mid-level IR (P6 E3).
    route_ir: Option<&'a RouteIr>,
    /// Range-loop binders currently in scope (UPSTREAM B-19).
    nat_idents: HashSet<String>,
}

fn ir_phase_stmts<'a>(route_ir: &'a RouteIr, phase: Option<&str>) -> Option<&'a [IrStmt]> {
    route_ir
        .phases
        .iter()
        .find(|p| p.name.as_deref() == phase)
        .map(|p| p.stmts.as_slice())
}

fn ir_send_target(stmt: &IrStmt) -> Option<&SendTarget> {
    match stmt {
        IrStmt::Send { target, .. } | IrStmt::VarCall { target, .. } => Some(target),
        _ => None,
    }
}

impl<'a> LowerCtx<'a> {
    /// Expression context for the active carrier: the world-threaded entry
    /// signature (`w`/`inst` in scope) vs the bare state-only signature.
    fn expr_ctx(&self, carrier: Carrier) -> LeanExprCtx<'a> {
        expr_ctx_for(
            carrier,
            self.program,
            self.entity,
            self.route,
            self.phase,
            false,
            self.profile,
        )
        .with_extra_nat_idents(self.nat_idents.iter().cloned())
    }

    fn with_phase(&self, phase: Option<&'a str>) -> LowerCtx<'a> {
        LowerCtx {
            phase,
            nat_idents: self.nat_idents.clone(),
            program: self.program,
            entity: self.entity,
            route: self.route,
            return_ty: self.return_ty,
            profile: self.profile,
            route_ir: self.route_ir,
        }
    }

    fn with_extra_nat_idents(&self, names: impl IntoIterator<Item = String>) -> LowerCtx<'a> {
        let mut nat_idents = self.nat_idents.clone();
        nat_idents.extend(names);
        LowerCtx {
            nat_idents,
            program: self.program,
            entity: self.entity,
            route: self.route,
            phase: self.phase,
            return_ty: self.return_ty,
            profile: self.profile,
            route_ir: self.route_ir,
        }
    }
}

/// Lower a sequence of actions. Returns the statement list plus whether the
/// sequence short-circuited (`return`/`throw` in a failing body), mirroring the
/// old `emit_actions` so callers can suppress dead trailing `pure s`/`()`.
///
/// In a state-only failing body both `return` and `throw`/`throwCustom`
/// terminate. In a *world-threaded* failing body only `throw`/`throwCustom`
/// terminate (they emit `throw …`, which closes the `do` block, so any trailing
/// actions and the success tail are dead — see T-X-009 / LEAN-H1). A world
/// `return` is a no-op inline (its value is produced by `world_threaded_payload`
/// as the `(w, <payload>)` tail), so it must **not** terminate — otherwise the
/// payload tail would be dropped.
fn lower_actions(
    lc: &LowerCtx,
    actions: &[RouteAction],
    carrier: Carrier,
) -> (Vec<LeanStmt>, bool) {
    let ir_stmts = lc.route_ir.and_then(|ir| ir_phase_stmts(ir, lc.phase));
    lower_actions_with_ir(lc, actions, carrier, ir_stmts)
}

fn lower_actions_with_ir(
    lc: &LowerCtx,
    actions: &[RouteAction],
    carrier: Carrier,
    ir_stmts: Option<&[IrStmt]>,
) -> (Vec<LeanStmt>, bool) {
    let mut out = Vec::new();
    let mut terminated = false;
    for (i, action) in actions.iter().enumerate() {
        let ir_stmt = ir_stmts.and_then(|stmts| stmts.get(i));
        out.extend(lower_action(lc, action, carrier, ir_stmt));
        let is_terminator = match action {
            RouteAction::Throw { .. } | RouteAction::ThrowCustom { .. } => true,
            RouteAction::Return { .. } => !carrier.is_world(),
            // LEAN-H2: both arms fully terminate ⇒ no fallthrough to `pure (s, default)`.
            RouteAction::Conditional {
                then_actions,
                else_actions,
                ..
            } if carrier.is_fail() && !carrier.is_world() => {
                actions_fully_terminate(then_actions) && actions_fully_terminate(else_actions)
            }
            _ => false,
        };
        if carrier.is_fail() && is_terminator {
            terminated = true;
            break;
        }
    }
    (out, terminated)
}

/// True when every control-flow path through `actions` ends in `return` /
/// `throw` (recursing into `if` arms). Used so fail-mode views whose only
/// `return`s live inside conditionals do not get a dead `pure (s, default)`
/// tail (LEAN-H2).
fn actions_fully_terminate(actions: &[RouteAction]) -> bool {
    if actions.is_empty() {
        return false;
    }
    for action in actions {
        match action {
            RouteAction::Return { .. }
            | RouteAction::Throw { .. }
            | RouteAction::ThrowCustom { .. } => return true,
            RouteAction::Conditional {
                then_actions,
                else_actions,
                ..
            } => {
                if actions_fully_terminate(then_actions) && actions_fully_terminate(else_actions) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Collect `var` binder names from an action tree (nested `if` / `for` / `rescue`).
fn collect_var_call_names(actions: &[RouteAction], out: &mut Vec<String>) {
    for action in actions {
        match action {
            RouteAction::VarCall { name, .. } => {
                if !out.iter().any(|n| n == name) {
                    out.push(name.clone());
                }
            }
            RouteAction::Conditional {
                then_actions,
                else_actions,
                ..
            } => {
                collect_var_call_names(then_actions, out);
                collect_var_call_names(else_actions, out);
            }
            RouteAction::For { body, .. } => collect_var_call_names(body, out),
            RouteAction::Rescue { action: inner, .. } => {
                collect_var_call_names(std::slice::from_ref(inner.as_ref()), out);
            }
            _ => {}
        }
    }
}

fn product_tail(w_term: &str, captures: &[String]) -> String {
    if captures.is_empty() {
        w_term.to_string()
    } else {
        format!("({}, {})", w_term, captures.join(", "))
    }
}

/// Like [`product_tail`], but slots not bound in this branch become `default`
/// (UPSTREAM B-21 — escaping `var` from one `if` arm must not name unbound
/// idents in the sibling arm).
fn product_tail_for_branch(w_term: &str, all_captures: &[String], bound: &[String]) -> String {
    if all_captures.is_empty() {
        return w_term.to_string();
    }
    let parts: Vec<&str> = all_captures
        .iter()
        .map(|c| {
            if bound.iter().any(|b| b == c) {
                c.as_str()
            } else {
                "default"
            }
        })
        .collect();
    format!("({}, {})", w_term, parts.join(", "))
}

fn product_default_tail(captures: &[String]) -> String {
    if captures.is_empty() {
        "w".to_string()
    } else {
        let defaults: Vec<&str> = captures.iter().map(|_| "default").collect();
        format!("(w, {})", defaults.join(", "))
    }
}

/// Lower one branch of a world-threaded `Conditional` as a self-contained
/// `(Id.run do …)` / `(do …)` block. When `all_captures` is non-empty the block
/// yields `(w × captured…)` so binders escape the branch (T-LEAN-ST-002).
/// `bound_in_branch` lists which of those captures this arm actually binds;
/// missing slots are filled with `default` (UPSTREAM B-21).
fn lower_world_branch(
    lc: &LowerCtx,
    actions: &[RouteAction],
    carrier: Carrier,
    ir_stmts: Option<&[IrStmt]>,
    all_captures: &[String],
    bound_in_branch: &[String],
) -> Vec<LeanStmt> {
    let fail = carrier.is_fail();
    if actions.is_empty() {
        let tail = product_default_tail(all_captures);
        return vec![LeanStmt::line(if fail {
            format!("pure {}", tail)
        } else {
            tail
        })];
    }
    let setter = format!("Cambrian.Generated.World.with{}", lc.entity.name);
    let field = entity_field_name(&lc.entity.name);
    let mut inner = vec![LeanStmt::line(format!("let s := w.storage.{} inst", field))];
    inner.extend(lower_actions_with_ir(lc, actions, carrier, ir_stmts).0);
    inner.push(LeanStmt::line(format!("let w := {} w inst s", setter)));
    let result = product_tail_for_branch("w", all_captures, bound_in_branch);
    inner.push(LeanStmt::line(if fail {
        format!("pure {})", result)
    } else {
        format!("{})", result)
    }));
    vec![
        LeanStmt::line(if fail { "(do" } else { "(Id.run do" }),
        LeanStmt::Block(inner),
    ]
}

/// World-carrier lowering for the effectful actions (`Send` / `VarCall` /
/// `Deploy` / `CallRoute` / `Emit`). Each lowers to a per-line `let` chain
/// (a `Raw` snippet from `lean_send`/`lean_deploy`/…) or a diagnostic
/// comment.
fn lower_world_effect(
    lc: &LowerCtx,
    action: &RouteAction,
    fail: bool,
    ir_stmt: Option<&IrStmt>,
) -> Vec<LeanStmt> {
    let program = lc.program;
    let entity = lc.entity;
    let route = lc.route;
    let ctx = lc.expr_ctx(Carrier::World { fail });
    match action {
        RouteAction::Send {
            message,
            args,
            dest,
            send_options,
        } => {
            let target = ir_stmt
                .and_then(ir_send_target)
                .cloned()
                .unwrap_or_else(|| classify_dest(dest, entity, route, program));
            match message {
                None => {
                    let value = extract_send_value(send_options.as_ref());
                    let snippet = if let Some(v) = value.as_ref() {
                        lower_raw_value_receive(program, entity, &target, v, &ctx, fail)
                            .unwrap_or_else(|| {
                                lower_raw_transfer(entity, dest, value.as_ref(), &ctx, fail)
                            })
                    } else {
                        lower_raw_transfer(entity, dest, value.as_ref(), &ctx, fail)
                    };
                    vec![LeanStmt::Raw(snippet)]
                }
                Some(msg_name) => match target {
                    SendTarget::SameEntity { id_args } => {
                        if let Some(target) = resolve_self_route(entity, msg_name) {
                            let callee_fail = route_fail_mode(program, entity, target);
                            let is_view = route_is_view(target);
                            let send_value = extract_send_value(send_options.as_ref());
                            let snippet = lower_self_var_call(
                                "_",
                                target,
                                args,
                                &id_args,
                                entity,
                                &ctx,
                                callee_fail,
                                fail,
                                is_view,
                                send_value.as_ref(),
                            );
                            vec![LeanStmt::Raw(snippet)]
                        } else {
                            vec![LeanStmt::line(format!(
                                "-- L8: typed send to '{}' did not resolve to a same-entity route",
                                msg_name,
                            ))]
                        }
                    }
                    SendTarget::CrossEntity {
                        entity: ent,
                        id_args,
                    } => {
                        if let Some(target_ent) = program.entities.iter().find(|e| e.name == ent) {
                            if let Some(target) = resolve_route(target_ent, msg_name) {
                                let callee_fail = route_fail_mode(program, target_ent, target);
                                let is_view = route_is_view(target);
                                let value = extract_send_value(send_options.as_ref());
                                let snippet = lower_cross_var_call(
                                    "_",
                                    &ent,
                                    target,
                                    args,
                                    &id_args,
                                    entity,
                                    target_ent,
                                    &ctx,
                                    callee_fail,
                                    fail,
                                    is_view,
                                    value.as_ref(),
                                );
                                return vec![LeanStmt::Raw(snippet)];
                            }
                        }
                        vec![LeanStmt::line(format!(
                            "-- L8: typed send to '{}' did not resolve on entity {}",
                            msg_name, ent,
                        ))]
                    }
                    SendTarget::DynamicTyped {
                        entity: ent,
                        dest_ident,
                    } => {
                        let target_ent = program.entities.iter().find(|e| e.name == ent);
                        let target_route = target_ent.and_then(|e| resolve_route(e, msg_name));
                        let value = extract_send_value(send_options.as_ref());
                        let snippet = super::evm::dispatch::lower_dynamic_send(
                            "_",
                            &ent,
                            target_route,
                            msg_name,
                            args,
                            &dest_ident,
                            entity,
                            &ctx,
                            fail,
                            value.as_ref(),
                        );
                        vec![LeanStmt::Raw(snippet)]
                    }
                    SendTarget::DynamicUntyped { dest_ident } => {
                        let value = extract_send_value(send_options.as_ref());
                        let snippet = super::evm::dispatch::lower_dynamic_send_untyped(
                            "_",
                            msg_name,
                            args,
                            &dest_ident,
                            entity,
                            &ctx,
                            fail,
                            value.as_ref(),
                            program,
                        );
                        vec![LeanStmt::Raw(snippet)]
                    }
                    SendTarget::ExternEntity { entity: ent } => {
                        let is_view = program
                            .extern_entities
                            .iter()
                            .find(|e| e.name == ent)
                            .and_then(|e| e.routes.iter().find(|r| r.name == *msg_name))
                            .map(|r| r.return_type.is_some())
                            .unwrap_or(false);
                        let snippet =
                            lower_extern_call("_", &ent, msg_name, args, &ctx, fail, is_view);
                        vec![LeanStmt::Raw(snippet)]
                    }
                    SendTarget::Raw => {
                        vec![LeanStmt::line(format!(
                            "-- L8: typed send '{}' to unresolvable dest skipped",
                            msg_name,
                        ))]
                    }
                },
            }
        }
        RouteAction::VarCall {
            name,
            message,
            args,
            dest,
            send_options,
        } => {
            let target = ir_stmt
                .and_then(ir_send_target)
                .cloned()
                .unwrap_or_else(|| classify_dest(dest, entity, route, program));
            match target {
                SendTarget::SameEntity { id_args } => {
                    if let Some(target) = resolve_self_route(entity, message) {
                        let callee_fail = route_fail_mode(program, entity, target);
                        let is_view = route_is_view(target);
                        let send_value = extract_send_value(send_options.as_ref());
                        let snippet = lower_self_var_call(
                            name,
                            target,
                            args,
                            &id_args,
                            entity,
                            &ctx,
                            callee_fail,
                            fail,
                            is_view,
                            send_value.as_ref(),
                        );
                        vec![LeanStmt::Raw(snippet)]
                    } else {
                        vec![LeanStmt::line(format!(
                            "-- L8: var-call target route '{}' missing on entity {}",
                            message, entity.name,
                        ))]
                    }
                }
                SendTarget::CrossEntity {
                    entity: ent,
                    id_args,
                } => {
                    if let Some(target_ent) = program.entities.iter().find(|e| e.name == ent) {
                        if let Some(target) = resolve_route(target_ent, message) {
                            let callee_fail = route_fail_mode(program, target_ent, target);
                            let is_view = route_is_view(target);
                            let value = extract_send_value(send_options.as_ref());
                            let snippet = lower_cross_var_call(
                                name,
                                &ent,
                                target,
                                args,
                                &id_args,
                                entity,
                                target_ent,
                                &ctx,
                                callee_fail,
                                fail,
                                is_view,
                                value.as_ref(),
                            );
                            return vec![LeanStmt::Raw(snippet)];
                        }
                    }
                    vec![LeanStmt::line(format!(
                        "-- L8: var-call '{}' = msg(...) ~> unresolved route on entity {}",
                        name, ent,
                    ))]
                }
                SendTarget::DynamicTyped {
                    entity: ent,
                    dest_ident,
                } => {
                    let target_ent = program.entities.iter().find(|e| e.name == ent);
                    let target_route = target_ent.and_then(|e| resolve_route(e, message));
                    let value = extract_send_value(send_options.as_ref());
                    let snippet = super::evm::dispatch::lower_dynamic_send(
                        name,
                        &ent,
                        target_route,
                        message,
                        args,
                        &dest_ident,
                        entity,
                        &ctx,
                        fail,
                        value.as_ref(),
                    );
                    vec![LeanStmt::Raw(snippet)]
                }
                SendTarget::DynamicUntyped { dest_ident } => {
                    let value = extract_send_value(send_options.as_ref());
                    let snippet = super::evm::dispatch::lower_dynamic_send_untyped(
                        name,
                        message,
                        args,
                        &dest_ident,
                        entity,
                        &ctx,
                        fail,
                        value.as_ref(),
                        program,
                    );
                    vec![LeanStmt::Raw(snippet)]
                }
                SendTarget::ExternEntity { entity: ent } => {
                    let is_view = program
                        .extern_entities
                        .iter()
                        .find(|e| e.name == ent)
                        .and_then(|e| e.routes.iter().find(|r| r.name == *message))
                        .map(|r| r.return_type.is_some())
                        .unwrap_or(false);
                    let snippet =
                        lower_extern_call(name, &ent, message, args, &ctx, fail, is_view);
                    vec![LeanStmt::Raw(snippet)]
                }
                SendTarget::Raw => {
                    vec![LeanStmt::line(format!(
                        "-- L8: var-call '{}' = msg(...) ~> unresolvable dest skipped",
                        name,
                    ))]
                }
            }
        }
        RouteAction::Deploy {
            entity: target,
            send_options,
            constructor_args,
        } => {
            let snippet = lower_deploy(
                program,
                entity,
                target,
                constructor_args,
                send_options.as_ref(),
                &ctx,
                fail,
            );
            vec![LeanStmt::Raw(snippet)]
        }
        RouteAction::CallRoute { name, args } => {
            if let Some(target) = resolve_self_route(entity, name) {
                let callee_fail = route_fail_mode(program, entity, target);
                let is_view = route_is_view(target);
                let snippet =
                    lower_call_route(target, args, entity, &ctx, callee_fail, fail, is_view);
                vec![LeanStmt::Raw(snippet)]
            } else {
                vec![LeanStmt::line(format!(
                    "-- V8: call target route '{}' missing on entity {}",
                    name, entity.name,
                ))]
            }
        }
        RouteAction::Emit { event_name, args } => {
            match super::evm::world::resolve_event_ctor(program, entity, event_name) {
                Some(ctor) => {
                    let arg_terms: Vec<String> = args.iter().map(|a| gen_expr(a, &ctx)).collect();
                    let payload = if arg_terms.is_empty() {
                        format!("Cambrian.Generated.Event.{}", ctor)
                    } else {
                        format!(
                            "(Cambrian.Generated.Event.{} {})",
                            ctor,
                            arg_terms.join(" ")
                        )
                    };
                    vec![LeanStmt::line(format!(
                        "let w := Cambrian.WorldState.emit w {}",
                        payload
                    ))]
                }
                None => {
                    vec![LeanStmt::line(format!(
                        "-- V34: emit '{}' references an undeclared event",
                        event_name,
                    ))]
                }
            }
        }
        _ => unreachable!("lower_world_effect called with a non-effect action"),
    }
}

/// The single exhaustive `RouteAction` × `Carrier` lowering. Adding a
/// `RouteAction` variant fails to compile here until handled; routing a
/// variant to a carrier that cannot model it yields `LeanStmt::Abort`.
fn lower_action(
    lc: &LowerCtx,
    action: &RouteAction,
    carrier: Carrier,
    ir_stmt: Option<&IrStmt>,
) -> Vec<LeanStmt> {
    let fail = carrier.is_fail();
    let world = carrier.is_world();
    match action {
        RouteAction::Let { pattern, value } => {
            let ctx = lc.expr_ctx(carrier);
            super::expr::lower_fail_mode_let_stmts(pattern, value, &ctx, fail)
                .into_iter()
                .map(LeanStmt::line)
                .collect()
        }
        RouteAction::Return { values } => {
            if world {
                // Rendered into the body tail `(w, <payload>)` by
                // `world_threaded_payload`; nothing to emit inline.
                return vec![];
            }
            // UPSTREAM B-23: bare `return()` in a non-view fail route is an
            // early exit on the `State` carrier — not `return (s, ())`.
            if fail && values.is_empty() && lc.return_ty.is_none() {
                return vec![LeanStmt::line("return s".to_string())];
            }
            let ctx = lc.expr_ctx(carrier);
            if fail && values.len() == 1 {
                let rr = super::expr::gen_expr_as_route_result(&values[0], &ctx);
                if let Some(inner) = super::expr::strip_pure_wrap(&rr) {
                    return vec![LeanStmt::line(format!("return (s, {})", inner))];
                }
                return vec![
                    LeanStmt::line(format!("let __ret ← {rr}")),
                    LeanStmt::line("return (s, __ret)".to_string()),
                ];
            }
            let payload = super::core::route_local::render_return_values(
                values,
                &ctx,
                lc.route.return_type.as_ref(),
            );
            if fail {
                vec![LeanStmt::line(format!("return (s, {})", payload))]
            } else if lc.return_ty.is_some() {
                vec![LeanStmt::line(format!("/- return -/ ({})", payload))]
            } else {
                vec![LeanStmt::line(format!(
                    "/- return (no view): {} -/",
                    payload
                ))]
            }
        }
        RouteAction::Throw { error_code } => {
            if !fail {
                return vec![LeanStmt::line(format!(
                    "-- throw {} (skipped: route inferred non-failing)",
                    error_code,
                ))];
            }
            let mut v = Vec::new();
            if world {
                v.push(LeanStmt::line(format!(
                    "let w := Cambrian.Generated.World.with{} w inst s",
                    lc.entity.name,
                )));
            }
            v.push(LeanStmt::line(format!(
                "throw (Cambrian.ThrowCode.ofNat {})",
                error_code
            )));
            v
        }
        RouteAction::ThrowCustom { name, .. } => {
            if !fail {
                return vec![LeanStmt::line(format!(
                    "-- throw {} (skipped: route inferred non-failing)",
                    name,
                ))];
            }
            let mut v = Vec::new();
            if world {
                v.push(LeanStmt::line(format!(
                    "let w := Cambrian.Generated.World.with{} w inst s",
                    lc.entity.name,
                )));
            }
            // The custom error's identity is a stable numeric `ThrowCode`
            // derived from its name (args are not part of the abstract
            // revert identity).
            v.push(LeanStmt::line(format!(
                "throw (Cambrian.ThrowCode.ofNat {}) -- {}",
                custom_error_code(name),
                name,
            )));
            v
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            let (then_ir, else_ir) = match ir_stmt {
                Some(IrStmt::Conditional {
                    then_stmts,
                    else_stmts,
                    ..
                }) => (Some(then_stmts.as_slice()), Some(else_stmts.as_slice())),
                _ => (None, None),
            };
            let ctx = lc.expr_ctx(carrier);
            let cond = gen_expr(condition, &ctx);
            if world {
                // Thread `w` through each branch: commit the pending `s`,
                // lower each branch to a sub-block returning the updated
                // `w` (× escaping `var` captures), then re-read `s`.
                // UPSTREAM B-21: union captures for the outer bind, but fill
                // per-branch slots the arm does not bind with `default`.
                let mut then_caps = Vec::new();
                let mut else_caps = Vec::new();
                collect_var_call_names(then_actions, &mut then_caps);
                collect_var_call_names(else_actions, &mut else_caps);
                let mut captures = then_caps.clone();
                for c in &else_caps {
                    if !captures.iter().any(|x| x == c) {
                        captures.push(c.clone());
                    }
                }
                let setter = format!("Cambrian.Generated.World.with{}", lc.entity.name);
                let field = entity_field_name(&lc.entity.name);
                let bind = product_tail("w", &captures);
                vec![
                    LeanStmt::line(format!("let w := {} w inst s", setter)),
                    LeanStmt::line(if fail {
                        format!("let {} ← (if {} then", bind, cond)
                    } else {
                        format!("let {} := (if {} then", bind, cond)
                    }),
                    LeanStmt::Block(lower_world_branch(
                        lc,
                        then_actions,
                        carrier,
                        then_ir,
                        &captures,
                        &then_caps,
                    )),
                    LeanStmt::Block(vec![LeanStmt::line("else")]),
                    LeanStmt::Block(lower_world_branch(
                        lc,
                        else_actions,
                        carrier,
                        else_ir,
                        &captures,
                        &else_caps,
                    )),
                    LeanStmt::line(")"),
                    LeanStmt::line(format!("let s := w.storage.{} inst", field)),
                ]
            } else {
                // Nested state-only bodies drop the phase context (matching
                // the historical emitter, which passed `phase = None`).
                let lc2 = lc.with_phase(None);
                let (mut then_block, then_term) =
                    lower_actions_with_ir(&lc2, then_actions, carrier, then_ir);
                if then_actions.is_empty() || (!then_term && all_actions_are_skipped(then_actions))
                {
                    then_block.push(LeanStmt::line(if fail { "pure s" } else { "()" }));
                }
                let (mut else_block, else_term) =
                    lower_actions_with_ir(&lc2, else_actions, carrier, else_ir);
                if else_actions.is_empty() || (!else_term && all_actions_are_skipped(else_actions))
                {
                    else_block.push(LeanStmt::line(if fail { "pure s" } else { "()" }));
                }
                vec![
                    LeanStmt::line(format!("if {} then", cond)),
                    LeanStmt::Block(then_block),
                    LeanStmt::line("else"),
                    LeanStmt::Block(else_block),
                ]
            }
        }
        RouteAction::For {
            pattern,
            iter,
            body,
        } => {
            let body_ir = match ir_stmt {
                Some(IrStmt::For { body, .. }) => Some(body.as_slice()),
                _ => None,
            };
            if !world && !fail {
                // Non-failing + state-only: the body provably has no
                // observable effect (no `throw`, no world effect, no
                // escaping `let`). Intentional elision, not a silent drop.
                return vec![LeanStmt::line(
                    "-- elided: effect-free for-loop body (non-failing, no state/world effect)",
                )];
            }
            let ctx = lc.expr_ctx(carrier);
            let pat_str = super::expr::pattern_to_lean(pattern);
            let iter_s = gen_expr(iter, &ctx);
            // UPSTREAM B-19: range binders are Nat inside the loop body.
            let body_lc = if matches!(iter, Expr::Range(_, _)) {
                lc.with_extra_nat_idents(super::core::route_local::pattern_binders(pattern))
            } else {
                lc.clone()
            };
            if world {
                // Fold over `w`: monadic `foldlM` (`Except`) in fail mode so
                // a body `throw`/failing send short-circuits; pure `foldl` +
                // `Id.run do` otherwise. Ascribe accumulator + monad (B-13)
                // so the elaborator does not collapse `w` to `PUnit`.
                let field = entity_field_name(&lc.entity.name);
                let (header, closer) = if fail {
                    (
                        // `((do …) : Except T)` so ascription attaches to the
                        // do-block, not the whole `fun` (Lean `: ` is loose).
                        format!(
                            "let w ← ({}).foldlM (fun (w : Cambrian.Generated.World) {} => ((do",
                            iter_s, pat_str
                        ),
                        ") : Except Cambrian.ThrowCode Cambrian.Generated.World)) w".to_string(),
                    )
                } else {
                    (
                        format!(
                            "let w := ({}).foldl (fun (w : Cambrian.Generated.World) {} => Id.run ((do",
                            iter_s, pat_str
                        ),
                        ") : Id Cambrian.Generated.World)) w".to_string(),
                    )
                };
                let mut block = vec![LeanStmt::line(format!("let s := w.storage.{} inst", field))];
                block.extend(lower_actions_with_ir(&body_lc, body, carrier, body_ir).0);
                block.push(LeanStmt::line("pure w"));
                vec![
                    LeanStmt::line(header),
                    LeanStmt::Block(block),
                    LeanStmt::line(closer),
                    LeanStmt::line(format!("let s := w.storage.{} inst", field)),
                ]
            } else {
                // State-only fail: fold monadically over `s` so a body
                // `throw` propagates. Keep the enclosing phase (if any)
                // so `expr_ctx` stays world-free (C-1); clearing phase
                // used to flip `world_var` on and emit `w.block.*`.
                let mut block = lower_actions_with_ir(&body_lc, body, carrier, body_ir).0;
                block.push(LeanStmt::line("pure s"));
                let state_ty = format!("{}.State", lc.entity.name);
                vec![
                    LeanStmt::line(format!(
                        "let s ← ({}).foldlM (fun (s : {}) {} => ((do",
                        iter_s, state_ty, pat_str
                    )),
                    LeanStmt::Block(block),
                    LeanStmt::line(format!(") : Except Cambrian.ThrowCode {})) s", state_ty)),
                ]
            }
        }
        RouteAction::Effect {
            namespace, name, ..
        } => {
            if world {
                vec![LeanStmt::line(format!("-- {}", describe_skipped(action)))]
            } else {
                vec![LeanStmt::line(format!(
                    "-- not supported on Lean: {}::{} (no EVM/Lean model)",
                    namespace, name,
                ))]
            }
        }
        RouteAction::Rescue { tag, action: _ } => {
            // `rescue`/`recover` is Acki Nacki bounce (E26 on EVM/Lean).
            // Force-codegen keeps a comment; the inner send is not lowered.
            if world {
                vec![LeanStmt::line(format!("-- {}", describe_skipped(action)))]
            } else {
                vec![LeanStmt::line(format!(
                    "-- not supported on Lean: rescue '{}' (TVM async bounce recovery; E26)",
                    tag,
                ))]
            }
        }
        RouteAction::UpdateCode { .. } => {
            if world {
                vec![LeanStmt::line(format!("-- {}", describe_skipped(action)))]
            } else {
                vec![LeanStmt::line(
                    "-- not supported on Lean: gosh::updateCode (on-chain code upgrade)",
                )]
            }
        }
        RouteAction::Send { .. }
        | RouteAction::VarCall { .. }
        | RouteAction::Deploy { .. }
        | RouteAction::CallRoute { .. }
        | RouteAction::Emit { .. } => {
            if world {
                lower_world_effect(lc, action, fail, ir_stmt)
            } else {
                // These force the world-threaded lowering, so they cannot
                // reach the state-only emitter. Fail closed.
                vec![LeanStmt::Abort(format!(
                    "world-effecting action {} on the state-only emitter (route `{}`)",
                    describe_skipped(action),
                    lc.route.name,
                ))]
            }
        }
    }
}

pub(crate) fn emit_actions(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    route: &Route,
    actions: &[RouteAction],
    phase: Option<&PhaseBlock>,
    return_ty: Option<&str>,
    fail_mode: bool,
    indent: usize,
    profile: LeanProfile,
) -> bool {
    let lc = LowerCtx {
        program,
        entity,
        route,
        phase: phase.map(|p| p.name.as_str()),
        return_ty,
        profile,
        route_ir: None,
        nat_idents: HashSet::new(),
    };
    let (stmts, terminated) = lower_actions(&lc, actions, Carrier::State { fail: fail_mode });
    super::core::stmt::render_into(out, &stmts, indent);
    terminated
}

// ---------------------------------------------------------------------------
// Fail-mode predicates
// ---------------------------------------------------------------------------

/// True when this route's signature is wrapped in `Cambrian.RouteResult`
/// — i.e. it has at least one `where` clause (route- or per-phase), a
/// `throw` / `throw CustomErr` action somewhere in its body, **or** it
/// synchronously `call`s a same-entity route that is itself a fail
/// surface. Kernel fail-closure is the base; see `crate::analysis::route_facts`.
///
/// **evm×lean adapter overlay (PN-106):** an unrescued typed send/var-call to
/// an `extern entity` also makes the route a fail surface (Solidity high-level
/// ABI revert bubbles). That fact is *not* in kernel `local_fail_surface` —
/// other domains may treat extern sends as total. Same-entity `call` callees
/// are re-checked under this overlay so a wrapper of an extern-calling helper
/// is itself fail-mode.
///
/// Under `lean.numerics: overflow-panic`, routes whose member transforms
/// (or body exprs) use checked `+`/`-`/`*` are also fail surfaces.
pub(crate) fn route_fail_mode(program: &Program, entity: &Entity, route: &Route) -> bool {
    let mut visiting = std::collections::HashSet::new();
    route_fail_mode_rec(program, entity, route, &mut visiting)
}

fn route_fail_mode_rec(
    program: &Program,
    entity: &Entity,
    route: &Route,
    visiting: &mut std::collections::HashSet<String>,
) -> bool {
    if !visiting.insert(route.name.clone()) {
        return false;
    }
    if crate::analysis::route_can_fail_evm_lean(entity, route) {
        return true;
    }
    if crate::analysis::route_has_unrescued_extern_send(program, entity, route) {
        return true;
    }
    if crate::analysis::route_has_unrescued_failing_cross_send(program, entity, route) {
        return true;
    }
    if super::evm::dispatch::route_has_dynamic_dispatch(program, entity, route) {
        return true;
    }
    if super::use_overflow_panic() && route_has_checked_arith(entity, route) {
        return true;
    }
    if route_has_div0_or_narrow(program, entity, route) {
        return true;
    }
    if route_has_vec_oob_index(entity, route) {
        return true;
    }
    if crate::analysis::route_has_value_bearing_transfer(program, entity, route) {
        return true;
    }
    // Adapter overlay is not in kernel fail_closure — close over CallRoute
    // callees under the full predicate (covers `call pay` when only `pay`
    // has an unrescued extern send).
    let mut found = false;
    walk_call_callees_for_fail(program, entity, route, visiting, &mut found);
    found
}

fn walk_call_callees_for_fail(
    program: &Program,
    entity: &Entity,
    route: &Route,
    visiting: &mut std::collections::HashSet<String>,
    found: &mut bool,
) {
    fn walk(
        action: &RouteAction,
        program: &Program,
        entity: &Entity,
        visiting: &mut std::collections::HashSet<String>,
        found: &mut bool,
    ) {
        if *found {
            return;
        }
        match action {
            RouteAction::CallRoute { name, .. } => {
                if let Some(target) = entity.routes.iter().find(|r| r.name == *name) {
                    if route_fail_mode_rec(program, entity, target, visiting) {
                        *found = true;
                    }
                }
            }
            RouteAction::Conditional {
                then_actions,
                else_actions,
                ..
            } => {
                for a in then_actions {
                    walk(a, program, entity, visiting, found);
                }
                for a in else_actions {
                    walk(a, program, entity, visiting, found);
                }
            }
            RouteAction::For { body, .. } => {
                for a in body {
                    walk(a, program, entity, visiting, found);
                }
            }
            RouteAction::Rescue { .. } => {}
            _ => {}
        }
    }
    match &route.body {
        RouteBody::Unphased(actions) => {
            for a in actions {
                walk(a, program, entity, visiting, found);
            }
        }
        RouteBody::Phased(phases) => {
            for p in phases {
                for a in &p.actions {
                    walk(a, program, entity, visiting, found);
                }
            }
        }
        RouteBody::Mixed(phases, trailing) => {
            for p in phases {
                for a in &p.actions {
                    walk(a, program, entity, visiting, found);
                }
            }
            for a in trailing {
                walk(a, program, entity, visiting, found);
            }
        }
    }
}

fn route_has_vec_oob_index(entity: &Entity, route: &Route) -> bool {
    for action in route.body.all_actions() {
        match action {
            RouteAction::Let { value, .. } => {
                if super::expr::expr_has_vec_list_index(value, entity) {
                    return true;
                }
            }
            RouteAction::Return { values } => {
                if values
                    .iter()
                    .any(|v| super::expr::expr_has_vec_list_index(v, entity))
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn route_has_checked_arith(entity: &Entity, route: &Route) -> bool {
    for m in &entity.members {
        for tr in &m.transforms {
            if tr.route_name != route.name {
                continue;
            }
            if super::expr::expr_has_checked_binop(&tr.body) {
                return true;
            }
        }
    }
    for action in route.body.all_actions() {
        match action {
            RouteAction::Let { value, .. } => {
                if super::expr::expr_has_checked_binop(value) {
                    return true;
                }
            }
            RouteAction::Return { values } => {
                if values.iter().any(|v| super::expr::expr_has_checked_binop(v)) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn route_has_div0_or_narrow(program: &Program, entity: &Entity, route: &Route) -> bool {
    let pures = program.pure_fns.as_slice();
    for m in &entity.members {
        for tr in &m.transforms {
            if tr.route_name != route.name {
                continue;
            }
            if super::expr::expr_forces_fail_surface(&tr.body, pures) {
                return true;
            }
        }
    }
    for action in route.body.all_actions() {
        if action_has_div0_or_narrow(action, pures) {
            return true;
        }
    }
    false
}

fn action_has_div0_or_narrow(action: &RouteAction, pures: &[crate::ast::PureFn]) -> bool {
    fn expr(e: &Expr, pures: &[crate::ast::PureFn]) -> bool {
        super::expr::expr_forces_fail_surface(e, pures)
    }
    match action {
        RouteAction::Let { value, .. } => expr(value, pures),
        RouteAction::Return { values } => values.iter().any(|v| expr(v, pures)),
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            expr(condition, pures)
                || then_actions.iter().any(|a| action_has_div0_or_narrow(a, pures))
                || else_actions.iter().any(|a| action_has_div0_or_narrow(a, pures))
        }
        RouteAction::Send { args, dest, .. } | RouteAction::VarCall { args, dest, .. } => {
            args.iter().any(|a| expr(a, pures)) || expr(dest, pures)
        }
        RouteAction::For { body, .. } => body.iter().any(|a| action_has_div0_or_narrow(a, pures)),
        _ => false,
    }
}

/// True when this route is a *view*: it declares an explicit
/// `return_type` and therefore lowers to `… × T` (or `RouteResult (… ×
/// T)` if also failing). Spec emitters need this to decide whether
/// `expect return …` is meaningful (otherwise: `L6`).
pub(crate) fn route_is_view(route: &Route) -> bool {
    route.return_type.is_some()
}

/// Stable numeric `ThrowCode` for a *named* custom error (`error Foo(...)`
/// thrown via `throw Foo(args)` or `where … : throw Foo(args)`). The
/// abstract Lean model represents a revert by the identity of its error,
/// so we derive a deterministic, collision-resistant code from the error
/// name via the same FNV-1a id used for func-ids. Args are not part of
/// the revert identity and are dropped. The grammar parks `error_code: 0`
/// on named clauses, so this is the single source of truth — never emit
/// the `0` placeholder for a custom error.

fn phase_fail_mode(program: &Program, entity: &Entity, route: &Route, ph: &PhaseBlock) -> bool {
    if !ph.where_clauses.is_empty() {
        return true;
    }
    ph.actions
        .iter()
        .any(|a| phase_action_can_fail(program, entity, route, a))
}

fn phase_action_can_fail(
    program: &Program,
    entity: &Entity,
    route: &Route,
    action: &RouteAction,
) -> bool {
    match action {
        RouteAction::Throw { .. } | RouteAction::ThrowCustom { .. } => true,
        RouteAction::Deploy { .. } => true,
        RouteAction::Send { message: None, .. } => true,
        RouteAction::Send {
            message: Some(message),
            dest,
            ..
        }
        | RouteAction::VarCall { message, dest, .. } => {
            let empty = std::collections::HashMap::new();
            matches!(
                crate::analysis::classify_message_dest(
                    dest, message, entity, route, program, &empty
                ),
                crate::analysis::SendTarget::ExternEntity { .. }
            )
        }
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            then_actions
                .iter()
                .any(|a| phase_action_can_fail(program, entity, route, a))
                || else_actions
                    .iter()
                    .any(|a| phase_action_can_fail(program, entity, route, a))
        }
        RouteAction::For { body, .. } => body
            .iter()
            .any(|a| phase_action_can_fail(program, entity, route, a)),
        RouteAction::CallRoute { name, .. } => entity
            .routes
            .iter()
            .find(|r| r.name == *name)
            .map(|t| route_fail_mode(program, entity, t))
            .unwrap_or(false),
        RouteAction::Rescue { .. } => false,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// View-route return type
// ---------------------------------------------------------------------------

fn view_return_type(
    program: &Program,
    entity: &Entity,
    route: &Route,
    profile: LeanProfile,
) -> Option<String> {
    route.return_type.as_ref().map(|ty| {
        let ctx = LeanTypeCtx::for_entity(
            program,
            &entity.name,
            &entity.records,
            &entity.enums,
            &entity.type_aliases,
            profile,
        );
        lower_type(ty, &ctx)
    })
}

// ---------------------------------------------------------------------------
// Unit tests — textual SCC rewrite (PM-037)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ih_of() -> HashMap<&'static str, String> {
        HashMap::from([("ping", "ih.1".to_string()), ("pong", "ih.2".to_string())])
    }

    /// PM-037 / D4(a): a Lean string literal is DATA. A Cambrian `String`
    /// whose content spells `<Entity>.Routes.<r>` must survive verbatim, while
    /// a real call on the same line is still rewritten to its `ih` projection.
    /// This is what keeps the emitter aligned with the predictor's AST mirror
    /// `subst_scc_calls` (`Expr::StrLit(_) => e.clone()`).
    #[test]
    fn scc_subst_skips_string_literals() {
        let body = "let tag := \"Ping.Routes.pong\"\n\
                    Except.bind (Ping.Routes.pong w inst ctx n) (fun w => (w, tag))";
        let out = substitute_scc_calls(body, "Ping", &ih_of());
        assert!(
            out.contains("\"Ping.Routes.pong\""),
            "string literal must be copied verbatim, got:\n{out}"
        );
        assert!(
            out.contains("Except.bind (ih.2 w inst ctx n)"),
            "the real call must still become an `ih` projection, got:\n{out}"
        );
    }

    /// The E6 residual guard uses the same string-literal spans, so a body
    /// whose ONLY remaining `<E>.Routes.<r>` occurrence sits inside a literal
    /// must not panic.
    #[test]
    fn scc_subst_residual_guard_ignores_string_literals() {
        let body = "let tag := \"Ping.Routes.ping\"\n(w, tag)";
        assert_eq!(substitute_scc_calls(body, "Ping", &ih_of()), body);
    }

    /// Escaped quotes inside the literal do not close it: the `\"` in the
    /// middle keeps `Ping.Routes.ping` on the data side of the boundary.
    #[test]
    fn scc_subst_string_literal_honours_escapes() {
        let body = r#"let tag := "a\"Ping.Routes.ping\"b""#;
        assert_eq!(substitute_scc_calls(body, "Ping", &ih_of()), body);
    }

    /// A multi-byte scalar before the call keeps the byte arithmetic honest
    /// (the scanner walks chars, not bytes).
    #[test]
    fn scc_subst_is_utf8_safe() {
        let body = "-- «пинг»\nPing.Routes.ping w";
        assert_eq!(
            substitute_scc_calls(body, "Ping", &ih_of()),
            "-- «пинг»\nih.1 w"
        );
    }

    /// Identifier-exactness is unchanged by the literal-skipping: `_pre_0`
    /// guards and out-of-SCC routes still pass through.
    #[test]
    fn scc_subst_keeps_identifier_exact_matching() {
        let body = "Ping.Local.ping_pre_0 s ctx inst n; Ping.Routes.peek w; Ping.Routes.ping w";
        assert_eq!(
            substitute_scc_calls(body, "Ping", &ih_of()),
            "Ping.Local.ping_pre_0 s ctx inst n; Ping.Routes.peek w; ih.1 w"
        );
    }
}
