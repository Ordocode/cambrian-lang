// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! CREATE2 address expression (deterministic deployment mode).

/// Generate the CREATE2 address computation expression for an entity.
/// `identity_exprs` are the Solidity expressions for identity arg values.
///
/// Routes through `ICambrianFactory(_factory).predict{Entity}(...)` rather
/// than inlining `type(Entity).creationCode`. Embedding peer
/// `creationCode` inside entity bytecode creates solc circularity
/// (Error 7813) whenever two entities mutually `from` / send to each
/// other — the factory already owns the creationCode references, so
/// `predict*` is the safe call site.
pub(crate) fn gen_create2_address_expr(entity_name: &str, identity_exprs: &[String]) -> String {
    format!(
        "ICambrianFactory(_factory).predict{}({})",
        entity_name,
        identity_exprs.join(", ")
    )
}
