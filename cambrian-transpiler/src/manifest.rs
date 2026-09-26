// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM transpile provenance manifest (SMAFD CAM-TRANSPILE-MANIFEST-UP).

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MANIFEST_REL_PATH: &str = ".cambrian-manifest.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CambrianManifest {
    pub version: u32,
    pub target: String,
    pub transpiler_version: String,
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestFile {
    pub path: String,
    pub sha256: String,
}

/// Build a deterministic manifest JSON document from emitted `(rel_path, contents)` pairs.
pub fn build_manifest_json(target: &str, files: &[(String, String)]) -> String {
    let mut entries: Vec<ManifestFile> = files
        .iter()
        .map(|(path, contents)| ManifestFile {
            path: path.clone(),
            sha256: sha256_hex(contents),
        })
        .collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    let manifest = CambrianManifest {
        version: 1,
        target: target.to_string(),
        transpiler_version: env!("CARGO_PKG_VERSION").to_string(),
        files: entries,
    };
    serde_json::to_string_pretty(&manifest).expect("manifest json")
}

pub fn sha256_hex(contents: &str) -> String {
    let hash = Sha256::digest(contents.as_bytes());
    format!("{:x}", hash)
}

/// Write `.cambrian-manifest.json` under `output_dir` (EVM target only).
pub fn write_evm_manifest(
    output_dir: &Path,
    files: &[(String, String)],
) -> std::io::Result<()> {
    let json = build_manifest_json("evm", files);
    std::fs::write(output_dir.join(MANIFEST_REL_PATH), json)
}
