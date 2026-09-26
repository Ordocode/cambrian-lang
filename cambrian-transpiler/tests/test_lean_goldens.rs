// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! P4 step A — byte-identical golden snapshots for Lean project emission.
//!
//! Gate for refactor steps B–F: every emitted file under a pinned fixture set
//! must match the checked-in trees in `tests/goldens/lean/<name>/`.
//!
//! Regenerate (deliberately, after an accepted output-changing step):
//!
//! ```bash
//! CAMBRIAN_UPDATE_LEAN_GOLDENS=1 cargo test -p cambrian-transpiler --test test_lean_goldens -- --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate parent")
        .to_path_buf()
}

fn goldens_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/lean")
}

fn tempdir(stem: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cambrian-lean-golden-{}-{}",
        stem,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_project(dir: &Path, yaml: &str, sources: &[(&str, &str)]) {
    for (name, body) in sources {
        let dest = dir.join(name);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&dest, body).unwrap();
    }
    std::fs::write(dir.join("project.yaml"), yaml).unwrap();
}

fn emit_project(dir: &Path) -> BTreeMap<String, String> {
    let project = Project::load(&dir.join("project.yaml")).expect("load project");
    LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .collect()
}

fn read_tree(root: &Path) -> BTreeMap<String, String> {
    fn is_golden_text_file(rel: &str) -> bool {
        if rel.contains("/.lake/") || rel.starts_with(".lake/") {
            return false;
        }
        matches!(
            rel.rsplit('.').next(),
            Some("lean" | "toml" | "json" | "trace" | "hash" | "setup")
        ) || rel.ends_with("lean-toolchain")
    }

    let mut out = BTreeMap::new();
    if !root.exists() {
        return out;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if !is_golden_text_file(&rel) {
                    continue;
                }
                let body = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                    panic!("read {}: {e}", path.display());
                });
                out.insert(rel, body);
            }
        }
    }
    out
}

fn write_tree(root: &Path, files: &BTreeMap<String, String>) {
    let _ = std::fs::remove_dir_all(root);
    for (rel, body) in files {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, body).unwrap();
    }
}

/// Drop paths only emitted by non-default cargo features, so the goldens
/// (recorded with all features on) still match a `--no-default-features`
/// build. The public snapshot ships neither the feature nor these goldens,
/// so there the filter is a no-op.
fn strip_feature_gated(files: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut files = files;
    if cfg!(not(feature = "plausible")) {
        files.retain(|path, _| !path.starts_with("plausible/") && path != "plausible-bounds.json");
    }
    files
}

fn assert_matches_golden(name: &str, files: &BTreeMap<String, String>) {
    let golden_dir = goldens_root().join(name);
    if std::env::var_os("CAMBRIAN_UPDATE_LEAN_GOLDENS").is_some() {
        assert!(
            cfg!(feature = "plausible"),
            "refusing to update goldens without the `plausible` feature \
             (would drop the feature-gated golden files)"
        );
        write_tree(&golden_dir, files);
        eprintln!("updated goldens for {name} ({} files)", files.len());
        return;
    }

    let expected = strip_feature_gated(read_tree(&golden_dir));
    assert!(
        !expected.is_empty(),
        "missing goldens for `{name}` under {}; run with CAMBRIAN_UPDATE_LEAN_GOLDENS=1",
        golden_dir.display()
    );

    let mut mismatches = Vec::new();
    for (path, body) in files {
        match expected.get(path) {
            None => mismatches.push(format!("EXTRA file: {path}")),
            Some(want) if want != body => {
                mismatches.push(format!(
                    "DIFF {path}: generated {} bytes, golden {} bytes",
                    body.len(),
                    want.len()
                ));
            }
            Some(_) => {}
        }
    }
    for path in expected.keys() {
        if !files.contains_key(path) {
            mismatches.push(format!("MISSING file: {path}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "Lean golden mismatch for `{name}` ({}):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

fn read_repo_file(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

#[test]
fn lean_golden_counter() {
    let dir = tempdir("counter");
    let cam = read_repo_file("contracts/counter.cam");
    write_project(
        &dir,
        "name: counter\ntarget: lean\nsources:\n  - counter.cam\n",
        &[("counter.cam", &cam)],
    );
    let files = emit_project(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert_matches_golden("counter", &files);
}

#[test]
fn lean_golden_phased_predictable() {
    let dir = tempdir("phased-escrow");
    let cam = read_repo_file("contracts/phased_predictable.cam");
    write_project(
        &dir,
        "name: phased-escrow\ntarget: lean\nsources:\n  - phased_predictable.cam\n",
        &[("phased_predictable.cam", &cam)],
    );
    let files = emit_project(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert_matches_golden("phased_predictable", &files);
}

#[test]
fn lean_golden_counter_specs() {
    let dir = tempdir("counter-specs");
    let cam = read_repo_file("contracts/counter.cam");
    let test = read_repo_file("contracts/counter.test.cam");
    let inv = read_repo_file("contracts/counter.invariant.cam");
    write_project(
        &dir,
        "name: counter-specs\ntarget: lean\nsources:\n  - counter.cam\n  - counter.test.cam\n  - counter.invariant.cam\n",
        &[
            ("counter.cam", &cam),
            ("counter.test.cam", &test),
            ("counter.invariant.cam", &inv),
        ],
    );
    let files = emit_project(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert_matches_golden("counter_specs", &files);
}

#[test]
fn lean_golden_counter_nat() {
    let dir = tempdir("counter-nat");
    let cam = read_repo_file("contracts/counter.cam");
    write_project(
        &dir,
        "name: counter-nat\ntarget: lean\nsources:\n  - counter.cam\nlean:\n  numerics: nat\n",
        &[("counter.cam", &cam)],
    );
    let files = emit_project(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert_matches_golden("counter_nat", &files);
}

#[test]
fn lean_golden_uniswap_v2() {
    // Multi-entity project without the Plausible overlay (keeps goldens
    // bounded). Sources + nat numerics still exercise the heavy path.
    // `shared.cam` is pulled in via `import` from the entity sources.
    let dir = tempdir("uniswap");
    let sources = [
        "shared.cam",
        "ERC20.cam",
        "UniswapV2Pair.cam",
        "UniswapV2Factory.cam",
    ];
    let mut pairs: Vec<(String, String)> = Vec::new();
    for name in sources {
        pairs.push((
            name.to_string(),
            read_repo_file(&format!("examples/uniswap-v2/{name}")),
        ));
    }
    let yaml = "\
name: uniswap-v2
target: lean
deterministic_addresses: true
sources:
  - ERC20.cam
  - UniswapV2Pair.cam
  - UniswapV2Factory.cam
lean:
  numerics: nat
";
    let refs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_str()))
        .collect();
    write_project(&dir, yaml, &refs);
    let files = emit_project(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert_matches_golden("uniswap_v2", &files);
}
