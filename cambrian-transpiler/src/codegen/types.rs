// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use crate::ast::*;
use cambrian_core::BeSerialize;
use std::collections::HashMap;

pub(super) fn resolve_alias(ty: &Type, alias_map: &HashMap<String, Type>) -> Type {
    match ty {
        Type::Simple(name) => alias_map
            .get(name)
            .map(|r| resolve_alias(r, alias_map))
            .unwrap_or_else(|| ty.clone()),
        Type::Generic(name, params) => Type::Generic(
            name.clone(),
            params.iter().map(|p| resolve_alias(p, alias_map)).collect(),
        ),
        Type::Tuple(elems) => {
            Type::Tuple(elems.iter().map(|e| resolve_alias(e, alias_map)).collect())
        }
        Type::TypedAddress(name) => Type::TypedAddress(name.clone()),
    }
}

pub fn gen_type(ty: &Type) -> String {
    match ty {
        Type::Simple(name) => name.clone(),
        Type::Generic(name, params) => {
            let p: Vec<String> = params.iter().map(gen_type).collect();
            format!("{}<{}>", name, p.join(", "))
        }
        Type::Tuple(elems) => {
            let e: Vec<String> = elems.iter().map(gen_type).collect();
            if elems.len() == 1 {
                format!("({},)", e[0])
            } else {
                format!("({})", e.join(", "))
            }
        }
        Type::TypedAddress(_entity) => "address".to_string(),
    }
}

pub fn is_complex_rust_type(ty: &str) -> bool {
    let t = ty.trim();
    t == "String"
        || t.starts_with("HashMap<")
        || t.starts_with("Vec<")
        || t.starts_with("Option<")
        || t.starts_with('(')
}

