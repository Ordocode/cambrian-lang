// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! CAM-H-02 — catalog stem resolution unit tests.

use cambrian_transpiler::ast::{Program, PropertyDecl, Span};
use cambrian_transpiler::catalog_stem::{
    resolve_catalog_stem, stem_matches, CatalogDeclRef, StemResolveError,
};

fn empty_program() -> Program {
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

fn prop(name: &str, entity: &str) -> PropertyDecl {
    PropertyDecl {
        name: name.to_string(),
        entity_name: entity.to_string(),
        params: vec![],
        init_state: vec![],
        forall_state: Default::default(),
        context: Default::default(),
        body: vec![],
        instances: vec![],
        tag: None,
        instantiates: None,
        span: Span::new(0, 0),
    }
}

#[test]
fn stem_matches_prefix_rules() {
    assert!(stem_matches("PROP-RHT-003 TransferConservation", "PROP-RHT-003"));
    assert!(stem_matches("PROP-RHT-003", "PROP-RHT-003"));
    assert!(!stem_matches("PROP-RHT-010a Partial", "PROP-RHT-010"));
    assert!(!stem_matches("X-PROP-RHT-003", "PROP-RHT-003"));
}

#[test]
fn resolve_property_by_stem() {
    let mut p = empty_program();
    p.properties.push(prop("PROP-RHT-003 TransferConservation", "RehearsalToken"));
    let got = resolve_catalog_stem(&p, "RehearsalToken", "PROP-RHT-003").unwrap();
    assert_eq!(got, CatalogDeclRef::Property(0));
}

#[test]
fn resolve_not_found() {
    let p = empty_program();
    assert_eq!(
        resolve_catalog_stem(&p, "E", "MISSING"),
        Err(StemResolveError::NotFound)
    );
}

#[test]
fn resolve_ambiguous_two_properties() {
    let mut p = empty_program();
    p.properties.push(prop("PROP-RHT-003 A", "E"));
    p.properties.push(prop("PROP-RHT-003 B", "E"));
    assert_eq!(
        resolve_catalog_stem(&p, "E", "PROP-RHT-003"),
        Err(StemResolveError::Ambiguous)
    );
}
