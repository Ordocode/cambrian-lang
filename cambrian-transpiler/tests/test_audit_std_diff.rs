// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase L Wave 6 — EVM↔Lean differential gate for shared `std::math` (T-STD-DIFF-001).
//!
//! See `docs/AUDIT_EVM_LEAN.md` §6 Phase L / §3.8 STD-H-DIFF-1.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::{EvmSolidityBackend, LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;

const FOUNDRY_TOML: &str = r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
evm_version = "prague"
optimizer = false
optimizer_runs = 200
via_ir = false
"#;

const TEST_ID: &str = "T-STD-DIFF-001";
const HYPOTHESIS: &str = "STD-H-DIFF-1";
const CAM_NAME: &str = "std_diff_math.cam";
const EVM_YAML: &str = "std_diff_math_evm.yaml";
const LEAN_YAML: &str = "std_diff_math_lean.yaml";
const FORGE_TEST_FILE: &str = "StdDiffMath.t.sol";
const FORGE_CONTRACT: &str = "StdDiffMathTest";
const LEAN_ROUTES_FILE: &str = "Cambrian/Generated/StdDiffMathRoutes.lean";
const REPRO_SUBDIR: &str = "std_diff_repro/t_std_diff_001";

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn audit_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit")
}

fn fixtures_dir() -> PathBuf {
    audit_root().join("fixtures")
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-std-diff-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ))
}

fn has_forge() -> bool {
    Command::new("forge")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ensure_forge_std(out_dir: &Path) {
    cambrian_transpiler::codegen::evm_test_codegen::install_forge_std(out_dir)
        .unwrap_or_else(|e| panic!("{e} in {}", out_dir.display()));
}

fn extract_route_body(lean: &str, route_name: &str) -> Option<String> {
    let needle = format!("def {route_name} ");
    let start = lean.find(&needle)?;
    let rest = &lean[start..];
    let end = rest.find("\n\n").unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

struct ForgeRun {
    ok: bool,
    log: String,
}

struct LeanInspect {
    ok: bool,
    detail: String,
}

fn run_forge() -> ForgeRun {
    let out_dir = unique_out_dir("forge");
    let _ = fs::remove_dir_all(&out_dir);
    fs::create_dir_all(&out_dir).expect("create out dir");

    let yaml = fixtures_dir().join(EVM_YAML);
    let project = Project::load(&yaml).expect("load evm project");
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    for (rel, contents) in backend.gen_project(&project) {
        let path = out_dir.join(&rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, contents).expect("write sol");
    }

    fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).expect("foundry.toml");

    let test_dir = out_dir.join("test");
    fs::create_dir_all(&test_dir).expect("test dir");
    let src_test = fixtures_dir().join("forge").join(FORGE_TEST_FILE);
    fs::copy(&src_test, test_dir.join(FORGE_TEST_FILE)).expect("copy forge test");
    ensure_forge_std(&out_dir);

    let forge = Command::new("forge")
        .args([
            "test",
            "--match-contract",
            FORGE_CONTRACT,
            "-vv",
            "--root",
        ])
        .arg(&out_dir)
        .output()
        .expect("forge test");

    let log = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );
    let ok = forge.status.success();
    let _ = fs::remove_dir_all(&out_dir);
    ForgeRun { ok, log }
}

fn inspect_lean_route(
    files: &HashMap<String, String>,
    route_name: &str,
    required: &[&str],
    forbidden: &[&str],
) -> LeanInspect {
    let lean = match files.get(LEAN_ROUTES_FILE) {
        Some(s) => s,
        None => {
            return LeanInspect {
                ok: false,
                detail: format!("missing {LEAN_ROUTES_FILE}"),
            };
        }
    };
    let body = match extract_route_body(lean, route_name) {
        Some(b) => b,
        None => {
            return LeanInspect {
                ok: false,
                detail: format!("route `{route_name}` not found in {LEAN_ROUTES_FILE}"),
            };
        }
    };

    for needle in forbidden {
        if body.contains(needle) {
            return LeanInspect {
                ok: false,
                detail: format!(
                    "route `{route_name}` contains forbidden `{needle}`\n{body}"
                ),
            };
        }
    }
    for needle in required {
        if !body.contains(needle) {
            return LeanInspect {
                ok: false,
                detail: format!(
                    "route `{route_name}` missing `{needle}`\n{body}"
                ),
            };
        }
    }

    LeanInspect {
        ok: true,
        detail: format!("route `{route_name}` aligned: {body}"),
    }
}