pub fn cambrian_function_id(name: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5; // Fowler–Noll–Vo FNV-1a 32-bit offset basis
    for b in name.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

pub fn infer_expr_type(expr: &Expr, entity: &Entity, route_params: &[Param]) -> Option<Type> {
    match expr {
        Expr::Ident(name) => {
            if let Some(m) = entity.members.iter().find(|m| m.name == *name) {
                return Some(m.ty.clone());
            }
            if let Some(p) = route_params.iter().find(|p| p.name == *name) {
                return Some(p.ty.clone());
            }
            None
        }
        Expr::TemporalRef(name) => entity
            .members
            .iter()
            .find(|m| m.name == *name)
            .map(|m| m.ty.clone()),
        Expr::IntLiteral(v) => {
            if !v.fits_u128() {
                Some(Type::Simple("U256".to_string()))
            } else if v.lo > u128::from(u64::MAX) {
                Some(Type::Simple("u128".to_string()))
            } else {
                Some(Type::Simple("u64".to_string()))
            }
        }
        Expr::BoolLiteral(_) => Some(Type::Simple("bool".to_string())),
        Expr::StringLiteral(_) => Some(Type::Simple("String".to_string())),
        Expr::Cast(_, ty) => Some(ty.clone()),
        Expr::SysField(field) => match field.as_str() {
            "pubkey" => Some(Type::Simple("pubkey".to_string())),
            "now" | "logicaltime" => Some(Type::Simple("u64".to_string())),
            "rnd_seed" | "balance" => Some(Type::Simple("u128".to_string())),
            "address" => Some(Type::Simple("address".to_string())),
            _ => None,
        },
        Expr::MsgField(field) => match field.as_str() {
            "pubkey" => Some(Type::Simple("pubkey".to_string())),
            "sender" => Some(Type::Simple("address".to_string())),
            "timestamp" | "logicaltime" => Some(Type::Simple("u64".to_string())),
            "value" => Some(Type::Simple("u128".to_string())),
            "int" | "ext" => Some(Type::Simple("bool".to_string())),
            "currencies" => Some(Type::Generic(
                "HashMap".to_string(),
                vec![
                    Type::Simple("u32".to_string()),
                    Type::Simple("u128".to_string()),
                ],
            )),
            _ => None,
        },
        Expr::FnCall(name, args) => match name.as_str() {
            "cam_add" | "cam_sub" | "cam_mul" => args
                .iter()
                .find_map(|a| infer_expr_type(a, entity, route_params)),
            _ => None,
        },
        Expr::BinOp(lhs, _, rhs) => infer_expr_type(lhs, entity, route_params)
            .or_else(|| infer_expr_type(rhs, entity, route_params)),
        Expr::Index(base, _) => {
            if let Some(Type::Generic(name, params)) = infer_expr_type(base, entity, route_params) {
                if (name == "HashMap" || name == "Map") && params.len() == 2 {
                    return Some(params[1].clone());
                }
                if name == "Vec" && params.len() == 1 {
                    return Some(params[0].clone());
                }
            }
            None
        }
        Expr::MethodCall(base, method, _args) => {
            if method == "cam_get" || method == "get" {
                if let Some(Type::Generic(name, params)) =
                    infer_expr_type(base, entity, route_params)
                {
                    if (name == "HashMap" || name == "Map") && params.len() == 2 {
                        return Some(params[1].clone());
                    }
                }
            }
            if method == "clone" {
                return infer_expr_type(base, entity, route_params);
            }
            None
        }
        Expr::NamespacedCall {
            namespace,
            name,
            type_params,
            ..
        } => {
            if super::stdlib::is_std_namespace(namespace) {
                return super::stdlib::infer_std_call_return_type(namespace, name);
            }
            match (namespace.as_str(), name.as_str()) {
                (_, "hash") => Some(Type::Simple("U256".to_string())),
                (_, "code") | (_, "codeSalt") => Some(Type::Simple("CamData".to_string())),
                (_, "decode") if !type_params.is_empty() => {
                    if type_params.len() == 1 {
                        Some(Type::Tuple(vec![
                            type_params[0].clone(),
                            Type::Simple("CamData".to_string()),
                        ]))
                    } else {
                        Some(Type::Tuple(vec![
                            Type::Tuple(type_params.clone()),
                            Type::Simple("CamData".to_string()),
                        ]))
                    }
                }
                (_, "encode") => Some(Type::Simple("CamData".to_string())),
                ("evm", "ecrecover") => Some(Type::Simple("address".to_string())),
                ("evm", "keccak256Packed") => Some(Type::Simple("U256".to_string())),
                _ => None,
            }
        }
        _ => None,
    }
}

pub fn resolve_target_entity(dest: &Expr, entity: &Entity, route: &Route) -> Option<String> {
    if let Some(name) = extract_entity_from_method_call(dest) {
        return Some(name);
    }

    if let Expr::Ident(var_name) = dest {
        if let Some(member) = entity.members.iter().find(|m| m.name == *var_name) {
            if let Type::TypedAddress(entity_name) = &member.ty {
                return Some(entity_name.clone());
            }
        }
        if let Some(param) = route.params.iter().find(|p| p.name == *var_name) {
            if let Type::TypedAddress(entity_name) = &param.ty {
                return Some(entity_name.clone());
            }
        }
    }

    if matches!(dest, Expr::MsgField(f) if f == "sender") {
        if let Some(name) = extract_sender_entity_from_where(route, entity) {
            return Some(name);
        }
    }

    if let Expr::MacroRef(macro_name, _) = dest {
        if let Some(mac) = entity.macros.iter().find(|m| m.name == *macro_name) {
            if let Type::TypedAddress(entity_name) = &mac.return_type {
                return Some(entity_name.clone());
            }
        }
    }

    if let Expr::MethodCall(receiver, method, _) = dest {
        if method == "unwrap" {
            if let Expr::Ident(var_name) = receiver.as_ref() {
                if let Some(member) = entity.members.iter().find(|m| m.name == *var_name) {
                    if let Type::Generic(outer, params) = &member.ty {
                        if outer == "Option" && params.len() == 1 {
                            if let Type::TypedAddress(entity_name) = &params[0] {
                                return Some(entity_name.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

/// A var-call (`var x = route(args) ~> dest`) whose destination entity could
/// not be resolved has no faithful Solidity lowering: without the callee's
/// interface there is no way to type the call or decode its return value.
///
/// Emitting a comment in its place used to let such a contract compile with
/// the bound variable silently left at its zero value, so every guard reading
/// it took the wrong branch. Validation rejects this shape (V23), but codegen
/// must not degrade fidelity on its own when validation is advisory — a
/// var-call either lowers faithfully or stops the build.
pub fn unresolved_var_call(name: &str, dest_expr: &str) -> ! {
    panic!(
        "EVM codegen: cannot resolve the target entity of var call '{name}' on \
         '{dest_expr}' — a cross-contract READ needs the callee's interface. \
         Type the destination as `Address<Entity>` (declaring `extern entity \
         Entity {{ view route ...; }}` when the callee is outside this program) \
         instead of a plain `address`. Sends lower without an interface via \
         `abi.encodeWithSignature`, which is why only reads hit this."
    );
}

pub fn extract_send_options<F>(opts: Option<&Expr>, gen_expr: F) -> Vec<(String, String)>
where
    F: Fn(&Expr) -> String,
{
    match opts {
        None => vec![],
        Some(Expr::RecordConstruct(_, fields)) | Some(Expr::RecordUpdate(_, fields)) => fields
            .iter()
            .map(|(k, v)| (k.clone(), gen_expr(v)))
            .collect(),
        Some(other) => {
            vec![("__expr".to_string(), gen_expr(other))]
        }
    }
}

// ---------------------------------------------------------------------------
// Default state computation (platform-agnostic core)
// ---------------------------------------------------------------------------

pub fn default_ser_be(
    ty: &Type,
    platform_fallback: &dyn Fn(&Type) -> Option<Vec<u8>>,
    alias_map: &HashMap<String, Type>,
) -> Vec<u8> {
    let resolved = resolve_alias(ty, alias_map);

    if let Some(bytes) = platform_fallback(&resolved) {
        return bytes;
    }

    match &resolved {
        Type::Simple(name) => match name.as_str() {
            "u8" => 0u8.ser_be(),
            "i8" => 0i8.ser_be(),
            "bool" => false.ser_be(),
            "u16" => 0u16.ser_be(),
            "i16" => 0i16.ser_be(),
            "u32" => 0u32.ser_be(),
            "i32" => 0i32.ser_be(),
            "u64" | "usize" => 0u64.ser_be(),
            "i64" => 0i64.ser_be(),
            "u128" => 0u128.ser_be(),
            "i128" => 0i128.ser_be(),
            "U256" | "uint256" => cambrian_core::U256::ZERO.ser_be(),
            "pubkey" => [0u8; 32].ser_be(),
            "String" => String::default().ser_be(),
            _ => vec![0u8; 4],
        },
        Type::Generic(name, _) => match name.as_str() {
            "Vec" | "HashMap" => Vec::<u8>::new().ser_be(),
            "Option" => Option::<u8>::None.ser_be(),
            _ => vec![0u8; 4],
        },
        Type::Tuple(elems) => elems
            .iter()
            .flat_map(|e| default_ser_be(e, platform_fallback, alias_map))
            .collect(),
        Type::TypedAddress(_) => vec![0u8; 4],
    }
}

pub fn is_complex_type(ty: &Type) -> bool {
    match ty {
        Type::Simple(name) => matches!(name.as_str(), "String"),
        Type::Generic(name, _) => matches!(name.as_str(), "Vec" | "HashMap" | "Option"),
        Type::Tuple(_) => true,
        Type::TypedAddress(_) => false,
    }
}

pub fn compute_default_state_hex(
    entity: &Entity,
    platform_fallback: &dyn Fn(&Type) -> Option<Vec<u8>>,
    alias_map: &HashMap<String, Type>,
) -> String {
    let mut bytes = Vec::new();
    for m in &entity.members {
        if m.is_identity {
            continue;
        }
        let member_bytes = default_ser_be(&m.ty, platform_fallback, alias_map);
        if is_complex_type(&resolve_alias(&m.ty, alias_map)) {
            bytes.extend_from_slice(&(member_bytes.len() as u32).to_be_bytes());
        }
        bytes.extend_from_slice(&member_bytes);
    }
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

// ---------------------------------------------------------------------------
// Helpers shared with entity/route submodules
// ---------------------------------------------------------------------------

pub(super) fn default_value_for_type(ty: &Type) -> String {
    match ty {
        Type::Simple(name) => match name.as_str() {
            "bool" => "false".to_string(),
            "String" => "String::new()".to_string(),
            "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32" | "i64"
            | "i128" => "0".to_string(),
            "U256" => "Default::default()".to_string(),
            "pubkey" => "[0u8; 32]".to_string(),
            "address" => "Default::default()".to_string(),
            _ => "Default::default()".to_string(),
        },
        Type::Generic(name, _) => match name.as_str() {
            "Vec" => "Vec::new()".to_string(),
            "HashMap" => "HashMap::new()".to_string(),
            _ => "Default::default()".to_string(),
        },
        Type::Tuple(elems) => {
            let defaults: Vec<String> = elems.iter().map(default_value_for_type).collect();
            if elems.len() == 1 {
                format!("({},)", defaults[0])
            } else {
                format!("({})", defaults.join(", "))
            }
        }
        Type::TypedAddress(_) => "Default::default()".to_string(),
    }
}

pub(super) fn resolve_type_with_aliases(ty: &Type, ta: &[TypeAlias]) -> Type {
    match ty {
        Type::Simple(name) => {
            for alias in ta {
                if alias.name == *name {
                    return resolve_type_with_aliases(&alias.ty, ta);
                }
            }
            ty.clone()
        }
        Type::Generic(name, params) => Type::Generic(
            name.clone(),
            params
                .iter()
                .map(|p| resolve_type_with_aliases(p, ta))
                .collect(),
        ),
        Type::Tuple(elems) => Type::Tuple(
            elems
                .iter()
                .map(|e| resolve_type_with_aliases(e, ta))
                .collect(),
        ),
        Type::TypedAddress(n) => Type::TypedAddress(n.clone()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NumType {
    width: u16,
    signed: bool,
}

fn parse_rust_num_type(name: &str) -> Option<NumType> {
    match name {
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize" => {
            let width = name
                .trim_start_matches('u')
                .trim_end_matches("size")
                .parse()
                .ok()?;
            Some(NumType {
                width,
                signed: false,
            })
        }
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" => {
            let width = name
                .trim_start_matches('i')
                .trim_end_matches("size")
                .parse()
                .ok()?;
            Some(NumType {
                width,
                signed: true,
            })
        }
        "U256" | "uint256" => Some(NumType {
            width: 256,
            signed: false,
        }),
        _ => None,
    }
}

fn widen_binop_result_type(a: &NumType, b: &NumType) -> Option<NumType> {
    if a.signed == b.signed {
        Some(NumType {
            width: a.width.max(b.width),
            signed: a.signed,
        })
    } else {
        let (u, s) = if a.signed { (b, a) } else { (a, b) };
        if u.width >= 128 {
            None
        } else {
            let needed = (u.width * 2).max(s.width);
            if needed > 128 {
                None
            } else {
                Some(NumType {
                    width: needed,
                    signed: true,
                })
            }
        }
    }
}

fn num_type_rust_name(nt: &NumType) -> String {
    if nt.width == 256 {
        "U256".to_string()
    } else if nt.signed {
        format!("i{}", nt.width)
    } else {
        format!("u{}", nt.width)
    }
}

pub(super) fn coerce_transform_for_member(
    body: String,
    expr: &Expr,
    member_ty: &Type,
    entity: &Entity,
    route_params: &[Param],
    ta: &[TypeAlias],
    pure_fns: &[PureFn],
) -> String {
    let resolved = resolve_type_with_aliases(member_ty, ta);
    let member_rust = gen_type(&resolved);
    if member_rust == "U256" && body.trim().parse::<i128>().is_ok() {
        return format!("CamCast::<{}>::cam_cast({})", gen_type(member_ty), body);
    }
    let expr_ty = infer_transform_result_rust_type(expr, entity, route_params, pure_fns, ta);
    if let (Some(src), dst) = (expr_ty.as_deref(), member_rust.as_str()) {
        if src == "String" && is_castable_numeric(dst) {
            return format!("({body}).parse::<{dst}>().unwrap_or_default()");
        }
        if dst == "String" && is_castable_numeric(src) {
            return format!("({body}).to_string()");
        }
        if src != dst {
            if dst == "U256" {
                return format!("CamCast::<U256>::cam_cast({body})");
            }
            if is_castable_numeric(dst) && is_castable_numeric(src) {
                return format!("({body}) as {dst}");
            }
        }
    }
    body
}

fn infer_transform_result_rust_type(
    expr: &Expr,
    entity: &Entity,
    route_params: &[Param],
    pure_fns: &[PureFn],
    ta: &[TypeAlias],
) -> Option<String> {
    match expr {
        Expr::BinOp(lhs, op, rhs) if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) => {
            let lt = infer_transform_result_rust_type(lhs, entity, route_params, pure_fns, ta)?;
            let rt = infer_transform_result_rust_type(rhs, entity, route_params, pure_fns, ta)?;
            let ln = parse_rust_num_type(&lt)?;
            let rn = parse_rust_num_type(&rt)?;
            Some(num_type_rust_name(&widen_binop_result_type(&ln, &rn)?))
        }
        _ => infer_expr_type_for_member(expr, entity, route_params, pure_fns, ta),
    }
}

fn infer_expr_type_for_member(
    expr: &Expr,
    entity: &Entity,
    route_params: &[Param],
    pure_fns: &[PureFn],
    ta: &[TypeAlias],
) -> Option<String> {
    if let Expr::FnCall(name, _) = expr {
        if let Some(f) = pure_fns.iter().find(|f| f.name == *name) {
            return Some(gen_type(&resolve_type_with_aliases(&f.return_type, ta)));
        }
    }
    infer_expr_type(expr, entity, route_params)
        .map(|t| gen_type(&resolve_type_with_aliases(&t, ta)))
}

fn is_castable_numeric(ty: &str) -> bool {
    matches!(
        ty,
        "u8" | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
    )
}

pub(super) fn cast_return_values(vals: &[String], return_type: &Option<Type>) -> Vec<String> {
    match return_type {
        Some(Type::Simple(name)) if name == "String" && vals.len() == 1 => {
            vec![format!("({}).to_string()", vals[0])]
        }
        Some(Type::Simple(name)) if is_castable_numeric(name) && vals.len() == 1 => {
            vec![format!("(({}) as {})", vals[0], name)]
        }
        Some(Type::Tuple(elems)) if vals.len() == elems.len() => vals
            .iter()
            .zip(elems.iter())
            .map(|(v, t)| {
                let ty_str = gen_type(t);
                if is_castable_numeric(&ty_str) {
                    format!("(({}) as {})", v, ty_str)
                } else {
                    v.clone()
                }
            })
            .collect(),
        _ => vals.to_vec(),
    }
}

// ---------------------------------------------------------------------------
// Internal helpers for resolve_target_entity
// ---------------------------------------------------------------------------

fn extract_entity_from_method_call(dest: &Expr) -> Option<String> {
    match dest {
        Expr::MethodCall(receiver, method, _) if method == "address" => {
            if let Expr::Ident(name) = receiver.as_ref() {
                if name
                    .chars()
                    .next()
                    .map(|c| c.is_uppercase())
                    .unwrap_or(false)
                {
                    return Some(name.clone());
                }
            }
            None
        }
        // `addressOf(Entity.state(args))` — equivalent spelling to `Entity.address(args)`.
        Expr::FnCall(fn_name, args) if fn_name == "addressOf" && args.len() == 1 => {
            if let Expr::MethodCall(inner_recv, inner_method, _) = &args[0] {
                if inner_method == "state" {
                    if let Expr::Ident(name) = inner_recv.as_ref() {
                        if name
                            .chars()
                            .next()
                            .map(|c| c.is_uppercase())
                            .unwrap_or(false)
                        {
                            return Some(name.clone());
                        }
                    }
                }
            }
            None
        }
        // Test-only `address_of Entity(args)` keyword form — equivalent on EVM in det mode.
        Expr::AddressOf { entity_name, .. } => {
            if entity_name
                .chars()
                .next()
                .map(|c| c.is_uppercase())
                .unwrap_or(false)
            {
                Some(entity_name.clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn extract_sender_entity_from_where(route: &Route, entity: &Entity) -> Option<String> {
    for wc in &route.where_clauses {
        if let Some(name) = extract_sender_entity_from_expr(&wc.condition, entity) {
            return Some(name);
        }
    }
    None
}

fn extract_sender_entity_from_expr(expr: &Expr, entity: &Entity) -> Option<String> {
    match expr {
        Expr::BinOp(lhs, BinOp::Eq, rhs) => {
            if matches!(lhs.as_ref(), Expr::MsgField(f) if f == "sender") {
                return extract_entity_from_addr_or_macro(rhs, entity);
            }
            if matches!(rhs.as_ref(), Expr::MsgField(f) if f == "sender") {
                return extract_entity_from_addr_or_macro(lhs, entity);
            }
            None
        }
        Expr::BinOp(lhs, BinOp::And | BinOp::Or, rhs) => {
            extract_sender_entity_from_expr(lhs, entity)
                .or_else(|| extract_sender_entity_from_expr(rhs, entity))
        }
        _ => None,
    }
}

fn extract_entity_from_addr_or_macro(expr: &Expr, entity: &Entity) -> Option<String> {
    if let Some(name) = extract_entity_from_method_call(expr) {
        return Some(name);
    }
    if let Expr::MacroRef(macro_name, _) = expr {
        if let Some(mac) = entity.macros.iter().find(|m| m.name == *macro_name) {
            if let Type::TypedAddress(entity_name) = &mac.return_type {
                return Some(entity_name.clone());
            }
        }
    }
    None
}
