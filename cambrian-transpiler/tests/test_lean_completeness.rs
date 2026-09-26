// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean-target *completeness* harness.
//!
//! The Lean route codegen interprets every `RouteAction` across several
//! emission contexts (state-only vs world-threaded, pure vs failing,
//! statement vs tail-value, top-level vs nested in `if`/`for`). Gaps
//! historically appeared when a variant was lowered faithfully in one
//! context but silently mishandled in another — the codegen "fails open",
//! emitting a *sentinel* (`-- skipped`, `(_, default)`, `Cambrian.Unsupported`)
//! instead of erroring, so transpilation still succeeds and only the
//! ill-typed subset is caught by `lake build`.
//!
//! This file makes the gap surface *decidable* and *enforced*:
//!
//! 1. [`corpus_sentinel_scan`] transpiles every **standalone-meaningful**
//!    source under `contracts/` and `examples/` to Lean and asserts no
//!    route file contains a silent-drop sentinel outside an explicit,
//!    documented baseline ([`known_gaps`]). Sources that only make sense
//!    as part of a multi-entity `project.yaml` are listed in
//!    [`standalone_skip`] and excluded (cover them via the project-level
//!    Lean examples). New gaps fail the test; fixing a known gap
//!    (without updating the baseline) also fails, so the list cannot rot.
//!
//! See `tests/test_lean_matrix.rs` for the generative (variant × context)
//! completeness prover that complements this regression net.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

use cambrian_transpiler::target::{Domain, Target};

#[path = "corpus/mod.rs"]
mod corpus;

// ---------------------------------------------------------------------------
// Sentinel taxonomy
// ---------------------------------------------------------------------------

/// A class of "fail-open" marker the codegen emits when it cannot lower an
/// action/expression faithfully in the current context. `needle` is matched
/// as a plain substring against each generated line.
struct Sentinel {
    category: &'static str,
    needle: &'static str,
}

/// Silent (well-typed-but-wrong) and loud (ill-typed) drop markers. These
/// must never appear in generated Lean except for the documented
/// [`known_gaps`]. `-- not supported on Lean:` is deliberately NOT listed:
/// it marks *intentionally* unsupported, validator-guarded effects
/// (`gosh::*`, `rescue`, `gosh::updateCode`) and is allowed everywhere.
const SENTINELS: &[Sentinel] = &[
    // A `RouteAction` dropped to a comment (e.g. an effect/throw inside a
    // structure that the current path doesn't thread).
    Sentinel {
        category: "dropped_action",
        needle: "-- skipped:",
    },
    // Defensive no-op that should be unreachable (effectful action on the
    // state-only path).
    Sentinel {
        category: "internal_noop",
        needle: "-- internal: effectful",
    },
    // Explicit "this should be impossible" marker.
    Sentinel {
        category: "bug_marker",
        needle: "-- bug:",
    },
    // Typed send / var-call whose destination didn't resolve (should be
    // validator-blocked by L8).
    Sentinel {
        category: "unresolved_send",
        needle: "-- L8:",
    },
    // An expression the Lean backend can't lower; makes `lake build` fail.
    Sentinel {
        category: "unsupported_expr",
        needle: "Cambrian.Unsupported",
    },
    // A view route whose return value collapsed to `default`.
    Sentinel {
        category: "default_payload",
        needle: ", default)",
    },
    // Proof/term hole.
    Sentinel {
        category: "sorry",
        needle: "sorry",
    },
];

/// Primary sources that are only meaningful as part of a multi-entity
/// `project.yaml`. Standalone transpile lacks sibling entities, so
/// cross-entity surfaces (e.g. `UniswapV2Pair.address(...)` in Factory)
/// are incomplete-program artifacts — not Lean expression-axis gaps.
/// Cover these via the project-level Lean examples (`project.lean.yaml`).
fn standalone_skip() -> BTreeSet<&'static str> {
    BTreeSet::from([
        // Multi-entity project fragments — cover via project.lean.yaml.
        "examples/uniswap-v2/UniswapV2Factory",
        "examples/uniswap-v2/UniswapV2Pair",
        "examples/governor/Governor",
        // Accumulator is an Acki Nacki showcase; fragments need siblings /
        // TVM constructs and are not Lean-target fixtures.
        "examples/accumulator/Exchange",
        "examples/accumulator/RootToken",
        "examples/accumulator/ShellAccumulatorRootUSDC",
        "examples/accumulator/ShellSellOrderLot",
        "examples/accumulator/TokenWallet",
        "examples/accumulator/Transaction",
        "examples/accumulator/UpdateZeroContract",
        // Acki Nacki / WASM LCC repro bundle — not Lean-target fixtures.
        "examples/lcc-wasm-repro/account-validate",
        "examples/lcc-wasm-repro/interest-calc",
        "examples/lcc-wasm-repro/ledger-post",
        "examples/lcc-wasm-repro/para-perform",
        "examples/lcc-wasm-repro/ref-inspect",
    ])
}

