// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! `#[instantiates("STEM")]` link validation (CAM-H-02 / SPEC-Z).

use crate::ast::{InvariantDecl, Program, PropertyDecl, TestDecl, TestStep};
use crate::catalog_stem::{CatalogDeclRef, resolve_catalog_stem, StemResolveError};

use super::Diagnostic;

/// Pre-merge catalog link errors + noop-link warnings (run before `merge_catalog_instantiations`).
pub fn catalog_pre_merge_check(program: &Program) -> Vec<Diagnostic> {
    let mut diags = check_catalog_links(program);
    diags.extend(check_instantiates_goal_divergence(program));
    diags.extend(check_noop_instantiates(program));
    diags
}

/// T37: linked `#[instantiates]` must not replace catalog verification steps.
pub fn check_instantiates_goal_divergence(program: &Program) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    for test in &program.tests {
        if test.instantiates.is_some() {
            check_linked_test_goal(test, &mut diags);
        }
    }

    for prop in &program.properties {
        if let Some(stem) = &prop.instantiates {
            if property_delta_overrides_goal(program, prop, stem) {
                diags.push(t37_divergence(
                    &format!("property \"{}\"", prop.name),
                    "linked property may add assume/bound deltas only — not call/expect overrides",
                    prop.span,
                ));
            }
        }
    }

    diags
}

fn check_linked_test_goal(test: &TestDecl, diags: &mut Vec<Diagnostic>) {
    if test
        .body
        .iter()
        .any(|step| !is_linked_test_harness_pin(step))
    {
        diags.push(t37_divergence(
            &format!("test \"{}\"", test.name),
            "linked test may set harness pins (`with { ... }`, `msg { ... }` / `sys { ... }` in body) only — use catalog steps/expects or a standalone test",
            test.span,
        ));
    }
}

fn property_delta_overrides_goal(
    program: &Program,
    prop: &PropertyDecl,
    stem: &str,
) -> bool {
    match resolve_catalog_stem(program, &prop.entity_name, stem) {
        Ok(CatalogDeclRef::Property(_)) => prop
            .body
            .iter()
            .any(|step| !matches!(step, TestStep::Assume { .. } | TestStep::Bound { .. })),
        _ => false,
    }
}

/// Harness-only steps permitted in a linked `test` body (applied before catalog steps).
fn is_linked_test_harness_pin(step: &TestStep) -> bool {
    matches!(
        step,
        TestStep::SetContext { .. } | TestStep::SetRegistry { .. } | TestStep::Let { .. }
    )
}

fn t37_divergence(label: &str, detail: &str, span: crate::ast::Span) -> Diagnostic {
    Diagnostic::error(
        "T37",
        format!(
            "{}: #[instantiates] cannot change catalog verification goal — {}",
            label,
            detail
        ),
    )
    .with_span(span)
}

/// W12: linked supplemental block adds no delta (empty shell).
pub fn check_noop_instantiates(program: &Program) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    for prop in &program.properties {
        if prop.instantiates.is_none() {
            continue;
        }
        if property_link_is_noop(prop) {
            let stem = prop.instantiates.as_deref().unwrap_or("");
            diags.push(
                Diagnostic::warning(
                    "W12",
                    format!(
                        "property \"{}\": #[instantiates(\"{}\")] adds no delta (empty body; add assume/steps or init/context overrides)",
                        prop.name,
                        stem
                    ),
                )
                .with_span(prop.span),
            );
        }
    }

    for inv in &program.invariants {
        if inv.instantiates.is_none() || !inv.is_single_entity() {
            continue;
        }
        let stem = inv.instantiates.as_deref().unwrap_or("");
        if is_invariant_fanout_overlay(program, inv, stem) {
            continue;
        }
        if invariant_link_is_noop(inv) {
            diags.push(
                Diagnostic::warning(
                    "W12",
                    format!(
                        "invariant \"{}\": #[instantiates(\"{}\")] adds no delta (empty init/actions/checks)",
                        inv.name,
                        stem
                    ),
                )
                .with_span(inv.span),
            );
        }
    }

    diags
}

fn property_link_is_noop(prop: &PropertyDecl) -> bool {
    prop.body.is_empty()
        && prop.init_state.is_empty()
        && prop.forall_state.is_empty()
        && prop.context.is_empty()
        && prop.tag.is_none()
}

fn invariant_link_is_noop(inv: &InvariantDecl) -> bool {
    let inst = inv.instances.first();
    let init_empty = inst.map(|i| i.init.is_empty()).unwrap_or(true);
    let forall_empty = inst.map(|i| i.forall_state.is_empty()).unwrap_or(true);
    init_empty
        && forall_empty
        && inv.senders.is_empty()
        && inv.context.is_empty()
        && inv.actions.is_empty()
        && inv.checks.is_empty()
        && inv.track.is_empty()
        && inv.derived.is_empty()
        && inv.exclude_senders.is_empty()
        && inv.exclude_selectors.is_empty()
        && !inv.skip_from
        && !inv.fail_on_revert
        && !inv.with_time
        && inv.runs.is_none()
        && inv.depth.is_none()
        && inv.tag.is_none()
}

