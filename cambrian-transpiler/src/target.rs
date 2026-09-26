// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Compilation target as an explicit (domain × language) pair.
//!
//! CLI / YAML names (`native`, `wasm`, `ackinacki`, `evm`, `lean`) are
//! aliases for the five supported cells of that matrix. Capability
//! methods replace the historical `target == "…"` / `target_models_evm`
//! string matches in the pipeline.

/// Execution-domain axis: what a step / send / address means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Domain {
    /// Non-blockchain host (`cambrian-runtime`).
    Container,
    /// TVM / Acki Nacki message model.
    Tvm,
    /// EVM synchronous call / CREATE2 / balance model.
    Evm,
}

/// Carrier-language axis: what syntax the emitter writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Solidity,
    Lean,
}

/// Named compilation target (a supported domain × language cell).
///
/// Acki Nacki is `Domain::Tvm` × `Language::Rust` for the WASM logic, plus a
/// domain-private TVM-Solidity wrapper artifact (not a second `Language`
/// cell).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    Native,
    Wasm,
    AckiNacki,
    Evm,
    Lean,
}

impl Target {
    /// Every supported target. Domain membership is derived via [`Self::domain`];
    /// test harnesses must iterate this (or [`Self::cores_of`]) instead of
    /// hardcoding core pairs, so a new core activates shared-fixture gates
    /// automatically.
    pub const ALL: [Target; 5] = [
        Target::Native,
        Target::Wasm,
        Target::AckiNacki,
        Target::Evm,
        Target::Lean,
    ];

