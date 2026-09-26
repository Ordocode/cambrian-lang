// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! `std::str::parse_*` metadata — see `docs/STDLIB.md` §4.1.

use crate::ast::{Expr, Program, RouteAction, Type};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseStrMeta {
    pub signed: bool,
    pub bits: u16,
}

pub fn parse_str_inner_type(meta: &ParseStrMeta) -> Type {
    let name = if meta.bits == 256 {
        if meta.signed {
            "i128".to_string()
        } else {
            "U256".to_string()
        }
    } else if meta.signed {
        match meta.bits {
            8 => "i8",
            16 => "i16",
            32 => "i32",
            64 => "i64",
            128 => "i128",
            _ => "i64",
        }
        .to_string()
    } else {
        match meta.bits {
            8 => "u8",
            16 => "u16",
            32 => "u32",
            64 => "u64",
            128 => "u128",
            256 => "U256",
            _ => "u64",
        }
        .to_string()
    };
    Type::Simple(name)
}

pub fn parse_str_return_type(meta: &ParseStrMeta) -> Type {
    Type::Generic("Option".to_string(), vec![parse_str_inner_type(meta)])
}

pub fn parse_str_meta(name: &str) -> Option<ParseStrMeta> {
    if name == "parse_uint" {
        return Some(ParseStrMeta {
            signed: false,
            bits: 64,
        });
    }
    if name == "parse_int" {
        return Some(ParseStrMeta {
            signed: true,
            bits: 64,
        });
    }

    let rest = name.strip_prefix("parse_")?;

    let (signed, bits) = match rest {
        "u8" => (false, 8),
        "u16" => (false, 16),
        "u32" => (false, 32),
        "u64" => (false, 64),
        "u128" => (false, 128),
        "U256" => (false, 256),
        "i8" => (true, 8),
        "i16" => (true, 16),
        "i32" => (true, 32),
        "i64" => (true, 64),
        "i128" => (true, 128),
        _ => return None,
    };

    Some(ParseStrMeta { signed, bits })
}

pub fn parse_str_meta_from_expr(expr: &Expr) -> Option<ParseStrMeta> {
    match expr {
        Expr::NamespacedCall {
            namespace, name, ..
        } if namespace == "std::str" => parse_str_meta(name),
        _ => None,
    }
}

pub fn parse_str_meta_sol_type(meta: &ParseStrMeta) -> String {
    if meta.signed {
        if meta.bits >= 128 {
            "int256".to_string()
        } else {
            format!("int{}", meta.bits)
        }
    } else if meta.bits >= 256 {
        "uint256".to_string()
    } else {
        format!("uint{}", meta.bits)
    }
}

pub fn infer_match_on_std_parse_sol_type(
    subject: &Expr,
    arms: &[crate::ast::MatchArm],
) -> Option<String> {
    let meta = parse_str_meta_from_expr(subject)?;
    if arms.iter().all(|a| matches!(a.body, Expr::BoolLiteral(_))) {
        return Some("bool".to_string());
    }
    Some(parse_str_meta_sol_type(&meta))
}

pub fn is_std_str_parse_call(namespace: &str, name: &str) -> bool {
    namespace == "std::str" && parse_str_meta(name).is_some()
}

pub fn std_str_parse_used_in_expr(expr: &Expr) -> bool {
    match expr {
        Expr::NamespacedCall {
            namespace,
            name,
            args,
            ..
        } => is_std_str_parse_call(namespace, name) || args.iter().any(std_str_parse_used_in_expr),
        Expr::FnCall(_, args) | Expr::MacroRef(_, args) => {
            args.iter().any(std_str_parse_used_in_expr)
        }
        Expr::BinOp(l, _, r) => std_str_parse_used_in_expr(l) || std_str_parse_used_in_expr(r),
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) | Expr::Some(e) => {
            std_str_parse_used_in_expr(e)
        }
        Expr::Index(b, k) => std_str_parse_used_in_expr(b) || std_str_parse_used_in_expr(k),
        Expr::If(c, t, e) => {
            std_str_parse_used_in_expr(c)
                || std_str_parse_used_in_expr(t)
                || e.as_ref().is_some_and(|el| std_str_parse_used_in_expr(el))
        }
        Expr::Let(_, v, b) => std_str_parse_used_in_expr(v) || std_str_parse_used_in_expr(b),
        Expr::Match(s, arms) => {
            std_str_parse_used_in_expr(s)
                || arms.iter().any(|a| std_str_parse_used_in_expr(&a.body))
        }
        Expr::Tuple(items) | Expr::Block(items) | Expr::ArrayLit(items) => {
            items.iter().any(std_str_parse_used_in_expr)
        }
        Expr::RecordConstruct(_, fields) | Expr::RecordUpdate(_, fields) => {
            fields.iter().any(|(_, v)| std_str_parse_used_in_expr(v))
        }
        Expr::MethodCall(b, _, args) => {
            std_str_parse_used_in_expr(b) || args.iter().any(std_str_parse_used_in_expr)
        }
        Expr::For(_, iter, body) => {
            std_str_parse_used_in_expr(iter) || std_str_parse_used_in_expr(body)
        }
        Expr::Closure(_, body) => std_str_parse_used_in_expr(body),
        Expr::AddressOf {
            args, with_params, ..
        } => {
            args.iter().any(std_str_parse_used_in_expr)
                || with_params
                    .iter()
                    .any(|(_, v)| std_str_parse_used_in_expr(v))
        }
        Expr::Encode { value, .. } => std_str_parse_used_in_expr(value),
        _ => false,
    }
}

