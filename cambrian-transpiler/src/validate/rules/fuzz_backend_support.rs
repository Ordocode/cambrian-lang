// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! W7: fuzz parameter type support warnings for the *selected* target's
//! fuzz backends (Targeted binding — needs a concrete target to know which
//! backends to name in the message).

use crate::target::{Domain, Target};

use super::super::registry::ValidateCtx;
use super::super::test_checks::{self, FuzzBackendKind};
use super::super::Diagnostic;

#[allow(dead_code)]
pub const CODES: &[&str] = &["W7"];

fn backends_for(target: Target) -> Vec<FuzzBackendKind> {
    match target.domain() {
        Domain::Evm => vec![FuzzBackendKind::Foundry],
        Domain::Tvm => vec![FuzzBackendKind::AckiNacki],
        Domain::Container => Vec::new(),
    }
}

pub fn run(ctx: &ValidateCtx<'_>, diags: &mut Vec<Diagnostic>) {
    let Some(target) = ctx.target else {
        return;
    };
    let backends = backends_for(target);
    if backends.is_empty() {
        return;
    }
    test_checks::emit_w7_for_backends(ctx.program, &backends, diags);
}
