// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM codegen — Solidity type lowering, default values, let-binding stack.

use crate::ast::{Entity, Expr, Pattern, Route, Type};

use super::ctx::{EmitScope, EvmCtx};
use super::expr::gen_expr_hoisted;
use super::iter::pattern_idents;
use super::option::{
    is_option_sol_ty, option_none_from_sol_ty, option_struct_name, option_tag_name,
};
use super::scratch::EmitScratch;
use super::state::{is_payload_enum, payload_enum_zero_literal};
use std::cell::RefCell;

// ---------------------------------------------------------------------------
// Type helpers
// ---------------------------------------------------------------------------

fn evm_intrinsic_sol_return_ty(name: &str) -> Option<&'static str> {
    match name {
        "ecrecover" => Some("address"),
        "balance" | "blockhash" | "keccak256" | "keccak256Packed" | "sha256" | "ripemd160" => {
            Some("uint256")
        }
        _ => None,
    }
}

pub(crate) fn is_entity_record(entity: &Entity, name: &str) -> bool {
    entity.records.iter().any(|r| r.name == name)
}

pub(crate) fn is_entity_enum(entity: &Entity, name: &str) -> bool {
    entity.enums.iter().any(|e| e.name == name)
}

pub(crate) fn is_mapping_type(ty: &Type) -> bool {
    matches!(ty, Type::Generic(name, _) if name == "HashMap")
}

fn sol_ident_referenced(haystack: &str, ident: &str) -> bool {
    if ident.is_empty() {
        return false;
    }
    haystack.match_indices(ident).any(|(i, _)| {
        let before = haystack[..i].chars().next_back();
        let after = haystack[i + ident.len()..].chars().next();
        let ident_cont = |c: char| c.is_ascii_alphanumeric() || c == '_';
        !before.is_some_and(ident_cont) && !after.is_some_and(ident_cont)
    })
}

/// Storage params for a `HashMap` pure-fn argument, plus optional
/// `_exists` / `_keys` sidecars. A name that the lowered body never
/// reads is omitted so solc warning 5667 does not fail `deny = "warnings"`.
pub(crate) fn format_hashmap_storage_params(
    ty: &Type,
    p_name: &str,
    body_src: &str,
    with_exists: bool,
    with_keys: bool,
) -> Vec<String> {
    let mut params = Vec::new();
    let map_ty = sol_type(ty, false);
    if sol_ident_referenced(body_src, p_name) {
        params.push(format!("{map_ty} storage {p_name}"));
    } else {
        params.push(format!("{map_ty} storage"));
    }
    let Type::Generic(_, type_params) = ty else {
        return params;
    };
    let Some(k_ty) = type_params.first() else {
        return params;
    };
    let k_sol = sol_type(k_ty, false);
    if with_exists {
        let exists_name = format!("{p_name}_exists");
        if sol_ident_referenced(body_src, &exists_name) {
            params.push(format!("mapping({k_sol} => bool) storage {exists_name}"));
        } else {
            params.push(format!("mapping({k_sol} => bool) storage"));
        }
    }
    if with_keys {
        let keys_name = format!("{p_name}_keys");
        if sol_ident_referenced(body_src, &keys_name) {
            params.push(format!("{k_sol}[] storage {keys_name}"));
        } else {
            params.push(format!("{k_sol}[] storage"));
        }
    }
    params
}

/// Numeric scalar types (not `Vec` / `HashMap` / `Option` / records / tuples).
pub(crate) fn is_scalar_numeric_type(ty: &Type) -> bool {
    match ty {
        Type::Generic(g, _) => !matches!(g.as_str(), "Vec" | "HashMap" | "Option"),
        Type::Tuple(_) | Type::TypedAddress(_) => false,
        Type::Simple(s) => matches!(
            s.as_str(),
            "u8" | "u16"
                | "u32"
                | "u64"
                | "u128"
                | "U256"
                | "i8"
                | "i16"
                | "i32"
                | "i64"
                | "i128"
                | "I256"
                | "usize"
                | "uint8"
                | "uint16"
                | "uint32"
                | "uint64"
                | "uint128"
                | "uint256"
                | "int8"
                | "int16"
                | "int32"
                | "int64"
                | "int128"
                | "int256"
        ),
    }
}

/// When `member` is `HashMap<K1, HashMap<K2, V>>`, return `K2` for
/// `.keys()` / iteration over a nested inner slot `member[outer]`.
pub(crate) fn nested_hashmap_inner_key_type(
    entity: &Entity,
    member_name: &str,
) -> Option<Type> {
    let member = entity.members.iter().find(|m| m.name == member_name)?;
    if let Type::Generic(g, params) = &member.ty {
        if g == "HashMap" && params.len() == 2 {
            if let Type::Generic(g2, inner_params) = &params[1] {
                if g2 == "HashMap" && !inner_params.is_empty() {
                    return Some(inner_params[0].clone());
                }
            }
        }
    }
    None
}

fn sol_array_elem_to_cambrian_type(sol: &str) -> Option<Type> {
    match sol {
        "uint8" => Some(Type::Simple("u8".into())),
        "uint16" => Some(Type::Simple("u16".into())),
        "uint32" => Some(Type::Simple("u32".into())),
        "uint64" => Some(Type::Simple("u64".into())),
        "uint128" => Some(Type::Simple("u128".into())),
        "uint256" => Some(Type::Simple("U256".into())),
        _ => None,
    }
}

fn hashmap_value_type(ty: &Type) -> Option<Type> {
    match ty {
        Type::Generic(g, params) if g == "HashMap" && params.len() == 2 => Some(params[1].clone()),
        _ => None,
    }
}

/// Cambrian type of the mapping container reached by `expr` (member,
/// hashmap-alias ident, or `m[k]` / nested `m[a][b]`).
fn infer_expr_container_type(
    expr: &Expr,
    entity: &Entity,
    scratch: &RefCell<EmitScratch>,
) -> Option<Type> {
    match expr {
        Expr::Ident(name) => {
            if let Some(alias) = scratch.borrow().lookup_hashmap_alias(name) {
                return infer_expr_container_type(&alias, entity, scratch);
            }
            entity
                .members
                .iter()
                .find(|m| m.name == *name)
                .map(|m| m.ty.clone())
        }
        Expr::Index(base, _) => infer_expr_container_type(base, entity, scratch)
            .and_then(|t| hashmap_value_type(&t)),
        _ => None,
    }
}

