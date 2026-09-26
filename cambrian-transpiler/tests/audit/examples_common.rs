// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Shared helpers for the examples audit harnesses: workspace paths, timed
//! command execution, and release-binary project transpilation. Included via
//! `#[path]` by the per-suite test binaries.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Instant;

pub const COMMAND_TIMEOUT_SECS: u64 = 300;

pub fn workspace_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .to_path_buf()
    })
}

pub fn audit_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit")
}

pub fn has_timeout() -> bool {
    Command::new("timeout")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn combined_output(output: &std::process::Output) -> String {
    format!(
        "exit={:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

pub fn run_timed(cmd: Command, label: &str) -> (std::process::Output, u128) {
    run_timed_secs(cmd, label, COMMAND_TIMEOUT_SECS)
}

pub fn run_timed_secs(
    mut cmd: Command,
    label: &str,
    timeout_secs: u64,
) -> (std::process::Output, u128) {
    let start = Instant::now();
    let output = if has_timeout() {
        let program = cmd.get_program().to_owned();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let mut timeout_cmd = Command::new("timeout");
        timeout_cmd
            .arg(timeout_secs.to_string())
            .arg(program)
            .args(args);
        if let Some(dir) = cmd.get_current_dir() {
            timeout_cmd.current_dir(dir);
        }
        timeout_cmd.output().unwrap_or_else(|e| panic!("{label}: {e}"))
    } else {
        cmd.output().unwrap_or_else(|e| panic!("{label}: {e}"))
    };
    (output, start.elapsed().as_millis())
}

pub fn transpile_project(workspace: &Path, project_rel: &str) -> Result<(String, u128), String> {
    transpile_project_with(workspace, project_rel, &[], COMMAND_TIMEOUT_SECS)
}

pub fn transpile_project_with(
    workspace: &Path,
    project_rel: &str,
    extra_args: &[&str],
    timeout_secs: u64,
) -> Result<(String, u128), String> {
    let project = workspace.join(project_rel);
    let release_bin = workspace.join("target/release/cambrian-transpiler");
    let (output, ms) = if release_bin.is_file() {
        run_timed_secs(
            {
                let mut cmd = Command::new(&release_bin);
                cmd.arg("--project")
                    .arg(&project)
                    .args(extra_args)
                    .current_dir(workspace);
                cmd
            },
            "cambrian-transpiler",
            timeout_secs,
        )
    } else {
        run_timed_secs(
            {
                let mut cmd = Command::new("cargo");
                cmd.args([
                    "run",
                    "-p",
                    "cambrian-transpiler",
                    "--bin",
                    "cambrian-transpiler",
                    "--release",
                    "--",
                    "--project",
                ])
                .arg(&project)
                .args(extra_args)
                .current_dir(workspace);
                cmd
            },
            "cargo run cambrian-transpiler",
            timeout_secs,
        )
    };
    let combined = format!("wall_ms={ms}\n{}", combined_output(&output));
    if output.status.success() {
        Ok((combined, ms))
    } else {
        Err(combined)
    }
}
