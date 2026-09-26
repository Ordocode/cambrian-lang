// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Per-rule validation registry (P7).
//!
//! Every diagnostic code is owned by exactly one [`RuleEntry`] that declares
//! a [`RuleBinding`]. Dispatch runs a rule iff `binding.applies(target)`.
//! Adding a rule = new unit + registry entry (binding is mandatory).

use crate::ast::Program;
use crate::target::{Domain, Language, Target};

use super::Diagnostic;

/// Where a validation rule binds in the (domain × language) matrix.
///
/// Domain bindings are *sets* so a rule states which domains model the
/// construct, not which single target it happens to fire on today. Example:
/// `msg::pubkey` belongs to the TVM message model, which both `Domain::Tvm`
/// and `Domain::Container` (the TVM test-bed runtime) implement — so its
/// rejection rule is `NotDomains(&[Tvm, Container])`. Today that fires only
/// on the EVM domain, but a future domain without the TVM message model
/// inherits the rejection automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleBinding {
    /// Kernel / universal — runs for every target (and for target-less `validate()`).
    Universal,
    /// Runs for every concrete target in `check_target_compat`, but not in
    /// target-less `validate()` (e.g. W7, whose message names a backend).
    Targeted,
    /// Domain semantics — fires when the target's domain is in the set.
    Domains(&'static [Domain]),
    /// Fires when the target's domain is *not* in the set (the listed
    /// domains are the ones that support the construct).
    NotDomains(&'static [Domain]),
    /// Carrier-language expressivity (SolidityCore / LeanCore / RustCore).
    Language(Language),
    /// Adapter-pair contract (e.g. Lean-EVM atomic-step rules L8–L11).
    Pair(Domain, Language),
}

impl RuleBinding {
    /// Whether this rule should run for `target`.
    pub fn applies(self, target: Target) -> bool {
        match self {
            RuleBinding::Universal | RuleBinding::Targeted => true,
            RuleBinding::Domains(ds) => ds.contains(&target.domain()),
            RuleBinding::NotDomains(ds) => !ds.contains(&target.domain()),
            RuleBinding::Language(l) => target.language() == l,
            RuleBinding::Pair(d, l) => target.domain() == d && target.language() == l,
        }
    }

    /// Target-less `validate()` mode: only universal rules.
    pub fn applies_universal_only(self) -> bool {
        matches!(self, RuleBinding::Universal)
    }

    /// Whether dispatch depends only on the domain axis (used by the
    /// domain-parity gate: such rules must agree across same-domain cores).
    pub fn is_domain_bound(self) -> bool {
        matches!(
            self,
            RuleBinding::Universal | RuleBinding::Domains(_) | RuleBinding::NotDomains(_)
        )
    }
}

/// Shared context passed to every rule's `run` function.
pub struct ValidateCtx<'a> {
    pub program: &'a Program,
    pub target: Option<Target>,
    pub deterministic: bool,
}

impl<'a> ValidateCtx<'a> {
    pub fn with_target(program: &'a Program, target: Target, deterministic: bool) -> Self {
        Self {
            program,
            target: Some(target),
            deterministic,
        }
    }

    pub fn universal_only(program: &'a Program) -> Self {
        Self {
            program,
            target: None,
            deterministic: false,
        }
    }

    pub fn target_name(&self) -> &'static str {
        self.target.map(|t| t.name()).unwrap_or("unknown")
    }
}

/// One independently dispatchable validation rule.
pub struct RuleEntry {
    /// Singleton diagnostic code this entry may emit (PM-031: one code per entry).
    pub codes: &'static [&'static str],
    pub binding: RuleBinding,
    pub run: fn(&ValidateCtx<'_>, &mut Vec<Diagnostic>),
}

/// Look up the binding declared for `code`. Panics if the code is unregistered
/// (completeness is enforced by unit tests).
pub fn rule_binding(code: &str) -> RuleBinding {
    for entry in RULES {
        if entry.codes.contains(&code) {
            return entry.binding;
        }
    }
    panic!("diagnostic code {code:?} has no RuleEntry — add it to RULES");
}

/// Run every rule whose binding applies to `ctx`.
pub fn run_rules(ctx: &ValidateCtx<'_>, diags: &mut Vec<Diagnostic>) {
    super::rules::clear_family_cache();
    for entry in RULES {
        let applies = match ctx.target {
            Some(t) => entry.binding.applies(t),
            None => entry.binding.applies_universal_only(),
        };
        if applies {
            (entry.run)(ctx, diags);
        }
    }
    super::rules::clear_family_cache();
}

/// All registered validation rules. Bindings here are the single source of
/// truth for dispatch; see `docs/plans/p7-validations-by-domain.md`.
///
/// Initial (Steps B–E) bindings reproduce pre-P7 gates. Step F flips
/// Solidity-language and Lean-language entries.
pub use super::rules::RULES;
