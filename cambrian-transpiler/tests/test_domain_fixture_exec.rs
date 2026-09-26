// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! P8 multi-core execution suite: per-target actions over the shared corpus.
//!
//! - `evm` → `forge build` when `forge` is on PATH
//! - `lean` → `lake build` when `CAMBRIAN_TEST_LEAN_BUILD=1`
//! - `native` / `wasm` → `cargo check` when `CAMBRIAN_TEST_RUST_BUILD=1`
//!
//! At least one backend must be configured or the suite panics (PM-010).
//! Lean: every EVM-domain corpus key is either [`LAKE_MUST_PASS`] or
//! [`LAKE_KNOWN_BROKEN`] (PM-032). Project-yaml CEI fixtures are forge-built
//! via [`PROJECT_FORGE_EXEC`] (PM-027).

#[path = "corpus/mod.rs"]
mod corpus;

use cambrian_transpiler::target::{Domain, Target};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static OUT_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_out(prefix: &str, key: &str) -> PathBuf {
    let n = OUT_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-p8-exec-{}-{}-{}-{}",
        prefix,
        key.replace('/', "__"),
        std::process::id(),
        n
    ))
}

fn transpiler_bin() -> PathBuf {
    // Compile-time path from Cargo; follows the active target dir
    // (`target/debug`, `target/llvm-cov-target/debug`, …).
    PathBuf::from(env!("CARGO_BIN_EXE_cambrian-transpiler"))
}

fn has_forge() -> bool {
    Command::new("forge")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn lake_available() -> bool {
    Command::new("lake")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

const FOUNDRY_TOML: &str = r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
auto_detect_solc = false
evm_version = "cancun"
optimizer = false

[lint]
lint_on_build = false
"#;

/// Corpus keys that must `lake build` today (historical lean_p3 ∩ corpus).
const LAKE_MUST_PASS: &[&str] = &[
    "counter",
    "predictable",
    "phased_vault",
    "lean_self_call",
    "lean_call",
    "lean_send_return",
    "lean_conditional",
    "lean_pair",
    "lean_deploy",
    "lean_cross_send",
    "lean_map",
    "keys_evm",
    "registry",
    "airdrop_evm",
    "fold_evm",
    "erc20_errors_evm",
    "erc20_events_evm",
    "eth_vault_evm",
    "staking",
    "voting",
    "wallet",
    "payment_channel",
    "phased_predictable",
    "enum_data",
    "invariant_lending",
    // P9 lake graduation (CAMBRIAN_TEST_LEAN_BUILD=1)
    "batch1_showcase",
    "dex",
    "extern_token_caller",
    "identity_pair/identity_vault",
    "messaging/receiver",
    "nft",
    "token",
    "token_query",
    "u256_messaging/u256_receiver",
    "batch1_an",
    "stdlib_demo",
    "rehearsal_token_events_evm",
    // Legacy contract filenames kept alongside predictable-profile aliases
    // (T1 symlinks); same content as the aliased fixtures, builds green.
    "escrow",
    "phased_escrow",
    "escrow_v2",
    "loop_evm",
    "phase_where_var",
];

/// Corpus keys allowed to fail `lake build` until promoted into [`LAKE_MUST_PASS`].
/// Every EVM-domain corpus key must appear in exactly one of these two lists (PM-032).
const LAKE_KNOWN_BROKEN: &[&str] = &[];

/// Shared with `test_evm_forge::ALL_FIXTURES_SOLC_KNOWN_BROKEN` (keep in sync).
const FORGE_KNOWN_BROKEN: &[&str] = &[];

/// `PROJECT_ONLY` primaries that still need `forge build` via their sibling
/// `contracts/<key>.yaml` (PM-027 CEI fixture).
const PROJECT_FORGE_EXEC: &[&str] = &["det_reentrancy_order"];

fn transpile(path: &Path, target: Target, out: &Path) -> (bool, String) {
    let output = Command::new(transpiler_bin())
        .args([
            path.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--target",
            target.name(),
        ])
        .output()
        .expect("run transpiler");
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), err)
}

