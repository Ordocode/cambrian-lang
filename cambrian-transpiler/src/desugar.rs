// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Property desugaring pass.
//!
//! Lowers each abstract [`PropertyDecl`] into the concrete
//! [`TestDecl`] / [`FuzzDecl`] shapes that the Rust / EVM / revm /
//! cargo-fuzz backends already understand:
//!
//!   * a `fuzz` instance becomes a `FuzzDecl` whose body prepends a
//!     `bound <p> in lo..hi` step for every ranged binding, followed by
//!     the property's logical body (so `assume` / `call` / `expect*`
//!     carry over unchanged);
//!   * a `test` instance becomes a `TestDecl` whose body prepends a
//!     `let <p> = value` for every concrete binding, followed by the
//!     property's logical body with `assume` / `bound` steps stripped
//!     (preconditions are presumed satisfied by the chosen values, and
//!     neither is legal in a plain test);
//!   * a property with **no** instances auto-derives a single instance —
//!     a full-range `FuzzDecl` when it has parameters, or a deterministic
//!     `TestDecl` when it has none — so every property still runs on the
//!     Rust host without boilerplate.
//!
//! Sampling bounds therefore live *only* in the derived `FuzzDecl`s; the
//! Lean backend reads [`PropertyDecl`] directly and never sees them.
//!
//! This pass is intentionally **not** run for the Lean target: Lean
//! consumes `program.properties` (and the hand-written `program.tests`)
//! directly, so leaving those collections untouched keeps the generated
//! theorems free of derived artefacts.

use std::collections::HashMap;

use crate::ast::*;

/// Lower every `PropertyDecl` in `program` into derived
/// `TestDecl` / `FuzzDecl` entries appended to `program.tests` /
/// `program.fuzz_tests`. The `program.properties` collection is left in
/// place (untouched) in case later passes want to inspect it.
pub fn desugar_properties(program: &mut Program) {
    let props = program.properties.clone();
    // Snapshot each entity's members so forall-ized state fields can recover
    // their declared types (for synthesized fuzz parameters) and `all_state`
    // can expand to the full member list.
    let members_by_entity: HashMap<String, Vec<Member>> = program
        .entities
        .iter()
        .map(|e| (e.name.clone(), e.members.clone()))
        .collect();
    for prop in &props {
        let members = members_by_entity
            .get(&prop.entity_name)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        if prop.instances.is_empty() {
            // A context *forall* (`ctx { msg::sender: * }`) needs sampling, so
            // it forces a fuzz even when there are no params / state foralls.
            let has_ctx_forall = prop.context.foralls().next().is_some();
            if prop.params.is_empty() && prop.forall_state.is_empty() && !has_ctx_forall {
                program.tests.push(prop_to_test(prop, None));
            } else {
                program.fuzz_tests.push(prop_to_fuzz(prop, None, members));
            }
            continue;
        }
        for (idx, inst) in prop.instances.iter().enumerate() {
            match inst.kind {
                PropertyInstanceKind::Test => {
                    program.tests.push(prop_to_test(prop, Some((idx, inst))));
                }
                PropertyInstanceKind::Fuzz => {
                    program.fuzz_tests.push(prop_to_fuzz(prop, Some((idx, inst)), members));
                }
            }
        }
    }
}

/// Merge the property-level and instance-level forall specs (union of
/// targets; `all_state` if either sets it).
fn resolve_forall(prop: &PropertyDecl, inst: Option<(usize, &PropertyInstance)>) -> ForallSpec {
    let mut spec = prop.forall_state.clone();
    if let Some((_, i)) = inst {
        spec.all_state |= i.forall_state.all_state;
        for t in &i.forall_state.targets {
            if !spec.targets.contains(t) {
                spec.targets.push(t.clone());
            }
        }
    }
    spec
}

/// Merge the property-level `ctx { ... }` with an optional per-instance
/// override (instance entries win per `(namespace, field)`; new entries
/// append).
fn resolve_context(prop: &PropertyDecl, inst: Option<(usize, &PropertyInstance)>) -> ContextSpec {
    let mut entries = prop.context.entries.clone();
    if let Some((_, i)) = inst {
        for e in &i.context.entries {
            if let Some(slot) = entries
                .iter_mut()
                .find(|x| x.namespace == e.namespace && x.field == e.field)
            {
                *slot = e.clone();
            } else {
                entries.push(e.clone());
            }
        }
    }
    ContextSpec { entries }
}

