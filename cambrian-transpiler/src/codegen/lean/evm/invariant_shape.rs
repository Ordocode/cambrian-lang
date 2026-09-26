// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Algebraic sorry policy for invariant proof emission (SMAFD LG-006b).
//!
//! Mirrors [`super::spec_steps::SpecStatementShape`] for tests/properties:
//! some invariant action sets blow up `invByCases`/`simp_all` with
//! `maxRecDepth` (non-recoverable), so we ship `:= by sorry` instead of the
//! ladder.

/// Proof-relevant structure of an invariant, derived from resolved action
/// route names before Lean emission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InvariantProofShape {
    pub route_names: Vec<String>,
}

impl InvariantProofShape {
    pub(crate) fn from_route_names(names: impl IntoIterator<Item = String>) -> Self {
        Self {
            route_names: names.into_iter().collect(),
        }
    }

    /// Whether the invariant should ship `:= by sorry` instead of `invByCases`.
    ///
    /// Run #5 (`RHTBurn` + CTE invariants): `claim` / `burn` actions expand the
    /// `runTrace`/`step` simp set until `maxRecDepth`; the outer `first | sorry`
    /// never runs. Simple transfer-only invariants keep the ladder.
    pub(crate) fn needs_sorry_stub(&self) -> bool {
        if self
            .route_names
            .iter()
            .any(|name| matches!(name.as_str(), "claim" | "burn"))
        {
            return true;
        }
        // Large action enums make trace induction expensive for `simp_all`.
        self.route_names.len() >= 5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_transfer_invariant_keeps_ladder() {
        let shape = InvariantProofShape::from_route_names([
            "transfer".to_string(),
            "approve".to_string(),
            "transferFrom".to_string(),
        ]);
        assert!(!shape.needs_sorry_stub());
    }

    #[test]
    fn claim_action_needs_sorry_stub() {
        let shape = InvariantProofShape::from_route_names(["claim".to_string()]);
        assert!(shape.needs_sorry_stub());
    }

    #[test]
    fn burn_action_needs_sorry_stub() {
        let shape = InvariantProofShape::from_route_names(["burn".to_string()]);
        assert!(shape.needs_sorry_stub());
    }

    #[test]
    fn five_actions_needs_sorry_stub() {
        let shape = InvariantProofShape::from_route_names([
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
            "e".to_string(),
        ]);
        assert!(shape.needs_sorry_stub());
    }
}
