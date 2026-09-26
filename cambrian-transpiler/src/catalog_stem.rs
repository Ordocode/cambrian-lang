// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Resolve catalog declaration marker stems for `#[instantiates("…")]`.

use crate::ast::Program;

/// Which top-level catalog declaration a stem resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogDeclRef {
    Property(usize),
    Invariant(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StemResolveError {
    Empty,
    NotFound,
    Ambiguous,
    AmbiguousCrossKind,
}

/// True when `decl_name` is `stem` or `stem` followed by space / `_` / end.
pub fn stem_matches(decl_name: &str, stem: &str) -> bool {
    if decl_name == stem {
        return true;
    }
    if decl_name.len() <= stem.len() {
        return false;
    }
    if !decl_name.starts_with(stem) {
        return false;
    }
    matches!(
        decl_name.as_bytes().get(stem.len()),
        Some(b' ') | Some(b'_')
    )
}

/// Resolve `stem` against catalog `property` / `invariant` declarations for `entity`.
pub fn resolve_catalog_stem(
    program: &Program,
    entity: &str,
    stem: &str,
) -> Result<CatalogDeclRef, StemResolveError> {
    let stem = stem.trim();
    if stem.is_empty() {
        return Err(StemResolveError::Empty);
    }

    let props: Vec<usize> = program
        .properties
        .iter()
        .enumerate()
        .filter(|(_, p)| p.entity_name == entity && stem_matches(&p.name, stem))
        .map(|(i, _)| i)
        .collect();

    let invs: Vec<usize> = program
        .invariants
        .iter()
        .enumerate()
        .filter(|(_, inv)| {
            inv.is_single_entity()
                && inv.entity_name() == entity
                && stem_matches(&inv.name, stem)
        })
        .map(|(i, _)| i)
        .collect();

    if !props.is_empty() && !invs.is_empty() {
        return Err(StemResolveError::AmbiguousCrossKind);
    }

    let mut matches: Vec<CatalogDeclRef> = props
        .into_iter()
        .map(CatalogDeclRef::Property)
        .collect();
    matches.extend(invs.into_iter().map(CatalogDeclRef::Invariant));

    match matches.len() {
        0 => Err(StemResolveError::NotFound),
        1 => Ok(matches[0]),
        _ => Err(StemResolveError::Ambiguous),
    }
}
