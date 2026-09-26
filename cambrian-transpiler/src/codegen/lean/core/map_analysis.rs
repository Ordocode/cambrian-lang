// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean P4c — detect HashMap members that need a parallel `_keys` ghost field.

use crate::ast::{Entity, Expr, Member, RouteAction};

fn action_walks_any(action: &RouteAction, walk: &dyn Fn(&Expr) -> bool) -> bool {
    crate::analysis::action_walks_any(action, walk)
}

fn entity_walks_any<F: Fn(&Expr) -> bool>(entity: &Entity, walk: F) -> bool {
    for route in &entity.routes {
        for action in route.body.all_actions() {
            if action_walks_any(action, &walk) {
                return true;
            }
        }
        for wc in &route.where_clauses {
            if walk(&wc.condition) {
                return true;
            }
        }
        for fc in &route.from_clauses {
            for arg in &fc.args {
                if walk(arg) {
                    return true;
                }
            }
        }
    }
    for member in &entity.members {
        if let Some(default) = &member.default_value {
            if walk(default) {
                return true;
            }
        }
        for transform in &member.transforms {
            if walk(&transform.body) {
                return true;
            }
        }
    }
    false
}

/// True when the entity iterates `member_name` via `.keys()` / `.values()` /
/// `.iter()`, `for pat in m`, or iterator chains rooted at the member.
pub fn member_needs_keys_list(entity: &Entity, member_name: &str) -> bool {
    fn is_iter_method(name: &str) -> bool {
        matches!(name, "keys" | "values" | "iter")
    }

    fn is_chain_method(name: &str) -> bool {
        matches!(
            name,
            "fold" | "collect" | "filter" | "map" | "take" | "enumerate" | "len"
        )
    }

    fn walk_expr(expr: &Expr, member_name: &str) -> bool {
        match expr {
            Expr::MethodCall(base, method, args) => {
                if (method == "is_empty" || method == "len") && args.is_empty() {
                    if let Expr::Ident(n) = base.as_ref() {
                        if n == member_name {
                            return true;
                        }
                    }
                }
                if (is_iter_method(method) && args.is_empty()) || is_chain_method(method) {
                    if let Expr::Ident(n) = base.as_ref() {
                        if n == member_name {
                            return true;
                        }
                    }
                }
                walk_expr(base, member_name) || args.iter().any(|a| walk_expr(a, member_name))
            }
            Expr::For(_, it, body) => {
                matches!(it.as_ref(), Expr::Ident(n) if n == member_name)
                    || walk_expr(it, member_name)
                    || walk_expr(body, member_name)
            }
            Expr::BinOp(l, _, r) | Expr::Index(l, r) => {
                walk_expr(l, member_name) || walk_expr(r, member_name)
            }
            Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) | Expr::Some(e) => {
                walk_expr(e, member_name)
            }
            Expr::If(c, t, e) => {
                walk_expr(c, member_name)
                    || walk_expr(t, member_name)
                    || e.as_ref().map_or(false, |x| walk_expr(x, member_name))
            }
            Expr::Let(_, v, b) => walk_expr(v, member_name) || walk_expr(b, member_name),
            Expr::Block(items) => items.iter().any(|e| walk_expr(e, member_name)),
            Expr::RecordConstruct(_, fields) | Expr::RecordUpdate(_, fields) => {
                fields.iter().any(|(_, e)| walk_expr(e, member_name))
            }
            Expr::Closure(_, b) => walk_expr(b, member_name),
            Expr::Tuple(es) => es.iter().any(|e| walk_expr(e, member_name)),
            Expr::Match(s, arms) => {
                walk_expr(s, member_name) || arms.iter().any(|a| walk_expr(&a.body, member_name))
            }
            Expr::EnumVariantWithData(_, _, args) => args.iter().any(|a| walk_expr(a, member_name)),
            Expr::FnCall(_, args) | Expr::MacroRef(_, args) => {
                args.iter().any(|a| walk_expr(a, member_name))
            }
            Expr::Range(s, e) => walk_expr(s, member_name) || walk_expr(e, member_name),
            Expr::NamespacedCall { args, .. } => args.iter().any(|a| walk_expr(a, member_name)),
            Expr::AddressOf {
                args, with_params, ..
            } => {
                args.iter().any(|a| walk_expr(a, member_name))
                    || with_params.iter().any(|(_, e)| walk_expr(e, member_name))
            }
            Expr::Encode { value, .. } => walk_expr(value, member_name),
            _ => false,
        }
    }

    entity_walks_any(entity, |e| walk_expr(e, member_name))
}

pub fn hashmap_members_needing_keys(entity: &Entity) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for m in &entity.members {
        if matches!(&m.ty, crate::ast::Type::Generic(name, p) if name == "HashMap" && p.len() == 2)
            && member_needs_keys_list(entity, &m.name)
        {
            out.insert(m.name.clone());
        }
    }
    out
}

pub fn member_is_hashmap(member: &Member) -> bool {
    matches!(&member.ty, crate::ast::Type::Generic(name, p) if name == "HashMap" && p.len() == 2)
}
