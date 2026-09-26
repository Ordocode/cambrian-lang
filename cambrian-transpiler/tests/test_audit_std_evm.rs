// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase L Wave 6 — audit EVM `std::` forge gates (T-STD-EVM-001/002/003).
//!
//! Reuses conformance fixtures under `tests/fixtures/`. See `docs/AUDIT_EVM_LEAN.md` §6 Phase L.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::codegen::{EvmSolidityBackend, OutputBackend};
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

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

struct StdForgeCase<'a> {
    test_id: &'a str,
    hypothesis: &'a str,
    yaml_name: &'a str,
    cam_name: &'a str,
    forge_test_file: &'a str,
    forge_contract: &'a str,
    repro_subdir: &'a str,
    route_ids: &'a [&'a str],
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn audit_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit")
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-std-evm-{}-{}-{}",
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

fn transpile_project(yaml_path: &Path, out_dir: &Path) {
    let project = Project::load(yaml_path).expect("load std fixture project");
    let det = project.config.resolved_deterministic_addresses();
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    for (rel, contents) in backend.gen_project(&project) {
        let path = out_dir.join(&rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }
}

fn read_generated_sol(out_dir: &Path) -> String {
    let src = out_dir.join("src");
    fs::read_dir(&src)
        .expect("src dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "sol"))
        .map(|p| fs::read_to_string(p).expect("read sol"))
        .collect::<Vec<_>>()
        .join("\n---\n")
}

fn write_repro(case: &StdForgeCase<'_>, out_dir: &Path, forge_log: &str) {
    let dir = audit_root().join(case.repro_subdir);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create repro dir");

    let yaml_src = fixtures_dir().join(case.yaml_name);
    let cam_src = fixtures_dir().join(case.cam_name);
    let forge_test = fixtures_dir().join(case.forge_test_file);

    let _ = fs::copy(&yaml_src, dir.join(case.yaml_name));
    let _ = fs::copy(&cam_src, dir.join(case.cam_name));
    let forge_dest = dir.join(
        Path::new(case.forge_test_file)
            .file_name()
            .expect("forge test filename"),
    );
    let _ = fs::copy(&forge_test, forge_dest);

    fs::write(dir.join("generated.sol"), read_generated_sol(out_dir)).expect("write generated.sol");
    fs::write(dir.join("forge.log"), forge_log).expect("write forge.log");

    let routes = case.route_ids.join(", ");
    let rerun = match case.test_id {
        "T-STD-EVM-001" => "audit_std_evm_math_matrix_forge",
        "T-STD-EVM-002" => "audit_std_evm_str_forge",
        "T-STD-EVM-003" => "audit_std_evm_crypto_forge",
        _ => "audit_std_evm_*",
    };
    let note = format!(
        "# {} / {} repro\n\n\
         Forge test failed for `{}`.\n\n\
         Fixture: `tests/fixtures/{}`\n\
         Routes: {}\n\n\
         Re-run:\n\
         `cargo test -p cambrian-transpiler --test test_audit_std_evm {} -- --nocapture`\n\n\
         --- forge log ---\n\
         {}\n",
        case.test_id,
        case.hypothesis,
        case.forge_contract,
        case.cam_name,
        routes,
        rerun,
        forge_log,
    );
    fs::write(dir.join("NOTE.md"), note).expect("write NOTE.md");
}

fn run_std_forge_case(case: &StdForgeCase<'_>) {
    if !has_forge() {
        eprintln!(
            "skipping {}: forge not on PATH (coverage / non-Foundry CI images; \
             Forge execution lives in test-transpiler-evm-forge)",
            case.test_id
        );
        return;
    }

    let tag = case.test_id.replace('-', "_").to_lowercase();
    let out_dir = unique_out_dir(&tag);
    let _ = fs::remove_dir_all(&out_dir);
    fs::create_dir_all(&out_dir).expect("create out dir");

    let yaml = fixtures_dir().join(case.yaml_name);
    transpile_project(&yaml, &out_dir);

    fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).expect("foundry.toml");

    let test_dir = out_dir.join("test");
    fs::create_dir_all(&test_dir).expect("test dir");
    let src_test = fixtures_dir().join(case.forge_test_file);
    let test_name = Path::new(case.forge_test_file)
        .file_name()
        .expect("forge test filename");
    fs::copy(&src_test, test_dir.join(test_name)).expect("copy forge test");

    ensure_forge_std(&out_dir);

    let forge = Command::new("forge")
        .args([
            "test",
            "--match-contract",
            case.forge_contract,
            "-vv",
            "--root",
        ])
        .arg(&out_dir)
        .output()
        .expect("forge test");

    let forge_log = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );

    let ok = forge.status.success();
    if !ok {
        write_repro(case, &out_dir, &forge_log);
    }

    let _ = fs::remove_dir_all(&out_dir);

    let verdict = if ok {
        match case.test_id {
            "T-STD-EVM-001" => "PASS — STD-H-EVM-1 REFUTED (math matrix)",
            "T-STD-EVM-002" => "PASS — STD-H-EVM-2 REFUTED (str matrix)",
            "T-STD-EVM-003" => "PASS — STD-H-EVM-3 REFUTED (crypto matrix)",
            _ => "PASS",
        }
    } else {
        match case.test_id {
            "T-STD-EVM-001" => "FAIL — STD-H-EVM-1 CONFIRMED",
            "T-STD-EVM-002" => "FAIL — STD-H-EVM-2 CONFIRMED",
            "T-STD-EVM-003" => "FAIL — STD-H-EVM-3 CONFIRMED",
            _ => "FAIL",
        }
    };

    eprintln!(
        "{} {}: {} — {} forge ({} routes)\n{forge_log}",
        case.test_id,
        case.hypothesis,
        verdict,
        case.forge_contract,
        case.route_ids.len(),
    );

    let repro = audit_root().join(case.repro_subdir);
    assert!(
        ok,
        "{} {} CONFIRMED — forge test failed; repro: {}\n{forge_log}",
        case.test_id,
        case.hypothesis,
        repro.display(),
    );
}

