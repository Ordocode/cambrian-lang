// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Shared type-shape helpers for INV-TYPED forge + revm predicate lowerers.

use crate::ast::{Entity, Expr, Type};

pub(crate) fn route_return_type(expr: &Expr, entity: &Entity) -> Option<Type> {
    if let Expr::FnCall(name, _) = expr {
        return entity
            .routes
            .iter()
            .find(|r| r.name == *name)
            .and_then(|r| r.return_type.clone());
    }
    None
}

pub(crate) fn member_field_type_single(expr: &Expr, entity: &Entity) -> Option<Type> {
    if let Expr::Ident(name) = expr {
        return entity
            .members
            .iter()
            .find(|m| m.name == *name)
            .map(|m| m.ty.clone());
    }
    None
}

pub(crate) fn member_field_type_multi(
    expr: &Expr,
    entity_for_inst: &[(String, &Entity)],
) -> Option<Type> {
    if let Expr::FieldAccess(inner, member) = expr {
        if let Expr::Ident(inst) = inner.as_ref() {
            if let Some((_, entity)) = entity_for_inst.iter().find(|(n, _)| n == inst) {
                if let Some(m) = entity.members.iter().find(|m| m.name == *member) {
                    return Some(m.ty.clone());
                }
            }
        }
    }
    None
}

pub(crate) fn hashmap_key_type(member_ty: &Type) -> Option<Type> {
    if let Type::Generic(name, args) = member_ty {
        if name == "HashMap" {
            return args.first().cloned();
        }
    }
    None
}

pub(crate) fn is_bool_type(ty: &Type) -> bool {
    matches!(ty, Type::Simple(s) if s == "bool")
}
