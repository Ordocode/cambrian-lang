// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! CAM-TRANSPILE-MANIFEST — `.cambrian-manifest.json` on EVM transpile.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cambrian_transpiler::codegen::{EvmSolidityBackend, OutputBackend};
use cambrian_transpiler::manifest::{build_manifest_json, MANIFEST_REL_PATH};
use cambrian_transpiler::project::Project;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn transpiler_bin() -> PathBuf {
    let mut path = std::env::current_exe().expect("current_exe");
    path.pop();
    path.pop();
    path.push("cambrian-transpiler");
    path
}

#[test]
fn build_manifest_json_sorted_and_hashed() {
    let files = vec![
        ("b.sol".into(), "contract B {}".into()),
        ("a.sol".into(), "contract A {}".into()),
    ];
    let json = build_manifest_json("evm", &files);
    let parsed: cambrian_transpiler::manifest::CambrianManifest =
        serde_json::from_str(&json).expect("parse manifest");
    assert_eq!(parsed.version, 1);
    assert_eq!(parsed.target, "evm");
    assert_eq!(parsed.files.len(), 2);
    assert_eq!(parsed.files[0].path, "a.sol");
    assert_eq!(parsed.files[1].path, "b.sol");
    assert_eq!(parsed.files[0].sha256.len(), 64);
}

#[test]
fn single_file_cli_writes_manifest() {
    let out = std::env::temp_dir().join(format!(
        "cambrian-manifest-cli-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).expect("mkdir out");

    let fixture = manifest_dir().join("../contracts/counter.cam");
    let status = Command::new(transpiler_bin())
        .arg(&fixture)
        .arg("-o")
        .arg(&out)
        .arg("--target")
        .arg("evm")
        .status()
        .expect("transpile");
    assert!(status.success(), "transpile counter.cam failed");

    let manifest_path = out.join(MANIFEST_REL_PATH);
    assert!(manifest_path.is_file(), "missing {}", manifest_path.display());
    let raw = fs::read_to_string(&manifest_path).expect("read manifest");
    let parsed: cambrian_transpiler::manifest::CambrianManifest =
        serde_json::from_str(&raw).expect("parse manifest");
    assert_eq!(parsed.target, "evm");
    assert!(
        parsed.files.iter().any(|f| f.path.ends_with(".sol")),
        "manifest must list emitted solidity: {:?}",
        parsed.files
    );
}

#[test]
fn project_api_writes_manifest_on_emit() {
    let out = std::env::temp_dir().join(format!(
        "cambrian-manifest-api-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).expect("mkdir out");

    let fixture_dir = manifest_dir().join("tests/fixtures/smafd_canonical");
    let project = Project::load(&fixture_dir.join("project.evm.yaml")).expect("load project");
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let files = EvmSolidityBackend {
        deterministic_addresses: det,
    }
    .gen_project(&project);
    emit_files(&files, &out);
    cambrian_transpiler::manifest::write_evm_manifest(&out, &files).expect("write manifest");

    assert!(out.join(MANIFEST_REL_PATH).is_file());
}

fn emit_files(files: &[(String, String)], output_dir: &Path) {
    for (rel, contents) in files {
        let path = output_dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, contents).expect("write");
    }
}