const MATH_ROUTE_IDS: &[&str] = &[
    "runMin",
    "runMax",
    "runAbsI",
    "runClamp",
    "runMuldiv",
    "runMuldivmod",
    "runDivmod",
    "runDivc",
    "runDivr",
    "runSign",
    "runMinmax",
    "runModpow2",
    "runPow",
    "runMuldivU256Wide",
];

const STR_ROUTE_IDS: &[&str] = &[
    "runFormatDefault",
    "runFormatPad6",
    "runParseDec",
    "runParseHex",
    "runRoundTrip",
];

const CRYPTO_ROUTE_IDS: &[&str] = &["runStdSha256", "runEvmSha256"];

#[test]
fn audit_std_evm_math_matrix_forge() {
    run_std_forge_case(&StdForgeCase {
        test_id: "T-STD-EVM-001",
        hypothesis: "STD-H-EVM-1",
        yaml_name: "std_math_matrix.yaml",
        cam_name: "std_math_matrix.cam",
        forge_test_file: "forge/StdMathMatrix.t.sol",
        forge_contract: "StdMathMatrixTest",
        repro_subdir: "std_evm_repro/t_std_evm_001",
        route_ids: MATH_ROUTE_IDS,
    });
}

#[test]
fn audit_std_evm_str_forge() {
    run_std_forge_case(&StdForgeCase {
        test_id: "T-STD-EVM-002",
        hypothesis: "STD-H-EVM-2",
        yaml_name: "std_str_matrix.yaml",
        cam_name: "std_str_matrix.cam",
        forge_test_file: "forge/StdStrMatrix.t.sol",
        forge_contract: "StdStrMatrixTest",
        repro_subdir: "std_evm_repro/t_std_evm_002",
        route_ids: STR_ROUTE_IDS,
    });
}

#[test]
fn audit_std_evm_crypto_forge() {
    run_std_forge_case(&StdForgeCase {
        test_id: "T-STD-EVM-003",
        hypothesis: "STD-H-EVM-3",
        yaml_name: "std_crypto_matrix.yaml",
        cam_name: "std_crypto_matrix.cam",
        forge_test_file: "forge/StdCryptoMatrix.t.sol",
        forge_contract: "StdCryptoMatrixTest",
        repro_subdir: "std_evm_repro/t_std_evm_003",
        route_ids: CRYPTO_ROUTE_IDS,
    });
}
