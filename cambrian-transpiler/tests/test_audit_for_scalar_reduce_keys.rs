// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! TI-02 — `gen_for_scalar_reduce` uses return-type hint over key element type.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const FIXTURE: &str = r#"
library TokenSpec {
    pure fn count_key_slots(m: HashMap<address, u128>) -> u64 {
        for k in m.keys() { k + 1 }
    }
}

entity KeyCountHost {
    routes {
        #[factory_only]
        constructor() => []
        view count() -> u64 => [ return(TokenSpec::count_key_slots(m_map)) ]
    }
    m_map: HashMap<address, u128> {
        in constructor() => HashMap::new()
    }
}
"#;

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_out_dir() -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-scalar-reduce-{}-{}",
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
    fs::write(dir.join("key_count.cam"), FIXTURE).expect("write cam");
    fs::write(
        dir.join("project.yaml"),
        "name: scalar-reduce-keys\n\
         target: evm\n\
         output_dir: build/\n\
         sources:\n\
           - key_count.cam\n",
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
fn scalar_reduce_keys_uses_return_type_for_acc_forge_build() {
    if !has_forge() {
        panic!("forge required for scalar_reduce_keys_uses_return_type_for_acc_forge_build");
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
        combined.contains("mapping(address => uint128) storage, address[] storage m_keys"),
        "unused mapping param must be unnamed so solc 5667 cannot fail the build:\n{combined}"
    );
    assert!(
        combined.contains("uint64 _cam_tmp") || combined.contains("uint64 _cam_tmp0"),
        "scalar reduce acc must be uint64 from return hint:\n{combined}"
    );
    assert!(
        combined.contains("uint64(uint160(") || combined.contains("uint64 (uint160("),
        "address key + literal must narrow to return width, not uint256(uint160):\n{combined}"
    );

    fs::write(
        out_dir.join("foundry.toml"),
        r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
optimizer = false
deny = "warnings"

[lint]
lint_on_build = false
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