fn inspect_lean() -> LeanInspect {
    let yaml = fixtures_dir().join(LEAN_YAML);
    let project = Project::load(&yaml).expect("load lean project");
    let files: HashMap<String, String> = LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .collect();

    let muldiv = inspect_lean_route(
        &files,
        "runMuldiv",
        &["Cambrian.checkedMuldiv 100 200 50"],
        &["Cambrian.Unsupported"],
    );
    if !muldiv.ok {
        return muldiv;
    }

    let clamp = inspect_lean_route(
        &files,
        "runClamp",
        &["Cambrian.clamp (150 : BitVec 64) (0 : BitVec 64) (100 : BitVec 64)"],
        &["Cambrian.Unsupported"],
    );
    if !clamp.ok {
        return clamp;
    }

    LeanInspect {
        ok: true,
        detail: format!("{}\n{}", muldiv.detail, clamp.detail),
    }
}

fn write_repro(forge_log: &str, lean_detail: &str, diverged: bool) {
    let dir = audit_root().join(REPRO_SUBDIR);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create repro dir");

    let fixtures = fixtures_dir();
    let _ = fs::copy(fixtures.join(CAM_NAME), dir.join(CAM_NAME));
    let _ = fs::copy(fixtures.join(EVM_YAML), dir.join(EVM_YAML));
    let _ = fs::copy(fixtures.join(LEAN_YAML), dir.join(LEAN_YAML));
    let _ = fs::copy(
        fixtures.join("forge").join(FORGE_TEST_FILE),
        dir.join(FORGE_TEST_FILE),
    );

    fs::write(dir.join("forge.log"), forge_log).expect("forge.log");
    fs::write(dir.join("lean_inspect.txt"), lean_detail).expect("lean_inspect.txt");

    let note = format!(
        "# {TEST_ID} / {HYPOTHESIS} repro\n\n\
         Divergence: {}\n\n\
         Fixture: `tests/audit/fixtures/{CAM_NAME}`\n\n\
         Re-run:\n\
         `cargo test -p cambrian-transpiler --test test_audit_std_diff audit_std_diff_math -- --nocapture`\n\n\
         --- forge log ---\n\
         {forge_log}\n\n\
         --- lean inspect ---\n\
         {lean_detail}\n",
        if diverged {
            "YES — EVM PASS, Lean not aligned"
        } else {
            "forge or inspect failure"
        },
    );
    fs::write(dir.join("NOTE.md"), note).expect("NOTE.md");
}

#[test]
fn audit_std_diff_math() {
    if !has_forge() {
        eprintln!(
            "skipping audit_std_diff_math: forge not on PATH (coverage / \
             non-Foundry CI images; Forge execution lives in \
             test-transpiler-phase-m-gates)"
        );
        return;
    }

    let forge = run_forge();
    let lean = inspect_lean();

    let diverged = forge.ok && !lean.ok;
    let ok = forge.ok && lean.ok;

    if !ok {
        write_repro(&forge.log, &lean.detail, diverged);
    }

    let verdict = if ok {
        "PASS — STD-H-DIFF-1 REFUTED (forge + Lean aligned)"
    } else if diverged {
        "FAIL — STD-H-DIFF-1 CONFIRMED (EVM PASS, Lean diverged)"
    } else if !forge.ok {
        "FAIL — forge did not PASS"
    } else {
        "FAIL — unexpected state"
    };

    eprintln!(
        "{TEST_ID} {HYPOTHESIS}: {verdict}\n\
         EVM forge: {}\n{}\n\
         Lean inspect: {}\n{}",
        if forge.ok { "PASS" } else { "FAIL" },
        forge.log,
        if lean.ok { "PASS" } else { "FAIL" },
        lean.detail,
    );

    let repro = audit_root().join(REPRO_SUBDIR);
    assert!(
        ok,
        "{TEST_ID} {HYPOTHESIS} — repro: {}\nforge ok={} lean ok={} diverged={}",
        repro.display(),
        forge.ok,
        lean.ok,
        diverged,
    );
}