/// Solidity type of a hashmap slot expression such as `m_nested[outer]`
/// (`mapping(K => V)` — no `memory` annotation).
pub(crate) fn infer_hashmap_slot_sol_ty(
    expr: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> Option<String> {
    infer_expr_container_type(expr, entity, scratch)
        .and_then(|t| hashmap_value_type(&t))
        .map(|inner| {
            if let Type::Generic(g, params) = &inner {
                if g == "HashMap" && params.len() == 2 {
                    return sol_type_entity(
                        entity,
                        &Type::Generic(g.clone(), params.clone()),
                        false,
                        ctx,
                    );
                }
            }
            sol_type_entity(entity, &inner, false, ctx)
        })
}

/// Result type of `container[key]` when `container` is a mapping slot.
pub(crate) fn infer_index_value_sol_ty(
    base: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> Option<String> {
    let resolved = match base {
        Expr::Ident(name) => scratch
            .borrow()
            .lookup_hashmap_alias(name)
            .unwrap_or_else(|| base.clone()),
        _ => base.clone(),
    };
    infer_expr_container_type(&resolved, entity, scratch)
        .and_then(|t| hashmap_value_type(&t))
        .map(|v| sol_type_entity(entity, &v, true, ctx))
}

/// Hoisted `match` temp type: prefer a non-`uint256` arm type, else
/// follow a narrow integral subject (e.g. `u64` route param).
pub(crate) fn infer_match_result_ty(
    subject: &Expr,
    arms: &[crate::ast::MatchArm],
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    for arm in arms {
        let t = infer_let_type_entity(&arm.body, entity, ctx, scratch);
        if t != "uint256" {
            return t;
        }
    }
    let subject_ty = infer_let_type_entity(subject, entity, ctx, scratch);
    if subject_ty.starts_with("uint") && subject_ty != "uint256" {
        return subject_ty;
    }
    if subject_ty == "bool" {
        return subject_ty;
    }
    "uint256".to_string()
}

/// Phase EVM-15 H5 (Cluster D): detect whether a route reads
/// `msg::value` anywhere in its reachable AST. The check is purely
/// syntactic: any `Expr::MsgField("value")` reached by walking the
/// route body, the `where` clauses, and every member transform that
/// fires for this route forces `payable` on the emitted Solidity
/// function.
pub(crate) fn route_uses_msg_value(route: &Route, entity: &Entity) -> bool {
    crate::analysis::route_uses_msg_value(route, entity)
}

/// Phase EVM-15 H4 (Cluster C): mangle Cambrian identifiers that
/// collide with Solidity reserved words / built-in type names. The
/// mangling is a single leading underscore — short enough to keep
/// generated code readable, distinctive enough to never clash with a
/// well-formed Cambrian identifier (Cambrian forbids leading
/// underscores in surface syntax).
///
/// Apply at:
///   1. parameter / return-binding declaration sites
///   2. local `let` binding sites
///   3. every identifier reference in expression lowering
///      (`Expr::Ident`, `Expr::TemporalRef`, etc.)
///
/// The function is idempotent: a sanitised name is itself a no-op on
/// the second pass (because the leading-underscore form isn't in the
/// reserved-words set).
pub(crate) fn sol_sanitize_ident(name: &str) -> String {
    // Solidity keywords and built-in type names that would shadow or
    // confuse the parser when used as a regular identifier. Limited
    // to the ones that have actually surfaced in fixtures + the
    // obvious additions; extend as we hit new collisions.
    matches!(
        name,
        // built-in type names
        "address" | "bool" | "bytes" | "string"
        | "int" | "int8" | "int16" | "int24" | "int32" | "int40" | "int48"
        | "int56" | "int64" | "int72" | "int80" | "int88" | "int96"
        | "int104" | "int112" | "int120" | "int128" | "int136" | "int144"
        | "int152" | "int160" | "int168" | "int176" | "int184" | "int192"
        | "int200" | "int208" | "int216" | "int224" | "int232" | "int240"
        | "int248" | "int256"
        | "uint" | "uint8" | "uint16" | "uint24" | "uint32" | "uint40" | "uint48"
        | "uint56" | "uint64" | "uint72" | "uint80" | "uint88" | "uint96"
        | "uint104" | "uint112" | "uint120" | "uint128" | "uint136" | "uint144"
        | "uint152" | "uint160" | "uint168" | "uint176" | "uint184" | "uint192"
        | "uint200" | "uint208" | "uint216" | "uint224" | "uint232" | "uint240"
        | "uint248" | "uint256"
        | "fixed" | "ufixed"
        // language keywords
        | "after" | "alias" | "apply" | "auto" | "byte" | "case" | "copyof"
        | "default" | "define" | "final" | "implements" | "in" | "inline"
        | "let" | "macro" | "match" | "mutable" | "null" | "of" | "partial"
        | "promise" | "reference" | "relocatable" | "sealed" | "sizeof"
        | "static" | "supports" | "switch" | "typedef" | "typeof" | "var"
        | "abstract" | "anonymous" | "as" | "assembly" | "break" | "calldata"
        | "catch" | "constant" | "constructor" | "continue" | "contract"
        | "delete" | "do" | "else" | "emit" | "enum" | "event" | "external"
        | "fallback" | "false" | "for" | "function" | "hex" | "if" | "immutable"
        | "import" | "indexed" | "interface" | "internal" | "is" | "library"
        | "mapping" | "memory" | "modifier" | "new" | "override" | "payable"
        | "pragma" | "private" | "public" | "pure" | "receive" | "return"
        | "returns" | "storage" | "struct" | "throw" | "true" | "try" | "type"
        | "unchecked" | "using" | "view" | "virtual" | "while"
        // pre-0.5 globals that still trip parsers in some contexts
        | "now" | "block" | "msg" | "tx" | "this" | "super" | "abi" | "keccak256"
        | "sha256" | "ripemd160" | "ecrecover" | "addmod" | "mulmod" | "selfdestruct"
        | "suicide" | "blockhash" | "gasleft" | "require" | "revert" | "assert"
    )
    .then(|| format!("_{}", name))
    .unwrap_or_else(|| name.to_string())
}

/// Phase EVM-15 H2 (Cluster A): return a Solidity literal that is
/// type-correct as the default value for `ty` when the Cambrian
/// source said `none` / `{}` / `[]`. `gen_expr` lowers all three to
/// the bare string `"0"`, which is fine for value types but fails
/// solc compilation for `string` / `bytes` / structs / arrays —
/// hence this thin lookup.
pub(crate) fn solidity_default_for_member_ty(entity: &Entity, ty: &Type, ctx: &EvmCtx) -> String {
    match ty {
        Type::Simple(name) => match name.as_str() {
            "String" => "\"\"".to_string(),
            "CamData" | "bytes" => "\"\"".to_string(),
            "address" => "address(0)".to_string(),
            "bool" => "false".to_string(),
            _ => {
                // Custom record names lower to `<Name> memory` — no
                // shorthand "default", construct an empty struct
                // literal (every field zero-init).
                if is_entity_record(entity, name) || ctx.is_program_record(name) {
                    if let Some(rec) = entity
                        .records
                        .iter()
                        .find(|r| r.name == *name)
                        .or_else(|| ctx.lookup_record(name))
                    {
                        let zeros: Vec<String> = rec
                            .fields
                            .iter()
                            .map(|f| {
                                let nested = solidity_default_for_member_ty(entity, &f.ty, ctx);
                                format!("{}: {}", f.name, nested)
                            })
                            .collect();
                        return format!("{}({{{}}})", name, zeros.join(", "));
                    }
                }
                // Phase EVM-4 J1: payload enums lower to a tagged-union
                // struct; default = first variant with all payload
                // fields zero-initialised.
                if let Some(decl) = ctx.lookup_enum(name) {
                    if is_payload_enum(&decl) {
                        return payload_enum_zero_literal(entity, &decl, ctx);
                    }
                }
                "0".to_string()
            }
        },
        Type::Generic(name, params) => match name.as_str() {
            "Vec" if !params.is_empty() => {
                let inner = sol_type_entity(entity, &params[0], false, ctx);
                format!("new {}[](0)", inner)
            }
            "Option" if !params.is_empty() => {
                let name = option_struct_name(&params[0]);
                let tag = option_tag_name(&params[0]);
                let payload = solidity_default_for_member_ty(entity, &params[0], ctx);
                format!("{name}({{ tag: {tag}.None, some_0: {payload} }})")
            }
            _ => "0".to_string(),
        },
        Type::Tuple(_) | Type::TypedAddress(_) => "0".to_string(),
    }
}

/// EVM-3 Batch G2: does the expression evaluate to a HashMap value
/// (i.e. a Solidity `mapping(...)`)? Solidity disallows mappings as
/// local variables, so any `let x = <hashmap-typed>` binding has to
/// be inlined at codegen time. Recognises:
///
///   * a bare entity-member `Ident` of HashMap type,
///   * an `Index` into a HashMap entity member whose value type is
///     itself a HashMap (nested-mapping access),
///   * an `if cond { hashmap-typed } else { hashmap-typed }` whose
///     then-branch is HashMap-typed (the `else { {} }` branch is the
///     idiomatic boilerplate around `m.exists(k)`),
///   * `Expr::EmptyCollection` *only* when paired with a HashMap
///     context — we don't have type info here, so the caller is the
///     conditional check that already saw a HashMap then-branch.
fn is_nested_or_leaf_hashmap_value(ty: &Type) -> bool {
    matches!(ty, Type::Generic(g, _) if g == "HashMap") || is_mapping_type(ty)
}

/// Tuple binding whose element is a mapping slot (e.g. `(inner, k)`
/// after HashMap-let substitution) cannot lower to a Solidity local.
pub(crate) fn tuple_contains_hashmap_slot(elems: &[Expr], entity: &Entity) -> bool {
    elems.iter().any(|e| is_hashmap_valued_expr(e, entity))
}

pub(crate) fn is_hashmap_valued_expr(expr: &Expr, entity: &Entity) -> bool {
    match expr {
        Expr::Ident(name) => entity
            .members
            .iter()
            .find(|m| m.name == *name)
            .map_or(false, |m| is_mapping_type(&m.ty)),
        Expr::Index(base, _) => {
            if let Expr::Ident(name) = base.as_ref() {
                if let Some(m) = entity.members.iter().find(|m| m.name == *name) {
                    if let Type::Generic(g, params) = &m.ty {
                        if g == "HashMap" && params.len() == 2 {
                            return is_nested_or_leaf_hashmap_value(&params[1]);
                        }
                    }
                }
            }
            false
        }
        Expr::If(_, then, _) => is_hashmap_valued_expr(then, entity),
        _ => false,
    }
}

/// EVM-3 Batch G2: substitute every free occurrence of `Ident(name)`
/// in `expr` with `replacement`. Closure parameters and inner `let`
/// bindings that re-bind `name` shadow the substitution.
pub(crate) fn subst_ident_in_expr(expr: &Expr, name: &str, replacement: &Expr) -> Expr {
    subst_expr_where(expr, &|e| match e {
        Expr::Ident(x) if x == name => Some(replacement.clone()),
        _ => None,
    })
}

/// Rewrite `expr` bottom-up, replacing any subexpression for which
/// `pick` returns `Some(replacement)`.
///
/// `pick` is consulted before descending, so a match short-circuits the
/// subtree — which is what a snapshot wants: once `m_balances[this]` has
/// been captured into a local, nothing inside it needs rewriting.
///
/// Binder-aware in the same way `subst_ident_in_expr` always was:
/// `let` / closure / `for` patterns that rebind a name stop
/// substitution inside their body. The check is by ident, so a `pick`
/// keyed on a compound expression is only shadowed when one of the
/// idents it mentions is rebound — conservative in the safe direction.
pub(crate) fn subst_expr_where(expr: &Expr, pick: &dyn Fn(&Expr) -> Option<Expr>) -> Expr {
    fn shadows(idents: &[String], pick: &dyn Fn(&Expr) -> Option<Expr>) -> bool {
        idents
            .iter()
            .any(|i| pick(&Expr::Ident(i.clone())).is_some())
    }

    fn rec(e: &Expr, pick: &dyn Fn(&Expr) -> Option<Expr>) -> Expr {
        if let Some(replacement) = pick(e) {
            return replacement;
        }
        match e {
            Expr::TemporalRef(_)
            | Expr::IntLiteral(_)
            | Expr::BoolLiteral(_)
           
           
            | Expr::StringLiteral(_)
            | Expr::BytesLiteral(_)
            | Expr::None
            | Expr::EmptyCollection
            | Expr::MsgField(_)
            | Expr::SysField(_)
            | Expr::TraceField(_)
            | Expr::TraceCall { .. }
            | Expr::Ident(_)
            | Expr::EnumVariant(_, _) => e.clone(),
            Expr::BinOp(l, op, r2) => {
                Expr::BinOp(Box::new(rec(l, pick)), op.clone(), Box::new(rec(r2, pick)))
            }
            Expr::UnaryOp(op, x) => Expr::UnaryOp(op.clone(), Box::new(rec(x, pick))),
            Expr::FieldAccess(b, f) => Expr::FieldAccess(Box::new(rec(b, pick)), f.clone()),
            Expr::Index(b, k) => Expr::Index(Box::new(rec(b, pick)), Box::new(rec(k, pick))),
            Expr::FnCall(name, args) => Expr::FnCall(
                name.clone(),
                args.iter().map(|a| rec(a, pick)).collect(),
            ),
            Expr::MacroRef(name, args) => Expr::MacroRef(
                name.clone(),
                args.iter().map(|a| rec(a, pick)).collect(),
            ),
            Expr::MethodCall(b, m, args) => Expr::MethodCall(
                Box::new(rec(b, pick)),
                m.clone(),
                args.iter().map(|a| rec(a, pick)).collect(),
            ),
            Expr::If(c, t, el) => Expr::If(
                Box::new(rec(c, pick)),
                Box::new(rec(t, pick)),
                el.as_ref().map(|x| Box::new(rec(x, pick))),
            ),
            Expr::Let(p, v, b) => {
                // Inner `let n = ...` shadows the outer binding; stop
                // substituting inside its body in that case.
                let shadowed = shadows(&pattern_idents(p), pick);
                Expr::Let(
                    p.clone(),
                    Box::new(rec(v, pick)),
                    Box::new(if shadowed { (**b).clone() } else { rec(b, pick) }),
                )
            }
            Expr::Block(items) => Expr::Block(items.iter().map(|i| rec(i, pick)).collect()),
            Expr::ArrayLit(items) => Expr::ArrayLit(items.iter().map(|i| rec(i, pick)).collect()),
            Expr::Cast(x, t) => Expr::Cast(Box::new(rec(x, pick)), t.clone()),
            Expr::Some(x) => Expr::Some(Box::new(rec(x, pick))),
            Expr::Range(s, ee) => Expr::Range(Box::new(rec(s, pick)), Box::new(rec(ee, pick))),
            Expr::Closure(params, body) => {
                let shadowed = params.iter().any(|p| shadows(&pattern_idents(p), pick));
                Expr::Closure(
                    params.clone(),
                    Box::new(if shadowed { (**body).clone() } else { rec(body, pick) }),
                )
            }
            Expr::For(p, it, body) => {
                let shadowed = shadows(&pattern_idents(p), pick);
                Expr::For(
                    p.clone(),
                    Box::new(rec(it, pick)),
                    Box::new(if shadowed { (**body).clone() } else { rec(body, pick) }),
                )
            }
            Expr::Tuple(items) => Expr::Tuple(items.iter().map(|i| rec(i, pick)).collect()),
            Expr::RecordConstruct(name, fields) => Expr::RecordConstruct(
                name.clone(),
                fields.iter().map(|(f, e)| (f.clone(), rec(e, pick))).collect(),
            ),
            Expr::RecordUpdate(b, fields) => Expr::RecordUpdate(
                Box::new(rec(b, pick)),
                fields.iter().map(|(f, e)| (f.clone(), rec(e, pick))).collect(),
            ),
            Expr::Match(s, arms) => Expr::Match(
                Box::new(rec(s, pick)),
                arms.iter()
                    .map(|a| crate::ast::MatchArm {
                        pattern: a.pattern.clone(),
                        body: rec(&a.body, pick),
                    })
                    .collect(),
            ),
            Expr::EnumVariantWithData(en, v, args) => Expr::EnumVariantWithData(
                en.clone(),
                v.clone(),
                args.iter().map(|a| rec(a, pick)).collect(),
            ),
            Expr::NamespacedCall {
                namespace,
                name,
                type_params,
                args,
            } => Expr::NamespacedCall {
                namespace: namespace.clone(),
                name: name.clone(),
                type_params: type_params.clone(),
                args: args.iter().map(|a| rec(a, pick)).collect(),
            },
            Expr::AddressOf {
                entity_name,
                args,
                with_params,
            } => Expr::AddressOf {
                entity_name: entity_name.clone(),
                args: args.iter().map(|a| rec(a, pick)).collect(),
                with_params: with_params
                    .iter()
                    .map(|(f, e)| (f.clone(), rec(e, pick)))
                    .collect(),
            },
            Expr::Encode { target_type, value } => Expr::Encode {
                target_type: target_type.clone(),
                value: Box::new(rec(value, pick)),
            },
        }
    }
    rec(expr, pick)
}

fn pattern_binds_name(pat: &Pattern, name: &str) -> bool {
    match pat {
        Pattern::Ident(n) => n == name,
        Pattern::Wildcard | Pattern::None => false,
        Pattern::Tuple(items) => items.iter().any(|p| pattern_binds_name(p, name)),
        Pattern::Deref(inner) | Pattern::Some(inner) => pattern_binds_name(inner, name),
    }
}

fn is_hashmap_access_method(method: &str) -> bool {
    matches!(
        method,
        "exists" | "contains" | "keys" | "values" | "insert" | "update" | "remove" | "is_empty"
            | "iter" | "len"
    )
}

/// Like [`subst_ident_in_expr`], but when a closure parameter re-binds
/// `name` only *mapping-shaped* uses (`inner.exists`, `inner[k]`, …)
/// keep the HashMap substitution; bare `inner` in the closure body still
/// refers to the closure parameter (e.g. a `.fold` accumulator).
pub(crate) fn subst_hashmap_let_in_expr(expr: &Expr, name: &str, replacement: &Expr) -> Expr {
    fn rec(e: &Expr, n: &str, r: &Expr, closure_shadow: bool) -> Expr {
        match e {
            Expr::Ident(x) if x == n && !closure_shadow => r.clone(),
            Expr::TemporalRef(_)
            | Expr::IntLiteral(_)
            | Expr::BoolLiteral(_)
           
           
            | Expr::StringLiteral(_)
            | Expr::BytesLiteral(_)
            | Expr::None
            | Expr::EmptyCollection
            | Expr::MsgField(_)
            | Expr::SysField(_)
            | Expr::TraceField(_)
            | Expr::TraceCall { .. }
            | Expr::Ident(_)
            | Expr::EnumVariant(_, _) => e.clone(),
            Expr::BinOp(l, op, r2) => Expr::BinOp(
                Box::new(rec(l, n, r, closure_shadow)),
                op.clone(),
                Box::new(rec(r2, n, r, closure_shadow)),
            ),
            Expr::UnaryOp(op, x) => {
                Expr::UnaryOp(op.clone(), Box::new(rec(x, n, r, closure_shadow)))
            }
            Expr::FieldAccess(b, f) => {
                Expr::FieldAccess(Box::new(rec(b, n, r, closure_shadow)), f.clone())
            }
            Expr::Index(b, k) => {
                let base = if closure_shadow {
                    if let Expr::Ident(x) = b.as_ref() {
                        if x == n {
                            r.clone()
                        } else {
                            rec(b, n, r, closure_shadow)
                        }
                    } else {
                        rec(b, n, r, closure_shadow)
                    }
                } else {
                    rec(b, n, r, closure_shadow)
                };
                Expr::Index(
                    Box::new(base),
                    Box::new(rec(k, n, r, closure_shadow)),
                )
            }
            Expr::FnCall(fname, args) => Expr::FnCall(
                fname.clone(),
                args.iter().map(|a| rec(a, n, r, closure_shadow)).collect(),
            ),
            Expr::MacroRef(fname, args) => Expr::MacroRef(
                fname.clone(),
                args.iter().map(|a| rec(a, n, r, closure_shadow)).collect(),
            ),
            Expr::MethodCall(b, method, args) => {
                let base = if closure_shadow && is_hashmap_access_method(method) {
                    if let Expr::Ident(x) = b.as_ref() {
                        if x == n {
                            r.clone()
                        } else {
                            rec(b, n, r, closure_shadow)
                        }
                    } else {
                        rec(b, n, r, closure_shadow)
                    }
                } else {
                    rec(b, n, r, closure_shadow)
                };
                Expr::MethodCall(
                    Box::new(base),
                    method.clone(),
                    args.iter().map(|a| rec(a, n, r, closure_shadow)).collect(),
                )
            }
            Expr::If(c, t, el) => Expr::If(
                Box::new(rec(c, n, r, closure_shadow)),
                Box::new(rec(t, n, r, closure_shadow)),
                el.as_ref()
                    .map(|x| Box::new(rec(x, n, r, closure_shadow))),
            ),
            Expr::Let(p, v, b) => {
                let shadowed = pattern_binds_name(p, n);
                Expr::Let(
                    p.clone(),
                    Box::new(rec(v, n, r, closure_shadow)),
                    Box::new(if shadowed {
                        (**b).clone()
                    } else {
                        rec(b, n, r, closure_shadow)
                    }),
                )
            }
            Expr::Block(items) => Expr::Block(
                items.iter().map(|i| rec(i, n, r, closure_shadow)).collect(),
            ),
            Expr::ArrayLit(items) => Expr::ArrayLit(
                items.iter().map(|i| rec(i, n, r, closure_shadow)).collect(),
            ),
            Expr::Cast(x, t) => Expr::Cast(Box::new(rec(x, n, r, closure_shadow)), t.clone()),
            Expr::Some(x) => Expr::Some(Box::new(rec(x, n, r, closure_shadow))),
            Expr::Range(s, ee) => Expr::Range(
                Box::new(rec(s, n, r, closure_shadow)),
                Box::new(rec(ee, n, r, closure_shadow)),
            ),
            Expr::Closure(params, body) => {
                let shadowed = params.iter().any(|p| pattern_binds_name(p, n));
                Expr::Closure(
                    params.clone(),
                    Box::new(rec(body, n, r, closure_shadow || shadowed)),
                )
            }
            Expr::For(p, it, body) => {
                let shadowed = pattern_binds_name(p, n);
                Expr::For(
                    p.clone(),
                    Box::new(rec(it, n, r, closure_shadow)),
                    Box::new(if shadowed {
                        (**body).clone()
                    } else {
                        rec(body, n, r, closure_shadow)
                    }),
                )
            }
            Expr::Tuple(items) => Expr::Tuple(
                items.iter().map(|i| rec(i, n, r, closure_shadow)).collect(),
            ),
            Expr::RecordConstruct(rname, fields) => Expr::RecordConstruct(
                rname.clone(),
                fields
                    .iter()
                    .map(|(f, e)| (f.clone(), rec(e, n, r, closure_shadow)))
                    .collect(),
            ),
            Expr::RecordUpdate(b, fields) => Expr::RecordUpdate(
                Box::new(rec(b, n, r, closure_shadow)),
                fields
                    .iter()
                    .map(|(f, e)| (f.clone(), rec(e, n, r, closure_shadow)))
                    .collect(),
            ),
            Expr::Match(s, arms) => Expr::Match(
                Box::new(rec(s, n, r, closure_shadow)),
                arms.iter()
                    .map(|a| crate::ast::MatchArm {
                        pattern: a.pattern.clone(),
                        body: rec(&a.body, n, r, closure_shadow),
                    })
                    .collect(),
            ),
            Expr::EnumVariantWithData(en, v, args) => Expr::EnumVariantWithData(
                en.clone(),
                v.clone(),
                args.iter().map(|a| rec(a, n, r, closure_shadow)).collect(),
            ),
            Expr::NamespacedCall {
                namespace,
                name: fname,
                type_params,
                args,
            } => Expr::NamespacedCall {
                namespace: namespace.clone(),
                name: fname.clone(),
                type_params: type_params.clone(),
                args: args.iter().map(|a| rec(a, n, r, closure_shadow)).collect(),
            },
            Expr::AddressOf {
                entity_name,
                args,
                with_params,
            } => Expr::AddressOf {
                entity_name: entity_name.clone(),
                args: args.iter().map(|a| rec(a, n, r, closure_shadow)).collect(),
                with_params: with_params
                    .iter()
                    .map(|(f, e)| (f.clone(), rec(e, n, r, closure_shadow)))
                    .collect(),
            },
            Expr::Encode { target_type, value } => Expr::Encode {
                target_type: target_type.clone(),
                value: Box::new(rec(value, n, r, closure_shadow)),
            },
        }
    }
    rec(expr, name, replacement, false)
}

/// EVM-3 Batch G2: collapse the boilerplate `if m.exists(k) { m[k] }
/// else { {} }` shape that surrounds many Cambrian HashMap
/// let-bindings down to the underlying `m[k]`. Solidity mappings
/// already return a zero-default for missing keys, so the `else { {} }`
/// branch is a no-op on EVM. Returns `Some(simplified)` only when the
/// shape matches exactly; callers that don't match keep the original
/// expression and rely on the let-substitution path or other lowering
/// rules.
pub(crate) fn simplify_hashmap_let_alias_init(val: &Expr) -> Option<Expr> {
    if let Expr::If(cond, then, Some(els)) = val {
        if let Expr::MethodCall(base, method, args) = cond.as_ref() {
            if method == "exists" && args.len() == 1 {
                if let (Expr::Ident(_), Expr::Index(_, _)) = (base.as_ref(), then.as_ref()) {
                    if matches!(els.as_ref(), Expr::EmptyCollection) {
                        return Some((**then).clone());
                    }
                }
            }
        }
    }
    None
}

pub(crate) fn is_vec_type(ty: &Type) -> bool {
    matches!(ty, Type::Generic(name, params) if name == "Vec" && params.len() == 1)
}

/// Whether `array()` on a storage `Vec<T>` member needs an explicit write.
/// Composite elements (records / enums) cannot be copied from a `T[] memory`
/// temp into storage; an empty dynamic storage array is already correct.
pub(crate) fn vec_empty_transform_needs_write(
    entity: &Entity,
    ty: &Type,
    ctx: &EvmCtx,
) -> bool {
    let Type::Generic(name, params) = ty else {
        return true;
    };
    if name != "Vec" || params.is_empty() {
        return true;
    }
    match &params[0] {
        Type::Simple(inner) => {
            !(is_entity_record(entity, inner)
                || ctx.is_program_record(inner)
                || is_entity_enum(entity, inner)
                || ctx.is_program_enum(inner))
        }
        _ => true,
    }
}

/// G-U7: render a Cambrian hex literal as a Solidity expression. Hex
/// literals shaped like an address — exactly 40 hex digits, or exactly
/// 64 hex digits with the high 24 bytes (top 48 nibbles) zero — are
/// wrapped in `address(uint160(uint256(...)))` so they typecheck against
/// Default Solidity literal for a rendered type string (`bool`, `address`, …).
pub(crate) fn default_sol_literal(ty: &str) -> String {
    let base = ty.split_whitespace().next().unwrap_or(ty);
    match base {
        "bool" => "false".to_string(),
        "address" => "address(0)".to_string(),
        "string" => "\"\"".to_string(),
        "bytes" => "\"\"".to_string(),
        _ if base.starts_with("bytes") && base != "bytes" => format!("{}(0)", base),
        _ if base.starts_with("Option_") => super::option::option_none_from_sol_name(base),
        _ => "0".to_string(),
    }
}

/// `address` storage slots and `address` parameters. The redundant
/// `uint256(...)` cast is what stops Solidity 0.8.x from refusing
/// 40-nibble literals as "addresses with invalid checksum"; treating
/// them as uint256 first keeps fixture authors free to spell test
/// addresses with arbitrary case. Other hex literals pass through
/// verbatim as `0x{h}` (Solidity infers them as uint256).
///
/// When `expected_ty` is `bytes32` / `uint*`, address-shaped hex is
/// *not* wrapped as `address` (EVM-H17).
pub(crate) fn hex_literal_to_sol_for_ty(h: &str, expected_ty: Option<&str>) -> String {
    fn is_address_shaped(h: &str) -> bool {
        if h.len() == 40 {
            return true;
        }
        if h.len() == 64 {
            return h.as_bytes()[..48].iter().all(|c| *c == b'0');
        }
        false
    }
    if !is_address_shaped(h) {
        return format!("0x{}", h);
    }
    let ty = expected_ty.unwrap_or("address");
    let base = ty.split_whitespace().next().unwrap_or(ty);
    if base.starts_with("bytes32") {
        format!("bytes32(uint256(0x{:0>64}))", h)
    } else if base.starts_with("uint") {
        format!("uint256(0x{:0>64})", h)
    } else if base == "address" {
        format!("address(uint160(uint256(0x{:0>64})))", h)
    } else {
        format!("uint256(0x{:0>64})", h)
    }
}

/// Widen two branch result types for `if` / ternary hoisting.
pub(crate) fn merge_sol_branch_types(a: &str, b: &str) -> String {
    if a == b {
        return a.to_string();
    }
    if a == "uint256" || b == "uint256" {
        return "uint256".to_string();
    }
    if a == "address" || b == "address" {
        return "address".to_string();
    }
    if a == "bool" || b == "bool" {
        return "bool".to_string();
    }
    let parse_uint = |ty: &str| -> Option<u16> {
        ty.strip_prefix("uint")
            .and_then(|n| n.parse().ok())
    };
    match (parse_uint(a), parse_uint(b)) {
        (Some(wa), Some(wb)) => format!("uint{}", wa.max(wb)),
        _ => a.to_string(),
    }
}

/// Solidity type for an untyped integer literal (never implicit `address`).
pub(crate) fn infer_int_literal_sol_ty(v: &cambrian_core::U256) -> String {
    if !v.fits_u128() {
        "uint256".to_string()
    } else if v.lo > u128::from(u64::MAX) {
        "uint128".to_string()
    } else {
        "uint64".to_string()
    }
}

/// G-U8: returns true iff the transform body (after any block-shedding)
/// introduces a `let` binding. Used by `emit_transforms_sol_split` to decide
/// whether to wrap the transform's emitted statements in their own
/// `{ ... }` scope so per-transform `let` names cannot collide.
pub(crate) fn transform_has_let(body: &Expr) -> bool {
    fn walk(e: &Expr) -> bool {
        match e {
            Expr::Let(_, _, _) => true,
            Expr::Block(items) => items.iter().any(walk),
            Expr::If(_, t, el) => walk(t) || el.as_ref().map_or(false, |x| walk(x)),
            Expr::Match(_, arms) => arms.iter().any(|a| walk(&a.body)),
            Expr::For(_, _, b) => walk(b),
            _ => false,
        }
    }
    walk(body)
}

/// G-U3 / G-U8: lower a transform body that *terminates* in
/// `m_xyz.push(arg)` against the named `Vec<T>` member. The body may
/// be wrapped in a `let`-chain (BlockBody lowers `{ let a = …; … }`
/// to nested `Expr::Let`s) so that callers can name intermediate
/// values with their own per-transform scope. Returns the prelude
/// statements (`let` declarations plus any hoisted setup from
/// `gen_expr_hoisted`) and the rendered push-argument string.
///
/// Anything else returns `None`; the caller keeps falling through to
/// the generic transform pipeline so the existing error path is
/// preserved when `.push` is used in non-statement position.
/// Statement-shaped transform that only pushes into a *different* `Vec`
/// member (e.g. `m_vals` transform body `{ m_shadow.push(1) }`).
pub(crate) fn lower_foreign_vec_push_body(
    body: &Expr,
    member_name: &str,
    entity: &Entity,
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> Option<Vec<String>> {
    fn find_push(expr: &Expr) -> Option<(&str, &Expr)> {
        match expr {
            Expr::Block(items) => items.iter().find_map(find_push),
            Expr::Let(_, _, inner) => find_push(inner),
            Expr::MethodCall(base, method, args) if method == "push" && args.len() == 1 => {
                if let Expr::Ident(name) = base.as_ref() {
                    Some((name.as_str(), &args[0]))
                } else {
                    None
                }
            }
            _ => None,
        }
    }
    let (push_member, arg_expr) = find_push(body)?;
    if push_member == member_name {
        return None;
    }
    if !entity
        .members
        .iter()
        .any(|m| m.name == push_member && is_vec_type(&m.ty))
    {
        return None;
    }
    let (mut stmts, arg_str) = gen_expr_hoisted(arg_expr, entity, ctx, scope, scratch);
    stmts.push(format!("{}.push({});", push_member, arg_str));
    Some(stmts)
}

pub(crate) fn lower_member_push_body(
    body: &Expr,
    member_name: &str,
    entity: &Entity,
    ctx: &EvmCtx,
    scope: &EmitScope,
    scratch: &RefCell<EmitScratch>,
) -> Option<(Vec<String>, String)> {
    let mut cursor = body;
    let mut stmts: Vec<String> = Vec::new();
    // Track our own pushed bindings so the iterative loop pops them
    // symmetrically before returning (preserves LET_BINDING_TYPES
    // hygiene across calls).
    let mut pushed: Vec<String> = Vec::new();
    let result = loop {
        match cursor {
            Expr::Let(Pattern::Ident(name), val, inner) => {
                let (val_stmts, val_str) = gen_expr_hoisted(val, entity, ctx, scope, scratch);
                let let_ty = infer_let_type_entity(val, entity, ctx, scratch);
                stmts.extend(val_stmts);
                stmts.push(format!(
                    "{} {} = {};",
                    let_ty,
                    sol_sanitize_ident(name),
                    val_str
                ));
                scratch.borrow_mut().push_let_binding(name, &let_ty);
                pushed.push(name.clone());
                cursor = inner;
            }
            Expr::Let(Pattern::Wildcard, val, inner) => {
                let (val_stmts, _) = gen_expr_hoisted(val, entity, ctx, scope, scratch);
                stmts.extend(val_stmts);
                cursor = inner;
            }
            Expr::MethodCall(base, method, args) if method == "push" && args.len() == 1 => {
                if let Expr::Ident(name) = base.as_ref() {
                    if name == member_name {
                        let (arg_stmts, arg_str) =
                            gen_expr_hoisted(&args[0], entity, ctx, scope, scratch);
                        stmts.extend(arg_stmts);
                        break Some((stmts, arg_str));
                    }
                }
                break None;
            }
            _ => break None,
        }
    };
    for n in pushed.iter().rev() {
        scratch.borrow_mut().pop_let_binding(n);
    }
    result
}

pub(crate) fn sol_type(ty: &Type, in_param: bool) -> String {
    match ty {
        Type::Simple(name) => match name.as_str() {
            // Phase EVM-P0-B (closes EVM_GAPS § 2.1.7): narrow integer
            // survival. Pre-P0-B every Cambrian narrow int (`u8` …
            // `u128`) widened to `uint256`, defeating Solidity's
            // storage-slot packing and forcing call sites to spend
            // 32 bytes on values that only need 1-16 bytes. Now we
            // map to the matching `uintN` directly. Solidity's
            // implicit literal conversion still lets call sites
            // write bare integer literals (e.g. `m_count + 1`)
            // without explicit casts. `usize` keeps its `uint256`
            // mapping (it's a pseudo-pointer / element-count type
            // and would mis-pack at narrow widths). `U256` keeps
            // `uint256` by name.
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
            "String" => {
                if in_param {
                    "string memory".to_string()
                } else {
                    "string".to_string()
                }
            }
            "CamData" | "bytes" => {
                if in_param {
                    "bytes memory".to_string()
                } else {
                    "bytes".to_string()
                }
            }
            "address" => "address".to_string(),
            "pubkey" | "bytes32" => "bytes32".to_string(),
            "bytes4" => "bytes4".to_string(),
            _ => "uint256".to_string(),
        },
        Type::Generic(name, params) => {
            if name == "Vec" && params.len() == 1 {
                let base = sol_type(&params[0], false);
                if in_param {
                    format!("{}[] memory", base)
                } else {
                    format!("{}[]", base)
                }
            } else if name == "HashMap" && params.len() == 2 {
                format!(
                    "mapping({} => {})",
                    sol_type(&params[0], false),
                    sol_type(&params[1], false)
                )
            } else if name == "Option" && params.len() == 1 {
                let n = option_struct_name(&params[0]);
                if in_param {
                    format!("{} memory", n)
                } else {
                    n
                }
            } else {
                "bytes".to_string()
            }
        }
        Type::Tuple(_) => "bytes".to_string(),
        Type::TypedAddress(_) => "address".to_string(),
    }
}

pub(crate) fn sol_type_entity(entity: &Entity, ty: &Type, in_param: bool, ctx: &EvmCtx) -> String {
    match ty {
        Type::Simple(name) => {
            // Entity-local and program-scope records both lower to the
            // Solidity struct name (PM-002). Without the program-record
            // arm, `sol_type` erases unknown names to `uint256`.
            if is_entity_record(entity, name) || ctx.is_program_record(name) {
                if in_param {
                    format!("{} memory", name)
                } else {
                    name.clone()
                }
            } else if is_entity_enum(entity, name) || ctx.is_program_enum(name) {
                // Phase EVM-4 J1: payload enums lower to a Solidity
                // `struct`, which requires `memory` annotation in
                // parameter / local positions just like records.
                if in_param && ctx.is_payload_enum_named(name) {
                    format!("{} memory", name)
                } else {
                    name.clone()
                }
            } else {
                sol_type(ty, in_param)
            }
        }
        Type::Generic(name, params) if name == "Vec" && params.len() == 1 => {
            if let Type::Simple(inner) = &params[0] {
                if is_entity_record(entity, inner)
                    || ctx.is_program_record(inner)
                    || is_entity_enum(entity, inner)
                    || ctx.is_program_enum(inner)
                {
                    return if in_param {
                        format!("{}[] memory", inner)
                    } else {
                        format!("{}[]", inner)
                    };
                }
            }
            sol_type(ty, in_param)
        }
        // Entity-aware HashMap lowering: when the value (or key) type is an
        // entity-local record/enum, sol_type() would fall through to the
        // catch-all `_ => "uint256"` and lose the type information. Resolve
        // both halves through sol_type_entity so e.g. `HashMap<U256, Status>`
        // becomes `mapping(uint256 => Status)`.
        Type::Generic(name, params) if name == "HashMap" && params.len() == 2 => {
            let k = sol_type_entity(entity, &params[0], false, ctx);
            let v = sol_type_entity(entity, &params[1], false, ctx);
            format!("mapping({} => {})", k, v)
        }
        Type::Generic(name, params) if name == "Option" && params.len() == 1 => {
            let n = option_struct_name(&params[0]);
            if in_param {
                format!("{} memory", n)
            } else {
                n
            }
        }
        _ => sol_type(ty, in_param),
    }
}

/// Phase EVM-P0-B follow-up: when a value flows into a *narrow* integer
/// position (`uintN` / `intN` for N < 256), Solidity rejects implicit
/// conversion from `uint256`. The transpiler can't always prove the
/// expression is already narrow (stdlib helpers return `uint256`,
/// `block.timestamp` is `uint256`, `match`/`if` arms widen, etc.), so
/// we wrap the rendered Solidity expression with an explicit
/// `uintN(...)` / `intN(...)` cast in those cases. Solidity accepts
/// redundant casts (`uint64(uint64(x))`) but for cleaner output we
/// skip the wrap when an `actual_ty` hint matches the target.
///
/// Returns `expr` unchanged when:
///   * the target Solidity type is `uint256` / `int256` / non-numeric;
///   * the expression is empty;
///   * the expression already begins with the same explicit cast.
///
/// Composite container types (`uintN[]`, `mapping(...)`, struct names)
/// are intentionally not wrapped — Solidity has no scalar-cast for
/// arrays / mappings, and structs/enums are nominal.
pub(crate) fn wrap_narrow_cast(target_sol_ty: &str, expr: &str) -> String {
    wrap_narrow_cast_with_actual(target_sol_ty, expr, None)
}

pub(crate) fn wrap_narrow_cast_with_actual(
    target_sol_ty: &str,
    expr: &str,
    actual_ty: Option<&str>,
) -> String {
    if expr.is_empty() {
        return expr.to_string();
    }
    let _actual = actual_ty.unwrap_or("");
    if actual_ty == Some("bool") {
        if let Some(prim) = target_sol_ty.strip_prefix("uint") {
            if prim.chars().all(|c| c.is_ascii_digit()) || target_sol_ty == "uint256" {
                return format!(
                    "({} ? {}(1) : {}(0))",
                    expr,
                    target_sol_ty,
                    target_sol_ty
                );
            }
        }
    }
    // Strict types: no implicit string↔numeric coercion (docs/STDLIB.md).
    let prim = match target_sol_ty {
        "uint8" | "uint16" | "uint24" | "uint32" | "uint40" | "uint48" | "uint56" | "uint64"
        | "uint72" | "uint80" | "uint88" | "uint96" | "uint104" | "uint112" | "uint120"
        | "uint128" | "int8" | "int16" | "int24" | "int32" | "int40" | "int48" | "int56"
        | "int64" | "int72" | "int80" | "int88" | "int96" | "int104" | "int112" | "int120"
        | "int128" => target_sol_ty,
        _ => return expr.to_string(),
    };
    if let Some(actual) = actual_ty {
        if actual == prim {
            return expr.to_string();
        }
    }
    if prim.starts_with("int") {
        let inner = if actual_ty == Some("uint256") || expr.starts_with("uint256(") {
            format!("int256({})", expr)
        } else {
            expr.to_string()
        };
        if let Some(rest) = inner.strip_prefix(prim) {
            if rest.starts_with('(') && rest.ends_with(')') {
                return inner;
            }
        }
        return format!("{}({})", prim, inner);
    }
    format!("{}({})", prim, expr)
}

/// Peel one `uint64(…)` wrapper so range-checked helpers see the wide expr.
fn strip_redundant_sol_narrow_cast<'a>(prim: &str, expr: &'a str) -> &'a str {
    let prefix = format!("{}(", prim);
    if expr.starts_with(&prefix) && expr.ends_with(')') {
        &expr[prefix.len()..expr.len() - 1]
    } else {
        expr
    }
}

pub(crate) fn wrap_checked_narrow_cast(target_sol_ty: &str, expr: &str) -> String {
    if expr.is_empty() {
        return expr.to_string();
    }
    let prim = match target_sol_ty {
        "uint8" | "uint16" | "uint32" | "uint64" | "uint128" | "int8" | "int16" | "int32"
        | "int64" | "int128" => target_sol_ty,
        _ => return format!("{}({})", target_sol_ty, expr),
    };
    if let Some(helper) = checked_downcast_helper_name(prim) {
        if wrapper_covers_entire(expr, &format!("{}(", helper)) {
            return expr.to_string();
        }
        let wide = strip_redundant_sol_narrow_cast(prim, expr);
        return format!("{}({})", helper, wide);
    }
    format!("{}({})", prim, expr)
}

fn checked_downcast_helper_name(prim: &str) -> Option<&'static str> {
    match prim {
        "uint8" => Some("_toUint8"),
        "uint16" => Some("_toUint16"),
        "uint32" => Some("_toUint32"),
        "uint64" => Some("_toUint64"),
        "uint128" => Some("_toUint128"),
        "int8" => Some("_toInt8"),
        "int16" => Some("_toInt16"),
        "int32" => Some("_toInt32"),
        "int64" => Some("_toInt64"),
        "int128" => Some("_toInt128"),
        _ => None,
    }
}

