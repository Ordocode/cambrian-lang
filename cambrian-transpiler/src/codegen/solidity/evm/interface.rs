// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM codegen — Solidity interface emission for typed targets.

use super::route::gen_params_entity;
use crate::ast::{Entity, ExternEntity, Type};
use crate::codegen::solidity::core::types::*;
use crate::analysis::route_infer_evm_view;

// ---------------------------------------------------------------------------
// Interface declarations (Phase EVM-6)
// ---------------------------------------------------------------------------

pub(super) fn gen_interface_decl(
    target_name: &str,
    target_entity: &Entity,
    ctx: &crate::codegen::solidity::core::ctx::EvmCtx,
) -> String {
    let mut out = format!("interface I{} {{\n", target_name);
    for route in &target_entity.routes {
        if route.is_init
            || route.name == "constructor"
            || route.is_private
            || route.recover_tag.is_some()
        {
            continue;
        }
        // Match impl payable detection: `accept` *or* any `msg::value` read
        // (EVM-H4 — typed send `{value:}` must compile against the interface).
        let visibility_suffix = if route.is_pure {
            " pure"
        } else if route.is_view || route_infer_evm_view(target_entity, route) {
            " view"
        } else if route.is_accept || route_uses_msg_value(route, target_entity) {
            " payable"
        } else {
            ""
        };
        let ret = match &route.return_type {
            Some(ty) => format!(" returns ({})", sol_return_type(target_entity, ty, ctx)),
            None => String::new(),
        };
        out.push_str(&format!(
            "    function {}({}) external{}{};\n",
            sol_sanitize_ident(&route.name),
            gen_params_entity(target_entity, &route.params, ctx),
            visibility_suffix,
            ret
        ));
    }
    out.push_str("}\n");
    out
}

/// Phase EVM-6 M1: emit a Solidity `interface I<Name> { ... }` from a
/// user-declared `extern entity Name { route ... ; }` block. Mirrors
/// `gen_interface_decl` but reads from `ExternRoute` instead of full
/// `Route` declarations.
pub(super) fn gen_extern_interface_decl(ext: &ExternEntity) -> String {
    let mut out = format!("interface I{} {{\n", ext.name);
    for route in &ext.routes {
        let visibility_suffix = if route.is_view {
            " view"
        } else if route.is_payable {
            " payable"
        } else {
            ""
        };
        let params = route
            .params
            .iter()
            .map(|p| format!("{} {}", sol_type(&p.ty, true), sol_sanitize_ident(&p.name)))
            .collect::<Vec<_>>()
            .join(", ");
        let ret = match &route.return_type {
            Some(ty_ref) => {
                let rendered = match ty_ref {
                    Type::Tuple(items) => items
                        .iter()
                        .map(|t| sol_type(t, true))
                        .collect::<Vec<_>>()
                        .join(", "),
                    other => sol_type(other, true),
                };
                format!(" returns ({})", rendered)
            }
            None => String::new(),
        };
        out.push_str(&format!(
            "    function {}({}) external{}{};\n",
            sol_sanitize_ident(&route.name),
            params,
            visibility_suffix,
            ret
        ));
    }
    out.push_str("}\n");
    out
}
