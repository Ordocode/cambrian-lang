// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use std::process::Command;
use std::path::Path;

fn git(args: &[&str], dir: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn main() {
    lalrpop::Configuration::new()
        .generate_in_source_tree()
        .process_current_dir()
        .unwrap();

    println!("cargo:rerun-if-changed=src/cambrian.lalrpop");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");

    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let dir = Path::new(&dir);

    match git(&["rev-parse", "HEAD"], dir) {
        Some(commit) => {
            println!("cargo:rustc-env=CAMBRIAN_GIT_COMMIT={commit}");
            let dirty = match Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(dir)
                .output()
            {
                Ok(out) if out.status.success() => {
                    !String::from_utf8_lossy(&out.stdout).trim().is_empty()
                }
                _ => true,
            };
            println!(
                "cargo:rustc-env=CAMBRIAN_GIT_DIRTY={}",
                if dirty { "true" } else { "false" }
            );
        }
        None => {
            println!("cargo:rustc-env=CAMBRIAN_GIT_COMMIT=unknown");
            println!("cargo:rustc-env=CAMBRIAN_GIT_DIRTY=true");
        }
    }

    let grammar_rev = git(&["hash-object", "src/cambrian.lalrpop"], dir)
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=CAMBRIAN_GRAMMAR_REVISION={grammar_rev}");
}
