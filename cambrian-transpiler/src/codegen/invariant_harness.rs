// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Shared invariant harness helpers (setup deploy vs trace senders).

use crate::ast::{Expr, InvariantDecl};

/// Explicit `deploy { D }` when present, otherwise legacy `senders[0]`.
pub(crate) fn effective_deploy(inv: &InvariantDecl) -> Option<&Expr> {
    inv.deploy.first().or_else(|| inv.senders.first())
}

pub(crate) fn has_explicit_deploy(inv: &InvariantDecl) -> bool {
    !inv.deploy.is_empty()
}
