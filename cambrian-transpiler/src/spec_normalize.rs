// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Spec-step lint helpers (`test` / `property` / `fuzz` bodies).
//!
//! When authors write `call constructor()` immediately followed by
//! `msg { sender: … }`, the init route executes under the prior `MsgCtx`
//! (often `default` with `sender = 0`). Canonical order is `msg` first
//! (see `docs/LANGUAGE.md`); validator **W11** warns on the bad pattern.

use crate::ast::{Entity, TestStep};

/// True when `route` names this entity's init / constructor route.
pub fn is_init_route_call(entity: &Entity, route: &str) -> bool {
    if route == "constructor" {
        return entity
            .routes
            .iter()
            .any(|r| r.is_init || r.name == "constructor");
    }
    entity
        .routes
        .iter()
        .any(|r| r.name == route && (r.is_init || r.name == "constructor"))
}

/// True when `steps` contain `call <init-route>` immediately followed by
/// `msg { sender: … }` (the pattern warned by **W11**).
pub fn has_ctor_before_msg_pattern(entity: &Entity, steps: &[TestStep]) -> bool {
    let mut i = 0;
    while i + 1 < steps.len() {
        if let TestStep::Call { route, .. } = &steps[i] {
            if is_init_route_call(entity, route) {
                if let TestStep::SetContext { namespace, fields } = &steps[i + 1] {
                    if namespace == "msg"
                        && fields.iter().any(|(name, _)| name == "sender")
                    {
                        return true;
                    }
                }
            }
        }
        i += 1;
    }
    false
}