fn transpile_project(yaml: &Path) -> (bool, String, PathBuf) {
    let project = match cambrian_transpiler::project::Project::load(yaml) {
        Ok(p) => p,
        Err(e) => return (false, format!("Project::load: {e}"), PathBuf::new()),
    };
    let out = project.output_dir();
    let _ = std::fs::remove_dir_all(&out);
    let output = Command::new(transpiler_bin())
        .args(["--project", yaml.to_str().unwrap()])
        .output()
        .expect("run transpiler --project");
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), err, out)
}

fn forge_build(out: &Path) -> (bool, String) {
    std::fs::write(out.join("foundry.toml"), FOUNDRY_TOML).expect("write foundry.toml");
    // Corpus gate checks entity Solidity, not generated Foundry harnesses
    // (those import forge-std which is not vendored in the temp root).
    let forge = Command::new("forge")
        .args(["build", "--root"])
        .arg(out)
        .args(["--skip", ".t.sol", "--use", "0.8.24"])
        .output()
        .expect("forge build");
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );
    (forge.status.success(), err)
}

fn lake_build(out: &Path) -> (bool, String) {
    let lake = Command::new("lake")
        .arg("build")
        .current_dir(out)
        .output()
        .expect("lake build");
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&lake.stdout),
        String::from_utf8_lossy(&lake.stderr)
    );
    (lake.status.success(), err)
}

fn cargo_check(out: &Path, target_dir: &Path) -> (bool, String) {
    // Generated Cargo.toml uses `path = "../cambrian-runtime"` (workspace-
    // relative). Rewrite to an absolute path so temp-dir builds resolve.
    let cargo_toml = out.join("Cargo.toml");
    if let Ok(contents) = std::fs::read_to_string(&cargo_toml) {
        let runtime = corpus::repo_root().join("cambrian-runtime");
        let patched = contents.replace(
            "path = \"../cambrian-runtime\"",
            &format!("path = \"{}\"", runtime.display()),
        );
        let _ = std::fs::write(&cargo_toml, patched);
    }
    let cargo = Command::new("cargo")
        .args(["check", "--quiet"])
        .env("CARGO_TARGET_DIR", target_dir)
        .current_dir(out)
        .output()
        .expect("cargo check");
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&cargo.stdout),
        String::from_utf8_lossy(&cargo.stderr)
    );
    (cargo.status.success(), err)
}

/// Container-domain fixtures that must `cargo check` for native + wasm.
const RUST_MUST_PASS: &[&str] = &["counter", "predictable", "voting", "staking", "wallet"];

#[test]
fn lake_corpus_fully_enumerated() {
    let keys: HashSet<_> = corpus::domain_fixtures(Domain::Evm)
        .into_iter()
        .map(|f| f.key)
        .collect();
    let must: HashSet<_> = LAKE_MUST_PASS.iter().copied().collect();
    let broken: HashSet<_> = LAKE_KNOWN_BROKEN.iter().copied().collect();

    let mut missing = Vec::new();
    for k in LAKE_MUST_PASS {
        if !keys.contains(*k) {
            missing.push(format!("LAKE_MUST_PASS:{k}"));
        }
    }
    for k in LAKE_KNOWN_BROKEN {
        if !keys.contains(*k) {
            missing.push(format!("LAKE_KNOWN_BROKEN:{k}"));
        }
    }
    assert!(
        missing.is_empty(),
        "lake list entries not in EVM-domain corpus: {missing:?}"
    );

    let overlap: Vec<_> = must.intersection(&broken).copied().collect();
    assert!(
        overlap.is_empty(),
        "keys in both LAKE_MUST_PASS and LAKE_KNOWN_BROKEN: {overlap:?}"
    );

    let mut orphans = Vec::new();
    for k in &keys {
        if !must.contains(k.as_str()) && !broken.contains(k.as_str()) {
            orphans.push(k.clone());
        }
    }
    assert!(
        orphans.is_empty(),
        "EVM corpus keys missing from LAKE_MUST_PASS and LAKE_KNOWN_BROKEN: {orphans:?}"
    );
}

