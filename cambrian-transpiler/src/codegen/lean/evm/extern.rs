// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean codegen — `extern entity` axiomatic interfaces (P4b.4).

use crate::ast::{ExternEntity, Program};

use super::super::core::types::{lower_type, LeanTypeCtx};

/// Emit `Cambrian/Generated/Extern.lean` with opaque route signatures
/// for every `extern entity` declaration.
pub fn gen_extern_module(program: &Program, profile: super::super::LeanProfile) -> String {
    if program.extern_entities.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str("/-\n  Auto-generated extern-entity axioms (P4b).\n-/\n\n");
    out.push_str("import Cambrian.Prelude\n");
    out.push_str("import Cambrian.Generated.World\n\n");
    out.push_str("namespace Cambrian.Generated.Extern\n\n");

    for ext in &program.extern_entities {
        emit_extern_entity(&mut out, program, ext, profile);
    }

    out.push_str("end Cambrian.Generated.Extern\n");
    out
}

fn emit_extern_entity(
    out: &mut String,
    program: &Program,
    ext: &ExternEntity,
    profile: super::super::LeanProfile,
) {
    let ctx = LeanTypeCtx::top_level(program, profile);
    out.push_str(&format!("namespace {}\n\n", ext.name));
    out.push_str("namespace Routes\n\n");
    for route in &ext.routes {
        let ret = route
            .return_type
            .as_ref()
            .map(|t| lower_type(t, &ctx))
            .unwrap_or_else(|| "Unit".to_string());
        // PN-106: high-level ABI CALL can revert — opaque returns Except,
        // not a total World × T (void: Except World).
        let result_ty = if route.return_type.is_some() {
            format!("Cambrian.Generated.World × {}", ret)
        } else {
            "Cambrian.Generated.World".to_string()
        };
        out.push_str(&format!(
            "/-- Axiomatic foreign route `{0}.{1}` (may revert — Except). -/\nopaque {1} (w : Cambrian.Generated.World) (ctx : Cambrian.MsgCtx)",
            ext.name, route.name,
        ));
        for p in &route.params {
            out.push_str(&format!(" ({} : {})", p.name, lower_type(&p.ty, &ctx)));
        }
        out.push_str(&format!(
            " : Except Cambrian.ThrowCode ({})",
            result_ty
        ));
        out.push_str("\n\n");
    }
    out.push_str("end Routes\n\n");
    out.push_str(&format!("end {}\n\n", ext.name));
}
