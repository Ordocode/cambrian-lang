// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Project load helpers with U4-6 deterministic-address policy.

use std::path::{Path, PathBuf};

use cambrian_transpiler::project::Project;

/// Workspace root (`cambrian-lang/`).
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

/// Resolve `deterministic_addresses` for harness transpile (U4-6 Step 10 default).
pub fn project_det(project: &Project) -> bool {
    project.config.resolved_deterministic_addresses()
}

pub fn load_project_yaml(path: &Path) -> Project {
    Project::load(path).expect("load project yaml")
}
