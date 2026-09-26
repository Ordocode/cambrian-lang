// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM lowering for `Option<T>` — tagged-union struct, not a 0-sentinel.
//!
//! ```solidity
//! enum Option_uint64_Tag { None, Some }
//! struct Option_uint64 {
//!     Option_uint64_Tag tag;
//!     uint64 some_0;
//! }
//! ```
//!
//! `none` / `some(v)` construct the struct; `match` dispatches on `.tag`.
//! `some(0)` is distinct from `none` (PW3-O-001).

use std::collections::{BTreeMap, HashMap};

use crate::ast::{Entity, Expr, Pattern, Program, RouteAction, Type};

use super::ctx::EvmCtx;

/// Solidity struct name for `Option<inner>` (`Option_uint64`, `Option_Option_uint64`, …).
pub(crate) fn option_struct_name(inner: &Type) -> String {
    format!("Option_{}", option_type_ident(inner))
}

pub(crate) fn option_tag_name(inner: &Type) -> String {
    format!("{}_Tag", option_struct_name(inner))
}

/// Identifier-safe spelling of `ty` used inside `Option_*` names.
pub(crate) fn option_type_ident(ty: &Type) -> String {
    match ty {
        Type::Generic(n, ps) if n == "Option" && ps.len() == 1 => {
            format!("Option_{}", option_type_ident(&ps[0]))
        }
        Type::Generic(n, ps) if n == "Vec" && ps.len() == 1 => {
            format!("{}_arr", option_type_ident(&ps[0]))
        }
        Type::Generic(n, ps) if n == "HashMap" && ps.len() == 2 => {
            format!(
                "Map_{}_{}",
                option_type_ident(&ps[0]),
                option_type_ident(&ps[1])
            )
        }
        Type::Simple(name) => match name.as_str() {
            "u8" => "uint8".to_string(),
            "u16" => "uint16".to_string(),
            "u32" => "uint32".to_string(),
            "u64" => "uint64".to_string(),
            "u128" => "uint128".to_string(),
            "usize" | "U256" => "uint256".to_string(),
            "i8" => "int8".to_string(),
            "i16" => "int16".to_string(),
            "i32" => "int32".to_string(),
            "i64" => "int64".to_string(),
            "i128" => "int128".to_string(),
            "bool" => "bool".to_string(),
            "String" => "string".to_string(),
            "CamData" | "bytes" => "bytes".to_string(),
            "address" => "address".to_string(),
            "pubkey" | "bytes32" => "bytes32".to_string(),
            other => other.to_string(),
        },
        Type::TypedAddress(_) => "address".to_string(),
        Type::Tuple(items) => {
            let parts: Vec<String> = items.iter().map(option_type_ident).collect();
            format!("Tup_{}", parts.join("_"))
        }
        Type::Generic(_, _) => "bytes".to_string(),
    }
}

pub(crate) fn strip_memory_ty(sol_ty: &str) -> &str {
    sol_ty
        .strip_suffix(" memory")
        .or_else(|| sol_ty.strip_suffix(" storage"))
        .unwrap_or(sol_ty)
}

pub(crate) fn is_option_sol_ty(sol_ty: &str) -> bool {
    strip_memory_ty(sol_ty).starts_with("Option_")
}

fn payload_sol_ty(inner: &Type) -> String {
    match inner {
        Type::Generic(n, ps) if n == "Option" && ps.len() == 1 => option_struct_name(&ps[0]),
        Type::Generic(n, ps) if n == "Vec" && ps.len() == 1 => {
            format!("{}[]", payload_sol_ty(&ps[0]))
        }
        _ => option_type_ident(inner),
    }
}

pub(crate) fn option_some_from_sol_inner(inner_sol_ty: &str, inner_val: &str) -> String {
    let ident = strip_memory_ty(inner_sol_ty)
        .replace("[]", "_arr")
        .replace(['(', ')', ' ', ',', '=', '>'], "_");
    let name = format!("Option_{ident}");
    format!("{name}({{ tag: {name}_Tag.Some, some_0: {inner_val} }})")
}

pub(crate) fn option_none_from_sol_name(struct_name: &str) -> String {
    let inner = struct_name.strip_prefix("Option_").unwrap_or("uint256");
    let payload = if inner.starts_with("Option_") {
        option_none_from_sol_name(inner)
    } else {
        "0".to_string()
    };
    format!("{struct_name}({{ tag: {struct_name}_Tag.None, some_0: {payload} }})")
}

pub(crate) fn option_none_from_sol_ty(sol_ty: &str) -> String {
    option_none_from_sol_name(strip_memory_ty(sol_ty))
}

fn option_nesting_depth(inner: &Type) -> u32 {
    match inner {
        Type::Generic(n, ps) if n == "Option" && ps.len() == 1 => 1 + option_nesting_depth(&ps[0]),
        _ => 0,
    }
}

