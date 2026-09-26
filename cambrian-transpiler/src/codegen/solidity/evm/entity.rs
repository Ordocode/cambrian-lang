// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM codegen — entity contract assembly + payload-enum / wrapping helpers.

use super::analysis::member_inner_uses_exists;
use super::route::{
    gen_constructor_deterministic, gen_constructor_impl, gen_initialize_fn, gen_route_impl_ext,
    has_non_identity_init_params,
};
use crate::ast::{BinOp, Entity, EnumDecl, Expr, Program, RouteAction, Type};
use crate::codegen::solidity::core::ctx::EmitScope;
use crate::codegen::solidity::core::ctx::EvmCtx;
use crate::codegen::solidity::core::expr::gen_expr;
use crate::codegen::solidity::core::pure::gen_macro_fn;
use crate::codegen::solidity::core::scratch::EmitScratch;
use crate::codegen::solidity::core::state::{is_payload_enum, payload_enum_field_name};
use crate::codegen::solidity::core::types::*;
use std::cell::RefCell;

// ---------------------------------------------------------------------------
// Entity contract generation
// ---------------------------------------------------------------------------

/// Detect whether any expression inside `entity` uses `+%`, `-%`, or
/// `*%`. Drives the conditional emit of `_wadd`/`_wsub`/`_wmul` helpers
/// in `gen_entity_contract`.
pub(super) fn entity_uses_wrapping_ops(entity: &Entity) -> bool {
    fn has_wrap_action(a: &RouteAction) -> bool {
        match a {
            RouteAction::Let { value, .. } => has_wrap_expr(value),
            RouteAction::Return { values }
            | RouteAction::Send { args: values, .. }
            | RouteAction::Effect { args: values, .. }
            | RouteAction::Deploy {
                constructor_args: values,
                ..
            }
            | RouteAction::CallRoute { args: values, .. } => values.iter().any(has_wrap_expr),
            RouteAction::Conditional {
                condition,
                then_actions,
                else_actions,
            } => {
                has_wrap_expr(condition)
                    || then_actions.iter().any(has_wrap_action)
                    || else_actions.iter().any(has_wrap_action)
            }
            RouteAction::VarCall { args, dest, .. } => {
                args.iter().any(has_wrap_expr) || has_wrap_expr(dest)
            }
            RouteAction::Rescue { action, .. } => has_wrap_action(action),
            _ => false,
        }
    }
    for member in &entity.members {
        for t in &member.transforms {
            if has_wrap_expr(&t.body) {
                return true;
            }
        }
    }
    for route in &entity.routes {
        for wc in &route.where_clauses {
            if has_wrap_expr(&wc.condition) {
                return true;
            }
        }
        for action in route.body.all_actions() {
            if has_wrap_action(action) {
                return true;
            }
        }
    }
    false
}


/// True when a free `pure fn` or a library function uses `+%` / `-%` / `*%`.
/// Those bodies sit outside every contract, so the helpers are emitted at
/// file scope instead of per contract.
pub(super) fn pure_code_uses_wrapping_ops(program: &Program) -> bool {
    program.pure_fns.iter().any(|f| has_wrap_expr(&f.body))
        || program
            .libraries
            .iter()
            .flat_map(|l| &l.pure_fns)
            .any(|f| has_wrap_expr(&f.body))
}