pub fn std_str_parse_used_in_action(action: &RouteAction) -> bool {
    match action {
        RouteAction::Return { values } => values.iter().any(std_str_parse_used_in_expr),
        RouteAction::Send {
            args,
            dest,
            send_options,
            ..
        }
        | RouteAction::VarCall {
            args,
            dest,
            send_options,
            ..
        } => {
            args.iter().any(std_str_parse_used_in_expr)
                || std_str_parse_used_in_expr(dest)
                || send_options
                    .as_ref()
                    .is_some_and(std_str_parse_used_in_expr)
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            std_str_parse_used_in_expr(condition)
                || then_actions.iter().any(std_str_parse_used_in_action)
                || else_actions.iter().any(std_str_parse_used_in_action)
        }
        RouteAction::Let { value, .. } => std_str_parse_used_in_expr(value),
        RouteAction::Effect { args, .. }
        | RouteAction::CallRoute { args, .. }
        | RouteAction::Emit { args, .. }
        | RouteAction::ThrowCustom { args, .. } => args.iter().any(std_str_parse_used_in_expr),
        RouteAction::Deploy {
            constructor_args,
            send_options,
            ..
        } => {
            constructor_args.iter().any(std_str_parse_used_in_expr)
                || send_options
                    .as_ref()
                    .is_some_and(std_str_parse_used_in_expr)
        }
        RouteAction::Rescue { action, .. } => std_str_parse_used_in_action(action),
        RouteAction::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            update_args.iter().any(std_str_parse_used_in_expr)
                || callback_args.iter().any(std_str_parse_used_in_expr)
        }
        RouteAction::For { iter, body, .. } => {
            std_str_parse_used_in_expr(iter) || body.iter().any(std_str_parse_used_in_action)
        }
        RouteAction::Throw { .. } => false,
    }
}

pub fn std_str_parse_used_in_program(program: &Program) -> bool {
    for entity in &program.entities {
        for route in &entity.routes {
            if route
                .body
                .all_actions()
                .iter()
                .any(|a| std_str_parse_used_in_action(a))
            {
                return true;
            }
            for wc in &route.where_clauses {
                if std_str_parse_used_in_expr(&wc.condition) {
                    return true;
                }
            }
        }
        for member in &entity.members {
            for transform in &member.transforms {
                if std_str_parse_used_in_expr(&transform.body) {
                    return true;
                }
            }
            if let Some(d) = &member.default_value {
                if std_str_parse_used_in_expr(d) {
                    return true;
                }
            }
        }
    }
    for pf in &program.pure_fns {
        if std_str_parse_used_in_expr(&pf.body) {
            return true;
        }
    }
    false
}

