// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Resolved Cambrian types after alias inlining.

use std::collections::HashMap;

use crate::ast::Type;

/// Normalized type used throughout the IR layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedType {
    Simple(String),
    Generic {
        name: String,
        params: Vec<ResolvedType>,
    },
    Tuple(Vec<ResolvedType>),
    TypedAddress(String),
}

impl ResolvedType {
    pub fn simple(name: impl Into<String>) -> Self {
        ResolvedType::Simple(name.into())
    }

    pub fn from_ast(ty: &Type, alias_map: &HashMap<String, Type>) -> Self {
        resolve_type(ty, alias_map)
    }

    /// Inline type aliases once, producing a [`ResolvedType`].
    pub fn resolve_aliases(ty: &Type, alias_map: &HashMap<String, Type>) -> Self {
        resolve_type(ty, alias_map)
    }

    pub fn to_ast(&self) -> Type {
        match self {
            ResolvedType::Simple(name) => Type::Simple(name.clone()),
            ResolvedType::Generic { name, params } => {
                Type::Generic(name.clone(), params.iter().map(ResolvedType::to_ast).collect())
            }
            ResolvedType::Tuple(elems) => {
                Type::Tuple(elems.iter().map(ResolvedType::to_ast).collect())
            }
            ResolvedType::TypedAddress(entity) => Type::TypedAddress(entity.clone()),
        }
    }
}

/// Single-pass alias inlining from `ast::Type` to [`ResolvedType`].
pub fn resolve_type(ty: &Type, alias_map: &HashMap<String, Type>) -> ResolvedType {
    match ty {
        Type::Simple(name) => alias_map
            .get(name)
            .map(|resolved| resolve_type(resolved, alias_map))
            .unwrap_or_else(|| ResolvedType::Simple(name.clone())),
        Type::Generic(name, params) => ResolvedType::Generic {
            name: name.clone(),
            params: params
                .iter()
                .map(|p| resolve_type(p, alias_map))
                .collect(),
        },
        Type::Tuple(elems) => ResolvedType::Tuple(
            elems
                .iter()
                .map(|e| resolve_type(e, alias_map))
                .collect(),
        ),
        Type::TypedAddress(entity) => ResolvedType::TypedAddress(entity.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_type_inlines_aliases() {
        let aliases = HashMap::from([(
            "Counter".to_string(),
            Type::Simple("u64".to_string()),
        )]);
        let ty = resolve_type(&Type::Simple("Counter".to_string()), &aliases);
        assert_eq!(ty, ResolvedType::Simple("u64".to_string()));
    }

    #[test]
    fn resolve_type_chains_aliases() {
        let map = HashMap::from([
            ("A".to_string(), Type::Simple("B".to_string())),
            ("B".to_string(), Type::Simple("u32".to_string())),
        ]);
        assert_eq!(
            resolve_type(&Type::Simple("A".to_string()), &map),
            ResolvedType::Simple("u32".to_string())
        );
    }

    #[test]
    fn from_ast_matches_resolve_type() {
        let map = HashMap::from([("Amt".to_string(), Type::Simple("u64".to_string()))]);
        let ast = Type::Simple("Amt".to_string());
        assert_eq!(
            ResolvedType::from_ast(&ast, &map),
            resolve_type(&ast, &map)
        );
    }
}