fn has_wrap_expr(e: &Expr) -> bool {
    match e {
        Expr::BinOp(l, op, r) => {
            matches!(
                op,
                BinOp::WrappingAdd | BinOp::WrappingSub | BinOp::WrappingMul
            ) || has_wrap_expr(l)
                || has_wrap_expr(r)
        }
        Expr::UnaryOp(_, e)
        | Expr::FieldAccess(e, _)
        | Expr::Cast(e, _)
        | Expr::Some(e)
        | Expr::Closure(_, e) => has_wrap_expr(e),
        Expr::Index(b, i) => has_wrap_expr(b) || has_wrap_expr(i),
        Expr::MethodCall(b, _, args) => has_wrap_expr(b) || args.iter().any(has_wrap_expr),
        Expr::FnCall(_, args)
        | Expr::ArrayLit(args)
        | Expr::Tuple(args)
        | Expr::EnumVariantWithData(_, _, args)
        | Expr::MacroRef(_, args)
        | Expr::Block(args) => args.iter().any(has_wrap_expr),
        Expr::NamespacedCall { args, .. } => args.iter().any(has_wrap_expr),
        Expr::If(c, t, el) => {
            has_wrap_expr(c)
                || has_wrap_expr(t)
                || el.as_ref().map_or(false, |e| has_wrap_expr(e))
        }
        Expr::Match(s, arms) => has_wrap_expr(s) || arms.iter().any(|a| has_wrap_expr(&a.body)),
        Expr::Let(_, v, b) | Expr::For(_, v, b) => has_wrap_expr(v) || has_wrap_expr(b),
        Expr::RecordConstruct(_, fs) => fs.iter().any(|(_, v)| has_wrap_expr(v)),
        Expr::RecordUpdate(b, fs) => {
            has_wrap_expr(b) || fs.iter().any(|(_, v)| has_wrap_expr(v))
        }
        Expr::Range(a, b) => has_wrap_expr(a) || has_wrap_expr(b),
        _ => false,
    }
}

/// EVM-G-U2: Solidity 0.8.x traps on every arithmetic overflow, but
/// Cambrian's `+%` / `-%` / `*%` operators are *defined* to wrap. Funnel
/// them through these helpers so the wrap semantics live in exactly one
/// `unchecked { }` block per op. `file_scope` emits free functions (visible
/// to `pure fn`s and libraries) instead of contract members.
pub(super) fn gen_wrapping_op_helpers(file_scope: bool) -> String {
    let (indent, vis) = if file_scope { ("", "") } else { ("    ", "internal ") };
    let mut out = String::new();
    for (name, ty, op) in [
        ("_wadd", "uint256", "+"),
        ("_wsub", "uint256", "-"),
        ("_wmul", "uint256", "*"),
        ("_wadds", "int256", "+"),
        ("_wsubs", "int256", "-"),
        ("_wmuls", "int256", "*"),
    ] {
        out.push_str(&format!(
            "{indent}function {name}({ty} a, {ty} b) {vis}pure returns ({ty}) {{ unchecked {{ return a {op} b; }} }}\n"
        ));
    }
    out.push('\n');
    out
}

