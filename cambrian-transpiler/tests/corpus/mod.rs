// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Shared fixture corpus for domain-parametric test sharing (P8).
//!
//! Single source of truth for "what are the fixtures of domain D". Harnesses
//! select from [`domain_fixtures`]; exemptions live in explicit,
//! self-checked lists instead of silent skips.
//!
//! Include from each integration-test binary via:
//! ```ignore
//! #[path = "corpus/mod.rs"]
//! mod corpus;
//! ```

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use cambrian_transpiler::target::{Domain, Target};

/// Workspace root (parent of `cambrian-transpiler/`).
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

/// One primary `.cam` under `contracts/`, keyed by the path relative to
/// `contracts/` without the `.cam` extension (`counter`, `marketplace/broker`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Fixture {
    pub key: String,
    pub path: PathBuf,
}

/// Fixtures that legitimately fail a domain's validation (hard errors).
/// A future domain adds rows here, not a new list type.
///
/// `code` is the diagnostic that must appear on every core of `domain`.
pub const REJECTED_ON: &[(&str, Domain, &str, &str)] = &[
    (
        "multisig",
        Domain::Evm,
        "E03",
        "msg::pubkey throughout — TVM message model",
    ),
    (
        "upgrade",
        Domain::Evm,
        "E03",
        "msg::pubkey + gosh::commit — TVM message / SDK model",
    ),
    (
        "identity_pair/identity_pair",
        Domain::Evm,
        "E14",
        "sys::pubkey — TVM contract keypair",
    ),
    (
        "guardian_vault/guardian",
        Domain::Evm,
        "E14",
        "sys::pubkey — TVM contract keypair (also needs Vault sibling)",
    ),
    (
        "bouncer",
        Domain::Evm,
        "E26",
        "rescue/recover — TVM bounce; no EVM try/catch",
    ),
];

/// Fixtures accepted by a domain but rejected by one core's language / pair
/// rules. Populated empirically; graduation panics when the rejection lifts.
pub const CORE_REJECTS: &[(&str, Target, &str)] = &[
    // Empty: current contracts/ corpus has no accept/reject divergence
    // across same-domain cores (asymmetry gate). Add rows when a Lean
    // pair rule (L8/L9/L11) or future core rejects a domain-valid fixture.
];

/// Sources that only make sense via a multi-file `project.yaml` (standalone
/// transpile lacks sibling entities or deterministic project config).
/// Domain-agnostic.
pub const PROJECT_ONLY: &[&str] = &[
    "proj_counter",
    "proj_vault",
    "proj_counter_dup",
    "det_counter",
    "det_vault",
    "det_guardian",
    "det_locker",
    "det_singleton",
    // Deterministic multi-entity primaries: need sibling yaml
    // (`deterministic_addresses: true`) — standalone erases `.address`.
    "det_addressof",
    "det_member_init",
    "det_reentrancy_order",
    "escrow_two_vaults",
    // Multi-entity subdir fragments: typed sends / from-clauses need siblings.
    "marketplace/broker",
    "marketplace/shop",
    "marketplace/marketplace",
    "marketplace/ledger",
    "guardian_vault/vault",
    "messaging/sender",
    "u256_messaging/u256_sender",
];

/// Recursively collect primary `.cam` files under `contracts/` (skip
/// `*.test` / `*.fuzz` / `*.invariant` companions).
pub fn contracts_primaries() -> Vec<Fixture> {
    let root = repo_root();
    let contracts = root.join("contracts");
    let mut out = Vec::new();
    fn walk(dir: &Path, contracts_root: &Path, out: &mut Vec<Fixture>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, contracts_root, out);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("cam") {
                continue;
            }
            let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if name.contains('.') {
                // Compound stem (`Foo.test`, `Foo.fuzz`, `Foo.invariant`).
                continue;
            }
            let key = path
                .strip_prefix(contracts_root)
                .unwrap_or(&path)
                .with_extension("")
                .to_string_lossy()
                .replace('\\', "/");
            out.push(Fixture { key, path });
        }
    }
    walk(&contracts, &contracts, &mut out);
    out.sort();
    out
}

