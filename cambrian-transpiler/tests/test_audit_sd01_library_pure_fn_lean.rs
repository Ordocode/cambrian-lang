// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! SD-01 — library-scoped `pure fn` must emit to `Cambrian.Generated.Pure` on Lean.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_out_dir() -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-sd01-lib-lean-{}-{}",
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

fn has_lake() -> bool {
    Command::new("lake")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

const FIXTURE: &str = r#"
library TokenSpec {
    pure fn sum_values(m: HashMap<address, U256>) -> U256 {
        m.values().fold(0, |acc, v| acc + v)
    }
}

entity MapHost {
    routes {
        constructor() => []
        view sumAll() -> U256 => [ return(TokenSpec::sum_values(m_map)) ]
    }
    m_map: HashMap<address, U256> {
        in constructor() => HashMap::new()
    }
}
"#;

#[test]
fn sd01_library_pure_fn_values_fold_lean_build() {
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").ok().as_deref() != Some("1") {
        eprintln!("skipping: set CAMBRIAN_TEST_LEAN_BUILD=1");
        return;
    }
    if !has_lake() {
        panic!("lake required when CAMBRIAN_TEST_LEAN_BUILD=1");
    }

    let work = unique_out_dir();
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).expect("mkdir work");
    fs::write(work.join("map_host.cam"), FIXTURE).expect("write cam");
    fs::write(
        work.join("project.lean.yaml"),
        "name: sd01-lib-lean\n\
         target: lean\n\
         output_dir: build/\n\
         sources:\n\
           - map_host.cam\n",
    )
    .expect("write yaml");

    let out_dir = work.join("build");
    let output = Command::new(transpiler_bin())
        .args(["--project", work.join("project.lean.yaml").to_str().unwrap()])
        .arg("-o")
        .arg(&out_dir)
        .arg("--target")
        .arg("lean")
        .output()
        .expect("transpile");
    assert!(
        output.status.success(),
        "transpile failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pure = fs::read_to_string(out_dir.join("Cambrian/Generated/Pure.lean")).expect("Pure.lean");
    assert!(
        pure.contains("def TokenSpec_sum_values"),
        "library pure fn must emit qualified symbol:\n{pure}"
    );
    assert!(
        pure.contains("AddressMap.values"),
        "library body must lower values chain:\n{pure}"
    );

    let routes = fs::read_to_string(out_dir.join("Cambrian/Generated/MapHostRoutes.lean"))
        .expect("routes");
    assert!(
        routes.contains("TokenSpec_sum_values"),
        "route must call library pure fn:\n{routes}"
    );

    let lake = Command::new("lake")
        .arg("build")
        .current_dir(&out_dir)
        .output()
        .expect("lake build");
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&lake.stdout),
        String::from_utf8_lossy(&lake.stderr)
    );
    let _ = fs::remove_dir_all(&work);
    assert!(lake.status.success(), "lake build failed:\n{log}");
}