pub(super) fn gen_entity_contract(
    entity: &Entity,
    program: &Program,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let scope = EmitScope::for_entity(entity);
    let mut out = String::new();

    out.push_str(&format!("contract {} {{\n", entity.name));

    // Enums / structs in dependency order (P2 kernel graphs).
    let graphs = crate::analysis::ProgramGraphs::build(program);
    for item in graphs.resolve_entity_types(entity) {
        match item {
            crate::analysis::TypeItem::Enum(e) => {
                out.push_str(&gen_enum_decl(e));
                out.push('\n');
            }
            crate::analysis::TypeItem::Record(rec) => {
                out.push_str(&format!("    struct {} {{\n", rec.name));
                for field in &rec.fields {
                    out.push_str(&format!(
                        "        {} {};\n",
                        sol_type_entity(entity, &field.ty, false, ctx),
                        field.name
                    ));
                }
                out.push_str("    }\n\n");
            }
        }
    }

    // Phase EVM-P0-C: entity-scope events lower to Solidity `event`
    // declarations inside the contract.
    for ev in &entity.events {
        let params: Vec<String> = ev
            .params
            .iter()
            .map(|p| {
                let ty = sol_type_entity(entity, &p.ty, true, ctx);
                if p.indexed {
                    format!("{} indexed {}", ty, p.name)
                } else {
                    format!("{} {}", ty, p.name)
                }
            })
            .collect();
        out.push_str(&format!("    event {}({});\n", ev.name, params.join(", ")));
    }
    if !entity.events.is_empty() {
        out.push('\n');
    }

    // Phase EVM-P0-D: entity-scope errors lower to Solidity `error`
    // declarations inside the contract.
    for er in &entity.errors {
        let params: Vec<String> = er
            .params
            .iter()
            .map(|p| {
                let ty = sol_type_entity(entity, &p.ty, true, ctx);
                format!("{} {}", ty, p.name)
            })
            .collect();
        out.push_str(&format!("    error {}({});\n", er.name, params.join(", ")));
    }
    if !entity.errors.is_empty() {
        out.push('\n');
    }

    // Constants — render with declared type (address consts need explicit wrap).
    for c in &entity.constants {
        let ty = sol_type_entity(entity, &c.ty, false, ctx);
        let val = match &c.value {
            Expr::IntLiteral(v) if ty == "address" => hex_literal_to_sol_for_ty(
                &crate::ast::u256_hex_digits(v),
                Some("address"),
            ),
            _ => {
                let raw =
                    gen_expr(&c.value, ctx, &scope, scratch).unwrap_or_else(|| "0".to_string());
                maybe_narrow_cast(&ty, &raw, &c.value, entity, ctx, scratch)
            }
        };
        out.push_str(&format!("    {} public constant {} = {};\n", ty, c.name, val));
    }
    if !entity.constants.is_empty() {
        out.push('\n');
    }

    // In deterministic mode, emit _factory immutable before other members
    if ctx.is_deterministic_mode() {
        out.push_str("    address public immutable _factory;\n");
    }

    // Members (identity → immutable, mappings → storage only, others → public)
    for member in &entity.members {
        if member.is_identity {
            let id_ty = sol_type_entity(entity, &member.ty, false, ctx);
            if id_ty.starts_with("string") || id_ty.contains("[]") {
                out.push_str(&format!("    {} public {};\n", id_ty, member.name));
            } else {
                out.push_str(&format!(
                    "    {} public immutable {};\n",
                    id_ty,
                    member.name
                ));
            }
        } else if is_mapping_type(&member.ty) {
            out.push_str(&format!(
                "    {} public {};\n",
                sol_type_entity(entity, &member.ty, false, ctx),
                member.name
            ));
            // EVM-13: when the HashMap is iterated anywhere in the entity
            // (`.keys()` / `.values()` / `.iter()`), emit the parallel
            // key-index array + reverse-index lookup, and force the
            // existence-flag mapping (used to detect first-time inserts
            // for the push step). When the map is *not* iterated, only
            // emit `_exists` if explicit `.exists()` calls require it
            // (preserves the prior gas-cheap layout for non-iterated
            // maps).
            let iterated = ctx.hashmap_member_is_iterated(entity, &member.name);
            // Read the decision from the program context rather than
            // recomputing it: the insert/update and remove lowering in
            // `transform` read the same entry, so a sidecar can never be
            // declared and read without also being written.
            let needs_exists = ctx.member_has_exists_sidecar(&entity.name, &member.name);
            if let Type::Generic(_, params) = &member.ty {
                if params.len() >= 1 {
                    let key_ty = sol_type(&params[0], false);
                    if needs_exists {
                        out.push_str(&format!(
                            "    mapping({} => bool) public {}_exists;\n",
                            key_ty, member.name
                        ));
                    }
                    // EVM-3 Batch G2: 2-level sidecar for nested
                    // HashMap members whose inner mapping is queried
                    // via `.exists()` (directly or through a
                    // let-bound alias). Only emitted for mappings
                    // shaped `HashMap<K1, HashMap<K2, V>>`.
                    if params.len() == 2
                        && is_mapping_type(&params[1])
                        && member_inner_uses_exists(entity, &member.name)
                    {
                        if let Type::Generic(_, inner_params) = &params[1] {
                            if inner_params.len() == 2 {
                                let inner_key_ty = sol_type(&inner_params[0], false);
                                out.push_str(&format!(
                                    "    mapping({} => mapping({} => bool)) public {}_inner_exists;\n",
                                    key_ty, inner_key_ty, member.name
                                ));
                            }
                        }
                    }
                    if iterated {
                        // Public getter on the key array gives users a
                        // cheap on-chain reader (auto-generated by
                        // Solidity for `T[] public`). The reverse-index
                        // mapping stays private — it's an
                        // implementation detail of swap-pop removal.
                        out.push_str(&format!("    {}[] public {}_keys;\n", key_ty, member.name));
                        out.push_str(&format!(
                            "    mapping({} => uint256) private {}_keys_index;\n",
                            key_ty, member.name
                        ));
                    }
                }
            }
        } else if type_contains_array_storage(entity, &member.ty, ctx) {
            out.push_str(&format!(
                "    {} {};\n",
                sol_type_entity(entity, &member.ty, false, ctx),
                member.name
            ));
        } else {
            out.push_str(&format!(
                "    {} public {};\n",
                sol_type_entity(entity, &member.ty, false, ctx),
                member.name
            ));
        }
    }

    // _initialized flag only needed when we'll emit initialize()
    let needs_initialize = ctx.is_deterministic_mode() && has_non_identity_init_params(entity);
    if needs_initialize {
        out.push_str("    bool private _initialized;\n");
    }

    if ctx.is_deterministic_mode() || !entity.members.is_empty() {
        out.push('\n');
    }

    // Constructor
    if ctx.is_deterministic_mode() {
        out.push_str(&gen_constructor_deterministic(
            entity, program, ctx, scratch,
        ));
    } else {
        out.push_str(&gen_constructor_impl(entity, program, ctx, scratch));
    }
    out.push('\n');

    // Initialize function (deterministic mode only, when needed)
    if needs_initialize {
        out.push_str(&gen_initialize_fn(entity, program, ctx, scratch));
        out.push('\n');
    }

    // Wrapping arithmetic helpers (G-U2). Emitted only when the entity
    // actually uses `+%` / `-%` / `*%` so contracts that don't need
    // wrap-around math stay clean.
    if entity_uses_wrapping_ops(entity) && !pure_code_uses_wrapping_ops(program) {
        out.push_str(&gen_wrapping_op_helpers(false));
    }

    // Macros (Phase EVM-5)
    for mac in &entity.macros {
        out.push_str(&gen_macro_fn(mac, entity, ctx, scratch));
        out.push('\n');
    }

    // Routes
    for route in &entity.routes {
        let body = gen_route_impl_ext(entity, route, program, &graphs, ctx, scratch);
        if !body.is_empty() {
            out.push_str(&body);
            out.push('\n');
        }
    }

    let inv_helpers =
        crate::codegen::predicate_expr::invariant_check_helpers_for_entity(program, entity, ctx);
    out.push_str(&crate::codegen::predicate_expr::emit_invariant_check_helpers(
        &inv_helpers,
    ));

    out.push_str("}\n");
    out
}