/// The synthesized fuzz type used to sample a forall-ized blockchain
/// context parameter on the dynamic backends.
fn ctx_param_type(namespace: &str, field: &str) -> Type {
    let name = match (namespace, field) {
        ("msg", "sender") => "address",
        ("msg", "value") => "U256",
        ("sys", "now") | ("sys", "timestamp") | ("sys", "block_number") | ("sys", "blockNumber") => {
            "u64"
        }
        ("sys", "balance") => "U256",
        ("sys", "chainid") | ("sys", "chain_id") => "U256",
        _ => "U256",
    };
    Type::Simple(name.to_string())
}

/// Allocate a parameter name that does not collide with anything already in
/// `used`, preferring `desired` and falling back to `__init_<desired>`.
fn fresh_name(desired: &str, used: &mut Vec<String>) -> String {
    let mut candidate = desired.to_string();
    if used.iter().any(|n| n == &candidate) {
        candidate = format!("__init_{}", desired);
        let mut n = 0;
        while used.iter().any(|x| x == &candidate) {
            n += 1;
            candidate = format!("__init_{}_{}", desired, n);
        }
    }
    used.push(candidate.clone());
    candidate
}

/// Compose the display name for a derived decl: the property name, plus
/// the instance name (or a positional suffix) when this came from a
/// nested instance.
fn derived_name(prop: &PropertyDecl, inst: Option<(usize, &PropertyInstance)>) -> String {
    match inst {
        Some((idx, i)) => match &i.name {
            Some(n) => format!("{} - {}", prop.name, n),
            None => format!("{} #{}", prop.name, idx + 1),
        },
        None => prop.name.clone(),
    }
}

/// Merge the property-level default init state with an optional
/// per-instance override (instance fields win; new fields append).
fn merged_init(
    prop: &PropertyDecl,
    inst: Option<(usize, &PropertyInstance)>,
) -> Vec<(String, Expr)> {
    let mut out = prop.init_state.clone();
    if let Some((_, i)) = inst {
        for (name, value) in &i.init_state {
            if let Some(slot) = out.iter_mut().find(|(n, _)| n == name) {
                slot.1 = value.clone();
            } else {
                out.push((name.clone(), value.clone()));
            }
        }
    }
    out
}

fn prop_to_fuzz(
    prop: &PropertyDecl,
    inst: Option<(usize, &PropertyInstance)>,
    members: &[Member],
) -> FuzzDecl {
    let mut body: Vec<TestStep> = Vec::new();
    if let Some((_, i)) = inst {
        for (name, arg) in &i.bindings {
            match arg {
                InstanceArg::Range { lo, hi, inclusive } => body.push(TestStep::Bound {
                    var: name.clone(),
                    lo: lo.clone(),
                    hi: hi.clone(),
                    inclusive: *inclusive,
                }),
                // A concrete binding inside a fuzz instance is rejected by
                // validation; if one slips through, pin it with an `assume`.
                InstanceArg::Concrete(v) => body.push(TestStep::Assume {
                    cond: Expr::BinOp(
                        Box::new(Expr::Ident(name.clone())),
                        BinOp::Eq,
                        Box::new(v.clone()),
                    ),
                }),
            }
        }
    }

    let mut params = prop.params.clone();
    let mut init = merged_init(prop, inst);
    // Names already in play: existing params + init pins, so synthesized
    // forall params don't collide.
    let mut used: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
    for (k, _) in &init {
        if !used.contains(k) {
            used.push(k.clone());
        }
    }

    // Lower the forall spec: sample each forall-ized target as a fuzz
    // parameter, pinning state fields in the initial state and pushing a
    // context step for blockchain params.
    let spec = resolve_forall(prop, inst);
    let pinned: Vec<String> = init.iter().map(|(k, _)| k.clone()).collect();
    let mut forall_fields: Vec<(String, Type)> = Vec::new();
    if spec.all_state {
        for m in members {
            if !pinned.contains(&m.name) {
                forall_fields.push((m.name.clone(), m.ty.clone()));
            }
        }
    }
    for t in &spec.targets {
        if let ForallTarget::StateField(f) = t {
            if pinned.contains(f) || forall_fields.iter().any(|(n, _)| n == f) {
                continue;
            }
            let ty = members
                .iter()
                .find(|m| &m.name == f)
                .map(|m| m.ty.clone())
                .unwrap_or_else(|| Type::Simple("U256".to_string()));
            forall_fields.push((f.clone(), ty));
        }
    }
    for (field, ty) in forall_fields {
        let pname = fresh_name(&field, &mut used);
        params.push(Param { name: pname.clone(), ty });
        init.push((field, Expr::Ident(pname)));
    }

    // Context block (`ctx { ... }`): concrete pins become a fixed `msg`/`sys`
    // context step; forall (`*`) params are sampled and applied via a context
    // step too.
    let ctx = resolve_context(prop, inst);
    let mut ctx_steps: Vec<TestStep> = Vec::new();
    for e in &ctx.entries {
        match &e.value {
            Some(v) => ctx_steps.push(TestStep::SetContext {
                namespace: e.namespace.clone(),
                fields: vec![(e.field.clone(), v.clone())],
            }),
            None => {
                let pname = fresh_name(&format!("{}_{}", e.namespace, e.field), &mut used);
                params.push(Param {
                    name: pname.clone(),
                    ty: ctx_param_type(&e.namespace, &e.field),
                });
                ctx_steps.push(TestStep::SetContext {
                    namespace: e.namespace.clone(),
                    fields: vec![(e.field.clone(), Expr::Ident(pname))],
                });
            }
        }
    }

    // Context steps run before the bindings/body so the sampled values are in
    // scope for `call` / `expect`.
    let mut full_body = ctx_steps;
    full_body.append(&mut body);
    append_property_body(&mut full_body, prop, inst.map(|(_, i)| i));

    FuzzDecl {
        name: derived_name(prop, inst),
        entity_name: prop.entity_name.clone(),
        params,
        init_state: init,
        skip_from: inst.map(|(_, i)| i.skip_from).unwrap_or(false),
        body: full_body,
        runs: inst.and_then(|(_, i)| i.runs),
        tag: inst.and_then(|(_, i)| i.tag.clone()).or_else(|| prop.tag.clone()),
        instantiates: None,
        span: prop.span,
    }
}