fn project_only_set() -> HashSet<&'static str> {
    PROJECT_ONLY.iter().copied().collect()
}

fn rejected_keys_for(domain: Domain) -> HashSet<&'static str> {
    REJECTED_ON
        .iter()
        .filter(|(_, d, _, _)| *d == domain)
        .map(|(k, _, _, _)| *k)
        .collect()
}

/// Primaries of domain `d`: contracts primaries minus [`REJECTED_ON`] for `d`
/// minus [`PROJECT_ONLY`].
pub fn domain_fixtures(domain: Domain) -> Vec<Fixture> {
    let rejected = rejected_keys_for(domain);
    let project_only = project_only_set();
    contracts_primaries()
        .into_iter()
        .filter(|f| !rejected.contains(f.key.as_str()) && !project_only.contains(f.key.as_str()))
        .collect()
}

/// Whether `(fixture_key, target)` is an expected core-level rejection.
pub fn is_core_reject(key: &str, target: Target) -> bool {
    CORE_REJECTS
        .iter()
        .any(|(k, t, _)| *k == key && *t == target)
}

/// Expected diagnostic code for a [`CORE_REJECTS`] entry, if any.
pub fn core_reject_code(key: &str, target: Target) -> Option<&'static str> {
    CORE_REJECTS
        .iter()
        .find(|(k, t, _)| *k == key && *t == target)
        .map(|(_, _, c)| *c)
}

/// Whether `key` is listed as rejected on `domain`.
pub fn is_rejected_on(key: &str, domain: Domain) -> bool {
    REJECTED_ON
        .iter()
        .any(|(k, d, _, _)| *k == key && *d == domain)
}

/// Expected diagnostic code for a [`REJECTED_ON`] entry, if any.
pub fn rejected_on_code(key: &str, domain: Domain) -> Option<&'static str> {
    REJECTED_ON
        .iter()
        .find(|(k, d, _, _)| *k == key && *d == domain)
        .map(|(_, _, c, _)| *c)
}

/// A multi-entity `project.yaml` that has at least two same-domain core
/// YAMLs (e.g. `project.yaml` for `--target evm` + `project.lean.yaml`
/// for `--target lean`). Used by the project-level asymmetry gate.
#[derive(Debug, Clone)]
pub struct DualCoreProject {
    pub key: &'static str,
    pub domain: Domain,
    /// `(Target, yaml path relative to repo root)`.
    pub yamls: &'static [(Target, &'static str)],
}

/// Dual-core projects covered by the project asymmetry gate.
pub const DUAL_CORE_PROJECTS: &[DualCoreProject] = &[
    DualCoreProject {
        key: "examples/governor",
        domain: Domain::Evm,
        yamls: &[
            (Target::Evm, "examples/governor/project.yaml"),
            (Target::Lean, "examples/governor/project.lean.yaml"),
        ],
    },
    DualCoreProject {
        key: "examples/uniswap-v2",
        domain: Domain::Evm,
        yamls: &[
            (Target::Evm, "examples/uniswap-v2/project.yaml"),
            (Target::Lean, "examples/uniswap-v2/project.lean.yaml"),
        ],
    },
];

/// Resolve [`DUAL_CORE_PROJECTS`] to absolute paths; panics if a listed
/// yaml is missing (self-check / gate invariant).
pub fn dual_core_projects() -> Vec<(String, Domain, Vec<(Target, PathBuf)>)> {
    let root = repo_root();
    DUAL_CORE_PROJECTS
        .iter()
        .map(|p| {
            let yamls: Vec<_> = p
                .yamls
                .iter()
                .map(|(t, rel)| {
                    let path = root.join(rel);
                    assert!(
                        path.is_file(),
                        "DUAL_CORE_PROJECTS yaml missing: {} ({})",
                        rel,
                        path.display()
                    );
                    (*t, path)
                })
                .collect();
            (p.key.to_string(), p.domain, yamls)
        })
        .collect()
}
