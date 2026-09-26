// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! SD-01-VFOLD — chained `HashMap.values().fold` / `keys().fold` in `pure fn` on Lean.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_out_dir() -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-sd01-vfold-lean-{}-{}",
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

fn write_fixture(dir: &Path, cam: &str) {
    fs::write(dir.join("map_fold.cam"), cam).expect("write cam");
    fs::write(
        dir.join("project.lean.yaml"),
        "name: sd01-vfold-lean\n\
         target: lean\n\
         output_dir: build/\n\
         sources:\n\
           - map_fold.cam\n",
    )
    .expect("write yaml");
}

fn transpile_lean(work: &Path) -> PathBuf {
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
    out_dir
}

fn lake_build(out_dir: &Path) -> (bool, String) {
    let lake = Command::new("lake")
        .arg("build")
        .current_dir(out_dir)
        .output()
        .expect("lake build");
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&lake.stdout),
        String::from_utf8_lossy(&lake.stderr)
    );
    (lake.status.success(), log)
}

fn read_pure_lean(out_dir: &Path) -> String {
    fs::read_to_string(out_dir.join("Cambrian/Generated/Pure.lean")).expect("Pure.lean")
}

const PURE_FN_FIXTURE: &str = r#"
pure fn sum_values(m: HashMap<address, U256>) -> U256 {
    m.values().fold(0, |acc, v| acc + v)
}

pure fn sum_keys(m: HashMap<address, U256>) -> U256 {
    m.keys().fold(0, |acc, k| acc + m[k])
}

entity MapFoldHost {
    routes {
        constructor() => []
        view sumAll() -> U256 => [ return(sum_values(m_map)) ]
    }
    m_map: HashMap<address, U256> {
        in constructor() => HashMap::new()
    }
}
"#;

fn run_lean_gate(test_name: &str, assert_pure: fn(&str)) {
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").ok().as_deref() != Some("1") {
        eprintln!("skipping {test_name}: set CAMBRIAN_TEST_LEAN_BUILD=1");
        return;
    }
    if !has_lake() {
        panic!("{test_name}: lake required when CAMBRIAN_TEST_LEAN_BUILD=1");
    }

    let work = unique_out_dir();
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).expect("mkdir work");
    write_fixture(&work, PURE_FN_FIXTURE);

    let out_dir = transpile_lean(&work);
    let pure = read_pure_lean(&out_dir);
    assert_pure(&pure);

    let (ok, log) = lake_build(&out_dir);
    let _ = fs::remove_dir_all(&work);
    assert!(ok, "lake build failed:\n{log}");
}

#[test]
fn sd01_pure_fn_values_fold_chain_lean_build() {
    run_lean_gate("sd01_pure_fn_values_fold_chain_lean_build", |pure| {
        assert!(
            pure.contains("Cambrian.AddressMap.values"),
            "values chain must lower via AddressMap.values:\n{pure}"
        );
        let bad = pure.lines().any(|l| {
            l.contains("def sum_values")
                && l.contains("List.foldl")
                && l.trim_end().ends_with("values)")
        });
        assert!(!bad, "must not List.foldl over AddressMap param:\n{pure}");
    });
}

#[test]
fn sd01_pure_fn_keys_fold_chain_lean_build() {
    run_lean_gate("sd01_pure_fn_keys_fold_chain_lean_build", |pure| {
        assert!(
            pure.contains("Cambrian.AddressMap.keys"),
            "keys chain must lower via AddressMap.keys:\n{pure}"
        );
        let bad = pure.lines().any(|l| {
            l.contains("def sum_keys")
                && l.contains("List.foldl")
                && l.trim_end().ends_with("m)")
        });
        assert!(!bad, "must not fold over map param:\n{pure}");
    });
}

#[test]
fn sd01_pure_fn_iter_fold_chain_unchanged() {
    const ITER_ONLY: &str = r#"
pure fn sum_iter(m: HashMap<address, U256>) -> U256 {
    m.iter().fold(0, |acc, kv| acc + kv.1)
}

entity MapFoldHost {
    routes {
        constructor() => []
        view sumAll() -> U256 => [ return(sum_iter(m_map)) ]
    }
    m_map: HashMap<address, U256> {
        in constructor() => HashMap::new()
    }
}
"#;
    let work = unique_out_dir();
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).expect("mkdir work");
    write_fixture(&work, ITER_ONLY);
    let out_dir = transpile_lean(&work);
    let pure = read_pure_lean(&out_dir);
    let _ = fs::remove_dir_all(&work);
    assert!(
        pure.contains("def sum_iter")
            && pure.contains("List.foldl (fun acc kv")
            && pure.contains(" m)"),
        "iter().fold must still fold the bare AddressMap pair list:\n{pure}"
    );
    assert!(
        !pure.contains("AddressMap.keys m)"),
        "iter-only chain must not expand to keys list:\n{pure}"
    );
}

#[test]
fn sd01_record_field_values_fold_emits_address_map() {
    const FIELD_FOLD: &str = r#"
record Bag { balances: HashMap<address, U256> }

pure fn sum_bag(b: Bag) -> U256 {
    b.balances.values().fold(0, |acc, v| acc + v)
}

entity MapFoldHost {
    routes {
        constructor() => []
        view sumAll(b: Bag) -> U256 => [ return(sum_bag(b)) ]
    }
    m_n: u64 { in constructor() => 0 }
}
"#;
    let work = unique_out_dir();
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).expect("mkdir work");
    write_fixture(&work, FIELD_FOLD);
    let out_dir = transpile_lean(&work);
    let pure = read_pure_lean(&out_dir);

    assert!(
        pure.contains("Cambrian.AddressMap.values"),
        "record-field values chain must lower via AddressMap.values:\n{pure}"
    );
    assert!(
        !pure.contains("_keys"),
        "must not treat record-field map as this entity's state sidecar:\n{pure}"
    );

    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").ok().as_deref() == Some("1") {
        if !has_lake() {
            panic!("sd01_record_field_values_fold_emits_address_map: lake required when CAMBRIAN_TEST_LEAN_BUILD=1");
        }
        let (ok, log) = lake_build(&out_dir);
        let _ = fs::remove_dir_all(&work);
        assert!(ok, "lake build failed:\n{log}");
    } else {
        let _ = fs::remove_dir_all(&work);
    }
}