/// Solidity 0.8 explicit downcasts wrap; Cambrian narrowing `as uN` panics
/// 0x11 if the value is out of range (PW3-O-007). File-scope so `pure fn`
/// bodies and contracts share the same helpers.
pub(crate) fn gen_checked_downcast_helpers() -> &'static str {
    concat!(
        "function _toUint8(uint256 x) pure returns (uint8) { require(x <= type(uint8).max); return uint8(x); }\n",
        "function _toUint16(uint256 x) pure returns (uint16) { require(x <= type(uint16).max); return uint16(x); }\n",
        "function _toUint32(uint256 x) pure returns (uint32) { require(x <= type(uint32).max); return uint32(x); }\n",
        "function _toUint64(uint256 x) pure returns (uint64) { require(x <= type(uint64).max); return uint64(x); }\n",
        "function _toUint128(uint256 x) pure returns (uint128) { require(x <= type(uint128).max); return uint128(x); }\n",
        "function _toInt8(int256 x) pure returns (int8) { require(x >= type(int8).min && x <= type(int8).max); return int8(x); }\n",
        "function _toInt16(int256 x) pure returns (int16) { require(x >= type(int16).min && x <= type(int16).max); return int16(x); }\n",
        "function _toInt32(int256 x) pure returns (int32) { require(x >= type(int32).min && x <= type(int32).max); return int32(x); }\n",
        "function _toInt64(int256 x) pure returns (int64) { require(x >= type(int64).min && x <= type(int64).max); return int64(x); }\n",
        "function _toInt128(int256 x) pure returns (int128) { require(x >= type(int128).min && x <= type(int128).max); return int128(x); }\n",
        "function _camI2U(int256 x) pure returns (uint256) { require(x >= 0); return uint256(x); }\n",
        "function _camU2I(uint256 x) pure returns (int256) { require(x <= uint256(type(int256).max)); return int256(x); }\n",
        "function _cam_sum_uint32(uint32[] memory xs) pure returns (uint32) { uint32 acc = 0; for (uint256 i = 0; i < xs.length; ++i) { acc += xs[i]; } return acc; }\n",
        "function _cam_sum_uint64(uint64[] memory xs) pure returns (uint64) { uint64 acc = 0; for (uint256 i = 0; i < xs.length; ++i) { acc += xs[i]; } return acc; }\n",
        "function _cam_sum_uint256(uint256[] memory xs) pure returns (uint256) { uint256 acc = 0; for (uint256 i = 0; i < xs.length; ++i) { acc += xs[i]; } return acc; }\n\n",
    )
}