pub fn std_str_u256_parse_used_in_program(program: &Program) -> bool {
    fn in_expr(expr: &Expr) -> bool {
        match expr {
            Expr::NamespacedCall {
                namespace,
                name,
                args,
                ..
            } => {
                if namespace == "std::str" {
                    if let Some(m) = parse_str_meta(name) {
                        if !m.signed && m.bits >= 256 {
                            return true;
                        }
                    }
                }
                args.iter().any(in_expr)
            }
            Expr::FnCall(_, args) | Expr::MacroRef(_, args) => args.iter().any(in_expr),
            Expr::BinOp(l, _, r) => in_expr(l) || in_expr(r),
            Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) | Expr::Some(e) => {
                in_expr(e)
            }
            Expr::Index(b, k) => in_expr(b) || in_expr(k),
            Expr::If(c, t, e) => {
                in_expr(c) || in_expr(t) || e.as_ref().is_some_and(|el| in_expr(el))
            }
            Expr::Let(_, v, b) => in_expr(v) || in_expr(b),
            Expr::Match(s, arms) => in_expr(s) || arms.iter().any(|a| in_expr(&a.body)),
            Expr::Tuple(items) | Expr::Block(items) | Expr::ArrayLit(items) => {
                items.iter().any(in_expr)
            }
            Expr::RecordConstruct(_, fields) | Expr::RecordUpdate(_, fields) => {
                fields.iter().any(|(_, v)| in_expr(v))
            }
            Expr::MethodCall(b, _, args) => in_expr(b) || args.iter().any(in_expr),
            Expr::For(_, iter, body) => in_expr(iter) || in_expr(body),
            Expr::Closure(_, body) => in_expr(body),
            Expr::AddressOf {
                args, with_params, ..
            } => args.iter().any(in_expr) || with_params.iter().any(|(_, v)| in_expr(v)),
            Expr::Encode { value, .. } => in_expr(value),
            _ => false,
        }
    }
    program.pure_fns.iter().any(|pf| in_expr(&pf.body))
        || program.entities.iter().any(|e| {
            e.routes.iter().any(|r| {
                r.body.all_actions().iter().any(|a| match a {
                    RouteAction::Return { values } => values.iter().any(|v| in_expr(v)),
                    RouteAction::Let { value, .. } => in_expr(value),
                    _ => std_str_parse_used_in_action(a),
                }) || r.where_clauses.iter().any(|w| in_expr(&w.condition))
            }) || e.members.iter().any(|m| {
                m.transforms.iter().any(|t| in_expr(&t.body))
                    || m.default_value.as_ref().is_some_and(|d| in_expr(d))
            })
        })
}

pub fn std_str_parse_needs_signed_helper(program: &Program) -> bool {
    fn in_expr(expr: &Expr) -> bool {
        match expr {
            Expr::NamespacedCall {
                namespace,
                name,
                args,
                ..
            } => {
                if namespace == "std::str" {
                    if let Some(m) = parse_str_meta(name) {
                        if m.signed {
                            return true;
                        }
                    }
                }
                args.iter().any(in_expr)
            }
            Expr::FnCall(_, args) | Expr::MacroRef(_, args) => args.iter().any(in_expr),
            Expr::BinOp(l, _, r) => in_expr(l) || in_expr(r),
            Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) | Expr::Some(e) => {
                in_expr(e)
            }
            Expr::Index(b, k) => in_expr(b) || in_expr(k),
            Expr::If(c, t, e) => {
                in_expr(c) || in_expr(t) || e.as_ref().is_some_and(|el| in_expr(el))
            }
            Expr::Let(_, v, b) => in_expr(v) || in_expr(b),
            Expr::Match(s, arms) => in_expr(s) || arms.iter().any(|a| in_expr(&a.body)),
            Expr::Tuple(items) | Expr::Block(items) | Expr::ArrayLit(items) => {
                items.iter().any(in_expr)
            }
            Expr::RecordConstruct(_, fields) | Expr::RecordUpdate(_, fields) => {
                fields.iter().any(|(_, v)| in_expr(v))
            }
            Expr::MethodCall(b, _, args) => in_expr(b) || args.iter().any(in_expr),
            Expr::For(_, iter, body) => in_expr(iter) || in_expr(body),
            Expr::Closure(_, body) => in_expr(body),
            Expr::AddressOf {
                args, with_params, ..
            } => args.iter().any(in_expr) || with_params.iter().any(|(_, v)| in_expr(v)),
            Expr::Encode { value, .. } => in_expr(value),
            _ => false,
        }
    }
    program.pure_fns.iter().any(|pf| in_expr(&pf.body))
        || program.entities.iter().any(|e| {
            e.routes.iter().any(|r| {
                r.body.all_actions().iter().any(|a| match a {
                    RouteAction::Return { values } => values.iter().any(|v| in_expr(v)),
                    RouteAction::Let { value, .. } => in_expr(value),
                    _ => std_str_parse_used_in_action(a),
                }) || r.where_clauses.iter().any(|w| in_expr(&w.condition))
            }) || e.members.iter().any(|m| {
                m.transforms.iter().any(|t| in_expr(&t.body))
                    || m.default_value.as_ref().is_some_and(|d| in_expr(d))
            })
        })
}