/// Whether a Lean transpile rejection is expected for this scan key
/// (`contracts/…` or `examples/…`).
fn lean_reject_allowed(scan_key: &str) -> bool {
    if standalone_skip().contains(scan_key) {
        return true;
    }
    let Some(corpus_key) = scan_key.strip_prefix("contracts/") else {
        // examples/ (and anything else): only standalone_skip is allowed.
        return false;
    };
    if corpus::PROJECT_ONLY.contains(&corpus_key) {
        return true;
    }
    if corpus::is_rejected_on(corpus_key, Domain::Evm) {
        return true;
    }
    if corpus::is_core_reject(corpus_key, Target::Lean) {
        return true;
    }
    false
}

/// Documented, accepted gaps in the current corpus. Each maps a source key
/// (repo-relative path without `.cam`, `/`-separated) to the sentinel
/// categories it is *known* to still emit. The scan asserts the live set
/// matches this exactly for every source that transpiles, so:
///   * a NEW sentinel (regression / newly-exercised gap) fails the test;
///   * FIXING a listed gap without trimming this baseline also fails,
///     prompting the entry's removal.
///
/// Keep this list shrinking. Each entry should reference the tracking work
/// needed to close it. Empty when the standalone corpus is sentinel-free.
fn known_gaps() -> BTreeMap<&'static str, BTreeSet<&'static str>> {
    BTreeMap::new()
}

// ---------------------------------------------------------------------------
// Scaffolding
// ---------------------------------------------------------------------------

fn transpiler_bin() -> PathBuf {
    // `CARGO_BIN_EXE_*` rather than a walk up from `current_exe()`: the two
    // are the same path under the classic `target/debug/deps` layout, but a
    // cargo configured with a split build directory puts the test executable
    // somewhere else entirely, and the walk then points at a file that does
    // not exist. The failure reads as "No such file or directory" from the
    // spawn, which names neither the binary nor the layout.
    PathBuf::from(env!("CARGO_BIN_EXE_cambrian-transpiler"))
}

fn repo_root() -> PathBuf {
    std::env::current_dir().expect("current_dir").join("..")
}

/// Collect every standalone-meaningful `.cam` under `contracts/` and
/// `examples/`, keyed by its repo-relative path without the `.cam`
/// extension (`/`-separated). Fragments with a compound stem
/// (`*.fuzz.cam`, `*.invariant.cam`, `*.test.cam`, …) and sources in
/// [`standalone_skip`] are omitted. Recursing `examples/` catches the
/// real-world contracts (governor, uniswap-v2, accumulator) that the
/// contracts-only scan missed.
fn collect_cams() -> Vec<(String, PathBuf)> {
    let root = repo_root();
    let skip = standalone_skip();
    let mut out = Vec::new();
    let mut stack = vec![root.join("contracts"), root.join("examples")];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if p.extension().and_then(|e| e.to_str()) != Some("cam") {
                continue;
            }
            // Compound stem (`Foo.fuzz`, `Foo.invariant`, `Foo.test`) →
            // fragment, not a primary source.
            let stem = p.file_stem().unwrap().to_string_lossy();
            if stem.contains('.') {
                continue;
            }
            let key = p
                .strip_prefix(&root)
                .unwrap_or(&p)
                .with_extension("")
                .to_string_lossy()
                .replace('\\', "/");
            if skip.contains(key.as_str()) {
                continue;
            }
            out.push((key, p));
        }
    }
    out.sort();
    out
}

fn tempdir(stem: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cambrian-lean-completeness-{}-{}",
        stem,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Recursively collect generated `.lean` file contents under `dir`.
///
/// `*Spec.lean` files are excluded: they hold the test / property /
/// invariant proof-harness codegen, which intentionally ships `sorry`
/// proof stubs (P2: statements only) and its own test-only markers. Route
/// / expression completeness — the subject of this scan — lives in the
/// entity, `*Routes.lean`, `World.lean`, and `Pure.lean` files. Spec/test
/// codegen completeness is a separate axis (future layer).
fn read_lean_files(dir: &std::path::Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("lean") {
                if p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with("Spec.lean"))
                {
                    continue;
                }
                if let Ok(txt) = std::fs::read_to_string(&p) {
                    out.push((p.display().to_string(), txt));
                }
            }
        }
    }
    out
}