/// Which inference rule to apply when lowering the same AST expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InferSolMode {
    /// Let bindings, loop/collect element types — prefer narrow `uintN`.
    Decl,
    /// Operand coercion / Solidity flow typing — widen on mismatch.
    Flow,
}

pub(crate) fn infer_expr_sol_ty(
    expr: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
    mode: InferSolMode,
) -> String {
    match mode {
        InferSolMode::Decl => infer_let_type_entity(expr, entity, ctx, scratch),
        InferSolMode::Flow => actual_sol_type(expr, entity, ctx, scratch),
    }
}

/// Resolve a bare identifier: let-binding stack, then entity member, else `uint256`.
fn infer_ident_sol_ty(
    name: &str,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    if let Some(ty) = scratch.borrow().lookup_let_binding(name) {
        return ty;
    }
    if let Some(member) = entity.members.iter().find(|m| m.name == name) {
        return sol_type_entity(entity, &member.ty, true, ctx);
    }
    "uint256".to_string()
}

/// `as T` — Decl uses the cast target; Flow keeps a known-narrow inner type.
fn infer_cast_sol_ty(
    inner: &Expr,
    ty: &Type,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
    mode: InferSolMode,
) -> String {
    match mode {
        InferSolMode::Decl => sol_type(ty, true),
        InferSolMode::Flow => {
            let inner_ty = infer_let_type_entity(inner, entity, ctx, scratch);
            if inner_ty != "uint256" {
                inner_ty
            } else {
                sol_type(ty, true)
            }
        }
    }
}