/// N:1 fan-out overlay renames the merged invariant — empty body is intentional.
fn is_invariant_fanout_overlay(program: &Program, inv: &InvariantDecl, stem: &str) -> bool {
    match resolve_catalog_stem(program, inv.entity_name(), stem) {
        Ok(CatalogDeclRef::Invariant(idx)) => program.invariants[idx].name != inv.name,
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkTargetKind {
    Property,
    Invariant,
}

/// Validate every `#[instantiates]` on supplemental `test` / `property` / `invariant`.
pub fn check_catalog_links(program: &Program) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    for test in &program.tests {
        if let Some(stem) = &test.instantiates {
            check_stem(
                program,
                &test.entity_name,
                stem,
                LinkTargetKind::Property,
                &format!("test \"{}\"", test.name),
                test.span,
                &mut diags,
            );
        }
    }

    for prop in &program.properties {
        if let Some(stem) = &prop.instantiates {
            check_property_link(program, prop, stem, &mut diags);
        }
    }

    for inv in &program.invariants {
        if let Some(stem) = &inv.instantiates {
            if !inv.is_single_entity() {
                diags.push(
                    Diagnostic::error(
                        "T32",
                        format!(
                            "invariant \"{}\": #[instantiates] is not supported on multi-entity invariants",
                            inv.name
                        ),
                    )
                    .with_span(inv.span),
                );
                continue;
            }
            check_stem(
                program,
                inv.entity_name(),
                stem,
                LinkTargetKind::Invariant,
                &format!("invariant \"{}\"", inv.name),
                inv.span,
                &mut diags,
            );
        }
    }

    diags
}

fn check_property_link(
    program: &Program,
    prop: &PropertyDecl,
    stem: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let label = format!("property \"{}\"", prop.name);
    match resolve_catalog_stem(program, &prop.entity_name, stem) {
        Ok(CatalogDeclRef::Property(idx)) => {
            let catalog = &program.properties[idx];
            if prop.params != catalog.params {
                diags.push(
                    Diagnostic::error(
                        "T28",
                        format!(
                            "{}: parameter list must match catalog property \"{}\"",
                            label, catalog.name
                        ),
                    )
                    .with_span(prop.span),
                );
            }
        }
        Ok(CatalogDeclRef::Invariant(_)) => {
            diags.push(kind_mismatch(&label, stem, "property", "invariant", prop.span));
        }
        Err(e) => push_stem_error(diags, &label, stem, e, prop.span),
    }
}

fn check_stem(
    program: &Program,
    entity: &str,
    stem: &str,
    expected: LinkTargetKind,
    label: &str,
    span: crate::ast::Span,
    diags: &mut Vec<Diagnostic>,
) {
    match resolve_catalog_stem(program, entity, stem) {
        Ok(CatalogDeclRef::Property(_)) if expected == LinkTargetKind::Invariant => {
            diags.push(kind_mismatch(label, stem, "invariant", "property", span));
        }
        Ok(CatalogDeclRef::Invariant(_)) if expected == LinkTargetKind::Property => {
            diags.push(kind_mismatch(label, stem, "property", "invariant", span));
        }
        Ok(_) => {}
        Err(e) => push_stem_error(diags, label, stem, e, span),
    }
}

fn kind_mismatch(
    label: &str,
    stem: &str,
    want: &str,
    got: &str,
    span: crate::ast::Span,
) -> Diagnostic {
    Diagnostic::error(
        "T33",
        format!(
            "{}: #[instantiates(\"{}\")] must reference a catalog {}, not a {}",
            label,
            stem,
            want,
            got
        ),
    )
    .with_span(span)
}

fn push_stem_error(
    diags: &mut Vec<Diagnostic>,
    label: &str,
    stem: &str,
    err: StemResolveError,
    span: crate::ast::Span,
) {
    let (code, msg) = match err {
        StemResolveError::Empty => (
            "T25",
            format!("{}: #[instantiates] requires a non-empty catalog stem", label),
        ),
        StemResolveError::NotFound => (
            "T29",
            format!(
                "{}: catalog stem \"{}\" not found for this entity",
                label, stem
            ),
        ),
        StemResolveError::Ambiguous | StemResolveError::AmbiguousCrossKind => (
            "T26",
            format!(
                "{}: catalog stem \"{}\" is ambiguous (multiple declarations match; if this is a supplemental overlay, add #[instantiates(\"{}\")])",
                label,
                stem,
                stem
            ),
        ),
    };
    diags.push(Diagnostic::error(code, msg).with_span(span));
}