/// Categories of sentinel found in a body of generated Lean.
fn scan_categories(files: &[(String, String)]) -> BTreeSet<&'static str> {
    let mut found = BTreeSet::new();
    for (_, txt) in files {
        for s in SENTINELS {
            if txt.contains(s.needle) {
                found.insert(s.category);
            }
        }
    }
    found
}

// ---------------------------------------------------------------------------
// The scan
// ---------------------------------------------------------------------------

#[test]
fn corpus_sentinel_scan() {
    let cams = collect_cams();
    assert!(!cams.is_empty(), "no contracts/examples sources found");
    assert!(
        !cams
            .iter()
            .any(|(k, _)| standalone_skip().contains(k.as_str())),
        "standalone_skip sources must not appear in collect_cams()"
    );

    let baseline = known_gaps();
    // source key -> live sentinel categories (only sources that transpiled
    // standalone are recorded).
    let mut live: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
    let mut transpiled: BTreeSet<String> = BTreeSet::new();
    let mut unexpected_rejects: Vec<String> = Vec::new();

    for (key, cam) in &cams {
        let out_dir = tempdir(&key.replace('/', "__"));
        let status = Command::new(transpiler_bin())
            .arg(cam)
            .arg("-o")
            .arg(&out_dir)
            .arg("--target")
            .arg("lean")
            .output()
            .expect("invoke transpiler");
        if !status.status.success() {
            // P8: silent skips are gone. A transpile failure is only
            // acceptable for corpus REJECTED_ON / CORE_REJECTS / PROJECT_ONLY
            // (contracts/) or standalone_skip (examples/).
            if !lean_reject_allowed(key) {
                let err = String::from_utf8_lossy(&status.stderr);
                let snip: String = err
                    .lines()
                    .find(|l| l.contains("error") || l.contains('['))
                    .unwrap_or("transpile failed")
                    .chars()
                    .take(160)
                    .collect();
                unexpected_rejects.push(format!("  {key}: {snip}"));
            }
            continue;
        }
        transpiled.insert(key.clone());
        let cats = scan_categories(&read_lean_files(&out_dir));
        if !cats.is_empty() {
            live.insert(key.clone(), cats);
        }
    }

    assert!(
        unexpected_rejects.is_empty(),
        "Lean transpile rejected corpus fixtures that are not in REJECTED_ON / CORE_REJECTS / PROJECT_ONLY / standalone_skip:\n{}",
        unexpected_rejects.join("\n")
    );

    // Build human-readable diffs against the baseline.
    let mut new_gaps: Vec<String> = Vec::new();
    let mut fixed_gaps: Vec<String> = Vec::new();

    for (key, cats) in &live {
        let expected = baseline.get(key.as_str());
        match expected {
            None => new_gaps.push(format!("  {key}: {:?} (no baseline entry)", cats)),
            Some(exp) => {
                let extra: Vec<_> = cats.difference(exp).collect();
                if !extra.is_empty() {
                    new_gaps.push(format!("  {key}: new categories {:?}", extra));
                }
            }
        }
    }
    // Baseline entries that transpiled but no longer show their gap.
    for (key, exp) in &baseline {
        if !transpiled.contains(*key) {
            continue; // didn't transpile here; can't judge
        }
        let cats = live.get(*key).cloned().unwrap_or_default();
        let gone: Vec<_> = exp.difference(&cats).collect();
        if !gone.is_empty() {
            fixed_gaps.push(format!(
                "  {key}: categories {:?} appear FIXED — remove from known_gaps()",
                gone
            ));
        }
    }

    let mut msg = String::new();
    if !new_gaps.is_empty() {
        msg.push_str(&format!(
            "\nNEW Lean codegen sentinels (unmodeled action/expr leaked into output):\n{}\n",
            new_gaps.join("\n")
        ));
    }
    if !fixed_gaps.is_empty() {
        msg.push_str(&format!(
            "\nStale baseline entries (gap closed — update known_gaps()):\n{}\n",
            fixed_gaps.join("\n")
        ));
    }
    assert!(msg.is_empty(), "{}", msg);
}