/// Shared numeric binop result type after operand types are known.
fn infer_binop_sol_ty(lty: &str, rty: &str, mode: InferSolMode) -> String {
    match mode {
        InferSolMode::Flow => match (lty, rty) {
            ("int_literal", _) => rty.to_string(),
            (_, "int_literal") => lty.to_string(),
            (a, b) if a == b => a.to_string(),
            _ => "uint256".to_string(),
        },
        InferSolMode::Decl => {
            if lty != "uint256" {
                return lty.to_string();
            }
            if rty != "uint256" {
                return rty.to_string();
            }
            "uint256".to_string()
        }
    }
}

/// Phase EVM-P0-B follow-up: stricter type-inference helper used when
/// deciding whether a flowing-into-narrow position needs an explicit
/// `uintN(...)` cast. Unlike `infer_let_type_entity` (which prefers
/// the narrowest operand to drive let-binding declarations), this
/// function models Solidity's *actual* type rule: any operand of
/// `uint256` / `int256` width widens the entire binop, and unknown
/// shapes are treated as `uint256` (the conservative widest type).
///
/// Sources of `uint256` width on EVM:
///   * `msg::value`, `msg::timestamp` (`block.timestamp`),
///     `msg::number` (`block.number`).
///   * `<expr>.balance` (`address.balance`).
///   * Stdlib helper return types (`min`, `max`, `clamp`, `muldiv`,
///     `divc`, `divr`, `divmod`).
///   * `keccak256(...)`, `hashOf(...)`.
///   * `Expr::Cast(_, U256 / usize)`.
pub(crate) fn actual_sol_type(
    expr: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    match expr {
        Expr::BoolLiteral(_) => "bool".to_string(),
        Expr::StringLiteral(_) => "string memory".to_string(),
        Expr::IntLiteral(v) if !v.fits_u128() => "uint256".to_string(),
        Expr::IntLiteral(_) => "int_literal".to_string(),
        Expr::None | Expr::EmptyCollection => "int_literal".to_string(),
        Expr::MsgField(f) => match f.as_str() {
            "sender" => "address".to_string(),
            "int" | "ext" => "bool".to_string(),
            "value" | "timestamp" | "number" | "gasleft" => "uint256".to_string(),
            _ => "uint256".to_string(),
        },
        Expr::SysField(f) => match f.as_str() {
            "address" | "coinbase" | "origin" => "address".to_string(),
            _ => "uint256".to_string(),
        },
        Expr::Cast(inner, ty) => infer_cast_sol_ty(inner, ty, entity, ctx, scratch, InferSolMode::Flow),
        Expr::FieldAccess(base, field) => {
            if field == "balance" {
                return "uint256".to_string();
            }
            if field == "length" {
                return "uint256".to_string();
            }
            if let Expr::Ident(base_name) = base.as_ref() {
                if let Some(member) = entity.members.iter().find(|m| m.name == *base_name) {
                    if let Type::Simple(rec_name) = &member.ty {
                        if let Some(rec) = entity.records.iter().find(|r| r.name == *rec_name) {
                            if let Some(f) = rec.fields.iter().find(|f| f.name == *field) {
                                return sol_type_entity(entity, &f.ty, true, ctx);
                            }
                        }
                    }
                }
                if let Some(ty) = scratch.borrow().lookup_let_binding(base_name) {
                    let rec_name = ty.strip_suffix(" memory").unwrap_or(ty.as_str());
                    if let Some(f_ty) = lookup_record_field_type(entity, ctx, rec_name, field) {
                        return sol_type_entity(entity, &f_ty, true, ctx);
                    }
                }
            }
            record_field_sol_type_of_base(base, field, entity, ctx, scratch)
                .unwrap_or_else(|| "uint256".to_string())
        }
        Expr::Ident(name) => infer_ident_sol_ty(name, entity, ctx, scratch),
        Expr::BinOp(l, op, r) => {
            let lty = actual_sol_type(l, entity, ctx, scratch);
            let rty = actual_sol_type(r, entity, ctx, scratch);
            // Comparison / boolean ops produce `bool`.
            use crate::ast::BinOp;
            if matches!(
                op,
                BinOp::WrappingAdd | BinOp::WrappingSub | BinOp::WrappingMul
            ) {
                return "uint256".to_string();
            }
            if matches!(
                op,
                BinOp::Eq
                    | BinOp::Ne
                    | BinOp::Lt
                    | BinOp::Le
                    | BinOp::Gt
                    | BinOp::Ge
                    | BinOp::And
                    | BinOp::Or
            ) {
                return "bool".to_string();
            }
            infer_binop_sol_ty(&lty, &rty, InferSolMode::Flow)
        }
        // T-EVM-EX-007: unary `!` is bool; `-` / `*` follow the operand.
        Expr::UnaryOp(op, inner) => {
            use crate::ast::UnaryOp;
            match op {
                UnaryOp::Not => "bool".to_string(),
                UnaryOp::Neg | UnaryOp::Deref => actual_sol_type(inner, entity, ctx, scratch),
            }
        }
        // Route `let b = m.exists(k)` / `.contains` / `.is_empty()` — bool.
        Expr::MethodCall(_, method, _)
            if method == "exists" || method == "contains" || method == "is_empty" =>
        {
            "bool".to_string()
        }
        // Temporal refs have the member's storage type.
        Expr::TemporalRef(name) => {
            if let Some(member) = entity.members.iter().find(|m| m.name == *name) {
                sol_type_entity(entity, &member.ty, true, ctx)
            } else {
                "uint256".to_string()
            }
        }
        Expr::FnCall(name, _) => {
            if let Some(route) = entity.routes.iter().find(|r| r.name == *name) {
                if let Some(ret) = &route.return_type {
                    return sol_type_entity(entity, ret, true, ctx);
                }
            }
            // Stdlib helpers always return `uint256` on EVM (their
            // bodies are monomorphic in `uint256`).
            const STDLIB_UINT256_FNS: &[&str] = &["min", "max", "clamp", "muldiv", "divc", "divr"];
            if STDLIB_UINT256_FNS.contains(&name.as_str()) {
                return "uint256".to_string();
            }
            if name == "hashOf" || name == "keccak256" {
                return "uint256".to_string();
            }
            if let Some(ty) = ctx.lookup_pure_fn_return(name) {
                return sol_type_entity(entity, &ty, true, ctx);
            }
            "uint256".to_string()
        }
        Expr::NamespacedCall {
            namespace, name, ..
        } => {
            if namespace == "evm" {
                return evm_intrinsic_sol_return_ty(name)
                    .unwrap_or("uint256")
                    .to_string();
            }
            if namespace == "std::str" && name == "format" {
                return "string memory".to_string();
            }
            if namespace == "std::crypto" && name == "sha256" {
                return "bytes memory".to_string();
            }
            if namespace == "std::str" && crate::codegen::std_str::parse_str_meta(name).is_some() {
                return "uint256".to_string();
            }
            if namespace == "std::math" {
                return "uint256".to_string();
            }
            "uint256".to_string()
        }
        Expr::EnumVariantWithData(en, variant, _) if en == "evm" => {
            evm_intrinsic_sol_return_ty(variant)
                .unwrap_or("uint256")
                .to_string()
        }
        Expr::Index(base, _) => {
            if let Expr::Ident(name) = base.as_ref() {
                if let Some(member) = entity.members.iter().find(|m| m.name == *name) {
                    if let Type::Generic(g, params) = &member.ty {
                        if (g == "HashMap" || g == "Vec") && !params.is_empty() {
                            return sol_type_entity(entity, params.last().unwrap(), true, ctx);
                        }
                    }
                }
            }
            "uint256".to_string()
        }
        Expr::If(_, then, _) => actual_sol_type(then, entity, ctx, scratch),
        Expr::Let(_, _, body) => actual_sol_type(body, entity, ctx, scratch),
        Expr::Some(inner) => {
            let inner_ty = actual_sol_type(inner, entity, ctx, scratch);
            let ident = super::option::strip_memory_ty(&inner_ty);
            format!("Option_{} memory", ident)
        }
        Expr::Block(items) => items
            .last()
            .map(|e| actual_sol_type(e, entity, ctx, scratch))
            .unwrap_or_else(|| "uint256".to_string()),
        Expr::EnumVariant(en, _) => {
            if is_entity_enum(entity, en) || ctx.is_program_enum(en) {
                en.clone()
            } else {
                "uint256".to_string()
            }
        }
        Expr::Match(subject, arms) => {
            if let Some(ty) =
                crate::codegen::std_str::infer_match_on_std_parse_sol_type(subject, arms)
            {
                return ty;
            }
            arms.first()
                .map(|a| actual_sol_type(&a.body, entity, ctx, scratch))
                .unwrap_or_else(|| "uint256".to_string())
        }
        Expr::MethodCall(base, method, args) if method == "address" && args.is_empty() => {
            actual_sol_type(base, entity, ctx, scratch)
        }
        _ => "uint256".to_string(),
    }
}

/// Solidity type for [`EmitScope::expected_ty`] when emitting a value expression.
pub(crate) fn scope_expected_sol_ty(
    scope: &super::ctx::EmitScope,
    entity: &Entity,
    ctx: &EvmCtx,
) -> Option<String> {
    scope
        .expected_ty
        .map(|t| sol_type_entity(entity, t, true, ctx))
}

/// Element type for `Vec<T>` in [`EmitScope::expected_ty`] (chain `.collect()` / `for` loops).
pub(crate) fn scope_expected_vec_elem_sol_ty(
    scope: &super::ctx::EmitScope,
    entity: &Entity,
    ctx: &EvmCtx,
) -> Option<String> {
    match scope.expected_ty {
        Some(Type::Generic(name, params)) if name == "Vec" && params.len() == 1 => {
            Some(sol_type_entity(entity, &params[0], true, ctx))
        }
        _ => None,
    }
}