    /// Parse a CLI / `project.yaml` target name. Unknown names return `None`
    /// (callers must error — silent fallback to native is no longer allowed).
    /// Without `rust-targets`, `native` / `wasm` / `ackinacki` are unknown.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            #[cfg(feature = "rust-targets")]
            "native" => Some(Self::Native),
            #[cfg(feature = "rust-targets")]
            "wasm" => Some(Self::Wasm),
            #[cfg(feature = "rust-targets")]
            "ackinacki" => Some(Self::AckiNacki),
            "evm" => Some(Self::Evm),
            "lean" => Some(Self::Lean),
            _ => None,
        }
    }

    /// CLI / YAML names this build accepts.
    pub fn expected_names() -> &'static str {
        #[cfg(feature = "rust-targets")]
        {
            "native|wasm|ackinacki|evm|lean"
        }
        #[cfg(not(feature = "rust-targets"))]
        {
            "evm|lean"
        }
    }

    /// Default CLI target when `--target` is omitted.
    pub fn default_cli() -> Self {
        #[cfg(feature = "rust-targets")]
        {
            Target::Native
        }
        #[cfg(not(feature = "rust-targets"))]
        {
            Target::Evm
        }
    }

    /// Targets whose emitters are compiled into this binary.
    pub fn emitters() -> &'static [Target] {
        #[cfg(feature = "rust-targets")]
        {
            &Self::ALL
        }
        #[cfg(not(feature = "rust-targets"))]
        {
            &[Target::Evm, Target::Lean]
        }
    }

    /// Canonical CLI / YAML name for this target.
    pub fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Wasm => "wasm",
            Self::AckiNacki => "ackinacki",
            Self::Evm => "evm",
            Self::Lean => "lean",
        }
    }

    pub fn domain(self) -> Domain {
        match self {
            Self::Native | Self::Wasm => Domain::Container,
            Self::AckiNacki => Domain::Tvm,
            Self::Evm | Self::Lean => Domain::Evm,
        }
    }

    /// Every target whose [`Self::domain`] equals `domain`.
    pub fn cores_of(domain: Domain) -> Vec<Target> {
        Self::ALL
            .into_iter()
            .filter(|t| t.domain() == domain)
            .collect()
    }

    pub fn language(self) -> Language {
        match self {
            Self::Native | Self::Wasm | Self::AckiNacki => Language::Rust,
            Self::Evm => Language::Solidity,
            Self::Lean => Language::Lean,
        }
    }

    /// Whether this target models EVM semantics (E-rules / `evm::*` allowed).
    /// Replaces the historical `target_models_evm = target == "evm" || "lean"`.
    pub fn models_evm(self) -> bool {
        self.domain() == Domain::Evm
    }

    /// Whether `property` decls are desugared into `test` / `fuzz` before
    /// codegen. Lean keeps properties raw for clean theorems.
    pub fn desugars_properties(self) -> bool {
        self.language() != Language::Lean
    }

    /// Whether multi-entity invariants (I7) are a hard error.
    /// TVM hosts (Acki Nacki) block them; Foundry / revm / Lean accept them.
    ///
    /// Since the P7 classification review this is redundant with the I7
    /// `Domains([Tvm])` rule binding (the rule only fires where it errors),
    /// kept as the capability-level statement of the same fact.
    pub fn multi_entity_invariants_hard_error(self) -> bool {
        self.domain() == Domain::Tvm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_round_trip() {
        for name in Target::emitters().iter().map(|t| t.name()) {
            let t = Target::from_name(name).unwrap();
            assert_eq!(t.name(), name);
        }
        assert!(Target::from_name("unknown").is_none());
        assert!(Target::from_name("").is_none());
    }

    #[cfg(not(feature = "rust-targets"))]
    #[test]
    fn rust_target_names_rejected() {
        assert!(Target::from_name("native").is_none());
        assert!(Target::from_name("wasm").is_none());
        assert!(Target::from_name("ackinacki").is_none());
        assert_eq!(Target::from_name("evm"), Some(Target::Evm));
        assert_eq!(Target::from_name("lean"), Some(Target::Lean));
        assert_eq!(Target::default_cli(), Target::Evm);
        assert_eq!(Target::emitters(), &[Target::Evm, Target::Lean]);
    }

    #[test]
    fn domain_language_matrix() {
        assert_eq!(Target::Native.domain(), Domain::Container);
        assert_eq!(Target::Wasm.domain(), Domain::Container);
        assert_eq!(Target::AckiNacki.domain(), Domain::Tvm);
        assert_eq!(Target::Evm.domain(), Domain::Evm);
        assert_eq!(Target::Lean.domain(), Domain::Evm);

        assert_eq!(Target::Native.language(), Language::Rust);
        assert_eq!(Target::Wasm.language(), Language::Rust);
        assert_eq!(Target::AckiNacki.language(), Language::Rust);
        assert_eq!(Target::Evm.language(), Language::Solidity);
        assert_eq!(Target::Lean.language(), Language::Lean);
    }

    #[test]
    fn cores_of_covers_all_targets_exactly_once() {
        let mut seen = Vec::new();
        for domain in [Domain::Container, Domain::Tvm, Domain::Evm] {
            seen.extend(Target::cores_of(domain));
        }
        seen.sort_by_key(|t| t.name());
        let mut all: Vec<_> = Target::ALL.to_vec();
        all.sort_by_key(|t| t.name());
        assert_eq!(seen, all);
        assert_eq!(Target::cores_of(Domain::Evm), vec![Target::Evm, Target::Lean]);
        assert_eq!(
            Target::cores_of(Domain::Container),
            vec![Target::Native, Target::Wasm]
        );
        assert_eq!(Target::cores_of(Domain::Tvm), vec![Target::AckiNacki]);
    }

    #[test]
    fn capability_table() {
        assert!(Target::Evm.models_evm());
        assert!(Target::Lean.models_evm());
        assert!(!Target::Native.models_evm());
        assert!(!Target::Wasm.models_evm());
        assert!(!Target::AckiNacki.models_evm());

        assert!(Target::Evm.desugars_properties());
        assert!(Target::Native.desugars_properties());
        assert!(!Target::Lean.desugars_properties());

        assert!(Target::AckiNacki.multi_entity_invariants_hard_error());
        assert!(!Target::Evm.multi_entity_invariants_hard_error());
        assert!(!Target::Lean.multi_entity_invariants_hard_error());
        assert!(!Target::Native.multi_entity_invariants_hard_error());
    }
}