pub(super) fn gen_enum_decl(e: &EnumDecl) -> String {
    if is_payload_enum(e) {
        return gen_payload_enum_struct(e, "    ");
    }
    let variants: Vec<&str> = e.variants.iter().map(|v| v.name.as_str()).collect();
    format!("    enum {} {{ {} }}\n", e.name, variants.join(", "))
}

/// Phase EVM-4 J1: emit a Solidity tagged union for a Cambrian enum
/// with payload variants:
///
///   enum <N>_Tag { v1, v2, ... }
///   struct <N> {
///       <N>_Tag tag;
///       <ty> v1_0;
///       <ty> v2_0;
///       <ty> v2_1;
///       ...
///   }
///
/// `indent` is the leading whitespace (4 spaces inside a contract,
/// empty at file scope).
pub(super) fn gen_payload_enum_struct(e: &EnumDecl, indent: &str) -> String {
    let tag_name = format!("{}_Tag", e.name);
    let variant_names: Vec<&str> = e.variants.iter().map(|v| v.name.as_str()).collect();
    let mut out = String::new();
    out.push_str(&format!(
        "{}enum {} {{ {} }}\n",
        indent,
        tag_name,
        variant_names.join(", ")
    ));
    out.push_str(&format!("{}struct {} {{\n", indent, e.name));
    out.push_str(&format!("{}    {} tag;\n", indent, tag_name));
    for v in &e.variants {
        for (i, fty) in v.fields.iter().enumerate() {
            let fname = payload_enum_field_name(v, i);
            out.push_str(&format!(
                "{}    {} {};\n",
                indent,
                sol_type(fty, false),
                fname
            ));
        }
    }
    out.push_str(&format!("{}}}\n", indent));
    out
}