#[test]
fn lake_must_pass_subset_of_corpus() {
    // Kept as a thin alias of the full enumeration gate for older callers.
    lake_corpus_fully_enumerated();
}

#[test]
fn domain_fixture_execution_actions() {
    let fixtures = corpus::domain_fixtures(Domain::Evm);
    assert!(!fixtures.is_empty());

    let run_forge = has_forge();
    let run_lake = std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() == Ok("1");
    let run_rust = std::env::var("CAMBRIAN_TEST_RUST_BUILD").as_deref() == Ok("1");
    if run_lake && !lake_available() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }
    if !run_forge && !run_lake && !run_rust {
        // PM-010 soft-skip-free contract: the P8 exec gate job
        // (`test-transpiler-phase-m-gates`) declares itself via
        // CAMBRIAN_TEST_P8_EXEC_GATE=1 and must fail loudly when no backend is
        // available. Non-gate jobs (coverage llvm-cov: no forge, no opt-in env)
        // skip instead.
        if std::env::var("CAMBRIAN_TEST_P8_EXEC_GATE").as_deref() == Ok("1") {
            panic!(
                "domain_fixture_execution_actions requires a backend in the P8 \
                 exec gate job: install `forge`, or set CAMBRIAN_TEST_LEAN_BUILD=1 \
                 (lake), or CAMBRIAN_TEST_RUST_BUILD=1 (native/wasm cargo check). \
                 See .gitlab-ci.yml test-transpiler-phase-m-gates."
            );
        }
        eprintln!(
            "skipping domain_fixture_execution_actions: no backend (forge not on \
             PATH, CAMBRIAN_TEST_LEAN_BUILD/CAMBRIAN_TEST_RUST_BUILD unset) and \
             not the P8 exec gate job (CAMBRIAN_TEST_P8_EXEC_GATE unset)"
        );
        return;
    }

    let forge_broken: HashSet<&str> = FORGE_KNOWN_BROKEN.iter().copied().collect();
    let lake_must: HashSet<&str> = LAKE_MUST_PASS.iter().copied().collect();
    let lake_broken: HashSet<&str> = LAKE_KNOWN_BROKEN.iter().copied().collect();
    let rust_must: HashSet<&str> = RUST_MUST_PASS.iter().copied().collect();
    let mut forge_failures = Vec::new();
    let mut forge_graduated = Vec::new();
    let mut lake_failures = Vec::new();
    let mut lake_graduated = Vec::new();
    let mut rust_failures: Vec<String> = Vec::new();

    for fx in &fixtures {
        if run_forge {
            let out = unique_out("forge", &fx.key);
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir_all(&out).unwrap();
            let (tp_ok, tp_err) = transpile(&fx.path, Target::Evm, &out);
            let broken = forge_broken.contains(fx.key.as_str());
            if !tp_ok {
                if !broken {
                    forge_failures.push(format!("{} [transpile]: {}", fx.key, snip(&tp_err)));
                }
            } else {
                let (ok, err) = forge_build(&out);
                match (ok, broken) {
                    (true, true) => forge_graduated.push(fx.key.clone()),
                    (false, false) => {
                        forge_failures.push(format!("{} [solc]: {}", fx.key, snip(&err)))
                    }
                    _ => {}
                }
            }
            let _ = std::fs::remove_dir_all(&out);
        }

        if run_lake {
            let out = unique_out("lake", &fx.key);
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir_all(&out).unwrap();
            let (tp_ok, tp_err) = transpile(&fx.path, Target::Lean, &out);
            let must = lake_must.contains(fx.key.as_str());
            let broken = lake_broken.contains(fx.key.as_str());
            if !tp_ok {
                if must {
                    lake_failures.push(format!("{} [transpile]: {}", fx.key, snip(&tp_err)));
                }
            } else {
                let (ok, err) = lake_build(&out);
                match (ok, must, broken) {
                    (false, true, _) => {
                        lake_failures.push(format!("{} [lake]: {}", fx.key, snip(&err)))
                    }
                    (true, false, true) => lake_graduated.push(fx.key.clone()),
                    (true, false, false) => {
                        // Should be unreachable after lake_corpus_fully_enumerated.
                        lake_graduated.push(fx.key.clone());
                    }
                    _ => {}
                }
            }
            let _ = std::fs::remove_dir_all(&out);
        }
    }

    if run_forge {
        for key in PROJECT_FORGE_EXEC {
            assert!(
                corpus::PROJECT_ONLY.contains(key),
                "PROJECT_FORGE_EXEC entry `{key}` must stay in PROJECT_ONLY"
            );
            let yaml = corpus::repo_root()
                .join("contracts")
                .join(format!("{key}.yaml"));
            assert!(
                yaml.is_file(),
                "PROJECT_FORGE_EXEC `{key}` needs contracts/{key}.yaml"
            );
            let (tp_ok, tp_err, out) = transpile_project(&yaml);
            if !tp_ok {
                forge_failures.push(format!("{key} [project transpile]: {}", snip(&tp_err)));
            } else {
                let (ok, err) = forge_build(&out);
                if !ok {
                    forge_failures.push(format!("{key} [project solc]: {}", snip(&err)));
                }
            }
            let _ = std::fs::remove_dir_all(&out);
        }
    }

    #[cfg(feature = "rust-targets")]
    if run_rust {
        let container = corpus::domain_fixtures(Domain::Container);
        let shared_target = unique_out("cargo-target", "shared");
        let _ = std::fs::create_dir_all(&shared_target);
        for fx in &container {
            if !rust_must.contains(fx.key.as_str()) {
                continue;
            }
            for target in [Target::Native, Target::Wasm] {
                let out = unique_out(target.name(), &fx.key);
                let _ = std::fs::remove_dir_all(&out);
                std::fs::create_dir_all(&out).unwrap();
                let (tp_ok, tp_err) = transpile(&fx.path, target, &out);
                if !tp_ok {
                    rust_failures.push(format!(
                        "{} [{} transpile]: {}",
                        fx.key,
                        target.name(),
                        snip(&tp_err)
                    ));
                } else {
                    let (ok, err) = cargo_check(&out, &shared_target);
                    if !ok {
                        rust_failures.push(format!(
                            "{} [{} cargo check]: {}",
                            fx.key,
                            target.name(),
                            snip(&err)
                        ));
                    }
                }
                let _ = std::fs::remove_dir_all(&out);
            }
        }
        let _ = std::fs::remove_dir_all(&shared_target);
    }

    let mut msg = String::new();
    if !forge_graduated.is_empty() {
        msg.push_str(&format!(
            "\nforge known-broken fixtures now compile — remove from FORGE_KNOWN_BROKEN:\n  {}\n",
            forge_graduated.join("\n  ")
        ));
    }
    if !forge_failures.is_empty() {
        msg.push_str(&format!(
            "\nforge build failures:\n  {}\n",
            forge_failures.join("\n  ")
        ));
    }
    if !lake_graduated.is_empty() {
        msg.push_str(&format!(
            "\nlake known-broken fixtures now build — promote them into LAKE_MUST_PASS \
             and remove from LAKE_KNOWN_BROKEN:\n  {}\n",
            lake_graduated.join("\n  ")
        ));
    }
    if !lake_failures.is_empty() {
        msg.push_str(&format!(
            "\nlake build failures (LAKE_MUST_PASS):\n  {}\n",
            lake_failures.join("\n  ")
        ));
    }
    if !rust_failures.is_empty() {
        msg.push_str(&format!(
            "\nnative/wasm cargo check failures (RUST_MUST_PASS):\n  {}\n",
            rust_failures.join("\n  ")
        ));
    }
    assert!(msg.is_empty(), "{msg}");
}

fn snip(s: &str) -> String {
    s.lines()
        .find(|l| l.contains("Error") || l.contains("error"))
        .unwrap_or(s.lines().next().unwrap_or(""))
        .chars()
        .take(160)
        .collect()
}