fn is_assume_or_bound(step: &TestStep) -> bool {
    matches!(step, TestStep::Assume { .. } | TestStep::Bound { .. })
}

/// Harness pins from a linked `#[instantiates]` test (`msg`/`sys` context, `let` bindings).
fn is_linked_test_harness_pin(step: &TestStep) -> bool {
    matches!(
        step,
        TestStep::SetContext { .. } | TestStep::SetRegistry { .. } | TestStep::Let { .. }
    )
}

/// Append catalog + optional instance delta to a fuzz/test body vector.
fn append_property_body(full_body: &mut Vec<TestStep>, prop: &PropertyDecl, inst: Option<&PropertyInstance>) {
    if inst.map(|i| i.replace_catalog_body).unwrap_or(false) {
        if let Some(i) = inst {
            full_body.extend(i.body_delta.iter().cloned());
        }
        return;
    }

    if let Some(i) = inst {
        for step in &i.body_delta {
            if is_assume_or_bound(step) {
                full_body.push(step.clone());
            }
        }
    }
    full_body.extend(prop.body.iter().cloned());
    if let Some(i) = inst {
        for step in &i.body_delta {
            if !is_assume_or_bound(step) {
                full_body.push(step.clone());
            }
        }
    }
}

fn prop_to_test(prop: &PropertyDecl, inst: Option<(usize, &PropertyInstance)>) -> TestDecl {
    let mut body: Vec<TestStep> = Vec::new();
    // Concrete context pins apply to a test; forall (`*`) context cannot be
    // quantified in a concrete test, so it is dropped (falls back to default).
    let ctx = resolve_context(prop, inst);
    for e in &ctx.entries {
        if let Some(v) = &e.value {
            body.push(TestStep::SetContext {
                namespace: e.namespace.clone(),
                fields: vec![(e.field.clone(), v.clone())],
            });
        }
    }
    if let Some((_, i)) = inst {
        for (name, arg) in &i.bindings {
            if let InstanceArg::Concrete(v) = arg {
                body.push(TestStep::Let {
                    name: name.clone(),
                    ty: None,
                    value: v.clone(),
                });
            }
        }
    }

    if inst.map(|(_, i)| i.replace_catalog_body).unwrap_or(false) {
        if let Some((_, i)) = inst {
            for step in &i.body_delta {
                if !is_assume_or_bound(step) {
                    body.push(step.clone());
                }
            }
        }
    } else {
        if let Some((_, i)) = inst {
            for step in &i.body_delta {
                if is_linked_test_harness_pin(step) {
                    body.push(step.clone());
                }
            }
        }
        for step in &prop.body {
            if !is_assume_or_bound(step) {
                body.push(step.clone());
            }
        }
        if let Some((_, i)) = inst {
            for step in &i.body_delta {
                if !is_assume_or_bound(step) && !is_linked_test_harness_pin(step) {
                    body.push(step.clone());
                }
            }
        }
    }

    TestDecl {
        name: derived_name(prop, inst),
        entity_name: prop.entity_name.clone(),
        init_state: merged_init(prop, inst),
        skip_from: inst.map(|(_, i)| i.skip_from).unwrap_or(false),
        body,
        tag: inst.and_then(|(_, i)| i.tag.clone()).or_else(|| prop.tag.clone()),
        instantiates: None,
        span: prop.span,
    }
}
