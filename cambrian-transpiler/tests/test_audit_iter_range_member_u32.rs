// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! TI-05 / Phase D.2 — `for i in 0..m_limit` in member transform keeps `uint32[]`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const FIXTURE: &str = r#"
entity RangeMemberHost {
    routes {
        #[factory_only]
        constructor() => []
        fill() => []
        view read() -> Vec<u32> => [ return(m_vec) ]
    }
    m_limit: u32 {
        in constructor() => 10
    }
    m_vec: Vec<u32> {
        in constructor() => array()
        in fill() => {
            for i in 0..m_limit { i }
        }
    }
}
"#;

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_out_dir() -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-range-member-{}-{}",
        std::process::id(),
        n
    ))
}

fn transpiler_bin() -> PathBuf {
    let mut path = std::env::current_exe().expect("current_exe");
    path.pop();
    path.pop();
    path.push("cambrian-transpiler");
    path
}

fn has_forge() -> bool {
    Command::new("forge")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn write_fixture_cam(dir: &Path) {
    fs::write(dir.join("range_member.cam"), FIXTURE).expect("write cam");
    fs::write(
        dir.join("project.yaml"),
        "name: range-member-u32\n\
         target: evm\n\
         output_dir: build/\n\
         sources:\n\
           - range_member.cam\n",
    )
    .expect("write yaml");
}

fn ensure_forge_std(out_dir: &Path) {
    if out_dir.join("lib/forge-std").is_dir() {
        return;
    }
    let _ = Command::new("git")
        .args(["init", "-q"])
        .current_dir(out_dir)
        .status();
    let status = Command::new("forge")
        .args(["install", "foundry-rs/forge-std", "--no-git"])
        .current_dir(out_dir)
        .status()
        .expect("forge install");
    assert!(status.success(), "forge install forge-std failed");
}

#[test]
fn range_over_member_limit_u32_forge_build() {
    if !has_forge() {
        panic!("forge required for range_over_member_limit_u32_forge_build");
    }

    let work = unique_out_dir();
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).expect("mkdir work");
    write_fixture_cam(&work);

    let out_dir = work.join("build");
    let output = Command::new(transpiler_bin())
        .args([
            "--project",
            work.join("project.yaml").to_str().unwrap(),
        ])
        .arg("-o")
        .arg(&out_dir)
        .arg("--target")
        .arg("evm")
        .output()
        .expect("transpile");
    assert!(
        output.status.success(),
        "transpile failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let combined = fs::read_dir(out_dir.join("src"))
        .expect("read src")
        .filter_map(|e| e.ok())
        .find(|e| {
            e.path()
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with("_project.sol"))
                .unwrap_or(false)
        })
        .map(|e| fs::read_to_string(e.path()).expect("read combined sol"))
        .expect("combined project .sol");
    assert!(
        combined.contains("function fill"),
        "fill route must be emitted:\n{combined}"
    );
    assert!(
        combined.contains("uint32 _cam_tmp4 = m_limit")
            || combined.contains("uint32 _cam_tmp3 = 0"),
        "range end must resolve m_limit as uint32:\n{combined}"
    );
    assert!(
        combined.contains("uint32[] memory"),
        "member transform for-loop must materialise uint32[]:\n{combined}"
    );

    fs::write(
        out_dir.join("foundry.toml"),
        r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
optimizer = false
"#,
    )
    .expect("foundry.toml");
    ensure_forge_std(&out_dir);

    let forge = Command::new("forge")
        .args(["build", "--root"])
        .arg(&out_dir)
        .output()
        .expect("forge build");

    let log = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );
    let _ = fs::remove_dir_all(&work);

    assert!(forge.status.success(), "forge build failed:\n{log}");
}
