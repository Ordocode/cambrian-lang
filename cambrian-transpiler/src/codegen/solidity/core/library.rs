// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase Library-3: EVM codegen for Cambrian `library` declarations.
//!
//! Cambrian:
//!     library SafeMath {
//!         pure fn add(a: u256, b: u256) -> u256 { a + b }
//!         const MAX: u256 = ...
//!         type Amount = u256
//!     }
//!
//! Solidity:
//!     library SafeMath {
//!         function add(uint256 a, uint256 b) internal pure returns (uint256) {
//!             return a + b;
//!         }
//!     }
//!
//! Free-function `pure fn`s at program scope continue to lower via
//! [`evm_pure::gen_program_pure_fns`] (inlined at every call site).
//! Library-scoped `pure fn`s lower to `internal` library functions
//! (jumped to once per call, saving bytecode for helpers used in
//! many entities). The two surfaces are intentionally separate so
//! existing fixtures keep their free-function lowering.

use super::ctx::{EmitScope, EvmCtx};
use super::expr::gen_expr_hoisted;
use super::scratch::EmitScratch;
use super::types::*;
use crate::ast::{Entity, LibraryDecl, Member, Program, Type};
use std::cell::RefCell;

/// Emit `library <Name> { ... }` for every library in the program.
pub(crate) fn gen_program_libraries(
    program: &Program,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    if program.libraries.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for lib in &program.libraries {
        out.push_str(&gen_library(lib, ctx, scratch));
        out.push('\n');
    }
    out
}

fn gen_library(lib: &LibraryDecl, ctx: &EvmCtx, scratch: &RefCell<EmitScratch>) -> String {
    let mut out = format!("library {} {{\n", lib.name);

    // type aliases first (so subsequent fns can reference them)
    for ta in &lib.type_aliases {
        out.push_str(&format!(
            "    type {} is {};\n",
            ta.name,
            user_defined_value_type_base(&ta.ty),
        ));
    }
    if !lib.type_aliases.is_empty() {
        out.push('\n');
    }

    // constants
    for c in &lib.constants {
        let synthetic = synthetic_entity(&lib.name);
        let scope = EmitScope::for_entity(&synthetic);
        let (stmts, val) = gen_expr_hoisted(&c.value, &synthetic, ctx, &scope, scratch);
        // Library constants don't allow non-trivial initialization
        // (Solidity requires compile-time constants). If hoisting
        // produced statements, fall back to inlining `val` directly
        // and let solc reject any non-constant expressions with a
        // clear diagnostic — this matches the validator's expectation
        // that V47 / V48 reject impure expressions earlier.
        debug_assert!(stmts.is_empty(), "library const cannot have hoisted stmts");
        out.push_str(&format!(
            "    {} constant {} = {};\n",
            sol_type(&c.ty, false),
            c.name,
            val,
        ));
    }
    if !lib.constants.is_empty() {
        out.push('\n');
    }

    // pure fns
    for pf in &lib.pure_fns {
        let exists_params = ctx
            .lookup_pure_fn_exists_params(&pf.name, Some(&lib.name))
            .unwrap_or_default();
        let exists_param_set: std::collections::HashSet<&str> =
            exists_params.iter().map(|(_, n)| n.as_str()).collect();
        let keys_params = ctx
            .lookup_pure_fn_keys_params(&pf.name, Some(&lib.name))
            .unwrap_or_default();
        let keys_param_set: std::collections::HashSet<&str> =
            keys_params.iter().map(|(_, n)| n.as_str()).collect();

        let synthetic = Entity {
            name: format!("lib_{}_{}", lib.name, pf.name),
            records: vec![],
            enums: vec![],
            type_aliases: vec![],
            constants: vec![],
            events: vec![],
            errors: vec![],
            macros: vec![],
            routes: vec![],
            members: pf
                .params
                .iter()
                .map(|p| Member {
                    name: p.name.clone(),
                    ty: p.ty.clone(),
                    is_identity: false,
                    default_value: None,
                    transforms: vec![],
                    span: crate::ast::Span::none(),
                })
                .collect(),
            span: crate::ast::Span::none(),
        };

        let (stmts, val) =
            super::pure::gen_pure_fn_body_hoisted(pf, &synthetic, ctx, scratch);
        let ret = match &pf.return_type {
            Type::Tuple(items) => items
                .iter()
                .map(|t| {
                    if is_mapping_type(t) {
                        format!(
                            "{} storage",
                            sol_type_entity(&synthetic, t, false, ctx)
                        )
                    } else {
                        sol_type_entity(&synthetic, t, true, ctx)
                    }
                })
                .collect::<Vec<_>>()
                .join(", "),
            _ if is_mapping_type(&pf.return_type) => format!(
                "{} storage",
                sol_type_entity(&synthetic, &pf.return_type, false, ctx)
            ),
            _ => sol_type_entity(&synthetic, &pf.return_type, true, ctx),
        };
        let return_val =
            coerce_return_value(&ret, val, &pf.body, &synthetic, ctx, scratch);
        let body_src = format!("{}\n{}", stmts.join("\n"), return_val);
        let mut params: Vec<String> = Vec::new();
        for p in &pf.params {
            let p_name = sol_sanitize_ident(&p.name);
            if is_mapping_type(&p.ty) {
                params.extend(format_hashmap_storage_params(
                    &p.ty,
                    &p_name,
                    &body_src,
                    exists_param_set.contains(p.name.as_str()),
                    keys_param_set.contains(p.name.as_str()),
                ));
            } else {
                params.push(format!(
                    "{} {}",
                    sol_type_entity(&synthetic, &p.ty, true, ctx),
                    p_name
                ));
            }
        }
        let has_mapping_param = pf.params.iter().any(|p| is_mapping_type(&p.ty));
        let purity = if has_mapping_param {
            "internal view"
        } else {
            "internal pure"
        };

        out.push_str(&format!(
            "    function {}({}) {} returns ({}) {{\n",
            pf.name,
            params.join(", "),
            purity,
            ret
        ));
        for s in &stmts {
            out.push_str(&format!("        {}\n", s));
        }
        if !stmts
            .iter()
            .any(|s| s.trim_start().starts_with("revert("))
        {
            out.push_str(&format!("        return {};\n", return_val));
        }
        out.push_str("    }\n");
    }

    out.push_str("}\n");
    out
}

pub(crate) fn synthetic_entity(prefix: &str) -> Entity {
    Entity {
        name: format!("lib_const_{}", prefix),
        records: vec![],
        enums: vec![],
        type_aliases: vec![],
        constants: vec![],
        events: vec![],
        errors: vec![],
        macros: vec![],
        routes: vec![],
        members: vec![],
        span: crate::ast::Span::none(),
    }
}

/// Solidity user-defined value types only accept a *primitive* base.
/// For `type Foo = u256;` we emit `type Foo is uint256;`. For more
/// complex aliases we fall back to a comment placeholder; the
/// validator (V47) is responsible for surfacing unsupported aliases.
fn user_defined_value_type_base(ty: &Type) -> String {
    match ty {
        Type::Simple(_) => sol_type(ty, false),
        _ => "uint256".to_string(),
    }
}