fn visit_ty(ty: &Type, out: &mut BTreeMap<String, Type>) {
    match ty {
        Type::Generic(n, ps) if n == "Option" && ps.len() == 1 => {
            visit_ty(&ps[0], out);
            out.insert(option_struct_name(&ps[0]), ps[0].clone());
        }
        Type::Generic(_, ps) => {
            for p in ps {
                visit_ty(p, out);
            }
        }
        Type::Tuple(items) => {
            for t in items {
                visit_ty(t, out);
            }
        }
        Type::Simple(_) | Type::TypedAddress(_) => {}
    }
}

fn infer_ty(expr: &Expr, entity: &Entity, ctx: &EvmCtx) -> Option<Type> {
    match expr {
        Expr::Ident(name) => entity
            .members
            .iter()
            .find(|m| m.name == *name)
            .map(|m| m.ty.clone())
            .or_else(|| {
                entity.routes.iter().find_map(|r| {
                    r.params
                        .iter()
                        .find(|p| p.name == *name)
                        .map(|p| p.ty.clone())
                })
            }),
        Expr::IntLiteral(_) => {
            Some(Type::Simple("u64".into()))
        }
        Expr::BoolLiteral(_) => Some(Type::Simple("bool".into())),
        Expr::If(_, then, _) => infer_ty(then, entity, ctx),
        Expr::Some(inner) => infer_ty(inner, entity, ctx)
            .map(|t| Type::Generic("Option".into(), vec![t])),
        Expr::FieldAccess(base, field) => {
            let base_ty = infer_ty(base, entity, ctx)?;
            let rec_name = match &base_ty {
                Type::Simple(n) => n.clone(),
                Type::Generic(g, ps) if (g == "HashMap" || g == "Vec") && !ps.is_empty() => {
                    match ps.last()? {
                        Type::Simple(n) => n.clone(),
                        _ => return None,
                    }
                }
                _ => return None,
            };
            let rec = entity
                .records
                .iter()
                .find(|r| r.name == rec_name)
                .or_else(|| ctx.lookup_record(&rec_name))?;
            rec.fields
                .iter()
                .find(|f| f.name == *field)
                .map(|f| f.ty.clone())
        }
        Expr::Index(base, _) => {
            if let Expr::Ident(name) = base.as_ref() {
                if let Some(member) = entity.members.iter().find(|m| m.name == *name) {
                    if let Type::Generic(g, ps) = &member.ty {
                        if g == "HashMap" && ps.len() == 2 {
                            return Some(ps[1].clone());
                        }
                        if g == "Vec" && ps.len() == 1 {
                            return Some(ps[0].clone());
                        }
                    }
                }
            }
            match infer_ty(base, entity, ctx)? {
                Type::Generic(g, ps) if (g == "HashMap" || g == "Vec") && !ps.is_empty() => {
                    ps.last().cloned()
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn walk_expr(expr: &Expr, entity: &Entity, ctx: &EvmCtx, out: &mut BTreeMap<String, Type>) {
    walk_expr_bindings(expr, entity, ctx, out, &mut HashMap::new());
}

fn walk_expr_bindings(
    expr: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    out: &mut BTreeMap<String, Type>,
    bindings: &mut HashMap<String, Type>,
) {
    match expr {
        Expr::Some(inner) => {
            let inner_ty = infer_ty(inner, entity, ctx).or_else(|| match inner.as_ref() {
                Expr::Ident(name) => bindings.get(name).cloned(),
                _ => None,
            });
            if let Some(ty) = inner_ty {
                visit_ty(&Type::Generic("Option".into(), vec![ty]), out);
            }
            walk_expr_bindings(inner, entity, ctx, out, bindings);
        }
        Expr::BinOp(l, _, r) | Expr::Index(l, r) | Expr::Range(l, r) => {
            walk_expr_bindings(l, entity, ctx, out, bindings);
            walk_expr_bindings(r, entity, ctx, out, bindings);
        }
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Closure(_, e) => {
            walk_expr_bindings(e, entity, ctx, out, bindings)
        }
        Expr::For(_, iter, body) => {
            walk_expr_bindings(iter, entity, ctx, out, bindings);
            walk_expr_bindings(body, entity, ctx, out, bindings);
        }
        Expr::Cast(e, ty) => {
            walk_expr_bindings(e, entity, ctx, out, bindings);
            visit_ty(ty, out);
        }
        Expr::If(c, t, e) => {
            walk_expr_bindings(c, entity, ctx, out, bindings);
            walk_expr_bindings(t, entity, ctx, out, bindings);
            if let Some(e) = e {
                walk_expr_bindings(e, entity, ctx, out, bindings);
            }
        }
        Expr::Match(s, arms) => {
            walk_expr_bindings(s, entity, ctx, out, bindings);
            for a in arms {
                walk_expr_bindings(&a.body, entity, ctx, out, bindings);
            }
        }
        Expr::Let(Pattern::Ident(name), v, b) => {
            walk_expr_bindings(v, entity, ctx, out, bindings);
            let ty = infer_ty(v, entity, ctx).or_else(|| {
                if let Expr::Index(base, _) = v.as_ref() {
                    if let Expr::Ident(inner) = base.as_ref() {
                        return bindings.get(inner).and_then(|t| match t {
                            Type::Generic(g, ps) if g == "HashMap" && ps.len() == 2 => {
                                Some(ps[1].clone())
                            }
                            _ => None,
                        });
                    }
                }
                None
            });
            if let Some(ty) = ty {
                bindings.insert(name.clone(), ty);
            }
            walk_expr_bindings(b, entity, ctx, out, bindings);
            bindings.remove(name);
        }
        Expr::Let(_, v, b) => {
            walk_expr_bindings(v, entity, ctx, out, bindings);
            walk_expr_bindings(b, entity, ctx, out, bindings);
        }
        Expr::Block(items) => {
            let mut local: Vec<String> = Vec::new();
            for item in items {
                if let Expr::Let(Pattern::Ident(name), v, _) = item {
                    walk_expr_bindings(v, entity, ctx, out, bindings);
                    let ty = infer_ty(v, entity, ctx).or_else(|| {
                        if let Expr::Index(base, _) = v.as_ref() {
                            if let Expr::Ident(inner) = base.as_ref() {
                                return bindings.get(inner).and_then(|t| match t {
                                    Type::Generic(g, ps) if g == "HashMap" && ps.len() == 2 => {
                                        Some(ps[1].clone())
                                    }
                                    _ => None,
                                });
                            }
                        }
                        None
                    });
                    if let Some(ty) = ty {
                        bindings.insert(name.clone(), ty);
                        local.push(name.clone());
                    }
                }
                let target = match item {
                    Expr::Let(_, _, body) => body.as_ref(),
                    other => other,
                };
                walk_expr_bindings(target, entity, ctx, out, bindings);
            }
            for name in local {
                bindings.remove(&name);
            }
        }
        Expr::ArrayLit(items) | Expr::Tuple(items) => {
            for e in items {
                walk_expr_bindings(e, entity, ctx, out, bindings);
            }
        }
        Expr::FnCall(_, args) | Expr::EnumVariantWithData(_, _, args) | Expr::MacroRef(_, args) => {
            for a in args {
                walk_expr_bindings(a, entity, ctx, out, bindings);
            }
        }
        Expr::MethodCall(recv, _, args) => {
            walk_expr_bindings(recv, entity, ctx, out, bindings);
            for a in args {
                walk_expr_bindings(a, entity, ctx, out, bindings);
            }
        }
        Expr::NamespacedCall { args, .. } => {
            for a in args {
                walk_expr_bindings(a, entity, ctx, out, bindings);
            }
        }
        Expr::RecordConstruct(_, fields) | Expr::RecordUpdate(_, fields) => {
            if let Expr::RecordUpdate(base, _) = expr {
                walk_expr_bindings(base, entity, ctx, out, bindings);
            }
            for (_, e) in fields {
                walk_expr_bindings(e, entity, ctx, out, bindings);
            }
        }
        _ => {}
    }
}

fn walk_action(a: &RouteAction, entity: &Entity, ctx: &EvmCtx, out: &mut BTreeMap<String, Type>) {
        match a {
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
                for e in args {
                    walk_expr(e, entity, ctx, out);
                }
                walk_expr(dest, entity, ctx, out);
                if let Some(o) = send_options {
                    walk_expr(o, entity, ctx, out);
                }
            }
            RouteAction::Conditional {
                condition,
                then_actions,
                else_actions,
            } => {
                walk_expr(condition, entity, ctx, out);
                for a in then_actions {
                    walk_action(a, entity, ctx, out);
                }
                for a in else_actions {
                    walk_action(a, entity, ctx, out);
                }
            }
            RouteAction::Return { values }
            | RouteAction::Effect { args: values, .. }
            | RouteAction::ThrowCustom { args: values, .. }
            | RouteAction::Emit { args: values, .. }
            | RouteAction::CallRoute { args: values, .. } => {
                for e in values {
                    walk_expr(e, entity, ctx, out);
                }
            }
            RouteAction::Let { value, .. } => walk_expr(value, entity, ctx, out),
            RouteAction::Deploy {
                send_options,
                constructor_args,
                ..
            } => {
                for e in constructor_args {
                    walk_expr(e, entity, ctx, out);
                }
                if let Some(o) = send_options {
                    walk_expr(o, entity, ctx, out);
                }
            }
            RouteAction::Rescue { action, .. } => walk_action(action, entity, ctx, out),
            RouteAction::UpdateCode {
                update_args,
                callback_args,
                ..
            } => {
                for e in update_args.iter().chain(callback_args.iter()) {
                    walk_expr(e, entity, ctx, out);
                }
            }
            RouteAction::For { iter, body, .. } => {
                walk_expr(iter, entity, ctx, out);
                for a in body {
                    walk_action(a, entity, ctx, out);
                }
            }
            RouteAction::Throw { .. } => {}
        }
}

fn collect_option_inners(program: &Program, ctx: &EvmCtx) -> Vec<Type> {
    let mut out = BTreeMap::new();
    for rec in &program.records {
        for f in &rec.fields {
            visit_ty(&f.ty, &mut out);
        }
    }
    for en in &program.enums {
        for v in &en.variants {
            for t in &v.fields {
                visit_ty(t, &mut out);
            }
        }
    }
    for ta in &program.type_aliases {
        visit_ty(&ta.ty, &mut out);
    }
    for pf in &program.pure_fns {
        for p in &pf.params {
            visit_ty(&p.ty, &mut out);
        }
        visit_ty(&pf.return_type, &mut out);
        if let Some(entity) = program.entities.first() {
            walk_expr(&pf.body, entity, ctx, &mut out);
        }
    }
    for entity in &program.entities {
        for m in &entity.members {
            visit_ty(&m.ty, &mut out);
            if let Some(d) = &m.default_value {
                walk_expr(d, entity, ctx, &mut out);
            }
            for tr in &m.transforms {
                walk_expr(&tr.body, entity, ctx, &mut out);
            }
        }
        for rec in &entity.records {
            for f in &rec.fields {
                visit_ty(&f.ty, &mut out);
            }
        }
        for en in &entity.enums {
            for v in &en.variants {
                for t in &v.fields {
                    visit_ty(t, &mut out);
                }
            }
        }
        for r in &entity.routes {
            for p in &r.params {
                visit_ty(&p.ty, &mut out);
            }
            if let Some(t) = &r.return_type {
                visit_ty(t, &mut out);
            }
            for a in r.body.all_actions() {
                walk_action(a, entity, ctx, &mut out);
            }
        }
    }
    let mut items: Vec<(u32, String, Type)> = out
        .into_iter()
        .map(|(name, inner)| (option_nesting_depth(&inner), name, inner))
        .collect();
    items.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    items.into_iter().map(|(_, _, t)| t).collect()
}

fn is_primitive_option_payload(ty: &Type) -> bool {
    match ty {
        Type::Generic(n, ps) if n == "Option" && ps.len() == 1 => {
            is_primitive_option_payload(&ps[0])
        }
        Type::Generic(n, _) if n == "Vec" || n == "HashMap" => true,
        Type::TypedAddress(_) => true,
        Type::Tuple(_) => true,
        Type::Simple(name) => matches!(
            name.as_str(),
            "u8" | "u16"
                | "u32"
                | "u64"
                | "u128"
                | "usize"
                | "U256"
                | "i8"
                | "i16"
                | "i32"
                | "i64"
                | "i128"
                | "bool"
                | "String"
                | "CamData"
                | "bytes"
                | "address"
                | "pubkey"
                | "bytes32"
        ),
        Type::Generic(_, _) => true,
    }
}

fn option_payload_type_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::Generic(n, ps) if n == "Option" && ps.len() == 1 => option_payload_type_name(&ps[0]),
        Type::Simple(name) if !is_primitive_option_payload(ty) => Some(name.as_str()),
        _ => None,
    }
}

fn emit_option_struct(inner: &Type) -> String {
    let sname = option_struct_name(inner);
    let tname = option_tag_name(inner);
    let payload_ty = payload_sol_ty(inner);
    format!(
        "enum {tname} {{ None, Some }}\nstruct {sname} {{\n    {tname} tag;\n    {payload_ty} some_0;\n}}\n\n"
    )
}

fn emit_option_structs<'a, I>(inners: I) -> String
where
    I: IntoIterator<Item = &'a Type>,
{
    let mut out = String::new();
    for inner in inners {
        out.push_str(&emit_option_struct(inner));
    }
    out
}

/// Options whose payload is a primitive / Vec / nested primitive Option.
pub(crate) fn gen_option_structs_primitive(program: &Program, ctx: &EvmCtx) -> String {
    emit_option_structs(
        collect_option_inners(program, ctx)
            .iter()
            .filter(|t| is_primitive_option_payload(t)),
    )
}

/// Options whose innermost named payload is `type_name` (a record or enum).
pub(crate) fn gen_option_structs_for_named(
    program: &Program,
    ctx: &EvmCtx,
    type_name: &str,
) -> String {
    emit_option_structs(
        collect_option_inners(program, ctx)
            .iter()
            .filter(|t| option_payload_type_name(t) == Some(type_name)),
    )
}