pub(crate) fn maybe_narrow_cast(
    target_sol_ty: &str,
    expr_str: &str,
    expr: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    if target_sol_ty == "address" {
        if let Expr::IntLiteral(v) = expr {
            return hex_literal_to_sol_for_ty(
                &crate::ast::u256_hex_digits(v),
                Some("address"),
            );
        }
    }
    if target_sol_ty == "bytes4" {
        if let Expr::IntLiteral(v) = expr {
            if v.hi == 0 && v.lo <= u64::MAX as u128 {
                return format!("bytes4(uint32({}))", v.lo);
            }
            return format!(
                "bytes4(uint32(uint256(0x{})))",
                crate::ast::u256_hex_digits(v)
            );
        }
    }
    if is_option_sol_ty(target_sol_ty)
        && (expr_str == "0" || matches!(expr, Expr::None))
    {
        return option_none_from_sol_ty(target_sol_ty);
    }
    if matches!(expr, Expr::Cast(inner, _) if infer_let_type_entity(inner, entity, ctx, scratch) == "bool")
        && target_sol_ty.starts_with("uint")
    {
        return expr_str.to_string();
    }
    let actual = infer_expr_sol_ty(expr, entity, ctx, scratch, InferSolMode::Flow);
    let actual_ref = if actual == "int_literal" {
        Some(target_sol_ty)
    } else {
        Some(actual.as_str())
    };
    wrap_narrow_cast_with_actual(target_sol_ty, expr_str, actual_ref)
}

/// `address(uint256(x))` from the `address(...)` builtin is not assignable to
/// `address`; widen through `uint160` like [`hex_literal_to_sol_for_ty`].
fn fix_address_uint256_wrapper(rendered: &str) -> String {
    if rendered.starts_with("address(uint256(") && rendered.ends_with(')') {
        let inner = &rendered["address(".len()..rendered.len() - 1];
        return format!("address(uint160({}))", inner);
    }
    rendered.to_string()
}

/// Coerce a rendered expression into an EVM `address` (send dest, from-clause).
pub(crate) fn coerce_to_address_sol(
    expr: &Expr,
    rendered: &str,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let rendered = fix_address_uint256_wrapper(rendered);
    if rendered.starts_with("address(") || is_address_shaped_return_expr(&rendered) {
        return rendered;
    }
    if let Expr::IntLiteral(v) = expr {
        return hex_literal_to_sol_for_ty(
            &crate::ast::u256_hex_digits(v),
            Some("address"),
        );
    }
    let actual = infer_expr_sol_ty(expr, entity, ctx, scratch, InferSolMode::Flow);
    if actual == "address" {
        return rendered.to_string();
    }
    if is_numeric_sol_ty(&actual) {
        return format!("address(uint160(uint256({})))", rendered);
    }
    rendered.to_string()
}

fn is_numeric_sol_ty(ty: &str) -> bool {
    if ty.contains('[') || ty.contains("memory") || ty.contains("mapping") {
        return false;
    }
    ty == "int_literal"
        || ty.starts_with("uint")
        || (ty.starts_with("int") && ty.as_bytes().get(3).is_some_and(|c| c.is_ascii_digit()))
}

fn is_address_shaped_return_expr(val: &str) -> bool {
    val.starts_with("address(")
        || val.contains(".address(")
        || val.contains(".predict")
        || val.contains("ecrecover(")
        || val == "address(this)"
        || val == "address(0)"
}

/// When a record-typed member transform body is a scalar expression that
/// only reads one `member.field`, lower it as `RecordUpdate(member,
/// field := body)` so solc sees a struct copy rather than a bare field
/// value assigned to the whole member.
pub(crate) fn try_wrap_record_member_field_transform(
    member_name: &str,
    member_ty: &Type,
    body: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
) -> Option<Expr> {
    let record_name = match member_ty {
        Type::Simple(n) => n,
        _ => return None,
    };
    if !is_entity_record(entity, record_name) && !ctx.is_program_record(record_name) {
        return None;
    }
    match body {
        Expr::RecordConstruct(..) | Expr::RecordUpdate(..) => return None,
        _ => {}
    }
    if let Expr::MethodCall(base, method, _) = body {
        if method == "fold" {
            if matches!(
                base.as_ref(),
                Expr::FieldAccess(inner, _) if matches!(inner.as_ref(), Expr::Ident(m) if m == member_name)
            ) {
                return None;
            }
        }
    }
    let mut fields = std::collections::HashSet::new();
    collect_member_field_refs(body, member_name, &mut fields);
    if fields.len() != 1 {
        return None;
    }
    let field = fields.into_iter().next().unwrap();
    let rec = ctx
        .lookup_record(record_name)
        .or_else(|| entity.records.iter().find(|r| r.name == *record_name))?;
    if !rec.fields.iter().any(|f| f.name == field) {
        return None;
    }
    Some(Expr::RecordUpdate(
        Box::new(Expr::Ident(member_name.to_string())),
        vec![(field, body.clone())],
    ))
}

fn collect_member_field_refs(expr: &Expr, member_name: &str, out: &mut std::collections::HashSet<String>) {
    match expr {
        Expr::FieldAccess(base, field) => {
            if matches!(base.as_ref(), Expr::Ident(n) if n == member_name) {
                out.insert(field.clone());
            }
            collect_member_field_refs(base, member_name, out);
        }
        Expr::BinOp(l, _, r) => {
            collect_member_field_refs(l, member_name, out);
            collect_member_field_refs(r, member_name, out);
        }
        Expr::UnaryOp(_, e) => collect_member_field_refs(e, member_name, out),
        Expr::Cast(e, _) => collect_member_field_refs(e, member_name, out),
        Expr::If(c, t, e) => {
            collect_member_field_refs(c, member_name, out);
            collect_member_field_refs(t, member_name, out);
            if let Some(x) = e {
                collect_member_field_refs(x, member_name, out);
            }
        }
        Expr::MethodCall(b, _, args) => {
            collect_member_field_refs(b, member_name, out);
            for a in args {
                collect_member_field_refs(a, member_name, out);
            }
        }
        Expr::FnCall(_, args) | Expr::MacroRef(_, args) => {
            for a in args {
                collect_member_field_refs(a, member_name, out);
            }
        }
        Expr::Index(b, k) => {
            collect_member_field_refs(b, member_name, out);
            collect_member_field_refs(k, member_name, out);
        }
        Expr::Match(s, arms) => {
            collect_member_field_refs(s, member_name, out);
            for a in arms {
                collect_member_field_refs(&a.body, member_name, out);
            }
        }
        Expr::Let(_, v, b) => {
            collect_member_field_refs(v, member_name, out);
            collect_member_field_refs(b, member_name, out);
        }
        Expr::Block(items) => {
            for e in items {
                collect_member_field_refs(e, member_name, out);
            }
        }
        Expr::For(_, it, b) => {
            collect_member_field_refs(it, member_name, out);
            collect_member_field_refs(b, member_name, out);
        }
        Expr::Closure(_, b) => collect_member_field_refs(b, member_name, out),
        Expr::Tuple(es) => {
            for e in es {
                collect_member_field_refs(e, member_name, out);
            }
        }
        _ => {}
    }
}

/// True when `val` is already explicitly narrowed to `narrow_ret`.
fn sol_expr_already_narrowed_to(narrow_ret: &str, val: &str) -> bool {
    if wrapper_covers_entire(val, &format!("{}(", narrow_ret)) {
        return true;
    }
    checked_downcast_helper_name(narrow_ret)
        .is_some_and(|helper| wrapper_covers_entire(val, &format!("{}(", helper)))
}

/// `prefix` includes the opening `(` (`uint64(` / `_toUint64(`). True only
/// when a matching close paren is the last character of `val`.
fn wrapper_covers_entire(val: &str, prefix: &str) -> bool {
    let Some(rest) = val.strip_prefix(prefix) else {
        return false;
    };
    let mut depth = 1i32;
    for (i, c) in rest.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return i + c.len_utf8() == rest.len();
                }
            }
            _ => {}
        }
    }
    false
}

/// Coerce a rendered Solidity expression to a declared function/route
/// return type (`ret` is already lowered, e.g. `uint64`, `Pair memory`).
pub(crate) fn coerce_return_value(
    ret: &str,
    val: String,
    expr: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    if val == "\"\"" && ret == "address" {
        return "address(0)".to_string();
    }
    if val == "0" {
        if let Some(elem) = ret.strip_suffix("[] memory") {
            return format!("new {}[](0)", elem);
        }
    }
    if let Some(elem) = ret.strip_suffix("[] memory") {
        if elem != "uint256"
            && (val == "new uint256[](0)" || matches!(expr, Expr::ArrayLit(items) if items.is_empty()))
        {
            return format!("new {}[](0)", elem);
        }
    }

    let narrow_ret = ret.strip_suffix(" memory").unwrap_or(ret);
    if narrow_ret == "address" && !is_address_shaped_return_expr(&val) {
        let actual = infer_expr_sol_ty(expr, entity, ctx, scratch, InferSolMode::Flow);
        if actual == "address" {
            return val;
        }
        return format!("address(uint160(uint256({})))", val);
    }
    let actual = if !ret.contains("[]") {
        match expr {
            Expr::For(_, _, body) => {
                infer_expr_sol_ty(body, entity, ctx, scratch, InferSolMode::Decl)
            }
            _ => infer_expr_sol_ty(expr, entity, ctx, scratch, InferSolMode::Decl),
        }
    } else {
        infer_let_type_entity(expr, entity, ctx, scratch)
    };
    if actual.ends_with("[] memory") && !ret.contains("[]") {
        let elem = actual.strip_suffix("[] memory").unwrap_or("uint256");
        let sum_fn = match elem {
            "uint32" => "_cam_sum_uint32",
            "uint64" => "_cam_sum_uint64",
            _ => "_cam_sum_uint256",
        };
        return format!("{}({})", sum_fn, val);
    }
    if narrow_ret.starts_with("uint") && narrow_ret != "uint256" {
        if sol_expr_already_narrowed_to(narrow_ret, &val) {
            return val;
        }
        let skip_cast = actual == narrow_ret
            && !matches!(expr, Expr::BinOp(_, _, _) | Expr::UnaryOp(_, _));
        if skip_cast {
            return val;
        }
        return wrap_narrow_cast_with_actual(narrow_ret, &val, Some("uint256"));
    }
    if narrow_ret.starts_with("int") && narrow_ret != "int256" {
        if sol_expr_already_narrowed_to(narrow_ret, &val) {
            return val;
        }
        let skip_cast = actual == narrow_ret
            && !matches!(expr, Expr::BinOp(_, _, _) | Expr::UnaryOp(_, _));
        if skip_cast {
            return val;
        }
        return wrap_narrow_cast_with_actual(narrow_ret, &val, Some("int256"));
    }
    if narrow_ret == "bytes32" && (actual == "uint256" || actual == "int_literal") {
        return format!("bytes32({})", val);
    }
    maybe_narrow_cast(ret, &val, expr, entity, ctx, scratch)
}

pub(crate) fn sol_return_type(entity: &Entity, ty: &Type, ctx: &EvmCtx) -> String {
    // Pass `in_param=true` so dynamic types (`Vec<T>`, `String`,
    // `bytes`, record memory) get the required `memory` data-location
    // annotation Solidity demands on returns. `in_param` is mis-named
    // historically — it really means "does this position need the
    // dynamic-type data location?", which is true for both function
    // parameters and return parameters.
    match ty {
        Type::Tuple(items) => items
            .iter()
            .map(|t| {
                if is_mapping_type(t) {
                    format!("{} storage", sol_type_entity(entity, t, false, ctx))
                } else {
                    sol_type_entity(entity, t, true, ctx)
                }
            })
            .collect::<Vec<_>>()
            .join(", "),
        _ if is_mapping_type(ty) => {
            format!("{} storage", sol_type_entity(entity, ty, false, ctx))
        }
        _ => sol_type_entity(entity, ty, true, ctx),
    }
}

pub(crate) fn default_value(ty: &Type) -> String {
    match ty {
        Type::Simple(name) => match name.as_str() {
            "bool" => "false".to_string(),
            "String" => "\"\"".to_string(),
            "bytes" => "\"\"".to_string(),
            "address" => "address(0)".to_string(),
            "pubkey" => "bytes32(0)".to_string(),
            _ => "0".to_string(),
        },
        Type::TypedAddress(_) => "address(0)".to_string(),
        Type::Generic(name, _) if name == "HashMap" => String::new(),
        // G-U3: dynamic Solidity arrays default-initialise to empty
        // at storage allocation; an explicit `m_xyz = 0;` would fail
        // to type-check, so we elide the default-write entirely.
        Type::Generic(name, _) if name == "Vec" => String::new(),
        _ => "0".to_string(),
    }
}

pub(crate) fn type_contains_dynamic_storage(entity: &Entity, ty: &Type, ctx: &EvmCtx) -> bool {
    contains_dynamic_storage(entity, ty, ctx, true)
}

/// A `Vec` / `HashMap` anywhere in `ty`: the positions a public Solidity
/// getter cannot return as a whole (`string` / `bytes` it can).
pub(crate) fn type_contains_array_storage(entity: &Entity, ty: &Type, ctx: &EvmCtx) -> bool {
    contains_dynamic_storage(entity, ty, ctx, false)
}

