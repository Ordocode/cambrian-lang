// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Merge supplemental `#[instantiates]` links into catalog declarations (CAM-H-02).

use std::collections::HashSet;

use crate::ast::{
    ForallSpec, InvariantDecl, InvariantEmitPolicy, Program, PropertyDecl, PropertyInstance,
    PropertyInstanceKind, TestDecl,
};
use crate::catalog_stem::{resolve_catalog_stem, CatalogDeclRef};

/// Fold linked supplemental decls into catalog `property` / `invariant` overlays.
/// No-op when no `#[instantiates]` attributes are present.
pub fn merge_catalog_instantiations(program: &mut Program) {
    if !has_catalog_links(program) {
        return;
    }

    let linked_tests: Vec<TestDecl> = program
        .tests
        .iter()
        .filter(|t| t.instantiates.is_some())
        .cloned()
        .collect();
    program.tests.retain(|t| t.instantiates.is_none());

    for test in linked_tests {
        attach_test_link(program, test);
    }

    let linked_props: Vec<PropertyDecl> = program
        .properties
        .iter()
        .filter(|p| p.instantiates.is_some())
        .cloned()
        .collect();
    program.properties.retain(|p| p.instantiates.is_none());

    for prop in linked_props {
        attach_property_link(program, prop);
    }

    let linked_invs: Vec<InvariantDecl> = program
        .invariants
        .iter()
        .filter(|i| i.instantiates.is_some())
        .cloned()
        .collect();
    program.invariants.retain(|i| i.instantiates.is_none());

    let mut superseded_catalog_idxs: HashSet<usize> = HashSet::new();
    for inv in linked_invs {
        let stem = inv
            .instantiates
            .clone()
            .expect("filtered linked invariant");
        let catalog_idx = invariant_index(program, inv.entity_name(), &stem);
        superseded_catalog_idxs.insert(catalog_idx);
        let merged = merge_invariant_overlay(&program.invariants[catalog_idx], &inv);
        program.invariants.push(merged);
    }

    for idx in superseded_catalog_idxs {
        program.invariants[idx].emit_policy = InvariantEmitPolicy::Superseded;
    }
}

fn has_catalog_links(program: &Program) -> bool {
    program.tests.iter().any(|t| t.instantiates.is_some())
        || program
            .properties
            .iter()
            .any(|p| p.instantiates.is_some())
        || program
            .invariants
            .iter()
            .any(|i| i.instantiates.is_some())
}

fn attach_test_link(program: &mut Program, test: TestDecl) {
    let stem = test
        .instantiates
        .clone()
        .expect("filtered linked test");
    let idx = property_index(program, &test.entity_name, &stem);
    program.properties[idx].instances.push(PropertyInstance {
        kind: PropertyInstanceKind::Test,
        name: Some(test.name),
        bindings: vec![],
        init_state: test.init_state,
        forall_state: ForallSpec::default(),
        context: Default::default(),
        skip_from: test.skip_from,
        runs: None,
        tag: test.tag,
        body_delta: test.body,
        replace_catalog_body: false,
        span: test.span,
    });
}

fn attach_property_link(program: &mut Program, prop: PropertyDecl) {
    let stem = prop
        .instantiates
        .clone()
        .expect("filtered linked property");
    let idx = property_index(program, &prop.entity_name, &stem);
    let kind = if prop.params.is_empty() {
        PropertyInstanceKind::Test
    } else {
        PropertyInstanceKind::Fuzz
    };
    program.properties[idx].instances.push(PropertyInstance {
        kind,
        name: Some(prop.name),
        bindings: vec![],
        init_state: prop.init_state,
        forall_state: prop.forall_state,
        context: prop.context,
        skip_from: false,
        runs: None,
        tag: prop.tag,
        body_delta: prop.body,
        replace_catalog_body: false,
        span: prop.span,
    });
}

fn merge_invariant_overlay(catalog: &InvariantDecl, supplemental: &InvariantDecl) -> InvariantDecl {
    let mut merged = catalog.clone();
    merged.name = supplemental.name.clone();
    merged.instantiates = None;
    merged.emit_policy = InvariantEmitPolicy::Emit;
    merged.span = supplemental.span;

    if supplemental.skip_from {
        merged.skip_from = true;
    }
    if supplemental.fail_on_revert {
        merged.fail_on_revert = true;
    }
    if supplemental.with_time {
        merged.with_time = true;
    }
    if supplemental.runs.is_some() {
        merged.runs = supplemental.runs;
    }
    if supplemental.depth.is_some() {
        merged.depth = supplemental.depth;
    }
    if supplemental.tag.is_some() {
        merged.tag = supplemental.tag.clone();
    }
    if !supplemental.context.entries.is_empty() {
        merged.context = supplemental.context.clone();
    }
    if !supplemental.deploy.is_empty() {
        merged.deploy = supplemental.deploy.clone();
    }
    if !supplemental.senders.is_empty() {
        merged.senders = supplemental.senders.clone();
    }
    if !supplemental.track.is_empty() {
        merged.track = supplemental.track.clone();
    }
    if !supplemental.derived.is_empty() {
        merged.derived = supplemental.derived.clone();
    }
    if !supplemental.exclude_senders.is_empty() {
        merged.exclude_senders = supplemental.exclude_senders.clone();
    }
    if !supplemental.exclude_selectors.is_empty() {
        merged.exclude_selectors = supplemental.exclude_selectors.clone();
    }

    let sup_inst = &supplemental.instances[0];
    if sup_inst.init_specified {
        merged.instances[0].init = sup_inst.init.clone();
        merged.instances[0].forall_state = sup_inst.forall_state.clone();
        merged.instances[0].init_specified = true;
    } else if !sup_inst.forall_state.is_empty() {
        merged.instances[0].forall_state = sup_inst.forall_state.clone();
    }

    merged.actions.extend(supplemental.actions.clone());
    if supplemental.checks.is_empty() {
        // Catalog checks stand when the supplemental shell is init/actions only.
    } else {
        // Supplemental checks replace the catalog predicate when the linked
        // shell carries an EVM-lowering-safe equivalent (CAM-H-02 delta).
        merged.checks = supplemental.checks.clone();
    }

    merged
}

fn property_index(program: &Program, entity: &str, stem: &str) -> usize {
    match resolve_catalog_stem(program, entity, stem) {
        Ok(CatalogDeclRef::Property(i)) => i,
        Ok(CatalogDeclRef::Invariant(_)) => {
            panic!("property link resolved to invariant stem {stem:?}");
        }
        Err(e) => panic!("merge_catalog_instantiations: unresolved stem {stem:?}: {e:?}"),
    }
}

fn invariant_index(program: &Program, entity: &str, stem: &str) -> usize {
    match resolve_catalog_stem(program, entity, stem) {
        Ok(CatalogDeclRef::Invariant(i)) => i,
        Ok(CatalogDeclRef::Property(_)) => {
            panic!("invariant link resolved to property stem {stem:?}");
        }
        Err(e) => panic!("merge_catalog_instantiations: unresolved stem {stem:?}: {e:?}"),
    }
}