fn contains_dynamic_storage(entity: &Entity, ty: &Type, ctx: &EvmCtx, bytes: bool) -> bool {
    match ty {
        Type::Simple(name) => {
            if name == "String" || name == "bytes" || name == "CamData" {
                return bytes;
            }
            if is_entity_record(entity, name) || ctx.is_program_record(name) {
                let rec = entity
                    .records
                    .iter()
                    .find(|r| r.name == *name)
                    .or_else(|| ctx.lookup_record(name));
                return rec
                    .map(|r| {
                        r.fields
                            .iter()
                            .any(|f| contains_dynamic_storage(entity, &f.ty, ctx, bytes))
                    })
                    .unwrap_or(false);
            }
            false
        }
        Type::Generic(name, params) => {
            name == "Vec"
                || name == "HashMap"
                || params
                    .iter()
                    .any(|p| contains_dynamic_storage(entity, p, ctx, bytes))
        }
        Type::Tuple(items) => items
            .iter()
            .any(|t| contains_dynamic_storage(entity, t, ctx, bytes)),
        _ => false,
    }
}

pub(crate) fn default_value_entity(entity: &Entity, ty: &Type, ctx: &EvmCtx) -> String {
    match ty {
        Type::Simple(name)
            if is_entity_record(entity, name) || ctx.is_program_record(name) =>
        {
            let rec = entity
                .records
                .iter()
                .find(|r| r.name == *name)
                .or_else(|| ctx.lookup_record(name))
                .expect("record exists");
            let fields = rec
                .fields
                .iter()
                .map(|f| format!("{}: {}", f.name, default_value_entity(entity, &f.ty, ctx)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}({{{}}})", name, fields)
        }
        Type::Simple(name) if is_entity_enum(entity, name) || ctx.is_program_enum(name) => {
            let decl = ctx
                .lookup_enum(name)
                .or_else(|| entity.enums.iter().find(|e| e.name == *name).cloned());
            if let Some(decl) = decl {
                // Phase EVM-4 J1: payload enums need a struct-literal
                // default; unit-only enums keep the simple `Enum.First`
                // form.
                if is_payload_enum(&decl) {
                    return payload_enum_zero_literal(entity, &decl, ctx);
                }
                if let Some(first) = decl.variants.first() {
                    return format!("{}.{}", name, first.name);
                }
            }
            "0".to_string()
        }
        Type::Generic(name, params) if name == "Vec" && !params.is_empty() => {
            if vec_empty_transform_needs_write(entity, ty, ctx) {
                solidity_default_for_member_ty(entity, ty, ctx)
            } else {
                String::new()
            }
        }
        Type::Generic(name, params) if name == "Option" && !params.is_empty() => {
            solidity_default_for_member_ty(entity, ty, ctx)
        }
        _ => default_value(ty),
    }
}

fn lookup_record_field_type(
    entity: &Entity,
    ctx: &EvmCtx,
    rec_name: &str,
    field: &str,
) -> Option<Type> {
    let rec = entity
        .records
        .iter()
        .find(|r| r.name == rec_name)
        .or_else(|| ctx.lookup_record(rec_name))?;
    rec.fields
        .iter()
        .find(|f| f.name == field)
        .map(|f| f.ty.clone())
}

fn record_name_from_ident_base(
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
    base_name: &str,
) -> Option<String> {
    if let Some(member) = entity.members.iter().find(|m| m.name == base_name) {
        if let Type::Simple(rec_name) = &member.ty {
            return Some(rec_name.clone());
        }
    }
    if let Some(ty) = scratch.borrow().lookup_let_binding(base_name) {
        if let Some(rec_name) = ty.split_whitespace().next() {
            if is_entity_record(entity, rec_name) || ctx.is_program_record(rec_name) {
                return Some(rec_name.to_string());
            }
        }
    }
    None
}

/// Best-effort inference of the element type of a `Vec<T>` expression in
/// the current entity context. Used by `for x in vec { ... }` lowering on
/// EVM to declare the loop variable with a typed Solidity binding.
/// Returns `None` if we cannot determine the type (caller falls back to
/// `uint256`, which works for primitive elements).
pub(crate) fn infer_iter_elem_type_entity(
    expr: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> Option<Type> {
    fn vec_elem(ty: &Type) -> Option<Type> {
        if let Type::Generic(name, params) = ty {
            if name == "Vec" && params.len() == 1 {
                return Some(params[0].clone());
            }
        }
        None
    }
    match expr {
        Expr::Ident(name) => {
            if let Some(ty) = scratch.borrow().lookup_let_binding(name) {
                if let Some(inner) = ty.strip_suffix("[] memory") {
                    return sol_array_elem_to_cambrian_type(inner);
                }
            }
            entity
                .members
                .iter()
                .find(|m| m.name == *name)
                .and_then(|m| vec_elem(&m.ty))
        }
        Expr::FnCall(name, _) => ctx.lookup_pure_fn_return(name).and_then(|t| vec_elem(&t)),
        // `m[k]` where `m: HashMap<K, Vec<T>>` — resolve through the
        // mapping's value type. Only handles the bare-Ident base case;
        // nested paths fall through to `None` and the loop-variable type
        // defaults to `uint256` per the existing fallback.
        Expr::Index(base, _) => {
            if let Expr::Ident(name) = base.as_ref() {
                if let Some(member) = entity.members.iter().find(|m| m.name == *name) {
                    if let Type::Generic(g, params) = &member.ty {
                        if g == "HashMap" && params.len() == 2 {
                            return vec_elem(&params[1]);
                        }
                    }
                }
            }
            None
        }
        // `m_record.list` where `m_record: SomeRecord` and the record
        // declares `list: Vec<T>`. Without this the for-loop element
        // type silently defaults to `uint256` even when we have full
        // record-field type info on hand.
        Expr::FieldAccess(base, field) => {
            if let Expr::Ident(base_name) = base.as_ref() {
                if let Some(rec_name) =
                    record_name_from_ident_base(entity, ctx, scratch, base_name)
                {
                    if let Some(f_ty) = lookup_record_field_type(entity, ctx, &rec_name, field) {
                        return vec_elem(&f_ty);
                    }
                }
            }
            None
        }
        // EVM-13: `m.keys()` / `m.keys().collect()` over a HashMap
        // member — element type is the HashMap's *key* type. The
        // parallel `K[] m_keys` storage array we emit alongside the
        // mapping produces `K`-typed elements when iterated, matching
        // Cambrian's iter-over-keys semantics on the other backends.
        // `.collect()` is a no-op transparency-wise, so we recurse
        // through it.
        Expr::MethodCall(base, method, args) if method == "collect" && args.is_empty() => {
            infer_iter_elem_type_entity(base, entity, ctx, scratch)
        }
        Expr::MethodCall(base, method, args) if method == "keys" && args.is_empty() => {
            if let Expr::Ident(name) = base.as_ref() {
                if let Some(member) = entity.members.iter().find(|m| m.name == *name) {
                    if let Type::Generic(g, params) = &member.ty {
                        if g == "HashMap" && params.len() >= 1 {
                            return Some(params[0].clone());
                        }
                    }
                }
            }
            if let Expr::Index(inner, _) = base.as_ref() {
                if let Expr::Ident(name) = inner.as_ref() {
                    return nested_hashmap_inner_key_type(entity, name);
                }
            }
            None
        }
        // EVM-13 Batch D: `m.values()` over a HashMap yields scalar V.
        // The for-loop / chain emitter walk the parallel `<m>_keys`
        // array and look up `m[m_keys[i]]` per iteration.
        Expr::MethodCall(base, method, args) if method == "values" && args.is_empty() => {
            if let Expr::Ident(name) = base.as_ref() {
                if let Some(member) = entity.members.iter().find(|m| m.name == *name) {
                    if let Type::Generic(g, params) = &member.ty {
                        if g == "HashMap" && params.len() >= 2 {
                            return Some(params[1].clone());
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Try to infer the entity record name for an `Expr::RecordUpdate`
/// whose base type cannot be resolved otherwise (e.g. when the base
/// is a let-bound local with an unknown source). Falls back to
/// matching the update field names against entity record field sets:
/// if exactly one record's fields are a superset of the updates,
/// pick it.
pub(crate) fn infer_record_from_update_fields(
    entity: &Entity,
    ctx: &EvmCtx,
    updates: &[(String, Expr)],
    scratch: &RefCell<EmitScratch>,
) -> Option<String> {
    let want: std::collections::HashSet<&str> = updates.iter().map(|(n, _)| n.as_str()).collect();
    let mut hits: Vec<String> = Vec::new();
    let mut consider = |rec: &crate::ast::Record| {
        let have: std::collections::HashSet<&str> =
            rec.fields.iter().map(|f| f.name.as_str()).collect();
        if want.is_subset(&have) && !hits.iter().any(|h| h == &rec.name) {
            hits.push(rec.name.clone());
        }
    };
    for rec in &entity.records {
        consider(rec);
    }
    for name in &ctx.program_records {
        if let Some(rec) = ctx.lookup_record(name) {
            consider(rec);
        }
    }
    if hits.len() == 1 {
        Some(format!("{} memory", hits[0]))
    } else if let Some(rec_name) = scratch.borrow().lookup_transform_member_record() {
        Some(format!("{} memory", rec_name))
    } else {
        None
    }
}

/// Field type of `base.field` when `base` is an arbitrary expression whose
/// inferred Solidity type is a record (`m_vec[i].owner`, `f(x).owner`, …).
fn record_field_sol_type_of_base(
    base: &Expr,
    field: &str,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> Option<String> {
    if matches!(base, Expr::Ident(_)) {
        return None;
    }
    let base_ty = infer_let_type_entity(base, entity, ctx, scratch);
    let rec_name = base_ty.strip_suffix(" memory").unwrap_or(base_ty.as_str());
    lookup_record_field_type(entity, ctx, rec_name, field)
        .map(|f_ty| sol_type_entity(entity, &f_ty, true, ctx))
}

pub(crate) fn infer_let_type_entity(
    expr: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    match expr {
        Expr::BoolLiteral(_) => "bool".to_string(),
        Expr::StringLiteral(_) => "string memory".to_string(),
        Expr::IntLiteral(v) if !v.fits_u128() => "uint256".to_string(),
        Expr::IntLiteral(v) => infer_int_literal_sol_ty(v),
        Expr::MsgField(f) if f == "sender" => "address".to_string(),
        Expr::MsgField(f) if f == "int" || f == "ext" => "bool".to_string(),
        Expr::SysField(f) if f == "address" || f == "coinbase" || f == "origin" => {
            "address".to_string()
        }
        Expr::RecordConstruct(name, _) => format!("{} memory", name),
        Expr::ArrayLit(items) if !items.is_empty() => {
            let elem = infer_let_type_entity(&items[0], entity, ctx, scratch);
            format!("{}[] memory", elem.trim_end_matches(" memory"))
        }
        Expr::Cast(inner, ty) => {
            infer_cast_sol_ty(inner, ty, entity, ctx, scratch, InferSolMode::Decl)
        }
        Expr::If(_, then, else_opt) => {
            let then_ty = infer_let_type_entity(then, entity, ctx, scratch);
            match else_opt {
                Some(e) => merge_sol_branch_types(
                    &then_ty,
                    &infer_let_type_entity(e, entity, ctx, scratch),
                ),
                None => then_ty,
            }
        }
        // Phase EVM-P0-B: arithmetic binop preserves the operand
        // narrow-type so `x * 2` (where `x: u32`) infers `uint32`
        // rather than the bare `uint256` fallback. Used by
        // `gen_for_loop` to pick the right element type for the
        // emitted `T[] memory` result allocation.
        //
        // Selection rule: prefer either operand's non-`uint256` type
        // (typically a `uintN` / record / address). When both are
        // `uint256` (e.g. `U256 + U256` or generic literal arithmetic)
        // we keep `uint256`. Mixed-narrow shapes (`uint32 + uint64`)
        // are reported by validator E24 (warning) but still take the
        // first operand's type here so codegen produces *something*
        // consumable and solc's stricter type-checker becomes the
        // ground truth.
        //
        // T-EVM-EX-006: comparison / `&&` / `||` always produce `bool`
        // (match `actual_sol_type`); left-preferring operand width
        // would invent illegal `uintN(bool)` casts.
        Expr::BinOp(l, op, r) => {
            use crate::ast::BinOp;
            if matches!(
                op,
                BinOp::Eq
                    | BinOp::Ne
                    | BinOp::Lt
                    | BinOp::Le
                    | BinOp::Gt
                    | BinOp::Ge
                    | BinOp::And
                    | BinOp::Or
            ) {
                return "bool".to_string();
            }
            let lty = infer_let_type_entity(l, entity, ctx, scratch);
            let rty = infer_let_type_entity(r, entity, ctx, scratch);
            // Untyped integer literals defer to the other operand's type
            // (e.g. `U256 * 2` → `uint256`, `u32 + 2` → `uint32`).
            if matches!(r.as_ref(), Expr::IntLiteral(_)) {
                return lty;
            }
            if matches!(l.as_ref(), Expr::IntLiteral(_)) {
                return rty;
            }
            infer_binop_sol_ty(&lty, &rty, InferSolMode::Decl)
        }
        // T-EVM-EX-007: unary `!` is bool; `-` / `*` follow the operand.
        Expr::UnaryOp(op, inner) => {
            use crate::ast::UnaryOp;
            match op {
                UnaryOp::Not => "bool".to_string(),
                UnaryOp::Neg | UnaryOp::Deref => infer_let_type_entity(inner, entity, ctx, scratch),
            }
        }
        // Match / Block parity with `actual_sol_type`.
        Expr::Match(subject, arms) => {
            if let Some(ty) =
                crate::codegen::std_str::infer_match_on_std_parse_sol_type(subject, arms)
            {
                return ty;
            }
            arms.first()
                .map(|a| infer_let_type_entity(&a.body, entity, ctx, scratch))
                .unwrap_or_else(|| "uint256".to_string())
        }
        Expr::Block(items) => {
            let mut last = "uint256".to_string();
            let mut pushed: Vec<String> = Vec::new();
            for item in items {
                if let Expr::Let(Pattern::Ident(name), val, _) = item {
                    let val_ty = infer_let_type_entity(val, entity, ctx, scratch);
                    scratch.borrow_mut().push_let_binding(name, &val_ty);
                    pushed.push(name.clone());
                }
                last = match item {
                    Expr::Let(_, _, body) => infer_let_type_entity(body, entity, ctx, scratch),
                    _ => infer_let_type_entity(item, entity, ctx, scratch),
                };
            }
            for name in pushed {
                scratch.borrow_mut().pop_let_binding(&name);
            }
            last
        }
        // Temporal refs have the member's storage type.
        Expr::TemporalRef(name) => {
            if let Some(member) = entity.members.iter().find(|m| m.name == *name) {
                sol_type_entity(entity, &member.ty, true, ctx)
            } else {
                "uint256".to_string()
            }
        }
        // Phase EVM-15 H2 (Cluster A): a `let .. in body` expression
        // *is* its body's type. Push the binding while we recurse so
        // any inner `Ident(name)` resolves to `val`'s inferred type
        // (e.g. `let p = m_proposals[k]; p { ... }` needs `p`'s type
        // when inferring the `RecordUpdate`'s type).
        Expr::Let(Pattern::Ident(name), val, body) => {
            let val_ty = infer_let_type_entity(val, entity, ctx, scratch);
            scratch.borrow_mut().push_let_binding(name, &val_ty);
            let result = infer_let_type_entity(body, entity, ctx, scratch);
            scratch.borrow_mut().pop_let_binding(name);
            result
        }
        Expr::Let(_, _, body) => infer_let_type_entity(body, entity, ctx, scratch),
        // `some(inner)` wraps as `Option_<inner>` (tagged struct on EVM).
        Expr::Some(inner) => {
            let inner_ty = infer_let_type_entity(inner, entity, ctx, scratch);
            let ident = super::option::strip_memory_ty(&inner_ty);
            format!("Option_{} memory", ident)
        }
        // `Range(start, end)` is `uint256..uint256` by construction; pin
        // it explicitly so callers that bind it to a `let` (not just to
        // a `for` head) get the right slot type.
        Expr::Range(_, _) => "uint256".to_string(),
        // Enum variant literals (`State::Active`) carry the enum name,
        // both for entity-local and program-level enums. Without this
        // arm, `let cur = State::Active` silently widens to `uint256`
        // and Solidity rejects the subsequent return / assignment.
        Expr::EnumVariant(en, _) => {
            if is_entity_enum(entity, en) || ctx.is_program_enum(en) {
                en.clone()
            } else {
                "uint256".to_string()
            }
        }
        Expr::EnumVariantWithData(en, variant, _) => {
            if en == "evm" {
                return evm_intrinsic_sol_return_ty(variant)
                    .unwrap_or("uint256")
                    .to_string();
            }
            if is_entity_enum(entity, en) || ctx.is_program_enum(en) {
                format!("{} memory", en)
            } else {
                "uint256".to_string()
            }
        }
        // Phase EVM-15 H2 (Cluster A): `RecordUpdate(base, fields)`
        // produces a copy of `base` with overrides — its type IS the
        // base's type. Defer to the base; if that also fails (returns
        // `"uint256"`) and the update fields uniquely identify an
        // entity record, fall back to the field-set match.
        Expr::RecordUpdate(base, updates) => {
            if let Some(rec_ty) =
                infer_record_from_update_fields(entity, ctx, updates, scratch)
            {
                return rec_ty;
            }
            infer_let_type_entity(base, entity, ctx, scratch)
        }
        Expr::Ident(name) => infer_ident_sol_ty(name, entity, ctx, scratch),
        Expr::Index(base, _) => {
            if let Some(ty) = infer_index_value_sol_ty(base, entity, ctx, scratch) {
                return ty;
            }
            if let Expr::Ident(name) = base.as_ref() {
                if let Some(member) = entity.members.iter().find(|m| m.name == *name) {
                    if let Type::Generic(g, params) = &member.ty {
                        if (g == "HashMap" || g == "Vec") && !params.is_empty() {
                            return sol_type_entity(entity, params.last().unwrap(), true, ctx);
                        }
                    }
                }
            }
            "uint256".to_string()
        }
        // EVM-13: `m.keys()` (and the no-op `.collect()` wrapper) over
        // a HashMap member — let-binding type is `K[] memory`. Without
        // this, `let ks = m.keys();` defaults to `uint256` and fails
        // Solidity type-checking against the storage `K[]` array.
        Expr::MethodCall(_, method, _)
            if method == "exists" || method == "contains" || method == "is_empty" =>
        {
            "bool".to_string()
        }
        Expr::MethodCall(base, method, args) if method == "collect" && args.is_empty() => {
            infer_let_type_entity(base, entity, ctx, scratch)
        }
        Expr::MethodCall(base, method, args) if method == "keys" && args.is_empty() => {
            if let Expr::Ident(name) = base.as_ref() {
                if let Some(member) = entity.members.iter().find(|m| m.name == *name) {
                    if let Type::Generic(g, params) = &member.ty {
                        if g == "HashMap" && params.len() >= 1 {
                            return format!("{}[] memory", sol_type(&params[0], false));
                        }
                    }
                }
            }
            if let Expr::Index(inner, _) = base.as_ref() {
                if let Expr::Ident(name) = inner.as_ref() {
                    if let Some(k_ty) = nested_hashmap_inner_key_type(entity, name) {
                        return format!("{}[] memory", sol_type(&k_ty, false));
                    }
                }
            }
            "uint256".to_string()
        }
        // EVM-13 Batch D: `m.values()` (typically wrapped in
        // `.collect()` to materialise the lazy iterator into a
        // `V[] memory`) — let-binding type is `V[] memory`.
        Expr::MethodCall(base, method, args) if method == "values" && args.is_empty() => {
            if let Expr::Ident(name) = base.as_ref() {
                if let Some(member) = entity.members.iter().find(|m| m.name == *name) {
                    if let Type::Generic(g, params) = &member.ty {
                        if g == "HashMap" && params.len() >= 2 {
                            return format!("{}[] memory", sol_type(&params[1], false));
                        }
                    }
                }
            }
            "uint256".to_string()
        }
        // `record.field` — look the field up on the entity record (or
        // program record) so a `let v = m_record.list` whose `list`
        // field is a `Vec<U256>` gets `uint256[] memory` rather than the
        // generic `uint256` default. Falls back to `uint256` when the
        // base type cannot be resolved.
        Expr::FieldAccess(base, field) => {
            if let Expr::Ident(base_name) = base.as_ref() {
                if let Ok(idx) = field.parse::<usize>() {
                    if let Some(comp) = scratch.borrow().lookup_tuple_component(base_name, idx) {
                        if let Some(ty) = scratch.borrow().lookup_let_binding(&comp) {
                            return ty;
                        }
                    }
                    if let Some((en, var)) = scratch.borrow().lookup_payload_enum_binding(base_name)
                    {
                        if let Some(decl) = ctx.lookup_enum(&en) {
                            if let Some(v) = decl.variants.iter().find(|v| v.name == var) {
                                if let Some(f) = v.fields.get(idx) {
                                    return sol_type_entity(entity, f, true, ctx);
                                }
                            }
                        }
                    }
                }
                if let Some(rec_name) =
                    record_name_from_ident_base(entity, ctx, scratch, base_name)
                {
                    if let Some(f_ty) = lookup_record_field_type(entity, ctx, &rec_name, field) {
                        return sol_type_entity(entity, &f_ty, true, ctx);
                    }
                }
            }
            record_field_sol_type_of_base(base, field, entity, ctx, scratch)
                .unwrap_or_else(|| "uint256".to_string())
        }
        Expr::FnCall(name, _) => {
            if name == "addressOf" {
                "address".to_string()
            } else if let Some(ty) = ctx.lookup_pure_fn_return(name) {
                sol_type_entity(entity, &ty, true, ctx)
            } else {
                "uint256".to_string()
            }
        }
        Expr::MethodCall(base, method, args) if method == "split" && args.len() == 1 => {
            let base_ty = infer_let_type_entity(base, entity, ctx, scratch);
            if base_ty.starts_with("string") {
                "string[] memory".to_string()
            } else {
                "uint256".to_string()
            }
        }
        Expr::MethodCall(base, method, args)
            if (method == "len" || method == "length") && args.is_empty() =>
        {
            let _ = infer_let_type_entity(base, entity, ctx, scratch);
            "uint256".to_string()
        }
        // `Entity.address(id_args)` (deterministic-mode address oracle)
        // and `Entity.state(...)` shortcuts return an `address` value.
        Expr::MethodCall(base, method, _) if method == "address" => {
            if let Expr::Ident(n) = base.as_ref() {
                if n.chars().next().map_or(false, |c| c.is_uppercase()) {
                    return "address".to_string();
                }
            }
            "uint256".to_string()
        }
        Expr::AddressOf { .. } => "address".to_string(),
        Expr::NamespacedCall {
            namespace, name, ..
        } if namespace == "std::str" && name == "format" => "string memory".to_string(),
        Expr::NamespacedCall {
            namespace, name, ..
        } if namespace == "std::crypto" && name == "sha256" => "bytes memory".to_string(),
        Expr::NamespacedCall {
            namespace, name, ..
        } if namespace == "evm" => evm_intrinsic_sol_return_ty(name)
            .unwrap_or("uint256")
            .to_string(),
        Expr::For(pat, iter, body) => {
            super::iter::infer_for_expr_type_entity(pat, iter, body, entity, ctx, scratch)
        }
        _ => "uint256".to_string(),
    }
}

/// Infer per-slot Solidity types for a tuple-destructure RHS.
pub(crate) fn infer_tuple_elem_types(
    value: &Expr,
    entity: &Entity,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> Option<Vec<String>> {
    fn lower_ty_list(items: &[Type], entity: &Entity, ctx: &EvmCtx) -> Vec<String> {
        items
            .iter()
            .map(|t| sol_type_entity(entity, t, true, ctx))
            .collect()
    }

    match value {
        Expr::Tuple(items) => {
            let types: Vec<String> = items
                .iter()
                .map(|e| infer_let_type_entity(e, entity, ctx, scratch))
                .collect();
            Some(types)
        }
        Expr::FnCall(name, _) => {
            if name == "divmod" {
                return Some(vec!["uint256".to_string(), "uint256".to_string()]);
            }
            match ctx.lookup_pure_fn_return(name) {
                Some(Type::Tuple(items)) => Some(lower_ty_list(&items, entity, ctx)),
                _ => None,
            }
        }
        Expr::If(_, then, _) => infer_tuple_elem_types(then, entity, ctx, scratch),
        Expr::Block(items) => {
            let mut pushed: Vec<String> = Vec::new();
            let mut last = None;
            for item in items {
                if let Expr::Let(Pattern::Ident(name), val, _) = item {
                    let val_ty = infer_let_type_entity(val, entity, ctx, scratch);
                    scratch.borrow_mut().push_let_binding(name, &val_ty);
                    pushed.push(name.clone());
                }
                last = match item {
                    Expr::Let(_, _, body) => infer_tuple_elem_types(body, entity, ctx, scratch).or(last),
                    _ => infer_tuple_elem_types(item, entity, ctx, scratch).or(last),
                };
            }
            for name in pushed {
                scratch.borrow_mut().pop_let_binding(&name);
            }
            last
        }
        Expr::Let(Pattern::Ident(name), val, body) => {
            let val_ty = infer_let_type_entity(val, entity, ctx, scratch);
            scratch.borrow_mut().push_let_binding(name, &val_ty);
            let result = infer_tuple_elem_types(body, entity, ctx, scratch);
            scratch.borrow_mut().pop_let_binding(name);
            result
        }
        Expr::Let(_, _, body) => infer_tuple_elem_types(body, entity, ctx, scratch),
        _ => None,
    }
}
