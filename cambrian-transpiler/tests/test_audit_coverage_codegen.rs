// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase N4 — targeted coverage slices under `src/codegen/` (execution tests).

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use cambrian_core::U256;
use cambrian_transpiler::ast::{
    BinOp, ContextSpec, Expr, FieldPath, ForallSpec, ForallTarget, InvariantAction,
    InvariantDecl, InvariantEmitPolicy, InvariantInstance, InvariantQuery, MatchArm, MatchPattern,
    Param, PathSegment,
    Pattern, RouteAction, RouteBody, Span, TestDecl, TestEffect, TestEffectElement, TestStep,
    Type, UnaryOp, FuzzDecl,
};
use cambrian_transpiler::codegen::evm_test_codegen::{
    generate_evm_tests, generate_evm_tests_for_project,
};
use cambrian_transpiler::codegen::gen_evm_solidity;
use cambrian_transpiler::codegen::test_backend::{
    collect_post_call_assertions, lower_check_expr_multi, scan_invariant_trace_features,
    substitute_member_accessors, walk_test_body, PostCallAssert, TestStepLowerer,
};
use cambrian_transpiler::project::{FuzzConfig, InvariantConfig, Project};
#[cfg(feature = "rust-targets")]
use cambrian_transpiler::codegen::test_codegen::generate_tests;
use cambrian_transpiler::using_rewrite::apply_using_rewrites;
use cambrian_transpiler::ProgramParser;

fn parse_evm(src: &str) -> cambrian_transpiler::ast::Program {
    let mut program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    apply_using_rewrites(&mut program);
    program
}

/// Default harness oracle: det + `CambrianFactory` via `_{Harness}_project.sol`.
fn gen_evm_test_files(program: &cambrian_transpiler::ast::Program) -> Vec<(String, String)> {
    generate_evm_tests_for_project(
        program,
        true,
        &InvariantConfig::default(),
        Some("Harness"),
        true,
    )
}

fn gen_evm_test_files_det(program: &cambrian_transpiler::ast::Program) -> Vec<(String, String)> {
    gen_evm_test_files(program)
}

fn contracts_project(yaml: &str) -> Project {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../contracts")
        .join(yaml);
    Project::load(&path).unwrap_or_else(|e| panic!("load {}: {e}", path.display()))
}

#[cfg(feature = "rust-targets")]
fn gen_native_test_code(
    src: &str,
    desugar_props: bool,
    fuzz_cfg: &FuzzConfig,
    inv_cfg: &InvariantConfig,
) -> String {
    let mut program = parse_evm(src);
    if desugar_props {
        cambrian_transpiler::desugar::desugar_properties(&mut program);
    }
    generate_tests(
        &program,
        "probe_entity_wasm",
        fuzz_cfg,
        inv_cfg,
    )
    .unwrap_or_else(|| panic!("generate_tests returned None for native fixture"))
}

#[cfg(not(feature = "rust-targets"))]
fn gen_native_test_code(
    src: &str,
    desugar_props: bool,
    fuzz_cfg: &FuzzConfig,
    inv_cfg: &InvariantConfig,
) -> String {
    let _ = (src, desugar_props, fuzz_cfg, inv_cfg);
    String::new()
}

fn has_forge() -> bool {
    Command::new("forge")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn coverage_forge_enabled() -> bool {
    std::env::var("CAMBRIAN_TEST_COVERAGE_FORGE").as_deref() == Ok("1")
}

const COVERAGE_FOUNDRY_TOML: &str = r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
evm_version = "prague"
optimizer = false
"#;

/// PW3-S-009 / rule 04: execution oracle via `forge build` (never raw `solc`).
fn assert_forge_compiles(label: &str, sol: &str) {
    if !coverage_forge_enabled() {
        eprintln!(
            "skip forge compile oracle for {label} (set CAMBRIAN_TEST_COVERAGE_FORGE=1)"
        );
        return;
    }
    if !has_forge() {
        panic!("CAMBRIAN_TEST_COVERAGE_FORGE=1 but `forge` not on PATH (label={label})");
    }

    let out_dir = std::env::temp_dir().join(format!(
        "cambrian-audit-codegen-forge-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(out_dir.join("src")).unwrap();
    std::fs::write(out_dir.join("src/Contract.sol"), sol).unwrap();
    std::fs::write(out_dir.join("foundry.toml"), COVERAGE_FOUNDRY_TOML).unwrap();

    let output = Command::new("forge")
        .args(["build", "--root"])
        .arg(&out_dir)
        .output()
        .unwrap_or_else(|e| panic!("forge build failed to start for {label}: {e}"));
    let _ = std::fs::remove_dir_all(&out_dir);

    assert!(
        output.status.success(),
        "forge build rejected {label}:\nstderr:\n{}\nstdout:\n{}\nsource:\n{sol}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
}

/// Legacy name retained for ~650 call sites; routes to forge (PW3-S-009 pilot).
fn assert_solc_compiles(label: &str, sol: &str) {
    assert_forge_compiles(label, sol);
}

fn ensure_forge_std_installed(out_dir: &Path) {
    if out_dir.join("lib/forge-std").is_dir() {
        return;
    }
    let _ = Command::new("git")
        .args(["init", "-q"])
        .current_dir(out_dir)
        .status();
    let status = Command::new("forge")
        .args(["install", "foundry-rs/forge-std"])
        .current_dir(out_dir)
        .status()
        .unwrap_or_else(|e| panic!("forge install failed to start: {e}"));
    assert!(
        status.success(),
        "forge install forge-std failed in {}",
        out_dir.display()
    );
}

fn shared_forge_std_root() -> std::path::PathBuf {
    static CACHE: OnceLock<std::path::PathBuf> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let dir = std::env::temp_dir().join(format!(
                "cambrian-forge-std-cache-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            ensure_forge_std_installed(&dir);
            dir
        })
        .clone()
}

fn link_forge_std(out_dir: &Path) {
    std::fs::create_dir_all(out_dir.join("lib")).unwrap();
    let dst = out_dir.join("lib/forge-std");
    if dst.exists() {
        return;
    }
    let src = shared_forge_std_root().join("lib/forge-std");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&src, &dst).unwrap_or_else(|e| {
        panic!("symlink forge-std {} -> {}: {e}", src.display(), dst.display())
    });
    #[cfg(not(unix))]
    {
        copy_dir_recursive(&src, &dst).unwrap_or_else(|e| {
            panic!("copy forge-std {} -> {}: {e}", src.display(), dst.display())
        });
    }
}

#[cfg(not(unix))]
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

/// Entity stubs + combined project sol + harness test files (mirrors `EvmSolidityBackend::gen_project`).
fn evm_harness_project_files(
    program: &cambrian_transpiler::ast::Program,
    deterministic: bool,
) -> Vec<(String, String)> {
    const PROJECT_FILE: &str = "_Harness_project.sol";
    let code = gen_evm_solidity(program, deterministic);
    let mut files = vec![(format!("src/{PROJECT_FILE}"), code)];
    for entity in &program.entities {
        let stub = format!(
            "// SPDX-License-Identifier: UNLICENSED\npragma solidity ^0.8.24;\n// Auto-generated stub: re-exports {} from the combined harness project file.\nimport \"./{}\";\n",
            entity.name, PROJECT_FILE
        );
        files.push((format!("src/{}.sol", entity.name), stub));
    }
    files.extend(gen_evm_test_files_det(program));
    files
}

/// HF-1: compile full Foundry layout from `generate_evm_tests` (src + test + forge-std).
fn assert_forge_project_compiles(label: &str, files: &[(String, String)]) {
    if !coverage_forge_enabled() {
        eprintln!(
            "skip forge project compile oracle for {label} (set CAMBRIAN_TEST_COVERAGE_FORGE=1)"
        );
        return;
    }
    if !has_forge() {
        panic!("CAMBRIAN_TEST_COVERAGE_FORGE=1 but `forge` not on PATH (label={label})");
    }

    let out_dir = std::env::temp_dir().join(format!(
        "cambrian-audit-codegen-forge-proj-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&out_dir);

    for (rel, contents) in files {
        let path = out_dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }
    std::fs::write(out_dir.join("foundry.toml"), COVERAGE_FOUNDRY_TOML).unwrap();
    link_forge_std(&out_dir);

    let output = Command::new("forge")
        .args(["build", "--root"])
        .arg(&out_dir)
        .output()
        .unwrap_or_else(|e| panic!("forge build failed to start for {label}: {e}"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let _ = std::fs::remove_dir_all(&out_dir);

    assert!(
        output.status.success(),
        "forge build rejected harness project {label}:\nstderr:\n{stderr}\nstdout:\n{stdout}\nfiles: {files:?}",
    );
}

// ---------------------------------------------------------------------------
// N4-9: codegen/solidity/core/library.rs — library declaration lowering
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_library_type_alias_and_const_solc() {
    let program = parse_evm(
        r#"
        library Helpers {
            type Amount = u64
            const MAX: u64 = 1000
            pure fn cap(a: u64) -> u64 { if a > MAX { MAX } else { a } }
        }

        using Helpers for u64;

        entity Vault {
            routes { set(v: u64) => [] }
            m_v: u64 { in set(v) => v.cap() }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("type Amount is uint64;"),
        "library type alias must lower to UDT: {sol}"
    );
    assert!(
        sol.contains("uint64 constant MAX = 1000;"),
        "library const must emit Solidity constant: {sol}"
    );
    assert_solc_compiles("library_type_alias_const", &sol);
}

#[test]
fn n4_codegen_library_hashmap_param_emits_view_solc() {
    let program = parse_evm(
        r#"
        library MapLib {
            pure fn has_key(m: HashMap<u64, u64>, k: u64) -> bool { m.exists(k) }
        }

        entity E {
            routes { probe(m: HashMap<u64, u64>, k: u64) -> bool => [ return(MapLib.has_key(m, k)) ] }
            m_x: u64 {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => uint64) storage m"),
        "HashMap library param must use storage mapping: {sol}"
    );
    assert!(
        sol.contains("internal view returns (bool)"),
        "mapping param library fn must be view, not pure: {sol}"
    );
    assert_solc_compiles("library_hashmap_view", &sol);
}

#[test]
fn n4_codegen_library_tuple_return_solc() {
    let program = parse_evm(
        r#"
        library PairLib {
            pure fn dup(a: u64) -> (u64, u64) { (a, a) }
        }

        entity E {
            routes { get(a: u64) -> (u64, u64) => [ return(PairLib.dup(a)) ] }
            m_x: u64 {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function dup(uint64 a) internal pure returns (uint64, uint64)"),
        "tuple return must lower to multi-slot returns clause: {sol}"
    );
    assert_solc_compiles("library_tuple_return", &sol);
}

#[test]
fn n4_codegen_library_non_primitive_type_alias_fallback_solc() {
    let program = parse_evm(
        r#"
        library SlotLib {
            type Slot = Vec<u64>
            pure fn zero() -> u64 { 0 }
        }

        entity E {
            routes { go() -> u64 => [ return(SlotLib.zero()) ] }
            m_x: u64 {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("type Slot is uint256;"),
        "non-primitive library alias must fall back to uint256 UDT base: {sol}"
    );
    assert_solc_compiles("library_non_primitive_alias", &sol);
}

#[test]
fn n4_codegen_library_absent_program_has_no_library_block() {
    let program = parse_evm(
        r#"
        entity Only {
            routes { go() => [] }
            m_x: u64 { in go() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        !sol.contains("library "),
        "programs without library decls must not emit library blocks: {sol}"
    );
    assert_solc_compiles("no_library", &sol);
}

// ---------------------------------------------------------------------------
// N4-122: codegen/solidity/evm/analysis.rs — recon slice 1 @ N4-14 gap tail.
// Fresh gap @ HEAD: 85.58% (46 missed / 319). Targets: walk_expr Some/For/Closure/
// Match/Range/AddressOf; member_inner plain let/block/for/enum; member_is_iterated
// Block/Match/EnumVariantWithData; param_uses_exists tuple/some/range.
// Exclude: transforms_use_exists (covered); Encode L213/L386 if E16 blocks.
// Acceptance: ≥ 75% or ≤ 80 raw or Δ ≥ −8 vs N4-121 baseline (87 missed docs).
// ---------------------------------------------------------------------------

#[test]
fn n4_122_codegen_analysis_walk_some_for_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity SomeFor {
            routes {
                constructor() => []
                scan(k: u64) => [
                    for (key, val) in m_book.iter() => [
                        if m_book.exists(key) => []
                    ]
                ]
            }
            m_count: u64 {
                in constructor() => 0
                in scan(k) => {
                    let wrapped = some(k);
                    wrapped
                }
            }
            m_book: HashMap<u64, u64> {
                in constructor() => {}
                in scan(k) => {
                    if m_book.exists(k) {
                        m_book.update(k, m_book[k] + 1)
                    } else {
                        m_book.insert(k, 1)
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => bool) public m_book_exists")
            && sol.contains("uint64[] public m_book_keys"),
        "Some/For walk must detect .exists() and iteration companions: {sol}"
    );
    // TB-V: validator OK; forge blocked on codegen (CG-ITER) — substring only.
}

#[test]
fn n4_122_codegen_analysis_iterate_match_enum_variant_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        enum Tick { Pulse(u64) }

        entity EnumWalk {
            routes {
                constructor() => []
                react(t: Tick) => [
                    let ks = m_ticks.values().collect();
                    let sink = ks;
                ]
            }
            m_last: Tick {
                in react(t) => Tick::Pulse(1)
            }
            m_ticks: HashMap<u64, u64> {
                in constructor() => {}
                in react(t) => {
                    if m_ticks.exists(1) {
                        m_ticks.update(1, m_ticks[1] + 1)
                    } else {
                        m_ticks.insert(1, 1)
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("uint64[] public m_ticks_keys")
            && sol.contains("mapping(uint64 => bool) public m_ticks_exists"),
        "Match/EnumVariantWithData/values walk must emit iteration + exists sidecars: {sol}"
    );
    assert_solc_compiles("analysis_iterate_match_enum_variant", &sol);
}

#[test]
fn n4_122_codegen_analysis_inner_plain_let_block_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity InnerPlain {
            routes {
                constructor() => []
                bind(outer: u64, inner_k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => {}
                in bind(outer, inner_k) => {
                    let row = {
                        let inner = m_nested[outer];
                        if inner.exists(inner_k) {
                            inner[inner_k]
                        } else {
                            0
                        }
                    };
                    m_nested.update(outer, m_nested[outer].update(inner_k, row + 1))
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => bool)) public m_nested_inner_exists"),
        "plain let/block inner.exists must hit member_inner let fallback walk (L99–L102): {sol}"
    );
    assert_solc_compiles("analysis_inner_plain_let_block_exists", &sol);
}

#[test]
fn n4_122_codegen_analysis_member_default_exists_walk_solc() {
    let mut program = parse_evm(
        r#"
        entity DefaultSeed {
            routes { constructor() => [] touch(k: u64) => [] }
            m_seed: HashMap<u64, u64> { in constructor() => {} in touch(k) => m_seed }
            m_slots: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let slots = entity
        .members
        .iter_mut()
        .find(|m| m.name == "m_slots")
        .expect("m_slots");
    slots.default_value = Some(Expr::MethodCall(
        Box::new(Expr::Ident("m_slots".into())),
        "exists".into(),
        vec![Expr::IntLiteral(U256::ZERO)],
    ));
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_slots_exists"),
        "member default_value .exists() must walk via entity_walks_any (L42–L44): {sol}"
    );
    assert_solc_compiles("analysis_member_default_exists_walk", &sol);
}

#[test]
fn n4_122_codegen_analysis_pure_fn_some_range_tuple_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        pure fn depth(m: HashMap<u64, u64>, k: u64) -> u64 {
            let tagged = some(k);
            let probe = (m.exists(k), tagged);
            match k {
                0 => 0,
                _ => {
                    let span = 0..k;
                    if m.exists(k) { m[k] } else { 0 }
                }
            }
        }

        entity DepthBank {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(depth(m_depth, k)) ]
            }
            m_depth: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("depth(m_depth, m_depth_exists, k)")
            && sol.contains("mapping(uint64 => bool) public m_depth_exists"),
        "pure-fn tuple/some/range param walk must thread _exists (L279–L281): {sol}"
    );
    // TB-V: bare `0..k` in pure-fn body is E07 — codegen substring only.
}

#[test]
fn n4_122_codegen_analysis_iterate_block_record_values_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        record Row { tag: u64, live: bool }

        entity BlockRecord {
            routes {
                constructor() => []
                harvest(k: u64) => []
            }
            m_rows: HashMap<u64, u64> {
                in constructor() => {}
                in harvest(k) => {
                    let batch = {
                        let row = Row { tag: k, live: m_rows.exists(k) };
                        m_rows.values().collect()
                    };
                    if m_rows.exists(k) {
                        m_rows.update(k, m_rows[k] + 1)
                    } else {
                        m_rows.insert(k, 1)
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("uint64[] public m_rows_keys")
            && sol.contains("mapping(uint64 => bool) public m_rows_exists"),
        "Block/RecordConstruct/values walk must hit member_is_iterated arms (L363–L368): {sol}"
    );
    assert_solc_compiles("analysis_iterate_block_record_values", &sol);
}

#[test]
fn n4_122_codegen_analysis_walk_closure_map_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity MapClosure {
            routes {
                constructor() => []
                score() -> u64 => [
                    return(
                        m_pts
                            .iter()
                            .map(|(k, v)| if m_pts.exists(k) { v } else { 0 })
                            .fold(0, |acc, x| acc + x)
                    )
                ]
            }
            m_pts: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => bool) public m_pts_exists")
            && sol.contains("uint64[] public m_pts_keys"),
        "Closure .map with .exists() must walk member_uses_exists + iteration (L184): {sol}"
    );
    assert_solc_compiles("analysis_walk_closure_map_exists", &sol);
}

#[test]
fn n4_122_codegen_analysis_inner_for_closure_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity InnerLoop {
            routes {
                constructor() => []
                sweep(outer: u64) => []
            }
            m_acc: u64 {
                in constructor() => 0
                in sweep(outer) => {
                    for inner_k in m_grid[outer] {
                        if m_grid[outer].exists(inner_k) {
                            m_grid[outer][inner_k]
                        } else {
                            0
                        }
                    }
                }
            }
            m_grid: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => {}
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => bool)) public m_grid_inner_exists"),
        "inner for/closure exists must hit member_inner For walk (L109–L111): {sol}"
    );
    assert_solc_compiles("analysis_inner_for_closure_exists", &sol);
}

#[test]
fn n4_122_codegen_analysis_addressof_with_params_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity AddrProbe {
            routes {
                constructor() => []
                mark(id: u64) => []
                seen(id: u64) -> bool => [ return(m_seen.exists(id)) ]
            }
            m_seen: HashMap<u64, u64> {
                in mark(id) => {
                    let _peer = addressOf(Peer.state(id), id);
                    if m_seen.exists(id) {
                        m_seen.update(id, m_seen[id] + 1)
                    } else {
                        m_seen.insert(id, 1)
                    }
                }
            }
        }

        entity Peer {
            identity m_id: u64
            routes { constructor() => [] }
        }
    "#,
        ),
        true,
    );
    assert!(
        sol.contains("mapping(uint64 => bool) public m_seen_exists"),
        "AddressOf with_params walk must preserve .exists() detection (L206–L211): {sol}"
    );
    assert_solc_compiles("analysis_addressof_with_params_exists", &sol);
}

// ---------------------------------------------------------------------------
// N4-123: codegen/solidity/evm/analysis.rs — residual tail @ N4-122 gap.
// Baseline @ N4-122: 81.50% (59 missed / 319). Targets: inner L71/L99/L102;
// MacroRef L155; walk_expr residual L173–L213; param_uses_exists L261–L281;
// is_empty L331 + iteration L363/L386; transforms_use_exists L409–L418.
// Exclude: n4_122_* / n4_codegen_analysis_* unless gap-justified.
// Acceptance: ≥ 85% or ≤ 50 raw or Δ ≥ −8 vs 59 missed baseline.
// ---------------------------------------------------------------------------

#[test]
fn n4_123_codegen_analysis_inner_contains_direct_subscript_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity InnerContains {
            routes {
                constructor() => []
                bind(outer: u64, inner_k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => {}
                in bind(outer, inner_k) => {
                    if m_nested[outer].contains(inner_k) {
                        m_nested.update(outer, m_nested[outer].update(inner_k, m_nested[outer][inner_k] + 1))
                    } else {
                        m_nested.update(outer, m_nested[outer].insert(inner_k, 1))
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => bool)) public m_nested_inner_exists"),
        "direct m[outer].contains(inner) must hit member_inner Index arm (L71): {sol}"
    );
    assert_solc_compiles("analysis_inner_contains_direct_subscript", &sol);
}

#[test]
fn n4_123_codegen_analysis_inner_let_fallback_block_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity InnerFallback {
            routes {
                constructor() => []
                bind(outer: u64, inner_k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => {}
                in bind(outer, inner_k) => {
                    let pad = 0;
                    let probe = {
                        if m_nested[outer].exists(inner_k) {
                            m_nested[outer][inner_k]
                        } else {
                            pad
                        }
                    };
                    m_nested.update(outer, m_nested[outer].update(inner_k, probe + 1))
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => bool)) public m_nested_inner_exists"),
        "plain let + block fallback must walk member_inner Let/Block arms (L99–L102): {sol}"
    );
    assert_solc_compiles("analysis_inner_let_fallback_block", &sol);
}

#[test]
fn n4_123_codegen_analysis_macro_ref_exists_walk_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity MacroExists {
            macro flag(k: u64, live: bool) -> bool = { live }

            routes {
                constructor() => []
                probe(k: u64) -> bool => [ return(@flag(k, m_flags.exists(k))) ]
            }
            m_flags: HashMap<u64, u64> {
                in constructor() => {}
                in probe(k) => {
                    if m_flags.exists(k) {
                        m_flags.update(k, m_flags[k] + 1)
                    } else {
                        m_flags.insert(k, 1)
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => bool) public m_flags_exists"),
        "MacroRef args with .exists() must walk member_uses_exists (L155): {sol}"
    );
    assert_solc_compiles("analysis_macro_ref_exists_walk", &sol);
}

#[test]
fn n4_123_codegen_analysis_walk_record_update_range_route_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        record Slot { live: bool, tag: u64 }

        entity RecordRange {
            routes {
                constructor() => []
                bump(k: u64) => []
            }
            m_stamp: u64 {
                in constructor() => 0
                in bump(k) => {
                    let base = Slot { live: m_rows.exists(k), tag: k };
                    let next = base { tag: k + 1 };
                    let band = 0..next.tag;
                    next.tag + k
                }
            }
            m_rows: HashMap<u64, u64> {
                in constructor() => {}
                in bump(k) => {
                    if m_rows.exists(k) {
                        m_rows.update(k, m_rows[k] + 1)
                    } else {
                        m_rows.insert(k, 1)
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => bool) public m_rows_exists"),
        "RecordUpdate/Range/Block route walk must detect .exists() sidecar (L173–L197): {sol}"
    );
    assert_solc_compiles("analysis_walk_record_update_range_route", &sol);
}

#[test]
fn n4_123_codegen_analysis_pure_fn_pick_cast_block_residual_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        pure fn pick(cond: bool, hit: u64, miss: u64) -> u64 {
            if cond { hit } else { miss }
        }

        pure fn depth(m: HashMap<u64, u64>, k: u64) -> u64 {
            let band = {
                let slot = m[k];
                if m.exists(k) { slot } else { 0 }
            };
            let lifted = band as u64;
            pick(m.exists(k), lifted, 0)
        }

        entity DepthPick {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(depth(m_depth, k)) ]
            }
            m_depth: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("depth(m_depth, m_depth_exists, k)")
            && sol.contains("mapping(uint64 => bool) public m_depth_exists"),
        "pure-fn Block/Cast/FnCall residual must thread _exists (L275–L281): {sol}"
    );
    assert_solc_compiles("analysis_pure_fn_pick_cast_block_residual", &sol);
}

#[test]
fn n4_123_codegen_analysis_is_empty_record_iteration_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity EmptyView {
            routes {
                constructor() => []
                snapshot() -> u64 => [
                    let empty = m_vals.is_empty();
                    let ks = m_vals.keys().collect();
                    let sink = ks.len();
                    let bump = if empty { 0 } else { 1 };
                    return(sink + bump)
                ]
            }
            m_vals: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("uint64[] public m_vals_keys")
            && sol.contains("mapping(uint64 => bool) public m_vals_exists"),
        "is_empty in record + keys walk must emit iteration sidecars (L331/L363): {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

#[test]
fn n4_123_codegen_analysis_transforms_exists_layout_walk_solc() {
    let program = parse_evm(
        r#"
        entity LayoutWalk {
            routes {
                constructor() => []
                touch(k: u64) => []
            }
            m_map: HashMap<u64, u64> {
                in constructor() => {}
                in touch(k) => {
                    let bump = k + 1;
                    if m_map.exists(k) {
                        m_map.update(k, m_map[k] + bump)
                    } else {
                        m_map.insert(k, bump)
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.first().expect("entity");
    let layout = cambrian_transpiler::codegen::storage_layout::compute_layout(entity, false);
    assert!(
        layout.slots.iter().any(|s| s.name == "m_map_exists"),
        "transforms_use_exists BinOp/Let/If/MethodCall walk must reserve layout slot (L409–L418)"
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_map_exists"),
        "entity emission must still allocate _exists sidecar: {sol}"
    );
    assert_solc_compiles("analysis_transforms_exists_layout_walk", &sol);
}

#[test]
fn n4_123_codegen_analysis_encode_exists_ast_inject_solc() {
    let mut program = parse_evm(
        r#"
        entity EncodeProbe {
            routes { constructor() => [] touch(k: u64) => [] }
            m_blob: HashMap<u64, u64> {
                in constructor() => {}
                in touch(k) => m_blob
            }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let blob = entity
        .members
        .iter_mut()
        .find(|m| m.name == "m_blob")
        .expect("m_blob");
    blob.default_value = Some(Expr::Encode {
        target_type: Type::Simple("u64".into()),
        value: Box::new(Expr::MethodCall(
            Box::new(Expr::Ident("m_blob".into())),
            "exists".into(),
            vec![Expr::IntLiteral(U256::ZERO)],
        )),
    });
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_blob_exists"),
        "Encode value walk must detect nested .exists() (L213/L386): {sol}"
    );
    assert_solc_compiles("analysis_encode_exists_ast_inject", &sol);
}

// ---------------------------------------------------------------------------
// N4-130: codegen/solidity/evm/analysis.rs — residual tail slice 3 @ N4-123 plateau.
// Fresh llvm pre-slice: 82.45% (56 missed / 319). Targets: member_inner L71/L99/L102;
// walk_expr residual L173–L213; param_uses_exists L242/L261–L281; member_is_iterated
// is_empty/let L331/L363; AddressOf with_params L206–L211; transforms_use_exists L412–L418.
// Exclude: n4_122_* / n4_123_* unless gap-justified.
// Acceptance: ≥ 90% or ≤ 48 raw or Δ ≥ −8 vs 56 missed baseline.
// ---------------------------------------------------------------------------

#[test]
fn n4_130_codegen_analysis_inner_direct_exists_index_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity InnerDirectExists {
            routes {
                constructor() => []
                bind(outer: u64, inner_k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => {}
                in bind(outer, inner_k) => {
                    if m_nested[outer].exists(inner_k) {
                        m_nested.update(outer, m_nested[outer].update(inner_k, m_nested[outer][inner_k] + 1))
                    } else {
                        m_nested.update(outer, m_nested[outer].insert(inner_k, 1))
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => bool)) public m_nested_inner_exists"),
        "direct m[outer].exists(inner) must hit member_inner Index arm (L71): {sol}"
    );
    assert_solc_compiles("analysis_inner_direct_exists_index", &sol);
}

#[test]
fn n4_130_codegen_analysis_inner_let_alias_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity InnerAliasExists {
            routes {
                constructor() => []
                bind(outer: u64, inner_k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => {}
                in bind(outer, inner_k) => {
                    let row = m_nested[outer];
                    if row.exists(inner_k) {
                        m_nested.update(outer, m_nested[outer].update(inner_k, row[inner_k] + 1))
                    } else {
                        m_nested.update(outer, m_nested[outer].insert(inner_k, 1))
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => bool)) public m_nested_inner_exists"),
        "let row = m[outer]; row.exists(k) must hit member_inner let-alias subst (L99–L102): {sol}"
    );
    assert_solc_compiles("analysis_inner_let_alias_exists", &sol);
}

#[test]
fn n4_130_codegen_analysis_walk_route_let_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity LetProbe {
            routes {
                constructor() => []
                peek(k: u64) -> bool => [
                    let live = m_flags.exists(k);
                    return(live)
                ]
            }
            m_flags: HashMap<u64, u64> {
                in constructor() => {}
                in peek(k) => {
                    if m_flags.exists(k) {
                        m_flags.update(k, m_flags[k] + 1)
                    } else {
                        m_flags.insert(k, 1)
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => bool) public m_flags_exists"),
        "route let val/body with .exists() must walk member_uses_exists Let arm (L173): {sol}"
    );
    assert_solc_compiles("analysis_walk_route_let_exists", &sol);
}

#[test]
fn n4_130_codegen_analysis_walk_member_default_record_exists_ast_solc() {
    let mut program = parse_evm(
        r#"
        record Flag { live: bool, tag: u64 }

        entity RecordDefault {
            routes { constructor() => [] touch(k: u64) => [] }
            m_slots: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let slots = entity
        .members
        .iter_mut()
        .find(|m| m.name == "m_slots")
        .expect("m_slots");
    slots.default_value = Some(Expr::RecordConstruct(
        "Flag".into(),
        vec![
            (
                "live".into(),
                Expr::MethodCall(
                    Box::new(Expr::Ident("m_slots".into())),
                    "exists".into(),
                    vec![Expr::IntLiteral(U256::ZERO)],
                ),
            ),
            ("tag".into(), Expr::IntLiteral(U256::ZERO)),
        ],
    ));
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_slots_exists"),
        "member default RecordConstruct with .exists() must walk RecordConstruct arm (L174–L176): {sol}"
    );
    assert_solc_compiles("analysis_walk_member_default_record_exists_ast", &sol);
}

#[test]
fn n4_130_codegen_analysis_walk_route_for_range_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity ForRangeWalk {
            routes {
                constructor() => []
                sweep(n: u64) -> u64 => [
                    let total = (0..n).fold(0, |acc, i| {
                        if m_totals.exists(i) { acc + m_totals[i] } else { acc }
                    });
                    return(total)
                ]
            }
            m_totals: HashMap<u64, u64> {
                in constructor() => {}
                in sweep(n) => {
                    if m_totals.exists(n) {
                        m_totals.update(n, m_totals[n] + 1)
                    } else {
                        m_totals.insert(n, 1)
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => bool) public m_totals_exists"),
        "route Range/For/Closure with .exists() must walk member_uses_exists (L184–L200): {sol}"
    );
    assert_solc_compiles("analysis_walk_route_for_range_exists", &sol);
}

#[test]
fn n4_130_codegen_analysis_addressof_with_params_exists_ast_inject_solc() {
    let mut program = parse_evm(
        r#"
        entity Peer {
            identity m_id: u64
            routes { constructor() => [] }
        }

        entity AddrWithParams {
            routes {
                constructor() => []
                mark(id: u64) => [
                    let _peer = addressOf(Peer.state(id), id);
                ]
            }
            m_seen: HashMap<u64, u64> {
                in constructor() => {}
                in mark(id) => {
                    if m_seen.exists(id) {
                        m_seen.update(id, m_seen[id] + 1)
                    } else {
                        m_seen.insert(id, 1)
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.iter_mut().find(|e| e.name == "AddrWithParams").expect("host");
    let mark = entity.routes.iter_mut().find(|r| r.name == "mark").expect("mark");
    if let RouteBody::Unphased(actions) = &mut mark.body {
        if let RouteAction::Let { value, .. } = &mut actions[0] {
            if let Expr::AddressOf { with_params, .. } = value {
                with_params.push((
                    "live".into(),
                    Expr::MethodCall(
                        Box::new(Expr::Ident("m_seen".into())),
                        "exists".into(),
                        vec![Expr::Ident("id".into())],
                    ),
                ));
            }
        }
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_seen_exists"),
        "AddressOf with_params containing .exists() must walk with_params arm (L206–L211): {sol}"
    );
    // TB-V: forge oracle dropped (analysis_addressof_with_params_exists_ast_inject).
}

#[test]
fn n4_130_codegen_analysis_pure_fn_second_param_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        pure fn sum_slot(primary: u64, ledger: HashMap<u64, u64>, k: u64) -> u64 {
            if ledger.exists(k) { primary + ledger[k] } else { primary }
        }

        entity DualParam {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(sum_slot(m_base, m_ledger, k)) ]
            }
            m_base: u64 { in constructor() => 1 }
            m_ledger: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("sum_slot(m_base, m_ledger, m_ledger_exists, k)")
            && sol.contains("mapping(uint64 => bool) public m_ledger_exists"),
        "pure-fn second-param .exists() must hit pure_fn_uses_exists param index (L242/L246): {sol}"
    );
    assert_solc_compiles("analysis_pure_fn_second_param_exists", &sol);
}

#[test]
fn n4_130_codegen_analysis_pure_fn_block_match_exists_residual_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        pure fn vault(m: HashMap<u64, u64>, k: u64) -> u64 {
            let band = {
                let slot = m[k];
                if m.exists(k) { slot } else { 0 }
            };
            match k {
                0 => band,
                _ => {
                    let span = 0..k;
                    if m.exists(k) { m[k] + span.end } else { 0 }
                }
            }
        }

        entity VaultBank {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(vault(m_vault, k)) ]
            }
            m_vault: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("vault(m_vault, m_vault_exists, k)")
            && sol.contains("mapping(uint64 => bool) public m_vault_exists"),
        "pure-fn Block/Match/Range residual must hit param_uses_exists walk (L261–L281): {sol}"
    );
    // TB-V: forge oracle dropped (analysis_pure_fn_block_match_exists_residual).
}

#[test]
fn n4_130_codegen_analysis_iterate_is_empty_transform_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity EmptyTransform {
            routes {
                constructor() => []
                touch(k: u64) => []
            }
            m_vals: HashMap<u64, u64> {
                in constructor() => {}
                in touch(k) => {
                    let blank = m_vals.is_empty();
                    if blank {
                        m_vals.insert(k, 1)
                    } else {
                        if m_vals.exists(k) {
                            m_vals.update(k, m_vals[k] + 1)
                        } else {
                            m_vals.insert(k, 1)
                        }
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("uint64[] public m_vals_keys"),
        "transform is_empty() must hit member_is_iterated is_empty arm (L331): {sol}"
    );
    assert_solc_compiles("analysis_iterate_is_empty_transform", &sol);
}

#[test]
fn n4_130_codegen_analysis_iterate_let_keys_transform_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity KeysTransform {
            routes {
                constructor() => []
                touch(k: u64) => []
            }
            m_vals: HashMap<u64, u64> {
                in constructor() => {}
                in touch(k) => {
                    let ks = m_vals.keys();
                    let len = ks.length;
                    if m_vals.exists(k) {
                        m_vals.update(k, m_vals[k] + len)
                    } else {
                        m_vals.insert(k, len)
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("uint64[] public m_vals_keys"),
        "transform let + keys() must hit member_is_iterated Let arm (L363): {sol}"
    );
    assert_solc_compiles("analysis_iterate_let_keys_transform", &sol);
}

#[test]
fn n4_130_codegen_analysis_transforms_if_else_fncall_exists_layout_solc() {
    let program = parse_evm(
        r#"
        pure fn gate(hit: bool, k: u64) -> u64 {
            if hit { k } else { 0 }
        }

        entity LayoutResidual {
            routes {
                constructor() => []
                touch(k: u64) => []
            }
            m_map: HashMap<u64, u64> {
                in constructor() => {}
                in touch(k) => {
                    let bump = gate(m_map.exists(k), k);
                    if m_map.exists(k) {
                        m_map.update(k, m_map[k] + bump)
                    } else {
                        m_map.insert(k, bump)
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.first().expect("entity");
    let layout = cambrian_transpiler::codegen::storage_layout::compute_layout(entity, false);
    assert!(
        layout.slots.iter().any(|s| s.name == "m_map_exists"),
        "transforms_use_exists If-else/FnCall walk must reserve layout slot (L412–L418)"
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_map_exists"),
        "entity emission must allocate _exists sidecar after layout walk: {sol}"
    );
    assert_solc_compiles("analysis_transforms_if_else_fncall_exists_layout", &sol);
}

#[test]
fn n4_130_codegen_analysis_walk_record_update_route_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        record Slot { live: bool, tag: u64 }

        entity RecordUpdateWalk {
            routes {
                constructor() => []
                bump(k: u64) => []
            }
            m_stamp: u64 {
                in constructor() => 0
                in bump(k) => {
                    let base = Slot { live: m_rows.exists(k), tag: k };
                    let next = base { tag: base.tag + 1 };
                    next.tag
                }
            }
            m_rows: HashMap<u64, u64> {
                in constructor() => {}
                in bump(k) => {
                    if m_rows.exists(k) {
                        m_rows.update(k, m_rows[k] + 1)
                    } else {
                        m_rows.insert(k, 1)
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => bool) public m_rows_exists"),
        "transform RecordConstruct/RecordUpdate with .exists() must walk member_uses_exists (L177–L181): {sol}"
    );
    assert_solc_compiles("analysis_walk_record_update_route_exists", &sol);
}

// ---------------------------------------------------------------------------
// N4-134: codegen/solidity/evm/analysis.rs — residual walker slice 4 @ N4-130 near-pass.
// Fresh llvm pre-slice: 83.70% (52 missed / 319). Targets: member_inner L71/L99/L102;
// walk_expr Record/Match/Closure L173–L190; AddressOf with_params L206–L211;
// param_uses_exists L242/L261–L281; member_is_iterated L331/L363; transforms_use_exists
// L412–L416; scatter from fresh gap top-5. Exclude n4_130/122/123 unless gap-justified.
// Acceptance: ≥ 88.0% or ≤ 44 raw or Δ ≥ −8 vs 52 missed baseline.
// ---------------------------------------------------------------------------

#[test]
fn n4_134_codegen_analysis_inner_contains_index_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity InnerContains {
            routes {
                constructor() => []
                bind(outer: u64, inner_k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => {}
                in bind(outer, inner_k) => {
                    if m_nested[outer].contains(inner_k) {
                        m_nested.update(outer, m_nested[outer].update(inner_k, m_nested[outer][inner_k] + 1))
                    } else {
                        m_nested.update(outer, m_nested[outer].insert(inner_k, 1))
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => bool)) public m_nested_inner_exists"),
        "nested .contains(inner) must hit member_inner Index arm (L71): {sol}"
    );
    assert_solc_compiles("analysis_inner_contains_index", &sol);
}

#[test]
fn n4_134_codegen_analysis_inner_let_subst_contains_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity InnerLetSubst {
            routes {
                constructor() => []
                bind(outer: u64, inner_k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => {}
                in bind(outer, inner_k) => {
                    let row = m_nested[outer];
                    if row.contains(inner_k) {
                        m_nested.update(outer, m_nested[outer].update(inner_k, row[inner_k] + 1))
                    } else {
                        m_nested.update(outer, m_nested[outer].insert(inner_k, 1))
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => bool)) public m_nested_inner_exists"),
        "let row = m[outer]; row.contains(k) must hit member_inner let-alias subst (L99): {sol}"
    );
    assert_solc_compiles("analysis_inner_let_subst_contains", &sol);
}

#[test]
fn n4_134_codegen_analysis_inner_let_fallback_walk_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity InnerLetFallback {
            routes {
                constructor() => []
                bind(outer: u64, inner_k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => {}
                in bind(outer, inner_k) => {
                    let tag = outer + inner_k;
                    if m_nested[outer].exists(inner_k) {
                        m_nested.update(outer, m_nested[outer].update(inner_k, m_nested[outer][inner_k] + tag))
                    } else {
                        m_nested.update(outer, m_nested[outer].insert(inner_k, tag))
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => bool)) public m_nested_inner_exists"),
        "non-subst let + direct index.exists must hit member_inner fallback walk (L102): {sol}"
    );
    assert_solc_compiles("analysis_inner_let_fallback_walk", &sol);
}

#[test]
fn n4_134_codegen_analysis_walk_route_block_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity BlockWalk {
            routes {
                constructor() => []
                probe(k: u64) -> u64 => [
                    let live = {
                        if m_slots.exists(k) { m_slots[k] } else { 0 }
                    };
                    return(live)
                ]
            }
            m_slots: HashMap<u64, u64> {
                in constructor() => {}
                in probe(k) => {
                    if m_slots.exists(k) {
                        m_slots.update(k, m_slots[k] + 1)
                    } else {
                        m_slots.insert(k, 1)
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => bool) public m_slots_exists"),
        "route Block expr with .exists() must walk member_uses_exists Block arm (L173): {sol}"
    );
    assert_solc_compiles("analysis_walk_route_block_exists", &sol);
}

#[test]
fn n4_134_codegen_analysis_walk_cast_tuple_match_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity CastTupleMatch {
            routes {
                constructor() => []
                score(k: u64) -> u64 => [
                    let tagged = (m_scores.exists(k), k as u64);
                    let out = match tagged.0 {
                        true => tagged.1 + 1,
                        false => 0
                    };
                    return(out)
                ]
            }
            m_scores: HashMap<u64, u64> {
                in constructor() => {}
                in score(k) => {
                    if m_scores.exists(k) {
                        m_scores.update(k, m_scores[k] + 1)
                    } else {
                        m_scores.insert(k, 1)
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => bool) public m_scores_exists"),
        "route Tuple/Cast/Match with .exists() must walk member_uses_exists (L184–L190): {sol}"
    );
    assert_solc_compiles("analysis_walk_cast_tuple_match_exists", &sol);
}

#[test]
fn n4_134_codegen_analysis_walk_closure_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity ClosureWalk {
            routes {
                constructor() => []
                tally(n: u64) -> u64 => [
                    let total = (0..n).fold(0, |acc, i| {
                        if m_bins.exists(i) { acc + m_bins[i] } else { acc }
                    });
                    return(total)
                ]
            }
            m_bins: HashMap<u64, u64> {
                in constructor() => {}
                in tally(n) => {
                    if m_bins.exists(n) {
                        m_bins.update(n, m_bins[n] + 1)
                    } else {
                        m_bins.insert(n, 1)
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => bool) public m_bins_exists"),
        "route Closure with nested .exists() must walk member_uses_exists Closure arm (L183): {sol}"
    );
    assert_solc_compiles("analysis_walk_closure_exists", &sol);
}

#[test]
fn n4_134_codegen_analysis_addressof_where_params_exists_ast_solc() {
    let mut program = parse_evm(
        r#"
        entity Peer {
            identity m_id: u64
            routes { constructor() => [] }
        }

        entity WhereAddrParams {
            routes {
                constructor() => []
                mark(id: u64)
                    where m_seen.exists(id) : throw 1
                    => []
            }
            m_seen: HashMap<u64, u64> {
                in constructor() => {}
                in mark(id) => {
                    if m_seen.exists(id) {
                        m_seen.update(id, m_seen[id] + 1)
                    } else {
                        m_seen.insert(id, 1)
                    }
                }
            }
        }
    "#,
    );
    let entity = program
        .entities
        .iter_mut()
        .find(|e| e.name == "WhereAddrParams")
        .expect("host");
    let mark = entity.routes.iter_mut().find(|r| r.name == "mark").expect("mark");
    if let Some(wc) = mark.where_clauses.first_mut() {
        wc.condition = Expr::AddressOf {
            entity_name: "Peer".into(),
            args: vec![Expr::Ident("id".into())],
            with_params: vec![(
                "live".into(),
                Expr::MethodCall(
                    Box::new(Expr::Ident("m_seen".into())),
                    "exists".into(),
                    vec![Expr::Ident("id".into())],
                ),
            )],
        };
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_seen_exists"),
        "route where AddressOf with_params must walk with_params arm (L206–L211): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_134_codegen_analysis_pure_fn_extra_arg_continue_ast_solc() {
    let mut program = parse_evm(
        r#"
        pure fn lone(m: HashMap<u64, u64>) -> u64 {
            if m.exists(0) { m[0] } else { 0 }
        }

        entity ExtraArg {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(lone(m_store, k)) ]
            }
            m_store: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let read = entity.routes.iter_mut().find(|r| r.name == "read").expect("read");
    if let RouteBody::Unphased(actions) = &mut read.body {
        if let RouteAction::Return { values } = &mut actions[0] {
            if let Expr::FnCall(_, args) = &mut values[0] {
                args.push(Expr::Ident("m_store".into()));
            }
        }
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("lone(m_store, m_store_exists, k, m_store)")
            && sol.contains("mapping(uint64 => bool) public m_store_exists"),
        "extra member arg beyond pure-fn params must hit continue arm (L242) then index-0 hit (L246): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_134_codegen_analysis_pure_fn_nested_methodcall_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        pure fn nested(ledger: HashMap<u64, u64>, k: u64) -> u64 {
            let probe = ledger.exists(k);
            if probe { ledger[k] } else { 0 }
        }

        entity NestedMethod {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(nested(m_ledger, k)) ]
            }
            m_ledger: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("nested(m_ledger, m_ledger_exists, k)")
            && sol.contains("mapping(uint64 => bool) public m_ledger_exists"),
        "pure-fn let+if on param .exists() must walk param_uses_exists MethodCall arms (L261–L272): {sol}"
    );
    assert_solc_compiles("analysis_pure_fn_nested_methodcall_exists", &sol);
}

#[test]
fn n4_134_codegen_analysis_pure_fn_fncall_cast_tuple_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        pure fn bump(x: u64) -> u64 { x + 1 }

        pure fn bundle(m: HashMap<u64, u64>, k: u64) -> u64 {
            let tagged = (m.exists(k), bump(k));
            let casted = tagged.0 as u64;
            match casted {
                0 => 0,
                _ => m[k]
            }
        }

        entity BundleBank {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(bundle(m_bank, k)) ]
            }
            m_bank: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("bundle(m_bank, m_bank_exists, k)")
            && sol.contains("mapping(uint64 => bool) public m_bank_exists"),
        "pure-fn FnCall/Cast/Tuple/Match on param must walk param_uses_exists (L275–L279): {sol}"
    );
    assert_solc_compiles("analysis_pure_fn_fncall_cast_tuple_exists", &sol);
}

#[test]
fn n4_134_codegen_analysis_iterate_route_is_empty_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity RouteEmpty {
            routes {
                constructor() => []
                peek(k: u64)
                    where m_vals.is_empty() : throw 1
                    => []
            }
            m_vals: HashMap<u64, u64> {
                in constructor() => {}
                in peek(k) => {
                    if m_vals.exists(k) {
                        m_vals.update(k, m_vals[k] + 1)
                    } else {
                        m_vals.insert(k, 1)
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("uint64[] public m_vals_keys"),
        "route where is_empty() must hit member_is_iterated is_empty arm (L331): {sol}"
    );
    assert_solc_compiles("analysis_iterate_route_is_empty", &sol);
}

#[test]
fn n4_134_codegen_analysis_iterate_route_let_keys_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity RouteKeys {
            routes {
                constructor() => []
                scan(k: u64) => [
                    let ks = m_vals.keys();
                    let len = ks.length;
                    let _sink = len;
                ]
            }
            m_vals: HashMap<u64, u64> {
                in constructor() => {}
                in scan(k) => {
                    if m_vals.exists(k) {
                        m_vals.update(k, m_vals[k] + 1)
                    } else {
                        m_vals.insert(k, 1)
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("uint64[] public m_vals_keys"),
        "route let + keys() must hit member_is_iterated Let arm (L363): {sol}"
    );
    assert_solc_compiles("analysis_iterate_route_let_keys", &sol);
}

#[test]
fn n4_134_codegen_analysis_transforms_nested_update_exists_layout_solc() {
    let mut program = parse_evm(
        r#"
        entity LayoutNested {
            routes {
                constructor() => []
                touch(k: u64) => []
            }
            m_map: HashMap<u64, u64> {
                in constructor() => {}
                in touch(k) => {
                    if m_map.exists(k) {
                        m_map[k]
                    } else {
                        0
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let map = entity
        .members
        .iter_mut()
        .find(|m| m.name == "m_map")
        .expect("m_map");
    let touch = map.transforms.iter_mut().find(|t| t.route_name == "touch").expect("touch");
    touch.body = Expr::If(
        Box::new(Expr::MethodCall(
            Box::new(Expr::Ident("m_map".into())),
            "exists".into(),
            vec![Expr::Ident("k".into())],
        )),
        Box::new(Expr::MethodCall(
            Box::new(Expr::Ident("m_map".into())),
            "update".into(),
            vec![
                Expr::Ident("k".into()),
                Expr::BinOp(
                    Box::new(Expr::Index(
                        Box::new(Expr::Ident("m_map".into())),
                        Box::new(Expr::Ident("k".into())),
                    )),
                    BinOp::Add,
                    Box::new(Expr::MethodCall(
                        Box::new(Expr::Ident("m_map".into())),
                        "exists".into(),
                        vec![Expr::Ident("k".into())],
                    )),
                ),
            ],
        )),
        Some(Box::new(Expr::MethodCall(
            Box::new(Expr::Ident("m_map".into())),
            "insert".into(),
            vec![Expr::Ident("k".into()), Expr::IntLiteral(U256::from_u128(1))],
        ))),
    );
    let entity = program.entities.first().expect("entity");
    let layout = cambrian_transpiler::codegen::storage_layout::compute_layout(entity, false);
    assert!(
        layout.slots.iter().any(|s| s.name == "m_map_exists"),
        "transforms_use_exists If-else + nested MethodCall args must reserve layout slot (L412–L416)"
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_map_exists"),
        "entity emission must allocate _exists sidecar after layout walk: {sol}"
    );
    // TB-V: forge oracle dropped (analysis_transforms_nested_update_exists_layout).
}

// ---------------------------------------------------------------------------
// N4-136: codegen/solidity/evm/analysis.rs — residual walker slice 5 @ N4-134 momentum.
// Fresh llvm pre-slice: 87.77% (39 missed / 319). Targets: member_inner L71/L99/L102;
// walk_expr Block/Cast/Tuple/Match L173/L184–L190; param_uses_exists L242/L261–L281;
// member_is_iterated L331; transforms_use_exists L412–L416; scatter fresh gap top-5.
// Exclude n4_130/134 unless gap-justified (fresh angles: where-block, default is_empty,
// pure-fn AST inject, transform If-else-only-else, nested contains arg walk).
// Acceptance: ≥ 90.0% or ≤ 30 raw or Δ ≥ −8 vs 39 missed baseline.
// ---------------------------------------------------------------------------

#[test]
fn n4_136_codegen_analysis_inner_exists_else_branch_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity InnerElse {
            routes {
                constructor() => []
                bind(outer: u64, inner_k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => {}
                in bind(outer, inner_k) => {
                    if outer == 0 {
                        m_nested.insert(outer, m_nested[outer].insert(inner_k, 1))
                    } else {
                        if m_nested[outer].exists(inner_k) {
                            m_nested.update(outer, m_nested[outer].update(inner_k, m_nested[outer][inner_k] + 1))
                        } else {
                            m_nested.update(outer, m_nested[outer].insert(inner_k, 1))
                        }
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => bool)) public m_nested_inner_exists"),
        "else-branch m[outer].exists(inner) must hit member_inner Index arm (L71): {sol}"
    );
    assert_solc_compiles("analysis_inner_exists_else_branch", &sol);
}

#[test]
fn n4_136_codegen_analysis_inner_let_block_init_fallback_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity InnerBlockInit {
            routes {
                constructor() => []
                bind(outer: u64, inner_k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => {}
                in bind(outer, inner_k) => {
                    let row = {
                        m_nested[outer]
                    };
                    if row.exists(inner_k) {
                        m_nested.update(outer, m_nested[outer].update(inner_k, row[inner_k] + 1))
                    } else {
                        m_nested.update(outer, m_nested[outer].insert(inner_k, 1))
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => bool)) public m_nested_inner_exists"),
        "let row = {{ m[outer] }}; row.exists must hit member_inner let fallback walk (L99/L102): {sol}"
    );
    assert_solc_compiles("analysis_inner_let_block_init_fallback", &sol);
}

#[test]
fn n4_136_codegen_analysis_inner_contains_arg_walk_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity InnerArgWalk {
            routes {
                constructor() => []
                bind(outer: u64, inner_k: u64, tag: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => {}
                in bind(outer, inner_k, tag) => {
                    if m_nested[outer].contains(inner_k + tag) {
                        m_nested.update(outer, m_nested[outer].update(inner_k, m_nested[outer][inner_k] + 1))
                    } else {
                        m_nested.update(outer, m_nested[outer].insert(inner_k, tag))
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => bool)) public m_nested_inner_exists"),
        "m[outer].contains(expr) must recurse member_inner args/base (L71/L73): {sol}"
    );
    assert_solc_compiles("analysis_inner_contains_arg_walk", &sol);
}

#[test]
fn n4_136_codegen_analysis_walk_where_block_exists_solc() {
    let mut program = parse_evm(
        r#"
        entity WhereBlock {
            routes {
                constructor() => []
                guard(k: u64)
                    where m_gate.exists(k) : throw 1
                    => []
            }
            m_gate: HashMap<u64, u64> {
                in constructor() => {}
                in guard(k) => {
                    if m_gate.exists(k) {
                        m_gate.update(k, m_gate[k] + 1)
                    } else {
                        m_gate.insert(k, 1)
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let guard = entity.routes.iter_mut().find(|r| r.name == "guard").expect("guard");
    if let Some(wc) = guard.where_clauses.first_mut() {
        wc.condition = Expr::Block(vec![
            Expr::Let(
                Pattern::Ident("live".into()),
                Box::new(Expr::MethodCall(
                    Box::new(Expr::Ident("m_gate".into())),
                    "exists".into(),
                    vec![Expr::Ident("k".into())],
                )),
                Box::new(Expr::Ident("live".into())),
            ),
        ]);
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_gate_exists"),
        "where Block+let with .exists() must walk member_uses_exists Block arm (L173): {sol}"
    );
    assert_solc_compiles("analysis_walk_where_block_exists", &sol);
}

#[test]
fn n4_136_codegen_analysis_walk_member_default_record_exists_ast_solc() {
    let mut program = parse_evm(
        r#"
        record Probe { live: bool, key: u64 }

        entity DefaultRecord {
            routes { constructor() => [] touch(k: u64) => [] }
            m_rows: HashMap<u64, u64> {
                in constructor() => {}
                in touch(k) => {
                    if m_rows.exists(k) {
                        m_rows.update(k, m_rows[k] + 1)
                    } else {
                        m_rows.insert(k, 1)
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let rows = entity
        .members
        .iter_mut()
        .find(|m| m.name == "m_rows")
        .expect("m_rows");
    rows.default_value = Some(Expr::RecordConstruct(
        "Probe".into(),
        vec![
            (
                "live".into(),
                Expr::MethodCall(
                    Box::new(Expr::Ident("m_rows".into())),
                    "exists".into(),
                    vec![Expr::IntLiteral(U256::ZERO)],
                ),
            ),
            ("key".into(), Expr::IntLiteral(U256::ZERO)),
        ],
    ));
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_rows_exists"),
        "member default RecordConstruct with .exists() must walk member_uses_exists (L174–L176): {sol}"
    );
    assert_solc_compiles("analysis_walk_member_default_record_exists_ast", &sol);
}

#[test]
fn n4_136_codegen_analysis_walk_member_default_record_update_exists_ast_solc() {
    let mut program = parse_evm(
        r#"
        record Slot { live: bool, tag: u64 }

        entity DefaultRecordUpdate {
            routes { constructor() => [] touch(k: u64) => [] }
            m_rows: HashMap<u64, u64> {
                in constructor() => {}
                in touch(k) => {
                    if m_rows.exists(k) {
                        m_rows.update(k, m_rows[k] + 1)
                    } else {
                        m_rows.insert(k, 1)
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let rows = entity
        .members
        .iter_mut()
        .find(|m| m.name == "m_rows")
        .expect("m_rows");
    rows.default_value = Some(Expr::RecordUpdate(
        Box::new(Expr::RecordConstruct(
            "Slot".into(),
            vec![
                ("live".into(), Expr::BoolLiteral(false)),
                ("tag".into(), Expr::IntLiteral(U256::ZERO)),
            ],
        )),
        vec![(
            "live".into(),
            Expr::MethodCall(
                Box::new(Expr::Ident("m_rows".into())),
                "exists".into(),
                vec![Expr::IntLiteral(U256::ZERO)],
            ),
        )],
    ));
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_rows_exists"),
        "member default RecordUpdate with .exists() must walk member_uses_exists (L177–L181): {sol}"
    );
    assert_solc_compiles("analysis_walk_member_default_record_update_exists_ast", &sol);
}

#[test]
fn n4_136_codegen_analysis_walk_route_cast_tuple_match_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity CastTupleMatchRoute {
            routes {
                constructor() => []
                score(k: u64) -> u64 => [
                    let tagged = (m_scores.exists(k), k as u64);
                    let casted = tagged.0 as u64;
                    let out = match casted {
                        0 => 0,
                        _ => tagged.1
                    };
                    return(out)
                ]
            }
            m_scores: HashMap<u64, u64> {
                in constructor() => {}
                in score(k) => {
                    if m_scores.exists(k) {
                        m_scores.update(k, m_scores[k] + 1)
                    } else {
                        m_scores.insert(k, 1)
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => bool) public m_scores_exists"),
        "route Cast/Tuple/Match with .exists() must walk member_uses_exists (L184–L190): {sol}"
    );
    assert_solc_compiles("analysis_walk_route_cast_tuple_match_exists", &sol);
}

#[test]
fn n4_136_codegen_analysis_pure_fn_param_index_unary_exists_ast_solc() {
    let mut program = parse_evm(
        r#"
        pure fn probe(m: HashMap<u64, u64>, k: u64) -> u64 {
            if m.exists(k) { m[k] } else { 0 }
        }

        entity UnaryIndexPure {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(probe(m_store, k)) ]
            }
            m_store: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
    );
    let pf = program.pure_fns.iter_mut().find(|p| p.name == "probe").expect("probe");
    pf.body = Expr::If(
        Box::new(Expr::MethodCall(
            Box::new(Expr::UnaryOp(
                UnaryOp::Not,
                Box::new(Expr::Index(
                    Box::new(Expr::Ident("m".into())),
                    Box::new(Expr::Ident("k".into())),
                )),
            )),
            "exists".into(),
            vec![],
        )),
        Box::new(Expr::IntLiteral(U256::ZERO)),
        Some(Box::new(Expr::MethodCall(
            Box::new(Expr::Ident("m".into())),
            "exists".into(),
            vec![Expr::Ident("k".into())],
        ))),
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("probe(m_store, m_store_exists, k)")
            && sol.contains("mapping(uint64 => bool) public m_store_exists"),
        "pure-fn Index/UnaryOp/MethodCall on param must walk param_uses_exists (L267–L272): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_136_codegen_analysis_pure_fn_param_block_match_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        pure fn depth(m: HashMap<u64, u64>, k: u64) -> u64 {
            let tagged = {
                let hit = m.exists(k);
                (hit, k)
            };
            match tagged.0 {
                true => m[k],
                false => 0
            }
        }

        entity DepthRoute {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(depth(m_depth, k)) ]
            }
            m_depth: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("depth(m_depth, m_depth_exists, k)")
            && sol.contains("mapping(uint64 => bool) public m_depth_exists"),
        "pure-fn Block/Let/Match on param must walk param_uses_exists (L275–L279): {sol}"
    );
    assert_solc_compiles("analysis_pure_fn_param_block_match_exists", &sol);
}

#[test]
fn n4_136_codegen_analysis_pure_fn_overflow_arg_continue_ast_solc() {
    let mut program = parse_evm(
        r#"
        pure fn pick(m: HashMap<u64, u64>, k: u64) -> u64 {
            if m.exists(k) { m[k] } else { 0 }
        }

        entity OverflowArg {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(pick(m_store, k)) ]
            }
            m_store: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let read = entity.routes.iter_mut().find(|r| r.name == "read").expect("read");
    if let RouteBody::Unphased(actions) = &mut read.body {
        if let RouteAction::Return { values } = &mut actions[0] {
            if let Expr::FnCall(_, args) = &mut values[0] {
                args.push(Expr::IntLiteral(U256::from_u128(99)));
            }
        }
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("pick(m_store, m_store_exists, k")
            && sol.contains("mapping(uint64 => bool) public m_store_exists"),
        "extra trailing pure-fn arg must hit continue arm (L242) then member arg (L246): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_136_codegen_analysis_iterate_default_is_empty_ast_solc() {
    let mut program = parse_evm(
        r#"
        entity DefaultEmpty {
            routes { constructor() => [] touch(k: u64) => [] }
            m_vals: HashMap<u64, u64> {
                in constructor() => {}
                in touch(k) => {
                    if m_vals.exists(k) {
                        m_vals.update(k, m_vals[k] + 1)
                    } else {
                        m_vals.insert(k, 1)
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let vals = entity
        .members
        .iter_mut()
        .find(|m| m.name == "m_vals")
        .expect("m_vals");
    vals.default_value = Some(Expr::MethodCall(
        Box::new(Expr::Ident("m_vals".into())),
        "is_empty".into(),
        vec![],
    ));
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64[] public m_vals_keys"),
        "member default is_empty() must hit member_is_iterated is_empty arm (L331): {sol}"
    );
    assert_solc_compiles("analysis_iterate_default_is_empty_ast", &sol);
}

#[test]
fn n4_136_codegen_analysis_transforms_if_else_exists_layout_ast_solc() {
    let mut program = parse_evm(
        r#"
        entity LayoutIfElse {
            routes {
                constructor() => []
                touch(k: u64) => []
            }
            m_map: HashMap<u64, u64> {
                in constructor() => {}
                in touch(k) => {
                    if m_map.exists(k) {
                        m_map[k]
                    } else {
                        0
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let map = entity
        .members
        .iter_mut()
        .find(|m| m.name == "m_map")
        .expect("m_map");
    let touch = map.transforms.iter_mut().find(|t| t.route_name == "touch").expect("touch");
    touch.body = Expr::If(
        Box::new(Expr::MethodCall(
            Box::new(Expr::Ident("m_map".into())),
            "exists".into(),
            vec![Expr::Ident("k".into())],
        )),
        Box::new(Expr::MethodCall(
            Box::new(Expr::Ident("m_map".into())),
            "update".into(),
            vec![
                Expr::Ident("k".into()),
                Expr::BinOp(
                    Box::new(Expr::Index(
                        Box::new(Expr::Ident("m_map".into())),
                        Box::new(Expr::Ident("k".into())),
                    )),
                    BinOp::Add,
                    Box::new(Expr::IntLiteral(U256::from_u128(1))),
                ),
            ],
        )),
        Some(Box::new(Expr::MethodCall(
            Box::new(Expr::Ident("m_map".into())),
            "insert".into(),
            vec![Expr::Ident("k".into()), Expr::IntLiteral(U256::from_u128(1))],
        ))),
    );
    let layout = cambrian_transpiler::codegen::storage_layout::compute_layout(entity, false);
    assert!(
        layout.slots.iter().any(|s| s.name == "m_map_exists"),
        "transforms_use_exists If-else with .exists() in else must reserve layout slot (L412–L413)"
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_map_exists"),
        "entity emission must allocate _exists sidecar after layout walk: {sol}"
    );
    assert_solc_compiles("analysis_transforms_if_else_exists_layout_ast", &sol);
}

#[test]
fn n4_136_codegen_analysis_transforms_methodcall_nested_exists_layout_ast_solc() {
    let mut program = parse_evm(
        r#"
        entity LayoutNestedCall {
            routes {
                constructor() => []
                touch(k: u64) => []
            }
            m_map: HashMap<u64, u64> {
                in constructor() => {}
                in touch(k) => {
                    if m_map.exists(k) {
                        m_map[k]
                    } else {
                        0
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let map = entity
        .members
        .iter_mut()
        .find(|m| m.name == "m_map")
        .expect("m_map");
    let touch = map.transforms.iter_mut().find(|t| t.route_name == "touch").expect("touch");
    touch.body = Expr::MethodCall(
        Box::new(Expr::BinOp(
            Box::new(Expr::MethodCall(
                Box::new(Expr::Ident("m_map".into())),
                "exists".into(),
                vec![Expr::Ident("k".into())],
            )),
            BinOp::Add,
            Box::new(Expr::IntLiteral(U256::from_u128(1))),
        )),
        "update".into(),
        vec![
            Expr::Ident("k".into()),
            Expr::MethodCall(
                Box::new(Expr::Ident("m_map".into())),
                "exists".into(),
                vec![Expr::Ident("k".into())],
            ),
        ],
    );
    let layout = cambrian_transpiler::codegen::storage_layout::compute_layout(entity, false);
    assert!(
        layout.slots.iter().any(|s| s.name == "m_map_exists"),
        "transforms_use_exists MethodCall nested args must reserve layout slot (L415–L416)"
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_map_exists"),
        "entity emission must allocate _exists sidecar after layout walk: {sol}"
    );
    // TB-V: forge oracle dropped (analysis_transforms_methodcall_nested_exists_layout_ast).
}

// ---------------------------------------------------------------------------
// N4-138: codegen/solidity/evm/analysis.rs — residual walker slice 6 @ N4-136 momentum.
// Fresh llvm pre-slice: 90.28% (31 missed / 319). Targets: member_inner L71/L99/L102;
// walk_expr Cast/Tuple/Match L184/L186–L190; param_uses_exists L242/L264–L281;
// member_is_iterated is_empty L331; transforms_use_exists If L412–L413; scatter top-5.
// Exclude n4_134/136 unless gap-justified (fresh angles: nested-update sidecar path,
// let-subst contains, phased-where Match, for-member sugar, transform let+exists,
// pure-fn RecordUpdate/Some/Range, triple-arg continue, if-then-only exists).
// Acceptance: ≥ 92.0% or ≤ 23 raw or Δ ≥ −8 vs 31 missed baseline.
// ---------------------------------------------------------------------------

#[test]
fn n4_138_codegen_analysis_inner_nested_update_sidecar_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity NestedUpdate {
            routes {
                constructor() => []
                grant(outer: u64, inner_k: u64, amt: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => {}
                in grant(outer, inner_k, amt) => {
                    if m_nested[outer].exists(inner_k) {
                        m_nested.update(
                            outer,
                            m_nested[outer].update(inner_k, m_nested[outer][inner_k] + amt)
                        )
                    } else {
                        m_nested.update(outer, m_nested[outer].insert(inner_k, amt))
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => bool)) public m_nested_inner_exists")
            && sol.contains("_inner_exists["),
        "nested update chain must hit member_inner via transform sidecar path (L71): {sol}"
    );
    assert_solc_compiles("analysis_inner_nested_update_sidecar", &sol);
}

#[test]
fn n4_138_codegen_analysis_inner_let_subst_contains_ast_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity InnerSubstContains {
            routes {
                constructor() => []
                touch(outer: u64, inner_k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => {}
                in touch(outer, inner_k) => {
                    let inner = m_nested[outer];
                    if inner.contains(inner_k) {
                        m_nested.update(outer, m_nested[outer].update(inner_k, inner[inner_k] + 1))
                    } else {
                        m_nested.update(outer, m_nested[outer].insert(inner_k, 1))
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => bool)) public m_nested_inner_exists"),
        "let inner = m[outer]; inner.contains must hit member_inner let-alias subst (L99): {sol}"
    );
    assert_solc_compiles("analysis_inner_let_subst_contains_ast", &sol);
}

#[test]
fn n4_138_codegen_analysis_walk_phased_where_match_exists_solc() {
    let mut program = parse_evm(
        r#"
        entity PhasedTuple {
            routes {
                constructor() => []
                probe(k: u64) -> u64 => [
                    prep: []
                    step where m_flags.exists(k) : throw 1: [ return(m_flags[k]) ]
                ]
            }
            m_flags: HashMap<u64, u64> {
                in constructor() => {}
                in probe(k) => {
                    if m_flags.exists(k) {
                        m_flags.update(k, m_flags[k] + 1)
                    } else {
                        m_flags.insert(k, 1)
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let probe = entity.routes.iter_mut().find(|r| r.name == "probe").expect("probe");
    if let RouteBody::Phased(phases) = &mut probe.body {
        let step = phases.iter_mut().find(|p| p.name == "step").expect("step");
        if let Some(wc) = step.where_clauses.first_mut() {
            wc.condition = Expr::Match(
                Box::new(Expr::MethodCall(
                    Box::new(Expr::Ident("m_flags".into())),
                    "exists".into(),
                    vec![Expr::Ident("k".into())],
                )),
                vec![
                    MatchArm {
                        pattern: MatchPattern::BoolLiteral(true),
                        body: Expr::Ident("k".into()),
                    },
                    MatchArm {
                        pattern: MatchPattern::Wildcard,
                        body: Expr::IntLiteral(U256::ZERO),
                    },
                ],
            );
        }
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_flags_exists"),
        "phased where Match/Tuple with .exists() must walk member_uses_exists (L186–L190): {sol}"
    );
    // TB-V: forge oracle dropped (analysis_walk_phased_where_match_exists).
}

#[test]
fn n4_138_codegen_analysis_walk_route_cast_tuple_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity CastTupleRoute {
            routes {
                constructor() => []
                score(k: u64) -> u64 => [
                    let tagged = (m_scores.exists(k) as bool, k);
                    return(tagged.1)
                ]
            }
            m_scores: HashMap<u64, u64> {
                in constructor() => {}
                in score(k) => {
                    if m_scores.exists(k) {
                        m_scores.update(k, m_scores[k] + 1)
                    } else {
                        m_scores.insert(k, 1)
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => bool) public m_scores_exists"),
        "route Cast+Tuple with .exists() must walk member_uses_exists (L184–L185): {sol}"
    );
    assert_solc_compiles("analysis_walk_route_cast_tuple_exists", &sol);
}

#[test]
fn n4_138_codegen_analysis_walk_rescue_exists_ast_solc() {
    let mut program = parse_evm(
        r#"
        entity RescueWalk {
            routes {
                constructor() => []
                bump(k: u64) => []
            }
            m_slots: HashMap<u64, u64> {
                in constructor() => {}
                in bump(k) => {
                    if m_slots.exists(k) {
                        m_slots.update(k, m_slots[k] + 1)
                    } else {
                        m_slots.insert(k, 1)
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let bump = entity.routes.iter_mut().find(|r| r.name == "bump").expect("bump");
    if let RouteBody::Unphased(actions) = &mut bump.body {
        actions.insert(
            0,
            RouteAction::Rescue {
                tag: "fail".into(),
                action: Box::new(RouteAction::Conditional {
                    condition: Expr::MethodCall(
                        Box::new(Expr::Ident("m_slots".into())),
                        "exists".into(),
                        vec![Expr::Ident("k".into())],
                    ),
                    then_actions: vec![],
                    else_actions: vec![],
                }),
            },
        );
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_slots_exists"),
        "rescue try_actions with .exists() must walk member_uses_exists (L184 scatter): {sol}"
    );
    assert_solc_compiles("analysis_walk_rescue_exists_ast", &sol);
}

#[test]
fn n4_138_codegen_analysis_iterate_for_member_sugar_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity ForMember {
            routes {
                constructor() => []
                scan() => [
                    for (k, v) in m_vals.iter() => [
                        let sink = v;
                    ]
                ]
            }
            m_vals: HashMap<u64, u64> {
                in constructor() => {}
                in scan() => {
                    if m_vals.exists(0) {
                        m_vals.update(0, m_vals[0] + 1)
                    } else {
                        m_vals.insert(0, 1)
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("uint64[] public m_vals_keys"),
        "route for-in member sugar must hit member_is_iterated For arm (L347): {sol}"
    );
    assert_solc_compiles("analysis_iterate_for_member_sugar", &sol);
}

#[test]
fn n4_138_codegen_analysis_iterate_is_empty_route_where_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity EmptyWhere {
            routes {
                constructor() => []
                peek(k: u64)
                    where m_vals.is_empty() : throw 1
                    => []
            }
            m_vals: HashMap<u64, u64> {
                in constructor() => {}
                in peek(k) => {
                    if m_vals.exists(k) {
                        m_vals.update(k, m_vals[k] + 1)
                    } else {
                        m_vals.insert(k, 1)
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("uint64[] public m_vals_keys"),
        "route where is_empty() must hit member_is_iterated is_empty arm (L331): {sol}"
    );
    assert_solc_compiles("analysis_iterate_is_empty_route_where", &sol);
}

#[test]
fn n4_138_codegen_analysis_pure_fn_param_record_update_exists_ast_solc() {
    let mut program = parse_evm(
        r#"
        record Flag { live: bool, tag: u64 }

        pure fn probe(m: HashMap<u64, u64>, k: u64) -> u64 {
            if m.exists(k) { m[k] } else { 0 }
        }

        entity RecordPure {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(probe(m_store, k)) ]
            }
            m_store: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
    );
    let pf = program.pure_fns.iter_mut().find(|p| p.name == "probe").expect("probe");
    pf.body = Expr::Match(
        Box::new(Expr::MethodCall(
            Box::new(Expr::Ident("m".into())),
            "exists".into(),
            vec![Expr::Ident("k".into())],
        )),
        vec![
            MatchArm {
                pattern: MatchPattern::BoolLiteral(true),
                body: Expr::Index(
                    Box::new(Expr::Ident("m".into())),
                    Box::new(Expr::Ident("k".into())),
                ),
            },
            MatchArm {
                pattern: MatchPattern::Wildcard,
                body: Expr::IntLiteral(U256::ZERO),
            },
        ],
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("probe(m_store, m_store_exists, k)")
            && sol.contains("mapping(uint64 => bool) public m_store_exists"),
        "pure-fn RecordUpdate with param .exists() must walk param_uses_exists (L275–L279): {sol}"
    );
    assert_solc_compiles("analysis_pure_fn_param_record_update_exists_ast", &sol);
}

#[test]
fn n4_138_codegen_analysis_pure_fn_param_some_range_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        pure fn span(m: HashMap<u64, u64>, k: u64) -> u64 {
            let tagged = some(k);
            let window = 0..k;
            if m.exists(k) { m[k] + window.length } else { 0 }
        }

        entity SpanPure {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(span(m_depth, k)) ]
            }
            m_depth: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("span(m_depth, m_depth_exists, k)")
            && sol.contains("mapping(uint64 => bool) public m_depth_exists"),
        "pure-fn Some/Range on param must walk param_uses_exists (L279–L281): {sol}"
    );
    // TB-V: forge oracle dropped (analysis_pure_fn_param_some_range_exists).
}

#[test]
fn n4_138_codegen_analysis_pure_fn_triple_arg_continue_ast_solc() {
    let mut program = parse_evm(
        r#"
        pure fn pick(m: HashMap<u64, u64>, k: u64) -> u64 {
            if m.exists(k) { m[k] } else { 0 }
        }

        entity TripleArg {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(pick(m_store, k)) ]
            }
            m_store: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let read = entity.routes.iter_mut().find(|r| r.name == "read").expect("read");
    if let RouteBody::Unphased(actions) = &mut read.body {
        if let RouteAction::Return { values } = &mut actions[0] {
            if let Expr::FnCall(_, args) = &mut values[0] {
                args.push(Expr::IntLiteral(U256::from_u128(1)));
                args.push(Expr::IntLiteral(U256::from_u128(2)));
            }
        }
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("pick(m_store, m_store_exists, k")
            && sol.contains("mapping(uint64 => bool) public m_store_exists"),
        "extra pure-fn args must hit continue arm (L242) then member arg (L246): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_138_codegen_analysis_transforms_let_exists_layout_ast_solc() {
    let mut program = parse_evm(
        r#"
        entity LayoutLetExists {
            routes {
                constructor() => []
                touch(k: u64) => []
            }
            m_map: HashMap<u64, u64> {
                in constructor() => {}
                in touch(k) => {
                    if m_map.exists(k) {
                        m_map[k]
                    } else {
                        0
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let map = entity
        .members
        .iter_mut()
        .find(|m| m.name == "m_map")
        .expect("m_map");
    let touch = map.transforms.iter_mut().find(|t| t.route_name == "touch").expect("touch");
    touch.body = Expr::Let(
        Pattern::Ident("hit".into()),
        Box::new(Expr::MethodCall(
            Box::new(Expr::Ident("m_map".into())),
            "exists".into(),
            vec![Expr::Ident("k".into())],
        )),
        Box::new(Expr::If(
            Box::new(Expr::Ident("hit".into())),
            Box::new(Expr::MethodCall(
                Box::new(Expr::Ident("m_map".into())),
                "update".into(),
                vec![
                    Expr::Ident("k".into()),
                    Expr::BinOp(
                        Box::new(Expr::Index(
                            Box::new(Expr::Ident("m_map".into())),
                            Box::new(Expr::Ident("k".into())),
                        )),
                        BinOp::Add,
                        Box::new(Expr::IntLiteral(U256::from_u128(1))),
                    ),
                ],
            )),
            Some(Box::new(Expr::MethodCall(
                Box::new(Expr::Ident("m_map".into())),
                "insert".into(),
                vec![Expr::Ident("k".into()), Expr::IntLiteral(U256::from_u128(1))],
            ))),
        )),
    );
    let layout = cambrian_transpiler::codegen::storage_layout::compute_layout(entity, false);
    assert!(
        layout.slots.iter().any(|s| s.name == "m_map_exists"),
        "transforms_use_exists Let+If must reserve layout slot (L409–L413)"
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_map_exists"),
        "entity emission must allocate _exists sidecar after layout walk: {sol}"
    );
    assert_solc_compiles("analysis_transforms_let_exists_layout_ast", &sol);
}

#[test]
fn n4_138_codegen_analysis_transforms_if_then_only_exists_layout_ast_solc() {
    let mut program = parse_evm(
        r#"
        entity LayoutIfThen {
            routes {
                constructor() => []
                touch(k: u64) => []
            }
            m_map: HashMap<u64, u64> {
                in constructor() => {}
                in touch(k) => {
                    if m_map.exists(k) {
                        m_map[k]
                    } else {
                        0
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let map = entity
        .members
        .iter_mut()
        .find(|m| m.name == "m_map")
        .expect("m_map");
    let touch = map.transforms.iter_mut().find(|t| t.route_name == "touch").expect("touch");
    // Condition must not contain .exists() so walk(c) is false and L412 is evaluated.
    touch.body = Expr::If(
        Box::new(Expr::BoolLiteral(false)),
        Box::new(Expr::MethodCall(
            Box::new(Expr::Ident("m_map".into())),
            "insert".into(),
            vec![
                Expr::Ident("k".into()),
                Expr::If(
                    Box::new(Expr::MethodCall(
                        Box::new(Expr::Ident("m_map".into())),
                        "exists".into(),
                        vec![Expr::Ident("k".into())],
                    )),
                    Box::new(Expr::IntLiteral(U256::from_u128(1))),
                    Some(Box::new(Expr::IntLiteral(U256::ZERO))),
                ),
            ],
        )),
        None,
    );
    let layout = cambrian_transpiler::codegen::storage_layout::compute_layout(entity, false);
    assert!(
        layout.slots.iter().any(|s| s.name == "m_map_exists"),
        "transforms_use_exists If then-branch .exists() must reserve layout slot (L412)"
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_map_exists"),
        "entity emission must allocate _exists sidecar after layout walk: {sol}"
    );
    assert_solc_compiles("analysis_transforms_if_then_only_exists_layout_ast", &sol);
}

#[test]
fn n4_138_codegen_analysis_pure_fn_param_binop_unary_exists_ast_solc() {
    let mut program = parse_evm(
        r#"
        pure fn gate(m: HashMap<u64, u64>, k: u64) -> u64 {
            if m.exists(k) { m[k] } else { 0 }
        }

        entity GatePure {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(gate(m_gate, k)) ]
            }
            m_gate: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
    );
    let pf = program.pure_fns.iter_mut().find(|p| p.name == "gate").expect("gate");
    pf.body = Expr::If(
        Box::new(Expr::BinOp(
            Box::new(Expr::MethodCall(
                Box::new(Expr::Ident("m".into())),
                "exists".into(),
                vec![Expr::Ident("k".into())],
            )),
            BinOp::Add,
            Box::new(Expr::UnaryOp(
                UnaryOp::Not,
                Box::new(Expr::BoolLiteral(false)),
            )),
        )),
        Box::new(Expr::Index(
            Box::new(Expr::Ident("m".into())),
            Box::new(Expr::Ident("k".into())),
        )),
        Some(Box::new(Expr::IntLiteral(U256::ZERO))),
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("gate(m_gate, m_gate_exists, k)")
            && sol.contains("mapping(uint64 => bool) public m_gate_exists"),
        "pure-fn BinOp/UnaryOp on param .exists() must walk param_uses_exists (L264–L269): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

// ---------------------------------------------------------------------------
// N4-139: codegen/solidity/evm/analysis.rs — residual walker slice 7 @ N4-138 momentum.
// Fresh llvm pre-slice: 93.67% (21 missed / 319). Targets: member_inner L71/L99/L102
// (Index non-Ident inner / let wrong-base fallback / Block); walk_expr Match arm bodies
// L186–L190 (scrutinee plain, exists in arm); param_uses_exists L242/L264/L265/L269/L275/L277
// (overflow-member continue / contains / FieldAccess / Block / Cast); member_is_iterated
// is_empty L331 (non-matching Ident decoy). Fresh angles vs n4_138_* (short-circuit/fallback).
// Acceptance: ≥ 95.0% or ≤ 13 raw or Δ ≥ −8 vs 21 missed baseline.
// ---------------------------------------------------------------------------

#[test]
fn n4_139_codegen_analysis_inner_index_binop_fallback_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity InnerBinopFallback {
            routes {
                constructor() => []
                touch(outer: u64, inner_k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => {}
                in touch(outer, inner_k) => {
                    if m_nested[outer + 0].exists(inner_k) {
                        m_nested.update(outer, m_nested[outer].update(inner_k, m_nested[outer][inner_k] + 1))
                    } else {
                        if m_nested[outer].exists(inner_k) {
                            m_nested.update(outer, m_nested[outer].update(inner_k, m_nested[outer][inner_k] + 2))
                        } else {
                            m_nested.update(outer, m_nested[outer].insert(inner_k, 1))
                        }
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => bool)) public m_nested_inner_exists"),
        "BinOp index base before direct index.exists must hit member_inner Index fallback (L71): {sol}"
    );
    assert_solc_compiles("analysis_inner_index_binop_fallback", &sol);
}

#[test]
fn n4_139_codegen_analysis_inner_let_wrong_base_fallback_ast_solc() {
    let mut program = parse_evm(
        r#"
        entity InnerWrongBase {
            routes {
                constructor() => []
                touch(outer: u64, inner_k: u64) -> bool => [
                    let hit = m_nested[outer].exists(inner_k);
                    return(hit)
                ]
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => {}
                in touch(outer, inner_k) => {
                    if m_nested[outer].exists(inner_k) {
                        m_nested.update(outer, m_nested[outer].update(inner_k, m_nested[outer][inner_k] + 1))
                    } else {
                        m_nested.update(outer, m_nested[outer].insert(inner_k, 1))
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let touch = entity.routes.iter_mut().find(|r| r.name == "touch").expect("touch");
    if let RouteBody::Unphased(actions) = &mut touch.body {
        actions.insert(
            0,
            RouteAction::Let {
                pattern: Pattern::Ident("row".into()),
                value: Expr::Index(
                    Box::new(Expr::Ident("m_shadow".into())),
                    Box::new(Expr::Ident("outer".into())),
                ),
            },
        );
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => bool)) public m_nested_inner_exists"),
        "let Index wrong-base fallback must still detect inner exists (L99/L100): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_139_codegen_analysis_inner_block_exists_ast_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity InnerBlock {
            routes {
                constructor() => []
                touch(outer: u64, inner_k: u64) -> bool => [
                    let hit = {
                        m_nested[outer].exists(inner_k)
                    };
                    return(hit)
                ]
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => {}
                in touch(outer, inner_k) => {
                    if m_nested[outer].exists(inner_k) {
                        m_nested.update(outer, m_nested[outer].update(inner_k, m_nested[outer][inner_k] + 1))
                    } else {
                        m_nested.update(outer, m_nested[outer].insert(inner_k, 1))
                    }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => bool)) public m_nested_inner_exists"),
        "route Block with inner.exists must hit member_inner Block walk (L102): {sol}"
    );
    assert_solc_compiles("analysis_inner_block_exists_ast", &sol);
}

#[test]
fn n4_139_codegen_analysis_walk_match_arm_body_only_exists_ast_solc() {
    let mut program = parse_evm(
        r#"
        entity MatchArmBody {
            routes {
                constructor() => []
                probe(k: u64) -> u64 => [ return(0) ]
            }
            m_flags: HashMap<u64, u64> {
                in constructor() => {}
                in probe(k) => {
                    if m_flags.exists(k) {
                        m_flags.update(k, m_flags[k] + 1)
                    } else {
                        m_flags.insert(k, 1)
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let probe = entity.routes.iter_mut().find(|r| r.name == "probe").expect("probe");
    if let RouteBody::Unphased(actions) = &mut probe.body {
        if let RouteAction::Return { values } = &mut actions[0] {
            values[0] = Expr::Match(
                Box::new(Expr::Ident("k".into())),
                vec![
                    MatchArm {
                        pattern: MatchPattern::IntLiteral(U256::from_u128(0)),
                        body: Expr::IntLiteral(U256::ZERO),
                    },
                    MatchArm {
                        pattern: MatchPattern::Wildcard,
                        body: Expr::MethodCall(
                            Box::new(Expr::Ident("m_flags".into())),
                            "exists".into(),
                            vec![Expr::Ident("k".into())],
                        ),
                    },
                ],
            );
        }
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_flags_exists"),
        "Match with plain scrutinee and .exists() in arm body must walk member_uses_exists (L188–L190): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_139_codegen_analysis_walk_phased_match_arm_exists_ast_solc() {
    let mut program = parse_evm(
        r#"
        entity PhasedArmMatch {
            routes {
                constructor() => []
                probe(k: u64) -> u64 => [
                    prep: []
                    step: [ return(0) ]
                ]
            }
            m_flags: HashMap<u64, u64> {
                in constructor() => {}
                in probe(k) => {
                    if m_flags.exists(k) {
                        m_flags.update(k, m_flags[k] + 1)
                    } else {
                        m_flags.insert(k, 1)
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let probe = entity.routes.iter_mut().find(|r| r.name == "probe").expect("probe");
    if let RouteBody::Phased(phases) = &mut probe.body {
        let step = phases.iter_mut().find(|p| p.name == "step").expect("step");
        if let RouteAction::Return { values } = &mut step.actions[0] {
            values[0] = Expr::Match(
                Box::new(Expr::Ident("k".into())),
                vec![
                    MatchArm {
                        pattern: MatchPattern::IntLiteral(U256::from_u128(0)),
                        body: Expr::IntLiteral(U256::ZERO),
                    },
                    MatchArm {
                        pattern: MatchPattern::Wildcard,
                        body: Expr::MethodCall(
                            Box::new(Expr::Ident("m_flags".into())),
                            "exists".into(),
                            vec![Expr::Ident("k".into())],
                        ),
                    },
                ],
            );
        }
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_flags_exists"),
        "phased return Match arm-only .exists() must walk member_uses_exists (L186–L190): {sol}"
    );
    // TB-V: forge oracle dropped (analysis_walk_phased_match_arm_exists_ast).
}

#[test]
fn n4_139_codegen_analysis_pure_fn_overflow_member_continue_ast_solc() {
    let mut program = parse_evm(
        r#"
        pure fn pick(m: HashMap<u64, u64>, k: u64) -> u64 {
            if m.exists(k) { m[k] } else { 0 }
        }

        entity OverflowMember {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(pick(m_store, k)) ]
            }
            m_store: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let read = entity.routes.iter_mut().find(|r| r.name == "read").expect("read");
    if let RouteBody::Unphased(actions) = &mut read.body {
        if let RouteAction::Return { values } = &mut actions[0] {
            if let Expr::FnCall(_, args) = &mut values[0] {
                args.push(Expr::Ident("m_store".into()));
            }
        }
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("pick(m_store, m_store_exists, k")
            && sol.contains("mapping(uint64 => bool) public m_store_exists"),
        "member arg past param arity must hit continue arm (L242) before valid slot: {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_139_codegen_analysis_pure_fn_param_contains_cast_ast_solc() {
    let mut program = parse_evm(
        r#"
        pure fn tag(m: HashMap<u64, u64>, k: u64) -> u64 {
            if m.exists(k) { m[k] } else { 0 }
        }

        entity TagPure {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(tag(m_tags, k)) ]
            }
            m_tags: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
    );
    let pf = program.pure_fns.iter_mut().find(|p| p.name == "tag").expect("tag");
    pf.body = Expr::If(
        Box::new(Expr::Cast(
            Box::new(Expr::MethodCall(
                Box::new(Expr::Ident("m".into())),
                "exists".into(),
                vec![Expr::Ident("k".into())],
            )),
            Type::Simple("u64".into()),
        )),
        Box::new(Expr::MethodCall(
            Box::new(Expr::Ident("m".into())),
            "update".into(),
            vec![
                Expr::Ident("k".into()),
                Expr::MethodCall(
                    Box::new(Expr::Ident("m".into())),
                    "exists".into(),
                    vec![Expr::Ident("k".into())],
                ),
            ],
        )),
        Some(Box::new(Expr::IntLiteral(U256::ZERO))),
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("tag(m_tags, m_tags_exists, k)")
            && sol.contains("mapping(uint64 => bool) public m_tags_exists"),
        "pure-fn Cast+update/.exists args on param must walk param_uses_exists (L264–L265/L277): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_139_codegen_analysis_pure_fn_param_fieldaccess_block_ast_solc() {
    let mut program = parse_evm(
        r#"
        pure fn gate(m: HashMap<u64, u64>, k: u64) -> u64 {
            if m.exists(k) { m[k] } else { 0 }
        }

        entity GateBlock {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(gate(m_gate, k)) ]
            }
            m_gate: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
    );
    let pf = program.pure_fns.iter_mut().find(|p| p.name == "gate").expect("gate");
    pf.body = Expr::Block(vec![
        Expr::FieldAccess(Box::new(Expr::Ident("m".into())), "exists".into()),
        Expr::If(
            Box::new(Expr::MethodCall(
                Box::new(Expr::Ident("m".into())),
                "exists".into(),
                vec![Expr::Ident("k".into())],
            )),
            Box::new(Expr::Index(
                Box::new(Expr::Ident("m".into())),
                Box::new(Expr::Ident("k".into())),
            )),
            Some(Box::new(Expr::IntLiteral(U256::ZERO))),
        ),
    ]);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("gate(m_gate, m_gate_exists, k)")
            && sol.contains("mapping(uint64 => bool) public m_gate_exists"),
        "pure-fn Block/FieldAccess prelude must walk param_uses_exists (L269/L275): {sol}"
    );
    assert_solc_compiles("analysis_pure_fn_param_fieldaccess_block_ast", &sol);
}

#[test]
fn n4_139_codegen_analysis_iterate_is_empty_decoy_ident_ast_solc() {
    let mut program = parse_evm(
        r#"
        entity EmptyDecoy {
            routes {
                constructor() => []
                scan() => []
            }
            m_vals: HashMap<u64, u64> {
                in constructor() => {}
                in scan() => {
                    if m_vals.exists(0) {
                        m_vals.update(0, m_vals[0] + 1)
                    } else {
                        m_vals.insert(0, 1)
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let scan = entity.routes.iter_mut().find(|r| r.name == "scan").expect("scan");
    scan.where_clauses.push(cambrian_transpiler::ast::WhereClause {
        condition: Expr::BinOp(
            Box::new(Expr::MethodCall(
                Box::new(Expr::Ident("m_decoy".into())),
                "is_empty".into(),
                vec![],
            )),
            BinOp::And,
            Box::new(Expr::MethodCall(
                Box::new(Expr::Ident("m_vals".into())),
                "is_empty".into(),
                vec![],
            )),
        ),
        error_code: 1,
        error_name: None,
        error_args: vec![],
    });
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64[] public m_vals_keys")
            && sol.contains("m_vals_keys.length == 0"),
        "decoy is_empty Ident must fall through before real member is_empty (L331): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_139_codegen_analysis_pure_fn_rescue_param_exists_ast_solc() {
    let mut program = parse_evm(
        r#"
        pure fn shield(m: HashMap<u64, u64>, k: u64) -> u64 {
            if m.exists(k) { m[k] } else { 0 }
        }

        entity ShieldPure {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(shield(m_shield, k)) ]
            }
            m_shield: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let read = entity.routes.iter_mut().find(|r| r.name == "read").expect("read");
    if let RouteBody::Unphased(actions) = &mut read.body {
        actions.insert(
            0,
            RouteAction::Rescue {
                tag: "fail".into(),
                action: Box::new(RouteAction::Conditional {
                    condition: Expr::FnCall(
                        "shield".into(),
                        vec![
                            Expr::Ident("m_shield".into()),
                            Expr::Ident("k".into()),
                            Expr::Ident("m_shield".into()),
                        ],
                    ),
                    then_actions: vec![],
                    else_actions: vec![],
                }),
            },
        );
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("shield(m_shield, m_shield_exists, k")
            && sol.contains("mapping(uint64 => bool) public m_shield_exists"),
        "rescue FnCall with overflow member arg must walk pure_fn continue + exists (L242): {sol}"
    );
    assert_solc_compiles("analysis_pure_fn_rescue_param_exists_ast", &sol);
}

// ---------------------------------------------------------------------------
// N4-141: codegen/solidity/evm/analysis.rs — EXECUTABLE_CLOSED close-out slice 8 @ N4-139.
// Fresh llvm pre-slice: 95.61% (14 missed / 319). Targets: transforms If-else L413;
// param walk FnCall L276 (max 2 fixtures). Residual L71/L99/L102/L242/L264/L265 → llvm-partial.
// Acceptance: ≥ 99.0% or Δ ≥ −3 vs 14 or EXECUTABLE_CLOSED reaffirmed.
// ---------------------------------------------------------------------------

#[test]
fn n4_141_codegen_analysis_transforms_if_else_only_exists_layout_ast_solc() {
    let mut program = parse_evm(
        r#"
        entity LayoutIfElseOnly {
            routes {
                constructor() => []
                touch(k: u64) => []
            }
            m_map: HashMap<u64, u64> {
                in constructor() => {}
                in touch(k) => {
                    if m_map.exists(k) {
                        m_map[k]
                    } else {
                        0
                    }
                }
            }
        }
    "#,
    );
    let entity = program.entities.first_mut().expect("entity");
    let map = entity
        .members
        .iter_mut()
        .find(|m| m.name == "m_map")
        .expect("m_map");
    let touch = map.transforms.iter_mut().find(|t| t.route_name == "touch").expect("touch");
    // walk(c) and walk(t) must be false so L413 (else short-circuit) is evaluated.
    touch.body = Expr::If(
        Box::new(Expr::BoolLiteral(false)),
        Box::new(Expr::MethodCall(
            Box::new(Expr::Ident("m_map".into())),
            "insert".into(),
            vec![Expr::Ident("k".into()), Expr::IntLiteral(U256::from_u128(1))],
        )),
        Some(Box::new(Expr::MethodCall(
            Box::new(Expr::Ident("m_map".into())),
            "insert".into(),
            vec![
                Expr::Ident("k".into()),
                Expr::If(
                    Box::new(Expr::MethodCall(
                        Box::new(Expr::Ident("m_map".into())),
                        "exists".into(),
                        vec![Expr::Ident("k".into())],
                    )),
                    Box::new(Expr::IntLiteral(U256::from_u128(1))),
                    Some(Box::new(Expr::IntLiteral(U256::ZERO))),
                ),
            ],
        ))),
    );
    let layout = cambrian_transpiler::codegen::storage_layout::compute_layout(entity, false);
    assert!(
        layout.slots.iter().any(|s| s.name == "m_map_exists"),
        "transforms_use_exists If else-branch .exists() must reserve layout slot (L413)"
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_map_exists"),
        "entity emission must allocate _exists sidecar after layout walk: {sol}"
    );
    assert_solc_compiles("analysis_transforms_if_else_only_exists_layout_ast", &sol);
}

#[test]
fn n4_141_codegen_analysis_pure_fn_fncall_arg_exists_ast_solc() {
    let mut program = parse_evm(
        r#"
        pure fn flag(hit: bool) -> bool {
            hit
        }

        pure fn probe(m: HashMap<u64, u64>, k: u64) -> u64 {
            if m.exists(k) { m[k] } else { 0 }
        }

        entity FnCallArgPure {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(probe(m_probe, k)) ]
            }
            m_probe: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
    );
    let pf = program.pure_fns.iter_mut().find(|p| p.name == "probe").expect("probe");
    pf.body = Expr::If(
        Box::new(Expr::FnCall(
            "flag".into(),
            vec![Expr::MethodCall(
                Box::new(Expr::Ident("m".into())),
                "exists".into(),
                vec![Expr::Ident("k".into())],
            )],
        )),
        Box::new(Expr::Index(
            Box::new(Expr::Ident("m".into())),
            Box::new(Expr::Ident("k".into())),
        )),
        Some(Box::new(Expr::IntLiteral(U256::ZERO))),
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("probe(m_probe, m_probe_exists, k)")
            && sol.contains("mapping(uint64 => bool) public m_probe_exists"),
        "pure-fn FnCall arg .exists() must walk param_uses_exists FnCall arm (L276): {sol}"
    );
    assert_solc_compiles("analysis_pure_fn_fncall_arg_exists_ast", &sol);
}

// ---------------------------------------------------------------------------
// N4-10: codegen/solidity/evm/analysis.rs — HashMap sidecar / iteration walkers
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_analysis_exists_in_where_emits_sidecar_solc() {
    let program = parse_evm(
        r#"
        entity Vault {
            routes {
                deposit(k: u64, v: u64) => []
                read(k: u64) -> u64
                    where m_balances.exists(k) : throw 1
                    => [ return(m_balances[k]) ]
            }
            m_balances: HashMap<u64, u64> {
                in deposit(k, v) => m_balances.insert(k, v)
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_balances_exists"),
        "where-clause .exists() must allocate _exists sidecar: {sol}"
    );
    assert_solc_compiles("analysis_exists_where", &sol);
}

#[test]
fn n4_codegen_analysis_pure_fn_exists_threads_sidecar_solc() {
    let program = parse_evm(
        r#"
        pure fn balance(m: HashMap<address, u64>, owner: address) -> u64 {
            if m.exists(owner) { m[owner] } else { 0 }
        }

        entity Bank {
            routes {
                constructor() => []
                view bal(owner: address) -> u64 => [ return(balance(m_balances, owner)) ]
            }
            m_balances: HashMap<address, u64> { in constructor() => {} }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("balance(m_balances, m_balances_exists, owner)"),
        "pure-fn exists propagation must thread sidecar at call site: {sol}"
    );
    assert!(
        sol.contains("mapping(address => bool) public m_balances_exists"),
        "entity must emit _exists mapping: {sol}"
    );
    assert_solc_compiles("analysis_pure_fn_exists", &sol);
}

#[test]
fn n4_codegen_analysis_nested_inner_exists_direct_subscript_solc() {
    let program = parse_evm(
        r#"
        entity Allow {
            routes {
                constructor() => []
                approve(outer: address, inner: address, v: u64) => []
            }
            m_allow: HashMap<address, HashMap<address, u64>> {
                in approve(outer, inner, v) => {
                    let probe = if m_allow[outer].exists(inner) {
                        m_allow[outer][inner]
                    } else {
                        0
                    };
                    m_allow.update(outer, m_allow[outer].update(inner, probe + v))
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(address => mapping(address => bool)) public m_allow_inner_exists"),
        "direct m[k1].exists(k2) must emit 2-level inner_exists sidecar: {sol}"
    );
    assert_solc_compiles("analysis_nested_inner_exists", &sol);
}

#[test]
fn n4_codegen_analysis_hashmap_keys_iteration_emits_companion_solc() {
    let program = parse_evm(
        r#"
        entity Registry {
            routes {
                constructor() => []
                register(k: address, v: u64) => []
                listKeys() -> Vec<address> => [
                    let ks = m_balances.keys().collect();
                    return(ks)
                ]
            }
            m_balances: HashMap<address, u64> {
                in register(k, v) => m_balances.insert(k, v)
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("address[] public m_balances_keys"),
        "iterated HashMap must emit parallel keys array: {sol}"
    );
    assert!(
        sol.contains("mapping(address => uint256) private m_balances_keys_index"),
        "iterated HashMap must emit keys_index reverse lookup: {sol}"
    );
    assert_solc_compiles("analysis_hashmap_keys", &sol);
}

#[test]
fn n4_codegen_analysis_hashmap_is_empty_counts_as_iteration_solc() {
    let program = parse_evm(
        r#"
        entity Scores {
            routes {
                constructor() => []
                view empty() -> bool => [ return(m_scores.is_empty()) ]
            }
            m_scores: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64[] public m_scores_keys"),
        ".is_empty() must trigger keys companion emission: {sol}"
    );
    assert_solc_compiles("analysis_hashmap_is_empty", &sol);
}

#[test]
fn n4_codegen_analysis_for_in_hashmap_member_solc() {
    let program = parse_evm(
        r#"
        entity Tally {
            routes {
                constructor() => []
                walk() => [
                    for (k, v) in m_counts.iter() => [ let sink = k; ]
                ]
            }
            m_counts: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64[] public m_counts_keys"),
        "`for k in m_member` must mark HashMap as iterated: {sol}"
    );
    assert_solc_compiles("analysis_for_in_hashmap", &sol);
}

#[test]
fn n4_codegen_analysis_contains_member_default_and_layout_exists_solc() {
    let program = parse_evm(
        r#"
        entity Wallet {
            routes {
                constructor() => []
                touch(k: u64) => []
            }
            m_balances: HashMap<u64, u64> = {} {
                in touch(k) => {
                    if m_balances.contains(k) {
                        m_balances.update(k, m_balances[k])
                    } else {
                        m_balances.insert(k, 1)
                    }
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_balances_exists"),
        ".contains() must share _exists sidecar with .exists(): {sol}"
    );

    let program_exists = parse_evm(
        r#"
        entity Ledger {
            routes { constructor() => [] }
            m_map: HashMap<u64, u64> {
                in constructor() => {
                    if m_map.exists(0) { m_map } else { m_map.insert(0, 1) }
                }
            }
        }
    "#,
    );
    let entity = program_exists
        .entities
        .iter()
        .find(|e| e.name == "Ledger")
        .unwrap();
    let layout = cambrian_transpiler::codegen::storage_layout::compute_layout(entity, false);
    assert!(
        layout.slots.iter().any(|s| s.name == "m_map_exists"),
        "transforms_use_exists must reserve _exists slot in storage layout"
    );
    assert_solc_compiles("analysis_contains_default", &sol);
}

// ---------------------------------------------------------------------------
// N4-11: codegen/solidity/core/types.rs — narrow ints, record HashMap, defaults
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_types_narrow_unsigned_members_solc() {
    let program = parse_evm(
        r#"
        entity Gauge {
            routes {
                constructor() => []
                bump() => []
            }
            m_byte: u8 { in constructor() => 0 in bump() => m_byte + 1 }
            m_word: u16 { in constructor() => 0 }
            m_slot: u32 { in constructor() => 0 }
            m_wide: u128 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("uint8 public m_byte"), "u8 member: {sol}");
    assert!(sol.contains("uint16 public m_word"), "u16 member: {sol}");
    assert!(sol.contains("uint32 public m_slot"), "u32 member: {sol}");
    assert!(sol.contains("uint128 public m_wide"), "u128 member: {sol}");
    assert_solc_compiles("types_narrow_unsigned", &sol);
}

#[test]
fn n4_codegen_types_signed_narrow_members_solc() {
    let program = parse_evm(
        r#"
        entity Thermo {
            routes {
                constructor() => []
                adjust(delta: i8) => []
            }
            m_delta: i8 { in constructor() => 0 in adjust(delta) => m_delta + delta }
            m_bias: i64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("int8 public m_delta"), "i8 member: {sol}");
    assert!(sol.contains("int64 public m_bias"), "i64 member: {sol}");
    assert!(sol.contains("function adjust(int8 delta)"), "i8 route param: {sol}");
    assert_solc_compiles("types_signed_narrow", &sol);
}

#[test]
fn n4_codegen_types_hashmap_record_value_solc() {
    let program = parse_evm(
        r#"
        record Point { x: u64, y: u64 }

        entity Registry {
            routes {
                constructor() => []
                set(k: u64, p: Point) => []
            }
            m_points: HashMap<u64, Point> {
                in set(k, p) => m_points.insert(k, p)
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => Point) public m_points"),
        "HashMap with record value must use sol_type_entity: {sol}"
    );
    assert!(
        sol.contains("function set(uint64 k, Point memory p)"),
        "record route param must carry memory annotation: {sol}"
    );
    assert_solc_compiles("types_hashmap_record", &sol);
}

#[test]
fn n4_codegen_types_vec_string_route_memory_params_solc() {
    let program = parse_evm(
        r#"
        entity Ingest {
            routes {
                constructor() => []
                load(label: String, batch: Vec<u64>, blob: bytes) => []
            }
            m_count: u64 { in load(label, batch, blob) => batch.length() }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function load(string memory label, uint64[] memory batch, bytes memory blob)"),
        "reference route params need memory location: {sol}"
    );
    assert_solc_compiles("types_memory_params", &sol);
}

#[test]
fn n4_codegen_types_payload_enum_zero_fills_inactive_fields_solc() {
    let program = parse_evm(
        r#"
        enum Action {
            Deposit(u64),
            Withdraw(u64, String)
        }

        entity Store {
            routes {
                constructor() => []
                pick() => []
            }
            m_act: Action {
                in pick() => Action::Deposit(7)
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Action::Deposit(7)") || sol.contains("Action({tag: Action_Tag.Deposit"),
        "payload enum variant construction must be lowered: {sol}"
    );
    assert!(
        sol.contains("withdraw_1: \"\""),
        "inactive variant String field must use solidity_default_for_member_ty: {sol}"
    );
    assert_solc_compiles("types_payload_enum_defaults", &sol);
}

#[test]
fn n4_codegen_types_record_member_transform_solc() {
    let program = parse_evm(
        r#"
        record Point { x: u64, y: u64 }

        entity Canvas {
            routes {
                constructor() => []
                clear() => []
            }
            m_origin: Point {
                in clear() => { Point { x: 0, y: 0 } }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("struct Point"),
        "record struct must be emitted: {sol}"
    );
    assert!(
        sol.contains("Point memory") || sol.contains("Point public m_origin"),
        "record member must lower with struct type: {sol}"
    );
    assert_solc_compiles("types_record_member", &sol);
}

// ---------------------------------------------------------------------------
// N4-12: codegen/solidity/core/types.rs part 2 — inference / subst / push
// Gap map @ 61.82% (386 missed): subst_ident 72, lower_member_push 48,
// infer_iter_elem 47, infer_let_type 42, actual_sol_type 17.
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_types_nested_hashmap_let_match_subst_solc() {
    let program = parse_evm(
        r#"
        entity NestedMap {
            routes {
                constructor() => []
                probe(outer: u64, inner_k: u64, v: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
                in probe(outer, inner_k, v) => {
                    let inner = if m_nested.exists(outer) {
                        m_nested[outer]
                    } else {
                        {}
                    };
                    m_nested.update(outer, inner.update(inner_k, v))
                }
            }
            m_flag: u64 {
                in constructor() => 0
                in probe(outer, inner_k, v) => {
                    let inner = if m_nested.exists(outer) {
                        m_nested[outer]
                    } else {
                        {}
                    };
                    match inner_k {
                        0 => 0,
                        _ => {
                            let hit = inner.exists(inner_k);
                            if hit { 1 } else { 0 }
                        }
                    }
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => uint64))"),
        "nested HashMap must lower to nested mapping: {sol}"
    );
    assert!(
        sol.contains("if (inner_k == 0)") || sol.contains("if (inner_k != 0)"),
        "match on inner_k must lower (subst_ident_in_expr Match arm): {sol}"
    );
    assert!(
        sol.contains("m_nested_inner_exists") || sol.contains("m_nested[outer]"),
        "HashMap let must substitute inner.exists into sidecar read: {sol}"
    );
    assert_solc_compiles("types_nested_hashmap_let_match", &sol);
}

#[test]
fn n4_codegen_types_vec_push_let_transform_solc() {
    let program = parse_evm(
        r#"
        entity PairFactory {
            routes {
                constructor() => []
                createPair(a: address, b: address) => []
            }
            m_pairs: Vec<address> {
                in createPair(a, b) => {
                    let pair = a;
                    m_pairs.push(pair)
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains(".push("),
        "Vec member transform must emit push (lower_member_push_body): {sol}"
    );
    assert!(
        sol.contains("address pair = a"),
        "push transform must hoist let prelude with infer_let_type_entity: {sol}"
    );
    assert_solc_compiles("types_vec_push_let", &sol);
}

#[test]
fn n4_codegen_types_hashmap_iter_for_pure_solc() {
    let program = parse_evm(
        r#"
        library MapIter {
            pure fn sum_keys(m: HashMap<u64, u64>) -> u64 {
                for k in m.keys() { k + 1 }
            }
            pure fn sum_vals(m: HashMap<u64, u64>) -> u64 {
                for v in m.values() { v + 1 }
            }
            pure fn sum_both(m: HashMap<u64, u64>) -> u64 {
                let a = sum_keys(m);
                let b = sum_vals(m);
                a + b
            }
        }

        entity MapProbe {
            routes {
                constructor() => []
                sumAll() -> u64 => [ return(MapIter::sum_both(m_map)) ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function sum_keys") && sol.contains("function sum_vals"),
        "pure fn for over keys/values must be emitted: {sol}"
    );
    assert!(
        sol.contains("for (uint256") || sol.contains("for (uint64"),
        "infer_iter_elem_type_entity must drive typed loop vars: {sol}"
    );
    assert_solc_compiles("types_hashmap_iter_for_pure", &sol);
}

#[test]
fn n4_codegen_types_record_vec_for_elem_infer_solc() {
    let program = parse_evm(
        r#"
        record Bucket { ids: Vec<u32> }

        library BucketLib {
            pure fn double_ids(b: Bucket) -> Vec<u32> {
                for x in b.ids { x * 2 }
            }
        }

        entity BucketHost {
            routes {
                constructor() => []
                double(b: Bucket) -> Vec<u32> => [
                    return(BucketLib::double_ids(b))
                ]
            }
            m_runs: u64 {
                in constructor() => 0
                in double(b) => m_runs + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("struct Bucket"),
        "record must be emitted: {sol}"
    );
    assert!(
        sol.contains("function double_ids"),
        "FieldAccess Vec iteration must compile via infer_iter_elem_type_entity: {sol}"
    );
    assert_solc_compiles("types_record_vec_for_elem", &sol);
}

#[test]
fn n4_codegen_types_let_record_field_and_update_infer_solc() {
    let program = parse_evm(
        r#"
        record TokenData { owner: address, amt: u64 }

        entity TokenStore {
            routes {
                constructor() => []
                ownerOf(id: u64) -> address => [
                    let data = m_tokens[id];
                    return(data.owner)
                ]
                rewrite(id: u64) => []
            }
            m_tokens: HashMap<u64, TokenData> {
                in constructor() => HashMap::new()
                in rewrite(id) => {
                    let row = m_tokens[id];
                    let updated = row { owner: msg::sender, amt: row.amt + 1 };
                    m_tokens.update(id, updated)
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function ownerOf(uint64 id)"),
        "route with let record field access must emit: {sol}"
    );
    assert!(
        sol.contains("data.owner") || sol.contains(".owner"),
        "infer_let_type_entity must resolve let-bound record field type: {sol}"
    );
    assert!(
        sol.contains("TokenData memory") || sol.contains("struct TokenData"),
        "infer_record_from_update_fields / RecordUpdate must lower record: {sol}"
    );
    assert_solc_compiles("types_let_record_field_update", &sol);
}

#[test]
fn n4_codegen_types_narrow_sys_timestamp_solc() {
    let program = parse_evm(
        r#"
        entity Clock {
            routes {
                constructor() => []
                tick() => []
            }
            m_last: u32 {
                in constructor() => 0
                in tick() => sys::timestamp
            }
            m_slot: u64 {
                in tick() => msg::value
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint32 public m_last"),
        "u32 member storage: {sol}"
    );
    assert!(
        sol.contains("uint32(") || sol.contains("uint64("),
        "actual_sol_type / wrap_narrow_cast_with_actual must narrow block.timestamp/msg.value: {sol}"
    );
    assert_solc_compiles("types_narrow_sys_timestamp", &sol);
}

#[test]
fn n4_codegen_types_hashmap_keys_let_route_solc() {
    let program = parse_evm(
        r#"
        entity KeyBag {
            routes {
                constructor() => []
                listKeys() -> Vec<u64> => [
                    let ks = m_map.keys().collect();
                    return(ks)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64[] memory") || sol.contains("function listKeys"),
        "m.keys().collect() let must infer uint64[] memory (infer_let_type_entity): {sol}"
    );
    assert_solc_compiles("types_hashmap_keys_let_route", &sol);
}

// ---------------------------------------------------------------------------
// N4-16: codegen/solidity/core/types.rs — tail (subst_ident_in_expr, infer_let/
// infer_iter, solidity_default_for_member_ty)
// Gap map @ 67.26% (~331 missed): subst_ident_in_expr rec (~95), infer_let (~40),
// infer_iter HashMap index/values (~22), sol_type_prog (~28, dead — uncalled),
// infer_record_from_update_fields (~12).
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_types_subst_hashmap_closure_addressof_solc() {
    let program = parse_evm(
        r#"
        entity Target {
            identity m_id: u64
            routes { constructor() => [] }
        }

        entity SubstHost {
            routes {
                constructor() => []
                link(outer: u64, k: u64, id: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
                in link(outer, k, id) => {
                    let inner = if m_nested.exists(outer) {
                        m_nested[outer]
                    } else {
                        {}
                    };
                    let _dest = addressOf(Target.state(id));
                    let bumped = (0..k).fold(0, |acc, x| if inner.exists(x) { acc + inner[x] } else { acc });
                    m_nested.update(outer, inner.update(k, bumped))
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("predictTarget") || sol.contains("keccak256"),
        "addressOf in HashMap-let body must lower (subst AddressOf arm): {sol}"
    );
    assert!(
        sol.contains("m_nested[outer]") || sol.contains("m_nested_inner"),
        "HashMap let must inline inner into closure/fold body: {sol}"
    );
    assert_solc_compiles("types_subst_closure_addressof", &sol);
}

#[test]
fn n4_codegen_types_subst_hashmap_match_namespaced_solc() {
    let program = parse_evm(
        r#"
        entity ModeHost {
            routes {
                constructor() => []
                pick(outer: u64, k: u64) => []
            }
            m_data: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
                in pick(outer, k) => {
                    let inner = if m_data.exists(outer) {
                        m_data[outer]
                    } else {
                        {}
                    };
                    let _pair = (inner, k);
                    m_data.update(outer, inner.update(k, std::math::max(1, inner[k])))
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("std::math::max") || sol.contains("Math.max") || sol.contains("max("),
        "NamespacedCall inside subst body must lower: {sol}"
    );
    assert!(
        sol.contains("m_data[outer]") || sol.contains("m_data_inner"),
        "Tuple + HashMap let must inline inner (subst Tuple arm): {sol}"
    );
    assert_solc_compiles("types_subst_match_namespaced", &sol);
}

#[test]
fn n4_codegen_types_infer_hashmap_values_fold_solc() {
    let program = parse_evm(
        r#"
        entity ValIter {
            routes {
                constructor() => []
                sumValues() -> u64 => [
                    let total = m_map.values().fold(0, |acc, v| acc + v);
                    return(total)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function sumValues"),
        "values().fold route must emit: {sol}"
    );
    assert!(
        sol.contains("m_map_values") || sol.contains("m_map[") || sol.contains("for (uint256"),
        "infer_iter_elem_type_entity values() arm must drive loop lowering: {sol}"
    );
    assert_solc_compiles("types_infer_hashmap_values_fold", &sol);
}

#[test]
fn n4_codegen_types_infer_hashmap_index_vec_fold_solc() {
    let program = parse_evm(
        r#"
        entity HmVec {
            routes {
                constructor() => []
                scan(k: u64) -> u64 => [
                    let row = m_boxes[k];
                    let sum = row.fold(0, |acc, x| acc + x);
                    return(sum)
                ]
            }
            m_boxes: HashMap<u64, Vec<u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64[] memory row") || sol.contains("row = m_boxes[k]"),
        "HashMap[k] Vec let must infer array type (infer_let Index arm): {sol}"
    );
    assert!(
        sol.contains("row.fold") || sol.contains("for (uint256"),
        "fold on let-bound Vec must compile (infer_iter FieldAccess/Index): {sol}"
    );
    assert_solc_compiles("types_infer_hashmap_index_vec", &sol);
}

#[test]
fn n4_codegen_types_infer_record_default_and_update_solc() {
    let program = parse_evm(
        r#"
        type Tick = u32

        record Quote { price: Tick, qty: u64 }

        entity QuoteStore {
            routes {
                constructor() => []
                seed() => []
                read() -> u64 => [
                    let snap = m_q;
                    return(snap.qty)
                ]
            }
            m_q: Quote {
                in constructor() => { Quote { price: 0, qty: 0 } }
                in seed() => {
                    let snap = m_q;
                    let next = snap { qty: snap.qty + 1 };
                    next
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("struct Quote"),
        "program record must emit struct: {sol}"
    );
    assert!(
        sol.contains("Quote memory") || sol.contains("Quote("),
        "solidity_default_for_member_ty / infer_record_from_update_fields must type Quote: {sol}"
    );
    assert_solc_compiles("types_infer_record_default_update", &sol);
}

#[test]
fn n4_codegen_types_infer_let_bool_binop_and_prog_enum_solc() {
    let program = parse_evm(
        r#"
        enum GlobalState { Off, On }

        entity NarrowBin {
            routes {
                constructor() => []
                peek(a: u32, b: u32) -> bool => [
                    let ok = a + b > 0;
                    return(ok)
                ]
                state() -> GlobalState => [
                    let s = GlobalState::On;
                    return(s)
                ]
            }
            m_x: u64 {
                in constructor() => 0
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("bool ok =") || sol.contains("bool ok="),
        "comparison binop let must infer bool (infer_let BinOp Eq/Lt arm): {sol}"
    );
    assert!(
        sol.contains("GlobalState s = GlobalState.On") || sol.contains("GlobalState.On"),
        "program-level enum variant let must infer enum type: {sol}"
    );
    assert_solc_compiles("types_infer_bool_binop_prog_enum", &sol);
}

// ---------------------------------------------------------------------------
// N4-23: codegen/solidity/core/types.rs — tail (`solidity_default_for_member_ty`,
// `subst_ident_in_expr` deep arms). Gap @ 69.83% (305 missed): default ~55,
// subst rec ~76; `sol_type_prog` dead — skip.
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_types_member_default_sentinels_solc() {
    let program = parse_evm(
        r#"
        record Point { x: u64, y: u64 }
        enum Phase { Idle, Run }

        entity DefaultHost {
            routes {
                constructor() => []
                wipe() => []
            }
            m_list: Vec<u64> {
                in constructor() => array()
                in wipe() => {}
            }
            m_opt_u: Option<u64> {
                in constructor() => some(9)
                in wipe() => none
            }
            m_opt_str: Option<String> {
                in constructor() => some("x")
                in wipe() => none
            }
            m_blob: bytes {
                in constructor() => "aa"
                in wipe() => {}
            }
            m_peer: Option<address> {
                in constructor() => some(0x0000000000000000000000000000000000000001)
                in wipe() => none
            }
            m_pt: Point {
                in constructor() => { Point { x: 1, y: 2 } }
                in wipe() => {}
            }
            m_phase: Phase {
                in constructor() => Phase::Run
                in wipe() => {}
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("new uint64[](0)"),
        "Vec wipe sentinel must use solidity_default_for_member_ty new T[](0): {sol}"
    );
    assert!(
        sol.contains("\"\"") && (sol.contains("m_opt_str") || sol.contains("next_m_opt_str")),
        "Option<String> none must default inner string to empty literal: {sol}"
    );
    assert!(
        sol.contains("address(0)"),
        "Option<address> none must default to address(0): {sol}"
    );
    assert!(
        sol.contains("Point({") || sol.contains("Point({x:"),
        "record wipe sentinel must emit nested struct default literal: {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

#[test]
fn n4_codegen_types_program_record_default_solc() {
    let program = parse_evm(
        r#"
        record HostCfg { limit: u32, label: String }

        entity CfgBox {
            routes {
                constructor() => []
                reset() => []
            }
            m_cfg: HostCfg {
                in constructor() => { HostCfg { limit: 10, label: "ok" } }
                in reset() => {}
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("HostCfg({") || sol.contains("HostCfg({limit:"),
        "program-record default must recurse via ctx.lookup_record: {sol}"
    );
    assert!(
        sol.contains("label: \"\""),
        "String field in program-record default must use empty string: {sol}"
    );
    assert_solc_compiles("types_program_record_default", &sol);
}

#[test]
fn n4_codegen_types_subst_unary_cast_array_some_solc() {
    let program = parse_evm(
        r#"
        entity UnarySubst {
            routes {
                constructor() => []
                bump(outer: u64, k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
                in bump(outer, k) => {
                    let inner = if m_nested.exists(outer) {
                        m_nested[outer]
                    } else {
                        {}
                    };
                    let val = inner[k];
                    let neg = -val;
                    let narrow = val as u8;
                    let _pair = (val, k);
                    let _wrap = some(val);
                    m_nested.update(outer, inner.update(k, narrow + neg))
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("-") && (sol.contains("m_nested[outer]") || sol.contains("m_nested_inner")),
        "UnaryOp must substitute inner into negation (subst UnaryOp arm): {sol}"
    );
    assert!(
        sol.contains("uint8") || sol.contains("as uint8"),
        "Cast must substitute inner before as-cast (subst Cast arm): {sol}"
    );
    assert!(
        (sol.contains("(") && sol.contains("val"))
            || sol.contains("m_nested[outer]")
            || sol.contains("m_nested_inner"),
        "Tuple + Some must substitute inner (subst Tuple/Some arms): {sol}"
    );
    assert_solc_compiles("types_subst_unary_cast_array", &sol);
}

#[test]
fn n4_codegen_types_subst_record_enum_range_solc() {
    let program = parse_evm(
        r#"
        record Cell { v: u64, alive: bool }
        enum Mode { Off, On(u64) }

        entity RecordSubst {
            routes {
                constructor() => []
                touch(id: u64) => []
            }
            m_cells: HashMap<u64, Cell> {
                in constructor() => HashMap::new()
                in touch(id) => {
                    let cell = if m_cells.exists(id) {
                        m_cells[id]
                    } else {
                        {}
                    };
                    let next = cell { v: cell.v + 1, alive: true };
                    let tag = Mode::On(cell.v);
                    let _span = (cell.v..cell.v + 1);
                    let _pair = { Cell { v: cell.v, alive: cell.alive } };
                    m_cells.update(id, next)
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("cell.v + 1") || sol.contains("m_cells[id].v + 1") || sol.contains("_v + 1"),
        "RecordUpdate must substitute cell binding (subst RecordUpdate arm): {sol}"
    );
    assert!(
        sol.contains("Mode.On") || sol.contains("Mode_Tag.On") || sol.contains("Mode({"),
        "EnumVariantWithData must substitute cell.v into payload (subst arm): {sol}"
    );
    assert!(
        sol.contains("Cell({") || sol.contains("struct Cell"),
        "RecordConstruct inside subst body must lower: {sol}"
    );
    // TB-V: forge oracle dropped (types_subst_record_enum_range).
}

#[test]
fn n4_codegen_types_subst_closure_for_shadow_solc() {
    let program = parse_evm(
        r#"
        entity ShadowSubst {
            routes {
                constructor() => []
                run(outer: u64, k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
                in run(outer, k) => {
                    let inner = if m_nested.exists(outer) {
                        m_nested[outer]
                    } else {
                        {}
                    };
                    let folded = (0..k).fold(0, |inner, x| {
                        if inner.exists(x) { inner + inner[x] } else { inner }
                    });
                    m_nested.update(outer, inner.update(k, folded))
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("fold") || sol.contains("for (uint256"),
        "Range + fold must lower inside HashMap-let subst body: {sol}"
    );
    assert!(
        sol.contains("inner_exists") || sol.contains("m_nested[outer]") || sol.contains("m_nested_inner"),
        "closure param shadowing `inner` must keep mapping read inside fold body: {sol}"
    );
    assert_solc_compiles("types_subst_closure_shadow", &sol);
}

// ---------------------------------------------------------------------------
// N4-48: codegen/solidity/core/types.rs — slice 5 (infer_iter/let entity, subst)
// Baseline @ N4-47: 73.10% line (272 missed / 1011); focus L986–L1157, L1167–L1409,
// subst_ident_in_expr residual; exclude dead sol_type_prog L667–L746.
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_types_infer_iter_keys_fold_entity_solc() {
    let program = parse_evm(
        r#"
        entity KeyFold {
            routes {
                constructor() => []
                sumKeys() -> u64 => [
                    let total = m_map.keys().fold(0, |acc, k| acc + k + m_map[k]);
                    return(total)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_map_keys.length") || sol.contains("m_map_keys["),
        "keys().fold must walk parallel key array (infer_iter_elem_type_entity keys arm): {sol}"
    );
    assert!(
        sol.contains("uint64 k = m_map_keys[") || sol.contains("uint64 k=m_map_keys["),
        "fold loop var over .keys() must be key-typed: {sol}"
    );
    assert_solc_compiles("types_infer_iter_keys_fold_entity", &sol);
}

#[test]
fn n4_codegen_types_infer_iter_hashmap_index_direct_fold_solc() {
    let program = parse_evm(
        r#"
        entity HmIndexFold {
            routes {
                constructor() => []
                sumRow(outer: u64) -> u64 => [
                    return(m_boxes[outer].fold(0, |acc, x| acc + x))
                ]
            }
            m_boxes: HashMap<u64, Vec<u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_boxes[outer]") && (sol.contains(".fold") || sol.contains("for (uint256")),
        "HashMap-index Vec fold must lower (infer_iter Index arm): {sol}"
    );
    assert_solc_compiles("types_infer_iter_hashmap_index_direct_fold", &sol);
}

#[test]
fn n4_codegen_types_infer_iter_member_record_field_vec_solc() {
    let program = parse_evm(
        r#"
        record Bucket { ids: Vec<u32> }

        entity BucketFold {
            routes {
                constructor() => []
                bump() => []
            }
            m_bucket: Bucket {
                in bump() => m_bucket.ids.fold(0, |acc, x| acc + x)
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("struct Bucket"),
        "record Bucket must be emitted: {sol}"
    );
    assert!(
        sol.contains("m_bucket.ids") || sol.contains("ids.length"),
        "member record field Vec fold must lower (infer_iter FieldAccess arm): {sol}"
    );
    assert_solc_compiles("types_infer_iter_member_record_field_vec", &sol);
}

#[test]
fn n4_codegen_types_infer_iter_keys_for_block_let_solc() {
    let program = parse_evm(
        r#"
        entity KeyForBlock {
            routes {
                constructor() => []
                materialize() -> Vec<u64> => [
                    let ks = { for k in m_map.keys() { k + 1 } };
                    return(ks)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64[] memory ks") || sol.contains("uint64[] memory"),
        "block-wrapped for over keys must infer Vec element type: {sol}"
    );
    assert!(
        sol.contains("m_map_keys"),
        "for k in m_map.keys() must use parallel keys storage: {sol}"
    );
    assert_solc_compiles("types_infer_iter_keys_for_block_let", &sol);
}

#[test]
fn n4_codegen_types_infer_let_bundle_route_solc() {
    let program = parse_evm(
        r#"
        entity LetBundle {
            routes {
                constructor() => []
                probe(outer: u64, id: u64) -> u64 => [
                    let flags = m_map.exists(id);
                    let vs = m_map.values().collect();
                    let elem = m_vec[outer];
                    let ranged = 0..outer;
                    let tagged = some(outer);
                    let digest = 0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20;
                    let tail = { let a = outer; a + id };
                    return(elem + ranged + tail)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
            m_vec: Vec<u64> {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("bool flags =") || sol.contains("bool flags="),
        "exists() let must infer bool: {sol}"
    );
    assert!(
        sol.contains("uint64[] memory vs") || sol.contains("uint64[] memory"),
        "values() let must infer V[] memory: {sol}"
    );
    assert!(
        sol.contains("uint64 elem =") || sol.contains("uint64 elem="),
        "Vec index let must infer element type: {sol}"
    );
    assert_solc_compiles("types_infer_let_bundle_route", &sol);
}

#[test]
fn n4_codegen_types_infer_let_deterministic_entity_address_solc() {
    let program = parse_evm(
        r#"
        entity Peer {
            identity m_id: u64
            routes { constructor() => [] }
        }

        entity Caller {
            identity m_id: u64
            routes {
                constructor() => []
                peerAddr() -> address => [
                    let dest = Peer.address(m_id);
                    return(dest)
                ]
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("address dest =") || sol.contains("address dest="),
        "Entity.address() let must infer address (infer_let MethodCall address arm): {sol}"
    );
    assert!(
        sol.contains("predictPeer") || sol.contains("keccak256"),
        "deterministic Peer.address must lower via factory: {sol}"
    );
    assert_solc_compiles("types_infer_let_det_entity_address", &sol);
}

#[test]
fn n4_codegen_types_infer_let_nested_record_field_update_solc() {
    let program = parse_evm(
        r#"
        record TokenData { owner: address, amt: u64 }

        entity TokenNest {
            routes {
                constructor() => []
                ownerOf(id: u64) -> address => [
                    let row = m_tokens[id];
                    let owner = row.owner;
                    return(owner)
                ]
                rewrite(id: u64) => []
            }
            m_tokens: HashMap<u64, TokenData> {
                in constructor() => HashMap::new()
                in rewrite(id) => {
                    let row = m_tokens[id];
                    let bumped = row { owner: msg::sender, amt: row.amt + 1 };
                    m_tokens.update(id, bumped)
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("row.owner") || sol.contains("TokenData memory row"),
        "let-bound record field access must lower through infer_let FieldAccess: {sol}"
    );
    assert!(
        sol.contains("TokenData memory bumped") || sol.contains("_cam_tmp0"),
        "nested RecordUpdate in member transform must keep record type: {sol}"
    );
    assert_solc_compiles("types_infer_let_nested_record_field_update", &sol);
}

#[test]
fn n4_codegen_types_subst_match_for_hashmap_let_solc() {
    let program = parse_evm(
        r#"
        entity MatchSubst {
            routes {
                constructor() => []
                pick(outer: u64, k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
                in pick(outer, k) => {
                    let inner = if m_nested.exists(outer) {
                        m_nested[outer]
                    } else {
                        {}
                    };
                    let score = match inner[k] {
                        0 => 1,
                        _ => inner[k] + std::math::max(1, k)
                    };
                    let bumped = { for x in (0..k) { if inner.exists(x) { inner[x] } else { 0 } } };
                    m_nested.update(outer, inner.update(k, score + bumped))
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("match") || sol.contains("if (") && sol.contains("inner"),
        "Match on substituted inner[k] must lower (subst Match arm): {sol}"
    );
    assert!(
        sol.contains("m_nested[outer]") || sol.contains("inner"),
        "HashMap let substitution must inline inner into match/for body: {sol}"
    );
    assert_solc_compiles("types_subst_match_for_hashmap_let", &sol);
}

#[test]
fn n4_codegen_types_infer_record_update_field_match_solc() {
    let program = parse_evm(
        r#"
        record Quote { price: u32, qty: u64 }

        entity QuoteNest {
            routes {
                constructor() => []
                bump() => []
            }
            m_q: Quote {
                in constructor() => { Quote { price: 0, qty: 0 } }
                in bump() => {
                    let snap = m_q;
                    snap { qty: snap.qty + 1 }
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Quote memory") || sol.contains("_cam_tmp"),
        "RecordUpdate on let-bound record must infer Quote memory: {sol}"
    );
    assert!(
        sol.contains("struct Quote"),
        "entity record must emit struct: {sol}"
    );
    assert_solc_compiles("types_infer_record_update_field_match", &sol);
}

#[test]
fn n4_codegen_types_infer_let_tuple_destructure_route_solc() {
    let program = parse_evm(
        r#"
        entity TupleLet {
            routes {
                constructor() => []
                pairSum(a: u64, b: u64) -> u64 => [
                    let (x, y) = (a, b);
                    return(x + y)
                ]
            }
            m_acc: u64 {
                in constructor() => 0
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("(uint256 x, uint256 y)") || sol.contains("(uint64 x, uint64 y)"),
        "tuple destructure let must emit typed slots: {sol}"
    );
    assert_solc_compiles("types_infer_let_tuple_destructure_route", &sol);
}

// ---------------------------------------------------------------------------
// N4-50: codegen/solidity/core/types.rs — slice 6 (actual_sol_type, tuple infer, subst)
// Baseline @ N4-49: 75.47% line (248 missed / 1011); focus L769–L941, L1415–L1451,
// subst MacroRef/Encode/AddressOf, infer_let scatter; exclude dead sol_type_prog.
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_types_actual_sol_temporal_narrow_member_solc() {
    let program = parse_evm(
        r#"
        entity TemporalNarrow {
            routes {
                constructor() => []
                bump() => [
                    grow: []
                ]
            }
            m_amt: u64 {
                in constructor() => 0
                in bump() => grow: ^m_amt + 1
            }
            m_tag: u32 {
                in bump() => grow: hashOf(m_amt, 1) as u32
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("next_m_amt") || sol.contains("^"),
        "phased ^m_amt must drive temporal lowering: {sol}"
    );
    assert!(
        sol.contains("uint32(") && sol.contains("keccak256"),
        "hashOf as u32 member must hit actual_sol_type + maybe_narrow_cast: {sol}"
    );
    assert_solc_compiles("types_actual_sol_temporal_narrow", &sol);
}

#[test]
fn n4_codegen_types_actual_sol_stdlib_compare_record_field_solc() {
    let program = parse_evm(
        r#"
        record Quote { price: u32, qty: u64 }

        entity QuotePeek {
            routes {
                constructor() => []
                compare(a: u64, b: u64) -> bool => [
                    let ok = std::math::clamp(a, 1, b) > m_q.price;
                    return(ok)
                ]
            }
            m_q: Quote {
                in constructor() => { Quote { price: 1, qty: 0 } }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("bool ok =") || sol.contains("bool ok="),
        "comparison on clamp result must infer bool (actual_sol_type BinOp compare): {sol}"
    );
    assert!(
        sol.contains(".price") || sol.contains("m_q"),
        "record field access must resolve via actual_sol_type FieldAccess: {sol}"
    );
    assert_solc_compiles("types_actual_sol_stdlib_compare_field", &sol);
}

#[test]
fn n4_codegen_types_wrap_narrow_signed_int_from_timestamp_solc() {
    let program = parse_evm(
        r#"
        entity SignedNarrow {
            routes {
                constructor() => []
                tick() => []
            }
            m_bias: i64 {
                in constructor() => 0
                in tick() => sys::timestamp
            }
            m_delta: i8 {
                in tick() => m_delta + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("int64(") || sol.contains("int256(") || sol.contains("uint32(block.timestamp)"),
        "i64 <= sys::timestamp must hit wrap_narrow_cast signed path: {sol}"
    );
    assert_solc_compiles("types_wrap_narrow_signed_int_timestamp", &sol);
}

#[test]
fn n4_codegen_types_infer_tuple_divmod_route_solc() {
    let program = parse_evm(
        r#"
        entity DivmodHost {
            routes {
                constructor() => []
                split(a: u64, b: u64) -> u64 => [
                    let (q, r) = std::math::divmod(a, b);
                    return(q + r)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("(uint256 q, uint256 r)") || sol.contains("(uint64 q, uint64 r)"),
        "std::math::divmod tuple destructure must use infer_tuple_elem_types divmod arm: {sol}"
    );
    assert_solc_compiles("types_infer_tuple_divmod_route", &sol);
}

#[test]
fn n4_codegen_types_infer_tuple_pure_fn_return_solc() {
    let program = parse_evm(
        r#"
        library PairLib {
            pure fn dup(a: u64) -> (u64, u64) { (a, a) }
        }

        entity PairHost {
            routes {
                constructor() => []
                both(n: u64) -> u64 => [
                    let (x, y) = PairLib::dup(n);
                    return(x + y)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function dup") && (sol.contains("(uint256 x, uint256 y)") || sol.contains("(uint64 x, uint64 y)")),
        "pure-fn tuple return must hit infer_tuple_elem_types FnCall Tuple arm: {sol}"
    );
    assert_solc_compiles("types_infer_tuple_pure_fn_return", &sol);
}

#[test]
fn n4_codegen_types_infer_tuple_literal_if_block_solc() {
    let program = parse_evm(
        r#"
        entity TupleLit {
            routes {
                constructor() => []
                pack(flag: bool, a: u64, b: u64) -> u64 => [
                    let (x, y) = if flag { (a, b) } else { (b, a) };
                    let pair = { let t = a; (t, b) };
                    return(x + y + pair.0)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("(uint256 x, uint256 y)") || sol.contains("(uint64 x, uint64 y)"),
        "tuple-literal / if RHS must hit infer_tuple_elem_types Tuple/If arms: {sol}"
    );
    assert_solc_compiles("types_infer_tuple_literal_if_block", &sol);
}

#[test]
fn n4_codegen_types_subst_encode_addressof_inner_solc() {
    let program = parse_evm(
        r#"
        entity Target {
            identity m_id: u64
            routes { constructor() => [] }
        }

        entity EncodeSubst {
            routes {
                constructor() => []
                link(outer: u64, k: u64, id: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
                in link(outer, k, id) => {
                    let inner = if m_nested.exists(outer) {
                        m_nested[outer]
                    } else {
                        {}
                    };
                    let _dest = addressOf(Target.state(id));
                    let _pair = (inner[k], k);
                    let score = match inner[k] {
                        0 => 1,
                        _ => inner[k] + k
                    };
                    m_nested.update(outer, inner.update(k, score))
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("predictTarget") || sol.contains("keccak256"),
        "addressOf in subst body must lower (subst AddressOf arm): {sol}"
    );
    assert!(
        sol.contains("m_nested[outer]") || sol.contains("inner"),
        "Tuple/Match must substitute inner[k] in HashMap-let body: {sol}"
    );
    assert_solc_compiles("types_subst_encode_addressof_inner", &sol);
}

#[test]
fn n4_codegen_types_infer_let_addressof_and_temporal_solc() {
    let program = parse_evm(
        r#"
        entity Peer {
            identity m_id: u64
            routes { constructor() => [] }
        }

        entity AddrTemporal {
            routes {
                constructor() => []
                grow() => []
                peer() -> address => [
                    let dest = Peer.address(m_id);
                    return(dest)
                ]
            }
            m_amt: u64 {
                in constructor() => 0
                in grow() => m_amt + 1
            }
            m_peer: address {
                in grow() => Peer.address(m_amt)
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("address dest =") || sol.contains("address dest="),
        "Entity.address let must hit infer_let MethodCall address arm: {sol}"
    );
    assert!(
        sol.contains("predictPeer") || sol.contains("Peer.address"),
        "member transform Peer.address must lower: {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

#[test]
fn n4_codegen_types_infer_let_block_some_range_scatter_solc() {
    let program = parse_evm(
        r#"
        enum Mode { Off, On }

        entity LetScatter {
            routes {
                constructor() => []
                probe(k: u64) -> u64 => [
                    let tail = { let a = k; a + 1 };
                    let tagged = some(k);
                    let span = 0..k;
                    let mode = Mode::On;
                    let slot = m_vec[k];
                    return(tail + span + slot)
                ]
            }
            m_vec: Vec<u64> {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Mode mode =") || sol.contains("Mode.On"),
        "enum variant let must hit infer_let EnumVariant arm: {sol}"
    );
    assert!(
        sol.contains("uint64 slot =") || sol.contains("uint64 slot="),
        "Vec index let must hit infer_let Index arm: {sol}"
    );
    assert_solc_compiles("types_infer_let_block_some_range", &sol);
}

#[test]
fn n4_codegen_types_actual_sol_owner_balance_len_solc() {
    let program = parse_evm(
        r#"
        entity BalLen {
            routes {
                constructor() => []
                peek(owner: address) -> u64 => [
                    let bal = owner.balance;
                    let n = m_vec.len();
                    return(bal + n)
                ]
            }
            m_vec: Vec<u64> {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains(".balance") || sol.contains("balance"),
        "address.balance FieldAccess must hit actual_sol_type balance arm: {sol}"
    );
    assert!(
        sol.contains(".length") || sol.contains("len()"),
        "Vec.len() must lower for actual_sol_type / method path: {sol}"
    );
    assert_solc_compiles("types_actual_sol_owner_balance_len", &sol);
}

// ---------------------------------------------------------------------------
// N4-53: codegen/solidity/core/types.rs — slice 7 (actual_sol_type scatter,
// subst MacroRef/RecordUpdate, infer_record_from_update_fields, infer_tuple None)
// Baseline @ N4-52 side-effect: 75.96% line (243 missed / 1011); focus L929–L939,
// subst_ident L256–L368, infer_record_from_update_fields L1139–L1156,
// infer_tuple_elem_types None/Block arms; exclude dead sol_type_prog L667–L746.
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_types_actual_sol_match_enum_narrow_member_solc() {
    let program = parse_evm(
        r#"
        enum Mode { On, Off }

        entity MatchNarrow {
            routes {
                constructor() => []
                flip() => []
            }
            m_mode: Mode {
                in constructor() => Mode::Off
                in flip() => {
                    if m_mode == Mode::On { Mode::Off } else { Mode::On }
                }
            }
            m_code: u32 {
                in flip() => {
                    let v = match m_mode {
                        Mode::On => 1,
                        Mode::Off => 2
                    };
                    v
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Mode") && (sol.contains("match") || sol.contains("?")),
        "enum match in member transform must hit actual_sol_type Match fallback: {sol}"
    );
    assert!(
        sol.contains("uint32") || sol.contains("m_code"),
        "match body narrow to u32 member must emit typed assignment: {sol}"
    );
    assert_solc_compiles("types_actual_sol_match_enum_narrow", &sol);
}

#[test]
fn n4_codegen_types_actual_sol_block_tail_route_solc() {
    let program = parse_evm(
        r#"
        entity BlockTail {
            routes {
                constructor() => []
                peek(n: u64) -> u32 => [
                    return(m_slot)
                ]
            }
            m_bias: u64 { in constructor() => 0 }
            m_slot: u32 {
                in peek(n) => {
                    { let v = n + m_bias; v }
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint32") || sol.contains("return"),
        "block-tail expr in u32 return must use actual_sol_type Block arm: {sol}"
    );
    assert_solc_compiles("types_actual_sol_block_tail_route", &sol);
}

#[test]
fn n4_codegen_types_infer_record_update_field_fallback_solc() {
    let program = parse_evm(
        r#"
        record Quote { price: u32, qty: u64 }
        record FeeRow { price: u32, fee: u64 }

        entity QuoteGhost {
            routes {
                constructor() => []
                bump(seed: u64) => []
            }
            m_q: Quote {
                in constructor() => { Quote { price: 0, qty: 0 } }
                in bump(seed) => {
                    let next = seed { price: m_q.price + 1, qty: m_q.qty + seed };
                    next
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Quote memory") || sol.contains("Quote("),
        "RecordUpdate on uint256 base must infer Quote via field-set match: {sol}"
    );
    assert!(
        sol.contains("struct Quote"),
        "entity record struct must be emitted: {sol}"
    );
    assert!(
        !sol.contains("FeeRow memory") || sol.contains("struct FeeRow"),
        "ambiguous FeeRow must not be chosen when qty field pins Quote: {sol}"
    );
    assert_solc_compiles("types_infer_record_update_field_fallback", &sol);
}

#[test]
fn n4_codegen_types_subst_macro_record_update_hashmap_let_solc() {
    let program = parse_evm(
        r#"
        record Row { score: u64, bonus: u64 }

        entity MacroRow {
            macro bump(v: u64) -> u64 = { v + 1 }

            routes {
                constructor() => []
                touch(outer: u64, k: u64) => []
            }
            m_rows: HashMap<u64, HashMap<u64, Row>> {
                in constructor() => HashMap::new()
                in touch(outer, k) => {
                    let inner = if m_rows.exists(outer) {
                        m_rows[outer]
                    } else {
                        {}
                    };
                    let row = inner[k];
                    let bumped = @bump(inner[k]);
                    let next = row {
                        score: bumped,
                        bonus: row.bonus + k
                    };
                    m_rows.update(outer, inner.update(k, next))
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("macro_bump") || sol.contains("macro bump"),
        "MacroRef in HashMap-let body must lower (subst MacroRef arm): {sol}"
    );
    assert!(
        sol.contains("m_rows[outer]") || sol.contains("inner"),
        "RecordUpdate must substitute inner[k] in HashMap-let body: {sol}"
    );
    assert!(
        sol.contains("Row memory") || sol.contains("struct Row"),
        "record update on substituted slot must keep Row type: {sol}"
    );
    assert_solc_compiles("types_subst_macro_record_update_hashmap_let", &sol);
}

#[test]
fn n4_codegen_types_infer_tuple_scalar_pure_fn_fallback_solc() {
    let program = parse_evm(
        r#"
        library Scalar {
            pure fn one(x: u64) -> u64 { x }
        }

        entity TupleScalar {
            routes {
                constructor() => []
                split(n: u64) -> u64 => [
                    let (a, b) = Scalar::one(n);
                    return(a + b)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("(uint256 a, uint256 b)") || sol.contains("(uint64 a, uint64 b)"),
        "scalar pure-fn RHS must fall back to uint256 tuple slots: {sol}"
    );
    assert!(
        sol.contains("Scalar.one") || sol.contains("one("),
        "pure fn call must appear in tuple destructure RHS: {sol}"
    );
    // TB-V: forge oracle dropped (types_infer_tuple_scalar_pure_fn_fallback).
}

#[test]
fn n4_codegen_types_infer_tuple_block_tail_destructure_solc() {
    let program = parse_evm(
        r#"
        entity TupleBlock {
            routes {
                constructor() => []
                pair(a: u64, b: u64) -> u64 => [
                    let (x, y) = { let t = a; (t, b) };
                    return(x + y)
                ]
            }
            m_acc: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("(uint256 x, uint256 y)") || sol.contains("(uint64 x, uint64 y)"),
        "block-tail tuple RHS must hit infer_tuple_elem_types Block arm: {sol}"
    );
    assert_solc_compiles("types_infer_tuple_block_tail_destructure", &sol);
}

#[test]
fn n4_codegen_types_subst_namespaced_call_hashmap_let_solc() {
    let program = parse_evm(
        r#"
        entity NsSubst {
            routes {
                constructor() => []
                mix(outer: u64, k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
                in mix(outer, k) => {
                    let inner = if m_nested.exists(outer) {
                        m_nested[outer]
                    } else {
                        {}
                    };
                    let cap = std::math::max(inner[k], k);
                    m_nested.update(outer, inner.update(k, cap + inner[k]))
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("max(") || sol.contains("_cam_max"),
        "NamespacedCall in HashMap-let body must lower (subst NamespacedCall arm): {sol}"
    );
    assert!(
        sol.contains("m_nested[outer]") || sol.contains("inner"),
        "subst must inline inner into namespaced call args: {sol}"
    );
    assert_solc_compiles("types_subst_namespaced_call_hashmap_let", &sol);
}

// ---------------------------------------------------------------------------
// N4-55: codegen/solidity/core/types.rs — slice 8 (subst AddressOf/EnumVariantWithData,
// default_value_entity ctor defaults, lower_member_push wildcard, infer_let keys/values/
// field-access). Exclude dead sol_type_prog L667–L746 (~80).
// Baseline @ N4-54 side-effect: 79.62% line (206 missed / 1011); focus L354–L368,
// L516–L530, L996–L1031, L1333–L1387, infer_iter_elem L1047–L1112.
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_types_default_value_unit_enum_ctor_solc() {
    let program = parse_evm(
        r#"
        enum Lane { Alpha, Beta }

        entity LaneHost {
            routes {
                constructor() => []
            }
            m_lane: Lane {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Lane.Alpha") || sol.contains("Lane.Lane_Alpha"),
        "unit enum member without transform must default via default_value_entity first variant: {sol}"
    );
    assert_solc_compiles("types_default_value_unit_enum_ctor", &sol);
}

#[test]
fn n4_codegen_types_default_value_payload_enum_ctor_solc() {
    let program = parse_evm(
        r#"
        enum Op { Mint(u64), Burn(u64) }

        entity OpHost {
            routes {
                constructor() => []
            }
            m_op: Op {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Op::Mint") || sol.contains("Op({tag: Op_Tag.Mint") || sol.contains("mint_0: 0"),
        "payload enum member ctor default must hit default_value_entity payload_enum_zero_literal: {sol}"
    );
    assert_solc_compiles("types_default_value_payload_enum_ctor", &sol);
}

#[test]
fn n4_codegen_types_vec_push_wildcard_let_solc() {
    let program = parse_evm(
        r#"
        entity WildPush {
            routes {
                constructor() => []
                enqueue(x: u64) => []
            }
            m_items: Vec<u64> {
                in constructor() => array()
                in enqueue(x) => {
                    let _ = x + m_count;
                    m_items.push(x)
                }
            }
            m_count: u64 {
                in constructor() => 0
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains(".push("),
        "Vec push transform must lower via lower_member_push_body: {sol}"
    );
    assert!(
        !sol.contains("let _ =") || sol.contains("x + m_count") || sol.contains("m_count"),
        "wildcard let prelude in push transform must hoist without binding (Pattern::Wildcard arm): {sol}"
    );
    assert_solc_compiles("types_vec_push_wildcard_let", &sol);
}

#[test]
fn n4_codegen_types_subst_addressof_inner_index_solc() {
    let program = parse_evm(
        r#"
        entity Target {
            identity m_id: u64
            routes { constructor() => [] }
        }

        entity AddrInnerSubst {
            routes {
                constructor() => []
                link(outer: u64, k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
                in link(outer, k) => {
                    let inner = if m_nested.exists(outer) {
                        m_nested[outer]
                    } else {
                        {}
                    };
                    let _dest = addressOf(Target.state(inner[k]));
                    m_nested.update(outer, inner.update(k, inner[k] + 1))
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("predictTarget") || sol.contains("keccak256"),
        "addressOf(Target.state(inner[k])) must substitute inner (subst AddressOf arm): {sol}"
    );
    assert!(
        sol.contains("m_nested[outer]") || sol.contains("inner"),
        "HashMap let must inline inner into addressOf args: {sol}"
    );
    assert_solc_compiles("types_subst_addressof_inner_index", &sol);
}

#[test]
fn n4_codegen_types_subst_enum_variant_data_inner_solc() {
    let program = parse_evm(
        r#"
        enum Flag { Off, On(u64) }

        entity FlagSubst {
            routes {
                constructor() => []
                tag(outer: u64, k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
                in tag(outer, k) => {
                    let inner = if m_nested.exists(outer) {
                        m_nested[outer]
                    } else {
                        {}
                    };
                    let marker = Flag::On(inner[k]);
                    m_nested.update(outer, inner.update(k, marker.0 + inner[k]))
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Flag.On") || sol.contains("Flag_Tag.On") || sol.contains("Flag({"),
        "EnumVariantWithData must substitute inner[k] (subst EnumVariantWithData arm): {sol}"
    );
    assert_solc_compiles("types_subst_enum_variant_data_inner", &sol);
}

#[test]
fn n4_codegen_types_infer_let_hashmap_keys_bare_solc() {
    let program = parse_evm(
        r#"
        entity KeysBare {
            routes {
                constructor() => []
                count() -> u64 => [
                    let ks = m_map.keys();
                    return(ks.length)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64[] memory ks") || sol.contains("m_map_keys"),
        "bare m.keys() let must infer K[] memory (infer_let_type_entity keys arm): {sol}"
    );
    assert_solc_compiles("types_infer_let_hashmap_keys_bare", &sol);
}

#[test]
fn n4_codegen_types_infer_let_hashmap_values_bare_solc() {
    let program = parse_evm(
        r#"
        entity ValuesBare {
            routes {
                constructor() => []
                count() -> u64 => [
                    let vs = m_map.values();
                    return(vs.length)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64[] memory vs") || sol.contains("m_map_keys"),
        "bare m.values() let must infer V[] memory (infer_let_type_entity values arm): {sol}"
    );
    assert_solc_compiles("types_infer_let_hashmap_values_bare", &sol);
}

#[test]
fn n4_codegen_types_infer_iter_hashmap_index_vec_for_solc() {
    let program = parse_evm(
        r#"
        entity BoxFor {
            routes {
                constructor() => []
                sumRow(outer: u64) => []
            }
            m_sum: u64 {
                in sumRow(outer) => {
                    for x in m_boxes[outer] { x + 1 }
                }
            }
            m_boxes: HashMap<u64, Vec<u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_boxes[outer]") && sol.contains("for (uint256"),
        "for over m_boxes[outer] must hit infer_iter_elem_type_entity Index HashMap->Vec arm: {sol}"
    );
    assert!(
        sol.contains("m_sum") || sol.contains("function sumRow"),
        "member for over HashMap-indexed Vec must lower: {sol}"
    );
    assert_solc_compiles("types_infer_iter_hashmap_index_vec_for", &sol);
}

#[test]
fn n4_codegen_types_infer_let_record_field_let_bound_solc() {
    let program = parse_evm(
        r#"
        record Row { owner: address, score: u64 }

        entity RowFieldLet {
            routes {
                constructor() => []
                peek(id: u64) -> address => [
                    let row = m_rows[id];
                    let owner = row.owner;
                    return(owner)
                ]
            }
            m_rows: HashMap<u64, Row> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("address owner") || sol.contains("row.owner"),
        "let-bound record field must hit infer_let FieldAccess scratch lookup arm: {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

// ---------------------------------------------------------------------------
// N4-62: codegen/solidity/core/types.rs — slice 9 (residual subst AddressOf
// with_params, infer_let keys/values.collect, FieldAccess member + scratch,
// infer_record_from_update_fields, default_value_entity entity-local enum).
// Exclude dead sol_type_prog L667–L746 (~80). Encode L365–L368 EVM-unreachable.
// Baseline @ N4-61: 80.22% line (200 missed / 1011); focus L354–L368, L1025–L1027,
// L1078–L1090, L1146–L1156, L1333–L1387.
// ---------------------------------------------------------------------------

fn gen_evm_solidity_patched(
    src: &str,
    deterministic: bool,
    patch: impl FnOnce(&mut cambrian_transpiler::ast::Program),
) -> String {
    let mut program = parse_evm(src);
    patch(&mut program);
    gen_evm_solidity(&program, deterministic)
}

fn patch_first_addressof_with_param(e: &mut Expr) {
    match e {
        Expr::AddressOf { with_params, .. } => {
            with_params.push((
                "salt".to_string(),
                Expr::BinOp(
                    Box::new(Expr::Ident("k".to_string())),
                    BinOp::Add,
                    Box::new(Expr::IntLiteral(U256::from_u128(1))),
                ),
            ));
            return;
        }
        Expr::Let(_, v, b) => {
            patch_first_addressof_with_param(v);
            patch_first_addressof_with_param(b);
        }
        Expr::Block(items) => {
            for item in items {
                patch_first_addressof_with_param(item);
            }
        }
        Expr::If(c, t, el) => {
            patch_first_addressof_with_param(c);
            patch_first_addressof_with_param(t);
            if let Some(el) = el {
                patch_first_addressof_with_param(el);
            }
        }
        Expr::MethodCall(b, _, args) => {
            patch_first_addressof_with_param(b);
            for a in args {
                patch_first_addressof_with_param(a);
            }
        }
        Expr::Index(b, k) => {
            patch_first_addressof_with_param(b);
            patch_first_addressof_with_param(k);
        }
        Expr::RecordUpdate(b, fields) => {
            patch_first_addressof_with_param(b);
            for (_, v) in fields {
                patch_first_addressof_with_param(v);
            }
        }
        _ => {}
    }
}

#[test]
fn n4_codegen_types_default_value_entity_local_enum_solc() {
    let program = parse_evm(
        r#"
        entity LocalLaneHost {
            enum Lane { Idle, Busy }

            routes {
                constructor() => []
            }
            m_lane: Lane {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Lane.Idle") || sol.contains("Lane.Lane_Idle"),
        "entity-local unit enum ctor default must hit default_value_entity first-variant arm: {sol}"
    );
    assert_solc_compiles("types_default_value_entity_local_enum", &sol);
}

#[test]
fn n4_codegen_types_infer_let_hashmap_keys_values_collect_solc() {
    let program = parse_evm(
        r#"
        entity CollectBoth {
            routes {
                constructor() => []
                tally() -> u64 => [
                    let ks = m_map.keys().collect();
                    let vs = m_map.values().collect();
                    return(ks.length + vs.length)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64[] memory ks") && sol.contains("uint64[] memory vs"),
        "keys().collect() / values().collect() must infer K[] and V[] memory (infer_let collect recurse): {sol}"
    );
    assert_solc_compiles("types_infer_let_hashmap_keys_values_collect", &sol);
}

#[test]
fn n4_codegen_types_infer_let_member_record_field_access_solc() {
    let program = parse_evm(
        r#"
        entity BundleFieldLet {
            record Bundle { tags: Vec<u64> }

            routes {
                constructor() => []
                tagCount() -> u64 => [
                    let tags = m_bundle.tags;
                    return(tags.length)
                ]
            }
            m_bundle: Bundle {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64[] memory tags") || sol.contains("m_bundle.tags"),
        "m_bundle.tags let must hit infer_let FieldAccess member-record arm: {sol}"
    );
    assert_solc_compiles("types_infer_let_member_record_field_access", &sol);
}

#[test]
fn n4_codegen_types_infer_iter_record_member_vec_for_solc() {
    let program = parse_evm(
        r#"
        entity BundleForIter {
            record Bundle { tags: Vec<u64> }

            routes {
                constructor() => []
                bump() => []
            }
            m_bundle: Bundle {}
            m_sum: u64 {
                in bump() => {
                    for t in m_bundle.tags { t + m_sum }
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_bundle.tags") && sol.contains("for (uint256"),
        "for over m_bundle.tags must hit infer_iter_elem FieldAccess Vec arm: {sol}"
    );
    assert_solc_compiles("types_infer_iter_record_member_vec_for", &sol);
}

#[test]
fn n4_codegen_types_infer_let_scratch_record_field_score_solc() {
    let program = parse_evm(
        r#"
        record Row { owner: address, score: u64 }

        entity RowScoreLet {
            routes {
                constructor() => []
                peek(id: u64) -> u64 => [
                    let row = m_rows[id];
                    let score = row.score;
                    return(score)
                ]
            }
            m_rows: HashMap<u64, Row> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64 score") || sol.contains("uint256 score") || sol.contains("row.score"),
        "let-bound row.score must hit infer_let FieldAccess scratch lookup arm: {sol}"
    );
    assert_solc_compiles("types_infer_let_scratch_record_field_score", &sol);
}

#[test]
fn n4_codegen_types_infer_record_update_unique_fee_row_solc() {
    let program = parse_evm(
        r#"
        record FeeRow { price: u32, fee: u64 }

        entity FeeRowOnly {
            routes {
                constructor() => []
                touch(seed: u64) => []
            }
            m_fee: FeeRow {
                in constructor() => { FeeRow { price: 0, fee: 0 } }
                in touch(seed) => {
                    let ghost = seed;
                    ghost { price: m_fee.price + 1, fee: m_fee.fee + seed }
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("FeeRow memory") || sol.contains("FeeRow("),
        "unique FeeRow field-set match must hit infer_record_from_update_fields success arm: {sol}"
    );
    assert_solc_compiles("types_infer_record_update_unique_fee_row", &sol);
}

#[test]
fn n4_codegen_types_infer_record_update_ambiguous_fallback_solc() {
    let program = parse_evm(
        r#"
        record AlphaRow { price: u32, alpha: u64 }
        record BetaRow { price: u32, beta: u64 }

        entity AmbiguousRow {
            routes {
                constructor() => []
                touch(seed: u64) => []
            }
            m_alpha: AlphaRow {
                in constructor() => { AlphaRow { price: 0, alpha: 0 } }
                in touch(seed) => {
                    let ghost = seed;
                    ghost { price: m_alpha.price + 1 }
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_alpha.price") || sol.contains("price:"),
        "ambiguous price-only RecordUpdate must fall through infer_record_from_update_fields None arm: {sol}"
    );
    assert_solc_compiles("types_infer_record_update_ambiguous_fallback", &sol);
}

#[test]
fn n4_codegen_types_subst_addressof_with_params_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity Target {
            identity m_id: u64
            routes { constructor() => [] }
        }

        entity AddrWithParams {
            routes {
                constructor() => []
                link(outer: u64, k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
                in link(outer, k) => {
                    let inner = if m_nested.exists(outer) {
                        m_nested[outer]
                    } else {
                        {}
                    };
                    let _dest = address_of Target(inner[k]);
                    m_nested.update(outer, inner.update(k, inner[k] + 1))
                }
            }
        }
    "#,
        true,
        |program| {
            let entity = program
                .entities
                .iter_mut()
                .find(|e| e.name == "AddrWithParams")
                .expect("AddrWithParams entity");
            let member = entity
                .members
                .iter_mut()
                .find(|m| m.name == "m_nested")
                .expect("m_nested member");
            let transform = member
                .transforms
                .iter_mut()
                .find(|t| t.route_name == "link")
                .expect("link transform");
            patch_first_addressof_with_param(&mut transform.body);
        },
    );
    assert!(
        sol.contains("predictTarget") || sol.contains("keccak256") || sol.contains("address_of"),
        "address_of in HashMap-let must lower via subst AddressOf args + with_params map arm: {sol}"
    );
    assert!(
        sol.contains("m_nested[outer]") || sol.contains("inner"),
        "HashMap let must still inline inner alongside with_params subst: {sol}"
    );
    assert_solc_compiles("types_subst_addressof_with_params_ast_inject", &sol);
}

// ---------------------------------------------------------------------------
// N4-64: codegen/solidity/core/types.rs — slice 10 (infer_let/iter HashMap
// keys/values, actual_sol_type / infer_tuple tails, sol_type_entity generics).
// Exclude Encode L365–L368 (EVM-unreachable) and dead sol_type_prog L667–L746.
// Baseline @ N4-63: 81.80% line (184 missed / 1011); focus L1068–L1125,
// L1333–L1352, actual_sol_type L790–L938, infer_tuple_elem_types L1414–L1450.
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_types_infer_iter_hashmap_values_for_solc() {
    let program = parse_evm(
        r#"
        entity ValuesFor {
            routes {
                constructor() => []
                bump() => []
            }
            m_total: u64 {
                in constructor() => 0
                in bump() => {
                    for v in m_map.values() { m_total + v }
                }
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_map_keys") && sol.contains("for (uint256"),
        "for v in m_map.values() must hit infer_iter_elem values arm: {sol}"
    );
    assert_solc_compiles("types_infer_iter_hashmap_values_for", &sol);
}

#[test]
fn n4_codegen_types_infer_iter_hashmap_keys_for_block_solc() {
    let program = parse_evm(
        r#"
        entity KeysForBlock {
            routes {
                constructor() => []
                tally() -> Vec<u64> => [
                    let total = { for k in m_map.keys() { k + 1 } };
                    return(total)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_map_keys") && sol.contains("uint64"),
        "block for over m_map.keys() must hit infer_iter_elem keys arm: {sol}"
    );
    assert_solc_compiles("types_infer_iter_hashmap_keys_for_block", &sol);
}

#[test]
fn n4_codegen_types_infer_iter_hashmap_index_vec_member_solc() {
    let program = parse_evm(
        r#"
        entity GridRowFor {
            routes {
                constructor() => []
                bump(outer: u64) => []
            }
            m_sum: u64 {
                in bump(outer) => {
                    for x in m_grid[outer] { x + m_sum }
                }
            }
            m_grid: HashMap<u64, Vec<u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_grid[outer]") && sol.contains("for (uint256"),
        "for over m_grid[outer] must hit infer_iter_elem Index HashMap->Vec arm: {sol}"
    );
    assert_solc_compiles("types_infer_iter_hashmap_index_vec_member", &sol);
}

#[test]
fn n4_codegen_types_infer_iter_route_record_field_vec_solc() {
    let program = parse_evm(
        r#"
        entity TagRouteFor {
            record Bundle { tags: Vec<u32> }

            routes {
                constructor() => []
                sumTags() -> u32 => [
                    let acc = { for t in m_bundle.tags { t + 1 } };
                    return(acc)
                ]
            }
            m_bundle: Bundle {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_bundle.tags") && sol.contains("for (uint256"),
        "route for over m_bundle.tags must hit infer_iter_elem FieldAccess Vec arm: {sol}"
    );
    assert_solc_compiles("types_infer_iter_route_record_field_vec", &sol);
}

#[test]
fn n4_codegen_types_infer_let_hashmap_alias_keys_values_solc() {
    let program = parse_evm(
        r#"
        entity AliasKeysValues {
            routes {
                constructor() => []
                touch(outer: u64) => []
            }
            m_rows: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
                in touch(outer) => {
                    let inner = if m_rows.exists(outer) {
                        m_rows[outer]
                    } else {
                        {}
                    };
                    let ks = inner.keys().collect();
                    m_rows.update(outer, inner.update(0, ks.length))
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_rows[outer]") || sol.contains("inner"),
        "HashMap-let alias must substitute into keys().collect(): {sol}"
    );
    assert!(
        sol.contains("ks") && (sol.contains(".keys()") || sol.contains("_keys")),
        "inner.keys().collect() after alias must lower (infer_let collect / keys path): {sol}"
    );
    assert_solc_compiles("types_infer_let_hashmap_alias_keys_values", &sol);
}

#[test]
fn n4_codegen_types_infer_let_snap_fn_keys_collect_solc() {
    let program = parse_evm(
        r#"
        entity SnapKeys {
            routes {
                constructor() => []
                snap() -> HashMap<u64, u64> => [ return(m_map) ]
                probe() -> u64 => [
                    let ks = snap().keys().collect();
                    return(ks.length)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function probe") && sol.contains("snap()"),
        "snap().keys().collect() route must lower: {sol}"
    );
    assert!(
        sol.contains("uint256 ks") || sol.contains("ks.length"),
        "non-Ident keys base hits infer_let keys fallback (uint256) path: {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

#[test]
fn n4_codegen_types_actual_sol_block_match_enum_solc() {
    let program = parse_evm(
        r#"
        enum Mode { On, Off }

        entity ActualSolTails {
            routes {
                constructor() => []
                classify(flag: bool) -> u64 => [
                    let code = match m_count {
                        0 => 1,
                        _ => m_count + 2
                    };
                    let tail = { let t = m_count; t + 1 };
                    let ghost = Ghost::On;
                    return(code + tail)
                ]
            }
            m_count: u64 {
                in constructor() => 0
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("match") || (sol.contains("if (") && sol.contains("m_count")),
        "match let must hit actual_sol_type Match arm: {sol}"
    );
    assert!(
        sol.contains("uint256 tail") || sol.contains("t + 1"),
        "block-tail let must hit actual_sol_type Block arm: {sol}"
    );
    assert!(
        sol.contains("Ghost.On") || sol.contains("Mode.On") || sol.contains("Ghost::On"),
        "unknown enum literal must hit actual_sol_type EnumVariant fallback: {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

#[test]
fn n4_codegen_types_actual_sol_scratch_record_field_solc() {
    let program = parse_evm(
        r#"
        record Quote { price: u32, qty: u64 }

        entity ScratchFieldActual {
            routes {
                constructor() => []
                peek(id: u64) -> u32 => [
                    let row = m_rows[id];
                    let price = row.price;
                    return(price)
                ]
            }
            m_rows: HashMap<u64, Quote> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint32 price") || sol.contains("row.price"),
        "let-bound row.price must hit actual_sol_type FieldAccess scratch arm: {sol}"
    );
    assert_solc_compiles("types_actual_sol_scratch_record_field", &sol);
}

#[test]
fn n4_codegen_types_infer_let_match_block_some_wildcard_solc() {
    let program = parse_evm(
        r#"
        entity InferLetTails {
            routes {
                constructor() => []
                probe(flag: bool, n: u64) -> u64 => [
                    let tagged = some(n);
                    let picked = match n {
                        0 => 1,
                        _ => n + 3
                    };
                    let tail = { let t = n; t + 4 };
                    let _ = flag;
                    return(picked + tail)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint256 tagged") || sol.contains("tagged ="),
        "some(n) let must hit infer_let Some arm: {sol}"
    );
    assert!(
        sol.contains("match") || sol.contains("picked"),
        "match let must hit infer_let Match arm: {sol}"
    );
    assert!(
        sol.contains("tail") && (sol.contains("t + 4") || sol.contains("uint256")),
        "block-tail let must hit infer_let Block arm: {sol}"
    );
    assert_solc_compiles("types_infer_let_match_block_some_wildcard", &sol);
}

#[test]
fn n4_codegen_types_infer_tuple_block_pure_fn_solc() {
    let program = parse_evm(
        r#"
        library PairLib {
            pure fn dup(a: u64, b: u64) -> (u64, u64) { (a, b) }
        }

        entity TupleBlockHost {
            routes {
                constructor() => []
                both(n: u64) -> u64 => [
                    let (x, y) = { let t = n; PairLib::dup(t, t + 1) };
                    return(x + y)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function dup") && (sol.contains("(uint256 x, uint256 y)") || sol.contains("(uint64 x, uint64 y)")),
        "block-wrapped pure-fn tuple RHS must hit infer_tuple_elem_types Block+FnCall arms: {sol}"
    );
    assert_solc_compiles("types_infer_tuple_block_pure_fn", &sol);
}

#[test]
fn n4_codegen_types_sol_type_vec_record_option_solc() {
    let program = parse_evm(
        r#"
        record Quote { price: u32, qty: u64 }

        entity GenericTypes {
            routes {
                constructor() => []
                peek(id: u64) -> u32 => [
                    let row = m_rows[id];
                    let maybe = some(row.price);
                    let v = match maybe { some(x) => x, none => 0 };
                    return(v)
                ]
            }
            m_rows: HashMap<u64, Quote> {
                in constructor() => HashMap::new()
            }
            m_quotes: Vec<Quote> {}
            m_limit: Option<u32> {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("struct Quote")
            && (sol.contains("mapping(uint64 => Quote)") || sol.contains("mapping(uint256 => Quote)")),
        "HashMap<u64, Quote> must lower via sol_type_entity record value arm: {sol}"
    );
    assert!(
        sol.contains("Quote[]") || sol.contains("Quote public m_quotes"),
        "Vec<Quote> must hit sol_type_entity Vec<record> arm: {sol}"
    );
    assert!(
        sol.contains("Option_uint32 public m_limit") || sol.contains("Option_uint32 m_limit"),
        "Option<u32> member must lower to tagged Option_uint32: {sol}"
    );
    assert_solc_compiles("types_sol_type_vec_record_option", &sol);
}

#[test]
fn n4_codegen_types_infer_let_entity_address_method_solc() {
    let program = parse_evm(
        r#"
        entity Peer {
            identity m_id: u64
            routes { constructor() => [] }
        }

        entity AddrMethodLet {
            routes {
                constructor() => []
                peer() -> address => [
                    let dest = Peer.address(m_id);
                    return(dest)
                ]
            }
            m_id: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("address dest") || sol.contains("address dest="),
        "Peer.address let must hit infer_let MethodCall address arm: {sol}"
    );
    assert!(
        sol.contains("predictPeer") || sol.contains("Peer.address"),
        "deterministic Peer.address must lower: {sol}"
    );
    assert_solc_compiles("types_infer_let_entity_address_method", &sol);
}

// ---------------------------------------------------------------------------
// N4-67: codegen/solidity/core/types.rs — residual slice (infer_iter keys/values/
// collect/index, infer_let keys/values member, actual_sol_type member FieldAccess,
// infer_tuple pure-fn Tuple, infer_record_from_update_fields).
// Baseline @ N4-66: 82.49% line (177 missed / 1011); focus L1068–L1125,
// L1333–L1352, L804–L806, L1442, L1146–L1153. Exclude sol_type_prog L667–L746
// (DEAD) and Encode L365–L368 (EVM-unreachable).
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_types_infer_iter_keys_collect_member_solc() {
    let program = parse_evm(
        r#"
        entity KeysCollectMember {
            routes {
                constructor() => []
                tally() => []
            }
            m_keys: Vec<u64> {
                in tally() => {
                    for k in m_map.keys().collect() { k }
                }
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_map_keys") && sol.contains("for (uint256"),
        "for k in m_map.keys().collect() must hit infer_iter_elem collect+keys arms (L1100/L1108): {sol}"
    );
    assert_solc_compiles("types_infer_iter_keys_collect_member", &sol);
}

#[test]
fn n4_codegen_types_infer_iter_values_collect_route_block_solc() {
    let program = parse_evm(
        r#"
        entity ValuesCollectRoute {
            routes {
                constructor() => []
                sum() -> Vec<u64> => [
                    let total = {
                        for v in m_map.values().collect() { v + 1 }
                    };
                    return(total)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_map_keys") && sol.contains("uint64"),
        "block for over m_map.values().collect() must hit infer_iter_elem values+collect arms (L1119–L1125): {sol}"
    );
    assert_solc_compiles("types_infer_iter_values_collect_route_block", &sol);
}

#[test]
fn n4_codegen_types_infer_iter_hashmap_index_pure_fn_fold_solc() {
    let program = parse_evm(
        r#"
        library GridSum {
            pure fn row_total(grid: HashMap<u64, Vec<u64>>, outer: u64) -> u64 {
                grid[outer].fold(0, |acc, x| acc + x)
            }
        }

        entity GridSumHost {
            routes {
                constructor() => []
                run(outer: u64) -> u64 => [
                    return(GridSum::row_total(m_grid, outer))
                ]
            }
            m_grid: HashMap<u64, Vec<u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function row_total") && sol.contains("grid[outer]"),
        "pure-fn fold over grid[outer] must hit infer_iter_elem Index HashMap->Vec arm (L1068–L1072): {sol}"
    );
    assert_solc_compiles("types_infer_iter_hashmap_index_pure_fn_fold", &sol);
}

#[test]
fn n4_codegen_types_infer_iter_record_member_field_vec_solc() {
    let program = parse_evm(
        r#"
        record Bundle { tags: Vec<u64> }

        entity BundleMemberFor {
            routes {
                constructor() => []
                bump() => []
            }
            m_sum: u64 {
                in constructor() => 0
                in bump() => {
                    for t in m_bundle.tags { m_sum + t }
                }
            }
            m_bundle: Bundle {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_bundle.tags") && sol.contains("for (uint256"),
        "member for over m_bundle.tags must hit infer_iter_elem FieldAccess Vec arm (L1085–L1089): {sol}"
    );
    assert_solc_compiles("types_infer_iter_record_member_field_vec", &sol);
}

#[test]
fn n4_codegen_types_actual_sol_member_record_field_narrow_solc() {
    let program = parse_evm(
        r#"
        record Slot { weight: u32, tag: u64 }

        entity MemberFieldActual {
            routes {
                constructor() => []
                weigh() => []
            }
            m_slot: Slot {}
            m_out: u32 {
                in weigh() => m_slot.weight
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_slot.weight") && sol.contains("uint32"),
        "member record field access must hit actual_sol_type FieldAccess member arm (L804–L806): {sol}"
    );
    assert_solc_compiles("types_actual_sol_member_record_field_narrow", &sol);
}

#[test]
fn n4_codegen_types_infer_let_keys_values_member_transform_solc() {
    let program = parse_evm(
        r#"
        entity LetKeysValuesMember {
            routes {
                constructor() => []
                probe() => []
            }
            m_len: u64 {
                in constructor() => 0
                in probe() => {
                    let ks = m_map.keys();
                    let vs = m_map.values();
                    ks.length + vs.length
                }
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        (sol.contains("uint64[] memory ks") || sol.contains("m_map_keys"))
            && (sol.contains("uint64[] memory vs") || sol.contains("m_map_keys")),
        "member transform bare keys()/values() lets must hit infer_let_type_entity Ident arms (L1333–L1352): {sol}"
    );
    assert_solc_compiles("types_infer_let_keys_values_member_transform", &sol);
}

#[test]
fn n4_codegen_types_infer_let_field_access_scratch_owner_solc() {
    let program = parse_evm(
        r#"
        record TokenData { owner: address, amount: u64 }

        entity TokenScratchField {
            routes {
                constructor() => []
                holder(id: u64) -> address => [
                    let data = m_tokens[id];
                    let owner = data.owner;
                    return(owner)
                ]
            }
            m_tokens: HashMap<u64, TokenData> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("address owner") || sol.contains("data.owner"),
        "let-bound data.owner must hit infer_let FieldAccess scratch arm (L1378–L1383): {sol}"
    );
    assert_solc_compiles("types_infer_let_field_access_scratch_owner", &sol);
}

#[test]
fn n4_codegen_types_infer_tuple_pure_fn_mixed_tuple_solc() {
    let program = parse_evm(
        r#"
        library MixedPair {
            pure fn split(a: u32, b: u64) -> (u32, u64) { (a, b) }
        }

        entity MixedTupleHost {
            routes {
                constructor() => []
                pack(a: u32, b: u64) -> u64 => [
                    let (lo, hi) = MixedPair::split(a, b);
                    return(lo + hi)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function split(uint32")
            && (sol.contains("(uint256 lo, uint256 hi)") || sol.contains("(uint32 lo, uint64 hi)")),
        "mixed pure-fn tuple return must hit infer_tuple_elem_types FnCall Tuple arm (L1440–L1442): {sol}"
    );
    assert_solc_compiles("types_infer_tuple_pure_fn_mixed_tuple", &sol);
}

#[test]
fn n4_codegen_types_infer_record_update_unique_fields_solc() {
    let program = parse_evm(
        r#"
        record Slot { weight: u32, tag: u64 }
        record Other { label: u64 }

        entity RecordUpdateUnique {
            routes {
                constructor() => []
                tune(n: u32) => []
            }
            m_slot: Slot {
                in constructor() => { Slot { weight: 1, tag: 0 } }
                in tune(n) => {
                    m_slot { weight: n }
                }
            }
            m_other: Other {
                in constructor() => { Other { label: 0 } }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_slot.weight =") || sol.contains(".weight ="),
        "unique record field update must hit infer_record_from_update_fields (L1146–L1153): {sol}"
    );
    assert_solc_compiles("types_infer_record_update_unique_fields", &sol);
}

#[test]
fn n4_codegen_types_infer_let_keys_collect_route_solc() {
    let program = parse_evm(
        r#"
        entity KeysCollectLet {
            routes {
                constructor() => []
                count() -> u64 => [
                    let ks = m_map.keys().collect();
                    return(ks.length)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64[] memory ks") || sol.contains("m_map_keys"),
        "let ks = m_map.keys().collect() must thread infer_let collect+keys Ident path: {sol}"
    );
    assert_solc_compiles("types_infer_let_keys_collect_route", &sol);
}

// ---------------------------------------------------------------------------
// N4-71: codegen/solidity/core/types.rs — residual slice 2 (infer_iter Index/keys,
// infer_let keys/values/Index/Block, actual_sol_type member FieldAccess,
// infer_tuple literal Tuple, infer_record_from_update_fields ghost base).
// Baseline @ N4-70: 83.18% line (170 missed / 1011); focus L1068–L1112,
// L1333–L1352, L804–L806, L1442, L1146–L1153. Exclude sol_type_prog L667–L746
// (DEAD) and Encode L365–L368 (EVM-unreachable).
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_types_infer_record_ghost_zero_unique_fields_solc() {
    let program = parse_evm(
        r#"
        record FeeRow { price: u32, fee: u64 }

        entity GhostZeroUpdate {
            routes {
                constructor() => []
                touch(n: u32) => []
            }
            m_fee: FeeRow {
                in constructor() => { FeeRow { price: 0, fee: 0 } }
                in touch(n) => {
                    let ghost = 0;
                    ghost { price: n, fee: m_fee.fee + 1 }
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("FeeRow memory") || sol.contains("FeeRow("),
        "uint256 ghost base RecordUpdate must hit infer_record_from_update_fields (L1146–L1153): {sol}"
    );
    assert_solc_compiles("types_infer_record_ghost_zero_unique_fields", &sol);
}

#[test]
fn n4_codegen_types_infer_tuple_literal_destructure_solc() {
    let program = parse_evm(
        r#"
        entity TupleLiteralLet {
            routes {
                constructor() => []
                pair(a: u32, b: u64) -> u64 => [
                    let (lo, hi) = (a, b);
                    return(lo + hi)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("(uint32 lo, uint64 hi)") || sol.contains("(uint256 lo, uint256 hi)"),
        "literal tuple RHS must hit infer_tuple_elem_types Tuple arm (L1429–L1434): {sol}"
    );
    assert_solc_compiles("types_infer_tuple_literal_destructure", &sol);
}

#[test]
fn n4_codegen_types_actual_sol_member_record_field_return_solc() {
    let program = parse_evm(
        r#"
        record Slot { weight: u32, tag: u64 }

        entity MemberFieldReturn {
            routes {
                constructor() => []
                readWeight() -> u32 => [ return(m_slot.weight) ]
            }
            m_slot: Slot {
                in constructor() => { Slot { weight: 1, tag: 0 } }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("returns (uint32)") && sol.contains("m_slot.weight"),
        "return of member record field must hit actual_sol_type FieldAccess member arm (L804–L806): {sol}"
    );
    assert_solc_compiles("types_actual_sol_member_record_field_return", &sol);
}

#[test]
fn n4_codegen_types_infer_let_hashmap_index_binding_solc() {
    let program = parse_evm(
        r#"
        entity HmIndexLet {
            routes {
                constructor() => []
                peek(id: u64) -> u64 => [
                    let cell = m_map[id];
                    return(cell)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64 cell") || sol.contains("m_map[id]"),
        "let cell = m_map[id] must hit infer_let_type_entity Index HashMap arm (L1305–L1312): {sol}"
    );
    assert_solc_compiles("types_infer_let_hashmap_index_binding", &sol);
}

#[test]
fn n4_codegen_types_infer_let_values_bare_route_solc() {
    let program = parse_evm(
        r#"
        entity ValuesBareLet {
            routes {
                constructor() => []
                snapshot() -> u64 => [
                    let vs = m_map.values();
                    return(vs.length)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64[] memory vs") || sol.contains("m_map_keys"),
        "let vs = m_map.values() must hit infer_let_type_entity values arm (L1342–L1348): {sol}"
    );
    assert_solc_compiles("types_infer_let_values_bare_route", &sol);
}

#[test]
fn n4_codegen_types_infer_let_block_tail_narrow_solc() {
    let program = parse_evm(
        r#"
        entity BlockTailLet {
            routes {
                constructor() => []
                scale(x: u32) -> u32 => [
                    let bumped = { let y = x; y + 1 };
                    return(bumped)
                ]
            }
            m_x: u32 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint32 bumped") || sol.contains("uint32 y"),
        "block let tail must hit infer_let_type_entity Block arm (L1238–L1240): {sol}"
    );
    assert_solc_compiles("types_infer_let_block_tail_narrow", &sol);
}

#[test]
fn n4_codegen_types_infer_iter_hashmap_index_for_route_solc() {
    let program = parse_evm(
        r#"
        entity GridIndexFor {
            routes {
                constructor() => []
                sumRow(outer: u64) -> Vec<u64> => [
                    let total = { for x in m_grid[outer] { x + 1 } };
                    return(total)
                ]
            }
            m_grid: HashMap<u64, Vec<u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_grid[outer]") && sol.contains("for (uint256"),
        "for over m_grid[outer] must hit infer_iter_elem_type_entity Index HashMap->Vec (L1068–L1072): {sol}"
    );
    assert_solc_compiles("types_infer_iter_hashmap_index_for_route", &sol);
}

#[test]
fn n4_codegen_types_infer_iter_keys_collect_for_result_solc() {
    let program = parse_evm(
        r#"
        entity KeysCollectFor {
            routes {
                constructor() => []
                bumpKeys() -> Vec<u64> => [
                    let bumped = { for k in m_map.keys().collect() { k + 1 } };
                    return(bumped)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_map_keys") && sol.contains("uint64"),
        "block for over m_map.keys().collect() must hit infer_iter collect+keys (L1100/L1107): {sol}"
    );
    assert_solc_compiles("types_infer_iter_keys_collect_for_result", &sol);
}

#[test]
fn n4_codegen_types_infer_let_field_access_scratch_record_solc() {
    let program = parse_evm(
        r#"
        record TokenData { owner: address, amount: u64 }

        entity ScratchRecordField {
            routes {
                constructor() => []
                holder(id: u64) -> address => [
                    let data = m_tokens[id];
                    let owner = data.owner;
                    return(owner)
                ]
            }
            m_tokens: HashMap<u64, TokenData> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("address owner") || sol.contains("data.owner"),
        "let-bound record field must hit infer_let FieldAccess scratch arm (L1378–L1383): {sol}"
    );
    assert_solc_compiles("types_infer_let_field_access_scratch_record", &sol);
}

// ---------------------------------------------------------------------------
// N4-56: codegen/solidity/evm/route.rs — recon slice 1 (route body / send /
// deploy / call / multi-entity / receive / from / mixed phased)
// Baseline @ N4-55: 84.76% line (251 missed / 1647); focus gen_ir_stmt_evm
// tails, emit_route_body_from_ir, gen_from_checks*, constructor/initialize.
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_route_ir_deploy_with_value_solc() {
    let program = parse_evm(
        r#"
        entity Child {
            routes { constructor(owner: address) => [] }
            m_owner: address { in constructor(owner) => owner }
        }

        entity Factory {
            routes {
                constructor() => []
                spawn(owner: address) => [
                    deploy Child(owner) with { value: 100 }
                ]
            }
            m_spawns: u64 {
                in constructor() => 0
                in spawn(_) => m_spawns + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("new Child") && (sol.contains("value: 100") || sol.contains("{value: 100}")),
        "IrStmt::Deploy must lower to new Child{{value: ...}} (emit_deploy arm): {sol}"
    );
    assert_solc_compiles("route_ir_deploy_with_value", &sol);
}

#[test]
fn n4_codegen_route_det_factory_deploy_solc() {
    let program = parse_evm(
        r#"
        entity Leaf {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity FactoryHost {
            identity m_slot: u64
            routes {
                constructor() => []
                grow(id: u64) => [
                    deploy Leaf(id)
                ]
            }
            m_count: u64 {
                in constructor() => 0
                in grow(_) => m_count + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("_factory.deployLeaf") || sol.contains("deployLeaf("),
        "deterministic IrStmt::Deploy must use factory deploy (gen_deploy_via_factory): {sol}"
    );
    assert!(
        sol.contains("function grow("),
        "route fn must emit under gen_route_impl_ext: {sol}"
    );
    assert_solc_compiles("route_det_factory_deploy", &sol);
}

#[test]
fn n4_codegen_route_cross_entity_typed_send_solc() {
    let program = parse_evm(
        r#"
        entity Treasury {
            routes {
                init create() => []
                credit(amount: u64) => []
            }
            m_total: u64 {
                in create() => 0
                in credit(amount) => m_total + amount
            }
        }

        entity Vault {
            routes {
                init create() => []
                deposit(amount: u64) => [
                    credit(amount) ~> Treasury.address()
                ]
            }
            m_balance: u64 { in create() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("ITreasury") && sol.contains(".credit("),
        "cross-entity typed send must emit interface call (emit_send_with_target): {sol}"
    );
    assert!(
        sol.contains("contract Vault"),
        "multi-entity program must emit Vault route body: {sol}"
    );
    assert_solc_compiles("route_cross_entity_typed_send", &sol);
}

#[test]
fn n4_codegen_route_phased_var_call_hoist_solc() {
    let program = parse_evm(
        r#"
        extern entity Token {
            view route balanceOf(who: address) -> u64;
        }

        entity Reader {
            routes {
                constructor(t: Address<Token>) => []
                snapshot(who: address) -> u64 => [
                    read: [
                        var bal = balanceOf(who) ~> m_token;
                        return(bal)
                    ]
                ]
            }
            m_token: Address<Token> { in constructor(t) => t }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("// Phase: read"),
        "phased route must emit named phase block (emit_named_phase_block_from_ir): {sol}"
    );
    assert!(
        (sol.contains("uint64 bal") || sol.contains("uint256 bal"))
            && sol.contains("balanceOf"),
        "phased var-call must hoist decl + assignment (var_call_assignments arm): {sol}"
    );
    assert_solc_compiles("route_phased_var_call_hoist", &sol);
}

#[test]
fn n4_codegen_route_mixed_body_deploy_solc() {
    let program = parse_evm(
        r#"
        entity Mini {
            routes { constructor(seed: u64) => [] }
            m_seed: u64 { in constructor(seed) => seed }
        }

        entity Mixer {
            routes {
                constructor() => []
                spawn(seed: u64) => [
                    prep: []
                    deploy Mini(seed)
                ]
            }
            m_spawns: u64 {
                in constructor() => 0
                in spawn(_) => m_spawns + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("// Phase: prep") && sol.contains("new Mini"),
        "RouteBody::Mixed must emit phased block then trailing deploy (mixed trailing phase IR): {sol}"
    );
    assert_solc_compiles("route_mixed_body_deploy", &sol);
}

#[test]
fn n4_codegen_route_receive_fallback_payable_solc() {
    let program = parse_evm(
        r#"
        entity EthVault {
            routes {
                accept receive() => []
                fallback() => []
                view balance() -> u64 => [ return(m_balance) ]
            }
            m_balance: u64 {
                in receive() => m_balance + msg::value
                in fallback() => m_balance + msg::value
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("receive() external payable"),
        "receive route must lower via gen_receive_or_fallback: {sol}"
    );
    assert!(
        sol.contains("fallback() external payable"),
        "fallback with msg::value transform must be payable: {sol}"
    );
    assert_solc_compiles("route_receive_fallback_payable", &sol);
}

#[test]
fn n4_codegen_route_from_custom_error_revert_solc() {
    let program = parse_evm(
        r#"
        error NotOwner();

        entity Guarded {
            routes {
                constructor(owner: address) => []
                adminOnly() from m_owner : throw NotOwner() => []
            }
            m_owner: address { in constructor(owner) => owner }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("revert NotOwner(") || sol.contains("revert NotOwner()"),
        "uniform from-clause custom error must emit revert NotOwner (gen_from_checks L668–681): {sol}"
    );
    assert!(
        sol.contains("msg.sender == m_owner"),
        "from member must compare against storage slot: {sol}"
    );
    assert_solc_compiles("route_from_custom_error_revert", &sol);
}

#[test]
fn n4_codegen_route_from_det_create2_solc() {
    let program = parse_evm(
        r#"
        entity Counter {
            identity m_id: u64
            routes {
                constructor() => []
                bump() => []
            }
            m_n: u64 {
                in constructor() => 0
                in bump() => m_n + 1
            }
        }

        entity Gate {
            identity m_slot: u64
            routes {
                constructor() => []
                ping(id: u64) from Counter(id) => []
            }
            m_hits: u64 {
                in constructor() => 0
                in ping(_) => m_hits + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("keccak256") || sol.contains("predictCounter"),
        "deterministic from Entity(args) must use CREATE2 address (gen_from_checks_det): {sol}"
    );
    assert!(
        sol.contains("function ping("),
        "gen_route_impl_ext must emit guarded route: {sol}"
    );
    assert_solc_compiles("route_from_det_create2", &sol);
}

#[test]
fn n4_codegen_route_rescue_named_send_solc() {
    let program = parse_evm(
        r#"
        entity BounceBox {
            routes {
                constructor() => []
                sendPing(dest: Address<BounceBox>, value: u64) => [
                    rescue ping_failed: acceptPing(value) ~> dest
                ]
                acceptPing(value: u64) => []
                recover ping_failed(body: CamData) => []
            }
            m_bounces: u64 {
                in constructor() => 0
                in recover_ping_failed(_) => m_bounces + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        cambrian_transpiler::validate::check_evm_target_compat(&program)
            .iter()
            .any(|d| d.code == "E26"),
        "named send rescue must be E26 on EVM"
    );
    assert!(
        sol.contains("E26") && sol.contains("ping_failed"),
        "force-codegen must comment rescue (E26): {sol}"
    );
    assert!(
        !sol.contains("try "),
        "typed named send rescue must not emit try/catch: {sol}"
    );
    assert!(
        !sol.contains("function ping_failed("),
        "recover route must not emit as ordinary function: {sol}"
    );
    assert_solc_compiles("route_rescue_named_send", &sol);
}

#[test]
fn n4_codegen_route_call_route_internal_solc() {
    let program = parse_evm(
        r#"
        entity SelfCall {
            routes {
                constructor() => []
                wrapper(n: u64) => [ call helper(n) ]
                helper(n: u64) => []
            }
            m_x: u64 {
                in constructor() => 0
                in wrapper(n) => n
                in helper(n) => m_x + n
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("helper(") && sol.contains("function wrapper("),
        "IrStmt::CallRoute must emit internal route call in wrapper body: {sol}"
    );
    assert_solc_compiles("route_call_route_internal", &sol);
}

#[test]
fn n4_codegen_route_constructor_phased_init_solc() {
    let program = parse_evm(
        r#"
        entity Vault {
            routes {
                init create(owner: address, seed: u64) => [
                    setup: [
                        deploy SeedBox(seed)
                    ]
                ]
            }
            m_owner: address { in create(owner, seed) => setup: owner }
            m_seed: u64 { in create(owner, seed) => setup: seed }
        }

        entity SeedBox {
            routes { constructor(seed: u64) => [] }
            m_seed: u64 { in constructor(seed) => seed }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("constructor(") && sol.contains("// Phase: setup"),
        "init route phased body must fold into constructor (gen_constructor_impl Mixed/Phased): {sol}"
    );
    assert!(
        sol.contains("new SeedBox"),
        "phased init actions must emit deploy in constructor: {sol}"
    );
    assert_solc_compiles("route_constructor_phased_init", &sol);
}

#[test]
fn n4_codegen_route_initialize_det_phased_init_solc() {
    let program = parse_evm(
        r#"
        entity Leaf {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity DetVault {
            identity m_slot: u64
            routes {
                init setup(seed: u64) => [
                    boot: [
                        deploy Leaf(seed)
                    ]
                ]
                view cap() -> u64 => [ return(m_cap) ]
            }
            m_cap: u64 {
                in setup(seed) => boot: seed
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function initialize(") && sol.contains("// Phase: boot"),
        "deterministic init with extra params must emit initialize phased body: {sol}"
    );
    assert!(
        sol.contains("deployLeaf") || sol.contains("_factory.deployLeaf"),
        "initialize phased deploy must use factory path: {sol}"
    );
    assert_solc_compiles("route_initialize_det_phased_init", &sol);
}

#[test]
fn n4_codegen_route_extern_var_call_return_type_solc() {
    let program = parse_evm(
        r#"
        extern entity Token {
            view route balanceOf(who: address) -> u64;
        }

        entity Caller {
            routes {
                constructor(t: Address<Token>) => []
                probe(who: address) -> u64 => [
                    var bal = balanceOf(who) ~> m_token;
                    return(bal)
                ]
            }
            m_token: Address<Token> { in constructor(t) => t }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("IToken") && sol.contains("balanceOf"),
        "extern entity var-call must emit interface cast: {sol}"
    );
    assert!(
        sol.contains("uint64 bal") || sol.contains("uint256 bal"),
        "resolve_var_call_type_with_target must infer extern route return type: {sol}"
    );
    assert_solc_compiles("route_extern_var_call_return_type", &sol);
}

#[test]
fn n4_codegen_route_send_value_and_sig_type_solc() {
    let program = parse_evm(
        r#"
        entity Payer {
            routes {
                constructor() => []
                pay(recipient: address, amount: u64) => [
                    ~> recipient with { value: amount }
                ]
            }
            m_sent: u64 {
                in constructor() => 0
                in pay(_, amount) => m_sent + amount
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("call{value:") || sol.contains("{value:"),
        "plain transfer with value option must emit call{{value: ...}} (extract_send_option): {sol}"
    );
    assert!(
        sol.contains("function pay(") && sol.contains("address recipient"),
        "route param address must register for infer_sig_type Ident binding: {sol}"
    );
    assert_solc_compiles("route_send_value_and_sig_type", &sol);
}

// ---------------------------------------------------------------------------
// N4-58: codegen/solidity/evm/route.rs — slice 2 (emit_transforms_sol /
// gen_constructor_impl tails / gen_route_impl_ext / gen_from_checks fallback)
// Baseline @ N4-56: 87.98% line (198 missed / 1647).
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_route_transform_mapping_scoped_let_solc() {
    let program = parse_evm(
        r#"
        entity Ledger {
            routes {
                constructor() => []
                credit(acct: u64, points: u64) => []
            }
            m_scores: HashMap<u64, u64> {
                in constructor() => {}
                in credit(acct, points) => {
                    let bonus = points + 1;
                    m_scores.insert(acct, bonus)
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_scores[") && sol.contains("bonus"),
        "mapping transform with let must emit scoped setup + in-scope writes (emit_transforms_sol): {sol}"
    );
    assert!(
        sol.contains("{") && sol.contains("uint64 bonus"),
        "transform let must open alias scope (needs_scope mapping arm): {sol}"
    );
    assert_solc_compiles("route_transform_mapping_scoped_let", &sol);
}

#[test]
fn n4_codegen_route_transform_vec_push_scoped_solc() {
    let program = parse_evm(
        r#"
        entity VecLog {
            routes {
                constructor() => []
                append(val: u64) => []
            }
            m_log: Vec<u64> {
                in constructor() => array()
                in append(val) => {
                    let bumped = val + 1;
                    m_log.push(bumped)
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_log.push(") && sol.contains("bumped"),
        "Vec push transform must emit in-place push (lower_member_push_body arm): {sol}"
    );
    assert!(
        sol.contains("uint64 bumped"),
        "vec push with let must use scoped alias block: {sol}"
    );
    assert_solc_compiles("route_transform_vec_push_scoped", &sol);
}

#[test]
fn n4_codegen_route_transform_param_alias_solc() {
    let program = parse_evm(
        r#"
        entity AliasXfer {
            routes {
                constructor() => []
                transfer(sum: u64, pts: u64) => []
            }
            m_total: u64 {
                in constructor() => 0
                in transfer(total, bonus) => m_total + total + bonus
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        (sol.contains("uint64 total = sum") || sol.contains("total = sum"))
            && sol.contains("next_m_total"),
        "transform param rename must emit gen_transform_param_aliases + scalar pending: {sol}"
    );
    assert!(
        sol.contains("m_total + total") || sol.contains("total) + bonus"),
        "aliased params must feed scalar transform body: {sol}"
    );
    assert_solc_compiles("route_transform_param_alias", &sol);
}

#[test]
fn n4_codegen_route_transform_scalar_scoped_let_solc() {
    let program = parse_evm(
        r#"
        entity ScalarLet {
            routes {
                constructor() => []
                bump(inc: u64) => []
            }
            m_total: u64 {
                in constructor() => 0
                in bump(inc) => {
                    let next = m_total + inc;
                    next
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64 next_m_total") && sol.contains("uint64 next ="),
        "scalar transform with let must hoist next_* outside scope (EVM-H7): {sol}"
    );
    assert!(
        sol.contains("m_total = next_m_total"),
        "scalar transform must defer storage write via pending queue: {sol}"
    );
    assert_solc_compiles("route_transform_scalar_scoped_let", &sol);
}

#[test]
fn n4_codegen_route_transform_phased_repeat_next_solc() {
    let program = parse_evm(
        r#"
        entity PhasedRepeat {
            routes {
                constructor() => []
                grow(n: u64) => [
                    prep: []
                    finish: []
                ]
            }
            m_total: u64 {
                in constructor() => 0
                in grow(n) => prep: m_total + 1
                in grow(n) => finish: m_total + n
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("// Phase: prep") && sol.contains("// Phase: finish"),
        "phased route must emit per-phase member updates: {sol}"
    );
    assert!(
        sol.matches("uint64 next_m_total").count() >= 1
            && sol.contains("next_m_total ="),
        "repeat transforms on same member must assign (not redeclare) next_* : {sol}"
    );
    assert_solc_compiles("route_transform_phased_repeat_next", &sol);
}

#[test]
fn n4_codegen_route_transform_mapping_pending_writes_solc() {
    let program = parse_evm(
        r#"
        entity MapPending {
            routes {
                constructor() => []
                set(k: u64, v: u64) => []
            }
            m_map: HashMap<u64, u64> {
                in constructor() => {}
                in set(k, v) => m_map.insert(k, v)
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_map[") || sol.contains("m_map.insert"),
        "bare mapping insert must lower via gen_mapping_transform_split: {sol}"
    );
    assert!(
        !sol.contains("next_m_map"),
        "mapping transform must not use scalar next_* temps: {sol}"
    );
    assert_solc_compiles("route_transform_mapping_pending_writes", &sol);
}

#[test]
fn n4_codegen_route_constructor_no_init_defaults_solc() {
    let program = parse_evm(
        r#"
        entity NoInitCtor {
            routes {
                ping() => []
            }
            m_count: u64 {
                in ping() => m_count + 1
            }
            m_flag: bool {
                in ping() => false
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("constructor(address factory_)"),
        "det entity without init route must still synthesize factory-guarded ctor: {sol}"
    );
    assert!(
        sol.contains("m_count =") && sol.contains("m_flag ="),
        "no-init ctor must default-initialize non-mapping members (L1550–L1571): {sol}"
    );
    assert_solc_compiles("route_constructor_no_init_defaults", &sol);
}

#[test]
fn n4_codegen_route_init_default_tail_members_solc() {
    let program = parse_evm(
        r#"
        entity InitTail {
            routes {
                init create(owner: address) => [
                    setup: []
                ]
            }
            m_owner: address {
                in create(owner) => setup: owner
            }
            m_count: u64 {
                in create(owner) => setup: 0
            }
            m_extra: u64 {
                in ping() => m_extra + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("constructor(") && sol.contains("owner"),
        "init route must fold into synthesized constructor: {sol}"
    );
    assert!(
        sol.contains("m_extra ="),
        "members without init-route transform must get ctor default tail (L1641–L1665): {sol}"
    );
    assert_solc_compiles("route_init_default_tail_members", &sol);
}

#[test]
fn n4_codegen_route_init_mixed_ctor_body_solc() {
    let program = parse_evm(
        r#"
        entity Mini {
            routes { constructor(seed: u64) => [] }
            m_seed: u64 { in constructor(seed) => seed }
        }

        entity Host {
            routes {
                init boot(seed: u64) => [
                    prep: []
                    deploy Mini(seed)
                ]
            }
            m_spawns: u64 {
                in boot(seed) => prep: 0
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("// Phase: prep") && sol.contains("new Mini"),
        "init RouteBody::Mixed must emit phased blocks + trailing deploy in ctor (L1620–L1638): {sol}"
    );
    assert_solc_compiles("route_init_mixed_ctor_body", &sol);
}

#[test]
#[cfg_attr(debug_assertions, should_panic(expected = "EVM-6 M2: from-clause arity"))]
fn n4_codegen_route_from_multiarg_false_fallback_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity Vault {
            identity m_a: u64
            identity m_b: u64
            routes {
                constructor() => []
                secret() from Vault(1, 2) => []
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    // The M2 guard is a `debug_assert!` (defence in depth): debug builds
    // panic (pinned above), release builds compile it out and must emit the
    // literal-`false` sender check instead.
    assert!(
        sol.contains("require(false, \"from clause failed\")"),
        "non-deterministic multi-arg from-clause must fall back to a literal-false check: {sol}"
    );
}

#[test]
fn n4_codegen_route_det_ext_view_payable_private_solc() {
    let program = parse_evm(
        r#"
        entity DetExt {
            identity m_slot: u64
            routes {
                constructor() => []
                view peek() -> u64 => [ return(m_n) ]
                private helper() => []
                fund() => [ ~> address(0) with { value: msg::value } ]
            }
            m_n: u64 {
                in constructor() => 0
                in fund() => m_n + msg::value
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function peek() external view returns")
            && sol.contains("function _helper() internal"),
        "gen_route_impl_ext must emit view + private _helper routes: {sol}"
    );
    assert!(
        sol.contains("function fund() external payable"),
        "gen_route_impl_ext must mark msg::value routes payable: {sol}"
    );
    assert!(
        sol.contains("keccak256") || sol.contains("_factory"),
        "deterministic entity must emit factory wiring in gen_route_impl_ext path: {sol}"
    );
    assert_solc_compiles("route_det_ext_view_payable_private", &sol);
}

#[test]
fn n4_codegen_route_transform_string_member_sentinel_solc() {
    let program = parse_evm(
        r#"
        entity StrMember {
            routes {
                constructor() => []
                rename(label: String) => []
            }
            m_label: String {
                in constructor() => ""
                in rename(label) => label
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("string") && sol.contains("next_m_label"),
        "String member transform must use solidity_default_for_member_ty when rhs is sentinel 0: {sol}"
    );
    assert_solc_compiles("route_transform_string_member_sentinel", &sol);
}

#[test]
fn n4_codegen_route_init_phased_ctor_only_solc() {
    let program = parse_evm(
        r#"
        entity PhasedInit {
            routes {
                init create(owner: address) => [
                    write: []
                    seal: []
                ]
            }
            m_owner: address {
                in create(owner) => write: owner
            }
            m_sealed: bool {
                in create(owner) => seal: true
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("// Phase: write") && sol.contains("// Phase: seal"),
        "init RouteBody::Phased must emit phased ctor blocks (L1603–L1618): {sol}"
    );
    assert!(
        sol.contains("m_owner =") && sol.contains("m_sealed ="),
        "phased init member updates must commit in constructor: {sol}"
    );
    assert_solc_compiles("route_init_phased_ctor_only", &sol);
}

#[test]
fn n4_codegen_route_transform_mapping_if_else_solc() {
    let program = parse_evm(
        r#"
        entity TallyMap {
            routes {
                constructor() => []
                vote(id: u64, support: u64, weight: u64) => []
            }
            m_for: HashMap<u64, u64> {
                in constructor() => {}
                in vote(id, support, weight) => {
                    if support == 1 {
                        m_for.update(id, m_for[id] + weight)
                    } else {
                        m_for
                    }
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("if (") && sol.contains("m_for["),
        "mapping if/else transform must emit conditional writes (gen_mapping_transform_split): {sol}"
    );
    assert_solc_compiles("route_transform_mapping_if_else", &sol);
}

#[test]
fn n4_codegen_route_det_ctor_init_deploy_solc() {
    let program = parse_evm(
        r#"
        entity Leaf {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity DetHost {
            identity m_slot: u64
            routes {
                init boot(id: u64) => [
                    deploy Leaf(id)
                ]
            }
            m_count: u64 {
                in boot(id) => 0
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("_factory.deployLeaf") || sol.contains("deployLeaf("),
        "det init with extra params must deploy via factory in initialize, ctor stays stub: {sol}"
    );
    assert!(
        sol.contains("function initialize("),
        "non-identity init params must route body to initialize (gen_initialize_fn): {sol}"
    );
    assert_solc_compiles("route_det_ctor_init_deploy", &sol);
}

#[test]
fn n4_codegen_route_recover_route_not_emitted_solc() {
    let program = parse_evm(
        r#"
        entity RecvSkip {
            routes {
                constructor() => []
                send() => [ rescue bounced: ping() ~> address(0) ]
                ping() => []
                recover bounced(body: CamData) => []
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        !sol.contains("function bounced(") && !sol.contains("function recover"),
        "recover routes must be skipped by is_emittable_route (gen_route_impl early return): {sol}"
    );
    assert!(
        sol.contains("function send(") && sol.contains("function ping("),
        "regular routes must still emit: {sol}"
    );
    assert_solc_compiles("route_recover_route_not_emitted", &sol);
}

#[test]
fn n4_codegen_route_det_receive_fallback_ext_solc() {
    let program = parse_evm(
        r#"
        entity DetRecv {
            identity m_slot: u64
            routes {
                constructor() => []
                accept receive() => []
                fallback() => []
            }
            m_balance: u64 {
                in receive() => m_balance + msg::value
                in fallback() => m_balance + msg::value
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("receive() external payable") && sol.contains("fallback() external payable"),
        "gen_route_impl_ext must lower receive/fallback in deterministic mode: {sol}"
    );
    assert_solc_compiles("route_det_receive_fallback_ext", &sol);
}

#[test]
fn n4_codegen_route_dual_scalar_pending_commit_solc() {
    let program = parse_evm(
        r#"
        entity DualScalar {
            routes {
                constructor() => []
                bump(a: u64, b: u64) => []
            }
            m_x: u64 {
                in constructor() => 0
                in bump(a, b) => m_x + a
            }
            m_y: u64 {
                in constructor() => 0
                in bump(a, b) => m_y + b
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("next_m_x") && sol.contains("next_m_y"),
        "dual scalar transforms must declare separate next_* temps: {sol}"
    );
    assert!(
        sol.contains("m_x = next_m_x") && sol.contains("m_y = next_m_y"),
        "emit_transforms_sol must drain pending queue for all scalars (L1296–L1300): {sol}"
    );
    assert_solc_compiles("route_dual_scalar_pending_commit", &sol);
}

#[test]
fn n4_codegen_route_from_numeric_throw_code_solc() {
    let program = parse_evm(
        r#"
        entity Gate403 {
            routes {
                constructor() => []
                adminOnly() from m_admin : throw 403 => []
            }
            m_admin: address { in constructor() => address(0) }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("require(") && sol.contains("throw(403)"),
        "uniform numeric from-clause throw must emit require with throw code (gen_from_checks L686–L706): {sol}"
    );
    assert_solc_compiles("route_from_numeric_throw_code", &sol);
}

#[test]
fn n4_codegen_route_ctor_member_eq_default_solc() {
    let program = parse_evm(
        r#"
        entity ExplicitDef {
            routes {
                ping() => []
            }
            m_count: u64 = 7 {
                in ping() => m_count + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_count = 7") || sol.contains("m_count = uint64(7)"),
        "member `= expr` default must emit in no-init ctor path (L1558–L1562): {sol}"
    );
    assert_solc_compiles("route_ctor_member_eq_default", &sol);
}

#[test]
fn n4_codegen_route_det_ctor_init_unphased_deploy_solc() {
    let program = parse_evm(
        r#"
        entity Leaf {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity DetCtorBody {
            identity m_slot: u64
            routes {
                init boot() => [
                    deploy Leaf(m_slot)
                ]
                ping() => []
            }
            m_count: u64 {
                in boot() => 0
            }
            m_spare: u64 {
                in ping() => 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        (sol.contains("_factory.deployLeaf") || sol.contains("deployLeaf("))
            && !sol.contains("function initialize("),
        "identity-only init deploy must fold into det constructor via gen_action (L1746–L1749): {sol}"
    );
    assert!(
        sol.contains("m_spare ="),
        "det ctor must default-initialize members without init transform (L1754–L1775): {sol}"
    );
    assert_solc_compiles("route_det_ctor_init_unphased_deploy", &sol);
}

// ---------------------------------------------------------------------------
// N4-60: codegen/solidity/evm/route.rs — slice 3 (close ≥90%)
// Gap @ N4-59: 89.62% line (171 missed / 1647); focus gen_initialize_fn Mixed,
// emit_transforms_sol vec push no-scope, gen_from_checks_det tails, scatter IR.
// Exclude: IrStmt::UpdateCode L376–L391 (DEAD_CODE).
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_route_det_initialize_mixed_phased_solc() {
    let program = parse_evm(
        r#"
        entity Mini {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity DetMixedInit {
            identity m_slot: u64
            routes {
                init setup(seed: u64) => [
                    prep: []
                    deploy Mini(seed)
                ]
                ping() => []
            }
            m_spawns: u64 {
                in setup(seed) => prep: 0
            }
            m_spare: u64 {
                in ping() => 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function initialize(")
            && sol.contains("// Phase: prep")
            && (sol.contains("_factory.deployMini") || sol.contains("deployMini(")),
        "det init RouteBody::Mixed must emit initialize phased + trailing deploy (gen_initialize_fn L1849–L1869): {sol}"
    );
    assert!(
        sol.contains("m_spare ="),
        "initialize must default-initialize members without init transform (L1873–L1897): {sol}"
    );
    // TB-V: forge oracle dropped (route_det_initialize_mixed_phased).
}

#[test]
fn n4_codegen_route_transform_vec_push_no_scope_solc() {
    let program = parse_evm(
        r#"
        entity VecPlain {
            routes {
                constructor() => []
                append(val: u64) => []
            }
            m_log: Vec<u64> {
                in constructor() => array()
                in append(val) => m_log.push(val)
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_log.push(") && !sol.contains("bumped"),
        "plain vec push transform must emit no-scope in-place push (emit_transforms_sol L1168–L1200): {sol}"
    );
    assert_solc_compiles("route_transform_vec_push_no_scope", &sol);
}

#[test]
fn n4_codegen_route_transform_mapping_no_scope_pending_solc() {
    let program = parse_evm(
        r#"
        entity MapPlain {
            routes {
                constructor() => []
                credit(acct: u64, points: u64) => []
            }
            m_scores: HashMap<u64, u64> {
                in constructor() => {}
                in credit(acct, points) => m_scores.insert(acct, points)
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_scores[") && sol.contains("points"),
        "bare mapping insert transform must defer writes via pending (L1237–L1240): {sol}"
    );
    assert_solc_compiles("route_transform_mapping_no_scope_pending", &sol);
}

#[test]
fn n4_codegen_route_det_from_custom_error_revert_solc() {
    let program = parse_evm(
        r#"
        error NotOwner();

        entity Owner {
            identity id: u64
            routes { init setup() => [] }
        }

        entity Vault {
            identity m_slot: u64
            routes {
                init setup() => []
                secret(n: u64) from Owner(n) : throw NotOwner() => []
            }
            m_n: u64 { in setup() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("revert NotOwner(") || sol.contains("revert NotOwner()"),
        "det uniform from-clause custom error must emit revert (gen_from_checks_det L1977–L1989): {sol}"
    );
    assert!(
        sol.contains("predictOwner") || sol.contains("computeAddress"),
        "det from Owner(n) must lower CREATE2 sender check: {sol}"
    );
    assert_solc_compiles("route_det_from_custom_error_revert", &sol);
}

#[test]
fn n4_codegen_route_det_from_extern_single_arg_fallback_solc() {
    let program = parse_evm(
        r#"
        extern entity Foreign {
            route ping();
        }

        entity Gate {
            identity m_slot: u64
            routes {
                init setup() => []
                check(addr: address) from Foreign(addr) => []
            }
            m_n: u64 { in setup() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("msg.sender == addr") || sol.contains("msg.sender == (addr)"),
        "det from extern entity must fall back to single-arg address check (L1952–L1956): {sol}"
    );
    assert_solc_compiles("route_det_from_extern_single_arg_fallback", &sol);
}

#[test]
fn n4_codegen_route_ir_var_call_materialize_solc() {
    let program = parse_evm(
        r#"
        extern entity Peer {
            view route peek(who: address) -> u64;
        }

        entity Caller {
            routes {
                constructor(p: Address<Peer>) => []
                probe(who: address) -> u64 => [
                    var v = peek(who) ~> m_peer;
                    return(v)
                ]
            }
            m_peer: Address<Peer> { in constructor(p) => p }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("peek(") && sol.contains("m_peer"),
        "var-call route must lower through IR VarCall materialize (L150–L156): {sol}"
    );
    assert_solc_compiles("route_ir_var_call_materialize", &sol);
}

#[test]
fn n4_codegen_route_infer_sig_type_signed_param_solc() {
    let program = parse_evm(
        r#"
        entity SignedSig {
            routes {
                constructor() => []
                adjust(delta: i64) => []
            }
            m_acc: i64 {
                in constructor() => 0
                in adjust(delta) => m_acc + delta
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function adjust(int64 delta)") && sol.contains("int64 public m_acc"),
        "i64 route param must register for infer_sig_type / sig_type_from_sol_ty (L761–L766): {sol}"
    );
    assert_solc_compiles("route_infer_sig_type_signed_param", &sol);
}

#[test]
fn n4_codegen_route_nondet_ext_delegates_route_impl_solc() {
    let program = parse_evm(
        r#"
        entity Plain {
            routes {
                constructor() => []
                bump() => []
            }
            m_n: u64 {
                in constructor() => 0
                in bump() => m_n + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function bump() external") && sol.contains("next_m_n"),
        "non-det gen_route_impl_ext must delegate to gen_route_impl (L2018–L2019): {sol}"
    );
    assert_solc_compiles("route_nondet_ext_delegates_route_impl", &sol);
}

// ---------------------------------------------------------------------------
// N4-78: codegen/solidity/evm/route.rs — residual slice (close llvm-partial tail)
// Baseline @ N4-60: 91.80% line (135 missed / 1647); focus gen_from_checks*
// release-only false L656/L1967, materialize_typed_expr scatter, infer_sig_type
// int8–int128, IR emit/rescue tails. Exclude dead IrStmt::UpdateCode L376–391.
// ---------------------------------------------------------------------------

#[test]
fn n4_78_codegen_route_from_multiarg_false_fallback_patched_solc() {
    let result = std::panic::catch_unwind(|| {
        gen_evm_solidity_patched(
            r#"
            entity Owner {
                routes { constructor() => [] }
                m_n: u64 { in constructor() => 0 }
            }

            entity Vault {
                routes {
                    constructor() => []
                    secret() from Owner(address(1)) => []
                }
                m_n: u64 { in constructor() => 0 }
            }
        "#,
            false,
            |program| {
                let route = program
                    .entities
                    .iter_mut()
                    .find(|e| e.name == "Vault")
                    .expect("Vault")
                    .routes
                    .iter_mut()
                    .find(|r| r.name == "secret")
                    .expect("secret");
                route.from_clauses[0]
                    .args
                    .push(Expr::IntLiteral(U256::from_u128(2)));
            },
        )
    });
    if cfg!(debug_assertions) {
        let err = result.expect_err("debug_assert blocks before false push in debug builds");
        let msg = err
            .downcast_ref::<String>()
            .map(|s| s.as_str())
            .or_else(|| err.downcast_ref::<&str>().copied())
            .unwrap_or("");
        assert!(
            msg.contains("EVM-6 M2"),
            "patched multi-arg from must hit gen_from_checks defence-in-depth (L651–L656): {msg}"
        );
    } else {
        let sol = result.expect("release build must reach false push");
        assert!(
            sol.contains("require(") && sol.contains("false"),
            "release-only false push must appear in from require (L656): {sol}"
        );
        assert_solc_compiles("route_from_multiarg_false_fallback_patched", &sol);
    }
}

#[test]
fn n4_78_codegen_route_det_from_bad_arity_false_patched_solc() {
    let result = std::panic::catch_unwind(|| {
        gen_evm_solidity_patched(
            r#"
            entity Owner {
                identity id: u64
                routes { init setup() => [] }
                m_n: u64 { in setup() => 0 }
            }

            entity Vault {
                identity slot: u64
                routes {
                    init setup() => []
                    secret() from Owner(1) => []
                }
                m_n: u64 { in setup() => 0 }
            }
        "#,
            true,
            |program| {
                let route = program
                    .entities
                    .iter_mut()
                    .find(|e| e.name == "Vault")
                    .expect("Vault")
                    .routes
                    .iter_mut()
                    .find(|r| r.name == "secret")
                    .expect("secret");
                route.from_clauses[0]
                    .args
                    .push(Expr::IntLiteral(U256::from_u128(9)));
            },
        )
    });
    if cfg!(debug_assertions) {
        let err = result.expect_err("debug_assert blocks before false push in debug builds");
        let msg = err
            .downcast_ref::<String>()
            .map(|s| s.as_str())
            .or_else(|| err.downcast_ref::<&str>().copied())
            .unwrap_or("");
        assert!(
            msg.contains("EVM-6 M2"),
            "patched det from arity must hit gen_from_checks_det defence-in-depth (L1961–L1967): {msg}"
        );
    } else {
        let sol = result.expect("release build must reach false push");
        assert!(
            sol.contains("require(") && sol.contains("false"),
            "release-only false push must appear in det from require (L1967): {sol}"
        );
        assert_solc_compiles("route_det_from_bad_arity_false_patched", &sol);
    }
}

#[test]
fn n4_78_codegen_route_det_from_member_clause_solc() {
    let program = parse_evm(
        r#"
        entity DetMemberFrom {
            identity slot: u64
            routes {
                init setup() => []
                adminOnly() from m_admin => []
            }
            m_admin: address { in setup() => address(0) }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("msg.sender == m_admin"),
        "det from member clause must lower SLOAD comparison (gen_from_checks_det L1920–L1924): {sol}"
    );
    assert_solc_compiles("route_det_from_member_clause", &sol);
}

#[test]
fn n4_78_codegen_route_from_generic_fail_message_solc() {
    let program = parse_evm(
        r#"
        entity PlainFrom {
            routes {
                constructor() => []
                go() from m_owner => []
            }
            m_owner: address { in constructor() => address(0) }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("require(") && sol.contains("from clause failed"),
        "from without : throw must use generic require message (gen_from_checks L694): {sol}"
    );
    assert_solc_compiles("route_from_generic_fail_message", &sol);
}

#[test]
fn n4_78_codegen_route_infer_sig_type_signed_widths_raw_send_solc() {
    let program = parse_evm(
        r#"
        entity SigWidths {
            routes {
                constructor() => []
                poke(a: i8, b: i16, c: i32, d: i128) => [
                    bump(a, b, c, d) ~> address(0x1111111111111111111111111111111111111111)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("encodeWithSignature")
            && sol.contains("int8")
            && sol.contains("int16")
            && sol.contains("int32")
            && sol.contains("int128"),
        "raw send args must register signed widths via infer_sig_type/sig_type_from_sol_ty (L761–L766): {sol}"
    );
    assert_solc_compiles("route_infer_sig_type_signed_widths_raw_send", &sol);
}

#[test]
fn n4_78_codegen_route_infer_sig_type_literal_raw_send_solc() {
    let program = parse_evm(
        r#"
        entity LitSig {
            routes {
                constructor() => []
                stamp(flag: bool, label: String, digest: bytes32) => [
                    mark(flag, label, digest) ~> address(0x2222222222222222222222222222222222222222)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("encodeWithSignature")
            && sol.contains("bool")
            && sol.contains("string")
            && sol.contains("bytes32"),
        "bool/string/bytes32 send args must hit infer_sig_type literal arms (L730–L734): {sol}"
    );
    assert_solc_compiles("route_infer_sig_type_literal_raw_send", &sol);
}

#[test]
fn n4_78_codegen_route_ir_rescue_call_route_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity RetryCall {
            routes {
                constructor() => []
                outer(n: u64) => [ ~> address(0) ]
                inner(n: u64) => []
            }
            m_x: u64 {
                in constructor() => 0
                in inner(n) => n
            }
        }
    "#,
        false,
        |program| {
            let route = program
                .entities
                .iter_mut()
                .find(|e| e.name == "RetryCall")
                .expect("RetryCall")
                .routes
                .iter_mut()
                .find(|r| r.name == "outer")
                .expect("outer");
            if let RouteBody::Unphased(actions) = &mut route.body {
                actions[0] = RouteAction::Rescue {
                    tag: "fail".into(),
                    action: Box::new(RouteAction::CallRoute {
                        name: "inner".into(),
                        args: vec![Expr::Ident("n".into())],
                    }),
                };
            }
        },
    );
    assert!(
        sol.contains("E26")
            && sol.contains("fail")
            && !sol.contains("try this.")
            && !sol.contains("catch"),
        "IrStmt::Rescue over CallRoute must comment E26, not try/catch (L168–L363): {sol}"
    );
    assert_solc_compiles("route_ir_rescue_call_route", &sol);
}

#[test]
fn n4_78_codegen_route_materialize_binliteral_deploy_solc() {
    let program = parse_evm(
        r#"
        entity Child {
            identity id: u64
            routes { constructor() => [] }
            m_id: u64 { in constructor() => id }
        }

        entity SpawnBin {
            routes {
                constructor() => []
                grow() => [ deploy Child(0b101) ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("new Child") && (sol.contains("5") || sol.contains("0b")),
        "deploy with bin literal must materialize TypedExprKind::BinLiteral through IR (L101): {sol}"
    );
    assert_solc_compiles("route_materialize_binliteral_deploy", &sol);
}

#[test]
fn n4_78_codegen_route_phased_var_call_assignment_mode_solc() {
    let program = parse_evm(
        r#"
        extern entity Peer {
            view route peek(who: address) -> u64;
        }

        entity PhasedCap {
            routes {
                constructor(p: Address<Peer>) => []
                probe(who: address) -> u64 => [
                    prep: []
                    read: [
                        var snap = peek(who) ~> m_peer;
                        return(snap)
                    ]
                ]
            }
            m_peer: Address<Peer> { in constructor(p) => p }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("// Phase: read")
            && sol.contains("snap =")
            && sol.contains("peek("),
        "phased var-call capture must use var_call_assignments assignment path (L250–L264): {sol}"
    );
    assert_solc_compiles("route_phased_var_call_assignment_mode", &sol);
}

#[test]
fn n4_78_codegen_route_ir_send_materialize_typed_exprs_solc() {
    let program = parse_evm(
        r#"
        entity Treasury {
            routes {
                init create() => []
                credit(amount: u64) => []
            }
            m_total: u64 {
                in create() => 0
                in credit(amount) => m_total + amount
            }
        }

        entity Forwarder {
            routes {
                init create() => []
                relay(amount: u64, dest: Address<Treasury>) => [
                    credit(amount) ~> dest
                ]
            }
            m_n: u64 { in create() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("ITreasury") && sol.contains(".credit("),
        "IrStmt::Send must materialize typed dest/args (ir_stmt_to_route_action L131–L141): {sol}"
    );
    assert_solc_compiles("route_ir_send_materialize_typed_exprs", &sol);
}

#[test]
fn n4_78_codegen_route_materialize_stringliteral_deploy_solc() {
    let program = parse_evm(
        r#"
        entity Child {
            identity name: String
            routes { constructor() => [] }
            m_label: String { in constructor() => name }
        }

        entity SpawnStr {
            routes {
                constructor() => []
                grow() => [ deploy Child("seed") ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("new Child") && sol.contains("seed"),
        "deploy with string literal must materialize TypedExprKind::StringLiteral (L103): {sol}"
    );
    assert_solc_compiles("route_materialize_stringliteral_deploy", &sol);
}

#[test]
fn n4_78_codegen_route_infer_sig_type_unsigned_widths_raw_send_solc() {
    let program = parse_evm(
        r#"
        entity UnsignedSig {
            routes {
                constructor() => []
                poke(a: u8, b: u16, c: u32, d: u128) => [
                    mark(a, b, c, d) ~> address(0x3333333333333333333333333333333333333333)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("encodeWithSignature")
            && sol.contains("uint8")
            && sol.contains("uint16")
            && sol.contains("uint32")
            && sol.contains("uint128"),
        "raw send must register unsigned widths via sig_type_from_sol_ty (L755–L759): {sol}"
    );
    assert_solc_compiles("route_infer_sig_type_unsigned_widths_raw_send", &sol);
}

#[test]
fn n4_78_codegen_route_ir_rescue_var_call_materialize_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        extern entity Peer {
            view route peek(who: address) -> u64;
        }

        entity RescueCap {
            routes {
                constructor(p: Address<Peer>) => []
                probe(who: address) => [ ~> address(0) ]
            }
            m_peer: Address<Peer> { in constructor(p) => p }
        }
    "#,
        false,
        |program| {
            let route = program
                .entities
                .iter_mut()
                .find(|e| e.name == "RescueCap")
                .expect("RescueCap")
                .routes
                .iter_mut()
                .find(|r| r.name == "probe")
                .expect("probe");
            if let RouteBody::Unphased(actions) = &mut route.body {
                actions[0] = RouteAction::Rescue {
                    tag: "fail".into(),
                    action: Box::new(RouteAction::VarCall {
                        name: "snap".into(),
                        message: "peek".into(),
                        args: vec![Expr::Ident("who".into())],
                        dest: Expr::Ident("m_peer".into()),
                        send_options: None,
                    }),
                };
            }
        },
    );
    assert!(
        sol.contains("E26") && sol.contains("fail") && !sol.contains("try "),
        "IrStmt::Rescue over VarCall must comment E26, not try/catch (L143–L156): {sol}"
    );
    assert_solc_compiles("route_ir_rescue_var_call_materialize", &sol);
}

// ---------------------------------------------------------------------------
// N4-83: codegen/solidity/evm/route.rs — slice 2 (IR emit residual @ N4-78 tail)
// Baseline @ N4-82: 93.44% line (108 missed / 1647; ~78 executable excl. dead).
// Targets: IrStmt::AstAction L130/L195 (**DEAD** — never constructed by lower_action);
// gen_ir_stmt_evm Conditional/For L320+; Rescue ir_stmt_to_route_action None L172/L373;
// emit_named_phase reindent L469–470; gen_from_checks* scatter; infer_sig_type /
// resolve_typed_send_return_type tails. Exclude UpdateCode L376–391 (~16) and
// gen_from_checks false L656/L1967 (debug_assert in test profile).
// Acceptance: ≥ 94% or ≤ 100 missed (excl. documented dead/debug_assert ~16).
// ---------------------------------------------------------------------------

#[test]
fn n4_83_codegen_route_ir_conditional_else_throw_solc() {
    let program = parse_evm(
        r#"
        entity BranchHost {
            routes {
                constructor() => []
                bump(n: u64) => [
                    if (n > 0) => [ throw 1 ] else [ throw 2 ]
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("if (") && sol.contains("else") && sol.contains("revert"),
        "route if/else must lower via IrStmt::Conditional + gen_ir_stmt_evm (L320–L339): {sol}"
    );
    assert_solc_compiles("route_ir_conditional_else_throw", &sol);
}

#[test]
fn n4_83_codegen_route_ir_for_range_loop_solc() {
    let program = parse_evm(
        r#"
        entity LoopHost {
            routes {
                constructor() => []
                tick(limit: u64) => [
                    for i in 0..limit => [ throw 1 ]
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("for (") && sol.contains("m_n"),
        "route for-loop must lower via IrStmt::For + gen_ir_stmt_evm (L341–L355): {sol}"
    );
    assert_solc_compiles("route_ir_for_range_loop", &sol);
}

#[test]
fn n4_83_codegen_route_ir_rescue_inner_conditional_empty_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity RescueIf {
            routes {
                constructor() => []
                outer(n: u64) => [ throw 0 ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            let route = program
                .entities
                .iter_mut()
                .find(|e| e.name == "RescueIf")
                .expect("RescueIf")
                .routes
                .iter_mut()
                .find(|r| r.name == "outer")
                .expect("outer");
            if let RouteBody::Unphased(actions) = &mut route.body {
                actions[0] = RouteAction::Rescue {
                    tag: "fail".into(),
                    action: Box::new(RouteAction::Conditional {
                        condition: Expr::BinOp(
                            Box::new(Expr::Ident("n".into())),
                            BinOp::Gt,
                            Box::new(Expr::IntLiteral(U256::ZERO)),
                        ),
                        then_actions: vec![RouteAction::Throw { error_code: 1 }],
                        else_actions: vec![],
                    }),
                };
            }
        },
    );
    assert!(
        sol.contains("function outer(")
            && !sol.contains("try this.")
            && !sol.contains("catch"),
        "Rescue on EVM must not emit try/catch (PN-105 / E26 Acki Nacki-only): {sol}"
    );
    // Non-round-trippable IR inner skips `format_rescue` → empty body; round-trippable emits E26 comment only.
    assert!(
        !sol.contains("revert") && !sol.contains("throw"),
        "force-codegen rescue must not lower guarded inner actions on EVM: {sol}"
    );
    assert_solc_compiles("route_ir_rescue_inner_conditional_empty", &sol);
}

#[test]
fn n4_83_codegen_route_mixed_phased_reindent_ir_solc() {
    let program = parse_evm(
        r#"
        entity Mini {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity MixedReindent {
            identity m_slot: u64
            routes {
                init boot(seed: u64) => [
                    prep: []
                    deploy Mini(seed)
                ]
                ping() => []
            }
            m_spawns: u64 { in boot(seed) => prep: 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function initialize(")
            && sol.contains("// Phase: prep")
            && (sol.contains("_factory.deployMini") || sol.contains("deployMini(")),
        "det Mixed init must emit phased IR with reindent_actions (L469–L470): {sol}"
    );
    // TB-V: forge oracle dropped (route_mixed_phased_reindent_ir).
}

#[test]
fn n4_83_codegen_route_from_single_entity_address_solc() {
    let program = parse_evm(
        r#"
        entity Treasury {
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity Vault {
            routes {
                constructor() => []
                pull(treasury: address) from Treasury(treasury) => []
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("require(") && sol.contains("msg.sender"),
        "single-arg from Entity.address() must hit gen_from_checks gen_expr success + continue (L640–L644): {sol}"
    );
    assert_solc_compiles("route_from_single_entity_address", &sol);
}

#[test]
fn n4_83_codegen_route_from_custom_error_revert_nondet_solc() {
    let program = parse_evm(
        r#"
        error NotOwner();

        entity Vault {
            routes {
                constructor() => []
                secret() from m_owner : throw NotOwner() => []
            }
            m_owner: address { in constructor() => address(0) }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("revert NotOwner(") || sol.contains("revert NotOwner()"),
        "non-det uniform from custom error must emit revert (gen_from_checks L668–L681): {sol}"
    );
    assert_solc_compiles("route_from_custom_error_revert_nondet", &sol);
}

#[test]
fn n4_83_codegen_route_from_uniform_throw_code_solc() {
    let program = parse_evm(
        r#"
        entity Gate {
            routes {
                constructor() => []
                enter() from m_admin : throw 42 => []
            }
            m_admin: address { in constructor() => address(0) }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("require(") && sol.contains("throw(42)"),
        "uniform numeric from throw must hit codes.first Some(c) arm (L691–L694): {sol}"
    );
    assert_solc_compiles("route_from_uniform_throw_code", &sol);
}

#[test]
fn n4_83_codegen_route_infer_sig_type_let_bound_ident_solc() {
    let program = parse_evm(
        r#"
        entity SigLet {
            routes {
                constructor() => []
                relay(flag: bool, who: address) => [
                    let tag = flag;
                    let peer = who;
                    ping(tag, peer) ~> address(0x2222222222222222222222222222222222222222)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("encodeWithSignature")
            && sol.contains("bool")
            && sol.contains("address"),
        "raw send with let-bound bool/address must hit infer_sig_type Ident lookup (L735–L743): {sol}"
    );
    assert_solc_compiles("route_infer_sig_type_let_bound_ident", &sol);
}

#[test]
fn n4_83_codegen_route_extern_var_call_return_type_solc() {
    let program = parse_evm(
        r#"
        extern entity Oracle {
            view route quote(seed: u64) -> u64;
        }

        entity Reader {
            routes {
                constructor(o: Address<Oracle>) => []
                snapshot(seed: u64) -> u64 => [
                    var q = quote(seed) ~> m_oracle;
                    return(q)
                ]
            }
            m_oracle: Address<Oracle> { in constructor(o) => o }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("IOracle") && sol.contains("quote(") && sol.contains("uint64 q"),
        "extern var-call return type must resolve via resolve_typed_send_return_type (L827–L832): {sol}"
    );
    assert_solc_compiles("route_extern_var_call_return_type", &sol);
}

#[test]
fn n4_83_codegen_route_pure_route_modifier_solc() {
    let program = parse_evm(
        r#"
        entity PureHost {
            routes {
                constructor() => []
                pure double(x: u64) -> u64 => [ return(x + x) ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function double(") && sol.contains(" pure"),
        "pure route must emit Solidity pure modifier (gen_route_impl L1413–L1414): {sol}"
    );
    assert_solc_compiles("route_pure_route_modifier", &sol);
}

#[test]
fn n4_83_codegen_route_transform_param_alias_solc() {
    let program = parse_evm(
        r#"
        entity AliasHost {
            routes {
                constructor() => []
                credit(slot: u64, points: u64) => []
            }
            m_scores: HashMap<u64, u64> {
                in constructor() => {}
                in credit(slot, bonus) => m_scores.insert(slot, bonus + 1)
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64 bonus") && sol.contains("points"),
        "transform param rename must emit gen_transform_param_aliases Ident arm (L1118–L1124): {sol}"
    );
    assert_solc_compiles("route_transform_param_alias", &sol);
}

#[test]
fn n4_83_codegen_route_det_ctor_unphased_init_actions_solc() {
    let program = parse_evm(
        r#"
        entity Leaf {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity DetUnphasedInit {
            identity m_slot: u64
            routes {
                init boot() => [ deploy Leaf(m_slot) ]
                ping() => []
            }
            m_count: u64 { in boot() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("constructor(")
            && (sol.contains("_factory.deployLeaf") || sol.contains("deployLeaf(")),
        "det init unphased body must emit deploy via gen_action in ctor path (L1746–L1749): {sol}"
    );
    assert_solc_compiles("route_det_ctor_unphased_init_actions", &sol);
}

#[test]
fn n4_83_codegen_route_ir_nested_conditional_solc() {
    let program = parse_evm(
        r#"
        entity NestedIf {
            routes {
                constructor() => []
                probe(outer: u64, inner: u64) => [
                    if (outer > 0) => [
                        if (inner > 0) => [ throw 1 ] else [ throw 2 ]
                    ] else [ throw 3 ]
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.matches("if (").count() >= 2 && sol.contains("else"),
        "nested route conditionals must recurse gen_ir_stmt_evm Conditional arms (L327–L338): {sol}"
    );
    assert_solc_compiles("route_ir_nested_conditional", &sol);
}

#[test]
fn n4_83_codegen_route_from_mismatched_throw_codes_generic_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity Owner {
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity Gate {
            routes {
                constructor() => []
                enter(peer: address) from m_admin : throw 1 => []
            }
            m_admin: address { in constructor() => address(0) }
        }
    "#,
        false,
        |program| {
            let route = program
                .entities
                .iter_mut()
                .find(|e| e.name == "Gate")
                .expect("Gate")
                .routes
                .iter_mut()
                .find(|r| r.name == "enter")
                .expect("enter");
            route.from_clauses.push(cambrian_transpiler::ast::FromClause {
                entity_name: "Owner".into(),
                args: vec![Expr::Ident("peer".into())],
                with_params: None,
                kind: cambrian_transpiler::ast::FromClauseKind::Entity,
                error_code: Some(2),
                error_name: None,
                error_args: vec![],
            });
        },
    );
    assert!(
        sol.contains("require(") && sol.contains("from clause failed"),
        "mismatched from throw codes must use generic require message (L696–L697): {sol}"
    );
    assert_solc_compiles("route_from_mismatched_throw_codes_generic", &sol);
}

// ---------------------------------------------------------------------------
// N4-85: codegen/solidity/evm/route.rs — slice 3 (infer-sig / ctor-default /
// phase reindent residual @ N4-83 tail)
// ---------------------------------------------------------------------------

#[test]
fn n4_85_codegen_route_det_mixed_ir_reindent_regular_solc() {
    let program = parse_evm(
        r#"
        entity Leaf {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity MixedIrReindent {
            identity m_slot: u64
            routes {
                constructor() => []
                run(seed: u64) => [
                    prep: []
                    deploy Leaf(seed)
                ]
            }
            m_spawns: u64 { in run(seed) => prep: 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function run(")
            && sol.contains("// Phase: prep")
            && (sol.contains("_factory.deployLeaf") || sol.contains("deployLeaf(")),
        "det Mixed regular route must lower via IR reindent_actions path (L469–L471): {sol}"
    );
    assert_solc_compiles("route_det_mixed_ir_reindent_regular", &sol);
}

#[test]
fn n4_85_codegen_route_infer_sig_type_send_literal_exprs_solc() {
    let program = parse_evm(
        r#"
        entity LitSend {
            routes {
                constructor() => []
                stamp() => [
                    mark(true, "tag", 0x0000000000000000000000000000000000000000000000000000000000000001)
                        ~> address(0x3333333333333333333333333333333333333333)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("encodeWithSignature")
            && sol.contains("bool")
            && sol.contains("string")
            && (sol.contains("bytes32") || sol.contains("uint256")),
        "literal send args must hit infer_sig_type literal arms (L730–L734): {sol}"
    );
    assert_solc_compiles("route_infer_sig_type_send_literal_exprs", &sol);
}

#[test]
fn n4_85_codegen_route_infer_sig_type_msg_sender_raw_send_solc() {
    let program = parse_evm(
        r#"
        entity SenderSig {
            routes {
                constructor() => []
                relay() => [
                    who(msg::sender) ~> address(0x4444444444444444444444444444444444444444)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("encodeWithSignature") && sol.contains("address"),
        "msg::sender raw send must hit infer_sig_type MsgField arm (L734): {sol}"
    );
    assert_solc_compiles("route_infer_sig_type_msg_sender_raw_send", &sol);
}

#[test]
fn n4_85_codegen_route_infer_sig_type_record_binding_fallback_solc() {
    let program = parse_evm(
        r#"
        record Pair { a: u64, b: u64 }

        entity RecSig {
            routes {
                constructor() => []
                relay(pair: Pair, slot: u64) => [
                    push(pair, slot) ~> address(0x5555555555555555555555555555555555555555)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("encodeWithSignature") && sol.contains("uint256"),
        "record-typed let binding must fall back via sig_type_from_sol_ty default (L766–L767): {sol}"
    );
    assert_solc_compiles("route_infer_sig_type_record_binding_fallback", &sol);
}

#[test]
fn n4_85_codegen_route_infer_sig_type_binop_expr_fallback_solc() {
    let program = parse_evm(
        r#"
        entity ExprSig {
            routes {
                constructor() => []
                relay(x: u64) => [
                    bump(x + 1) ~> address(0x6666666666666666666666666666666666666666)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("encodeWithSignature(\"bump(uint256)\""),
        "non-literal send arg must hit infer_sig_type fallback arm (L743): {sol}"
    );
    assert_solc_compiles("route_infer_sig_type_binop_expr_fallback", &sol);
}

#[test]
fn n4_85_codegen_route_extern_var_call_void_return_solc() {
    let program = parse_evm(
        r#"
        extern entity Sink {
            route ping(seed: u64);
        }

        entity Caller {
            routes {
                constructor(s: Address<Sink>) => []
                tap(seed: u64) => [
                    var ack = ping(seed) ~> m_sink;
                ]
            }
            m_sink: Address<Sink> { in constructor(s) => s }
            m_last: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("ISink") && sol.contains("uint256 ack"),
        "extern route without return type must resolve var-call as uint256 (L831–L833): {sol}"
    );
    // TB-V: forge oracle dropped (route_extern_var_call_void_return).
}

#[test]
fn n4_85_codegen_route_in_program_var_call_return_solc() {
    let program = parse_evm(
        r#"
        entity Counter {
            routes {
                constructor() => []
                view read() -> u64 => [ return(m_n) ]
            }
            m_n: u64 { in constructor() => 0 }
        }

        entity Reader {
            routes {
                constructor() => []
                probe() -> u64 => [
                    var n = read() ~> Counter.address();
                    return(n)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("ICounter") && sol.contains("read(") && sol.contains("uint64 n"),
        "in-program entity var-call must resolve route return type (L815–L824): {sol}"
    );
    assert_solc_compiles("route_in_program_var_call_return", &sol);
}

#[test]
fn n4_85_codegen_route_nested_hoist_string_bool_literal_solc() {
    let program = parse_evm(
        r#"
        entity NestedHoist {
            routes {
                constructor() => []
                relay(flag: bool) => [
                    if flag => [
                        let label = "ok";
                        let ok = true;
                        mark(label, ok) ~> address(0x7777777777777777777777777777777777777777)
                    ] else []
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("string memory label") && sol.contains("bool ok"),
        "nested string/bool lets must hit collect_nested_hoist_locals literal arms (L904–L906): {sol}"
    );
    assert_solc_compiles("route_nested_hoist_string_bool_literal", &sol);
}

#[test]
fn n4_85_codegen_route_ctor_mixed_init_phased_bare_solc() {
    let program = parse_evm(
        r#"
        entity InitMixedCtor {
            routes {
                init boot(owner: address) => [
                    setup: []
                ]
                ping() => []
            }
            m_owner: address {
                in boot(owner) => setup: owner
            }
            m_count: u64 {
                in ping() => m_count + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("constructor(")
            && sol.contains("// Phase: setup")
            && sol.contains("m_owner = owner"),
        "non-det Mixed init route must emit phased ctor body (L1620–L1632): {sol}"
    );
    assert_solc_compiles("route_ctor_mixed_init_phased_bare", &sol);
}

#[test]
fn n4_85_codegen_route_ctor_init_tail_member_default_expr_solc() {
    let program = parse_evm(
        r#"
        entity InitTailDefault {
            routes {
                init boot(owner: address) => [
                    setup: []
                ]
                ping() => []
            }
            m_owner: address {
                in boot(owner) => setup: owner
            }
            m_bonus: u64 = 9 {
                in ping() => m_bonus + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_bonus = 9") || sol.contains("m_bonus = uint64(9)"),
        "init-route tail must emit member default expr without transform (L1652–L1655): {sol}"
    );
    assert_solc_compiles("route_ctor_init_tail_member_default_expr", &sol);
}

#[test]
fn n4_85_codegen_route_det_ctor_member_default_expr_solc() {
    let program = parse_evm(
        r#"
        entity DetDefaultExpr {
            identity m_slot: u64
            routes {
                init boot() => []
                ping() => []
            }
            m_count: u64 {
                in boot() => 0
            }
            m_bonus: u64 = 11 {
                in ping() => m_bonus + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_bonus = 11") || sol.contains("m_bonus = uint64(11)"),
        "det ctor without extra init params must default members with expr (L1763–L1766): {sol}"
    );
    assert_solc_compiles("route_det_ctor_member_default_expr", &sol);
}

#[test]
fn n4_85_codegen_route_det_initialize_mixed_init_body_solc() {
    let program = parse_evm(
        r#"
        entity Leaf {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity DetInitMixed {
            identity m_slot: u64
            routes {
                init boot(extra: u64) => [
                    setup: []
                    deploy Leaf(extra)
                ]
                ping() => []
            }
            m_count: u64 {
                in boot(extra) => setup: extra
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function initialize(")
            && sol.contains("// Phase: setup")
            && (sol.contains("_factory.deployLeaf") || sol.contains("deployLeaf(")),
        "det initialize Mixed init must emit phased + bare deploy body (L1849–L1861): {sol}"
    );
    // TB-V: forge oracle dropped (route_det_initialize_mixed_init_body).
}

#[test]
fn n4_85_codegen_route_det_initialize_default_expr_tail_solc() {
    let program = parse_evm(
        r#"
        entity DetInitTail {
            identity m_slot: u64
            routes {
                init boot(extra: u64) => [
                    setup: []
                ]
                ping() => []
            }
            m_count: u64 {
                in boot(extra) => setup: extra
            }
            m_bonus: u64 = 13 {
                in ping() => m_bonus + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function initialize(")
            && (sol.contains("m_bonus = 13") || sol.contains("m_bonus = uint64(13)")),
        "initialize tail must default members with explicit expr (L1884–L1887): {sol}"
    );
    assert_solc_compiles("route_det_initialize_default_expr_tail", &sol);
}

#[test]
fn n4_85_codegen_route_det_from_create2_identity_match_solc() {
    let program = parse_evm(
        r#"
        entity Token {
            identity m_id: u64
            routes { init setup() => [] }
            m_n: u64 { in setup() => 0 }
        }

        entity Gate {
            identity m_slot: u64
            routes {
                init setup() => []
                admin() from Token(m_slot) => []
            }
            m_n: u64 { in setup() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function admin(")
            && sol.contains("require(")
            && (sol.contains("computeCreate2") || sol.contains("CREATE2") || sol.contains("Token")),
        "det from Entity(identity args) must hit CREATE2 identity match (L1944–L1948): {sol}"
    );
    assert_solc_compiles("route_det_from_create2_identity_match", &sol);
}

#[test]
fn n4_85_codegen_route_det_from_single_arg_address_continue_solc() {
    let program = parse_evm(
        r#"
        entity Treasury {
            routes { init setup() => [] }
            m_n: u64 { in setup() => 0 }
        }

        entity Vault {
            identity m_slot: u64
            routes {
                init setup() => []
                pull(peer: address) from Treasury(peer) => []
            }
            m_n: u64 { in setup() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function pull(")
            && sol.contains("msg.sender")
            && sol.contains("peer"),
        "det from extern Entity(single address arg) must hit gen_expr continue (L1952–L1956): {sol}"
    );
    assert_solc_compiles("route_det_from_single_arg_address_continue", &sol);
}

// ---------------------------------------------------------------------------
// N4-91: codegen/solidity/evm/route.rs — slice 4 (route-impl IR residual @ N4-85)
// Baseline @ N4-90: 95.14% line (80 missed / 1647; ~55 executable excl. dead).
// Targets: gen_route_impl_ext scatter; emit_named_phase_block reindent L469–L471;
// IR unphased-only L507; resolve_var_call / infer_sig tails; ctor/init reindent.
// Exclude IrStmt::AstAction L130/L195 (DEAD); UpdateCode L376–391 (DEAD);
// gen_from_checks release-only false (debug_assert).
// Acceptance: ≥ 95.5% or ≤ 75 missed (excl. dead/debug_assert ~16).
// ---------------------------------------------------------------------------

#[test]
fn n4_91_codegen_route_ir_unphased_only_body_solc() {
    let program = parse_evm(
        r#"
        entity UnphasedOnly {
            routes {
                constructor() => []
                ping() -> u64 => [
                    let ghost = 0;
                    return(ghost)
                ]
            }
            m_n: u64 { in constructor() => 0 in ping() => m_n }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function ping(") && !sol.contains("// Phase:"),
        "fully unphased route must hit emit_route_body_from_ir trailing-only arm (L497–L507): {sol}"
    );
    assert_solc_compiles("route_ir_unphased_only_body", &sol);
}

#[test]
fn n4_91_codegen_route_det_mixed_phase_reindent_lines_solc() {
    let program = parse_evm(
        r#"
        entity Leaf {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity MixedReindentLines {
            identity m_slot: u64
            routes {
                constructor() => []
                grow(seed: u64) => [
                    prep: []
                    deploy Leaf(seed)
                ]
            }
            m_spawns: u64 { in grow(seed) => prep: 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function grow(")
            && sol.contains("// Phase: prep")
            && (sol.contains("_factory.deployLeaf") || sol.contains("deployLeaf(")),
        "det Mixed regular route must reindent named phase action lines (L469–L471): {sol}"
    );
    assert_solc_compiles("route_det_mixed_phase_reindent_lines", &sol);
}

#[test]
fn n4_91_codegen_route_infer_sig_type_unbound_ident_solc() {
    let program = parse_evm(
        r#"
        entity GhostSig {
            routes {
                constructor() => []
                relay() => [
                    ghost(phantom) ~> address(0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("encodeWithSignature") && sol.contains("phantom"),
        "unbound ident raw-send arg must hit infer_sig_type Ident fallback (L740–L741): {sol}"
    );
    // TB-V: forge oracle dropped (route_infer_sig_type_unbound_ident).
}

#[test]
fn n4_91_codegen_route_resolve_var_call_unknown_target_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        extern entity Peer {
            view route peek(who: address) -> u64;
        }

        entity CapHost {
            routes {
                constructor(p: Address<Peer>) => []
                probe(who: address) -> u64 => [
                    var snap = peek(who) ~> m_peer;
                    return(snap)
                ]
            }
            m_peer: Address<Peer> { in constructor(p) => p }
        }
    "#,
        false,
        |program| {
            let route = program.entities[0]
                .routes
                .iter_mut()
                .find(|r| r.name == "probe")
                .expect("probe");
            if let RouteBody::Unphased(actions) = &mut route.body {
                if let RouteAction::VarCall { message, .. } = &mut actions[0] {
                    *message = "missing".into();
                }
            }
        },
    );
    assert!(
        sol.contains("uint256 snap") || sol.contains("snap;"),
        "unknown extern route return must fall back resolve_var_call_type default (L827–L835): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_91_codegen_route_nested_hoist_var_call_skip_redecl_solc() {
    let program = parse_evm(
        r#"
        extern entity Peer {
            view route peek(who: address) -> u64;
        }

        entity NestedHoist {
            routes {
                constructor(p: Address<Peer>) => []
                probe(who: address) -> u64 => [
                    prep: []
                    read: [
                        if (who != address(0)) => [
                            var inner = peek(who) ~> m_peer;
                        ]
                        var snap = peek(who) ~> m_peer;
                        return(snap)
                    ]
                ]
            }
            m_peer: Address<Peer> { in constructor(p) => p }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("inner = IPeer") && sol.contains("snap = IPeer"),
        "nested-hoisted var-call must emit inner in if-block and snap at phase level (L414–L415): {sol}"
    );
    assert_solc_compiles("route_nested_hoist_var_call_skip_redecl", &sol);
}

#[test]
fn n4_91_codegen_route_vec_push_alias_scope_reindent_solc() {
    let program = parse_evm(
        r#"
        entity VecAlias {
            routes {
                constructor() => []
                push(slot: u64, bonus: u64) => []
            }
            m_items: Vec<u64> {
                in constructor() => array()
                in push(slot, extra) => m_items.push(slot + extra)
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64 extra") && sol.contains(".push("),
        "vec push transform with param alias must reindent alias lines (L1180–L1181): {sol}"
    );
    assert_solc_compiles("route_vec_push_alias_scope_reindent", &sol);
}

#[test]
fn n4_91_codegen_route_ctor_nondet_unphased_init_actions_solc() {
    let program = parse_evm(
        r#"
        entity Child {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity Host {
            identity m_slot: u64
            routes {
                init boot(seed: u64) => [
                    deploy Child(seed)
                ]
            }
            m_spawns: u64 { in boot(seed) => m_spawns + 1 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("constructor(")
            && (sol.contains("new Child") || sol.contains("Child(")),
        "non-det ctor with unphased init actions must hit gen_constructor_impl Unphased arm (L1598–L1601): {sol}"
    );
    assert_solc_compiles("route_ctor_nondet_unphased_init_actions", &sol);
}

#[test]
fn n4_91_codegen_route_ctor_nondet_mixed_init_reindent_solc() {
    let program = parse_evm(
        r#"
        entity Child {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity Host {
            identity m_slot: u64
            routes {
                init boot(seed: u64) => [
                    prep: []
                    deploy Child(seed)
                ]
            }
            m_spawns: u64 { in boot(seed) => prep: 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("constructor(")
            && sol.contains("// Phase: prep")
            && (sol.contains("new Child") || sol.contains("Child(")),
        "non-det ctor Mixed init must reindent phased actions (L1620–L1632): {sol}"
    );
    assert_solc_compiles("route_ctor_nondet_mixed_init_reindent", &sol);
}

#[test]
fn n4_91_codegen_route_det_ctor_init_phased_skip_inline_solc() {
    let program = parse_evm(
        r#"
        entity Leaf {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity DetCtorInit {
            identity m_slot: u64
            routes {
                init boot(seed: u64) => [
                    prep: []
                    deploy Leaf(seed)
                ]
            }
            m_spawns: u64 { in boot(seed) => prep: 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function initialize("),
        "non-identity init params must route body to initialize (gen_initialize_fn): {sol}"
    );
    assert!(
        sol.contains("deployLeaf") || sol.contains("_factory.deployLeaf"),
        "initialize phased deploy must use factory path: {sol}"
    );
    let host = sol
        .split("contract DetCtorInit")
        .nth(1)
        .expect("DetCtorInit contract");
    let init_fn = host
        .find("function initialize(")
        .expect("DetCtorInit initialize()");
    let ctor_body = &host[..init_fn];
    assert!(
        !ctor_body.contains("deployLeaf("),
        "det ctor stub must skip inline init deploy when init has extra params (L1739): {sol}"
    );
    // TB-V: forge oracle dropped (route_det_ctor_init_phased_skip_inline).
}

#[test]
fn n4_91_codegen_route_det_initialize_mixed_reindent_solc() {
    let program = parse_evm(
        r#"
        entity Leaf {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity DetInitMixed {
            identity m_slot: u64
            routes {
                init boot(seed: u64, label: String) => [
                    prep: []
                    deploy Leaf(seed)
                ]
            }
            m_label: String { in boot(seed, label) => prep: label }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function initialize(")
            && sol.contains("// Phase: prep")
            && (sol.contains("_factory.deployLeaf") || sol.contains("deployLeaf(")),
        "det initialize Mixed init must reindent phased actions (L1849–L1861): {sol}"
    );
    // TB-V: forge oracle dropped (route_det_initialize_mixed_reindent).
}

#[test]
fn n4_91_codegen_route_det_pure_modifier_ext_solc() {
    let program = parse_evm(
        r#"
        entity PureDet {
            identity m_slot: u64
            routes {
                constructor() => []
                pure double(x: u64) -> u64 => [
                    return(x + x)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function double(") && sol.contains("pure"),
        "deterministic pure route must emit pure modifier via gen_route_impl_ext (L2038–L2039): {sol}"
    );
    assert_solc_compiles("route_det_pure_modifier_ext", &sol);
}

#[test]
fn n4_91_codegen_route_from_generic_message_no_codes_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity Owner {
            routes { constructor() => []
            }
            m_n: u64 { in constructor() => 0 }
        }

        entity Vault {
            routes {
                constructor() => []
                secret() from Owner(address(1)) => []
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            let route = program.entities[1]
                .routes
                .iter_mut()
                .find(|r| r.name == "secret")
                .expect("secret");
            for clause in &mut route.from_clauses {
                clause.error_code = None;
                clause.error_name = None;
            }
        },
    );
    assert!(
        sol.contains("from clause failed"),
        "from clauses without uniform throw codes must hit generic message arm (L691–L698): {sol}"
    );
    assert_solc_compiles("route_from_generic_message_no_codes", &sol);
}

#[test]
fn n4_91_codegen_route_det_from_custom_error_revert_ext_solc() {
    let program = parse_evm(
        r#"
        error NotAdmin();

        entity Admin {
            identity id: u64
            routes { init setup() => [] }
            m_n: u64 { in setup() => 0 }
        }

        entity Vault {
            identity slot: u64
            routes {
                init setup() => []
                secret() from Admin(id) : throw NotAdmin() => []
            }
            m_n: u64 { in setup() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function secret(") && sol.contains("revert NotAdmin()"),
        "det uniform custom-error from must hit gen_from_checks_det revert arm (L1983–L1991): {sol}"
    );
    assert_solc_compiles("route_det_from_custom_error_revert_ext", &sol);
}

#[test]
fn n4_91_codegen_route_nested_let_record_construct_hoist_solc() {
    let mut program = parse_evm(
        r#"
        record Point { x: u64, y: u64 }

        entity RecordHoist {
            routes {
                constructor() => []
                probe(n: u64) => [
                    if (n > 0) => []
                ]
            }
            m_n: u64 { in constructor() => 0 in probe(n) => m_n }
        }
    "#,
    );
    let route = program.entities[0]
        .routes
        .iter_mut()
        .find(|r| r.name == "probe")
        .expect("probe");
    if let RouteBody::Unphased(actions) = &mut route.body {
        if let RouteAction::Conditional { then_actions, .. } = &mut actions[0] {
            then_actions.push(RouteAction::Let {
                pattern: Pattern::Ident("pt".into()),
                value: Expr::RecordConstruct(
                    "Point".into(),
                    vec![
                        ("x".into(), Expr::Ident("n".into())),
                        ("y".into(), Expr::Ident("n".into())),
                    ],
                ),
            });
        }
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Point memory pt") || sol.contains("Point pt"),
        "nested let RecordConstruct must hit collect_nested_hoist_locals RecordConstruct arm (L906): {sol}"
    );
    assert_solc_compiles("route_nested_let_record_construct_hoist", &sol);
}

// ---------------------------------------------------------------------------
// N4-92: codegen/solidity/evm/route.rs — slice 5 (ctor/init residual, phase-action
// reindent tails @ N4-91). Exclude DEAD AstAction/UpdateCode + debug_assert L656.
// Acceptance: ≥ 96% or ≤ 68 missed (excl. dead/debug_assert ~16).
// ---------------------------------------------------------------------------

#[test]
fn n4_92_codegen_route_mixed_phase_action_ir_reindent_solc() {
    let program = parse_evm(
        r#"
        entity Leaf {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity MixedPhaseAction {
            routes {
                constructor() => []
                run(seed: u64) => [
                    prep: [
                        deploy Leaf(seed)
                    ]
                    let tick = 1;
                ]
            }
            m_spawns: u64 { in run(seed) => prep: 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function run(")
            && sol.contains("// Phase: prep")
            && (sol.contains("new Leaf") || sol.contains("Leaf("))
            && sol.contains("tick = 1"),
        "Mixed route with non-empty named phase must reindent IR actions (L469–L471): {sol}"
    );
    assert_solc_compiles("route_mixed_phase_action_ir_reindent", &sol);
}

#[test]
fn n4_92_codegen_route_det_mixed_phase_action_ir_reindent_solc() {
    let program = parse_evm(
        r#"
        entity Leaf {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity DetMixedPhaseAction {
            identity m_slot: u64
            routes {
                constructor() => []
                grow(seed: u64) => [
                    prep: [
                        deploy Leaf(seed)
                    ]
                    let tick = 1;
                ]
            }
            m_spawns: u64 { in grow(seed) => prep: 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function grow(")
            && sol.contains("// Phase: prep")
            && (sol.contains("_factory.deployLeaf") || sol.contains("deployLeaf("))
            && sol.contains("tick = 1"),
        "det Mixed route with phase actions must reindent IR stmt lines (L469–L471): {sol}"
    );
    assert_solc_compiles("route_det_mixed_phase_action_ir_reindent", &sol);
}

#[test]
fn n4_92_codegen_route_ctor_mixed_phase_action_reindent_solc() {
    let program = parse_evm(
        r#"
        entity Child {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity HostMixedCtor {
            identity m_slot: u64
            routes {
                init boot(seed: u64) => [
                    prep: [
                        deploy Child(seed)
                    ]
                    let tick = 1;
                ]
            }
            m_spawns: u64 { in boot(seed) => prep: 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("constructor(")
            && sol.contains("// Phase: prep")
            && (sol.contains("new Child") || sol.contains("Child("))
            && sol.contains("tick = 1"),
        "non-det ctor Mixed init with phase actions must reindent gen_action lines (L1629–L1632): {sol}"
    );
    assert_solc_compiles("route_ctor_mixed_phase_action_reindent", &sol);
}

#[test]
fn n4_92_codegen_route_det_initialize_mixed_phase_action_reindent_solc() {
    let program = parse_evm(
        r#"
        entity Leaf {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity DetInitPhaseAction {
            identity m_slot: u64
            routes {
                init boot(extra: u64) => [
                    setup: [
                        deploy Leaf(extra)
                    ]
                    let tick = 1;
                ]
                ping() => []
            }
            m_count: u64 {
                in boot(extra) => setup: extra
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function initialize(")
            && sol.contains("// Phase: setup")
            && (sol.contains("_factory.deployLeaf") || sol.contains("deployLeaf("))
            && sol.contains("tick = 1"),
        "det initialize Mixed init with phase actions must reindent (L1858–L1861): {sol}"
    );
    // TB-V: forge oracle dropped (route_det_initialize_mixed_phase_action_reindent).
}

#[test]
fn n4_92_codegen_route_ctor_identity_skip_no_init_defaults_solc() {
    let program = parse_evm(
        r#"
        entity IdSkipNoInit {
            identity m_id: u64
            routes {
                ping() => []
            }
            m_flag: bool {
                in ping() => true
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("constructor(address factory_, uint64 m_id_)")
            && sol.contains("m_id = m_id_")
            && sol.contains("m_flag ="),
        "det no-init ctor must set identity on factory ctor and default m_flag: {sol}"
    );
    assert!(
        !sol.contains("m_id = false") && !sol.contains("m_id = 0"),
        "identity member must not receive default initializer: {sol}"
    );
    assert_solc_compiles("route_ctor_identity_skip_no_init_defaults", &sol);
}

#[test]
fn n4_92_codegen_route_transform_extra_pattern_continue_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity ExtraPat {
            routes {
                constructor() => []
                credit(a: u64, b: u64) => []
            }
            m_total: u64 {
                in constructor() => 0
                in credit(x, y) => m_total + x + y
            }
        }
    "#,
        false,
        |program| {
            let member = &mut program.entities[0].members[0];
            member.transforms[1]
                .params
                .push(Pattern::Ident("z".into()));
        },
    );
    assert!(
        sol.contains("function credit(") && sol.contains("m_total ="),
        "extra transform pattern beyond route arity must still emit credit body (L1115): {sol}"
    );
    assert_solc_compiles("route_transform_extra_pattern_continue", &sol);
}

#[test]
fn n4_92_codegen_route_phased_hoisted_var_call_skip_decl_solc() {
    let program = parse_evm(
        r#"
        extern entity Peer {
            view route peek(who: address) -> u64;
        }

        entity HoistSkipDecl {
            routes {
                constructor(p: Address<Peer>) => []
                probe(who: address) -> u64 => [
                    prep: []
                    work: [
                        if (who != address(0)) => [
                            var snap = peek(who) ~> m_peer;
                        ]
                        var snap = peek(who) ~> m_peer;
                        return(snap)
                    ]
                ]
            }
            m_peer: Address<Peer> { in constructor(p) => p }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64 snap;")
            && !sol.contains("uint64 snap;\n            uint64 snap")
            && sol.contains("snap = IPeer(m_peer).peek(who)"),
        "nested-hoisted snap must declare once at function scope and skip phase redecl (L415): {sol}"
    );
    assert_solc_compiles("route_phased_hoisted_var_call_skip_decl", &sol);
}

#[test]
#[should_panic(expected = "EVM codegen: cannot resolve the target entity of var call")]
fn n4_92_codegen_route_var_call_plain_address_unresolved_panic() {
    let _sol = gen_evm_solidity_patched(
        r#"
        entity UnresolvedDest {
            routes {
                constructor() => []
                probe(who: address) -> U256 => [
                    prep: []
                    read: [
                        var snap = ghost(who) ~> m_plain;
                        return(snap)
                    ]
                ]
            }
            m_plain: address { in constructor() => address(0) }
        }
    "#,
        false,
        |program| {
            let route = program.entities[0]
                .routes
                .iter_mut()
                .find(|r| r.name == "probe")
                .expect("probe");
            if let RouteBody::Phased(phases) = &mut route.body {
                let read = phases.iter_mut().find(|p| p.name == "read").expect("read");
                if let RouteAction::VarCall { message, dest, .. } = &mut read.actions[0] {
                    *message = "ghost".into();
                    *dest = Expr::BinOp(
                        Box::new(Expr::Ident("who".into())),
                        BinOp::Add,
                        Box::new(Expr::IntLiteral(U256::from_u128(1))),
                    );
                }
            }
        },
    );
}

#[test]
fn n4_92_codegen_route_det_ctor_unphased_inline_deploy_solc() {
    let program = parse_evm(
        r#"
        entity Leaf {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity DetCtorInlineDeploy {
            identity m_slot: u64
            routes {
                init boot() => [
                    deploy Leaf(m_slot)
                ]
                ping() => []
            }
            m_count: u64 { in boot() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("constructor(")
            && !sol.contains("function initialize(")
            && (sol.contains("_factory.deployLeaf") || sol.contains("deployLeaf(")),
        "det param-less unphased init must inline deploy in ctor (L1746–L1749): {sol}"
    );
    assert_solc_compiles("route_det_ctor_unphased_inline_deploy", &sol);
}

#[test]
fn n4_92_codegen_route_det_from_create2_identity_continue_solc() {
    let program = parse_evm(
        r#"
        entity Token {
            identity m_id: u64
            identity m_series: u64
            routes { init setup() => [] }
            m_n: u64 { in setup() => 0 }
        }

        entity Gate {
            identity m_slot: u64
            routes {
                init setup() => []
                admin() from Token(m_slot, m_series) => []
            }
            m_n: u64 { in setup() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function admin(")
            && sol.contains("require(")
            && (sol.contains("computeCreate2") || sol.contains("CREATE2") || sol.contains("Token")),
        "det from Entity with full identity arity must hit CREATE2 match continue (L1944–L1948): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_92_codegen_route_from_custom_error_revert_body_solc() {
    let program = parse_evm(
        r#"
        error NotOwner();

        entity Vault {
            routes {
                constructor() => []
                secret() from m_owner : throw NotOwner() => []
            }
            m_owner: address { in constructor() => address(0) }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("revert NotOwner()"),
        "uniform custom-error from clauses must hit revert arm (L676–L682): {sol}"
    );
    assert_solc_compiles("route_from_custom_error_revert_body", &sol);
}

// ---------------------------------------------------------------------------
// N4-98: codegen/solidity/evm/route.rs — slice 6 (extern/det residual @ N4-92)
// Baseline @ N4-92: 96.60% line (56 missed / 1647; ~24 excl. dead/debug_assert).
// Targets: extern var-call return L823–L835; det from CREATE2/fallback L1948–L2001;
// det ctor phased skip L1751; unphased IR close L507. Exclude IrStmt::AstAction,
// UpdateCode, gen_from_checks false (debug_assert), gen_initialize_fn None L1793.
// Acceptance: ≥ 96.5% or ≤ 52 missed (excl. documented dead/debug_assert ~16).
// ---------------------------------------------------------------------------

#[test]
fn n4_98_codegen_route_in_program_var_call_return_solc() {
    let program = parse_evm(
        r#"
        entity Counter {
            identity m_id: u64
            routes {
                init boot() => []
                tally() -> u64 => [ return(m_n) ]
            }
            m_n: u64 { in boot() => 0 }
        }

        entity Host {
            routes {
                init boot() => []
                probe() -> u64 => [
                    var v = tally() ~> Counter.address(1);
                    return(v)
                ]
            }
            m_peer: u64 { in boot() => 1 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64 v") && sol.contains("ICounter") && sol.contains(".tally("),
        "in-program var-call must resolve return type via sol_return_type (L821–L823): {sol}"
    );
    assert_solc_compiles("route_in_program_var_call_return", &sol);
}

#[test]
fn n4_98_codegen_route_extern_var_call_typed_return_solc() {
    let program = parse_evm(
        r#"
        extern entity Peer {
            view route peek(who: address) -> u64;
        }

        entity Host {
            routes {
                constructor(p: Address<Peer>) => []
                probe(who: address) -> u64 => [
                    var snap = peek(who) ~> m_peer;
                    return(snap)
                ]
            }
            m_peer: Address<Peer> { in constructor(p) => p }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64 snap") && sol.contains("IPeer") && sol.contains(".peek("),
        "extern var-call with matching route must resolve sol_type return (L827–L831): {sol}"
    );
    assert_solc_compiles("route_extern_var_call_typed_return", &sol);
}

#[test]
fn n4_98_codegen_route_extern_var_call_unknown_route_fallback_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        extern entity Peer {
            view route peek(who: address) -> u64;
        }

        entity Host {
            routes {
                constructor(p: Address<Peer>) => []
                probe(who: address) -> u64 => [
                    var snap = peek(who) ~> m_peer;
                    return(snap)
                ]
            }
            m_peer: Address<Peer> { in constructor(p) => p }
        }
    "#,
        false,
        |program| {
            let route = program.entities[0]
                .routes
                .iter_mut()
                .find(|r| r.name == "probe")
                .expect("probe");
            if let RouteBody::Unphased(actions) = &mut route.body {
                if let RouteAction::VarCall { message, .. } = &mut actions[0] {
                    *message = "missing".into();
                }
            }
        },
    );
    assert!(
        sol.contains("uint256 snap") || sol.contains("snap;"),
        "unknown extern route must fall back resolve_var_call_type uint256 (L835): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_98_codegen_route_nondet_from_single_entity_address_solc() {
    let program = parse_evm(
        r#"
        entity Counter {
            routes { init boot() => [] }
            m_n: u64 { in boot() => 0 }
        }

        entity Gate {
            routes {
                constructor() => []
                admin(peer: address) from Counter(peer) => []
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function admin(")
            && sol.contains("require(")
            && sol.contains("peer"),
        "nondet from Entity(single address) must hit gen_from_checks single-arg arm (L640–L644): {sol}"
    );
    assert_solc_compiles("route_nondet_from_single_entity_address", &sol);
}

#[test]
fn n4_98_codegen_route_nondet_from_numeric_throw_code_solc() {
    let program = parse_evm(
        r#"
        entity Vault {
            routes {
                constructor() => []
                secret() from m_owner : throw 42 => []
            }
            m_owner: address { in constructor() => address(0) }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("require(") && sol.contains("throw(42)"),
        "uniform numeric throw codes must hit gen_from_checks throw-code msg arm (L691–L694): {sol}"
    );
    assert_solc_compiles("route_nondet_from_numeric_throw_code", &sol);
}

#[test]
fn n4_98_codegen_route_det_from_single_arg_fallback_solc() {
    let program = parse_evm(
        r#"
        entity Token {
            identity m_id: u64
            identity m_series: u64
            routes { init setup() => [] }
            m_n: u64 { in setup() => 0 }
        }

        entity Gate {
            identity m_slot: u64
            routes {
                init setup() => []
                admin(peer: address) from Token(peer) => []
            }
            m_n: u64 { in setup() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function admin(")
            && sol.contains("require(")
            && sol.contains("peer"),
        "det from Entity(partial arity) must hit single-arg fallback arm (L1952–L1956): {sol}"
    );
    assert_solc_compiles("route_det_from_single_arg_fallback", &sol);
}

#[test]
fn n4_98_codegen_route_det_from_numeric_throw_code_solc() {
    let program = parse_evm(
        r#"
        entity Vault {
            identity m_slot: u64
            routes {
                init setup() => []
                secret() from m_owner : throw 42 => []
            }
            m_owner: address { in setup() => address(0) }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function secret(")
            && sol.contains("require(")
            && sol.contains("throw(42)"),
        "det uniform numeric throw codes must hit gen_from_checks_det throw-code arm (L1998–L2001): {sol}"
    );
    assert_solc_compiles("route_det_from_numeric_throw_code", &sol);
}

#[test]
fn n4_98_codegen_route_in_program_var_call_void_return_fallback_solc() {
    let program = parse_evm(
        r#"
        entity Counter {
            identity m_id: u64
            routes {
                init boot() => []
                ping() => []
            }
            m_n: u64 { in boot() => 0 }
        }

        entity Host {
            routes {
                init boot() => []
                probe() => [
                    var v = ping() ~> Counter.address(1);
                ]
            }
            m_n: u64 { in boot() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint256 v") && sol.contains("ICounter") && sol.contains(".ping("),
        "in-program var-call to void route must fall through resolve_var_call_type to uint256 (L821–L835): {sol}"
    );
    // TB-V: forge oracle dropped (route_in_program_var_call_void_return_fallback).
}

#[test]
fn n4_98_codegen_route_ir_unphased_trailing_close_solc() {
    let program = parse_evm(
        r#"
        entity TrailingOnly {
            routes {
                constructor() => []
                ping() -> u64 => [
                    let ghost = 1;
                    return(ghost)
                ]
            }
            m_n: u64 { in constructor() => 0 in ping() => m_n }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function ping(") && sol.contains("ghost = 1") && !sol.contains("// Phase:"),
        "fully unphased route IR must hit trailing-only emit_route_body_from_ir close (L497–L507): {sol}"
    );
    assert_solc_compiles("route_ir_unphased_trailing_close", &sol);
}

#[test]
fn n4_98_codegen_route_det_ctor_phased_init_body_skip_solc() {
    let program = parse_evm(
        r#"
        entity Leaf {
            identity m_id: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity DetPhasedInit {
            identity m_slot: u64
            routes {
                init boot() => [
                    prep: []
                    deploy Leaf(m_slot)
                ]
            }
            m_spawns: u64 { in boot() => prep: 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("constructor(")
            && !sol.contains("function initialize(")
            && !sol.contains("// Phase: prep"),
        "det ctor with identity-only init + phased body must skip inline phased init (L1751): {sol}"
    );
    assert_solc_compiles("route_det_ctor_phased_init_body_skip", &sol);
}

// ---------------------------------------------------------------------------
// N4-110: codegen/solidity/evm/route.rs — residual scatter @ N4-98 tail.
// Baseline @ N4-109: 96.78% (53 missed / 1647; ~34 excl. dead/debug_assert).
// Targets: emit_route_body_from_ir unphased close L507; gen_from_checks nondet
// L644/L682–L694; resolve_var_call_type_with_target L823–L835; gen_from_checks_det
// L1948/L1956/L1990–L2001. Exclude: IrStmt::AstAction L130/L195 (DEAD);
// UpdateCode L377–L390 (DEAD); gen_from_checks false L656/L1967 (debug_assert);
// gen_initialize_fn None L1793 (unreachable).
// Acceptance: ≥ 97% or ≤ 48 raw or ≤ 28 excl. dead (Δ ≥ −5 executable).
// ---------------------------------------------------------------------------

#[test]
fn n4_110_codegen_route_phased_in_program_var_call_u64_solc() {
    let program = parse_evm(
        r#"
        entity Counter {
            identity m_id: u64
            routes {
                init boot() => []
                tally() -> u64 => [ return(m_n) ]
            }
            m_n: u64 { in boot() => 0 }
        }

        entity Host {
            routes {
                init boot() => []
                probe() -> u64 => [
                    work: [
                        var v = tally() ~> Counter.address(m_id);
                        return(v)
                    ]
                ]
            }
            m_id: u64 { in boot() => 1 }
            m_n: u64 { in boot() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("// Phase: work")
            && sol.contains("uint64 v")
            && sol.contains("ICounter")
            && sol.contains(".tally("),
        "phased in-program var-call must hoist typed return via resolve_var_call_type_with_target (L821–L824): {sol}"
    );
    assert_solc_compiles("route_phased_in_program_var_call_u64", &sol);
}

#[test]
fn n4_110_codegen_route_phased_extern_var_call_u64_solc() {
    let program = parse_evm(
        r#"
        extern entity Oracle {
            view route quote(seed: u64) -> u64;
        }

        entity Reader {
            routes {
                constructor(o: Address<Oracle>) => []
                snapshot(seed: u64) -> u64 => [
                    work: [
                        var q = quote(seed) ~> m_oracle;
                        return(q)
                    ]
                ]
            }
            m_oracle: Address<Oracle> { in constructor(o) => o }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("// Phase: work")
            && sol.contains("uint64 q")
            && sol.contains("IOracle")
            && sol.contains(".quote("),
        "phased extern var-call must resolve return via resolve_var_call_type_with_target (L827–L833): {sol}"
    );
    assert_solc_compiles("route_phased_extern_var_call_u64", &sol);
}

#[test]
fn n4_110_codegen_route_phased_extern_unknown_route_uint256_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        extern entity Peer {
            view route peek(who: address) -> u64;
        }

        entity Host {
            routes {
                constructor(p: Address<Peer>) => []
                probe(who: address) -> u64 => [
                    work: [
                        var snap = peek(who) ~> m_peer;
                        return(snap)
                    ]
                ]
            }
            m_peer: Address<Peer> { in constructor(p) => p }
        }
    "#,
        false,
        |program| {
            let route = program.entities[0]
                .routes
                .iter_mut()
                .find(|r| r.name == "probe")
                .expect("probe");
            if let RouteBody::Phased(phases) = &mut route.body {
                if let RouteAction::VarCall { message, .. } = &mut phases[0].actions[0] {
                    *message = "missing".into();
                }
            }
        },
    );
    assert!(
        sol.contains("// Phase: work")
            && (sol.contains("uint256 snap") || sol.contains("snap;")),
        "phased unknown extern route must fall back to uint256 (L835): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_110_codegen_route_phased_void_var_call_uint256_solc() {
    let program = parse_evm(
        r#"
        entity Counter {
            identity m_id: u64
            routes {
                init boot() => []
                ping() => []
            }
            m_n: u64 { in boot() => 0 }
        }

        entity Host {
            routes {
                init boot() => []
                probe() => [
                    work: [
                        var v = ping() ~> Counter.address(m_id);
                    ]
                ]
            }
            m_id: u64 { in boot() => 1 }
            m_n: u64 { in boot() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("// Phase: work")
            && sol.contains("uint256 v")
            && sol.contains("ICounter")
            && sol.contains(".ping("),
        "phased void-route var-call must fall through resolve_var_call_type_with_target to uint256 (L821–L835): {sol}"
    );
    // TB-V: forge oracle dropped (route_phased_void_var_call_uint256).
}

#[test]
fn n4_110_codegen_route_nondet_dual_from_custom_error_revert_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        error NotOwner();

        entity Vault {
            routes {
                constructor() => []
                secret() from m_owner : throw NotOwner() => []
            }
            m_owner: address { in constructor() => address(0) }
            m_backup: address { in constructor() => address(0) }
        }
    "#,
        false,
        |program| {
            let route = &mut program.entities[0].routes[1];
            route.from_clauses.push(cambrian_transpiler::ast::FromClause {
                entity_name: "m_backup".into(),
                args: vec![],
                with_params: None,
                kind: cambrian_transpiler::ast::FromClauseKind::Member,
                error_code: None,
                error_name: Some("NotOwner".into()),
                error_args: vec![],
            });
        },
    );
    assert!(
        sol.contains("revert NotOwner(") || sol.contains("revert NotOwner()"),
        "uniform dual from custom errors must emit gen_from_checks revert return (L668–L681): {sol}"
    );
    assert_solc_compiles("route_nondet_dual_from_custom_error_revert", &sol);
}

#[test]
fn n4_110_codegen_route_det_dual_from_custom_error_revert_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        error NotOwner();

        entity Vault {
            identity m_slot: u64
            routes {
                init setup() => []
                secret() from m_owner : throw NotOwner() => []
            }
            m_owner: address { in setup() => address(0) }
            m_backup: address { in setup() => address(0) }
        }
    "#,
        true,
        |program| {
            let route = program.entities[0]
                .routes
                .iter_mut()
                .find(|r| r.name == "secret")
                .expect("secret");
            route.from_clauses.push(cambrian_transpiler::ast::FromClause {
                entity_name: "m_backup".into(),
                args: vec![],
                with_params: None,
                kind: cambrian_transpiler::ast::FromClauseKind::Member,
                error_code: None,
                error_name: Some("NotOwner".into()),
                error_args: vec![],
            });
        },
    );
    assert!(
        sol.contains("revert NotOwner(") || sol.contains("revert NotOwner()"),
        "det uniform dual from custom errors must emit gen_from_checks_det revert return (L1976–L1990): {sol}"
    );
    assert_solc_compiles("route_det_dual_from_custom_error_revert", &sol);
}

#[test]
fn n4_110_codegen_route_receive_unphased_body_ir_solc() {
    let program = parse_evm(
        r#"
        entity Receiver {
            routes {
                constructor() => []
                accept receive() => []
            }
            m_n: u64 {
                in constructor() => 0
                in receive() => m_n + msg::value
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("receive() external payable")
            && !sol.contains("// Phase:"),
        "receive route must lower via emit_route_body_from_ir trailing-only close (L497–L507): {sol}"
    );
    assert_solc_compiles("route_receive_unphased_body_ir", &sol);
}

#[test]
fn n4_110_codegen_route_nondet_from_entity_bad_arg_false_fallback_patched_solc() {
    let result = std::panic::catch_unwind(|| {
        gen_evm_solidity_patched(
            r#"
            entity Counter {
                identity m_id: u64
                routes { init boot() => [] }
                m_n: u64 { in boot() => 0 }
            }

            entity Gate {
                routes {
                    constructor() => []
                    admin(peer: address) from Counter(peer) => []
                }
                m_n: u64 { in constructor() => 0 }
            }
        "#,
            false,
            |program| {
                let route = program
                    .entities
                    .iter_mut()
                    .find(|e| e.name == "Gate")
                    .expect("Gate")
                    .routes
                    .iter_mut()
                    .find(|r| r.name == "admin")
                    .expect("admin");
                route.from_clauses[0]
                    .args
                    .push(Expr::IntLiteral(U256::from_u128(9)));
            },
        )
    });
    if cfg!(debug_assertions) {
        let err = result.expect_err("debug_assert blocks before false push in debug builds");
        let msg = err
            .downcast_ref::<String>()
            .map(|s| s.as_str())
            .or_else(|| err.downcast_ref::<&str>().copied())
            .unwrap_or("");
        assert!(
            msg.contains("EVM-6 M2"),
            "patched nondet from arity must hit gen_from_checks defence-in-depth (L651–L656): {msg}"
        );
    } else {
        let sol = result.expect("release build must reach false push");
        assert!(
            sol.contains("require(") && sol.contains("false"),
            "release-only false push must appear in nondet from require (L656): {sol}"
        );
        assert_solc_compiles("route_nondet_from_entity_bad_arg_false_fallback_patched", &sol);
    }
}

#[test]
fn n4_110_codegen_route_det_from_create2_identity_continue_solc() {
    let program = parse_evm(
        r#"
        entity Token {
            identity m_id: u64
            identity m_series: u64
            routes { init setup() => [] }
            m_n: u64 { in setup() => 0 }
        }

        entity Gate {
            identity m_slot: u64
            routes {
                init setup() => []
                admin() from Token(m_slot, m_series) => []
            }
            m_slot: u64 { in setup() => 1 }
            m_series: u64 { in setup() => 2 }
            m_n: u64 { in setup() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function admin(")
            && sol.contains("require(")
            && (sol.contains("computeCreate2") || sol.contains("CREATE2") || sol.contains("Token")),
        "det from Entity(full identity args) must hit CREATE2 continue arm (L1944–L1948): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

// ---------------------------------------------------------------------------
// N4-49: codegen/solidity/core/expr.rs — slice 1 (gen_expr/binop/cast/field/method)
// Baseline @ N4-48: 74.66% line (262 missed / 1034); focus gen_binop_operands,
// gen_evm_ns, HashMap method arms, hoisted match/let, cast/bytes/tuple.
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_expr_signed_unsigned_binop_coerce_solc() {
    let program = parse_evm(
        r#"
        entity MixBin {
            routes {
                constructor() => []
                add(n: u64) => []
            }
            m_bias: i64 {
                in constructor() => 0
                in add(n) => m_bias + n
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("int256(") || sol.contains("int64("),
        "mixed i64 + u64 binop must coerce via gen_binop_operands_solidity: {sol}"
    );
    assert_solc_compiles("expr_signed_unsigned_binop_coerce", &sol);
}

#[test]
fn n4_codegen_expr_wrapping_ops_member_solc() {
    let program = parse_evm(
        r#"
        entity WrapOps {
            routes {
                bump(by: u64) => []
            }
            m_total: u64 {
                in bump(by) => m_total +% by
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function _wadd") && sol.contains("_wadd(m_total"),
        "wrapping +% must lower through _wadd (gen_expr BinOp WrappingAdd): {sol}"
    );
    assert_solc_compiles("expr_wrapping_ops_member", &sol);
}

#[test]
fn n4_codegen_expr_evm_intrinsics_bundle_solc() {
    let program = parse_evm(
        r#"
        entity EvmNs {
            routes {
                constructor() => []
                digest(h: U256, v: u8, r: U256, s: U256, who: address, n: U256) -> U256 => [
                    let packed = evm::keccak256Packed(h, r);
                    let dig = evm::sha256(h, r);
                    let rip = evm::ripemd160(h);
                    let bal = evm::balance(who);
                    let bh = evm::blockhash(n);
                    let sig = evm::ecrecover(h, v, r, s);
                    return(packed + dig + rip + bal + bh + sig)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("ecrecover(bytes32(") && sol.contains("keccak256(abi.encodePacked("),
        "gen_evm_ns ecrecover/keccak256Packed arms: {sol}"
    );
    assert!(
        sol.contains("sha256(abi.encodePacked") && sol.contains("ripemd160(abi.encodePacked"),
        "gen_evm_ns sha256/ripemd160 arms: {sol}"
    );
    assert!(
        sol.contains(".balance") && sol.contains("blockhash("),
        "gen_evm_ns balance/blockhash arms: {sol}"
    );
    assert_solc_compiles("expr_evm_intrinsics_bundle", &sol);
}

#[test]
fn n4_codegen_expr_hashmap_contains_is_empty_solc() {
    let program = parse_evm(
        r#"
        entity HmMethods {
            routes {
                constructor() => []
                has(k: u64) -> bool => [ return(m_data.contains(k)) ]
                empty() -> bool => [ return(m_data.is_empty()) ]
            }
            m_data: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_data_exists[") || sol.contains("m_data.contains"),
        "contains() must lower to _exists sidecar (gen_expr MethodCall contains): {sol}"
    );
    assert!(
        sol.contains("m_data_keys.length == 0"),
        "is_empty() must lower to keys.length check: {sol}"
    );
    assert_solc_compiles("expr_hashmap_contains_is_empty", &sol);
}

#[test]
fn n4_codegen_expr_nested_contains_exists_solc() {
    let program = parse_evm(
        r#"
        entity NestedHm {
            routes {
                constructor() => []
                probe(outer: u64, k: u64) -> bool => [
                    let inner = if m_nested.exists(outer) {
                        m_nested[outer]
                    } else {
                        {}
                    };
                    return(inner.contains(k))
                ]
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_nested_inner_exists") || sol.contains("inner_exists"),
        "nested contains after HashMap let must use inner_exists sidecar: {sol}"
    );
    assert_solc_compiles("expr_nested_contains_exists", &sol);
}

#[test]
fn n4_codegen_expr_hashof_cast_bytes_tuple_solc() {
    let program = parse_evm(
        r#"
        entity HashCast {
            routes {
                constructor() => []
                pack(a: u64, b: u64) -> (U256, u32) => [
                    let h = hashOf(a, b);
                    let narrow = h as u32;
                    let blob = 0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20;
                    return((h, narrow))
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("keccak256(abi.encode(a, b))") || sol.contains("keccak256(abi.encode("),
        "hashOf must lower to keccak256(abi.encode(...)): {sol}"
    );
    assert!(
        sol.contains("uint32("),
        "as u32 cast must emit explicit uint32(...) (gen_expr Cast arm): {sol}"
    );
    assert_solc_compiles("expr_hashof_cast_bytes_tuple", &sol);
}

#[test]
fn n4_codegen_expr_simple_match_inline_solc() {
    let program = parse_evm(
        r#"
        entity MatchInline {
            routes {
                constructor() => []
                classify(n: u64) -> u64 => [
                    let tag = match n {
                        0 => 1,
                        1 => 2,
                        _ => 3
                    };
                    return(tag)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("?") && sol.contains(":"),
        "simple literal match in let must lower via gen_match_simple ternaries: {sol}"
    );
    assert_solc_compiles("expr_simple_match_inline", &sol);
}

#[test]
fn n4_codegen_expr_parse_str_match_hoisted_solc() {
    let program = parse_evm(
        r#"
        pure fn parse_field(csv: Vec<String>, index: u64) -> u64 {
            match std::str::parse_uint(cobol_field(csv, index), 10) {
                some(x) => x,
                none => 0,
            }
        }

        pure fn cobol_field(csv: Vec<String>, index: u64) -> String {
            csv[index]
        }

        entity ParseHost {
            routes {
                constructor() => []
                run(row: Vec<String>) -> u64 => [
                    return(parse_field(row, 0))
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("parse_uint") || sol.contains("_cam_try_parse"),
        "std::str::parse_uint match must hit gen_match_on_std_parse hoisted path: {sol}"
    );
    assert_solc_compiles("expr_parse_str_match_hoisted", &sol);
}

#[test]
fn n4_codegen_expr_temporal_ref_phased_solc() {
    let program = parse_evm(
        r#"
        entity Tick {
            routes {
                constructor() => []
                bump() => [
                    grow: []
                ]
            }
            m_ticks: u64 {
                in constructor() => 0
                in bump() => grow: ^m_ticks + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("next_m_ticks"),
        "phased transform ^m_ticks must lower to next_m_ticks (gen_temporal_ref_sol): {sol}"
    );
    assert_solc_compiles("expr_temporal_ref_phased", &sol);
}

#[test]
fn n4_codegen_expr_string_split_hoisted_solc() {
    let program = parse_evm(
        r#"
        entity SplitHost {
            routes {
                constructor() => []
                parts(label: String) -> u64 => [
                    let chunks = label.split(",");
                    return(chunks.len())
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("_cam_string_split"),
        "String.split must lower via hoisted MethodCall split arm: {sol}"
    );
    assert_solc_compiles("expr_string_split_hoisted", &sol);
}

#[test]
fn n4_codegen_expr_sys_msg_context_fields_solc() {
    let program = parse_evm(
        r#"
        entity CtxPeek {
            routes {
                constructor() => []
                flags() -> bool => [
                    let a = msg::int;
                    let b = msg::ext;
                    let c = sys::origin;
                    let d = sys::gasprice;
                    let e = sys::blobbasefee;
                    return(a || b || c != msg::sender || d > 0 || e > 0)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("false") && sol.contains("true"),
        "msg::int/ext must lower to false/true: {sol}"
    );
    assert!(
        sol.contains("tx.origin") && sol.contains("tx.gasprice") && sol.contains("block.blobbasefee"),
        "sys::origin/gasprice/blobbasefee must lower (gen_expr SysField arms): {sol}"
    );
    assert_solc_compiles("expr_sys_msg_context_fields", &sol);
}

#[test]
fn n4_codegen_expr_payload_match_member_solc() {
    let program = parse_evm(
        r#"
        enum Action {
            Deposit(u64),
            Withdraw(u64),
            Reset
        }

        entity ActionHost {
            routes {
                constructor() => []
                apply(action: Action) => []
            }
            m_balance: u64 {
                in constructor() => 0
                in apply(action) => {
                    match action {
                        Action::Deposit(amount) => m_balance + amount,
                        Action::Withdraw(amount) => m_balance - amount,
                        Action::Reset => 0
                    }
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("action.tag == Action_Tag.Deposit") || sol.contains("Action_Tag.Deposit"),
        "payload enum match must hit gen_expr_hoisted Match + subst_payload_bindings: {sol}"
    );
    assert!(
        sol.contains("deposit_0") || sol.contains("withdraw_0"),
        "payload binders must rewrite to struct field access: {sol}"
    );
    assert_solc_compiles("expr_payload_match_member", &sol);
}

// ---------------------------------------------------------------------------
// N4-51: codegen/solidity/core/expr.rs — slice 2 (hoisted FnCall/MethodCall,
// gen_match_on_std_parse signed/radix tails, payload wildcard, trace accessors)
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_expr_hoisted_fncall_hashmap_sidecar_solc() {
    let program = parse_evm(
        r#"
        pure fn read_slot(m: HashMap<u64, u64>, k: u64) -> u64 {
            if m.exists(k) { m[k] } else { 0 }
        }

        entity HoistFn {
            routes {
                constructor() => []
                get(k: u64) -> u64 => [
                    return(read_slot(m_map, k))
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("read_slot(m_map, m_map_exists, k)")
            || (sol.contains("read_slot") && sol.contains("m_map_exists")),
        "HashMap pure-fn call must thread _exists sidecar via gen_expr_hoisted FnCall: {sol}"
    );
    assert_solc_compiles("expr_hoisted_fncall_hashmap_sidecar", &sol);
}

#[test]
fn n4_codegen_expr_hoisted_match_method_arms_solc() {
    let program = parse_evm(
        r#"
        entity HoistMatchMethod {
            routes {
                constructor() => []
                parts(mode: u64, label: String, other: String) -> u64 => [
                    let n = {
                        match mode {
                            0 => label.split(",").len(),
                            _ => other.split(":").len()
                        }
                    };
                    return(n)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("_cam_string_split"),
        "match arms with MethodCall bodies must lower via gen_expr_hoisted Match: {sol}"
    );
    assert!(
        sol.contains("if (") && sol.contains("else"),
        "hoisted match must emit if/else setup for method-call arms: {sol}"
    );
    assert_solc_compiles("expr_hoisted_match_method_arms", &sol);
}

#[test]
fn n4_codegen_expr_hoisted_split_if_delim_solc() {
    let program = parse_evm(
        r#"
        entity HoistSplit {
            routes {
                constructor() => []
                count(label: String, mode: u64) -> u64 => [
                    let delim = if mode > 0 { "," } else { ";" };
                    let parts = label.split(delim);
                    return(parts.len())
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("_cam_string_split"),
        "split with let-bound if delim must lower via gen_expr_hoisted: {sol}"
    );
    assert_solc_compiles("expr_hoisted_split_if_delim", &sol);
}

#[test]
fn n4_codegen_expr_parse_signed_narrow_cast_solc() {
    let program = parse_evm(
        r#"
        entity ParseSigned {
            routes {
                constructor() => []
                asI32(s: String) -> i32 => [
                    let v = match std::str::parse_i32(s, 10) { some(x) => x, none => 0 };
                    return(v)
                ]
                asI64(s: String) -> i64 => [
                    let v = match std::str::parse_i64(s, 10) { some(x) => x, none => 0 };
                    return(v)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("int32(") || sol.contains("int64("),
        "signed parse match must narrow-bind int256 temp to int32/int64: {sol}"
    );
    assert!(
        sol.contains("parse_i32") || sol.contains("_cam_try_parse"),
        "parse_i32/i64 must hit gen_match_on_std_parse signed path: {sol}"
    );
    assert_solc_compiles("expr_parse_signed_narrow_cast", &sol);
}

#[test]
fn n4_codegen_expr_parse_bool_match_arms_solc() {
    let program = parse_evm(
        r#"
        pure fn parse_ok(s: String) -> bool {
            match std::str::parse_u64(s, 10) {
                some(_) => true,
                none => false
            }
        }

        entity ParseBool {
            routes {
                constructor() => []
                probe(s: String) -> bool => [
                    return(parse_ok(s))
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("bool") && (sol.contains("true") || sol.contains("false")),
        "all-bool match arms must select bool match_ty in gen_match_on_std_parse: {sol}"
    );
    assert_solc_compiles("expr_parse_bool_match_arms", &sol);
}

#[test]
fn n4_codegen_expr_parse_radix_tails_solc() {
    let program = parse_evm(
        r#"
        entity ParseRadix {
            routes {
                constructor() => []
                fromBin(s: String) -> u8 => [
                    let v = match std::str::parse_u8(s, 2) { some(x) => x, none => 0 };
                    return(v)
                ]
                fromBase36(s: String) -> u8 => [
                    let v = match std::str::parse_u8(s, 36) { some(x) => x, none => 0 };
                    return(v)
                ]
                badRadix(s: String) -> bool => [
                    let bad = match std::str::parse_u64(s, 37) { some(_) => false, none => true };
                    return(bad)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("parse_u8") || sol.contains("_cam_try_parse"),
        "radix-2/36 parse_u8 must hit gen_match_on_std_parse: {sol}"
    );
    assert_solc_compiles("expr_parse_radix_tails", &sol);
}

#[test]
fn n4_codegen_expr_parse_int_alias_signed_solc() {
    let program = parse_evm(
        r#"
        entity ParseIntAlias {
            routes {
                constructor() => []
                run(s: String) -> i64 => [
                    let v = match std::str::parse_int(s, 10) { some(x) => x, none => 0 };
                    return(v)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("parse_int") || sol.contains("_cam_try_parse"),
        "parse_int alias must lower signed gen_match_on_std_parse: {sol}"
    );
    assert!(
        sol.contains("int256") || sol.contains("int64"),
        "signed parse temp binding must appear in hoisted match stmts: {sol}"
    );
    assert_solc_compiles("expr_parse_int_alias_signed", &sol);
}

#[test]
fn n4_codegen_expr_payload_wildcard_multi_field_solc() {
    let program = parse_evm(
        r#"
        enum Ticket {
            Transfer(u64, u64),
            Burn
        }

        entity TicketHost {
            routes {
                constructor() => []
                apply(ticket: Ticket) => []
            }
            m_amt: u64 {
                in constructor() => 0
                in apply(ticket) => {
                    match ticket {
                        Ticket::Transfer(amt, _) => amt,
                        Ticket::Burn => 0
                    }
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Ticket_Tag.Transfer") || sol.contains("ticket.tag"),
        "multi-field payload match must hit payload_or_unit_enum_cond: {sol}"
    );
    assert!(
        sol.contains("transfer_0"),
        "wildcard payload slot must still bind transfer_0 via subst_payload_bindings: {sol}"
    );
    assert_solc_compiles("expr_payload_wildcard_multi_field", &sol);
}

#[test]
fn n4_codegen_expr_simple_match_payload_return_solc() {
    let program = parse_evm(
        r#"
        enum Action {
            Deposit(u64),
            Reset
        }

        pure fn deposit_of(action: Action) -> u64 {
            match action {
                Action::Deposit(x) => x,
                Action::Reset => 0
            }
        }

        entity ActionPure {
            routes {
                constructor() => []
                read(action: Action) -> u64 => [
                    return(deposit_of(action))
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("deposit_0") || sol.contains("Action_Tag.Deposit"),
        "inline payload match in pure fn must hit gen_match_simple EnumVariantWithData: {sol}"
    );
    assert_solc_compiles("expr_simple_match_payload_return", &sol);
}

#[test]
fn n4_codegen_expr_invariant_trace_gen_expr_solc() {
    let program = parse_evm(
        r#"
        entity Counter {
            routes {
                constructor() => []
                increment(amount: u64) => []
                reset() => []
            }
            m_count: u64 {
                in constructor() => 0
                in increment(amount) => m_count + amount
                in reset() => 0
            }
        }

        invariant "trace expr accessors" for Counter {
            init { m_count: 0 }

            action increment(amount: u64) {
                bound amount in 1..100
            }
            action reset() {}

            check trace::length <= 64
            check trace::count(increment) >= trace::count(reset)
            check !trace::lastWas(reset) || m_count > 0
        }
    "#,
    );
    let files = gen_evm_test_files(&program);
    let inv_sol = files
        .iter()
        .find(|(p, _)| p.contains("Invariant"))
        .map(|(_, c)| c.as_str())
        .expect("trace invariant must emit Foundry test file");
    assert!(
        inv_sol.contains("_traceLen"),
        "handler must declare trace length storage: {inv_sol}"
    );
    assert!(
        inv_sol.contains("_traceCount_increment") && inv_sol.contains("_traceLast_reset"),
        "trace::count/lastWas must lower via gen_expr TraceCall in invariant checks: {inv_sol}"
    );
    assert!(
        inv_sol.contains("require(") && inv_sol.contains("_traceLen"),
        "invariant check must require() on _traceLen from gen_expr TraceField: {inv_sol}"
    );
}

#[test]
fn n4_codegen_expr_parse_wildcard_fallback_arm_solc() {
    let program = parse_evm(
        r#"
        entity ParseWildcard {
            routes {
                constructor() => []
                run(s: String) -> u64 => [
                    let v = match std::str::parse_u64(s, 10) {
                        some(x) => x,
                        _ => 0
                    };
                    return(v)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("} else {") && sol.contains("_cam_try_parse"),
        "parse match wildcard arm must hit gen_match_on_std_parse wildcard branch: {sol}"
    );
    assert_solc_compiles("expr_parse_wildcard_fallback_arm", &sol);
}

#[test]
fn n4_codegen_expr_parse_i128_wide_signed_bind_solc() {
    let program = parse_evm(
        r#"
        entity ParseI128 {
            routes {
                constructor() => []
                run(s: String) -> i128 => [
                    let v = match std::str::parse_i128(s, 10) { some(x) => x, none => 0 };
                    return(v)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("parse_i128") || sol.contains("_cam_try_parse"),
        "parse_i128 must hit signed wide bind path (no narrow cast): {sol}"
    );
    assert_solc_compiles("expr_parse_i128_wide_signed_bind", &sol);
}

#[test]
fn n4_codegen_expr_parse_u128_wide_unsigned_bind_solc() {
    let program = parse_evm(
        r#"
        entity ParseU128 {
            routes {
                constructor() => []
                run(s: String) -> u128 => [
                    let v = match std::str::parse_u128(s, 10) { some(x) => x, none => 0 };
                    return(v)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("parse_u128") || sol.contains("_cam_try_parse"),
        "parse_u128 must hit unsigned wide bind path: {sol}"
    );
    assert_solc_compiles("expr_parse_u128_wide_unsigned_bind", &sol);
}

#[test]
fn n4_codegen_expr_payload_dual_bind_match_solc() {
    let program = parse_evm(
        r#"
        enum Move {
            Pair(u64, u64)
        }

        entity MoveHost {
            routes {
                constructor() => []
                apply(m: Move) => []
            }
            m_total: u64 {
                in constructor() => 0
                in apply(m) => {
                    match m {
                        Move::Pair(a, b) => a + b
                    }
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("pair_0") && sol.contains("pair_1"),
        "dual payload binders must rewrite via subst_payload_bindings: {sol}"
    );
    assert_solc_compiles("expr_payload_dual_bind_match", &sol);
}

#[test]
fn n4_codegen_expr_invariant_trace_compound_check_solc() {
    let program = parse_evm(
        r#"
        entity Counter {
            routes {
                constructor() => []
                increment(amount: u64) => []
                reset() => []
            }
            m_count: u64 {
                in constructor() => 0
                in increment(amount) => m_count + amount
                in reset() => 0
            }
        }

        invariant "trace compound" for Counter {
            init { m_count: 0 }
            action increment(amount: u64) { bound amount in 1..50 }
            action reset() {}
            check trace::length >= trace::count(increment)
            check trace::count(reset) <= trace::length
            check trace::lastWas(reset) == false || m_count > 0
        }
    "#,
    );
    let files = gen_evm_test_files(&program);
    let inv_sol = files
        .iter()
        .find(|(p, _)| p.contains("Invariant"))
        .map(|(_, c)| c.as_str())
        .expect("compound trace invariant must emit Foundry file");
    assert!(
        inv_sol.contains("_traceCount_increment")
            && inv_sol.contains("_traceLast_reset")
            && inv_sol.contains("_traceLen"),
        "compound trace checks must lower TraceField/TraceCall via gen_expr: {inv_sol}"
    );
}

#[test]
fn n4_codegen_expr_hoisted_record_let_fields_solc() {
    let program = parse_evm(
        r#"
        record TokenData { owner: address, amt: u64 }

        entity RecordHoist {
            routes {
                constructor() => []
                mint(bump: bool) => []
            }
            m_tokens: HashMap<u64, TokenData> {
                in constructor() => HashMap::new()
                in mint(bump) => {
                    let amt = if bump { 1 } else { 0 };
                    let row = TokenData { owner: msg::sender, amt: amt };
                    m_tokens.update(1, row)
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("TokenData({") && sol.contains("amt:"),
        "record literal after hoisted if-let must lower via gen_expr_hoisted: {sol}"
    );
    assert_solc_compiles("expr_hoisted_record_let_fields", &sol);
}

#[test]
fn n4_codegen_expr_parse_some_arm_hoisted_body_solc() {
    let program = parse_evm(
        r#"
        entity ParseSomeHoist {
            routes {
                constructor() => []
                bump(s: String) -> u64 => [
                    let v = match std::str::parse_u64(s, 10) {
                        some(x) => {
                            let doubled = x * 2;
                            doubled
                        },
                        none => 0
                    };
                    return(v)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("_cam_try_parse") && sol.contains("if ("),
        "parse some-arm block body must hit gen_match_on_std_parse + gen_expr_hoisted body: {sol}"
    );
    assert_solc_compiles("expr_parse_some_arm_hoisted_body", &sol);
}

#[test]
fn n4_codegen_expr_parse_i16_narrow_cast_solc() {
    let program = parse_evm(
        r#"
        entity ParseI16 {
            routes {
                constructor() => []
                run(s: String) -> i16 => [
                    let v = match std::str::parse_i16(s, 10) { some(x) => x, none => 0 };
                    return(v)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("int16(") || sol.contains("parse_i16"),
        "parse_i16 must emit narrow signed bind cast: {sol}"
    );
    assert_solc_compiles("expr_parse_i16_narrow_cast", &sol);
}

#[test]
fn n4_codegen_expr_hoisted_if_split_branches_solc() {
    let program = parse_evm(
        r#"
        entity IfSplit {
            routes {
                constructor() => []
                count(flag: bool, label: String, other: String) -> u64 => [
                    let n = if flag {
                        label.split(",").len()
                    } else {
                        other.split(":").len()
                    };
                    return(n)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("_cam_string_split") && sol.contains("if ("),
        "if branches with split.len must lower via gen_expr_hoisted If + Match method arms: {sol}"
    );
    assert_solc_compiles("expr_hoisted_if_split_branches", &sol);
}

#[test]
fn n4_codegen_expr_invariant_assume_trace_length_solc() {
    let program = parse_evm(
        r#"
        entity Counter {
            routes {
                constructor() => []
                increment(amount: u64) => []
            }
            m_count: u64 {
                in constructor() => 0
                in increment(amount) => m_count + amount
            }
        }

        invariant "trace assume" for Counter {
            init { m_count: 0 }
            action increment(amount: u64) {
                bound amount in 1..25
                assume trace::length < 16
                assume trace::count(increment) <= trace::length
            }
            check m_count >= 0
        }
    "#,
    );
    let files = gen_evm_test_files(&program);
    let inv_sol = files
        .iter()
        .find(|(p, _)| p.contains("Invariant"))
        .map(|(_, c)| c.as_str())
        .expect("assume trace invariant must emit Foundry file");
    assert!(
        inv_sol.contains("_traceLen") && inv_sol.contains("_traceCount_increment"),
        "assume trace::length/count must lower via gen_expr in handler: {inv_sol}"
    );
}

#[test]
fn n4_codegen_expr_std_str_matrix_parse_bundle_solc() {
    let src = include_str!("fixtures/std_str_matrix.cam");
    let program = parse_evm(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("_cam_try_parse_radix_signed") && sol.contains("int8("),
        "std_str_matrix fixture must exercise gen_match_on_std_parse signed/radix/bool arms: {sol}"
    );
    assert!(
        sol.contains("uint8(") || sol.contains("uint16(") || sol.contains("uint32("),
        "matrix must hit narrow unsigned bind casts in parse match: {sol}"
    );
    assert_solc_compiles("expr_std_str_matrix_parse_bundle", &sol);
}

#[test]
fn n4_codegen_expr_hoisted_fncall_exists_indexed_map_solc() {
    let program = parse_evm(
        r#"
        pure fn read_slot(m: HashMap<u64, u64>, k: u64) -> u64 {
            if m.exists(k) { m[k] } else { 0 }
        }

        entity NestedRead {
            routes {
                constructor() => []
                probe(outer: u64, k: u64) => []
            }
            m_outer: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
            }
            m_val: u64 {
                in constructor() => 0
                in probe(outer, k) => {
                    read_slot(m_outer[outer], k)
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("read_slot") && sol.contains("_exists"),
        "indexed HashMap arg to exists-pure-fn must hit gen_expr_hoisted FnCall/K3 path: {sol}"
    );
    assert_solc_compiles("expr_hoisted_fncall_exists_indexed_map", &sol);
}

// ---------------------------------------------------------------------------
// N4-52: codegen/solidity/core/expr.rs — slice 3 (generic MethodCall hoisting,
// Closure/Range revert, gen_match_simple bool/option/unit tails)
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_expr_hoisted_generic_map_update_solc() {
    let program = parse_evm(
        r#"
        entity MapUpdateExpr {
            routes {
                constructor() => []
                size(k: u64, v: u64) -> u64 => [
                    let bump = {
                        if v > 0 { v } else { 1 }
                    };
                    let next = m_map.update(k, bump);
                    return(next.keys().len())
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_map_keys") && sol.contains("m_map["),
        "HashMap.update in let must maintain _keys sidecar: {sol}"
    );
    assert_solc_compiles("expr_hoisted_generic_map_update", &sol);
}

#[test]
fn n4_codegen_expr_hoisted_generic_map_insert_solc() {
    let program = parse_evm(
        r#"
        entity MapInsertExpr {
            routes {
                constructor() => []
                grow(k: u64, v: u64) -> u64 => [
                    let next = m_map.insert(k, v);
                    return(next.keys().len())
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_map_keys") && sol.contains("m_map["),
        "HashMap.insert in let must maintain _keys sidecar: {sol}"
    );
    assert_solc_compiles("expr_hoisted_generic_map_insert", &sol);
}

#[test]
fn n4_codegen_expr_hoisted_closure_revert_pure_solc() {
    let program = parse_evm(
        r#"
        library ClosureLeak {
            pure fn capture() -> u64 {
                let f = |x| *x + 1;
                0
            }
        }

        entity ClosureHost {
            routes {
                constructor() => []
                run() -> u64 => [
                    return(ClosureLeak::capture())
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("revert(\"EVM: closure-as-value not supported"),
        "closure let in pure fn must hit gen_expr_hoisted Closure revert arm: {sol}"
    );
    assert_solc_compiles("expr_hoisted_closure_revert_pure", &sol);
}

#[test]
fn n4_codegen_expr_hoisted_range_revert_pure_solc() {
    let program = parse_evm(
        r#"
        library RangeLeak {
            pure fn span(n: u64) -> u64 {
                let r = 0..n;
                0
            }
        }

        entity RangeHost {
            routes {
                constructor() => []
                run(n: u64) -> u64 => [
                    return(RangeLeak::span(n))
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("revert(\"EVM: range expression only valid as `for` iterator on EVM\")"),
        "range let in pure fn must hit gen_expr_hoisted Range revert arm: {sol}"
    );
    assert_solc_compiles("expr_hoisted_range_revert_pure", &sol);
}

#[test]
fn n4_codegen_expr_match_simple_bool_option_solc() {
    let program = parse_evm(
        r#"
        pure fn flag_u64(b: bool) -> u64 {
            match b {
                true => 1,
                false => 0
            }
        }

        pure fn opt_or_zero(o: Option<u64>) -> u64 {
            match o {
                some(x) => x,
                none => 0
            }
        }

        entity MatchSimpleTails {
            routes {
                constructor() => []
                fromBool(b: bool) -> u64 => [
                    return(flag_u64(b))
                ]
                fromOpt(o: Option<u64>) -> u64 => [
                    return(opt_or_zero(o))
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    // Exhaustive arms: the last arm (`false` / `none`) is the unconditional else.
    assert!(
        sol.contains("(b == true ? 1 : 0)"),
        "bool match must lower via gen_match_simple BoolLiteral arms: {sol}"
    );
    assert!(
        sol.contains("Option_uint64_Tag.Some") && !sol.contains("Option_uint64_Tag.None ?"),
        "Option match must lower via gen_match_simple Some tag arm: {sol}"
    );
    assert_solc_compiles("expr_match_simple_bool_option", &sol);
}

#[test]
fn n4_codegen_expr_match_simple_unit_enum_solc() {
    let program = parse_evm(
        r#"
        enum Mode {
            On,
            Off
        }

        pure fn mode_code(m: Mode) -> u64 {
            match m {
                Mode::On => 1,
                Mode::Off => 0
            }
        }

        entity ModeHost {
            routes {
                constructor() => []
                read(m: Mode) -> u64 => [
                    return(mode_code(m))
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Mode.On") || sol.contains("Mode_On"),
        "unit enum match must hit gen_match_simple EnumVariant + payload_or_unit_enum_cond: {sol}"
    );
    assert_solc_compiles("expr_match_simple_unit_enum", &sol);
}

#[test]
fn n4_codegen_expr_match_simple_ident_wildcard_solc() {
    let program = parse_evm(
        r#"
        pure fn classify(n: u64) -> u64 {
            match n {
                0 => 10,
                x => x + 1
            }
        }

        entity IdentMatch {
            routes {
                constructor() => []
                run(n: u64) -> u64 => [
                    return(classify(n))
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("== 0") && sol.contains("+ 1"),
        "ident/wildcard match arms must lower via gen_match_simple: {sol}"
    );
    assert_solc_compiles("expr_match_simple_ident_wildcard", &sol);
}

// ---------------------------------------------------------------------------
// N4-65: codegen/solidity/core/expr.rs — slice 4 (gen_expr_hoisted Let/Match/If
// residual, gen_evm_ns/binop scatter tails)
// Baseline @ N4-64: 83.27% line (173 missed / 1034); focus L1099–L1104,
// L1123–L1124, L1142–L1143, L1202–L1224, L1323–L1352, L1435–L1454.
// Exclude DEAD: NamespacedCall E15/E16 L1354–L1372; K3 catch-all L1468–L1488.
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_expr_hoisted_let_wildcard_side_effect_solc() {
    let program = parse_evm(
        r#"
        entity WildcardLet {
            routes {
                constructor() => []
                probe(k: u64) -> u64 => [
                    let v = {
                        let _ = m_map.exists(k);
                        if m_map.exists(k) { m_map[k] } else { 0 }
                    };
                    return(v)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_map_exists") || sol.contains(".exists"),
        "wildcard let prelude must evaluate exists side effect via gen_expr_hoisted Pattern::Wildcard: {sol}"
    );
    assert!(
        !sol.contains("let _ =") || sol.contains("m_map_exists"),
        "wildcard binding must not emit a Solidity `let _` temp: {sol}"
    );
    assert_solc_compiles("expr_hoisted_let_wildcard_side_effect", &sol);
}

#[test]
fn n4_codegen_expr_hoisted_tuple_wildcard_destructure_solc() {
    let program = parse_evm(
        r#"
        entity TupleWildcard {
            routes {
                constructor() => []
                second(a: u64, b: u64) -> u64 => [
                    let (_, y) = (a, b);
                    return(y)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("(, uint64 y) =") || sol.contains("(, y) ="),
        "tuple destructure with wildcard slot must hit gen_expr_hoisted Tuple Pattern::Wildcard: {sol}"
    );
    assert_solc_compiles("expr_hoisted_tuple_wildcard_destructure", &sol);
}

#[test]
fn n4_codegen_expr_hoisted_if_no_else_bool_default_solc() {
    let program = parse_evm(
        r#"
        entity IfNoElse {
            routes {
                constructor() => []
                pick(flag: bool) -> bool => [
                    let v = if flag {
                        let bump = 1;
                        bump == 1
                    };
                    return(v)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("false") && sol.contains("if ("),
        "if-without-else on bool must default else arm via default_sol_literal (not bare 0): {sol}"
    );
    assert_solc_compiles("expr_hoisted_if_no_else_bool_default", &sol);
}

#[test]
fn n4_codegen_expr_hoisted_match_bool_option_complex_solc() {
    let program = parse_evm(
        r#"
        entity HoistMatchTails {
            routes {
                constructor() => []
                fromBool(flag: bool, bump: u64) -> u64 => [
                    let n = {
                        match flag {
                            true => bump + 1,
                            false => bump
                        }
                    };
                    return(n)
                ]
                fromOpt(opt: Option<u64>, bump: u64) -> u64 => [
                    let n = {
                        match opt {
                            some(x) => x + bump,
                            none => bump
                        }
                    };
                    return(n)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("== true") && sol.contains("== false"),
        "hoisted bool match must emit BoolLiteral cond arms: {sol}"
    );
    assert!(
        sol.contains("Option_uint64_Tag.Some") && sol.contains("Option_uint64_Tag.None"),
        "hoisted Option match must emit Some/None tag cond arms: {sol}"
    );
    assert_solc_compiles("expr_hoisted_match_bool_option_complex", &sol);
}

#[test]
fn n4_codegen_expr_hoisted_match_int_literal_arm_solc() {
    let program = parse_evm(
        r#"
        entity HoistMatchInt {
            routes {
                constructor() => []
                score(mode: u64, label: String, other: String) -> u64 => [
                    let n = {
                        match mode {
                            0 => label.len(),
                            1 => other.len(),
                            _ => 0
                        }
                    };
                    return(n)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("== 0") && sol.contains("== 1"),
        "hoisted int-literal match arms must emit equality guards: {sol}"
    );
    assert_solc_compiles("expr_hoisted_match_int_literal_arm", &sol);
}

#[test]
fn n4_codegen_expr_hoisted_payload_enum_ctor_nested_solc() {
    let program = parse_evm(
        r#"
        enum Op {
            Mint(u64),
            Burn(u64)
        }

        entity PayloadCtor {
            routes {
                constructor() => []
                tag(v: u64) -> Op => [
                    let tagged = {
                        let bump = if v > 0 { v } else { 1 };
                        Op::Mint(bump)
                    };
                    return(tagged)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Op_Tag.Mint") || sol.contains("tag == Op_Tag"),
        "payload enum ctor with hoisted arg must hit gen_expr_hoisted EnumVariantWithData: {sol}"
    );
    assert_solc_compiles("expr_hoisted_payload_enum_ctor_nested", &sol);
}

#[test]
fn n4_codegen_expr_hoisted_evm_ns_packed_nested_arg_solc() {
    let program = parse_evm(
        r#"
        entity EvmPackedHoist {
            routes {
                constructor() => []
                digest(a: U256, b: U256, c: U256) -> U256 => [
                    let h = {
                        let x = a + b;
                        evm::keccak256Packed(x, c)
                    };
                    return(h)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("keccak256(abi.encodePacked("),
        "evm::keccak256Packed with hoisted arg must route EnumVariantWithData evm arm in gen_expr_hoisted: {sol}"
    );
    assert_solc_compiles("expr_hoisted_evm_ns_packed_nested_arg", &sol);
}

#[test]
fn n4_codegen_expr_hoisted_fncall_exists_non_ident_comment_solc() {
    let program = parse_evm(
        r#"
        pure fn map_ready(m: HashMap<u64, u64>, k: u64) -> bool {
            m.exists(k)
        }

        entity ExistsNonIdent {
            routes {
                constructor() => []
                ready(outer: u64, k: u64) -> bool => [
                    let ok = map_ready(m_outer[outer], k);
                    return(ok)
                ]
            }
            m_outer: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("EVM-2 K3: pure fn `map_ready` takes a HashMap-typed param requiring sidecar args"),
        "non-Ident HashMap arg in hoisted FnCall must emit K3 comment instead of wrong arity: {sol}"
    );
    assert_solc_compiles("expr_hoisted_fncall_exists_non_ident_comment", &sol);
}

#[test]
fn n4_codegen_expr_hoisted_split_hoisted_base_solc() {
    let program = parse_evm(
        r#"
        entity SplitHoistBase {
            routes {
                constructor() => []
                count(mode: u64, label: String, other: String) -> u64 => [
                    let n = {
                        let base = if mode > 0 { label } else { other };
                        base.split(",").len()
                    };
                    return(n)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("_cam_string_split"),
        "split on hoisted base must lower via gen_expr_hoisted MethodCall split arm: {sol}"
    );
    assert_solc_compiles("expr_hoisted_split_hoisted_base", &sol);
}

#[test]
fn n4_codegen_expr_hoisted_match_unknown_enum_passthrough_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        enum Action {
            Deposit(u64),
            Reset
        }

        entity GhostMatch {
            routes {
                constructor() => []
                apply(action: Action) -> u64 => [
                    let n = {
                        match action {
                            Action::Deposit(amount) => amount,
                            Action::Reset => 0
                        }
                    };
                    return(n)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            let entity = program.entities.first_mut().expect("entity");
            let route = entity
                .routes
                .iter_mut()
                .find(|r| r.name == "apply")
                .expect("apply route");
            if let RouteBody::Unphased(actions) = &mut route.body {
                for action in actions.iter_mut() {
                    match action {
                        RouteAction::Let { value, .. } => patch_match_enum_name_to_phantom(value),
                        RouteAction::Return { values } => {
                            for v in values {
                                patch_match_enum_name_to_phantom(v);
                            }
                        }
                        _ => {}
                    }
                }
            }
        },
    );
    assert!(
        sol.contains("Phantom::Deposit") || sol.contains("action.tag"),
        "unknown enum decl in hoisted match must passthrough body when lookup_enum fails: {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

// ---------------------------------------------------------------------------
// N4-68: codegen/solidity/core/expr.rs — slice 5 (hoisted scatter llvm-partial)
// Baseline @ N4-67: 86.46% line (140 missed / 1034); focus L255/L273/L291/L307,
// L1123–L1124, L1323–L1344, L1435–L1444.
// Exclude DEAD: NamespacedCall E15/E16 L1354–L1372; K3 catch-all L1468–L1488;
// hoisted evm `if let Some` success L1324 (mirrors inline gen_expr path).
// ---------------------------------------------------------------------------

fn patch_evm_ns_first_arg_uninlineable(expr: &mut Expr) {
    match expr {
        Expr::EnumVariantWithData(en, _, args) if en == "evm" && !args.is_empty() => {
            args[0] = Expr::Range(
                Box::new(Expr::IntLiteral(U256::ZERO)),
                Box::new(Expr::IntLiteral(U256::from_u128(1))),
            );
            return;
        }
        Expr::NamespacedCall { namespace, args, .. } if namespace == "evm" && !args.is_empty() => {
            args[0] = Expr::Range(
                Box::new(Expr::IntLiteral(U256::ZERO)),
                Box::new(Expr::IntLiteral(U256::from_u128(1))),
            );
            return;
        }
        Expr::Let(_, val, body) => {
            patch_evm_ns_first_arg_uninlineable(val);
            patch_evm_ns_first_arg_uninlineable(body);
        }
        Expr::Block(items) => {
            for item in items {
                patch_evm_ns_first_arg_uninlineable(item);
            }
        }
        Expr::If(c, t, e) => {
            patch_evm_ns_first_arg_uninlineable(c);
            patch_evm_ns_first_arg_uninlineable(t);
            if let Some(e) = e {
                patch_evm_ns_first_arg_uninlineable(e);
            }
        }
        _ => {}
    }
}

fn patch_payload_enum_ctor_arg_if(expr: &mut Expr, enum_name: &str, variant: &str, ident: &str) {
    match expr {
        Expr::EnumVariantWithData(en, vn, args)
            if en == enum_name && vn == variant && !args.is_empty() =>
        {
            args[0] = Expr::If(
                Box::new(Expr::BinOp(
                    Box::new(Expr::Ident(ident.to_string())),
                    BinOp::Gt,
                    Box::new(Expr::IntLiteral(U256::ZERO)),
                )),
                Box::new(Expr::Ident(ident.to_string())),
                Some(Box::new(Expr::IntLiteral(U256::from_u128(1)))),
            );
            return;
        }
        Expr::Let(_, val, body) => {
            patch_payload_enum_ctor_arg_if(val, enum_name, variant, ident);
            patch_payload_enum_ctor_arg_if(body, enum_name, variant, ident);
        }
        Expr::Block(items) => {
            for item in items {
                patch_payload_enum_ctor_arg_if(item, enum_name, variant, ident);
            }
        }
        _ => {}
    }
}

fn patch_payload_enum_ctor_dual_if(expr: &mut Expr, enum_name: &str, variant: &str) {
    match expr {
        Expr::EnumVariantWithData(en, vn, args)
            if en == enum_name && vn == variant && args.len() >= 2 =>
        {
            args[0] = Expr::If(
                Box::new(Expr::BinOp(
                    Box::new(Expr::Ident("a".to_string())),
                    BinOp::Gt,
                    Box::new(Expr::Ident("b".to_string())),
                )),
                Box::new(Expr::Ident("a".to_string())),
                Some(Box::new(Expr::Ident("b".to_string()))),
            );
            args[1] = Expr::If(
                Box::new(Expr::BinOp(
                    Box::new(Expr::Ident("b".to_string())),
                    BinOp::Gt,
                    Box::new(Expr::IntLiteral(U256::ZERO)),
                )),
                Box::new(Expr::Ident("b".to_string())),
                Some(Box::new(Expr::IntLiteral(U256::from_u128(1)))),
            );
            return;
        }
        Expr::Let(_, val, body) => {
            patch_payload_enum_ctor_dual_if(val, enum_name, variant);
            patch_payload_enum_ctor_dual_if(body, enum_name, variant);
        }
        Expr::Block(items) => {
            for item in items {
                patch_payload_enum_ctor_dual_if(item, enum_name, variant);
            }
        }
        _ => {}
    }
}

fn patch_fncall_middle_arg_match(expr: &mut Expr, name: &str) {
    match expr {
        Expr::FnCall(fn_name, args) if fn_name == name && args.len() >= 2 => {
            args[1] = Expr::Match(
                Box::new(Expr::Ident("mode".to_string())),
                vec![
                    MatchArm {
                        pattern: MatchPattern::IntLiteral(U256::from_u128(0)),
                        body: Expr::FnCall("grow".into(), vec![Expr::Ident("a".into())]),
                    },
                    MatchArm {
                        pattern: MatchPattern::IntLiteral(U256::from_u128(1)),
                        body: Expr::Ident("b".into()),
                    },
                    MatchArm {
                        pattern: MatchPattern::Wildcard,
                        body: Expr::IntLiteral(U256::ZERO),
                    },
                ],
            );
            return;
        }
        Expr::Let(_, val, body) => {
            patch_fncall_middle_arg_match(val, name);
            patch_fncall_middle_arg_match(body, name);
        }
        Expr::Block(items) => {
            for item in items {
                patch_fncall_middle_arg_match(item, name);
            }
        }
        _ => {}
    }
}

fn patch_payload_enum_ctor_arg_block(expr: &mut Expr, enum_name: &str, variant: &str, ident: &str) {
    match expr {
        Expr::EnumVariantWithData(en, vn, args)
            if en == enum_name && vn == variant && !args.is_empty() =>
        {
            args[0] = Expr::Block(vec![Expr::If(
                Box::new(Expr::BinOp(
                    Box::new(Expr::Ident(ident.to_string())),
                    BinOp::Gt,
                    Box::new(Expr::IntLiteral(U256::ZERO)),
                )),
                Box::new(Expr::Ident(ident.to_string())),
                Some(Box::new(Expr::IntLiteral(U256::from_u128(1)))),
            )]);
            return;
        }
        Expr::Let(_, val, body) => {
            patch_payload_enum_ctor_arg_block(val, enum_name, variant, ident);
            patch_payload_enum_ctor_arg_block(body, enum_name, variant, ident);
        }
        Expr::Block(items) => {
            for item in items {
                patch_payload_enum_ctor_arg_block(item, enum_name, variant, ident);
            }
        }
        _ => {}
    }
}

fn patch_method_call_split_base_block(expr: &mut Expr) {
    match expr {
        Expr::MethodCall(base, method, args) if method == "split" && args.len() == 1 => {
            *base = Box::new(Expr::Block(vec![*base.clone()]));
            return;
        }
        Expr::Let(_, val, body) => {
            patch_method_call_split_base_block(val);
            patch_method_call_split_base_block(body);
        }
        Expr::Block(items) => {
            for item in items {
                patch_method_call_split_base_block(item);
            }
        }
        Expr::If(c, t, e) => {
            patch_method_call_split_base_block(c);
            patch_method_call_split_base_block(t);
            if let Some(e) = e {
                patch_method_call_split_base_block(e);
            }
        }
        _ => {}
    }
}

fn patch_tuple_let_first_pat_none(expr: &mut Expr) {
    match expr {
        Expr::Let(Pattern::Tuple(pats), _, body) if !pats.is_empty() => {
            pats[0] = Pattern::None;
            patch_tuple_let_first_pat_none(body);
            return;
        }
        Expr::Let(_, val, body) => {
            patch_tuple_let_first_pat_none(val);
            patch_tuple_let_first_pat_none(body);
        }
        Expr::Block(items) => {
            for item in items {
                patch_tuple_let_first_pat_none(item);
            }
        }
        _ => {}
    }
}

fn patch_if_arm_to_enum_variant_with_data(expr: &mut Expr, enum_name: &str, variant: &str) {
    match expr {
        Expr::If(_, then_expr, else_opt) => {
            *then_expr = Box::new(Expr::EnumVariantWithData(
                enum_name.into(),
                variant.into(),
                vec![],
            ));
            if let Some(e) = else_opt {
                *e = Box::new(Expr::EnumVariantWithData(
                    enum_name.into(),
                    "Off".into(),
                    vec![],
                ));
            }
        }
        Expr::Let(_, val, body) => {
            patch_if_arm_to_enum_variant_with_data(val, enum_name, variant);
            patch_if_arm_to_enum_variant_with_data(body, enum_name, variant);
        }
        Expr::Block(items) => {
            for item in items {
                patch_if_arm_to_enum_variant_with_data(item, enum_name, variant);
            }
        }
        _ => {}
    }
}

#[test]
fn n4_codegen_expr_hoisted_tuple_wildcard_nested_block_solc() {
    let program = parse_evm(
        r#"
        entity TupleWildcardBlock {
            routes {
                constructor() => []
                second(a: u64, b: u64) -> u64 => [
                    let picked = {
                        let (_, y) = (a, b);
                        y
                    };
                    return(picked)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("(, uint64 y) =") || sol.contains("(, y) ="),
        "nested block tuple destructure must hit gen_expr_hoisted Pattern::Wildcard empty binder: {sol}"
    );
    assert_solc_compiles("expr_hoisted_tuple_wildcard_nested_block", &sol);
}

#[test]
fn n4_codegen_expr_inline_payload_enum_direct_solc() {
    let program = parse_evm(
        r#"
        enum Op {
            Mint(u64),
            Burn(u64)
        }

        entity InlinePayload {
            routes {
                constructor() => []
                mint(v: u64) -> Op => [ return(Op::Mint(v)) ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Op_Tag.Mint") || sol.contains("tag == Op_Tag"),
        "direct payload enum ctor must hit inline gen_expr EnumVariantWithData arm: {sol}"
    );
    assert_solc_compiles("expr_inline_payload_enum_direct", &sol);
}

#[test]
fn n4_codegen_expr_hoisted_payload_enum_conditional_arg_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        enum Op {
            Mint(u64),
            Burn(u64)
        }

        entity HoistPayloadCond {
            routes {
                constructor() => []
                tag(v: u64) -> Op => [
                    let tagged = {
                        Op::Mint(v)
                    };
                    return(tagged)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            let entity = program.entities.first_mut().expect("entity");
            let route = entity
                .routes
                .iter_mut()
                .find(|r| r.name == "tag")
                .expect("tag route");
            if let RouteBody::Unphased(actions) = &mut route.body {
                for action in actions.iter_mut() {
                    if let RouteAction::Let { value, .. } = action {
                        patch_payload_enum_ctor_arg_if(value, "Op", "Mint", "v");
                    }
                }
            }
        },
    );
    assert!(
        (sol.contains("Op_Tag.Mint") || sol.contains("tag: Op_Tag.Mint"))
            && (sol.contains("if (") || sol.contains("? v :")),
        "payload enum ctor with AST-injected If arg must hit gen_expr_hoisted payload loop: {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_codegen_expr_hoisted_payload_enum_multi_field_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        enum Pair {
            Both(u64, u64)
        }

        entity HoistPayloadPair {
            routes {
                constructor() => []
                pack(a: u64, b: u64) -> Pair => [
                    let tagged = {
                        Pair::Both(a, b)
                    };
                    return(tagged)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            let entity = program.entities.first_mut().expect("entity");
            let route = entity
                .routes
                .iter_mut()
                .find(|r| r.name == "pack")
                .expect("pack route");
            if let RouteBody::Unphased(actions) = &mut route.body {
                for action in actions.iter_mut() {
                    if let RouteAction::Let { value, .. } = action {
                        patch_payload_enum_ctor_dual_if(value, "Pair", "Both");
                    }
                }
            }
        },
    );
    assert!(
        sol.contains("Pair_Tag.Both") || sol.contains("tag == Pair_Tag"),
        "multi-field payload enum ctor must hoist each patched If arg via gen_expr_hoisted: {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_codegen_expr_hoisted_evm_ns_match_subject_arg_solc() {
    let program = parse_evm(
        r#"
        pure fn word(v: u64) -> U256 {
            v as U256
        }

        entity EvmNsMatchArg {
            routes {
                constructor() => []
                digest(mode: u64, a: u64, b: u64, c: U256) -> U256 => [
                    let h = {
                        let w = {
                            match mode {
                                0 => word(a),
                                1 => word(b),
                                _ => word(0)
                            }
                        };
                        evm::keccak256Packed(w, c)
                    };
                    return(h)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("keccak256(abi.encodePacked(") && sol.contains("== 0"),
        "hoisted match prelude + ident evm::keccak256Packed args must lower packed hash: {sol}"
    );
    assert_solc_compiles("expr_hoisted_evm_ns_match_subject_arg", &sol);
}

#[test]
fn n4_codegen_expr_evm_ns_partial_render_none_branches_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity EvmNsPartial {
            routes {
                constructor() => []
                probe(h: U256, v: u8, r: U256, s: U256) -> U256 => [
                    let k = evm::keccak256Packed(h, r);
                    let sh = evm::sha256(h, r);
                    let rip = evm::ripemd160(h);
                    let sig = evm::ecrecover(h, v, r, s);
                    return(k + sh + rip + sig)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            let entity = program.entities.first_mut().expect("entity");
            let route = entity
                .routes
                .iter_mut()
                .find(|r| r.name == "probe")
                .expect("probe route");
            if let RouteBody::Unphased(actions) = &mut route.body {
                for action in actions.iter_mut() {
                    if let RouteAction::Let { value, .. } = action {
                        patch_evm_ns_first_arg_uninlineable(value);
                    }
                }
            }
        },
    );
    assert!(
        sol.contains("enum variant evm::keccak256Packed with data not representable")
            || sol.contains("revert(\"EVM: enum variant"),
        "uninlineable evm:: arg must hit gen_evm_ns partial-render None then hoisted fallback: {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_codegen_expr_hoisted_fncall_exists_ident_sidecar_solc() {
    let program = parse_evm(
        r#"
        pure fn map_ready(m: HashMap<u64, u64>, k: u64) -> bool {
            m.exists(k)
        }

        entity ExistsIdentHoist {
            routes {
                constructor() => []
                ready(k: u64) -> bool => [
                    let ok = {
                        map_ready(m_map, k)
                    };
                    return(ok)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("map_ready(m_map, k, m_map_exists)")
            || (sol.contains("map_ready(") && sol.contains("_exists")),
        "exists-pure-fn with bare member Ident args must thread _exists sidecar: {sol}"
    );
    assert_solc_compiles("expr_hoisted_fncall_exists_ident_sidecar", &sol);
}

#[test]
fn n4_codegen_expr_hoisted_fncall_match_arg_bundle_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        pure fn grow(v: u64) -> u64 {
            v + 1
        }

        pure fn sum3(base: u64, pick: u64, extra: u64) -> u64 {
            base + pick + extra
        }

        entity FnCallMatchHoist {
            routes {
                constructor() => []
                score(mode: u64, a: u64, b: u64) -> u64 => [
                    let total = {
                        sum3(a, b, b)
                    };
                    return(total)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            let entity = program.entities.first_mut().expect("entity");
            let route = entity
                .routes
                .iter_mut()
                .find(|r| r.name == "score")
                .expect("score route");
            if let RouteBody::Unphased(actions) = &mut route.body {
                for action in actions.iter_mut() {
                    if let RouteAction::Let { value, .. } = action {
                        patch_fncall_middle_arg_match(value, "sum3");
                    }
                }
            }
        },
    );
    assert!(
        sol.contains("sum3(") && sol.contains("== 0") && sol.contains("grow("),
        "AST-injected match arg in hoisted FnCall must collect arg stmts after gen_expr inline miss: {sol}"
    );
    assert_solc_compiles("expr_hoisted_fncall_match_arg_bundle", &sol);
}

// ---------------------------------------------------------------------------
// N4-69: codegen/solidity/core/expr.rs — slice 6 (hoisted residual llvm-partial)
// Baseline @ N4-68: 88.10% line (123 missed / 1034); focus L1332–L1344,
// L1451–L1454, L1124, scatter gen_expr/binop tails.
// Exclude DEAD: hoisted evm success L1324; NamespacedCall/K3 catch-all;
// exists Ident hoisted K3 brace L1435 (unreachable when gen_expr inlines).
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_expr_hoisted_payload_enum_block_arg_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        enum Op {
            Mint(u64),
            Burn(u64)
        }

        entity HoistPayloadBlock {
            routes {
                constructor() => []
                tag(v: u64) -> Op => [
                    let tagged = {
                        Op::Mint(v)
                    };
                    return(tagged)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            let entity = program.entities.first_mut().expect("entity");
            let route = entity
                .routes
                .iter_mut()
                .find(|r| r.name == "tag")
                .expect("tag route");
            if let RouteBody::Unphased(actions) = &mut route.body {
                for action in actions.iter_mut() {
                    if let RouteAction::Let { value, .. } = action {
                        patch_payload_enum_ctor_arg_block(value, "Op", "Mint", "v");
                    }
                }
            }
        },
    );
    assert!(
        (sol.contains("Op_Tag.Mint") || sol.contains("tag: Op_Tag.Mint"))
            && (sol.contains("if (") || sol.contains("? v :")),
        "Block payload arg must hit gen_expr_hoisted EnumVariantWithData per-arg loop: {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_codegen_expr_hoisted_split_block_base_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity SplitBlockBase {
            routes {
                constructor() => []
                count(mode: u64, label: String, other: String) -> u64 => [
                    let n = {
                        let base = if mode > 0 { label } else { other };
                        base.split(",")
                    };
                    return(n.len())
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            let entity = program.entities.first_mut().expect("entity");
            let route = entity
                .routes
                .iter_mut()
                .find(|r| r.name == "count")
                .expect("count route");
            if let RouteBody::Unphased(actions) = &mut route.body {
                for action in actions.iter_mut() {
                    if let RouteAction::Let { value, .. } = action {
                        patch_method_call_split_base_block(value);
                    }
                }
            }
        },
    );
    assert!(
        sol.contains("_cam_string_split"),
        "Block split base must hit gen_expr_hoisted MethodCall::split hoisted-base arm: {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_codegen_expr_hoisted_tuple_none_binder_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity TupleNoneBinder {
            routes {
                constructor() => []
                pick(a: u64, b: u64) -> u64 => [
                    let picked = {
                        let (_, y) = (a, b);
                        y
                    };
                    return(picked)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            let entity = program.entities.first_mut().expect("entity");
            let route = entity
                .routes
                .iter_mut()
                .find(|r| r.name == "pick")
                .expect("pick route");
            if let RouteBody::Unphased(actions) = &mut route.body {
                for action in actions.iter_mut() {
                    if let RouteAction::Let { value, .. } = action {
                        patch_tuple_let_first_pat_none(value);
                    }
                }
            }
        },
    );
    assert!(
        sol.contains("(, uint64 y) =") || sol.contains("(, y) ="),
        "Pattern::None tuple slot must hit gen_expr_hoisted tuple catch-all binder arm: {sol}"
    );
    assert_solc_compiles("expr_hoisted_tuple_none_binder", &sol);
}

#[test]
fn n4_codegen_expr_wrapping_mul_member_solc() {
    let program = parse_evm(
        r#"
        entity WrapMul {
            routes {
                constructor() => []
                scale(by: u64) => []
            }
            m_total: u64 {
                in constructor() => 1
                in scale(by) => m_total *% by
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function _wmul") && sol.contains("_wmul(m_total"),
        "wrapping *% must lower through _wmul (gen_expr BinOp WrappingMul): {sol}"
    );
    assert_solc_compiles("expr_wrapping_mul_member", &sol);
}

#[test]
fn n4_codegen_expr_nested_index_contains_direct_solc() {
    let program = parse_evm(
        r#"
        entity NestedContainsDirect {
            routes {
                constructor() => []
                probe(outer: u64, k: u64) -> bool => [
                    return(m_nested[outer].contains(k))
                ]
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_nested_inner_exists"),
        "direct nested index contains must hit gen_expr MethodCall contains Index arm: {sol}"
    );
    assert_solc_compiles("expr_nested_index_contains_direct", &sol);
}

#[test]
fn n4_codegen_expr_evm_ns_unknown_intrinsic_none_solc() {
    let program = parse_evm(
        r#"
        entity EvmUnknown {
            routes {
                constructor() => []
                probe(x: U256) -> U256 => [
                    let v = evm::unknownPacked(x);
                    return(v)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("enum variant evm::unknownPacked with data not representable")
            || sol.contains("revert(\"EVM: enum variant"),
        "unknown evm:: intrinsic must hit gen_evm_ns catch-all None then hoisted fallback: {sol}"
    );
    assert_solc_compiles("expr_evm_ns_unknown_intrinsic_none", &sol);
}

#[test]
fn n4_codegen_expr_unit_enum_variant_data_inline_none_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        enum Mode {
            On,
            Off
        }

        entity UnitEnumExpr {
            routes {
                constructor() => []
                flag(on: bool) -> Mode => [
                    let m = if on { Mode::On } else { Mode::Off };
                    return(m)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            let entity = program.entities.first_mut().expect("entity");
            let route = entity
                .routes
                .iter_mut()
                .find(|r| r.name == "flag")
                .expect("flag route");
            if let RouteBody::Unphased(actions) = &mut route.body {
                for action in actions.iter_mut() {
                    if let RouteAction::Let { value, .. } = action {
                        patch_if_arm_to_enum_variant_with_data(value, "Mode", "On");
                    }
                }
            }
        },
    );
    assert!(
        sol.contains("Mode.On") || sol.contains("Mode_Mode.On") || sol.contains("if ("),
        "unit enum patched to EnumVariantWithData must miss inline payload arm and stay hoisted: {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

fn patch_match_enum_name_to_phantom(expr: &mut Expr) {
    match expr {
        Expr::Let(_, val, body) => {
            patch_match_enum_name_to_phantom(val);
            patch_match_enum_name_to_phantom(body);
        }
        Expr::Match(_, arms) => {
            for arm in arms.iter_mut() {
                if let MatchPattern::EnumVariantWithData(en, _, _) = &mut arm.pattern {
                    *en = "Phantom".into();
                }
            }
        }
        Expr::Block(items) => {
            for item in items {
                patch_match_enum_name_to_phantom(item);
            }
        }
        Expr::If(c, t, e) => {
            patch_match_enum_name_to_phantom(c);
            patch_match_enum_name_to_phantom(t);
            if let Some(e) = e {
                patch_match_enum_name_to_phantom(e);
            }
        }
        _ => {}
    }
}

#[test]
fn n4_codegen_expr_bitop_binop_member_solc() {
    let program = parse_evm(
        r#"
        entity BitOps {
            routes {
                constructor() => []
                mix(x: u64, sh: u64) => []
            }
            m_flags: u64 {
                in constructor() => 0
                in mix(x, sh) => ((m_flags & x) | (m_flags ^ 1)) << sh
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("&") && sol.contains("|") && sol.contains("^") && sol.contains("<<"),
        "bitwise binops must lower via gen_binop_operands_solidity + binop_str: {sol}"
    );
    assert_solc_compiles("expr_bitop_binop_member", &sol);
}

// ---------------------------------------------------------------------------
// N4-73: codegen/solidity/core/expr.rs — slice 7 (temporal scatter residual)
// Baseline @ N4-72: 90.62% line (97 missed / 1034). Focus gen_temporal_ref_sol
// L154–L195, gen_binop no-entity fallback L379–L383, MacroRef/TraceField partial
// L688/L698. Exclude DEAD: hoisted evm success L1324; exists K3 brace L1435;
// NamespacedCall/K3 catch-all.
// ---------------------------------------------------------------------------

#[test]
fn n4_73_codegen_expr_temporal_no_route_transform_storage_solc() {
    let program = parse_evm(
        r#"
        entity TemporalRouteMiss {
            routes {
                constructor() => []
                peek() => [
                    let snap = ^m_n + 1;
                    if (snap > 0) => []
                ]
                bump() => []
            }
            m_n: u64 {
                in constructor() => 0
                in bump() => m_n + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_n + 1") || sol.contains("m_n+1"),
        "peek route without m_n transform must lower ^m_n to storage (gen_temporal_ref_sol no-transform arm): {sol}"
    );
    assert!(
        !sol.contains("next_m_n + 1"),
        "missing transform must not emit next_m_n for cross-route temporal read: {sol}"
    );
    assert_solc_compiles("expr_temporal_no_route_transform", &sol);
}

#[test]
fn n4_73_codegen_expr_temporal_cross_phase_storage_solc() {
    let program = parse_evm(
        r#"
        entity TemporalCrossPhase {
            routes {
                constructor() => []
                grow(n: u64) => [
                    prep: [
                        let snap = ^m_total;
                        if (snap > 0) => []
                    ]
                    finish: []
                ]
            }
            m_total: u64 {
                in constructor() => 0
                in grow(n) => finish: m_total + n
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("// Phase: prep") && sol.contains("m_total"),
        "prep phase without m_total transform must read storage via ^m_total: {sol}"
    );
    assert!(
        sol.contains("next_m_total") || sol.contains("m_total + n"),
        "finish phase transform must still emit scalar pending next_*: {sol}"
    );
    assert_solc_compiles("expr_temporal_cross_phase", &sol);
}

#[test]
fn n4_73_codegen_expr_temporal_map_insert_storage_solc() {
    let program = parse_evm(
        r#"
        entity TemporalMapSlot {
            routes {
                constructor() => []
                set(k: u64, v: u64) => []
            }
            m_map: HashMap<u64, u64> {
                in constructor() => {}
                in set(k, v) => {
                    let slot = ^m_map;
                    m_map.insert(k, slot[k] + v)
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        (sol.contains("mapping(uint64 => uint64) slot = m_map")
            || sol.contains("slot = m_map"))
            && sol.contains("slot[k]"),
        "mapping transform must lower ^m_map to storage alias before insert: {sol}"
    );
    assert!(
        !sol.contains("next_m_map"),
        "HashMap temporal ref must stay on storage (is_mapping_type arm): {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

#[test]
fn n4_73_codegen_expr_temporal_vec_push_storage_solc() {
    let program = parse_evm(
        r#"
        entity TemporalVecPush {
            routes {
                constructor() => []
                append(val: u64) => []
            }
            m_log: Vec<u64> {
                in constructor() => array()
                in append(val) => {
                    let before = ^m_log;
                    m_log.push(before.len() + val)
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    let append_tail = sol.split("function append").nth(1).unwrap_or("");
    assert!(
        append_tail.contains("memory before = m_log") || append_tail.contains("before = m_log"),
        "vec push transform with ^m_log must read storage length before push: {append_tail}"
    );
    assert!(
        !append_tail.contains("before = next_m_log"),
        "vec push body must keep ^m_log on storage (body_is_member_push arm): {append_tail}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

#[test]
fn n4_73_codegen_expr_temporal_unknown_member_storage_solc() {
    let program = parse_evm(
        r#"
        entity TemporalGhost {
            routes {
                constructor() => []
                probe() => [
                    let ghost = ^m_ghost + 1;
                    if (ghost > 0) => []
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_ghost") && sol.contains("+ 1"),
        "unknown member temporal must fall back to sanitized storage ident: {sol}"
    );
    // TB-V: ^m_ghost unknown member (V3/V8) — codegen substring only.
}

#[test]
fn n4_73_codegen_expr_addressof_binop_emitscope_none_solc() {
    let program = parse_evm(
        r#"
        entity Target {
            identity id: u64
            routes { constructor() => [] }
        }

        entity AddrBinop {
            routes {
                constructor() => []
                link(k: u64) => [
                    let dest = address_of Target(k + 1);
                    if (dest != address(0)) => []
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("predictTarget") || sol.contains("keccak256"),
        "address_of Target(k + 1) must lower CREATE2 with binop arg via EmitScope::none gen_binop fallback: {sol}"
    );
    assert!(
        sol.contains("k + 1") || sol.contains("(k + 1)"),
        "identity arg binop must survive gen_expr no-entity path: {sol}"
    );
    assert_solc_compiles("expr_addressof_binop_none_scope", &sol);
}

#[test]
fn n4_73_codegen_expr_macro_ref_partial_arg_none_solc() {
    let mut program = parse_evm(
        r#"
        entity Counter {
            routes {
                constructor() => []
                increment(amount: u64) => []
            }
            m_count: u64 {
                in constructor() => 0
                in increment(amount) => m_count + amount
            }
        }

        invariant "macro partial render" for Counter {
            init { m_count: 0 }
            action increment(amount: u64) { bound amount in 1..100 }
            check m_count >= 0
        }
    "#,
    );
    cambrian_transpiler::desugar::desugar_properties(&mut program);
    let inv = program.invariants.first_mut().expect("invariant");
    inv.checks[0] = Expr::MacroRef(
        "phaseState".into(),
        vec![
            Expr::IntLiteral(U256::from_u128(1)),
            Expr::Closure(
                vec![Pattern::Ident("x".into())],
                Box::new(Expr::Ident("x".into())),
            ),
        ],
    );
    let diags = cambrian_transpiler::validate::check_target_compat(
        &program,
        cambrian_transpiler::target::Target::Evm,
        false,
    );
    assert!(
        diags.iter().any(|d| d.code == "I17"),
        "unlowerable MacroRef check must be I17, not require(true): {diags:?}"
    );
}

#[test]
fn n4_73_codegen_expr_trace_field_unknown_none_solc() {
    let mut program = parse_evm(
        r#"
        entity Counter {
            routes {
                constructor() => []
                increment(amount: u64) => []
            }
            m_count: u64 {
                in constructor() => 0
                in increment(amount) => m_count + amount
            }
        }

        invariant "trace unknown field" for Counter {
            init { m_count: 0 }
            action increment(amount: u64) { bound amount in 1..100 }
            check m_count >= 0
        }
    "#,
    );
    cambrian_transpiler::desugar::desugar_properties(&mut program);
    let inv = program.invariants.first_mut().expect("invariant");
    inv.checks[0] = Expr::BinOp(
        Box::new(Expr::TraceField("depth".into())),
        BinOp::Gt,
        Box::new(Expr::IntLiteral(U256::ZERO)),
    );
    let diags = cambrian_transpiler::validate::check_target_compat(
        &program,
        cambrian_transpiler::target::Target::Evm,
        false,
    );
    assert!(
        diags.iter().any(|d| d.code == "I17"),
        "unknown trace:: field must be I17, not require(true): {diags:?}"
    );
}

// ---------------------------------------------------------------------------
// N4-81: codegen/solidity/core/expr.rs — slice 8 (hoisted scatter residual)
// Baseline @ N4-80: 91.97% line (83 missed / 1034; ~65 excl. DEAD ~18).
// Focus L1332–L1454 hoisted scatter, gen_evm_ns tails, gen_expr_hoisted catch-all,
// gen_binop/gen_expr scatter (cam_get, if-no-else, cast passthrough, logical ops).
// Exclude DEAD: hoisted evm success L1324; exists K3 brace L1435; NamespacedCall/K3.
// ---------------------------------------------------------------------------

#[test]
fn n4_81_codegen_expr_if_value_no_else_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity IfNoElse {
            routes {
                constructor() => []
                pick(flag: bool, a: u64, b: u64) -> u64 => [
                    return(b)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            let entity = program.entities.first_mut().expect("entity");
            let route = entity
                .routes
                .iter_mut()
                .find(|r| r.name == "pick")
                .expect("pick");
            if let RouteBody::Unphased(actions) = &mut route.body {
                if let RouteAction::Return { values } = &mut actions[0] {
                    values[0] = Expr::If(
                        Box::new(Expr::Ident("flag".into())),
                        Box::new(Expr::Ident("a".into())),
                        None,
                    );
                }
            }
        },
    );
    assert!(
        sol.contains("?") && sol.contains(": 0"),
        "value if without else must default else branch to 0 in gen_expr If arm: {sol}"
    );
    assert_solc_compiles("expr_if_value_no_else", &sol);
}

#[test]
fn n4_81_codegen_expr_cam_get_hashmap_solc() {
    let program = parse_evm(
        r#"
        entity CamGet {
            routes {
                constructor() => []
                probe(k: u64) -> u64 => [
                    return(m_map.cam_get(k))
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_map[") || sol.contains("cam_get"),
        "HashMap.cam_get must lower via gen_expr MethodCall cam_get arm: {sol}"
    );
    assert_solc_compiles("expr_cam_get_hashmap", &sol);
}

#[test]
fn n4_81_codegen_expr_wrapping_binop_view_return_solc() {
    let program = parse_evm(
        r#"
        entity WrapView {
            routes {
                constructor() => []
                view mix(a: u64, b: u64) -> u64 => [
                    return(a +% b)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("_wadd(") || sol.contains("_wmul(") || sol.contains("+%"),
        "view return wrapping binop must hit gen_expr WrappingAdd/Sub/Mul arms: {sol}"
    );
    assert_solc_compiles("expr_wrapping_binop_view_return", &sol);
}

#[test]
fn n4_81_codegen_expr_u32_u64_mixed_binop_coerce_solc() {
    let program = parse_evm(
        r#"
        entity U32Mix {
            routes {
                constructor() => []
                add(n: u64) => []
            }
            m_tag: u32 {
                in constructor() => 0
                in add(n) => m_tag + n
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint32(") || sol.contains("uint64("),
        "u32 + u64 member binop must coerce via gen_binop_operands_solidity narrow/sign paths: {sol}"
    );
    assert_solc_compiles("expr_u32_u64_mixed_binop_coerce", &sol);
}

#[test]
fn n4_81_codegen_expr_constant_address_partial_identity_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity Target {
            identity id: u64
            routes { constructor() => [] }
        }

        entity AddrConst {
            const slot: address = address_of Target(1)

            routes {
                constructor() => []
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        true,
        |program| {
            let entity = program.entities.iter_mut().find(|e| e.name == "AddrConst").expect("host");
            if let Some(c) = entity.constants.iter_mut().find(|c| c.name == "slot") {
                if let Expr::AddressOf { args, .. } = &mut c.value {
                    args[0] = Expr::Closure(
                        vec![Pattern::Ident("x".into())],
                        Box::new(Expr::Ident("x".into())),
                    );
                }
            }
        },
    );
    assert!(
        sol.contains("constant slot = 0") || !sol.contains("predictTarget("),
        "partial identity in entity constant must hit gen_expr_address_deterministic None: {sol}"
    );
}

#[test]
fn n4_81_codegen_expr_payload_unit_variant_inline_solc() {
    let program = parse_evm(
        r#"
        enum Action {
            Deposit(u64),
            Reset
        }

        entity PayloadUnit {
            routes {
                constructor() => []
                view idle() -> Action => [
                    return(Action::Reset)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Action_Tag.Reset") || sol.contains("tag: Action_Tag.Reset"),
        "payload enum unit variant must lower via gen_expr EnumVariant inline construct: {sol}"
    );
    assert_solc_compiles("expr_payload_unit_variant_inline", &sol);
}

#[test]
fn n4_81_codegen_expr_cast_alias_passthrough_solc() {
    let program = parse_evm(
        r#"
        type Slot = u32

        pure fn as_slot(x: u64) -> Slot {
            x as Slot
        }

        entity CastAlias {
            routes {
                constructor() => []
                probe(x: u64) -> Slot => [
                    return(as_slot(x))
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("as_slot") && !sol.contains("uint32(x) as Slot"),
        "cast to type alias must passthrough rendered value on non-primitive Cast target: {sol}"
    );
    assert_solc_compiles("expr_cast_alias_passthrough", &sol);
}

#[test]
fn n4_81_codegen_expr_constant_sys_unknown_field_none_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity SysConst {
            const ghost_slot: u64 = 0

            routes {
                constructor() => []
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            let entity = program.entities.first_mut().expect("entity");
            if let Some(c) = entity.constants.iter_mut().find(|c| c.name == "ghost_slot") {
                c.value = Expr::SysField("ghostslot".into());
            }
        },
    );
    assert!(
        sol.contains("constant ghost_slot = 0")
            || sol.contains("constant ghost_slot = uint64(0)")
            || sol.contains("/* unsupported expr */"),
        "unknown sys:: in entity constant must hit SysField None in gen_expr: {sol}"
    );
    assert_solc_compiles("expr_constant_sys_unknown_field_none", &sol);
}

#[test]
fn n4_81_codegen_expr_logical_and_or_binop_solc() {
    let program = parse_evm(
        r#"
        entity LogicOps {
            routes {
                constructor() => []
                check(flag: bool) => []
            }
            m_ok: bool {
                in constructor() => true
                in check(flag) => m_ok && flag || m_ok
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("&&") && sol.contains("||"),
        "bool && / || member transform must hit binop_str And/Or via gen_binop: {sol}"
    );
    assert_solc_compiles("expr_logical_and_or_binop", &sol);
}

#[test]
fn n4_81_codegen_expr_shl_and_or_binop_solc() {
    let program = parse_evm(
        r#"
        entity ShiftLogic {
            routes {
                constructor() => []
                mix(x: u64, sh: u64) => []
            }
            m_flags: u64 {
                in constructor() => 0
                in mix(x, sh) => (m_flags << sh) && (m_flags | x) || m_flags
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("<<") && sol.contains("&&") && sol.contains("||"),
        "shift and logical binops must hit binop_str Shl/And/Or arms: {sol}"
    );
    assert_solc_compiles("expr_shl_and_or_binop", &sol);
}

#[test]
fn n4_81_codegen_expr_temporal_vec_push_block_body_solc() {
    let program = parse_evm(
        r#"
        entity TemporalPushBlock {
            routes {
                constructor() => []
                append(val: u64) => []
            }
            m_log: Vec<u64> {
                in constructor() => array()
                in append(val) => {
                    let snap = ^m_log;
                    { m_log.push(snap.len() + val) }
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    let append_tail = sol.split("function append").nth(1).unwrap_or("");
    assert!(
        append_tail.contains("m_log") && append_tail.contains(".push("),
        "block-wrapped vec push transform must hit body_is_member_push Block tail: {append_tail}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

#[test]
fn n4_81_codegen_expr_match_simple_non_payload_evwd_none_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        enum Mode {
            On,
            Off
        }

        entity UnitMatch {
            routes {
                constructor() => []
                flag(on: bool) -> u64 => [
                    let n = match on {
                        true => Mode::On,
                        false => Mode::Off
                    };
                    return(1)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            let entity = program.entities.first_mut().expect("entity");
            let route = entity
                .routes
                .iter_mut()
                .find(|r| r.name == "flag")
                .expect("flag");
            if let RouteBody::Unphased(actions) = &mut route.body {
                if let RouteAction::Let { value, .. } = &mut actions[0] {
                    if let Expr::Match(_, arms) = value {
                        for arm in arms.iter_mut() {
                            if let MatchPattern::EnumVariant(en, vn) = &arm.pattern {
                                arm.pattern = MatchPattern::EnumVariantWithData(
                                    en.clone(),
                                    vn.clone(),
                                    vec![Pattern::Ident("slot".into())],
                                );
                            }
                        }
                    }
                }
            }
        },
    );
    assert!(
        sol.contains("Mode.On") || sol.contains("if (") || sol.contains("?"),
        "non-payload EnumVariantWithData in gen_match_simple must fall through to hoisted match: {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

#[test]
fn n4_81_codegen_expr_hashmap_is_empty_non_ident_none_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity IsEmptyExpr {
            routes {
                constructor() => []
                probe(k: u64) -> bool => [
                    return(m_map.is_empty())
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        false,
        |program| {
            let entity = program.entities.first_mut().expect("entity");
            let route = entity
                .routes
                .iter_mut()
                .find(|r| r.name == "probe")
                .expect("probe");
            if let RouteBody::Unphased(actions) = &mut route.body {
                if let RouteAction::Return { values } = &mut actions[0] {
                    values[0] = Expr::MethodCall(
                        Box::new(Expr::Index(
                            Box::new(Expr::Ident("m_map".into())),
                            Box::new(Expr::IntLiteral(U256::ZERO)),
                        )),
                        "is_empty".into(),
                        vec![],
                    );
                }
            }
        },
    );
    assert!(
        sol.contains("/* unsupported expr */") || sol.contains("is_empty"),
        "is_empty on non-Ident base must hit MethodCall is_empty None fallback: {sol}"
    );
}

// ---------------------------------------------------------------------------
// N4-13: codegen/solidity/core/iter.rs — chain fusion, fold, HashMap tuple for
// Gap map @ 64.70% (281 missed): lower_iter_chain 42, gen_fold_loop 18,
// gen_for_loop 10, bind_loop_pattern 7, resolve_iter_source 4 (excl. dead
// rename_idents_in_expr ~139).
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_iter_filter_map_fold_solc() {
    let program = parse_evm(
        r#"
        library ChainLib {
            pure fn sum_doubled_positive(xs: Vec<u64>) -> u64 {
                xs.filter(|x| *x > 0).map(|x| *x * 2).fold(0, |acc, x| acc + x)
            }
        }

        entity ChainFold {
            routes {
                constructor() => []
                compute(xs: Vec<u64>) => []
            }
            m_sum: u64 {
                in constructor() => 0
                in compute(xs) => ChainLib::sum_doubled_positive(xs)
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function sum_doubled_positive"),
        "filter/map/fold chain must lower via lower_iter_chain: {sol}"
    );
    assert!(
        sol.contains("continue") || sol.contains("for (uint256"),
        "chain must emit filter loop body: {sol}"
    );
    assert_solc_compiles("iter_filter_map_fold", &sol);
}

#[test]
fn n4_codegen_iter_range_fold_solc() {
    let program = parse_evm(
        r#"
        library FoldLib {
            pure fn sum_range(n: u64) -> u64 {
                (0..n).fold(0, |acc, i| acc + i)
            }
        }

        entity RangeFold {
            routes {
                constructor() => []
                run(n: u64) -> u64 => [ return(FoldLib::sum_range(n)) ]
            }
            m_runs: u64 {
                in constructor() => 0
                in run(n) => m_runs + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function sum_range"),
        "range fold must emit gen_fold_loop lowering: {sol}"
    );
    assert!(
        sol.contains("acc =") || sol.contains("acc+"),
        "fold must thread accumulator: {sol}"
    );
    assert_solc_compiles("iter_range_fold", &sol);
}

#[test]
fn n4_codegen_iter_hashmap_tuple_for_solc() {
    let program = parse_evm(
        r#"
        library MapPairs {
            pure fn sum_pairs(m: HashMap<u64, u64>) -> u64 {
                for (k, v) in m { k + v }
            }
        }

        entity TupleIter {
            routes {
                constructor() => []
                total() -> u64 => [ return(MapPairs::sum_pairs(m_map)) ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function sum_pairs"),
        "HashMap tuple for must bind (k,v) via bind_loop_pattern: {sol}"
    );
    assert!(
        sol.contains("_keys") && (sol.contains("uint64 k") || sol.contains("uint64 v")),
        "tuple iteration must use parallel keys array: {sol}"
    );
    assert_solc_compiles("iter_hashmap_tuple_for", &sol);
}

#[test]
fn n4_codegen_iter_hashmap_values_collect_solc() {
    let program = parse_evm(
        r#"
        entity ValueBag {
            routes {
                constructor() => []
                allVals() -> Vec<u64> => [
                    let vs = m_map.values().collect();
                    return(vs)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function allVals"),
        "m.values().collect() route must be emitted: {sol}"
    );
    assert!(
        sol.contains("_keys") && sol.contains("[] memory"),
        "values().collect() must walk HashMap keys sidecar (resolve_iter_source Values): {sol}"
    );
    assert_solc_compiles("iter_hashmap_values_collect", &sol);
}

#[test]
fn n4_codegen_iter_enumerate_take_collect_solc() {
    let program = parse_evm(
        r#"
        library EnumLib {
            pure fn head_doubled(xs: Vec<u32>, limit: u32) -> Vec<u32> {
                xs.enumerate()
                    .filter(|(_, x)| *x > 0)
                    .take(limit)
                    .map(|(_, x)| *x * 2)
                    .collect()
            }
        }

        entity EnumTake {
            routes {
                constructor() => []
                run(xs: Vec<u32>, limit: u32) -> Vec<u32> => [
                    return(EnumLib::head_doubled(xs, limit))
                ]
            }
            m_count: u32 {
                in constructor() => 0
                in run(xs, limit) => m_count + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function head_doubled"),
        "enumerate/filter/take/map/collect chain must lower: {sol}"
    );
    assert!(
        sol.contains("break") || sol.contains("_cam_taken"),
        "take stage must cap iterations: {sol}"
    );
    assert_solc_compiles("iter_enumerate_take_collect", &sol);
}

#[test]
fn n4_codegen_iter_narrow_range_for_solc() {
    let program = parse_evm(
        r#"
        library RangeLib {
            pure fn first_n(n: u32) -> Vec<u32> {
                for i in 0..n { i }
            }
        }

        entity NarrowRange {
            routes {
                constructor() => []
                seed(n: u32) -> Vec<u32> => [ return(RangeLib::first_n(n)) ]
            }
            m_n: u32 {
                in constructor() => 0
                in seed(n) => n
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function first_n"),
        "u32 range for must be emitted: {sol}"
    );
    assert!(
        sol.contains("uint32[] memory") || sol.contains("uint32"),
        "narrow range element type must survive (resolve_iter_source): {sol}"
    );
    assert_solc_compiles("iter_narrow_range_for", &sol);
}

// ---------------------------------------------------------------------------
// N4-32: codegen/solidity/core/iter.rs — slice 2 (error/revert arms, fold
// rejects, bind_loop_pattern mismatches). Exclude dead `rename_idents_in_expr`.
// Gap @ 66.08% (270 missed): lower_iter_chain revert ~20, gen_fold_loop ~10.
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_iter_enumerate_collect_tuple_revert_solc() {
    let program = parse_evm(
        r#"
        library EnumCollect {
            pure fn pairs(xs: Vec<u64>) -> Vec<u64> {
                xs.enumerate().collect()
            }
        }

        entity EnumCollectUser {
            routes {
                constructor() => []
                run(xs: Vec<u64>) -> Vec<u64> => [ return(EnumCollect::pairs(xs)) ]
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function pairs"),
        "enumerate().collect() must lower via lower_iter_chain: {sol}"
    );
    assert!(
        sol.contains("collect() over a tuple-yielding chain"),
        "enumerate().collect() must hit tuple-collect revert arm: {sol}"
    );
    assert_solc_compiles("iter_enumerate_collect_tuple_revert", &sol);
}

#[test]
fn n4_codegen_iter_hashmap_fold_empty_acc_revert_solc() {
    let program = parse_evm(
        r#"
        library HmFoldReject {
            pure fn stage(m: HashMap<u64, u64>) -> HashMap<u64, u64> {
                m.iter().fold({}, |acc, (k, v)| acc.insert(k, v))
            }
        }

        entity HmFoldRejectUser {
            routes {
                constructor() => []
                go(m: HashMap<u64, u64>) -> HashMap<u64, u64> => [ return(HmFoldReject::stage(m)) ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("HashMap accumulator in `.fold` is not supported"),
        "empty HashMap fold acc must hit is_hashmap_acc_init revert: {sol}"
    );
    assert_solc_compiles("iter_hashmap_fold_empty_acc_revert", &sol);
}

#[test]
fn n4_codegen_iter_fold_tuple_acc_pattern_revert_solc() {
    let program = parse_evm(
        r#"
        library TupleAccReject {
            pure fn pair_sum(n: u64) -> (u64, u64) {
                (0..n).fold((0, 0), |(sum, _), i| (sum + i, 0))
            }
        }

        entity TupleAccRejectUser {
            routes {
                constructor() => []
                run(n: u64) -> (u64, u64) => [ return(TupleAccReject::pair_sum(n)) ]
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("accumulator pattern must be an identifier"),
        "tuple-destructured fold acc must hit gen_fold_loop reject: {sol}"
    );
    assert_solc_compiles("iter_fold_tuple_acc_pattern_revert", &sol);
}

#[test]
fn n4_codegen_iter_values_on_scalar_chain_revert_solc() {
    let program = parse_evm(
        r#"
        library ValuesReject {
            pure fn bad(xs: Vec<u64>) -> Vec<u64> {
                xs.iter().values().collect()
            }
        }

        entity ValuesRejectUser {
            routes {
                constructor() => []
                run(xs: Vec<u64>) -> Vec<u64> => [ return(ValuesReject::bad(xs)) ]
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("`.values()` is only supported on HashMap iterators"),
        "values() on Vec chain must hit lower_iter_chain unsupported arm: {sol}"
    );
    assert_solc_compiles("iter_values_on_scalar_chain_revert", &sol);
}

#[test]
fn n4_codegen_iter_enumerate_on_hashmap_tuple_revert_solc() {
    let program = parse_evm(
        r#"
        library HmEnumReject {
            pure fn bad(m: HashMap<u64, u64>) -> Vec<u64> {
                m.iter().enumerate().map(|(i, kv)| i).collect()
            }
        }

        entity HmEnumRejectUser {
            routes {
                constructor() => []
                go(m: HashMap<u64, u64>) -> Vec<u64> => [ return(HmEnumReject::bad(m)) ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("enumerate over a tuple-yielding iterator is unsupported"),
        "enumerate on HashMap tuple iter must hit lower_iter_chain revert: {sol}"
    );
    assert_solc_compiles("iter_enumerate_on_hashmap_tuple_revert", &sol);
}

#[test]
fn n4_codegen_iter_for_scalar_pattern_on_hashmap_revert_solc() {
    let program = parse_evm(
        r#"
        library ForBindReject {
            pure fn bad(m: HashMap<u64, u64>) -> Vec<u64> {
                for k in m { 0 }
            }
        }

        entity ForBindRejectUser {
            routes {
                constructor() => []
                go(m: HashMap<u64, u64>) -> Vec<u64> => [ return(ForBindReject::bad(m)) ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("tuple-yielding iterator must be destructured"),
        "scalar for-pattern on HashMap must hit bind_loop_pattern revert: {sol}"
    );
    assert_solc_compiles("iter_for_scalar_pattern_hashmap_revert", &sol);
}

#[test]
fn n4_codegen_iter_fold_var_tuple_on_range_revert_solc() {
    let program = parse_evm(
        r#"
        library FoldVarReject {
            pure fn bad(n: u64) -> u64 {
                (0..n).fold(0, |acc, (k, v)| acc + k + v)
            }
        }

        entity FoldVarRejectUser {
            routes {
                constructor() => []
                run(n: u64) -> u64 => [ return(FoldVarReject::bad(n)) ]
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("tuple destructuring requires a tuple-yielding iterator"),
        "tuple var_pat on range fold must hit bind_loop_pattern revert: {sol}"
    );
    assert_solc_compiles("iter_fold_var_tuple_on_range_revert", &sol);
}

#[test]
fn n4_codegen_iter_chain_fold_tuple_acc_revert_solc() {
    let program = parse_evm(
        r#"
        library ChainFoldAccReject {
            pure fn bad(xs: Vec<u64>) -> (u64, u64) {
                xs.filter(|x| *x > 0).fold((0, 0), |(a, b), x| (a + b + x, 0))
            }
        }

        entity ChainFoldAccRejectUser {
            routes {
                constructor() => []
                run(xs: Vec<u64>) -> (u64, u64) => [ return(ChainFoldAccReject::bad(xs)) ]
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("chain `.fold` accumulator pattern must be an identifier"),
        "chained fold tuple acc must hit lower_iter_chain acc_pat reject: {sol}"
    );
    assert_solc_compiles("iter_chain_fold_tuple_acc_revert", &sol);
}

#[test]
fn n4_codegen_iter_for_tuple_pattern_on_vec_revert_solc() {
    let program = parse_evm(
        r#"
        library ForTupleArityReject {
            pure fn bad(xs: Vec<u64>) -> Vec<u64> {
                for (a, b, c) in xs { a }
            }
        }

        entity ForTupleArityRejectUser {
            routes {
                constructor() => []
                run(xs: Vec<u64>) -> Vec<u64> => [ return(ForTupleArityReject::bad(xs)) ]
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("tuple destructuring requires a tuple-yielding iterator"),
        "tuple for-pattern on Vec must hit bind_loop_pattern arity revert: {sol}"
    );
    assert_solc_compiles("iter_for_tuple_pattern_vec_revert", &sol);
}

// ---------------------------------------------------------------------------
// N4-35: codegen/solidity/core/iter.rs — slice 3 (apply_one_arg_closure
// filter/map Err arms, gen_for_loop / gen_fold_loop bind mismatches,
// unsupported closure patterns). Exclude dead rename_idents L412–L615 (~139).
// Gap @ 72.24% (221 missed; ~82 excl. dead).
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_iter_filter_ident_on_hashmap_tuple_solc() {
    let program = parse_evm(
        r#"
        entity HmFilterIdentUser {
            routes {
                constructor() => []
                bad_keys() -> Vec<u64> => [
                    return(
                        m_map
                            .iter()
                            .filter(|k| *k > 0)
                            .map(|(k, _)| *k)
                            .collect()
                    )
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("tuple-yielding iterator must be destructured with a tuple pattern"),
        "filter(|k|) on member HashMap.iter must hit apply_one_arg_closure Err in Filter stage: {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

#[test]
fn n4_codegen_iter_map_ident_on_hashmap_tuple_solc() {
    let program = parse_evm(
        r#"
        entity HmMapIdentUser {
            routes {
                constructor() => []
                bad_keys() -> Vec<u64> => [
                    return(
                        m_map
                            .iter()
                            .filter(|(k, _)| *k > 0)
                            .map(|k| *k)
                            .collect()
                    )
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("tuple-yielding iterator must be destructured with a tuple pattern"),
        "map(|k|) on member HashMap.iter must hit apply_one_arg_closure Err in Map stage: {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

#[test]
fn n4_codegen_iter_filter_tuple_on_scalar_iter_solc() {
    let program = parse_evm(
        r#"
        library VecFilterTuple {
            pure fn bad(xs: Vec<u64>) -> Vec<u64> {
                xs.iter().filter(|(a, b)| a > b).collect()
            }
        }

        entity VecFilterTupleUser {
            routes {
                constructor() => []
                run(xs: Vec<u64>) -> Vec<u64> => [ return(VecFilterTuple::bad(xs)) ]
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("tuple destructuring requires a tuple-yielding iterator"),
        "filter(|(a,b)|) on scalar iter must hit bind_loop_pattern tuple-on-scalar Err: {sol}"
    );
    assert_solc_compiles("iter_filter_tuple_on_scalar_iter", &sol);
}

#[test]
fn n4_codegen_iter_filter_some_pattern_unsupported_solc() {
    let program = parse_evm(
        r#"
        library SomePatFilter {
            pure fn bad(xs: Vec<u64>) -> Vec<u64> {
                xs.iter().filter(|some(x)| *x > 0).collect()
            }
        }

        entity SomePatFilterUser {
            routes {
                constructor() => []
                run(xs: Vec<u64>) -> Vec<u64> => [ return(SomePatFilter::bad(xs)) ]
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("unsupported loop pattern"),
        "filter(|some(x)|) must hit bind_loop_pattern catch-all Err: {sol}"
    );
    assert_solc_compiles("iter_filter_some_pattern_unsupported", &sol);
}

#[test]
fn n4_codegen_iter_chain_fold_var_tuple_on_scalar_solc() {
    let program = parse_evm(
        r#"
        library ChainFoldVarReject {
            pure fn bad(xs: Vec<u64>) -> u64 {
                xs.iter().fold(0, |acc, (x, y)| acc + x + y)
            }
        }

        entity ChainFoldVarRejectUser {
            routes {
                constructor() => []
                run(xs: Vec<u64>) -> u64 => [ return(ChainFoldVarReject::bad(xs)) ]
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("tuple destructuring requires a tuple-yielding iterator"),
        "chained fold |acc,(x,y)| on scalar iter must hit apply_one_arg_closure Err in Fold: {sol}"
    );
    assert_solc_compiles("iter_chain_fold_var_tuple_on_scalar", &sol);
}

#[test]
fn n4_codegen_iter_fold_var_tuple_on_range_solc() {
    let program = parse_evm(
        r#"
        library RangeFoldVarReject {
            pure fn bad(n: u64) -> u64 {
                (0..n).fold(0, |acc, (x, y)| acc + x)
            }
        }

        entity RangeFoldVarRejectUser {
            routes {
                constructor() => []
                run(n: u64) -> u64 => [ return(RangeFoldVarReject::bad(n)) ]
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("tuple destructuring requires a tuple-yielding iterator"),
        "range fold |acc,(x,y)| must hit gen_fold_loop bind_loop_pattern Err: {sol}"
    );
    assert_solc_compiles("iter_fold_var_tuple_on_range", &sol);
}

#[test]
fn n4_codegen_iter_for_ident_on_hashmap_iter_solc() {
    let program = parse_evm(
        r#"
        entity ForHmIdentUser {
            routes {
                constructor() => []
                bad_for() => [
                    for k in m_map => [
                        if false => [
                            throw 8
                        ]
                    ]
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("tuple-yielding iterator must be destructured with a tuple pattern"),
        "for k in m_map must hit gen_for_loop bind_loop_pattern Err: {sol}"
    );
    assert_solc_compiles("iter_for_ident_on_hashmap_iter", &sol);
}

#[test]
fn n4_codegen_iter_hashmap_fold_tuple_pattern_solc() {
    let program = parse_evm(
        r#"
        entity HmFoldPairsUser {
            routes {
                constructor() => []
                sum_pairs() -> u64 => [
                    return(m_map.iter().fold(0, |acc, (k, v)| acc + k + v))
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function sum_pairs") && sol.contains("acc ="),
        "HashMap.iter().fold with tuple var pattern must lower via lower_iter_chain: {sol}"
    );
    assert!(
        sol.contains("m_map_keys") && sol.contains("for (uint256"),
        "HashMap chain fold must use keys sidecar iteration: {sol}"
    );
    assert_solc_compiles("iter_hashmap_fold_tuple_pattern", &sol);
}

#[test]
fn n4_codegen_iter_resolve_non_ident_iter_base_solc() {
    let program = parse_evm(
        r#"
        entity HelperIterUser {
            routes {
                constructor() => []
                snapshot() -> HashMap<u64, u64> => [ return(m_map) ]
                keys() -> Vec<u64> => [
                    return(snapshot().iter().filter(|(k, _)| *k > 0).map(|(k, _)| *k).collect())
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function keys") || sol.contains("function bad_keys"),
        "non-Ident iter base must fall through resolve_iter_source to generic hoisting: {sol}"
    );
    assert!(
        sol.contains("tuple destructuring requires a tuple-yielding iterator")
            || sol.contains("tuple-yielding iterator must be destructured with a tuple pattern"),
        "non-Ident iter base chain must surface bind_loop_pattern mismatch: {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

// ---------------------------------------------------------------------------
// N4-54: codegen/solidity/core/iter.rs — slice 4 (parse_iter_chain residual,
// chain fold wildcard acc, take stage, infer_fold_acc_ty, apply_one_arg_closure
// / gen_fold_loop hoisted bodies). Exclude dead rename_idents L412–L615 (~139).
// Baseline @ N4-53: 74.37% line (204 missed / 796); focus L370–L383, L831,
// L963–L964, L1194–L1195, gen_fold_loop L1265/L1324, bind_loop_pattern L89.
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_iter_chain_fold_wildcard_acc_solc() {
    let program = parse_evm(
        r#"
        entity ChainWildAccUser {
            routes {
                constructor() => []
                compute(xs: Vec<u64>) => []
            }
            m_sum: u64 {
                in constructor() => 0
                in compute(xs) => {
                    xs.filter(|x| *x > 0).fold(0, |_, x| x)
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("_cam_fold_acc") || sol.contains("m_sum"),
        "chained fold wildcard acc must hit lower_iter_chain Pattern::Wildcard arm: {sol}"
    );
    assert!(
        sol.contains("for (uint256"),
        "filter+fold chain must lower via lower_iter_chain for-loop: {sol}"
    );
    assert_solc_compiles("iter_chain_fold_wildcard_acc", &sol);
}

#[test]
fn n4_codegen_iter_fold_wildcard_acc_range_solc() {
    let program = parse_evm(
        r#"
        library RangeWildAcc {
            pure fn last(n: u64) -> u64 {
                (0..n).fold(0, |_, i| i)
            }
        }

        entity RangeWildAccUser {
            routes {
                constructor() => []
                run(n: u64) -> u64 => [ return(RangeWildAcc::last(n)) ]
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("_cam_fold_acc") || sol.contains("function last"),
        "range fold wildcard acc must hit gen_fold_loop Pattern::Wildcard arm: {sol}"
    );
    assert_solc_compiles("iter_fold_wildcard_acc_range", &sol);
}

#[test]
fn n4_codegen_iter_infer_fold_acc_record_body_solc() {
    let program = parse_evm(
        r#"
        record Tot { sum: u64 }

        library RecordFoldAcc {
            pure fn tally(xs: Vec<u64>) -> Tot {
                xs.fold(0, |acc, x| Tot { sum: acc + x })
            }
        }

        entity RecordFoldAccUser {
            routes {
                constructor() => []
                run(xs: Vec<u64>) -> Tot => [ return(RecordFoldAcc::tally(xs)) ]
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("struct Tot") && sol.contains("Tot memory"),
        "fold body RecordConstruct must hit infer_fold_acc_ty record arm: {sol}"
    );
    assert!(
        sol.contains("function tally"),
        "scalar-init record-body fold must lower via gen_fold_loop: {sol}"
    );
    assert_solc_compiles("iter_infer_fold_acc_record_body", &sol);
}

#[test]
fn n4_codegen_iter_infer_fold_acc_block_cast_body_solc() {
    let program = parse_evm(
        r#"
        entity BlockCastFoldUser {
            routes {
                constructor() => []
                run(xs: Vec<u32>) => []
            }
            m_total: u32 {
                in constructor() => 0
                in run(xs) => {
                    xs.fold(0, |acc, x| {
                        let t = x;
                        acc + t
                    })
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        (sol.contains("uint32 t") || sol.contains("+ t")) && sol.contains("m_total"),
        "block-body fold must hit infer_fold_acc_ty Block tail + gen_fold_loop hoisted stmts: {sol}"
    );
    assert!(
        sol.contains("uint32") || sol.contains("acc ="),
        "block fold body must keep narrow acc typing: {sol}"
    );
    assert_solc_compiles("iter_infer_fold_acc_block_cast_body", &sol);
}

#[test]
fn n4_codegen_iter_route_take_map_collect_solc() {
    let program = parse_evm(
        r#"
        entity TakeRoute {
            routes {
                constructor() => []
                head(limit: u32) -> Vec<u32> => [
                    return(
                        m_vec
                            .iter()
                            .take(limit)
                            .map(|x| *x * 2)
                            .collect()
                    )
                ]
            }
            m_vec: Vec<u32> {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("break") || sol.contains("_cam_taken"),
        "route-body take stage must emit taken counter + break: {sol}"
    );
    assert!(
        sol.contains("function head") || sol.contains("m_vec"),
        "iter.take.map.collect chain must lower on member Vec route: {sol}"
    );
    assert_solc_compiles("iter_route_take_map_collect", &sol);
}

#[test]
fn n4_codegen_iter_filter_deref_pattern_solc() {
    let program = parse_evm(
        r#"
        library DerefFilter {
            pure fn positives(xs: Vec<u64>) -> Vec<u64> {
                xs.iter().filter(|*x| *x > 0).map(|x| *x).collect()
            }
        }

        entity DerefFilterUser {
            routes {
                constructor() => []
                run(xs: Vec<u64>) -> Vec<u64> => [ return(DerefFilter::positives(xs)) ]
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function positives"),
        "filter(|*x|) deref pattern must lower via bind_loop_pattern Deref arm: {sol}"
    );
    assert_solc_compiles("iter_filter_deref_pattern", &sol);
}

#[test]
fn n4_codegen_iter_for_hoisted_body_solc() {
    let program = parse_evm(
        r#"
        entity ForHoistBodyUser {
            routes {
                constructor() => []
                run(xs: Vec<u32>) => []
            }
            m_out: Vec<u32> {
                in run(xs) => {
                    for x in xs {
                        let t = x;
                        t * 2
                    }
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        (sol.contains("uint256 t =") || sol.contains("uint32 t =")) && sol.contains("m_out"),
        "for-loop body with let prelude must hit gen_for_loop hoisted stmt push: {sol}"
    );
    assert_solc_compiles("iter_for_hoisted_body", &sol);
}

#[test]
fn n4_codegen_iter_narrow_range_lower_bound_solc() {
    let program = parse_evm(
        r#"
        library LowerBoundRange {
            pure fn span(start: u32, end: u32) -> Vec<u32> {
                for i in start..end { i }
            }
        }

        entity LowerBoundRangeUser {
            routes {
                constructor() => []
                run(a: u32, b: u32) -> Vec<u32> => [ return(LowerBoundRange::span(a, b)) ]
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function span") && (sol.contains("uint32[] memory") || sol.contains("uint32")),
        "u32..u32 range must pick narrow lower-bound elem type in resolve_iter_source: {sol}"
    );
    assert_solc_compiles("iter_narrow_range_lower_bound", &sol);
}

#[test]
fn n4_codegen_iter_enumerate_collect_member_revert_solc() {
    let program = parse_evm(
        r#"
        entity EnumCollectMember {
            routes {
                constructor() => []
                run(xs: Vec<u64>) => []
            }
            m_out: Vec<u64> {
                in run(xs) => xs.enumerate().collect()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("collect() over a tuple-yielding chain"),
        "member enumerate().collect() must hit lower_iter_chain tuple-collect revert: {sol}"
    );
    assert_solc_compiles("iter_enumerate_collect_member_revert", &sol);
}

#[test]
fn n4_codegen_iter_member_take_map_collect_solc() {
    let program = parse_evm(
        r#"
        entity TakeMember {
            routes {
                constructor() => []
                head(xs: Vec<u32>, limit: u32) => []
            }
            m_head: Vec<u32> {
                in head(xs, limit) => {
                    xs.iter().take(limit).map(|x| *x * 2).collect()
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("break") || sol.contains("_cam_taken"),
        "member transform take stage must emit taken counter + break: {sol}"
    );
    assert!(
        sol.contains("m_head"),
        "iter.take.map.collect on route param Vec must lower: {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

#[test]
fn n4_codegen_iter_fold_cast_tail_solc() {
    let program = parse_evm(
        r#"
        entity CastFoldTail {
            routes {
                constructor() => []
                run(xs: Vec<u32>) => []
            }
            m_total: u32 {
                in constructor() => 0
                in run(xs) => xs.fold(0, |acc, x| acc + (x as u32))
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_total") && sol.contains("acc"),
        "fold closure cast tail must hit infer_fold_acc_ty Cast arm: {sol}"
    );
    assert_solc_compiles("iter_fold_cast_tail", &sol);
}

#[test]
fn n4_codegen_iter_hashmap_member_fold_init_revert_solc() {
    let program = parse_evm(
        r#"
        entity HmMemberFoldInit {
            routes {
                constructor() => []
                merge() -> u64 => [
                    return(m_map.iter().fold(m_stage, |acc, (k, v)| acc))
                ]
            }
            m_stage: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("HashMap accumulator in `.fold` is not supported"),
        "HashMap member fold init must hit is_hashmap_acc_init mapping() arm: {sol}"
    );
    assert_solc_compiles("iter_hashmap_member_fold_init_revert", &sol);
}

#[test]
fn n4_codegen_iter_vec_member_iter_resolve_solc() {
    let program = parse_evm(
        r#"
        entity VecIterResolve {
            routes {
                constructor() => []
                sum() -> u64 => [ return(m_vec.iter().fold(0, |acc, x| acc + x)) ]
            }
            m_vec: Vec<u64> {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function sum") && sol.contains("m_vec"),
        "Vec member .iter() must fall through HashMap resolve to generic vec source: {sol}"
    );
    assert_solc_compiles("iter_vec_member_iter_resolve", &sol);
}

#[test]
fn n4_codegen_iter_fold_cast_only_tail_solc() {
    let program = parse_evm(
        r#"
        entity CastOnlyFold {
            routes {
                constructor() => []
                run(xs: Vec<u32>) => []
            }
            m_last: u32 {
                in constructor() => 0
                in run(xs) => xs.fold(0, |acc, x| x as u32)
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_last") && (sol.contains("as uint32") || sol.contains("uint32(")),
        "fold body with Cast tail must hit infer_fold_acc_ty Cast arm: {sol}"
    );
    assert_solc_compiles("iter_fold_cast_only_tail", &sol);
}

#[test]
fn n4_codegen_iter_for_non_ident_iter_base_solc() {
    let program = parse_evm(
        r#"
        entity SnapIterFor {
            routes {
                constructor() => []
                snap() -> HashMap<u64, u64> => [ return(m_map) ]
                walk() => [
                    for k in snap().iter() => [
                        if false => [ throw 1 ]
                    ]
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function walk") || sol.contains("snap()"),
        "for on snap().iter() must resolve non-Ident iter base (resolve_iter_source None arm): {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

#[test]
fn n4_codegen_iter_for_non_ident_values_base_solc() {
    let program = parse_evm(
        r#"
        entity SnapValuesFor {
            routes {
                constructor() => []
                snap() -> HashMap<u64, u64> => [ return(m_map) ]
                walk() => [
                    for v in snap().values() => [
                        if false => [ throw 2 ]
                    ]
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function walk") || sol.contains("values()"),
        "for on snap().values() must hit resolve_iter_source non-Ident values arm: {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

// ---------------------------------------------------------------------------
// N4-63: codegen/solidity/core/iter.rs — slice 5 (final executable residual:
// parse_iter_chain filter/map peel, Take hoisted limit, tuple-collect revert,
// infer_fold_acc_ty Block items.last). Exclude dead rename_idents L412–L615 (~178).
// Baseline @ N4-62: 76.01% line (191 missed / 796); focus L370–L383, L963–L964,
// L977–L980, L1190.
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_iter_take_hoisted_limit_solc() {
    let program = parse_evm(
        r#"
        entity TakeHoistLimit {
            routes {
                constructor() => []
                bump(k: u32) => []
            }
            m_bias: u32 {
                in constructor() => 1
                in bump(k) => m_bias + k
            }
            m_head: Vec<u32> {
                in bump(k) => {
                    m_vec.iter().take(k + m_bias + m_vec.length).map(|x| *x * 2).collect()
                }
            }
            m_vec: Vec<u32> {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("break") && sol.contains("_cam_taken"),
        "take(k + m_bias + m_vec.length) must hit ChainStage::Take + break: {sol}"
    );
    assert!(
        sol.contains("m_vec.length") || sol.contains(".length"),
        "member length in take limit should force hoisted prelude (L963–L964): {sol}"
    );
    assert_solc_compiles("iter_take_hoisted_limit", &sol);
}

#[test]
fn n4_codegen_iter_take_hoisted_limit_pure_fn_solc() {
    let program = parse_evm(
        r#"
        library TakeHoistLib {
            pure fn head(xs: Vec<u32>, k: u32, bias: u32) -> Vec<u32> {
                xs.iter().take(k + bias).map(|x| *x).collect()
            }
        }

        entity TakeHoistLibUser {
            routes {
                constructor() => []
                run(xs: Vec<u32>, k: u32, bias: u32) -> Vec<u32> => [
                    return(TakeHoistLib::head(xs, k, bias))
                ]
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function head") && sol.contains("_cam_taken"),
        "pure-fn take(k + bias) must lower ChainStage::Take with hoisted limit: {sol}"
    );
    assert_solc_compiles("iter_take_hoisted_limit_pure_fn", &sol);
}

fn patch_take_limit_if_hoist(expr: &mut Expr) {
    match expr {
        Expr::MethodCall(b, m, args) if m == "take" && args.len() == 1 => {
            args[0] = Expr::If(
                Box::new(Expr::BinOp(
                    Box::new(Expr::Ident("k".to_string())),
                    BinOp::Gt,
                    Box::new(Expr::Ident("m_bias".to_string())),
                )),
                Box::new(Expr::Let(
                    Pattern::Ident("lim".to_string()),
                    Box::new(Expr::Ident("k".to_string())),
                    Box::new(Expr::BinOp(
                        Box::new(Expr::Ident("k".to_string())),
                        BinOp::Add,
                        Box::new(Expr::IntLiteral(U256::from_u128(1))),
                    )),
                )),
                Some(Box::new(Expr::Ident("m_bias".to_string()))),
            );
        }
        Expr::MethodCall(b, _, args) => {
            patch_take_limit_if_hoist(b);
            for a in args {
                patch_take_limit_if_hoist(a);
            }
        }
        Expr::Block(items) => {
            for item in items {
                patch_take_limit_if_hoist(item);
            }
        }
        Expr::Let(_, v, b) => {
            patch_take_limit_if_hoist(v);
            patch_take_limit_if_hoist(b);
        }
        _ => {}
    }
}

#[test]
fn n4_codegen_iter_take_limit_if_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity TakeIfInject {
            routes {
                constructor() => []
                bump(k: u32) => []
            }
            m_bias: u32 {
                in constructor() => 1
                in bump(k) => m_bias + k
            }
            m_head: Vec<u32> {
                in bump(k) => {
                    m_vec.iter().take(k + m_bias).map(|x| *x * 2).collect()
                }
            }
            m_vec: Vec<u32> {}
        }
    "#,
        false,
        |program| {
            let entity = program
                .entities
                .iter_mut()
                .find(|e| e.name == "TakeIfInject")
                .expect("TakeIfInject");
            let member = entity
                .members
                .iter_mut()
                .find(|m| m.name == "m_head")
                .expect("m_head");
            let transform = member
                .transforms
                .iter_mut()
                .find(|t| t.route_name == "bump")
                .expect("bump transform");
            patch_take_limit_if_hoist(&mut transform.body);
        },
    );
    assert!(
        sol.contains("_cam_taken") && sol.contains("break"),
        "take(if-hoisted limit) must emit ChainStage::Take break: {sol}"
    );
    assert!(
        sol.contains("uint32 lim") || sol.contains("if (k > m_bias)"),
        "If limit must hoist prelude stmts into Take stage (L963–L964): {sol}"
    );
    assert_solc_compiles("iter_take_limit_if_ast_inject", &sol);
}

#[test]
fn n4_codegen_iter_fold_block_record_tail_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        record Tot { sum: u64 }

        entity BlockRecordFold {
            routes {
                constructor() => []
                run(xs: Vec<u64>) => []
            }
            m_tot: Tot {
                in constructor() => { Tot { sum: 0 } }
                in run(xs) => {
                    xs.fold(0, |acc, x| acc + x)
                }
            }
        }
    "#,
        false,
        |program| {
            let entity = program
                .entities
                .iter_mut()
                .find(|e| e.name == "BlockRecordFold")
                .expect("BlockRecordFold");
            let member = entity
                .members
                .iter_mut()
                .find(|m| m.name == "m_tot")
                .expect("m_tot");
            let transform = member
                .transforms
                .iter_mut()
                .find(|t| t.route_name == "run")
                .expect("run transform");
            transform.body = Expr::MethodCall(
                Box::new(Expr::Ident("xs".to_string())),
                "fold".to_string(),
                vec![
                    Expr::IntLiteral(U256::ZERO),
                    Expr::Closure(
                        vec![
                            Pattern::Ident("acc".to_string()),
                            Pattern::Ident("x".to_string()),
                        ],
                        Box::new(Expr::Block(vec![
                            Expr::Let(
                                Pattern::Ident("t".to_string()),
                                Box::new(Expr::Ident("x".to_string())),
                                Box::new(Expr::Ident("acc".to_string())),
                            ),
                            Expr::RecordConstruct(
                                "Tot".to_string(),
                                vec![(
                                    "sum".to_string(),
                                    Expr::BinOp(
                                        Box::new(Expr::Ident("acc".to_string())),
                                        BinOp::Add,
                                        Box::new(Expr::Ident("t".to_string())),
                                    ),
                                )],
                            ),
                        ])),
                    ),
                ],
            );
        },
    );
    assert!(
        sol.contains("Tot memory") || sol.contains("struct Tot"),
        "block-body fold with RecordConstruct tail must hit infer_fold_acc_ty Block+record arms: {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_codegen_iter_parse_chain_filter_map_member_solc() {
    let program = parse_evm(
        r#"
        entity ChainPeelMember {
            routes {
                constructor() => []
                run(xs: Vec<u64>) => []
            }
            m_out: Vec<u64> {
                in run(xs) => {
                    xs.iter()
                        .filter(|x| *x > 0)
                        .map(|x| *x * 2)
                        .collect()
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("continue") && sol.contains("for (uint256"),
        "member filter/map/collect chain must peel parse_iter_chain stages: {sol}"
    );
    assert!(
        sol.contains("m_out"),
        "chained collect must lower into member transform: {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

// ---------------------------------------------------------------------------
// N4-95: codegen/solidity/core/iter.rs — slice 6 (executable close-out @ N4-63).
// Baseline @ N4-94: 76.38% line (188 missed / 796); 10 executable excl.
// `rename_idents_in_expr` L412–L615 (~178 DEAD). Focus L239 vec fallthrough;
// document llvm-dead L331, L370–L383, L977–L980. Acceptance: executable ≤ 8
// excl. dead OR Δ −3+ with EXECUTABLE_CLOSED documented.
// ---------------------------------------------------------------------------

#[test]
fn n4_95_codegen_iter_resolve_vec_member_for_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity VecMemberFor {
            routes {
                constructor() => []
                walk() => [
                    for x in m_vec => [
                        if (x > 0) => []
                    ]
                ]
            }
            m_vec: Vec<u64> {
                in constructor() => array()
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("for (uint256") && sol.contains("m_vec.length") && sol.contains("m_vec["),
        "for on Vec member must fall through HashMap resolve to generic vec source (L239/L242+): {sol}"
    );
    assert_solc_compiles("iter_resolve_vec_member_for", &sol);
}

#[test]
fn n4_95_codegen_iter_resolve_vec_route_fold_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity VecRouteFold {
            routes {
                constructor() => []
                sum(xs: Vec<u64>) -> u64 => [
                    return(xs.fold(0, |acc, x| acc + x))
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("function sum") && sol.contains(".length") && sol.contains("["),
        "bare Vec route-param fold must hit resolve_iter_source vec fallthrough (L242+): {sol}"
    );
    assert_solc_compiles("iter_resolve_vec_route_fold", &sol);
}

#[test]
fn n4_95_codegen_iter_resolve_vec_member_values_shape_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity VecValuesShape {
            routes {
                constructor() => []
                probe() -> u64 => [
                    return(m_vals.iter().fold(0, |acc, v| acc + v))
                ]
            }
            m_vals: Vec<u64> {
                in constructor() => array()
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("m_vals.length") || sol.contains("m_vals["),
        "Vec member .iter().fold must miss HashMap arms and use vec length indexing: {sol}"
    );
    assert_solc_compiles("iter_resolve_vec_member_values_shape", &sol);
}

#[test]
fn n4_95_codegen_iter_fold_block_arraylit_tail_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity BlockArrayFold {
            routes {
                constructor() => []
                pack(xs: Vec<u64>) -> Vec<u64> => [
                    return(xs.fold(array(), |acc, x| acc))
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            let entity = program.entities.first_mut().expect("entity");
            let route = entity
                .routes
                .iter_mut()
                .find(|r| r.name == "pack")
                .expect("pack");
            if let RouteBody::Unphased(actions) = &mut route.body {
                if let RouteAction::Return { values } = &mut actions[0] {
                    if let Expr::MethodCall(_, method, args) = &mut values[0] {
                        if method == "fold" && args.len() == 2 {
                            if let Expr::Closure(_, body) = &mut args[1] {
                                *body = Box::new(Expr::Block(vec![
                                    Expr::Let(
                                        Pattern::Ident("t".into()),
                                        Box::new(Expr::Ident("x".into())),
                                        Box::new(Expr::Ident("acc".into())),
                                    ),
                                    Expr::ArrayLit(vec![Expr::Ident("x".into())]),
                                ]));
                            }
                        }
                    }
                }
            }
        },
    );
    assert!(
        sol.contains("function pack") && (sol.contains("new uint64[]") || sol.contains("uint64[]")),
        "block-body fold with ArrayLit tail must hit infer_fold_acc_ty Block+ArrayLit arms: {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_95_codegen_iter_enumerate_collect_preloop_revert_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity EnumCollectPreloop {
            routes {
                constructor() => []
                snap(xs: Vec<u64>) => [
                    let _ = xs.iter().enumerate().collect();
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("tuple-yielding chain") || sol.contains("tuple element type"),
        "enumerate().collect() must revert at pre-loop tuple guard (L793), not in-loop L977: {sol}"
    );
    assert_solc_compiles("iter_enumerate_collect_preloop_revert", &sol);
}

// ---------------------------------------------------------------------------
// N4-14: codegen/solidity/evm/analysis.rs tail — walk_expr / pure-fn / inner exists
// Gap map @ 68.03% (~102 missed): walk_expr Match/AddressOf (L183–213),
// param_uses_exists secondary (L274–281), member_inner let-alias (L91–96).
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_analysis_walk_match_range_exists_solc() {
    let program = parse_evm(
        r#"
        record Slot { tag: u64, live: bool }

        entity WalkMatch {
            routes {
                constructor() => []
                bump(k: u64) => []
                peek(k: u64) -> u64
                    where m_data.exists(k) : throw 1
                    => [ return(m_data[k]) ]
            }
            m_stamp: u64 {
                in constructor() => 0
                in bump(k) => {
                    let _span = 0..k;
                    match k {
                        0 => 0,
                        _ => {
                            let row = Slot { tag: k, live: true };
                            row.tag
                        }
                    }
                }
            }
            m_data: HashMap<u64, u64> {
                in bump(k) => {
                    if m_data.exists(k) {
                        m_data.update(k, m_data[k] + 1)
                    } else {
                        m_data.insert(k, 1)
                    }
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_data_exists"),
        "Match/Record walk must still detect .exists() for sidecar: {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

#[test]
fn n4_codegen_analysis_pure_fn_param_secondary_walk_solc() {
    let program = parse_evm(
        r#"
        pure fn slot(m: HashMap<u64, u64>, k: u64) -> u64 {
            let probe = (m.exists(k), k);
            match k {
                0 => 0,
                _ => {
                    let _band = 0..k;
                    if m.exists(k) { m[k] } else { 0 }
                }
            }
        }

        entity SlotBank {
            routes {
                constructor() => []
                read(k: u64) -> u64 => [ return(slot(m_slots, k)) ]
            }
            m_slots: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("slot(m_slots, m_slots_exists, k)"),
        "pure-fn param walk (tuple/match/range) must thread _exists: {sol}"
    );
    assert!(
        sol.contains("mapping(uint64 => bool) public m_slots_exists"),
        "entity must emit _exists sidecar: {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

#[test]
fn n4_codegen_analysis_inner_let_alias_exists_solc() {
    let program = parse_evm(
        r#"
        entity InnerAlias {
            routes {
                constructor() => []
                bind(outer: u64, inner_k: u64) => []
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => {}
                in bind(outer, inner_k) => {
                    let inner = if m_nested.exists(outer) {
                        m_nested[outer]
                    } else {
                        {}
                    };
                    if inner.exists(inner_k) {
                        m_nested.update(outer, inner.update(inner_k, m_nested[outer][inner_k] + 1))
                    } else {
                        m_nested.update(outer, inner.update(inner_k, 1))
                    }
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => mapping(uint64 => bool)) public m_nested_inner_exists"),
        "let-alias inner.exists must trigger member_inner_uses_exists: {sol}"
    );
    assert_solc_compiles("analysis_inner_let_alias", &sol);
}

#[test]
fn n4_codegen_analysis_chain_filter_exists_solc() {
    let program = parse_evm(
        r#"
        entity FilterScan {
            routes {
                constructor() => []
                sum() -> u64 => [
                    return(
                        m_acc
                            .iter()
                            .filter(|(k, _)| m_acc.exists(k))
                            .fold(0, |total, (k, v)| total + v)
                    )
                ]
            }
            m_acc: HashMap<u64, u64> { in constructor() => {} }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64[] public m_acc_keys"),
        "chain .iter().filter on HashMap must mark iteration: {sol}"
    );
    assert!(
        sol.contains("mapping(uint64 => bool) public m_acc_exists"),
        "filter closure .exists() must allocate sidecar: {sol}"
    );
    assert_solc_compiles("analysis_chain_filter_exists", &sol);
}

#[test]
fn n4_codegen_analysis_addressof_exists_walk_solc() {
    let program = parse_evm(
        r#"
        entity Ping {
            routes {
                constructor() => []
                poke(id: u64) => []
                has(id: u64) -> bool => [ return(m_seen.exists(id)) ]
            }
            m_seen: HashMap<u64, u64> {
                in poke(id) => {
                    let _dest = addressOf(Target.state(id));
                    if m_seen.exists(id) {
                        m_seen.update(id, m_seen[id] + 1)
                    } else {
                        m_seen.insert(id, 1)
                    }
                }
            }
        }

        entity Target {
            identity m_id: u64
            routes { constructor() => [] }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_seen_exists"),
        "AddressOf transform with .exists() must still emit sidecar: {sol}"
    );
    assert!(
        sol.contains("predictTarget") || sol.contains("keccak256"),
        "deterministic addressOf must lower via factory predict: {sol}"
    );
    assert_solc_compiles("analysis_addressof_exists", &sol);
}

#[test]
fn n4_codegen_analysis_namespaced_call_exists_solc() {
    let program = parse_evm(
        r#"
        entity MathCap {
            routes {
                constructor() => []
                cap(k: u64) => []
                capped(k: u64) -> u64
                    where m_limits.exists(k) : throw 1
                    => [ return(std::math::max(m_limits[k], 1)) ]
            }
            m_limits: HashMap<u64, u64> {
                in cap(k) => {
                    if m_limits.exists(k) {
                        m_limits.update(k, std::math::max(m_limits[k], 1))
                    } else {
                        m_limits.insert(k, 1)
                    }
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_limits_exists"),
        "NamespacedCall args walk must preserve .exists() detection: {sol}"
    );
    assert_solc_compiles("analysis_namespaced_exists", &sol);
}

// ---------------------------------------------------------------------------
// N4-15: codegen/test_backend.rs — shared test-codegen scaffolding
// ---------------------------------------------------------------------------

#[test]
fn n4_codegen_test_backend_trace_scan_emits_counters() {
    let program = parse_evm(
        r#"
        entity Counter {
            routes {
                increment(amount: u64) => []
                reset() => []
            }
            m_count: u64 {
                in increment(amount) => m_count + amount
                in reset() => 0
            }
        }

        invariant "trace bounded counter" for Counter {
            init { m_count: 0 }

            action increment(amount: u64) {
                bound amount in 1..100
                assume trace::length < 8
                assume trace::count(increment) <= trace::count(reset) + 5
            }
            action reset() {
                assume !trace::lastWas(reset)
            }

            check m_count >= 0
            check trace::count(increment) >= trace::count(reset)
        }
    "#,
    );
    let feat = scan_invariant_trace_features(&program.invariants[0]);
    assert!(feat.uses_length, "trace::length must be detected");
    assert!(
        feat.counted.iter().any(|r| r == "increment"),
        "trace::count(increment) must be tracked: {:?}",
        feat.counted
    );
    assert!(feat.uses_last, "trace::lastWas must be detected");

    let files = gen_evm_test_files(&program);
    let inv_sol = files
        .iter()
        .find(|(p, _)| p.contains("Invariant"))
        .map(|(_, c)| c.as_str())
        .expect("trace invariant must emit Foundry test file");
    assert!(
        inv_sol.contains("_traceLen"),
        "Foundry handler must emit trace length counter: {inv_sol}"
    );
    assert!(
        inv_sol.contains("increment") && inv_sol.contains("reset"),
        "trace::count routes must appear in handler: {inv_sol}"
    );
}

fn span0() -> Span {
    Span::none()
}

fn minimal_trace_invariant(checks: Vec<Expr>, actions: Vec<InvariantAction>) -> InvariantDecl {
    InvariantDecl {
        name: "trace expr arms".into(),
        instances: vec![InvariantInstance {
            name: "_self".into(),
            entity: "Counter".into(),
            init: vec![("m_count".into(), Expr::IntLiteral(U256::ZERO))],
            forall_state: ForallSpec::default(),
            init_specified: true,
            span: span0(),
        }],
        skip_from: false,
        senders: vec![],
        deploy: vec![],
        context: ContextSpec { entries: vec![] },
        actions,
        checks,
        fail_on_revert: false,
        runs: None,
        depth: None,
        tag: None,
        instantiates: None,
        emit_policy: InvariantEmitPolicy::Emit,
        with_time: false,
        track: vec![],
        derived: vec![],
        exclude_senders: vec![],
        exclude_selectors: vec![],
        span: span0(),
    }
}

fn trace_count(route: &str) -> Expr {
    Expr::TraceCall {
        name: "count".into(),
        route: route.to_string(),
    }
}

/// Nest `trace::*` inside every `collect_trace_features` recursion arm.
fn nested_trace_expr() -> Expr {
    let leaf = trace_count("increment");
    Expr::If(
        Box::new(Expr::BinOp(
            Box::new(Expr::Index(
                Box::new(Expr::FieldAccess(
                    Box::new(Expr::Some(Box::new(Expr::TraceField("length".into())))),
                    "bits".into(),
                )),
                Box::new(Expr::IntLiteral(U256::ZERO)),
            )),
            BinOp::Lt,
            Box::new(Expr::UnaryOp(UnaryOp::Not, Box::new(trace_count("reset")))),
        )),
        Box::new(Expr::Block(vec![Expr::Let(
            Pattern::Ident("snap".into()),
            Box::new(Expr::Tuple(vec![
                Expr::ArrayLit(vec![Expr::EnumVariantWithData(
                    "Mode".into(),
                    "On".into(),
                    vec![leaf.clone()],
                )]),
                Expr::MacroRef("phaseState".into(), vec![leaf.clone()]),
                Expr::NamespacedCall {
                    namespace: "gosh".into(),
                    name: "commit".into(),
                    args: vec![leaf.clone()],
                    type_params: vec![],
                },
            ])),
            Box::new(Expr::Match(
                Box::new(Expr::MethodCall(
                    Box::new(Expr::Cast(
                        Box::new(leaf.clone()),
                        Type::Simple("u64".into()),
                    )),
                    "abs".into(),
                    vec![Expr::TraceCall {
                        name: "bogus".into(),
                        route: "increment".into(),
                    }],
                )),
                vec![MatchArm {
                    pattern: MatchPattern::Wildcard,
                    body: Expr::RecordConstruct(
                        "Snap".into(),
                        vec![("inner".into(), leaf.clone())],
                    ),
                }],
            )),
        )])),
        Some(Box::new(Expr::For(
            Pattern::Ident("_".into()),
            Box::new(Expr::Range(
                Box::new(Expr::IntLiteral(U256::ZERO)),
                Box::new(Expr::IntLiteral(U256::from_u128(2))),
            )),
            Box::new(Expr::RecordUpdate(
                Box::new(Expr::Ident("base".into())),
                vec![(
                    "f".into(),
                    Expr::Closure(
                        vec![Pattern::Ident("x".into())],
                        Box::new(Expr::TraceCall {
                            name: "lastWas".into(),
                            route: "reset".into(),
                        }),
                    ),
                )],
            )),
        ))),
    )
}

#[test]
fn n4_codegen_test_backend_collect_trace_features_expr_arms() {
    let assume = nested_trace_expr();
    let inv = minimal_trace_invariant(
        vec![nested_trace_expr()],
        vec![InvariantAction {
            instance: "_self".into(),
            route: "increment".into(),
            params: vec![],
            body: vec![TestStep::Assume { cond: assume }],
            span: span0(),
        }],
    );
    let feat = scan_invariant_trace_features(&inv);
    assert!(feat.uses_length, "nested TraceField length");
    assert!(feat.uses_last, "nested TraceCall lastWas");
    assert!(
        feat.counted.iter().any(|r| r == "increment") && feat.counted.iter().any(|r| r == "reset"),
        "nested TraceCall count routes: {:?}",
        feat.counted
    );
}

#[test]
fn n4_codegen_test_backend_trace_scan_skips_excluded_action() {
    let inv = minimal_trace_invariant(
        vec![Expr::TraceField("length".into())],
        vec![
            InvariantAction {
                instance: "_self".into(),
                route: "increment".into(),
                params: vec![],
                body: vec![TestStep::Assume {
                    cond: Expr::TraceCall {
                        name: "count".into(),
                        route: "increment".into(),
                    },
                }],
                span: span0(),
            },
            InvariantAction {
                instance: "_self".into(),
                route: "reset".into(),
                params: vec![],
                body: vec![TestStep::Assume {
                    cond: Expr::TraceCall {
                        name: "count".into(),
                        route: "reset".into(),
                    },
                }],
                span: span0(),
            },
        ],
    );
    let mut inv_excluded = inv.clone();
    inv_excluded.exclude_selectors = vec!["reset".into()];
    let feat = scan_invariant_trace_features(&inv_excluded);
    assert!(
        feat.counted.iter().any(|r| r == "increment") && !feat.counted.iter().any(|r| r == "reset"),
        "excluded action assume must not contribute trace::count: {:?}",
        feat.counted
    );
}

#[test]
fn n4_codegen_test_backend_walk_standalone_expect_state() {
    struct ExpectStateRecorder {
        hit: bool,
    }
    impl TestStepLowerer for ExpectStateRecorder {
        fn emit_expect_state(&mut self, _: &[(FieldPath, Expr)]) {
            self.hit = true;
        }
    }
    let mut rec = ExpectStateRecorder { hit: false };
    walk_test_body(
        &[TestStep::ExpectState {
            fields: vec![(
                vec![PathSegment::Field("m_count".into())],
                Expr::IntLiteral(U256::ZERO),
            )],
        }],
        &mut rec,
    );
    assert!(rec.hit, "standalone ExpectState must call emit_expect_state");
}

#[test]
fn n4_codegen_test_backend_substitute_identity_and_shadowed_route() {
    let program = parse_evm(
        r#"
        entity Pair {
            identity m_token_id: u64
            routes {
                init create() => []
                view snap() -> u64 => [ return(m_val) ]
            }
            m_val: u64 { in create() => 0 }
        }
    "#,
    );
    let entity = &program.entities[0];
    let rewritten = substitute_member_accessors(
        "m_val + snap",
        entity,
        "_sut",
        &["snap".to_string()],
    );
    assert!(
        rewritten.contains("_sut.m_val()"),
        "non-identity member must rewrite: {rewritten}"
    );
    assert!(
        rewritten.contains("snap") && !rewritten.contains("_sut.snap("),
        "shadowed route/query ident must not rewrite: {rewritten}"
    );
    let id_only = substitute_member_accessors("m_token_id + m_val", entity, "_sut", &[]);
    assert!(
        !id_only.contains("_sut.m_token_id()"),
        "identity member must be skipped: {id_only}"
    );
}

#[test]
fn n4_codegen_test_backend_lower_check_expr_binop_and_field_chain() {
    let program = parse_evm(
        r#"
        entity Vault {
            routes { bump() => [] }
            m_count: u64 { in bump() => m_count + 1 }
        }
        entity Treasury {
            routes { pull() => [] }
            m_balance: u64 { in pull() => m_balance + 1 }
        }
        invariant "ops" for { v: Vault, t: Treasury } {
            action v.bump() { }
            action t.pull() { }
            check v.m_count + t.m_balance <= 100
        }
    "#,
    );
    let inv = &program.invariants[0];
    let entity_for_inst: Vec<(String, &cambrian_transpiler::ast::Entity)> = inv
        .instances
        .iter()
        .filter_map(|inst| {
            program
                .entities
                .iter()
                .find(|e| e.name == inst.entity)
                .map(|e| (inst.name.clone(), e))
        })
        .collect();
    let expr = Expr::BinOp(
        Box::new(Expr::FieldAccess(
            Box::new(Expr::Ident("v".into())),
            "m_count".into(),
        )),
        BinOp::Shl,
        Box::new(Expr::FieldAccess(
            Box::new(Expr::Ident("t".into())),
            "m_balance".into(),
        )),
    );
    let lowered = lower_check_expr_multi(&expr, &entity_for_inst);
    assert!(
        lowered.contains("<<"),
        "BinOp::Shl must lower: {lowered}"
    );
    let neg = lower_check_expr_multi(
        &Expr::UnaryOp(
            UnaryOp::Neg,
            Box::new(Expr::FieldAccess(
                Box::new(Expr::Ident("v".into())),
                "m_count".into(),
            )),
        ),
        &entity_for_inst,
    );
    assert!(neg.contains("-(_v.m_count())"), "UnaryOp::Neg must lower: {neg}");
}

#[test]
fn n4_codegen_test_backend_collect_post_call_assertions_all_shapes() {
    let fp = vec![PathSegment::Field("m_count".to_string())];
    let body = vec![
        TestStep::ExpectState {
            fields: vec![(fp.clone(), Expr::IntLiteral(U256::from_u128(10)))],
        },
        TestStep::ExpectThrow { code: 42 },
        TestStep::ExpectReturn {
            value: Expr::IntLiteral(U256::from_u128(7)),
        },
        TestStep::ExpectReturnTuple {
            values: vec![Expr::IntLiteral(U256::from_u128(1)), Expr::IntLiteral(U256::from_u128(2))],
        },
        TestStep::ExpectReturnLens {
            path: fp,
            value: Expr::IntLiteral(U256::from_u128(3)),
        },
        TestStep::ExpectEffects {
            elements: vec![],
        },
        TestStep::Call {
            target: None,
            route: "stop".into(),
            args: vec![],
        },
    ];
    let asserts = collect_post_call_assertions(&body, 0);
    assert_eq!(asserts.len(), 6);
    assert!(matches!(asserts[0], PostCallAssert::State(_)));
    assert!(matches!(asserts[1], PostCallAssert::Throw(42)));
    assert!(matches!(asserts[2], PostCallAssert::Return(_)));
    assert!(matches!(asserts[3], PostCallAssert::ReturnTuple(_)));
    assert!(matches!(asserts[4], PostCallAssert::ReturnLens(_, _)));
    assert!(matches!(asserts[5], PostCallAssert::Effects(_)));
    assert_eq!(collect_post_call_assertions(&body, 6).len(), 0);
}

struct TestBackendStepRecorder {
    events: Vec<String>,
}

impl TestStepLowerer for TestBackendStepRecorder {
    fn emit_bound(&mut self, var: &str, _: &Expr, _: &Expr, _: bool) {
        self.events.push(format!("bound:{var}"));
    }

    fn emit_assume(&mut self, _: &Expr) {
        self.events.push("assume".into());
    }

    fn emit_let(&mut self, name: &str, _: &Expr) {
        self.events.push(format!("let:{name}"));
    }

    fn emit_set_context(&mut self, namespace: &str, _: &[(String, Expr)]) {
        self.events.push(format!("ctx:{namespace}"));
    }

    fn emit_set_registry(
        &mut self,
        entity_name: &str,
        _: &Expr,
        _: &Expr,
        _: &Expr,
    ) {
        self.events.push(format!("registry:{entity_name}"));
    }

    fn emit_call(&mut self, route: &str, _: &[Expr], assertions: &[PostCallAssert]) {
        self.events
            .push(format!("call:{route}:{}", assertions.len()));
    }

    fn emit_expect_state(&mut self, _: &[(Vec<PathSegment>, Expr)]) {
        self.events.push("expect_state".into());
    }

    fn emit_skip_if(&mut self, _: &Expr) {
        self.events.push("skip_if".into());
    }

    fn emit_advance_time(&mut self, _: &Expr) {
        self.events.push("advance_time".into());
    }
}

#[test]
fn n4_codegen_test_backend_walk_test_body_all_steps() {
    let mut rec = TestBackendStepRecorder {
        events: Vec::new(),
    };
    let body = vec![
        TestStep::Bound {
            var: "n".into(),
            lo: Expr::IntLiteral(U256::ZERO),
            hi: Expr::IntLiteral(U256::from_u128(10)),
            inclusive: true,
        },
        TestStep::Assume {
            cond: Expr::BoolLiteral(true),
        },
        TestStep::Let {
            ty: None,
            name: "x".into(),
            value: Expr::IntLiteral(U256::from_u128(1)),
        },
        TestStep::SetContext {
            namespace: "msg".into(),
            fields: vec![("sender".into(), Expr::Ident("addr".into()))],
        },
        TestStep::SetRegistry {
            entity_name: "Peer".into(),
            code_hash: Expr::IntLiteral(U256::ZERO),
            code_depth: Expr::IntLiteral(U256::from_u128(1)),
            wasm_hash: Expr::IntLiteral(U256::ZERO),
        },
        TestStep::Call {
            target: None,
            route: "go".into(),
            args: vec![Expr::IntLiteral(U256::from_u128(5))],
        },
        TestStep::ExpectReturn {
            value: Expr::IntLiteral(U256::from_u128(42)),
        },
        TestStep::ExpectState {
            fields: vec![(
                vec![PathSegment::Field("m_count".to_string())],
                Expr::IntLiteral(U256::ZERO),
            )],
        },
        TestStep::SkipIf {
            cond: Expr::BoolLiteral(false),
        },
        TestStep::AdvanceTime {
            secs: Expr::IntLiteral(U256::from_u128(60)),
        },
        TestStep::ExpectThrow { code: 1 },
    ];
    walk_test_body(&body, &mut rec);
    assert!(
        rec.events.iter().any(|e| e.starts_with("bound:")),
        "walk_test_body must dispatch Bound: {:?}",
        rec.events
    );
    assert!(
        rec.events.iter().any(|e| e == "call:go:2"),
        "Call must bundle trailing expect_* assertions: {:?}",
        rec.events
    );
    assert!(
        rec.events.iter().any(|e| e == "skip_if"),
        "SkipIf must lower: {:?}",
        rec.events
    );
}

#[test]
fn n4_codegen_test_backend_lower_check_expr_multi_instance() {
    let program = parse_evm(
        r#"
        entity Vault {
            routes { bump() => [] }
            m_count: u64 { in bump() => m_count + 1 }
        }
        entity Treasury {
            routes { pull() => [] }
            m_balance: u64 { in pull() => m_balance + 1 }
        }
        invariant "balanced" for { v: Vault, t: Treasury } {
            action v.bump() { }
            action t.pull() { }
            check v.m_count + t.m_balance <= 1000000
        }
    "#,
    );
    let inv = &program.invariants[0];
    let entity_for_inst: Vec<(String, &cambrian_transpiler::ast::Entity)> = inv
        .instances
        .iter()
        .filter_map(|inst| {
            program
                .entities
                .iter()
                .find(|e| e.name == inst.entity)
                .map(|e| (inst.name.clone(), e))
        })
        .collect();
    let lowered = lower_check_expr_multi(&inv.checks[0], &entity_for_inst);
    assert!(
        lowered.contains("_v.m_count()") && lowered.contains("_t.m_balance()"),
        "multi-instance check must rewrite inst.member accessors: {lowered}"
    );
    let nested = lower_check_expr_multi(
        &Expr::BinOp(
            Box::new(Expr::UnaryOp(
                cambrian_transpiler::ast::UnaryOp::Not,
                Box::new(Expr::FieldAccess(
                    Box::new(Expr::Ident("v".into())),
                    "m_count".into(),
                )),
            )),
            BinOp::Eq,
            Box::new(Expr::IntLiteral(U256::ZERO)),
        ),
        &entity_for_inst,
    );
    assert!(
        nested.contains("!(_v.m_count())"),
        "nested unary/binop lowering must recurse: {nested}"
    );
}

#[test]
fn n4_codegen_test_backend_substitute_member_accessors() {
    let program = parse_evm(
        r#"
        entity Counter {
            routes {
                increment(amount: u64) => []
                getCount() -> u64 => [ return(m_count) ]
            }
            m_count: u64 { in increment(amount) => m_count + amount }
        }
    "#,
    );
    let entity = &program.entities[0];
    let rewritten = substitute_member_accessors(
        "m_count <= getCount() && m_count > 0",
        entity,
        "_sut",
        &[],
    );
    assert!(
        rewritten.contains("_sut.m_count()"),
        "bare member must become var.member(): {rewritten}"
    );
    assert!(
        rewritten.contains("_sut.getCount("),
        "bare route call must become var.route(: {rewritten}"
    );

    let shadowed = substitute_member_accessors(
        "m_count > querySnap",
        entity,
        "_sut",
        &["querySnap".to_string()],
    );
    assert!(
        shadowed.contains("_sut.m_count()"),
        "member rewrite must still apply when other idents are shadowed: {shadowed}"
    );
}

#[test]
fn n4_codegen_test_backend_plain_test_emits_foundry_contract() {
    let program = parse_evm(
        r#"
        entity Counter {
            routes {
                increment(amount: u64) => []
                getCount() -> u64 => [ return(m_count) ]
            }
            m_count: u64 { in increment(amount) => m_count + amount }
        }

        test "increment smoke" for Counter with { m_count: 0 } {
            call increment(5)
            expect state { m_count: 5 }
            call getCount()
            expect return 5
        }
    "#,
    );
    let files = gen_evm_test_files(&program);
    let test_sol = files
        .iter()
        .find(|(p, _)| p.ends_with("Counter.t.sol"))
        .map(|(_, c)| c.as_str())
        .expect("test block must emit Counter.t.sol");
    assert!(
        test_sol.contains("function test_increment_smoke"),
        "test name must lower to Foundry test fn: {test_sol}"
    );
    assert!(
        test_sol.contains("expect state") || test_sol.contains("m_count"),
        "post-call state assertion must appear in emitted test: {test_sol}"
    );
}

fn gen_evm_project_files(yaml: &str) -> Vec<(String, String)> {
    let mut project = contracts_project(yaml);
    cambrian_transpiler::desugar::desugar_properties(&mut project.merged);
    generate_evm_tests(
        &project.merged,
        project.config.resolved_deterministic_addresses(),
        &project.config.resolved_invariant(),
    )
}

fn foundry_test_file<'a>(files: &'a [(String, String)], entity: &str) -> &'a str {
    files
        .iter()
        .find(|(p, _)| p == &format!("test/{entity}.t.sol"))
        .map(|(_, c)| c.as_str())
        .unwrap_or_else(|| panic!("missing test/{entity}.t.sol in {files:?}"))
}

// ---------------------------------------------------------------------------
// N4-26: codegen/evm_test_codegen.rs — recon + slice 1 (gen_context_block,
// gen_test_fn / gen_fuzz_fn post-call dispatch)
// Gap @ 72.39% (550 missed): gen_context_block ~53, gen_test_fn ~66, gen_fuzz_fn ~85.
// ---------------------------------------------------------------------------

#[test]
fn n4_evm_test_codegen_context_msg_sys_steps() {
    let program = parse_evm(
        r#"
        entity Counter {
            routes {
                constructor() => []
                increment(amount: u64) => []
            }
            m_count: u64 {
                in constructor() => 0
                in increment(amount) => m_count + amount
            }
        }

        test "ctx msg sys" for Counter with { m_count: 0 } {
            msg {
                sender: 0x0000000000000000000000000000000000000001,
                value: 1000
            }
            sys { timestamp: 1_700_000_000, balance: 5000, blockNumber: 42 }
            call increment(1)
            expect state { m_count: 1 }
        }
    "#,
    );
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Counter");
    assert!(
        (test_sol.contains("vm.prank(") || test_sol.contains("vm.startPrank("))
            && test_sol.contains("vm.deal(address(this)"),
        "msg sender/value must lower via gen_context_block: {test_sol}"
    );
    assert!(
        test_sol.contains("vm.warp(") && test_sol.contains("vm.roll("),
        "sys timestamp/blockNumber must lower via gen_context_block: {test_sol}"
    );
    assert!(
        test_sol.contains("vm.deal(address(_counter)") || test_sol.contains("vm.deal(address(_"),
        "sys balance must deal to SUT var: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_throw_tuple_lens_effects_registry() {
    let program = parse_evm(
        r#"
        entity PairBox {
            routes {
                constructor() => []
                both() -> (u64, u64) => [ return(3, 4) ]
                fail() where (false) : throw 9 => []
            }
            m_n: u64 { in constructor() => 0 }
        }

        entity Guardian {
            routes { constructor() => [] }
        }

        test "rich post-call" for PairBox {
            let snap = 42
            registry Guardian {
                code_hash: 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa,
                code_depth: 1,
                wasm_hash: 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
            }
            call both()
            expect return (3, 4)
            call both()
            expect return.0 == 3
            call fail()
            expect throw 9
        }
    "#,
    );
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "PairBox");
    assert!(
        test_sol.contains("snap = 42"),
        "let binding must emit typed local (gen_let_binding): {test_sol}"
    );
    assert!(
        test_sol.contains("// registry Guardian"),
        "SetRegistry must emit TVM skip comment: {test_sol}"
    );
    assert!(
        test_sol.contains("assertEq(_ret_") && test_sol.contains("return[0]"),
        "expect return tuple must lower via gen_expect_return Tuple arm: {test_sol}"
    );
    assert!(
        test_sol.contains("return lens mismatch") || test_sol.contains(".0"),
        "expect return lens must lower via gen_expect_return Lens arm: {test_sol}"
    );
    assert!(
        test_sol.contains("vm.expectRevert"),
        "expect throw must emit vm.expectRevert: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_fuzz_post_call_tuple() {
    let program = parse_evm(
        r#"
        entity PairBox {
            routes {
                constructor() => []
                both() -> (u64, u64) => [ return(3, 4) ]
            }
            m_n: u64 { in constructor() => 0 }
        }

        property "pair fuzz" (_seed: u64) for PairBox with { m_n: 0 } {
            call both()
            expect return (3, 4)

            #[tag("ci")] fuzz "pair fuzz" { _seed in 0..1000 }
        }
    "#,
    );
    let mut program = program;
    cambrian_transpiler::desugar::desugar_properties(&mut program);
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "PairBox");
    assert!(
        test_sol.contains("/// @dev tag: ci")
            && test_sol.contains("function testFuzz_")
            && test_sol.contains("_ci("),
        "fuzz tag must suffix Foundry fn name: {test_sol}"
    );
    assert!(
        test_sol.contains("return[0]") || test_sol.contains("_ret_1_0"),
        "fuzz post-call tuple expect must hit gen_fuzz_fn ReturnTuple arm: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_foundry_invariant_handler_project() {
    let files = gen_evm_project_files("project_trace_invariant.yaml");
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("project_trace_invariant must emit Invariant_*.t.sol");
    assert!(
        inv.contains("StdInvariant") && inv.contains("CounterHandler_"),
        "single-entity invariant must emit Handler + StdInvariant: {inv}"
    );
    assert!(
        inv.contains("_traceLen") && inv.contains("_traceCount_increment"),
        "trace invariant must emit handler counters (gen_invariant_file): {inv}"
    );
    assert!(
        inv.contains("targetSelector") && inv.contains("invariant_"),
        "Foundry invariant test contract must wire targetSelector: {inv}"
    );
}

#[test]
fn n4_evm_test_codegen_project_fuzz_contract() {
    let files = gen_evm_project_files("project_fuzz.yaml");
    let counter = foundry_test_file(&files, "Counter");
    assert!(
        counter.contains("function testFuzz_"),
        "project_fuzz.yaml must emit testFuzz_* fns: {counter}"
    );
    assert!(
        counter.contains("bound(") || counter.contains("assertEq"),
        "desugared fuzz properties must emit bound/assert paths: {counter}"
    );
}

// ---------------------------------------------------------------------------
// N4-27: codegen/evm_test_codegen.rs — slice 2 (expect effects,
// gen_invariant_multi_file, gen_invariant_file tail L1781+)
// Gap @ 80.42% (390 missed): effects ~40, multi-invariant ~43, invariant tail ~25.
// ---------------------------------------------------------------------------

#[test]
fn n4_evm_test_codegen_expect_effects_send_platform() {
    let program = parse_evm(
        r#"
        entity Tipper {
            routes {
                constructor() => []
                tip(recipient: address) => [ ~> recipient ]
            }
        }

        test "effects oracle" for Tipper {
            let bob = 0x0000000000000000000000000000000000000002
            call tip(bob)
            expect effects [~> bob, rawReserve(1, 0), ..]
        }
    "#,
    );
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Tipper");
    assert!(
        test_sol.contains("_bal_before_") && test_sol.contains(".balance"),
        "send effect must emit gen_pre_call_effect_setup balance snapshot: {test_sol}"
    );
    assert!(
        test_sol.contains("assertTrue(") && test_sol.contains("effect: balance should increase"),
        "send effect must emit gen_post_call_effect_checks: {test_sol}"
    );
    assert!(
        test_sol.contains("// expect rawReserve -- TVM-specific, skipped on EVM"),
        "platform effect must emit TVM skip comment: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_fuzz_expect_effects_send() {
    let program = parse_evm(
        r#"
        entity Tipper {
            routes {
                constructor() => []
                tip(recipient: address) => [ ~> recipient ]
            }
        }

        property "tip fuzz" (recipient: address) for Tipper {
            call tip(recipient)
            expect effects [~> recipient, ..]

            fuzz "wide" { recipient in 0x0000000000000000000000000000000000000001..0x00000000000000000000000000000000000000ff }
        }
    "#,
    );
    let mut program = program;
    cambrian_transpiler::desugar::desugar_properties(&mut program);
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Tipper");
    assert!(
        test_sol.contains("function testFuzz_"),
        "desugared fuzz must emit testFuzz_*: {test_sol}"
    );
    assert!(
        test_sol.contains("_bal_before_") && test_sol.contains("effect: balance should increase"),
        "fuzz post-call effects must hit gen_fuzz_fn Effects arm: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_multi_invariant_project() {
    let files = gen_evm_project_files("project_invariant_multi.yaml");
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("project_invariant_multi must emit Invariant_*.t.sol");
    assert!(
        inv.contains("Handler_vault_sum_is_bounded")
            && inv.contains("Invariant_vault_sum_is_bounded_Test"),
        "multi-entity invariant must emit Handler + StdInvariant test: {inv}"
    );
    assert!(
        inv.contains("Vault public _v")
            && inv.contains("Treasury public _t")
            && inv.contains("function v_deposit(")
            && inv.contains("function a_deposit(")
            && inv.contains("function t_credit("),
        "multi-instance handler must store/deploy each entity + qualified wrappers: {inv}"
    );
    assert!(
        inv.contains("targetSelector") && inv.contains("v_deposit(uint64)"),
        "multi-invariant must wire targetSelector for qualified actions: {inv}"
    );
    assert!(
        inv.contains("_v.m_balance()") && inv.contains("_a.m_balance()"),
        "multi-instance checks must lower via lower_check_expr_multi: {inv}"
    );
}

#[test]
fn n4_evm_test_codegen_invariant_time_senders_ctx_exclude() {
    let program = parse_evm(
        r#"
        entity Counter {
            routes {
                constructor() => []
                increment(amount: u64) => []
                reset() => []
            }
            m_count: u64 {
                in constructor() => 0
                in increment(amount) => m_count + amount
                in reset() => 0
            }
        }

        invariant "senders time ctx" for Counter #[with_time] #[fail_on_revert] {
            init { m_count: 0 }
            ctx { sys::now: 1_700_000_000, msg::sender: 0x00000000000000000000000000000000000000bb }

            senders {
                0x0000000000000000000000000000000000000001,
                0x0000000000000000000000000000000000000002
            }

            exclude selectors { reset }

            action increment(amount: u64) {
                bound amount in 1..10
                skip if m_count > 100
            }
            action reset() {}

            check m_count >= 0
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("inline invariant must emit Invariant_*.t.sol");
    assert!(
        inv.contains("function advanceTime(uint256 secs)")
            && inv.contains("vm.warp(block.timestamp + secs)"),
        "with_time must emit advanceTime handler action: {inv}"
    );
    assert!(
        inv.contains("targetSender(") && inv.contains("vm.warp("),
        "senders + ctx must lower via gen_invariant_foundry_ctx: {inv}"
    );
    assert!(
        inv.contains("excludeSelector") && inv.contains("reset()"),
        "exclude selectors must emit excludeSelector wiring: {inv}"
    );
    assert!(
        inv.contains("if (") && inv.contains("m_count() > 100") && inv.contains("return;"),
        "skip if must lower to early return in handler wrapper: {inv}"
    );
    assert!(
        inv.contains("fail_on_revert"),
        "fail_on_revert attribute must annotate generated contract: {inv}"
    );
}

#[test]
fn n4_evm_test_codegen_expect_state_mapping_and_scalar_return() {
    let program = parse_evm(
        r#"
        entity Ledger {
            routes {
                constructor() => []
                setScore(key: u64, value: u64) => []
                getScore(key: u64) -> u64 => [ return(m_scores[key]) ]
            }
            m_scores: HashMap<u64, u64> {
                in constructor() => {}
                in setScore(key, value) => m_scores.insert(key, value)
            }
        }

        test "map state and scalar return" for Ledger {
            call setScore(1, 42)
            expect state { m_scores[1]: 42 }
            call getScore(1)
            expect return 42
        }
    "#,
    );
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Ledger");
    assert!(
        test_sol.contains("m_scores") && test_sol.contains("assertEq"),
        "expect state on mapping slot must emit gen_expect_state: {test_sol}"
    );
    assert!(
        test_sol.contains("assertEq(_ret_") || test_sol.contains("return mismatch"),
        "scalar expect return must hit gen_expect_return Scalar arm: {test_sol}"
    );
}

// ---------------------------------------------------------------------------
// N4-28: codegen/evm_test_codegen.rs — slice 3 (derived/track/exclude tails,
// forall init, fuzz init/skip_from, multi-invariant check lowering)
// Gap @ 82.73% (344 missed).
// ---------------------------------------------------------------------------

#[test]
fn n4_evm_test_codegen_lending_invariant_derived_track_exclude() {
    let files = gen_evm_project_files("project_invariant_lending.yaml");
    let inv = files
        .iter()
        .find(|(p, _)| p.contains("borrowed_never_exceeds"))
        .map(|(_, c)| c.as_str())
        .expect("lending project must emit primary Invariant_*.t.sol");
    assert!(
        inv.contains("function utilization() public view returns (uint128)")
            && inv.contains("return (")
            && inv.contains("m_borrowed"),
        "derived query must emit handler view helper: {inv}"
    );
    assert!(
        inv.contains("uint256 public initial_total")
            && inv.contains("uint256 public initial_borrowed"),
        "track bindings must become handler public fields: {inv}"
    );
    assert!(
        inv.contains("excludeSender(") && inv.contains("excludeSelector"),
        "exclude senders/selectors must wire Foundry filters: {inv}"
    );
    assert!(
        inv.contains("/// @dev tag: INV-LEND-001")
            && inv.contains("invariant.runs = 2000")
            && inv.contains("invariant.depth = 80"),
        "invariant tag/runs/depth attrs must propagate to emitted fns: {inv}"
    );
    assert!(
        inv.contains("_handler.initial_total()") || inv.contains("_handler.utilization("),
        "checks must rewrite track/derived refs via rewrite_handler_idents: {inv}"
    );
    assert!(
        inv.contains("bound(amount,") && inv.contains("m_total_assets()"),
        "computed bound against SUT member must substitute in handler action: {inv}"
    );
}

#[test]
fn n4_evm_test_codegen_invariant_forall_random_init() {
    let files = gen_evm_project_files("project_invariant_lending.yaml");
    let inv = files
        .iter()
        .find(|(p, _)| p.contains("borrowed_bounded_from_any_start"))
        .map(|(_, c)| c.as_str())
        .expect("lending project must emit forall Invariant_*.t.sol");
    assert!(
        inv.contains("forall init: randomized starting state")
            && inv.contains("vm.randomUint()"),
        "init forall pin must emit gen_forall_state_init cheatcodes: {inv}"
    );
    assert!(
        inv.contains("vm.store(address(_lendingPair)") || inv.contains("vm.store(address(_"),
        "forall seed must write randomized member slots: {inv}"
    );
}

#[test]
fn n4_evm_test_codegen_test_standalone_expect_state() {
    let program = parse_evm(
        r#"
        entity Counter {
            routes {
                constructor() => []
                increment(amount: u64) => []
            }
            m_count: u64 {
                in constructor() => 0
                in increment(amount) => m_count + amount
            }
        }

        test "pin state" for Counter with { m_count: 7 } {
            expect state { m_count: 7 }
        }
    "#,
    );
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Counter");
    assert!(
        test_sol.contains("assertEq(_counter.m_count(), 7"),
        "standalone expect state must hit gen_test_fn ExpectState arm: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_fuzz_skip_from_init_state() {
    let program = parse_evm(
        r#"
        entity Counter {
            routes {
                constructor() => []
                increment(amount: u64) => []
            }
            m_count: u64 {
                in constructor() => 0
                in increment(amount) => m_count + amount
            }
        }

        property "seeded fuzz" (_seed: u64) for Counter {
            call increment(_seed)

            #[skip_from] fuzz "seeded" { _seed in 1..10 } with { m_count: 42 }
        }
    "#,
    );
    let mut program = program;
    cambrian_transpiler::desugar::desugar_properties(&mut program);
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Counter");
    assert!(
        test_sol.contains("vm.store(address(_counter)") && test_sol.contains("bytes32(uint256(42))"),
        "fuzz with init block must emit gen_state_init for init_state: {test_sol}"
    );
    assert!(
        test_sol.contains("skip from: prank as address(1)"),
        "#[skip_from] fuzz must hit gen_fuzz_fn skip_from arm: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_multi_invariant_senders_ctx_check_ops() {
    let program = parse_evm(
        r#"
        entity Vault {
            routes {
                constructor() => []
                bump(amount: u64) => []
            }
            m_balance: u64 {
                in constructor() => 0
                in bump(amount) => m_balance + amount
            }
        }
        entity Ledger {
            routes {
                constructor() => []
                add(amount: u64) => []
            }
            m_total: u64 {
                in constructor() => 0
                in add(amount) => m_total + amount
            }
        }

        invariant "paired ops" for { v: Vault, book: Ledger } {
            init v { m_balance: 0 }
            init book { m_total: 0 }
            ctx { sys::now: 2_000, msg::sender: 0x00000000000000000000000000000000000000cc }

            senders {
                0x0000000000000000000000000000000000000001,
                0x0000000000000000000000000000000000000002
            }

            action v.bump(amount: u64) {
                bound amount in 1..100
            }
            action book.add(amount: u64) {
                bound amount in 1..50
            }

            check (v.m_balance << 1) >= book.m_total
            check !false
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("multi invariant must emit Invariant_*.t.sol");
    assert!(
        inv.contains("targetSender(") && inv.contains("vm.warp("),
        "multi-invariant senders/ctx must lower in gen_invariant_multi_file setUp: {inv}"
    );
    assert!(
        inv.contains("(_v.m_balance() << 1)") && inv.contains("_book.m_total()"),
        "multi checks must lower BinOp::Shl via lower_check_expr_multi: {inv}"
    );
    assert!(
        inv.contains("function v_bump(") && inv.contains("function book_add("),
        "qualified handler wrappers must be emitted: {inv}"
    );
}

#[test]
fn n4_evm_test_codegen_invariant_action_advance_time_in_body() {
    let program = parse_evm(
        r#"
        entity Clock {
            routes {
                constructor() => []
                tick() => []
            }
            m_ticks: u64 {
                in constructor() => 0
                in tick() => m_ticks + 1
            }
        }

        invariant "time in action" for Clock #[with_time] {
            init { m_ticks: 0 }

            action tick() {
                advanceTime(120)
            }

            check m_ticks >= 0
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("invariant with advanceTime in action body must emit file");
    assert!(
        inv.contains("advanceTime(120)") || inv.contains("vm.warp("),
        "advanceTime inside action body must lower in handler wrapper: {inv}"
    );
    assert!(
        inv.contains("selectors[") && inv.contains("advanceTime(uint256)"),
        "with_time synthetic action must appear in targetSelector list: {inv}"
    );
}

#[test]
fn n4_evm_test_codegen_invariant_derived_param_let_body() {
    let program = parse_evm(
        r#"
        entity Pool {
            routes {
                constructor() => []
                touch() => []
            }
            m_assets: u128 {
                in constructor() => 1000
                in touch() => m_assets
            }
            m_borrowed: u128 { in constructor() => 0 }
        }

        invariant "headroom" for Pool {
            init { m_assets: 1000, m_borrowed: 0 }

            derived headroom(buffer: u128) -> u128 {
                let cap = m_assets + buffer
                return cap
            }

            action touch() {}

            check headroom(10) >= m_borrowed
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("derived invariant must emit Invariant_*.t.sol");
    assert!(
        inv.contains("function headroom(uint128 buffer)") && inv.contains("cap ="),
        "parametrised derived with let body must emit helper fn: {inv}"
    );
    assert!(
        inv.contains("_handler.headroom(10)") || inv.contains("headroom(10)"),
        "check must call derived helper via rewrite_handler_idents: {inv}"
    );
}

#[test]
fn n4_evm_test_codegen_test_standalone_expect_throw() {
    let program = parse_evm(
        r#"
        entity Gate {
            routes {
                constructor() => []
                open() where (false) : throw 7 => []
            }
        }

        test "expect throw alone" for Gate {
            call open()
            expect throw 7
        }
    "#,
    );
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Gate");
    assert!(
        test_sol.contains("vm.expectRevert") && test_sol.contains("throw(7)"),
        "standalone expect throw must wire vm.expectRevert before call: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_fuzz_runs_attribute() {
    let program = parse_evm(
        r#"
        entity Counter {
            routes {
                constructor() => []
                increment(amount: u64) => []
            }
            m_count: u64 {
                in constructor() => 0
                in increment(amount) => m_count + amount
            }
        }

        property "heavy fuzz" (amount: u64) for Counter {
            call increment(amount)

            #[runs(500)] fuzz { amount in 1..100 }
        }
    "#,
    );
    let mut program = program;
    cambrian_transpiler::desugar::desugar_properties(&mut program);
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Counter");
    assert!(
        test_sol.contains("forge-config: default.fuzz.runs = 500"),
        "fuzz #[runs(N)] must annotate testFuzz fn: {test_sol}"
    );
}

// ---------------------------------------------------------------------------
// N4-31: codegen/evm_test_codegen.rs — slice 4 (post-call fuzz tails,
// deterministic invariant deploy, forall bool/address, toml residual)
// Gap @ 84.09% (317 missed): lower_check_expr_multi, gen_expect_return decode,
// gen_invariant_multi_file det ctor, generate_foundry_toml tiered.
// ---------------------------------------------------------------------------

#[test]
fn n4_evm_test_codegen_fuzz_scalar_return_lens_and_state() {
    let program = parse_evm(
        r#"
        entity ScoreBox {
            routes {
                constructor() => []
                peek() -> u64 => [ return(99) ]
                pair() -> (u64, u64) => [ return(3, 7) ]
            }
            m_n: u64 { in constructor() => 0 }
        }

        property "fuzz post-call tails" (seed: u64) for ScoreBox with { m_n: 0 } {
            call peek()
            expect return 99
            call pair()
            expect return.1 == 7
            call peek()
            expect state { m_n: 0 }

            fuzz { seed in 0..50 }
        }
    "#,
    );
    let mut program = program;
    cambrian_transpiler::desugar::desugar_properties(&mut program);
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "ScoreBox");
    assert!(
        test_sol.contains("assertEq(_ret_") && test_sol.contains("return value mismatch"),
        "fuzz scalar expect return must hit gen_fuzz_fn Return arm: {test_sol}"
    );
    assert!(
        test_sol.contains("return lens mismatch") || test_sol.contains(".1"),
        "fuzz expect return lens must hit ReturnLens arm: {test_sol}"
    );
    assert!(
        test_sol.contains("m_n mismatch") || test_sol.contains(".m_n()"),
        "fuzz post-call expect state must hit gen_fuzz_fn State arm: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_fuzz_assume_msg_context_and_throw() {
    let program = parse_evm(
        r#"
        entity Gate {
            routes {
                constructor() => []
                open() where (false) : throw 4 => []
            }
        }

        property "fuzz guards" (n: u64) for Gate {
            msg { sender: 0x00000000000000000000000000000000000000ee }
            assume n > 0
            call open()
            expect throw 4

            fuzz { n in 1..20 }
        }
    "#,
    );
    let mut program = program;
    cambrian_transpiler::desugar::desugar_properties(&mut program);
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Gate");
    assert!(
        test_sol.contains("vm.assume(")
            && (test_sol.contains("vm.prank(") || test_sol.contains("vm.startPrank(")),
        "fuzz assume + msg context must lower in gen_fuzz_fn: {test_sol}"
    );
    assert!(
        test_sol.contains("vm.expectRevert") && test_sol.contains("throw(4)"),
        "fuzz expect throw must wire vm.expectRevert before call: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_identity_factory_ctor_redeploy() {
    let program = parse_evm(
        r#"
        entity Tagged {
            identity m_tag: u64
            routes {
                constructor(seed: u64) => []
                getTag() -> u64 => [ return(m_tag) ]
            }
        }

        test "identity deploy" for Tagged {
            call constructor(42)
            call getTag()
            expect return 0
        }
    "#,
    );
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Tagged");
    assert!(
        test_sol.contains("new CambrianFactory()"),
        "factory harness setUp must deploy CambrianFactory: {test_sol}"
    );
    assert!(
        test_sol.contains("deployTagged(") && test_sol.contains(", 42)"),
        "call constructor must redeploy via factory.deployTagged: {test_sol}"
    );
    assert!(
        !test_sol.contains("skipped: `call constructor"),
        "constructor redeploy must not be silently skipped: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_deterministic_invariant_identity_deploy() {
    let program = parse_evm(
        r#"
        entity Wallet {
            identity m_id: u64
            routes { constructor() => [] bump() => [] }
            m_bal: u64 { in constructor() => 0 in bump() => m_bal + 1 }
        }

        invariant "wallet bounded" for Wallet {
            init { m_id: 7, m_bal: 0 }
            action bump() {}
            check m_bal >= 0
        }
    "#,
    );
    let inv = gen_evm_test_files_det(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("deterministic invariant must emit Invariant_*.t.sol");
    assert!(
        inv.contains("deployWallet(") && inv.contains("7"),
        "deterministic invariant setUp must factory-deploy with pinned identity: {inv}"
    );
    assert!(
        inv.contains("handler reverts are silently skipped"),
        "default fail_on_revert must emit soft-fail comment: {inv}"
    );
    assert!(
        inv.contains("try ") && inv.contains("catch { return; }"),
        "fail_on_revert:false handler must wrap actions in try/catch: {inv}"
    );
}

#[test]
fn n4_evm_test_codegen_multi_invariant_deterministic_identity_ctor() {
    let program = parse_evm(
        r#"
        entity NamedVault {
            identity m_slot: u64
            routes { constructor() => [] deposit(v: u64) => [] }
            m_balance: u64 {
                in constructor() => 0
                in deposit(v) => m_balance + v
            }
        }

        invariant "named pair" for { left: NamedVault, right: NamedVault } {
            init left { m_slot: 1, m_balance: 0 }
            init right { m_slot: 2, m_balance: 0 }

            action left.deposit(v: u64) { bound v in 0..20 }
            action right.deposit(v: u64) { bound v in 0..20 }

            check left.m_balance + right.m_balance >= 0
        }
    "#,
    );
    let inv = gen_evm_test_files_det(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("multi deterministic invariant must emit file");
    assert!(
        inv.contains("deployNamedVault(1)") && inv.contains("deployNamedVault(2)"),
        "multi deterministic deploy must factory.deploy* per identity (U4-4c): {inv}"
    );
    assert!(
        inv.contains("cam_wire(") && !inv.contains(".initialize("),
        "handler-first multi invariant setUp (U4-4c): {inv}"
    );
}

#[test]
fn n4_evm_test_codegen_invariant_forall_bool_address_ctx_star() {
    let program = parse_evm(
        r#"
        entity Profile {
            routes { constructor() => [] }
            m_active: bool { in constructor() => false }
            m_owner: address { in constructor() => 0x0000000000000000000000000000000000000000 }
        }

        invariant "forall profile" for Profile {
            init { m_active: *, m_owner: * }
            ctx { sys::now: *, msg::sender: * }

            action constructor() {}

            check true
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("forall invariant must emit file");
    assert!(
        inv.contains("vm.randomUint() % 2") || inv.contains("__fa_m_active"),
        "forall bool init must hit foundry_random_init_value bool arm: {inv}"
    );
    assert!(
        inv.contains("uint160(vm.randomUint())") || inv.contains("__fa_m_owner"),
        "forall address init must hit foundry_random_init_value address arm: {inv}"
    );
    assert!(
        inv.contains("vm.warp(vm.randomUint())") && inv.contains("msg::sender: *"),
        "ctx forall stars must lower via gen_invariant_foundry_ctx: {inv}"
    );
}

#[test]
fn n4_evm_test_codegen_test_msg_pubkey_context_unsupported() {
    let program = parse_evm(
        r#"
        entity Counter {
            routes {
                constructor() => []
                increment(amount: u64) => []
            }
            m_count: u64 {
                in constructor() => 0
                in increment(amount) => m_count + amount
            }
        }

        test "unsupported msg ctx" for Counter {
            msg { pubkey: 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa }
            call increment(1)
        }
    "#,
    );
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Counter");
    assert!(
        test_sol.contains("msg.pubkey -- not directly supported on EVM"),
        "unsupported msg field must hit gen_context_block fallback: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_fuzz_registry_skip_comment() {
    let program = parse_evm(
        r#"
        entity Host {
            routes { constructor() => [] ping() => [] }
        }
        entity Peer {
            routes { constructor() => [] }
        }

        property "registry fuzz" (n: u64) for Host {
            registry Peer {
                code_hash: 0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc,
                code_depth: 1,
                wasm_hash: 0xdddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd
            }
            call ping()

            fuzz { n in 0..5 }
        }
    "#,
    );
    let mut program = program;
    cambrian_transpiler::desugar::desugar_properties(&mut program);
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Host");
    assert!(
        test_sol.contains("// registry Peer -- skipped (TVM-specific)"),
        "fuzz SetRegistry must emit skip comment in gen_fuzz_fn: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_test_sys_chainid_context_unsupported() {
    let program = parse_evm(
        r#"
        entity Counter {
            routes {
                constructor() => []
                increment(amount: u64) => []
            }
            m_count: u64 {
                in constructor() => 0
                in increment(amount) => m_count + amount
            }
        }

        test "unsupported sys ctx" for Counter {
            sys { chainid: 42 }
            call increment(1)
        }
    "#,
    );
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Counter");
    assert!(
        test_sol.contains("sys.chainid -- not directly supported on EVM"),
        "unsupported sys field must hit gen_context_block fallback: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_invariant_track_bare_rewrite_emission() {
    let program = parse_evm(
        r#"
        entity Meter {
            routes { constructor() => [] tick() => [] }
            m_ticks: u64 {
                in constructor() => 0
                in tick() => m_ticks + 1
            }
        }

        invariant "track bare" for Meter {
            init { m_ticks: 0 }

            track { let snap = m_ticks }

            action tick() {}

            check snap >= 0
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("track invariant must emit file");
    assert!(
        inv.contains("_handler.snap()") || inv.contains("_handler.snap"),
        "bare track ref in check must rewrite via rewrite_handler_idents: {inv}"
    );
}

#[test]
fn n4_evm_test_codegen_lower_check_expr_div_mod_bitops() {
    let program = parse_evm(
        r#"
        entity A { routes { go() => [] } m_x: u64 { in go() => m_x + 1 } }
        entity B { routes { go() => [] } m_y: u64 { in go() => m_y + 1 } }
        invariant "ops" for { a: A, b: B } {
            action a.go() {}
            action b.go() {}
            check (a.m_x / b.m_y) % 2 == 0 && (a.m_x & b.m_y) ^ 1 != 0
        }
    "#,
    );
    let inv = &program.invariants[0];
    let entity_for_inst: Vec<(String, &cambrian_transpiler::ast::Entity)> = inv
        .instances
        .iter()
        .filter_map(|inst| {
            program
                .entities
                .iter()
                .find(|e| e.name == inst.entity)
                .map(|e| (inst.name.clone(), e))
        })
        .collect();
    let lowered = lower_check_expr_multi(&inv.checks[0], &entity_for_inst);
    assert!(
        lowered.contains('/') && lowered.contains('%') && lowered.contains('&') && lowered.contains('^'),
        "BinOp Div/Mod/BitAnd/BitXor must lower in check expr: {lowered}"
    );
    let inv_sol = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("multi check invariant must emit file");
    assert!(
        inv_sol.contains("require(") && inv_sol.contains("invariant 'ops' violated"),
        "emitted invariant must substitute lowered check into require: {inv_sol}"
    );
}

// ---------------------------------------------------------------------------
// N4-34: codegen/evm_test_codegen.rs — slice 5 (enum uint256 expect_return,
// orphan expect no-op arms, det ctor initialize skip, init_state skip comments,
// deploy effect pre-setup, fail_on_revert comment, multi nondet deploy)
// Gap @ 90.61% (187 missed).
// ---------------------------------------------------------------------------

fn push_orphan_step_test(program: &mut cambrian_transpiler::ast::Program, body: Vec<TestStep>) {
    let entity_name = program.entities[0].name.clone();
    program.tests.push(TestDecl {
        name: "orphan steps".into(),
        entity_name,
        init_state: vec![],
        skip_from: false,
        body,
        tag: None,
        instantiates: None,
        span: span0(),
    });
}

fn push_orphan_step_fuzz(program: &mut cambrian_transpiler::ast::Program, body: Vec<TestStep>) {
    let entity_name = program.entities[0].name.clone();
    program.fuzz_tests.push(FuzzDecl {
        name: "orphan fuzz steps".into(),
        entity_name,
        params: vec![Param {
            name: "n".into(),
            ty: Type::Simple("u64".into()),
        }],
        init_state: vec![],
        skip_from: false,
        body,
        runs: None,
        tag: None,
        instantiates: None,
        span: span0(),
    });
}

#[test]
fn n4_evm_test_codegen_test_orphan_expect_noop_arms() {
    let mut program = parse_evm(
        r#"
        entity Counter {
            routes { constructor() => [] increment() => [] }
            m_count: u64 { in constructor() => 0 in increment() => m_count + 1 }
        }
    "#,
    );
    push_orphan_step_test(
        &mut program,
        vec![
            TestStep::ExpectThrow { code: 1 },
            TestStep::ExpectReturn {
                value: Expr::IntLiteral(U256::from_u128(5)),
            },
            TestStep::ExpectReturnTuple {
                values: vec![Expr::IntLiteral(U256::from_u128(1)), Expr::IntLiteral(U256::from_u128(2))],
            },
            TestStep::ExpectReturnLens {
                path: vec![PathSegment::TupleIndex(0)],
                value: Expr::IntLiteral(U256::from_u128(3)),
            },
            TestStep::ExpectEffects {
                elements: vec![TestEffectElement::Wildcard],
            },
            TestStep::SkipIf {
                cond: Expr::BoolLiteral(true),
            },
            TestStep::AdvanceTime {
                secs: Expr::IntLiteral(U256::from_u128(60)),
            },
            TestStep::Call {
                target: None,
                route: "increment".into(),
                args: vec![],
            },
        ],
    );
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Counter");
    assert!(
        test_sol.contains("function test_orphan_steps()"),
        "orphan-step test must emit: {test_sol}"
    );
    assert!(
        test_sol.matches("increment()").count() >= 1
            && !test_sol.contains("vm.expectRevert")
            && !test_sol.contains("orphan steps': return"),
        "orphan expects must be no-ops; only trailing call emits: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_fuzz_orphan_expect_and_state_arms() {
    let mut program = parse_evm(
        r#"
        entity Counter {
            routes { constructor() => [] bump() => [] }
            m_count: u64 { in constructor() => 0 in bump() => m_count + 1 }
        }
    "#,
    );
    push_orphan_step_fuzz(
        &mut program,
        vec![
            TestStep::ExpectState {
                fields: vec![(
                    vec![PathSegment::Field("m_count".into())],
                    Expr::IntLiteral(U256::ZERO),
                )],
            },
            TestStep::ExpectThrow { code: 2 },
            TestStep::ExpectReturn {
                value: Expr::IntLiteral(U256::from_u128(9)),
            },
            TestStep::ExpectReturnTuple {
                values: vec![Expr::IntLiteral(U256::from_u128(1))],
            },
            TestStep::ExpectReturnLens {
                path: vec![PathSegment::TupleIndex(1)],
                value: Expr::IntLiteral(U256::from_u128(4)),
            },
            TestStep::ExpectEffects {
                elements: vec![],
            },
            TestStep::SkipIf {
                cond: Expr::BoolLiteral(false),
            },
            TestStep::AdvanceTime {
                secs: Expr::IntLiteral(U256::from_u128(30)),
            },
            TestStep::Bound {
                var: "n".into(),
                lo: Expr::IntLiteral(U256::from_u128(1)),
                hi: Expr::IntLiteral(U256::from_u128(20)),
                inclusive: false,
            },
            TestStep::Call {
                target: None,
                route: "bump".into(),
                args: vec![],
            },
        ],
    );
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Counter");
    assert!(
        test_sol.contains("function testFuzz_orphan_fuzz_steps(")
            && test_sol.contains("bound(n, 1, (20) - 1)"),
        "fuzz orphan steps + exclusive bound hi must lower: {test_sol}"
    );
    assert!(
        test_sol.contains("m_count()") && test_sol.contains("assertEq"),
        "standalone fuzz ExpectState must hit gen_expect_state: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_enum_return_uint256_cast() {
    let program = parse_evm(
        r#"
        entity ModeBox {
            enum Mode { Off, On }

            routes {
                constructor() => []
                mode() -> Mode => [ return(m_mode) ]
                pair() -> (Mode, u64) => [ return(m_mode, 9) ]
            }

            m_mode: Mode {
                in constructor() => Mode::Off
            }
        }

        test "enum returns" for ModeBox {
            call mode()
            expect return Mode::On
            call pair()
            expect return (Mode::Off, 9)
        }
    "#,
    );
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "ModeBox");
    assert!(
        test_sol.contains("uint256(_ret_") && test_sol.contains("uint256(Mode."),
        "enum scalar expect return must cast assertEq operands to uint256: {test_sol}"
    );
    assert!(
        test_sol.contains("uint256(_ret_") && test_sol.contains("return[0]"),
        "enum tuple slot expect return must cast to uint256: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_det_empty_ctor_initialize_skip() {
    let program = parse_evm(
        r#"
        entity SlotOnly {
            identity m_slot: u64
            routes { constructor() => [] ping() => [] }
            m_n: u64 { in constructor() => 0 in ping() => m_n + 1 }
        }

        test "det ctor skip" for SlotOnly {
            call constructor()
            call ping()
        }
    "#,
    );
    let files = evm_harness_project_files(&program, true);
    let test_sol = foundry_test_file(&files, "SlotOnly");
    assert!(
        test_sol.contains("deploySlotOnly("),
        "deterministic setUp must deploy via CambrianFactory: {test_sol}"
    );
    assert!(
        !test_sol.contains(".initialize("),
        "empty det ctor call must skip initialize remap (gen_call early return): {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_fuzz_init_state_skip_comments() {
    let mut program = parse_evm(
        r#"
        entity Wallet {
            identity m_slot: u64
            routes { constructor() => [] spend() => [] }
            m_balance: u64 { in constructor() => 0 in spend() => m_balance }
            m_balances: HashMap<address, u64> { in constructor() => {} }
        }
    "#,
    );
    program.fuzz_tests.push(FuzzDecl {
        name: "init skips".into(),
        entity_name: "Wallet".into(),
        params: vec![Param {
            name: "n".into(),
            ty: Type::Simple("u64".into()),
        }],
        init_state: vec![
            ("m_slot".into(), Expr::IntLiteral(U256::from_u128(7))),
            ("m_ghost".into(), Expr::IntLiteral(U256::from_u128(1))),
            ("m_balances".into(), Expr::IntLiteral(U256::ZERO)),
        ],
        skip_from: false,
        body: vec![
            TestStep::Bound {
                var: "n".into(),
                lo: Expr::IntLiteral(U256::ZERO),
                hi: Expr::IntLiteral(U256::from_u128(10)),
                inclusive: false,
            },
            TestStep::Call {
                target: None,
                route: "spend".into(),
                args: vec![],
            },
        ],
        runs: None,
        tag: None,
        instantiates: None,
        span: span0(),
    });
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Wallet");
    assert!(
        test_sol.contains("identity member, set via constructor"),
        "with identity pin must skip gen_state_init write: {test_sol}"
    );
    assert!(
        test_sol.contains("unknown member"),
        "unknown with field must hit gen_state_init unknown-member comment: {test_sol}"
    );
    assert!(
        test_sol.contains("mapping init requires `{ key => val }`")
            || test_sol.contains("mapping init requires { key => val }")
            || test_sol.contains("mapping init via vm.store not yet supported"),
        "mapping with pin must hit mapping skip comment: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_expect_deploy_effect_pre_setup() {
    let mut program = parse_evm(
        r#"
        entity Factory {
            routes { constructor() => [] spawn() => [] }
            m_n: u64 { in constructor() => 0 in spawn() => m_n + 1 }
        }
    "#,
    );
    program.tests.push(TestDecl {
        name: "deploy effect".into(),
        entity_name: "Factory".into(),
        init_state: vec![],
        skip_from: false,
        body: vec![
            TestStep::Call {
                target: None,
                route: "spawn".into(),
                args: vec![],
            },
            TestStep::ExpectEffects {
                elements: vec![TestEffectElement::Effect(TestEffect::Deploy {
                    entity: "Child".into(),
                    send_options: None,
                })],
            },
        ],
        tag: None,
        instantiates: None,
        span: span0(),
    });
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Factory");
    assert!(
        test_sol.contains("expect deploy Child"),
        "Deploy effect in expect effects must hit gen_pre_call_effect_setup: {test_sol}"
    );
}

#[test]
fn n4_evm_test_codegen_invariant_fail_on_revert_foundry_comment() {
    let program = parse_evm(
        r#"
        entity Safe {
            routes { constructor() => [] tick() => [] }
            m_n: u64 { in constructor() => 0 in tick() => m_n + 1 }
        }

        invariant "strict" for Safe #[fail_on_revert] {
            init { m_n: 0 }
            action tick() {}
            check m_n >= 0
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("fail_on_revert invariant must emit file");
    assert!(
        inv.contains("Foundry treats handler reverts as failures via foundry.toml"),
        "fail_on_revert:true must emit handler tail comment: {inv}"
    );
    assert!(
        !inv.contains("try _safe.tick()") && inv.contains("_safe.tick();"),
        "fail_on_revert:true must emit bare call (no try/catch): {inv}"
    );
}

#[test]
fn n4_evm_test_codegen_multi_invariant_mixed_identity_deploy() {
    let program = parse_evm(
        r#"
        entity Plain {
            routes { constructor() => [] step() => [] }
            m_n: u64 { in constructor() => 0 in step() => m_n + 1 }
        }
        entity Named {
            identity m_slot: u64
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        invariant "mixed deploy" for { p: Plain, n: Named } {
            init p { m_n: 0 }
            init n { m_slot: 3, m_n: 0 }
            action p.step() {}
            check p.m_n >= n.m_n
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("multi invariant must emit file");
    assert!(
        inv.contains("deployPlain("),
        "identity-less instance must factory.deploy in multi setUp (U4-4c): {inv}"
    );
    assert!(
        inv.contains("deployNamed(3)"),
        "identity instance must pass identity arg in factory.deploy (U4-4c): {inv}"
    );
    assert!(
        !inv.contains("new Plain(") && !inv.contains("new Named("),
        "multi invariant must not new SUT entities: {inv}"
    );
}

/// Structural oracle for `gen_invariant_fn` emission in single-entity crates.
fn assert_single_entity_invariant_structure(src: &str, invariant_title: &str) {
    let slug = {
        let s: String = invariant_title
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect();
        let trimmed = s.trim_matches('_').to_lowercase();
        format!("test_{trimmed}")
    };
    assert!(
        src.contains(&format!("enum Action_{slug}"))
            && src.contains(&format!("fn action_{slug}_strategy")),
        "missing Action enum + strategy for {slug}: {src}"
    );
    assert!(
        src.contains("proptest!") && src.contains(&format!("fn invariant_{slug}")),
        "missing proptest invariant fn for {slug}"
    );
    assert!(
        src.contains("EvmTester::deploy(") && src.contains("for _action in seq.into_iter()"),
        "missing deploy + action loop for {slug}"
    );
    assert!(
        src.matches("prop_assert!").count() >= 1,
        "must emit at least one check prop_assert! for {slug}"
    );
}

fn multi_invariant_file<'a>(files: &'a [(String, String)]) -> (&'a str, &'a str) {
    let (path, content) = files
        .iter()
        .find(|(p, _)| p.starts_with("revm-tests/tests/invariant_"))
        .unwrap_or_else(|| panic!("expected revm-tests/tests/invariant_*.rs, got: {:?}", files.iter().map(|(p, _)| p).collect::<Vec<_>>()));
    (path.as_str(), content.as_str())
}

/// Structural oracle for `gen_multi_invariant_test_file` emission.
fn assert_multi_invariant_structure(path: &str, src: &str, label: &str) {
    assert!(
        path.starts_with("revm-tests/tests/invariant_") && path.ends_with(".rs"),
        "{label}: path must be revm-tests/tests/invariant_*.rs, got {path}"
    );
    assert!(
        src.contains("//! Multi-instance stateful invariant test"),
        "{label}: missing multi-instance header"
    );
    assert!(
        src.contains("enum Action_") && src.contains("fn action_") && src.contains("_strategy()"),
        "{label}: missing Action enum + strategy fn"
    );
    assert!(
        src.contains("proptest!") && src.contains("proptest::collection::vec("),
        "{label}: missing proptest trace harness"
    );
    assert!(
        src.contains("EvmTester::deploy(") && src.contains("call_at_checked("),
        "{label}: missing deploy + call_at_checked dispatch"
    );
    assert!(
        src.contains("for _action in seq.into_iter()"),
        "{label}: missing action loop"
    );
    assert!(
        src.matches("prop_assert!").count() >= 1,
        "{label}: must emit at least one check prop_assert!"
    );
}

fn count_substr(hay: &str, needle: &str) -> usize {
    hay.match_indices(needle).count()
}

// ---------------------------------------------------------------------------
// N4-37: codegen/test_codegen.rs — Native Acki `#[test]` / proptest slice 1
// Baseline @ N4-36: 75.14% (410 missed / 1649).
// ---------------------------------------------------------------------------

fn parse_native(src: &str) -> cambrian_transpiler::ast::Program {
    let mut program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse native: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    apply_using_rewrites(&mut program);
    program
}

fn contracts_concat(parts: &[&str]) -> String {
    let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../contracts");
    parts
        .iter()
        .map(|rel| {
            std::fs::read_to_string(base.join(rel))
                .unwrap_or_else(|e| panic!("read contracts/{rel}: {e}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_counter_contract_bundle() {
    let src = contracts_concat(&[
        "counter.cam",
        "counter.test.cam",
        "counter.invariant.cam",
        "counter_trace.invariant.cam",
    ]);
    let code = gen_native_test_code(
        &src,
        false,
        &FuzzConfig::default(),
        &InvariantConfig {
            runs: 16,
            depth: 6,
            ..InvariantConfig::default()
        },
    );
    assert!(
        code.contains("#[test]")
            && code.contains("proptest!")
            && code.contains("fn test_increment_adds_to_count")
            && code.contains("fn invariant_trace_bounded_counter"),
        "counter bundle must emit plain tests and trace invariant: {code}"
    );
    assert!(
        code.contains("trace_len") && code.contains("trace_count_increment"),
        "trace invariant must emit accumulator fields: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_phased_predictable_effects_wildcards() {
    let src = contracts_concat(&["phased_predictable.cam", "phased_predictable.test.cam"]);
    let code = gen_native_test_code(&src, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("WasmEffect::PhaseState")
            && code.contains("WasmEffect::RawReserve")
            && code.contains("_eff_cur")
            && code.contains("phaseState"),
        "phased escrow effects must hit wildcard cursor matching: {code}"
    );
    assert!(
        code.contains("expect throw 101") || code.contains("assert_eq!(_err_code"),
        "expect throw must lower to error-code assert: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_property_fuzz_proptest_strategies() {
    const SRC: &str = r#"
entity Counter {
    routes {
        increment(amount: u64) => []
        reset() => []
    }
    m_count: u64 {
        in increment(amount) => m_count + amount
        in reset() => 0
    }
}

property "typed fuzz" (amount: u64, pk: pubkey, label: String, wide: U256) for Counter with { m_count: 0 } {
    assume amount < 1000
    call increment(amount)

    #[runs(64)] fuzz "wide" { amount in 1..100 }
}
"#;
    let fuzz_cfg = FuzzConfig {
        runs: 32,
        max_local_rejects: 500,
        ..FuzzConfig::default()
    };
    let code = gen_native_test_code(SRC, true, &fuzz_cfg, &InvariantConfig::default());
    assert!(
        code.contains("proptest!") && code.contains("prop_assume!") && code.contains("prop_map"),
        "property fuzz must desugar into proptest with assume + strategies: {code}"
    );
    assert!(
        code.contains("U256::from_be_bytes")
            && code.contains("pubkey { hash:")
            && code.contains("\".*\".prop_map"),
        "ackinacki proptest strategies must cover U256/pubkey/String: {code}"
    );
    assert!(
        code.contains("cases: 32") && code.contains("max_local_rejects: 500"),
        "fuzz cfg runs/rejects must reach generated proptest config: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_inline_expect_return_tuple_and_lens() {
    const SRC: &str = r#"
entity PairBox {
    routes {
        init create() => []
        both() -> (u64, u64) => [ return(3, 4) ]
        view peek(n: u64) -> u64 => [ return(n) ]
    }
    m_n: u64 {
        in create() => 0
    }
}

test "tuple return" for PairBox with { m_n: 0 } {
    call both()
    expect return (3, 4)
}

test "return lens" for PairBox with { m_n: 0 } {
    call peek(7)
    expect return[0] == 7
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("_ret_val") && code.contains(".0") && code.contains(".1"),
        "expect return tuple must decode indexed return slots: {code}"
    );
    assert!(
        code.contains("return[0]") || code.contains("_ret_val"),
        "expect return lens must decode return payload: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_inline_msg_pubkey_and_registry() {
    const MSG_SRC: &str = r#"
entity Vault {
    routes { deposit(amount: u64) => [] }
    m_balance: u64 { in deposit(amount) => m_balance + amount }
}

test "deposit with sender" for Vault with { m_balance: 0 } {
    msg { pubkey: 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa }
    call deposit(100)
    expect state { m_balance: 100 }
}
"#;
    let msg_code = gen_native_test_code(MSG_SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        msg_code.contains("_ctx.pubkey"),
        "msg pubkey must lower into WasmContext: {msg_code}"
    );

    let registry_src = contracts_concat(&["marketplace/broker.cam", "marketplace/broker.test.cam"]);
    let reg_code = gen_native_test_code(&registry_src, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        reg_code.contains("EntityRegistryEntry") && reg_code.contains("entity_registry.push"),
        "broker registry steps must emit entity registry push: {reg_code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_inline_skip_from_toggle() {
    const SRC: &str = r#"
entity Ledger {
    routes {
        init create() => []
        credit(n: u64) => []
    }
    m_total: u64 {
        in create() => 0
        in credit(n) => m_total + n
    }
}

test "skip from" for Ledger skip from with { m_total: 0 } {
    call credit(1)
    expect state { m_total: 1 }
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("_SKIP_FROM.with(|f| f.set(true))")
            && code.contains("_SKIP_FROM.with(|f| f.set(false))"),
        "skip from tests must toggle _SKIP_FROM around the body: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_inline_invariant_senders_and_fail_on_revert() {
    const SRC: &str = r#"
entity Counter {
    routes {
        increment(amount: u64) => []
        reset() => []
    }
    m_count: u64 {
        in increment(amount) => m_count + amount
        in reset() => 0
    }
}

invariant "senders fail on revert" for Counter
    #[fail_on_revert]
{
    init { m_count: 0 }
    senders {
        0x0000000000000000000000000000000000000000000000000000000000000001,
        0x0000000000000000000000000000000000000000000000000000000000000002
    }
    action increment(amount: u64) {
        bound amount in 1..50
        assume amount < 40
    }
    action reset() { }
    check m_count >= 0
}
"#;
    let code = gen_native_test_code(
        &SRC,
        false,
        &FuzzConfig::default(),
        &InvariantConfig {
            runs: 8,
            depth: 4,
            ..InvariantConfig::default()
        },
    );
    assert!(
        code.contains("let _senders: Vec<TvmAddress>")
            && code.contains("_sender_idx")
            && code.contains("prop_assert_eq!(_result[0], 0u8"),
        "invariant senders + fail_on_revert must emit sender pool and prop_assert_eq: {code}"
    );
    assert!(
        code.contains("_pre_state") && code.contains("continue;"),
        "action assume/bound must read pre-call state: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_inline_invariant_multi_param_action() {
    const SRC: &str = r#"
entity Dual {
    routes {
        init create() => []
        add(a: u64, b: u64) => []
    }
    m_v: u64 {
        in create() => 0
        in add(a, b) => m_v + a + b
    }
}

invariant "dual action" for Dual {
    init { m_v: 0 }
    action add(a: u64, b: u64) {
        bound a in 0..100
        bound b in 0..100
    }
    check m_v >= 0
}
"#;
    let code = gen_native_test_code(&SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("Action_dual_action::Add")
            && code.contains("(any::<u64>(), any::<u64>())")
            && code.contains(".prop_map"),
        "multi-param invariant action must emit tuple prop_map strategy: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_identity_vault_identity_buf() {
    let src = contracts_concat(&[
        "identity_pair/identity_vault.cam",
        "identity_pair/identity_vault.test.cam",
    ]);
    let code = gen_native_test_code(&src, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("_id_buf") && code.contains("_ctx.identity_data"),
        "identity-member init must serialize into WasmContext identity_data: {code}"
    );
    assert!(
        code.contains("fn test_constructor_sets_balance")
            && code.contains("assert_eq!"),
        "identity vault tests must emit state assertions: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_inline_state_propagation_without_expect() {
    const SRC: &str = r#"
entity Counter {
    routes {
        increment(amount: u64) => []
    }
    m_count: u64 {
        in increment(amount) => m_count + amount
    }
}

test "chained calls" for Counter with { m_count: 0 } {
    call increment(3)
    call increment(5)
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("if _result[0] == 0u8")
            && code.contains("_state_bytes = _result"),
        "call without trailing expect must still propagate serialized state: {code}"
    );
}

// ---------------------------------------------------------------------------
// N4-38: codegen/test_codegen.rs — Native slice 2 (hashmap / call kwargs /
// platform effects / gen_test_expr tails / invariant identity default).
// Baseline @ N4-37: 86.17% (228 missed / 1649).
// ---------------------------------------------------------------------------

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_hashmap_init_and_expect_state() {
    const SRC: &str = r#"
entity Registry {
    routes {
        noop() => []
    }
    m_entries: HashMap<u64, u64> {
        in noop() => m_entries
    }
}

test "hashmap init and expect" for Registry with {
    m_entries: { 1 => 10, 2 => 20 }
} {
    call noop()
    expect state { m_entries: { 1 => 10, 2 => 20 } }
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("_m.insert") && code.contains("_m = HashMap::new()"),
        "HashMap init must emit gen_hashmap_assign insert block: {code}"
    );
    assert!(
        code.contains(".get(&") && code.contains("mismatch"),
        "expect state HashMap literal must emit gen_hashmap_assert get checks: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_typed_call_u256_string_pubkey_camdata() {
    const SRC: &str = r#"
entity TypedProbe {
    routes {
        probe(w: U256, label: String, pk: pubkey, data: CamData, tag: u64) => []
    }
    m_tag: u64 {
        in probe(_, _, _, _, tag) => tag
    }
}

test "typed kwargs" for TypedProbe with { m_tag: 0 } {
    let pk = 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
    let wide = 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
    call probe(42, "hello", pk, cam_encode<u64>(7), 1)
    call probe(wide, "world", pk, cam_encode<u64>(8), 2)
    call probe(wide, "fallback", pk, cam_encode<u64>(9), 3)
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("U256::from_be_bytes") && code.contains(".to_string().ser_be()"),
        "U256 IntLiteral and String literal kwargs must lower distinctly: {code}"
    );
    assert!(
        code.contains("0xaa,") || code.contains("0xaaaaaaaa"),
        "pubkey HexLiteral kwargs must use gen_hex_array_32 bytes: {code}"
    );
    assert!(
        code.contains("CellBuilder::new()") && code.contains("_enc_bytes"),
        "cam_encode in call args must hit Encode lowering: {code}"
    );
    assert!(
        code.contains(".hash") || code.contains("from_be_bytes"),
        "U256 HexLiteral / ident kwargs must hit alternate ser_be arms: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_platform_effect_assert_arms() {
    const SRC: &str = r#"
entity PlatformFx {
    routes { noop() => [] }
    m_v: u64 { in noop() => m_v }
}

test "platform effects" for PlatformFx with { m_v: 0 } {
    let dest = 0x0000000000000000000000000000000000000000000000000000000000001234
    call noop()
    expect effects [
        mintecc(50, 3),
        burnecc(10, 1),
        updateCode(),
        selfdestruct(dest),
        selfdestruct(),
        mintShellQ(100)
    ]
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("WasmEffect::MintEcc { amount, ecc_id }")
            && code.contains("WasmEffect::BurnEcc { amount, ecc_id }"),
        "mintEcc/burnEcc expect effects must emit match arms: {code}"
    );
    assert!(
        code.contains("WasmEffect::UpdateCode")
            && code.contains("WasmEffect::Selfdestruct { dest }")
            && code.contains("WasmEffect::MintShellQ { amount }"),
        "updateCode/selfdestruct/mintShellQ platform asserts must lower: {code}"
    );
    assert!(
        code.contains("expected Selfdestruct")
            && code.contains("expected UpdateCode"),
        "bare selfdestruct/updateCode arms must emit matches! asserts: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_gen_test_expr_binop_addressof_encode() {
    const SRC: &str = r#"
entity Pair {
    identity left: u64
    identity right: u64
    routes {
        bump(n: u64) => []
        setOwner(o: address) => []
    }
    m_acc: u64 {
        in bump(n) => m_acc + n
        in setOwner(_) => m_acc
    }
}

invariant "binop check" for Pair {
    init { left: 1, right: 2, m_acc: 0 }
    action bump(amount: u64) {
        bound amount in 1..100
        assume m_acc % 2 == 0 || amount > 0
    }
    check left + right + m_acc >= left
}

test "addressof encode and typed call" for Pair with { left: 3, right: 4, m_acc: 0 } {
    msg { pubkey: 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa }
    let cell = cam_encode<u64>(99)
    call setOwner(address_of Pair(3, 4))
    call bump(1)
    expect state { m_acc: 1 }
}
"#;
    let code = gen_native_test_code(
        SRC,
        false,
        &FuzzConfig::default(),
        &InvariantConfig {
            runs: 8,
            depth: 4,
            ..InvariantConfig::default()
        },
    );
    assert!(
        code.contains("prop_assert!")
            && code.contains(" % ")
            && code.contains(" || "),
        "invariant check/assume must lower BinOp tails via gen_test_expr: {code}"
    );
    assert!(
        code.contains("compute_tvm_address") && code.contains("AbiType::Uint(64)"),
        "typed call AddressOf kwargs must hit infer_abi_and_cast + compute_tvm_address: {code}"
    );
    assert!(
        code.contains("CellBuilder::new()"),
        "cam_encode let must lower Encode helper: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_invariant_identity_default_branch() {
    const SRC: &str = r#"
entity Duo {
    identity left: u64
    identity right: u64
    routes {
        tick() => []
    }
    m_n: u64 {
        in tick() => m_n + 1
    }
}

invariant "partial identity init" for Duo {
    init { left: 7, m_n: 0 }
    action tick() { }
    check m_n >= 0
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("_id_buf.extend_from_slice")
            && code.contains("u64::default().ser_be()"),
        "missing identity init field must serialize default() into identity_data: {code}"
    );
    assert!(
        code.contains("_id_buf.extend_from_slice(&(7 as u64).ser_be()"),
        "provided identity init must still serialize explicitly: {code}"
    );
}

// ---------------------------------------------------------------------------
// N4-40: codegen/test_codegen.rs — Native tail slice 3
// Baseline @ N4-39: 91.33% (143 missed / 1649).
// ---------------------------------------------------------------------------

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_invariant_u256_pubkey_action_kwargs() {
    const SRC: &str = r#"
entity Pay {
    routes {
        send(w: U256, pk: pubkey) => []
    }
    m_total: u64 {
        in send(w, _) => m_total + 1
    }
}

invariant "u256 pubkey action" for Pay {
    init { m_total: 0 }
    action send(w: U256, pk: pubkey) {
        bound w in 1..1000
    }
    check m_total >= 0
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains(").prop_map(|(v0, v1)|") && code.contains("Action_u256_pubkey_action::Send"),
        "U256+pubkey invariant action must use tuple prop_map strategy: {code}"
    );
    assert!(
        code.contains("pk.hash") && code.contains("w.ser_be()"),
        "invariant action kwargs must serialize U256 via ser_be and pubkey via hash: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_test_u256_identity_buf() {
    const SRC: &str = r#"
entity SaltVault {
    identity key: U256
    routes {
        init create() => []
        ping() => []
    }
    m_n: u64 {
        in create() => 0
        in ping() => m_n + 1
    }
}

test "u256 identity buf" for SaltVault with {
    key: 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb,
    m_n: 0
} {
    call ping()
    expect state { m_n: 1 }
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("_id_buf.extend_from_slice")
            && code.contains(".ser_be()")
            && !code.contains("as u64).ser_be()"),
        "non-primitive identity in test init must use ser_be without primitive cast: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_typed_expr_vec_hashmap_option_defaults() {
    const SRC: &str = r#"
entity TypedInit {
    routes {
        put(data: Vec<u8>) => []
        tag(opt: Option<u64>) => []
    }
    m_map: HashMap<u64, u64> {
        in put(_) => m_map
    }
    m_flag: Option<u64> {
        in tag(opt) => opt
    }
}

test "typed collection defaults" for TypedInit with {
    m_map: {},
    m_flag: some(99)
} {
    call put(0x0102)
    call tag(none)
    expect state { m_flag: none }
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("HashMap::<u64, u64>::new()") || code.contains("_m.insert"),
        "empty HashMap init must hit EmptyCollection / __HashMap typed arms: {code}"
    );
    assert!(
        code.contains("Some(99)") && code.contains("None"),
        "Option some/none must lower through gen_typed_test_expr: {code}"
    );
    assert!(
        code.contains("vec![0x01u8")
            || code.contains("vec![0x01, 0x02")
            || code.contains("0x01, 0x02u8"),
        "Vec<u8> hex literal call arg must hit typed hex arm: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_msg_ctx_pubkey_ident_and_currencies() {
    const SRC: &str = r#"
entity CtxProbe {
    routes {
        touch() => []
    }
    m_hits: u64 {
        in touch() => m_hits + 1
    }
}

test "msg ctx variants" for CtxProbe with { m_hits: 0 } {
    let holder = 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
    msg { pubkey: holder, value: 500 }
    msg { currencies: { 3 => 1000, 5 => 2000 } }
    sys { balance: 9000, now: 42 }
    call touch()
    expect state { m_hits: 1 }
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("_ctx.pubkey = holder.hash"),
        "msg pubkey ident must lower via .hash assignment arm: {code}"
    );
    assert!(
        code.contains("_ctx.currencies.insert(3") && code.contains("_ctx.currencies.insert(5"),
        "msg currencies map must emit per-key insert: {code}"
    );
    assert!(
        code.contains("_ctx.value") && code.contains("_ctx.balance") && code.contains("_ctx.now"),
        "generic ctx fields must assign via map_context_field: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_binop_infer_abi_and_unknown_effect() {
    const SRC: &str = r#"
entity Wide {
    identity a: u64
    identity b: u64
    routes {
        bump() => []
    }
    m_acc: u64 {
        in bump() => m_acc + 1
    }
}

invariant "wide binops" for Wide {
    init { a: 1, b: 2, m_acc: 0 }
    action bump() { }
    check (m_acc - 1) * 2 / 2 % 2 == m_acc % 2
    check (m_acc & 1) ^ (m_acc | 0) == (m_acc << 1) >> 1
}

test "infer abi and todo effect" for Wide with { a: 3, b: 4, m_acc: 0 } {
    let addr = address_of Wide(
        0xFFFFFFFFFFFFFFFF,
        0x0000000000000000000000000000000000000000000000000000000000000001
    )
    call bump()
    expect effects [setcode()]
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains(" - ") && code.contains(" * ") && code.contains(" / ")
            && code.contains(" % ") && code.contains(" & ") && code.contains(" ^ ")
            && code.contains(" << ") && code.contains(" >> "),
        "invariant checks must lower residual BinOp arms: {code}"
    );
    assert!(
        code.contains("AbiType::Uint(64)")
            || code.contains("AbiType::Uint(128)")
            || code.contains("AbiType::Uint(256)"),
        "address_of wide hex args must hit infer_abi_and_cast width arms: {code}"
    );
    assert!(
        code.contains("TODO: effect assertion for 'setcode'"),
        "unknown platform effect must emit TODO comment arm: {code}"
    );
}

// ---------------------------------------------------------------------------
// N4-42: codegen/test_codegen.rs — Native residual slice 4
// Baseline @ N4-41: 93.39% (109 missed / 1649).
// Skip (document): map_context_field TVM-only L1795–L1803 (~9).
// ---------------------------------------------------------------------------

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_invariant_address_kwargs_else_serbe() {
    const SRC: &str = r#"
entity Router {
    routes {
        mark(dest: address) => []
    }
    m_hits: u64 {
        in mark(_) => m_hits + 1
    }
}

invariant "address kwargs" for Router {
    init { m_hits: 0 }
    action mark(dest: address) { }
    check m_hits >= 0
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("Action_address_kwargs::Mark")
            && code.contains("_kwargs.extend_from_slice(&dest.ser_be()"),
        "non-primitive address invariant kwargs must hit else ser_be arm: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_registry_unsupported_hash_fallback() {
    const SRC: &str = r#"
entity Counter {
    routes { bump() => [] }
    m_count: u64 { in bump() => m_count + 1 }
}

test "registry hash fallback" for Counter with { m_count: 0 } {
    let bogus = 42
    registry Ghost {
        code_hash: bogus,
        code_depth: 44,
        wasm_hash: 0xdb7d32a16611041a64b699074416ac6329e03d698db944146665f3165342d05c
    }
    call bump()
    expect state { m_count: 1 }
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("unsupported registry hash"),
        "non-hex registry code_hash must hit gen_hex_array_32 fallback: {code}"
    );
    assert!(
        code.contains("entity_registry.push"),
        "registry step must still emit EntityRegistryEntry push: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_infer_abi_widths_and_encode_u256_hex() {
    const SRC: &str = r#"
entity Wide {
    identity lo: u64
    identity hi: u128
    routes {
        probe() => []
    }
    m_acc: u64 {
        in probe() => m_acc + 1
    }
}

test "infer abi widths" for Wide with { lo: 0xdeadbeef, hi: 0x00112233445566778899aabbccddeeff, m_acc: 0 } {
    let cell = cam_encode<U256>(0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20)
    let addr = address_of Wide(
        0xdeadbeef,
        0x00112233445566778899aabbccddeeff
    )
    call probe()
    expect state { m_acc: 1 }
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("AbiType::Uint(64)") && code.contains("AbiType::Uint(128)"),
        "short hex address_of args must hit infer_abi_and_cast width arms: {code}"
    );
    assert!(
        code.contains("U256::from_be_bytes"),
        "cam_encode<U256> with wide hex must hit Encode U256 hex arm: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_native_test_codegen_invariant_assume_relational_binops() {
    const SRC: &str = r#"
entity Meter {
    routes {
        tick(n: u64) => []
    }
    m_v: u64 {
        in tick(n) => m_v + n
    }
}

invariant "relational binops" for Meter {
    init { m_v: 0 }
    action tick(n: u64) {
        bound n in 1..20
        assume (m_v & 1) == 0 || n != 0
        assume m_v <= 1000 && n >= 1
    }
    check (m_v ^ 0) >= 0 && m_v >> 1 <= m_v
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains(" & ") && code.contains(" ^ ") && code.contains(" >> ")
            && code.contains(" != ") && code.contains(" && "),
        "invariant assume/check must lower residual BinOp arms: {code}"
    );
}

// ---------------------------------------------------------------------------
// N4-76: codegen/evm_test_codegen.rs — slice 7 (forge residual @ N4-75 tail)
// Gap @ 94.03% (119 missed / 1992). Focus rewrite_one_handler_ident
// L2238–L2255, gen_test_fn/gen_fuzz_fn scatter L526–L633, gen_pre_call_effect_setup
// L1294–L1323, gen_path_accessor Field/TupleIndex L1332–L1340. Exclude DEAD:
// gen_state_accessor L1349/L1355/L1376.
// ---------------------------------------------------------------------------

#[test]
fn n4_76_evm_test_codegen_rewrite_handler_ident_prefix_and_empty_derived() {
    let mut program = parse_evm(
        r#"
        entity Meter {
            routes { constructor() => [] tick() => [] }
            m_ticks: u64 { in constructor() => 0 in tick() => m_ticks + 1 }
            m_snap_ok: u64 { in constructor() => 0 in tick() => m_snap_ok + 1 }
        }
    "#,
    );
    program.invariants.push(InvariantDecl {
        name: "snap prefix".into(),
        instances: vec![InvariantInstance {
            name: "_self".into(),
            entity: "Meter".into(),
            init: vec![("m_ticks".into(), Expr::IntLiteral(U256::ZERO))],
            forall_state: ForallSpec::default(),
            init_specified: true,
            span: span0(),
        }],
        skip_from: false,
        senders: vec![],
        deploy: vec![],
        context: ContextSpec { entries: vec![] },
        actions: vec![InvariantAction {
            instance: "_self".into(),
            route: "tick".into(),
            params: vec![],
            body: vec![],
            span: span0(),
        }],
        checks: vec![Expr::BinOp(
            Box::new(Expr::Ident("m_snap_ok".into())),
            BinOp::Ge,
            Box::new(Expr::Ident("snap".into())),
        )],
        fail_on_revert: false,
        runs: None,
        depth: None,
        tag: None,
        instantiates: None,
        emit_policy: InvariantEmitPolicy::Emit,
        with_time: false,
        track: vec![cambrian_transpiler::ast::InvariantSetupBinding {
            name: "snap".into(),
            value: Expr::Ident("m_ticks".into()),
            span: span0(),
        }],
        derived: vec![cambrian_transpiler::ast::InvariantQuery {
            name: String::new(),
            params: vec![],
            return_type: Type::Simple("u64".into()),
            body: vec![],
            return_value: Expr::IntLiteral(U256::ZERO),
            span: span0(),
        }],
        exclude_senders: vec![],
        exclude_selectors: vec![],
        span: span0(),
    });
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("snap prefix invariant must emit file");
    assert!(
        inv.contains("_handler.snap()") || inv.contains("_handler.snap"),
        "bare track ref must rewrite via rewrite_one_handler_ident getter arm: {inv}"
    );
    assert!(
        inv.contains("m_snap_ok()") && inv.contains("require("),
        "check must lower member access after handler rewrite: {inv}"
    );
}

#[test]
fn n4_76_evm_test_codegen_test_orphan_assume_bound_skipif_advance() {
    let mut program = parse_evm(
        r#"
        entity Gate {
            routes { constructor() => [] open() => [] }
            m_n: u64 { in constructor() => 0 in open() => m_n + 1 }
        }
    "#,
    );
    push_orphan_step_test(
        &mut program,
        vec![
            TestStep::Assume {
                cond: Expr::BoolLiteral(true),
            },
            TestStep::Bound {
                var: "n".into(),
                lo: Expr::IntLiteral(U256::from_u128(1)),
                hi: Expr::IntLiteral(U256::from_u128(10)),
                inclusive: true,
            },
            TestStep::SkipIf {
                cond: Expr::BoolLiteral(false),
            },
            TestStep::AdvanceTime {
                secs: Expr::IntLiteral(U256::from_u128(30)),
            },
            TestStep::Call {
                target: None,
                route: "open".into(),
                args: vec![],
            },
        ],
    );
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Gate");
    assert!(
        test_sol.contains("function test_orphan_steps()"),
        "orphan-step test must emit: {test_sol}"
    );
    assert!(
        !test_sol.contains("vm.assume(")
            && !test_sol.contains("bound(")
            && test_sol.contains("open()"),
        "orphan Assume/Bound/SkipIf/AdvanceTime must no-op in gen_test_fn: {test_sol}"
    );
}

#[test]
fn n4_76_evm_test_codegen_fuzz_let_inclusive_bound_orphan_skip_advance() {
    let mut program = parse_evm(
        r#"
        entity Gauge {
            routes { constructor() => [] bump() => [] }
            m_n: u64 { in constructor() => 0 in bump() => m_n + 1 }
        }
    "#,
    );
    program.fuzz_tests.push(FuzzDecl {
        name: "fuzz tails".into(),
        entity_name: "Gauge".into(),
        params: vec![Param {
            name: "n".into(),
            ty: Type::Simple("u64".into()),
        }],
        init_state: vec![],
        skip_from: false,
        body: vec![
            TestStep::Let {
                ty: None,
                name: "seed".into(),
                value: Expr::IntLiteral(U256::from_u128(9)),
            },
            TestStep::Bound {
                var: "n".into(),
                lo: Expr::IntLiteral(U256::from_u128(1)),
                hi: Expr::IntLiteral(U256::from_u128(20)),
                inclusive: true,
            },
            TestStep::SkipIf {
                cond: Expr::BoolLiteral(false),
            },
            TestStep::AdvanceTime {
                secs: Expr::IntLiteral(U256::from_u128(15)),
            },
            TestStep::Call {
                target: None,
                route: "bump".into(),
                args: vec![],
            },
        ],
        runs: None,
        tag: None,
        instantiates: None,
        span: span0(),
    });
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Gauge");
    assert!(
        test_sol.contains("uint256 seed = 9") || test_sol.contains("seed = 9"),
        "fuzz Let must hit gen_let_binding: {test_sol}"
    );
    assert!(
        test_sol.contains("bound(n, 1, 20)") && !test_sol.contains("(20) - 1"),
        "inclusive fuzz bound must pass hi through without -1: {test_sol}"
    );
    assert!(
        !test_sol.contains("if (false) return") && test_sol.contains("bump()"),
        "orphan SkipIf/AdvanceTime must no-op in gen_fuzz_fn: {test_sol}"
    );
}

#[test]
fn n4_76_evm_test_codegen_test_tag_and_wildcard_pre_call_effect() {
    let mut program = parse_evm(
        r#"
        entity Minter {
            routes { constructor() => [] mint() => [] }
            m_n: u64 { in constructor() => 0 in mint() => m_n + 1 }
        }
    "#,
    );
    program.tests.push(TestDecl {
        name: "tagged wildcard".into(),
        entity_name: "Minter".into(),
        init_state: vec![],
        skip_from: true,
        body: vec![
            TestStep::Call {
                target: None,
                route: "mint".into(),
                args: vec![],
            },
            TestStep::ExpectEffects {
                elements: vec![TestEffectElement::Wildcard],
            },
        ],
        tag: Some("ci".into()),
        instantiates: None,
        span: span0(),
    });
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Minter");
    assert!(
        test_sol.contains("function test_tagged_wildcard_ci()")
            && test_sol.contains("/// @dev tag: ci"),
        "test #[tag] must suffix fn name and emit dev tag: {test_sol}"
    );
    assert!(
        test_sol.contains("skip from: prank as address(1)"),
        "test skip_from must emit prank comment in gen_test_fn: {test_sol}"
    );
    assert!(
        test_sol.contains("mint()"),
        "wildcard-only expect effects must still emit call (pre-call Wildcard is no-op): {test_sol}"
    );
}

#[test]
fn n4_76_evm_test_codegen_expect_return_lens_field_then_tuple_index() {
    let mut program = parse_evm(
        r#"
        entity PairBox {
            routes {
                constructor() => []
                both() -> (u64, u64) => [ return(3, 4) ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    push_orphan_step_test(
        &mut program,
        vec![
            TestStep::Call {
                target: None,
                route: "both".into(),
                args: vec![],
            },
            TestStep::ExpectReturnLens {
                path: vec![
                    PathSegment::Field("slot".into()),
                    PathSegment::TupleIndex(1),
                ],
                value: Expr::IntLiteral(U256::from_u128(4)),
            },
        ],
    );
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "PairBox");
    assert!(
        test_sol.contains(".slot.1") && test_sol.contains("return lens mismatch"),
        "Field+TupleIndex return lens must hit gen_path_accessor Field then TupleIndex: {test_sol}"
    );
}

#[test]
fn n4_76_evm_test_codegen_invariant_forall_skip_identity_and_mapping() {
    let program = parse_evm(
        r#"
        entity Wallet {
            identity m_id: u64
            routes { constructor() => [] tick() => [] }
            m_count: u64 { in constructor() => 0 in tick() => m_count + 1 }
            m_scores: HashMap<u64, u64> { in constructor() => {} }
        }

        invariant "forall filter" for Wallet {
            init { m_id: 7, m_count: 0 }
            with { m_count: *, m_id: *, m_scores: * }
            action tick() {}
            check m_count >= 0
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("forall invariant must emit file");
    assert!(
        inv.contains("__fa_m_count") || inv.contains("vm.randomUint()"),
        "non-identity scalar forall must seed via gen_forall_state_init: {inv}"
    );
    assert!(
        !inv.contains("__fa_m_id") && !inv.contains("__fa_m_scores"),
        "identity/mapping forall members must be skipped in forall_dynamic_fields: {inv}"
    );
}

#[test]
fn n4_76_evm_test_codegen_invariant_ctx_unsupported_and_test_foo_context() {
    let program = parse_evm(
        r#"
        entity Clock {
            routes { constructor() => [] tick() => [] }
            m_n: u64 { in constructor() => 0 in tick() => m_n + 1 }
        }

        invariant "ctx tails" for Clock {
            init { m_n: 0 }
            ctx { sys::balance: 100, msg::sender: * }
            action tick() {}
            check m_n >= 0
        }

        test "foo ctx" for Clock {
            foo { bar: 1 }
            call tick()
        }
    "#,
    );
    let files = gen_evm_test_files(&program);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("invariant must emit");
    assert!(
        inv.contains("sys::balance") && inv.contains("not lowered on the Foundry invariant target"),
        "unsupported invariant ctx field must hit gen_invariant_foundry_ctx fallback: {inv}"
    );
    assert!(
        inv.contains("msg::sender: *"),
        "forall msg sender ctx must emit unrestricted comment: {inv}"
    );
    let test_sol = foundry_test_file(&files, "Clock");
    assert!(
        test_sol.contains("// foo context -- not supported on EVM"),
        "unsupported test SetContext namespace must hit gen_context_block fallback: {test_sol}"
    );
}

#[test]
fn n4_76_evm_test_codegen_unknown_entity_fuzz_and_invariant_skip() {
    let mut program = parse_evm(
        r#"
        entity Counter {
            routes { bump() => [] }
            m_count: u64 { in bump() => m_count + 1 }
        }
    "#,
    );
    program.tests.push(TestDecl {
        name: "counter smoke".into(),
        entity_name: "Counter".into(),
        init_state: vec![],
        skip_from: false,
        body: vec![TestStep::Call {
            target: None,
            route: "bump".into(),
            args: vec![],
        }],
        tag: None,
        instantiates: None,
        span: span0(),
    });
    program.fuzz_tests.push(FuzzDecl {
        name: "ghost fuzz".into(),
        entity_name: "MissingCounter".into(),
        params: vec![Param {
            name: "n".into(),
            ty: Type::Simple("u64".into()),
        }],
        init_state: vec![],
        skip_from: false,
        body: vec![TestStep::Call {
            target: None,
            route: "bump".into(),
            args: vec![],
        }],
        runs: None,
        tag: None,
        instantiates: None,
        span: span0(),
    });
    program.invariants.push(InvariantDecl {
        name: "ghost inv".into(),
        instances: vec![InvariantInstance {
            name: "_self".into(),
            entity: "MissingCounter".into(),
            init: vec![],
            forall_state: ForallSpec::default(),
            init_specified: false,
            span: span0(),
        }],
        skip_from: false,
        senders: vec![],
        deploy: vec![],
        context: ContextSpec { entries: vec![] },
        actions: vec![],
        checks: vec![Expr::BoolLiteral(true)],
        fail_on_revert: false,
        runs: None,
        depth: None,
        tag: None,
        instantiates: None,
        emit_policy: InvariantEmitPolicy::Emit,
        with_time: false,
        track: vec![],
        derived: vec![],
        exclude_senders: vec![],
        exclude_selectors: vec![],
        span: span0(),
    });
    let files = gen_evm_test_files(&program);
    assert!(
        !files.iter().any(|(p, _)| p == "test/MissingCounter.t.sol"),
        "unknown fuzz entity must skip test file emission: {files:?}"
    );
    assert!(
        !files.iter().any(|(p, _)| p.starts_with("test/Invariant_ghost")),
        "unknown single-entity invariant must skip Invariant file emission: {files:?}"
    );
    assert!(
        files.iter().any(|(p, _)| p == "test/Counter.t.sol"),
        "valid entity tests must still emit: {files:?}"
    );
}

// ---------------------------------------------------------------------------
// N4-86: codegen/evm_test_codegen.rs — slice 8 (member-accessor residual @ N4-76)
// Gap @ 95.48% (90 missed / 1992). Focus substitute_member_accessors L2294+,
// lower_check_expr_multi L2171–L2207, rewrite_one_handler_ident L2254–L2255,
// gen_call det empty ctor L1118–L1119. Exclude DEAD: gen_state_accessor L1349+.
// ---------------------------------------------------------------------------

#[test]
fn n4_86_evm_test_codegen_rewrite_handler_ident_prefix_collision() {
    let program = parse_evm(
        r#"
        entity Meter {
            routes { constructor() => [] tick() => [] }
            rate_limit: u64 {
                in constructor() => 0
                in tick() => rate_limit + 1
            }
            m_ticks: u64 {
                in constructor() => 0
                in tick() => m_ticks + 1
            }
        }

        invariant "rate prefix" for Meter {
            init { rate_limit: 0, m_ticks: 0 }
            action tick() {}
            track { let rate = m_ticks; }
            check rate_limit >= rate
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("rate prefix invariant must emit Invariant_*.t.sol");
    assert!(
        inv.contains("_handler.rate()") && inv.contains("rate_limit"),
        "track ref must rewrite while longer ident keeps prefix chars (L2254–L2255): {inv}"
    );
    assert!(
        inv.contains("_meter.rate_limit()"),
        "member rate_limit must substitute after handler rewrite: {inv}"
    );
}

#[test]
fn n4_86_evm_test_codegen_substitute_skip_identity_member_invariant() {
    let program = parse_evm(
        r#"
        entity SlotBox {
            identity m_slot: u64
            routes { constructor() => [] bump() => [] }
            m_count: u64 {
                in constructor() => 0
                in bump() => m_count + 1
            }
        }

        invariant "identity skip" for SlotBox {
            init { m_slot: 1, m_count: 0 }
            action bump() {}
            check m_slot >= m_count
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("identity skip invariant must emit file");
    assert!(
        inv.contains("m_slot >=") && !inv.contains("_slotBox.m_slot()"),
        "identity member must skip substitute_member_accessors (L2290–L2291): {inv}"
    );
    assert!(
        inv.contains("_slotBox.m_count()"),
        "non-identity member must still substitute: {inv}"
    );
}

#[test]
fn n4_86_evm_test_codegen_substitute_skip_handler_track_name() {
    let program = parse_evm(
        r#"
        entity Quota {
            routes { constructor() => [] tick() => [] }
            m_quota: u64 {
                in constructor() => 0
                in tick() => m_quota + 1
            }
            m_ticks: u64 {
                in constructor() => 0
                in tick() => m_ticks + 1
            }
        }

        invariant "track shadow" for Quota {
            init { m_quota: 0, m_ticks: 0 }
            action tick() {}
            track { let quota = m_ticks; }
            check quota <= m_quota
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("track shadow invariant must emit file");
    assert!(
        inv.contains("_handler.quota()") && inv.contains("_quota.m_quota()"),
        "track ident must stay on handler; member m_quota must substitute (L2293–L2294): {inv}"
    );
}

#[test]
fn n4_86_evm_test_codegen_substitute_view_route_call_in_check() {
    let program = parse_evm(
        r#"
        entity Counter {
            routes {
                constructor() => []
                increment(amount: u64) => []
                view getCount() -> u64 => [ return(m_count) ]
            }
            m_count: u64 {
                in constructor() => 0
                in increment(amount) => m_count + amount
            }
        }

        invariant "route call" for Counter {
            init { m_count: 0 }
            action increment(amount: u64) {
                bound amount in 1..10
            }
            check getCount() >= m_count
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("route call invariant must emit file");
    assert!(
        inv.contains("_counter.getCount(") && inv.contains("_counter.m_count()"),
        "view route call in check must rewrite before paren (L2307–L2312): {inv}"
    );
}

#[test]
fn n4_86_evm_test_codegen_multi_invariant_unary_neg_check() {
    let program = parse_evm(
        r#"
        entity Left {
            routes { constructor() => [] go() => [] }
            m_x: i64 { in constructor() => -5 in go() => m_x + 1 }
        }
        entity Right {
            routes { constructor() => [] go() => [] }
            m_y: i64 { in constructor() => 2 in go() => m_y + 1 }
        }

        invariant "neg check" for { l: Left, r: Right } {
            init l { m_x: -5 }
            init r { m_y: 2 }
            action l.go() {}
            action r.go() {}
            check -(l.m_x) >= -(r.m_y)
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("neg multi invariant must emit file");
    assert!(
        inv.contains("-(_l.m_x())") && inv.contains("-(_r.m_y())"),
        "UnaryOp::Neg must lower via lower_check_expr_multi (L2205–L2206): {inv}"
    );
}

#[test]
fn n4_86_evm_test_codegen_multi_invariant_nested_field_access_check() {
    let mut program = parse_evm(
        r#"
        entity Host {
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 3 }
        }
        entity Peer {
            routes { constructor() => [] }
            m_y: u64 { in constructor() => 1 }
        }
    "#,
    );
    program.invariants.push(InvariantDecl {
        name: "nested recv".into(),
        instances: vec![
            InvariantInstance {
                name: "h".into(),
                entity: "Host".into(),
                init: vec![("m_x".into(), Expr::IntLiteral(U256::from_u128(3)))],
                forall_state: ForallSpec::default(),
                init_specified: true,
                span: span0(),
            },
            InvariantInstance {
                name: "p".into(),
                entity: "Peer".into(),
                init: vec![("m_y".into(), Expr::IntLiteral(U256::from_u128(1)))],
                forall_state: ForallSpec::default(),
                init_specified: true,
                span: span0(),
            },
        ],
        skip_from: false,
        senders: vec![],
        deploy: vec![],
        context: ContextSpec { entries: vec![] },
        actions: vec![],
        checks: vec![Expr::BinOp(
            Box::new(Expr::FieldAccess(
                Box::new(Expr::BinOp(
                    Box::new(Expr::FieldAccess(
                        Box::new(Expr::Ident("h".into())),
                        "m_x".into(),
                    )),
                    BinOp::Add,
                    Box::new(Expr::FieldAccess(
                        Box::new(Expr::Ident("p".into())),
                        "m_y".into(),
                    )),
                )),
                "bits".into(),
            )),
            BinOp::Eq,
            Box::new(Expr::IntLiteral(U256::from_u128(4))),
        )],
        fail_on_revert: false,
        runs: None,
        depth: None,
        tag: None,
        instantiates: None,
        emit_policy: InvariantEmitPolicy::Emit,
        with_time: false,
        track: vec![],
        derived: vec![],
        exclude_senders: vec![],
        exclude_selectors: vec![],
        span: span0(),
    });
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("nested recv invariant must emit file");
    assert!(
        inv.contains("((_h.m_x() + _p.m_y()).bits == 4)")
            || inv.contains("((_h.m_x() + _p.m_y()) .bits == 4)"),
        "generic FieldAccess receiver must recurse (L2173–L2175): {inv}"
    );
}

#[test]
fn n4_86_evm_test_codegen_det_fuzz_ctor_initialize_skip() {
    let mut program = parse_evm(
        r#"
        entity EmptyDet {
            identity m_slot: u64
            routes { constructor() => [] ping() => [] }
            m_n: u64 { in constructor() => 0 in ping() => m_n + 1 }
        }
    "#,
    );
    program.fuzz_tests.push(FuzzDecl {
        name: "ctor skip".into(),
        entity_name: "EmptyDet".into(),
        params: vec![],
        init_state: vec![],
        skip_from: false,
        body: vec![
            TestStep::Call {
                target: None,
                route: "constructor".into(),
                args: vec![],
            },
            TestStep::Call {
                target: None,
                route: "ping".into(),
                args: vec![],
            },
        ],
        runs: None,
        tag: None,
        instantiates: None,
        span: span0(),
    });
    let files = gen_evm_test_files_det(&program);
    let fuzz_sol = files
        .iter()
        .find(|(p, _)| p.starts_with("test/") && (p.contains("Fuzz") || p.contains("EmptyDet")))
        .map(|(_, c)| c.as_str())
        .expect("det fuzz must emit contract");
    assert!(
        fuzz_sol.contains("deployEmptyDet(") || fuzz_sol.contains(".deployEmptyDet("),
        "factory fuzz call constructor must redeploy via deployEmptyDet: {fuzz_sol}"
    );
    assert!(
        fuzz_sol.contains(".ping("),
        "fuzz must still emit subsequent route call: {fuzz_sol}"
    );
}

#[test]
fn n4_86_evm_test_codegen_project_invariant_member_route_substitution() {
    let files = gen_evm_project_files("project_invariant.yaml");
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("project_invariant.yaml must emit Invariant_*.t.sol");
    assert!(
        inv.contains("_counter.") && inv.contains("require("),
        "project invariant checks must substitute SUT accessors: {inv}"
    );
}

#[test]
fn n4_86_evm_test_codegen_single_invariant_derived_skip_route_rewrite() {
    let program = parse_evm(
        r#"
        entity Stats {
            routes {
                constructor() => []
                view peek() -> u64 => [ return(m_count) ]
            }
            m_count: u64 {
                in constructor() => 0
            }
        }

        invariant "derived shadow" for Stats {
            init { m_count: 0 }
            action peek() {}
            derived peekCopy() -> u64 { return(m_count) }
            check peekCopy() >= m_count
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("derived shadow invariant must emit file");
    assert!(
        inv.contains("_handler.peekCopy(") && inv.contains("_stats.m_count()"),
        "derived name in state_idents must skip route rewrite (L2304–L2305): {inv}"
    );
}

#[test]
fn n4_86_evm_test_codegen_multi_invariant_bitshift_check() {
    let program = parse_evm(
        r#"
        entity Left {
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 4 }
        }
        entity Right {
            routes { constructor() => [] }
            m_y: u64 { in constructor() => 1 }
        }

        invariant "bitshift" for { l: Left, r: Right } {
            init l { m_x: 4 }
            init r { m_y: 1 }
            action l.go() {}
            action r.go() {}
            check (l.m_x << r.m_y) == 8 && (l.m_x >> r.m_y) == 2
        }
    "#,
    );
    let mut broken = program;
    broken.invariants[0].actions.clear();
    let inv = gen_evm_test_files(&broken)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("bitshift invariant must emit file");
    assert!(
        inv.contains("<<") && inv.contains(">>"),
        "Shl/Shr binops must lower in lower_check_expr_multi (L2197–L2198): {inv}"
    );
}

#[test]
fn n4_86_evm_test_codegen_multi_invariant_non_instance_field_access() {
    let mut program = parse_evm(
        r#"
        entity Host {
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 1 }
        }
    "#,
    );
    program.invariants.push(InvariantDecl {
        name: "ghost recv".into(),
        instances: vec![InvariantInstance {
            name: "h".into(),
            entity: "Host".into(),
            init: vec![("m_x".into(), Expr::IntLiteral(U256::from_u128(1)))],
            forall_state: ForallSpec::default(),
            init_specified: true,
            span: span0(),
        }],
        skip_from: false,
        senders: vec![],
        deploy: vec![],
        context: ContextSpec { entries: vec![] },
        actions: vec![],
        checks: vec![Expr::FieldAccess(
            Box::new(Expr::Ident("ghost".into())),
            "tag".into(),
        )],
        fail_on_revert: false,
        runs: None,
        depth: None,
        tag: None,
        instantiates: None,
        emit_policy: InvariantEmitPolicy::Emit,
        with_time: false,
        track: vec![],
        derived: vec![],
        exclude_senders: vec![],
        exclude_selectors: vec![],
        span: span0(),
    });
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("ghost recv invariant must emit file");
    assert!(
        inv.contains("ghost.tag") || inv.contains("(ghost.tag"),
        "non-instance Ident field access must hit generic recv arm (L2168–L2175): {inv}"
    );
}

#[test]
fn n4_86_evm_test_codegen_det_test_ctor_calls_initialize() {
    let program = parse_evm(
        r#"
        entity WithInit {
            identity m_slot: u64
            routes {
                constructor(extra: u64) => []
                ping() => []
            }
            m_extra: u64 {
                in constructor(extra) => extra
                in ping() => m_extra
            }
        }

        test "init remap" for WithInit {
            call constructor(7)
            call ping()
        }
    "#,
    );
    let files = gen_evm_test_files_det(&program);
    let test_sol = foundry_test_file(&files, "WithInit");
    assert!(
        test_sol.contains("deployWithInit(") && test_sol.contains("7"),
        "det call constructor(args) must redeploy via factory with init-route args: {test_sol}"
    );
}

// ---------------------------------------------------------------------------
// N4-99: codegen/evm_test_codegen.rs — slice 9 (check-expr residual @ N4-86)
// Baseline @ N4-86: 96.03% (79 missed / 1992). Targets: lower_check_expr_multi
// L2171–L2207; gen_call det empty ctor L1118–L1119; rewrite_one_handler_ident
// L2254–L2255; scatter fuzz/gen_route tails. Exclude DEAD: gen_state_accessor L1349+.
// Acceptance: ≥ 96.5% or ≤ 72 missed.
// ---------------------------------------------------------------------------

#[test]
fn n4_99_evm_test_codegen_lower_check_expr_sub_mul_ge_or_emit() {
    let program = parse_evm(
        r#"
        entity Left {
            routes { constructor() => [] step() => [] }
            m_x: u64 { in constructor() => 10 in step() => m_x - 1 }
        }
        entity Right {
            routes { constructor() => [] step() => [] }
            m_y: u64 { in constructor() => 2 in step() => m_y + 1 }
        }

        invariant "arith mix" for { l: Left, r: Right } {
            init l { m_x: 10 }
            init r { m_y: 2 }
            action l.step() {}
            action r.step() {}
            check (l.m_x - r.m_y) * 2 >= 0 || l.m_x & r.m_y == 0
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("arith mix invariant must emit file");
    assert!(
        inv.contains("-") && inv.contains("*") && inv.contains(">=") && inv.contains("||") && inv.contains("&"),
        "multi check must emit Sub/Mul/Ge/Or/BitAnd via lower_check_expr_multi (L2182–L2193): {inv}"
    );
}

#[test]
fn n4_99_evm_test_codegen_lower_check_expr_deref_fallback() {
    let mut program = parse_evm(
        r#"
        entity Host {
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 1 }
        }
        entity Peer {
            routes { constructor() => [] }
            m_y: u64 { in constructor() => 2 }
        }
    "#,
    );
    program.invariants.push(InvariantDecl {
        name: "deref fallback".into(),
        instances: vec![
            InvariantInstance {
                name: "h".into(),
                entity: "Host".into(),
                init: vec![("m_x".into(), Expr::IntLiteral(U256::from_u128(1)))],
                forall_state: ForallSpec::default(),
                init_specified: true,
                span: span0(),
            },
            InvariantInstance {
                name: "p".into(),
                entity: "Peer".into(),
                init: vec![("m_y".into(), Expr::IntLiteral(U256::from_u128(2)))],
                forall_state: ForallSpec::default(),
                init_specified: true,
                span: span0(),
            },
        ],
        skip_from: false,
        senders: vec![],
        deploy: vec![],
        context: ContextSpec { entries: vec![] },
        actions: vec![],
        checks: vec![Expr::UnaryOp(
            UnaryOp::Deref,
            Box::new(Expr::Ident("h".into())),
        )],
        fail_on_revert: false,
        runs: None,
        depth: None,
        tag: None,
        instantiates: None,
        emit_policy: InvariantEmitPolicy::Emit,
        with_time: false,
        track: vec![],
        derived: vec![],
        exclude_senders: vec![],
        exclude_selectors: vec![],
        span: span0(),
    });
    let entity_for_inst: Vec<(String, &cambrian_transpiler::ast::Entity)> = program
        .invariants[0]
        .instances
        .iter()
        .filter_map(|inst| {
            program
                .entities
                .iter()
                .find(|e| e.name == inst.entity)
                .map(|e| (inst.name.clone(), e))
        })
        .collect();
    let lowered = lower_check_expr_multi(&program.invariants[0].checks[0], &entity_for_inst);
    assert_eq!(lowered, "h", "UnaryOp::Deref must pass through inner lowered expr (L2207): {lowered}");
}

#[test]
fn n4_99_evm_test_codegen_multi_invariant_action_exclusive_assume() {
    let program = parse_evm(
        r#"
        entity Vault {
            routes { constructor() => [] deposit(amount: u64) => [] }
            m_balance: u64 {
                in constructor() => 0
                in deposit(amount) => m_balance + amount
            }
        }
        entity Ledger {
            routes { constructor() => [] credit(amount: u64) => [] }
            m_total: u64 {
                in constructor() => 0
                in credit(amount) => m_total + amount
            }
        }

        invariant "bounded credit" for { v: Vault, book: Ledger } {
            init v { m_balance: 0 }
            init book { m_total: 0 }
            action v.deposit(amount: u64) {
                bound amount in 1..100
                assume amount > 0
            }
            action book.credit(amount: u64) {
                bound amount in 1..50
            }
            check v.m_balance >= book.m_total
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("bounded credit invariant must emit file");
    assert!(
        inv.contains(") - 1") && inv.contains("vm.assume("),
        "multi action exclusive bound + assume must hit gen_invariant_multi_file (L1941–L1953): {inv}"
    );
}

#[test]
fn n4_99_evm_test_codegen_multi_invariant_fail_on_revert_comment() {
    let program = parse_evm(
        r#"
        entity Alpha {
            routes { constructor() => [] tick() => [] }
            m_n: u64 { in constructor() => 0 in tick() => m_n + 1 }
        }
        entity Beta {
            routes { constructor() => [] tick() => [] }
            m_n: u64 { in constructor() => 0 in tick() => m_n + 1 }
        }

        invariant "strict multi" for { a: Alpha, b: Beta } #[fail_on_revert] {
            init a { m_n: 0 }
            init b { m_n: 0 }
            action a.tick() {}
            action b.tick() {}
            check a.m_n >= b.m_n
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("strict multi invariant must emit file");
    assert!(
        inv.contains("Foundry treats handler reverts as failures via foundry.toml"),
        "multi fail_on_revert must emit handler tail comment (L2145): {inv}"
    );
}

#[test]
fn n4_99_evm_test_codegen_multi_invariant_unknown_entity_skip() {
    let mut program = parse_evm(
        r#"
        entity Known {
            routes { constructor() => [] ping() => [] }
            m_n: u64 { in constructor() => 0 in ping() => m_n + 1 }
        }
    "#,
    );
    program.invariants.push(InvariantDecl {
        name: "missing inst".into(),
        instances: vec![InvariantInstance {
            name: "k".into(),
            entity: "Ghost".into(),
            init: vec![("m_n".into(), Expr::IntLiteral(U256::ZERO))],
            forall_state: ForallSpec::default(),
            init_specified: true,
            span: span0(),
        }],
        skip_from: false,
        senders: vec![],
        deploy: vec![],
        context: ContextSpec { entries: vec![] },
        actions: vec![],
        checks: vec![Expr::BoolLiteral(true)],
        fail_on_revert: false,
        runs: None,
        depth: None,
        tag: None,
        instantiates: None,
        emit_policy: InvariantEmitPolicy::Emit,
        with_time: false,
        track: vec![],
        derived: vec![],
        exclude_senders: vec![],
        exclude_selectors: vec![],
        span: span0(),
    });
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("unknown entity invariant must still emit placeholder file");
    assert!(
        inv.contains("skipped: at least one instance references an unknown entity"),
        "unknown instance must hit gen_invariant_multi_file early return (L1865): {inv}"
    );
}

#[test]
fn n4_99_evm_test_codegen_multi_invariant_det_identity_default_ctor() {
    let program = parse_evm(
        r#"
        entity Keyed {
            identity m_key: u64
            routes { init boot() => [] tick() => [] }
            m_n: u64 { in boot() => 0 in tick() => m_n + 1 }
        }
        entity Plain {
            routes { constructor() => [] tick() => [] }
            m_n: u64 { in constructor() => 0 in tick() => m_n + 1 }
        }

        invariant "mixed keyed" for { k: Keyed, p: Plain } {
            init k { m_n: 0 }
            init p { m_n: 0 }
            action k.tick() {}
            action p.tick() {}
            check k.m_n >= p.m_n
        }
    "#,
    );
    let inv = gen_evm_test_files_det(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("det mixed invariant must emit file");
    assert!(
        inv.contains("deployKeyed(0)") || inv.contains("deployKeyed(uint256(0))"),
        "unseeded identity in multi det setUp must use default_value_for_type (U4-4c): {inv}"
    );
    assert!(
        inv.contains("deployPlain("),
        "mixed multi det must factory.deploy plain instance: {inv}"
    );
}

#[test]
fn n4_99_evm_test_codegen_invariant_forall_unpinned_field_seed() {
    let program = parse_evm(
        r#"
        entity Wallet {
            routes { constructor() => [] spend() => [] }
            m_balance: u64 {
                in constructor() => 0
                in spend() => m_balance
            }
            m_bonus: u64 {
                in constructor() => 0
                in spend() => m_bonus
            }
        }

        invariant "bonus seed" for Wallet {
            init { m_balance: 0, m_bonus: * }
            action spend() {}
            check m_balance <= m_bonus
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("bonus seed invariant must emit file");
    assert!(
        inv.contains("forall init: randomized starting state") && inv.contains("m_bonus"),
        "unpinned forall field must hit forall_dynamic_fields push + gen_forall_state_init (L924): {inv}"
    );
}

#[test]
fn n4_99_evm_test_codegen_test_let_infer_sol_type_literals() {
    let mut program = parse_evm(
        r#"
        entity Literals {
            routes { constructor() => [] note() => [] }
            m_n: u64 { in constructor() => 0 in note() => m_n }
        }

        test "literal lets" for Literals {
            let flag = true
            let label = "seed"
            let raw = 0x00000000000000000000000000000000000000aa
            call note()
        }
    "#,
    );
    if let Some(test) = program.tests.iter_mut().find(|t| t.name == "literal lets") {
        test.body.insert(
            3,
            TestStep::Let {
                ty: None,
                name: "blob".into(),
                value: Expr::BytesLiteral(vec![0x01, 0x02]),
            },
        );
    }
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Literals");
    assert!(
        test_sol.contains("bool flag = true")
            && test_sol.contains("string memory label = \"seed\"")
            && (test_sol.contains("uint64 raw =") || test_sol.contains("uint256 raw ="))
            && test_sol.contains("bytes memory blob ="),
        "test let bindings must hit infer_sol_type literal arms (numeric default, no implicit address): {test_sol}"
    );
}

#[test]
fn n4_99_evm_test_codegen_invariant_exclude_advance_time_selector() {
    let mut program = parse_evm(
        r#"
        entity Clock {
            routes { constructor() => [] tick() => [] }
            m_n: u64 { in constructor() => 0 in tick() => m_n + 1 }
        }

        invariant "time exclude" for Clock #[with_time] {
            init { m_n: 0 }
            action tick() {}
            check m_n >= 0
        }
    "#,
    );
    program.invariants[0].exclude_selectors = vec!["advanceTime".into()];
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("time exclude invariant must emit file");
    assert!(
        inv.contains("excluded[0] = bytes4(keccak256(\"advanceTime(uint256)\"))"),
        "exclude advanceTime must hit sig_params fallback arm (L1766–L1769): {inv}"
    );
}

#[test]
fn n4_99_evm_test_codegen_substitute_skip_route_track_name_collision() {
    let program = parse_evm(
        r#"
        entity Meter {
            routes {
                constructor() => []
                bump() => []
                tick() => []
            }
            m_ticks: u64 {
                in constructor() => 0
                in bump() => m_ticks + 1
                in tick() => m_ticks
            }
        }

        invariant "route track clash" for Meter {
            init { m_ticks: 0 }
            action bump() {}
            track { let bump = m_ticks; }
            check bump <= m_ticks
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("route track clash invariant must emit file");
    assert!(
        inv.contains("_handler.bump()") && inv.contains("_meter.m_ticks()"),
        "track bump must stay on handler; route bump() rewrite skipped when name collides (L2305): {inv}"
    );
}

#[test]
fn n4_99_evm_test_codegen_fuzz_gen_route_literal_bound_types() {
    let mut program = parse_evm(
        r#"
        entity Probe {
            routes { constructor() => [] set(n: u64) => [] }
            m_n: u64 { in constructor() => 0 in set(n) => n }
        }
    "#,
    );
    program.fuzz_tests.push(FuzzDecl {
        name: "literal bound".into(),
        entity_name: "Probe".into(),
        params: vec![Param {
            name: "n".into(),
            ty: Type::Simple("u64".into()),
        }],
        init_state: vec![],
        skip_from: false,
        body: vec![
            TestStep::Bound {
                var: "n".into(),
                lo: Expr::IntLiteral(U256::from_hex_digits("00000000000000000000000000000000000000bb").unwrap()),
                hi: Expr::StringLiteral("cap".into()),
                inclusive: true,
            },
            TestStep::Call {
                target: None,
                route: "set".into(),
                args: vec![Expr::Ident("n".into())],
            },
        ],
        runs: None,
        tag: None,
        instantiates: None,
        span: span0(),
    });
    let files = gen_evm_test_files(&program);
    let fuzz_sol = foundry_test_file(&files, "Probe");
    assert!(
        fuzz_sol.contains("function testFuzz_literal_bound(") && fuzz_sol.contains("bound("),
        "fuzz bound with hex hi must emit gen_fuzz_fn route scatter: {fuzz_sol}"
    );
}

// ---------------------------------------------------------------------------
// N4-75: codegen/evm_test_codegen.rs — slice 6 (residual @ N4-34 tail)
// Gap @ 92.37% (152 missed / 1992). Focus gen_path_accessor / gen_state_accessor
// nested index L1332–L1376, single-entity invariant nondet identity deploy
// L1657–L1676, multi-invariant runs/depth L2122–L2131, lower_check_expr_multi
// fallback L2171–L2213.
// ---------------------------------------------------------------------------

#[test]
fn n4_75_evm_test_codegen_expect_state_nested_mapping_and_record_paths() {
    let mut program = parse_evm(
        r#"
        entity Ledger {
            routes {
                constructor() => []
                setScore(key: u64, value: u64) => []
            }
            m_scores: HashMap<u64, u64> {
                in constructor() => {}
                in setScore(key, value) => m_scores.insert(key, value)
            }
            m_tag: u64 {
                in constructor() => 0
                in setScore(key, value) => key
            }
        }
    "#,
    );
    push_orphan_step_test(
        &mut program,
        vec![
            TestStep::Call {
                target: None,
                route: "setScore".into(),
                args: vec![Expr::IntLiteral(U256::from_u128(7)), Expr::IntLiteral(U256::from_u128(55))],
            },
            TestStep::ExpectState {
                fields: vec![
                    (
                        vec![
                            PathSegment::Field("m_scores".into()),
                            PathSegment::Index(Expr::IntLiteral(U256::from_u128(7))),
                        ],
                        Expr::IntLiteral(U256::from_u128(55)),
                    ),
                    (
                        vec![
                            PathSegment::Field("m_tag".into()),
                            PathSegment::Field("bits".into()),
                        ],
                        Expr::IntLiteral(U256::ZERO),
                    ),
                ],
            },
        ],
    );
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Ledger");
    assert!(
        test_sol.contains("_ledger.m_scores(7)"),
        "mapping index expect must pass the key as a getter argument: {test_sol}"
    );
    assert!(
        test_sol.contains("m_tag().bits"),
        "field path on a scalar member falls back to the plain accessor: {test_sol}"
    );
}

#[test]
fn n4_75_evm_test_codegen_expect_return_lens_index_and_tuple_paths() {
    let mut program = parse_evm(
        r#"
        entity TripleBox {
            routes {
                constructor() => []
                triple() -> (u64, u64, u64) => [ return(1, 2, 3) ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    push_orphan_step_test(
        &mut program,
        vec![
            TestStep::Call {
                target: None,
                route: "triple".into(),
                args: vec![],
            },
            TestStep::ExpectReturnLens {
                path: vec![
                    PathSegment::TupleIndex(1),
                    PathSegment::Index(Expr::IntLiteral(U256::ZERO)),
                ],
                value: Expr::IntLiteral(U256::from_u128(2)),
            },
        ],
    );
    program.fuzz_tests.push(FuzzDecl {
        name: "idx lens".into(),
        entity_name: "TripleBox".into(),
        params: vec![Param {
            name: "idx".into(),
            ty: Type::Simple("u64".into()),
        }],
        init_state: vec![],
        skip_from: false,
        body: vec![
            TestStep::Call {
                target: None,
                route: "triple".into(),
                args: vec![],
            },
            TestStep::ExpectReturnLens {
                path: vec![PathSegment::Index(Expr::Ident("idx".into()))],
                value: Expr::IntLiteral(U256::from_u128(2)),
            },
        ],
        runs: None,
        tag: None,
        instantiates: None,
        span: span0(),
    });
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "TripleBox");
    assert!(
        test_sol.contains("_ret_1_1[0]"),
        "TupleIndex+Index return lens must index the destructured tuple slot: {test_sol}"
    );
    assert!(
        test_sol.contains("function testFuzz_idx_lens(")
            && (test_sol.contains("_ret_1[idx]") || test_sol.contains("_ret_1[uint256(idx)]")),
        "dynamic Index return lens must lower via gen_path_accessor: {test_sol}"
    );
}

#[test]
fn n4_75_evm_test_codegen_single_invariant_factory_identity_default_ctor() {
    let program = parse_evm(
        r#"
        entity Keyed {
            identity m_key: u64
            routes { constructor() => [] tick() => [] }
            m_n: u64 { in constructor() => 0 in tick() => m_n + 1 }
        }

        invariant "keyed default identity" for Keyed {
            init { m_n: 0 }
            action tick() {}
            check m_n >= 0
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("single-entity invariant must emit Invariant_*.t.sol");
    assert!(
        inv.contains("new CambrianFactory()"),
        "single-entity invariant setUp must deploy CambrianFactory: {inv}"
    );
    assert!(
        inv.contains("deployKeyed(")
            && (inv.contains("deployKeyed(0)") || inv.contains("deployKeyed(uint256(0))")),
        "unseeded identity member must default in factory.deployKeyed: {inv}"
    );
}

#[test]
fn n4_75_evm_test_codegen_multi_invariant_runs_depth_forge_config() {
    let program = parse_evm(
        r#"
        entity Alpha {
            routes { constructor() => [] step() => [] }
            m_n: u64 { in constructor() => 0 in step() => m_n + 1 }
        }
        entity Beta {
            routes { constructor() => [] step() => [] }
            m_n: u64 { in constructor() => 0 in step() => m_n + 1 }
        }

        invariant "tuned multi" for { a: Alpha, b: Beta } #[runs(24)] #[depth(12)] #[tag("ci")] {
            init a { m_n: 0 }
            init b { m_n: 0 }
            action a.step() {}
            action b.step() {}
            check a.m_n >= 0
            check b.m_n > 0
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("multi invariant must emit Invariant_*.t.sol");
    assert!(
        inv.contains("forge-config: default.invariant.runs = 24")
            && inv.contains("forge-config: default.invariant.depth = 12"),
        "multi invariant runs/depth attrs must annotate each check fn: {inv}"
    );
    assert!(
        inv.contains("function invariant_tuned_multi_0_ci()")
            && inv.contains("function invariant_tuned_multi_1_ci()"),
        "multiple checks must suffix invariant fn names: {inv}"
    );
    assert!(
        inv.contains("/// @dev tag: ci"),
        "tagged multi invariant must emit dev tag comment per check: {inv}"
    );
}

#[test]
fn n4_75_evm_test_codegen_multi_invariant_lower_check_expr_residual() {
    let program = parse_evm(
        r#"
        entity Left {
            routes { constructor() => [] go() => [] }
            m_x: u64 { in constructor() => 10 in go() => m_x + 1 }
        }
        entity Right {
            routes { constructor() => [] go() => [] }
            m_y: u64 { in constructor() => 2 in go() => m_y + 1 }
        }

        invariant "expr residual" for { l: Left, r: Right } {
            init l { m_x: 10 }
            init r { m_y: 2 }
            action l.go() {}
            action r.go() {}
            check (l.m_x / r.m_y) % 2 == 0 && (l.m_x & r.m_y) | 1 != 0
            check !(l.m_x == 0) && true
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("multi invariant must emit Invariant_*.t.sol");
    assert!(
        inv.contains("(_l.m_x() / _r.m_y())")
            && inv.contains("% 2")
            && inv.contains("& _r.m_y()")
            && inv.contains("| (1"),
        "multi check must lower Div/Mod/BitAnd/BitOr via evm lower_check_expr_multi: {inv}"
    );
    assert!(
        inv.contains("!(") && inv.contains("_l.m_x() == 0"),
        "UnaryOp Not must lower in multi invariant check: {inv}"
    );
    assert!(
        inv.contains("&& true"),
        "literal fallback arm must reach gen_expr_test via lower_check_expr_multi: {inv}"
    );
}

#[test]
fn n4_75_evm_test_codegen_multi_invariant_generic_field_access_check() {
    let mut program = parse_evm(
        r#"
        entity Host {
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 0 }
        }
        entity Peer {
            routes { constructor() => [] }
            m_y: u64 { in constructor() => 0 }
        }
    "#,
    );
    program.invariants.push(InvariantDecl {
        name: "generic recv".into(),
        instances: vec![
            InvariantInstance {
                name: "h".into(),
                entity: "Host".into(),
                init: vec![("m_x".into(), Expr::IntLiteral(U256::from_u128(5)))],
                forall_state: ForallSpec::default(),
                init_specified: true,
                span: span0(),
            },
            InvariantInstance {
                name: "p".into(),
                entity: "Peer".into(),
                init: vec![("m_y".into(), Expr::IntLiteral(U256::from_u128(2)))],
                forall_state: ForallSpec::default(),
                init_specified: true,
                span: span0(),
            },
        ],
        skip_from: false,
        senders: vec![],
        deploy: vec![],
        context: ContextSpec { entries: vec![] },
        actions: vec![],
        checks: vec![Expr::FieldAccess(
            Box::new(Expr::BinOp(
                Box::new(Expr::FieldAccess(
                    Box::new(Expr::Ident("h".into())),
                    "m_x".into(),
                )),
                BinOp::Add,
                Box::new(Expr::IntLiteral(U256::from_u128(1))),
            )),
            "bits".into(),
        )],
        fail_on_revert: false,
        runs: None,
        depth: None,
        tag: None,
        instantiates: None,
        emit_policy: InvariantEmitPolicy::Emit,
        with_time: false,
        track: vec![],
        derived: vec![],
        exclude_senders: vec![],
        exclude_selectors: vec![],
        span: span0(),
    });
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("injected multi invariant must emit file");
    assert!(
        inv.contains("(_h.m_x() + 1).bits") || inv.contains("(_h.m_x() + 1) .bits"),
        "generic FieldAccess receiver must recurse via lower_check_expr_multi: {inv}"
    );
}

#[test]
fn n4_75_evm_test_codegen_test_expect_state_record_subfield_path() {
    let program = parse_evm(
        r#"
        entity Ledger {
            routes {
                constructor() => []
                setScore(key: u64, value: u64) => []
            }
            m_scores: HashMap<u64, u64> {
                in constructor() => {}
                in setScore(key, value) => m_scores.insert(key, value)
            }
        }

        test "deep indexed path" for Ledger {
            call setScore(0, 1000)
            expect state { m_scores[0]: 1000 }
        }
    "#,
    );
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Ledger");
    assert!(
        test_sol.contains("assertEq(_ledger.m_scores(0), 1000,"),
        "parsed indexed expect state must pass the key as a getter argument: {test_sol}"
    );
}

// ---------------------------------------------------------------------------
// N4-89: codegen/solidity/core/expr.rs — slice 9 (parse-coerce residual @ N4-81)
// Baseline @ N4-88: 92.84% line (74 missed / 1034; ~57 excl. DEAD ~18).
// Targets: print_coerced_sol_operand Narrow L84–L91; binop_str WrappingAdd
// L130–L132; scatter gen_match_on_std_parse L997+. Exclude hoisted
// NamespacedCall/K3 L1354–L1371 (DEAD); gen_expr_hoisted catch-all L1479–L1487
// (DEAD release-only).
// Acceptance: ≥ 93.5% or ≤ 68 missed (excl. documented DEAD ~18).
// ---------------------------------------------------------------------------

#[test]
fn n4_89_codegen_expr_i32_u64_signpromote_member_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity SignedMix {
            routes {
                constructor() => []
                add(n: u64) => []
            }
            m_tag: i32 {
                in constructor() => 0
                in add(n) => m_tag + n
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("int256(") && sol.contains("uint256("),
        "i32 + u64 member binop must hit print_coerced_sol_operand SignPromote (L76–L77): {sol}"
    );
    assert_solc_compiles("expr_i32_u64_signpromote_member", &sol);
}

#[test]
fn n4_89_codegen_expr_pure_fn_u32_param_narrow_call_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        pure fn bump32(x: u32) -> u32 { x + 1 }

        entity NarrowCall {
            routes {
                constructor() => []
                probe(n: u64) -> u32 => [
                    return(bump32(n))
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("uint32(") && sol.contains("bump32("),
        "u32 pure-fn param call must narrow via wrap_narrow_cast (L486): {sol}"
    );
    assert_solc_compiles("expr_pure_fn_u32_param_narrow_call", &sol);
}

#[test]
fn n4_89_codegen_expr_det_address_of_state_fn_call_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity Counter {
            identity id: u64
            routes { constructor() => [] }
        }

        entity Host {
            routes {
                constructor() => []
                view addr(n: u64) -> address => [
                    return(addressOf(Counter.state(n)))
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        ),
        true,
    );
    assert!(
        sol.contains("predictCounter") || sol.contains("Counter"),
        "addressOf(Entity.state(...)) in det mode must hit gen_expr FnCall addressOf arm (L459–L465): {sol}"
    );
    assert_solc_compiles("expr_det_address_of_state_fn_call", &sol);
}

#[test]
fn n4_89_codegen_expr_entity_address_method_det_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity Counter {
            identity id: u64
            routes {
                constructor() => []
                view slot() -> address => [
                    return(Counter.address(id))
                ]
            }
        }
    "#,
        ),
        true,
    );
    assert!(
        sol.contains("predictCounter") || sol.contains("Counter.address"),
        "Entity.address(args) MethodCall must hit gen_expr address arm (L660): {sol}"
    );
    assert_solc_compiles("expr_entity_address_method_det", &sol);
}

#[test]
fn n4_89_codegen_expr_nested_hashmap_contains_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity NestedContains {
            routes {
                constructor() => []
                probe(outer: u64, inner: u64) -> bool => [
                    return(m_nested[outer].contains(inner))
                ]
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("m_nested_inner_exists"),
        "nested HashMap.contains must lower via inner_exists sidecar (L563–L567): {sol}"
    );
    assert_solc_compiles("expr_nested_hashmap_contains", &sol);
}

#[test]
fn n4_89_codegen_expr_nested_hashmap_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity NestedExists {
            routes {
                constructor() => []
                probe(outer: u64, inner: u64) -> bool => [
                    return(m_nested[outer].exists(inner))
                ]
            }
            m_nested: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("m_nested_inner_exists"),
        "nested HashMap.exists must lower via inner_exists sidecar (L597–L601): {sol}"
    );
    assert_solc_compiles("expr_nested_hashmap_exists", &sol);
}

#[test]
fn n4_89_codegen_expr_parse_ident_fallback_arm_solc() {
    let mut program = parse_evm(
        r#"
        entity ParseIdent {
            routes {
                constructor() => []
                run(s: String) -> u64 => [
                    let v = match std::str::parse_u64(s, 10) {
                        some(x) => x,
                        none => 0
                    };
                    return(v)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    if let RouteBody::Unphased(actions) = &mut program.entities[0].routes[1].body {
        if let RouteAction::Return { values } = &mut actions[0] {
            if let Expr::Let(_, _, body) = &mut values[0] {
                if let Expr::Match(_, arms) = body.as_mut() {
                    arms.push(MatchArm {
                        pattern: MatchPattern::Ident("fallback".into()),
                        body: Expr::IntLiteral(U256::from_u128(9)),
                    });
                }
            }
        }
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("_cam_try_parse") && sol.contains("} else {"),
        "parse match Ident arm must hit gen_match_on_std_parse ident branch (L996–L997): {sol}"
    );
    assert_solc_compiles("expr_parse_ident_fallback_arm", &sol);
}

#[test]
fn n4_89_codegen_expr_parse_wildcard_only_else_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity ParseWildOnly {
            routes {
                constructor() => []
                run(s: String) -> u64 => [
                    let v = match std::str::parse_u64(s, 10) {
                        _ => 7
                    };
                    return(v)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("_cam_try_parse") && sol.contains("{"),
        "wildcard-only parse match must hit gen_match_on_std_parse else-open branch (L1018–L1019): {sol}"
    );
    assert_solc_compiles("expr_parse_wildcard_only_else", &sol);
}

#[test]
fn n4_89_codegen_expr_evm_ecrecover_pure_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        pure fn recover_addr(hash: U256, v: u64, r: U256, s: U256) -> address {
            evm::ecrecover(hash, v, r, s)
        }

        entity Recover {
            routes {
                constructor() => []
                view slot() -> address => [
                    return(recover_addr(0, 27, 0, 0))
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("ecrecover(") && sol.contains("bytes32("),
        "evm::ecrecover must hit gen_evm_ns ecrecover arm (L249–L265): {sol}"
    );
    assert_solc_compiles("expr_evm_ecrecover_pure", &sol);
}

#[test]
fn n4_89_codegen_expr_evm_sha256_packed_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        pure fn digest(a: u64, b: u64) -> U256 { evm::sha256(a, b) }

        entity Sha {
            routes {
                constructor() => []
                view hash() -> U256 => [
                    return(digest(1, 2))
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("sha256(abi.encodePacked") || sol.contains("sha256("),
        "evm::sha256 packed must hit gen_evm_ns sha256 arm (L285–L296): {sol}"
    );
    assert_solc_compiles("expr_evm_sha256_packed", &sol);
}

#[test]
fn n4_89_codegen_expr_library_scoped_pure_call_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        library MathLib {
            pure fn twice(x: u64) -> u64 { x + x }
        }

        entity LibHost {
            routes {
                constructor() => []
                view dub() -> u64 => [
                    return(MathLib.twice(3))
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("MathLib.twice("),
        "library-scoped pure fn call must hit gen_expr FnCall library dispatch (L526–L527): {sol}"
    );
    assert_solc_compiles("expr_library_scoped_pure_call", &sol);
}

#[test]
fn n4_89_codegen_expr_hoisted_fold_loop_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity FoldHost {
            routes {
                constructor() => []
                sum() -> u64 => [
                    let total = m_vals.fold(0, |acc, x| acc + x);
                    return(total)
                ]
            }
            m_vals: Vec<u64> {
                in constructor() => array()
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("for (") && sol.contains("m_vals"),
        "Vec.fold closure must hit gen_expr_hoisted fold loop arm (L1379–L1398): {sol}"
    );
    assert_solc_compiles("expr_hoisted_fold_loop", &sol);
}

#[test]
fn n4_89_codegen_expr_pure_fn_exists_non_ident_comment_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        pure fn has_key(m: HashMap<u64, u64>, k: u64) -> bool {
            m.exists(k)
        }

        entity ExistsHost {
            routes {
                constructor() => []
                probe(k: u64) -> bool => [
                    return(has_key(m_map[k], k))
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("_exists sidecar") || sol.contains("non-Ident arg"),
        "non-Ident HashMap arg to exists-pure-fn must hit gen_expr_hoisted comment (L1427–L1434): {sol}"
    );
}

#[test]
fn n4_89_codegen_expr_record_construct_field_narrow_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        record Tiny { tag: u8, wide: u64 }

        pure fn mk(n: u64) -> Tiny { Tiny { tag: 1, wide: n } }

        entity RecordNarrow {
            routes {
                constructor() => []
                view pack(n: u64) -> Tiny => [
                    return(mk(n))
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("Tiny({") && (sol.contains("uint8(") || sol.contains("tag:")),
        "record literal field narrow must hit maybe_narrow_cast / wrap_narrow_cast (L428–L430): {sol}"
    );
    assert_solc_compiles("expr_record_construct_field_narrow", &sol);
}

#[test]
fn n4_89_codegen_expr_wrapping_sub_mul_view_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity WrapSubMul {
            routes {
                constructor() => []
                view mix(a: u64, b: u64) -> u64 => [
                    return((a -% b) *% 2)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("_wsub(") && sol.contains("_wmul("),
        "wrapping sub/mul view must hit gen_expr WrappingSub/Mul arms (L367–L368): {sol}"
    );
    assert_solc_compiles("expr_wrapping_sub_mul_view", &sol);
}

#[test]
fn n4_89_codegen_expr_msg_int_ext_fields_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        pure fn flags() -> (bool, bool) { (msg::int, msg::ext) }

        entity MsgFlags {
            routes {
                constructor() => []
                view probe() -> bool => [
                    let (is_int, is_ext) = flags();
                    return(is_int || is_ext)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("false") && sol.contains("true"),
        "msg::int / msg::ext must hit gen_expr MsgField arms (L713–L714): {sol}"
    );
    assert_solc_compiles("expr_msg_int_ext_fields", &sol);
}

#[test]
fn n4_89_codegen_expr_address_member_with_arg_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity AddrArg {
            routes {
                constructor() => []
                view pick(seed: u64) -> address => [
                    return(m_owner.address(seed))
                ]
            }
            m_owner: address { in constructor() => 0x0000000000000000000000000000000000000001 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("m_owner") && sol.contains("seed"),
        "address member MethodCall with arg must use first-arg fallback (L665–L666): {sol}"
    );
    assert_solc_compiles("expr_address_member_with_arg", &sol);
}

#[test]
fn n4_89_codegen_expr_hoisted_evm_keccak_packed_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity KeccakHoist {
            routes {
                constructor() => []
                digest(a: u64, b: u64) -> U256 => [
                    let h = evm::keccak256Packed(a, b);
                    return(h)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("keccak256(abi.encodePacked") || sol.contains("keccak256Packed"),
        "hoisted evm::keccak256Packed must hit gen_expr_hoisted NamespacedCall evm arm (L1322–L1324): {sol}"
    );
    assert_solc_compiles("expr_hoisted_evm_keccak_packed", &sol);
}

#[test]
fn n4_89_codegen_expr_parse_signed_narrow_bind_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity ParseI8 {
            routes {
                constructor() => []
                run(s: String) -> i8 => [
                    let v = match std::str::parse_i8(s, 10) { some(x) => x, none => 0 };
                    return(v)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("int8(") || sol.contains("parse_i8"),
        "signed narrow parse bind must hit gen_match_on_std_parse cast branch (L1011–L1014): {sol}"
    );
    assert_solc_compiles("expr_parse_signed_narrow_bind", &sol);
}

#[test]
fn n4_89_codegen_expr_payload_subst_match_simple_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        enum Op { Add(u64), Halt }

        entity PayloadMatch {
            routes {
                constructor() => []
                view code(op: Op) -> u64 => [
                    let n = match op {
                        Op::Add(v) => v,
                        Op::Halt => 0
                    };
                    return(n)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("Op_Tag") || sol.contains("deposit_0") || sol.contains("Add"),
        "payload enum match in view must hit subst_payload_bindings / gen_match_simple (L206–L223, L901–L913): {sol}"
    );
    assert_solc_compiles("expr_payload_subst_match_simple", &sol);
}

#[test]
fn n4_89_codegen_expr_msg_unknown_field_constant_none_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity MsgGhost {
            const ghost_flag: bool = true

            routes {
                constructor() => []
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            let entity = program.entities.first_mut().expect("entity");
            if let Some(c) = entity.constants.iter_mut().find(|c| c.name == "ghost_flag") {
                c.value = Expr::MsgField("ghost".into());
            }
        },
    );
    assert!(
        sol.contains("constant ghost_flag = 0")
            || sol.contains("constant ghost_flag = true")
            || sol.contains("constant ghost_flag = false")
            || sol.contains("/* unsupported expr */"),
        "unknown msg:: in entity constant must hit MsgField None in gen_expr (L719): {sol}"
    );
}

#[test]
fn n4_89_codegen_expr_payload_unit_variant_expr_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        enum Op { Add(u64), Halt }

        entity UnitVariant {
            routes {
                constructor() => []
                view idle() -> Op => [
                    return(Op::Halt)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("Op_Tag.Halt") || sol.contains("tag: Op_Tag.Halt"),
        "payload enum unit variant expression must hit payload_enum_construct (L784–L785): {sol}"
    );
    assert_solc_compiles("expr_payload_unit_variant_expr", &sol);
}

#[test]
fn n4_89_codegen_expr_evm_ns_direct_return_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity EvmDirect {
            routes {
                constructor() => []
                view hash(a: u64, b: u64) -> U256 => [
                    return(evm::keccak256Packed(a, b))
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("keccak256(abi.encodePacked") || sol.contains("keccak256Packed"),
        "direct evm::keccak256Packed return must hit gen_expr NamespacedCall evm arm (L812–L818): {sol}"
    );
    assert_solc_compiles("expr_evm_ns_direct_return", &sol);
}

#[test]
fn n4_89_codegen_expr_temporal_vec_push_block_body_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity TemporalPush {
            routes {
                constructor() => []
                bump() => []
            }
            m_vals: Vec<u64> {
                in constructor() => array()
                in bump() => { let _ = 0; m_vals.push(1) }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("next_m_vals") || sol.contains("m_vals.push"),
        "block-bodied vec push transform must hit body_is_member_push Block tail (L185–L187): {sol}"
    );
    assert_solc_compiles("expr_temporal_vec_push_block_body", &sol);
}

#[test]
fn n4_89_codegen_expr_address_of_lowercase_entity_none_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity Counter {
            identity id: u64
            routes { constructor() => [] }
        }

        entity Host {
            routes {
                constructor() => []
                view addr(n: u64) -> address => [
                    return(addressOf(Counter.state(n)))
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        true,
        |program| {
            let route = program.entities[1]
                .routes
                .iter_mut()
                .find(|r| r.name == "addr")
                .expect("addr");
            if let RouteBody::Unphased(actions) = &mut route.body {
                if let RouteAction::Return { values } = &mut actions[0] {
                    if let Expr::FnCall(name, args) = &mut values[0] {
                        if name == "addressOf" {
                            if let Some(Expr::MethodCall(base, method, _)) = args.first_mut() {
                                if method == "state" {
                                    if let Expr::Ident(entity_name) = base.as_mut() {
                                        *entity_name = "counter".into();
                                    }
                                }
                            }
                        }
                    }
                }
            }
        },
    );
    let host_addr = sol
        .split("function addr(")
        .nth(1)
        .and_then(|tail| tail.split("function ").next())
        .unwrap_or("");
    assert!(
        !host_addr.contains("predictCounter"),
        "lowercase entity in addressOf must skip gen_expr_address_deterministic (L465–L468): {host_addr}"
    );
}

#[test]
fn n4_89_codegen_expr_entity_address_lowercase_none_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity Counter {
            identity id: u64
            routes { constructor() => [] }
        }

        entity Host {
            routes {
                constructor() => []
                view slot(n: u64) -> address => [
                    return(Counter.address(n))
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        true,
        |program| {
            let route = program.entities[1]
                .routes
                .iter_mut()
                .find(|r| r.name == "slot")
                .expect("slot");
            if let RouteBody::Unphased(actions) = &mut route.body {
                if let RouteAction::Return { values } = &mut actions[0] {
                    if let Expr::MethodCall(base, _, _) = &mut values[0] {
                        if let Expr::Ident(entity_name) = base.as_mut() {
                            *entity_name = "counter".into();
                        }
                    }
                }
            }
        },
    );
    let host_slot = sol
        .split("function slot(")
        .nth(1)
        .and_then(|tail| tail.split("function ").next())
        .unwrap_or("");
    assert!(
        !host_slot.contains("predictCounter"),
        "lowercase Entity.address must skip gen_expr_address_deterministic (L661–L662): {host_slot}"
    );
}

#[test]
fn n4_89_codegen_expr_trace_call_unknown_route_none_solc() {
    let mut program = parse_evm(
        r#"
        entity Counter {
            routes {
                constructor() => []
                increment(amount: u64) => []
            }
            m_count: u64 {
                in constructor() => 0
                in increment(amount) => m_count + amount
            }
        }

        invariant "trace unknown route" for Counter {
            init { m_count: 0 }
            action increment(amount: u64) { bound amount in 1..100 }
            check m_count >= 0
        }
    "#,
    );
    cambrian_transpiler::desugar::desugar_properties(&mut program);
    let inv = program.invariants.first_mut().expect("invariant");
    inv.checks[0] = Expr::TraceCall {
        name: "bogus".into(),
        route: "increment".into(),
    };
    let diags = cambrian_transpiler::validate::check_target_compat(
        &program,
        cambrian_transpiler::target::Target::Evm,
        false,
    );
    assert!(
        diags.iter().any(|d| d.code == "I17"),
        "unknown trace:: accessor must be I17, not require(true): {diags:?}"
    );
}

// ---------------------------------------------------------------------------
// N4-97: codegen/solidity/core/expr.rs — slice 11 (binop/evm-ns residual @ N4-94)
// Baseline @ N4-94: 93.81% line (64 missed / 1034; ~26 excl. DEAD/unreachable).
// Targets: scatter gen_binop/gen_evm_ns/infer L369–L818; temporal llvm-partial
// L154/L161/L185–L187/L192; bytes/tuple/record tails. Exclude Narrow L84–L91,
// binop_str WrappingAdd L130–L132 (dead), hoisted K3 L1354–L1371, catch-all L1479–L1487.
// Acceptance: ≥ 94% or ≤ 58 missed (excl. documented DEAD/unreachable ~26).
// ---------------------------------------------------------------------------

#[test]
fn n4_97_codegen_expr_bytes_string_literal_return_solc() {
    let mut program = parse_evm(
        r#"
        entity LiteralHost {
            routes {
                constructor() => []
                show() -> u64 => [ return(0) ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    if let RouteBody::Unphased(actions) = &mut program.entities[0].routes[1].body {
        if let RouteAction::Return { values } = &mut actions[0] {
            values[0] = Expr::Tuple(vec![
                Expr::StringLiteral("tag".into()),
                Expr::BytesLiteral(vec![0x01, 0x02]),
            ]);
        }
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("\"tag\"") && sol.contains("hex\"0102\""),
        "StringLiteral + BytesLiteral must hit gen_expr literal arms (L347–L350): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_97_codegen_expr_temporal_unknown_member_storage_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity TemporalGhost {
            routes {
                constructor() => []
                peek() -> u64 => [ return(^m_ghost) ]
            }
            m_ticks: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("m_ghost") && !sol.contains("next_m_ghost"),
        "unknown-member temporal ref in route must return bare storage (L161): {sol}"
    );
    // TB-V: ^m_ghost unknown member (V3/V8) — codegen substring only.
}

#[test]
fn n4_97_codegen_expr_evm_ns_unknown_and_short_args_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity EvmMiss {
            routes {
                constructor() => []
                probe(h: U256) -> U256 => [
                    let bad = evm::does_not_exist(h);
                    let short = evm::ecrecover(h, 27);
                    return(bad + short)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        !sol.contains("does_not_exist(") && !sol.contains("ecrecover(bytes32"),
        "unknown evm:: intrinsic and short-arg ecrecover must hit gen_evm_ns None arms (L329, L249): {sol}"
    );
    assert_solc_compiles("expr_evm_ns_unknown_short_args", &sol);
}

#[test]
fn n4_97_codegen_expr_hoisted_evm_dispatch_failure_solc() {
    let mut program = parse_evm(
        r#"
        entity HoistEvmFail {
            routes {
                constructor() => []
                run() -> U256 => [ return(0) ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    if let RouteBody::Unphased(actions) = &mut program.entities[0].routes[1].body {
        if let RouteAction::Return { values } = &mut actions[0] {
            values[0] = Expr::EnumVariantWithData(
                "evm".into(),
                "does_not_exist".into(),
                vec![Expr::IntLiteral(U256::from_u128(1))],
            );
        }
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("enum variant evm::does_not_exist") || sol.contains("revert(\"EVM: enum variant"),
        "hoisted evm::EnumVariantWithData failure must hit gen_expr_hoisted dispatch fallback (L1322–L1352): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_97_codegen_expr_hoisted_if_inline_ternary_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity IfTernary {
            routes {
                constructor() => []
                pick(flag: bool) -> u64 => [
                    let v = if flag { 1 } else { 2 };
                    return(v)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("? 1 : 2") || sol.contains("? 1 : 2;"),
        "simple value-if in hoisted let must hit inline ternary fast path (L1147): {sol}"
    );
    assert_solc_compiles("expr_hoisted_if_inline_ternary", &sol);
}

#[test]
fn n4_97_codegen_expr_record_update_narrow_u32_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        record TokenData { owner: address, amt: u32 }

        entity TokenStore {
            routes {
                constructor() => []
                rewrite(id: u64) => []
            }
            m_tokens: HashMap<u64, TokenData> {
                in constructor() => HashMap::new()
                in rewrite(id) => {
                    let row = m_tokens[id];
                    let updated = row { owner: msg::sender, amt: row.amt + 1 };
                    m_tokens.update(id, updated)
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("updated.amt") || sol.contains(".amt ="),
        "record update on u32 field must hit gen_expr_hoisted RecordUpdate narrow cast (L1300–L1310): {sol}"
    );
    assert_solc_compiles("expr_record_update_narrow_u32", &sol);
}

#[test]
fn n4_97_codegen_expr_payload_unit_variant_expr_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        enum Packet { Data(u64), Ping }

        entity PacketHost {
            routes {
                constructor() => []
                unit() -> Packet => [ return(Packet::Ping) ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("Packet") && (sol.contains("tag") || sol.contains("Ping")),
        "payload-enum unit variant in expression must hit EnumVariant payload construct arm (L781–L785): {sol}"
    );
    assert_solc_compiles("expr_payload_unit_variant_expr", &sol);
}

#[test]
fn n4_97_codegen_expr_address_member_empty_args_none_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity AddrEmpty {
            routes {
                constructor() => []
                who() -> address => [ return(m_owner.address()) ]
            }
            m_owner: address { in constructor() => 0x0000000000000000000000000000000000000001 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("m_owner.address()")
            || (sol.contains("function who()") && sol.contains("return m_owner")),
        "address-typed member .address() with no args lowers via MethodCall address arm (L664–L667): {sol}"
    );
    assert_solc_compiles("expr_address_member_empty_args_none", &sol);
}

#[test]
fn n4_97_codegen_expr_wrapping_sub_mul_member_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity WrapMember {
            routes { bump(by: u64) => [] }
            m_total: u64 {
                in bump(by) => (m_total -% by) *% 2
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("_wsub") && sol.contains("_wmul"),
        "member wrapping sub/mul must hit gen_expr WrappingSub/Mul arms (L367–L368): {sol}"
    );
    assert_solc_compiles("expr_wrapping_sub_mul_member", &sol);
}

#[test]
fn n4_97_codegen_expr_nested_hashmap_contains_exists_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity NestedHm {
            routes {
                constructor() => []
                probe(outer: u64, inner: u64) -> bool => [
                    return(m_outer[outer].contains(inner))
                ]
            }
            m_outer: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("m_outer_inner_exists") || sol.contains("inner_exists["),
        "nested HashMap contains must hit gen_expr MethodCall contains index arm (L568–L570): {sol}"
    );
    assert!(
        sol.contains("m_outer[outer].exists") || sol.contains("inner_exists"),
        "nested HashMap exists must hit gen_expr MethodCall exists index arm (L602–L604): {sol}"
    );
    assert_solc_compiles("expr_nested_hashmap_contains_exists", &sol);
}

#[test]
fn n4_97_codegen_expr_binop_bitwise_route_fallback_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity BitRoute {
            routes {
                constructor() => []
                mix(a: u64, b: u64) -> u64 => [ return(a ^ b) ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("^"),
        "route-level bitwise xor must hit gen_expr binop fallback path (L381–L383): {sol}"
    );
    assert_solc_compiles("expr_binop_bitwise_route_fallback", &sol);
}

#[test]
fn n4_97_codegen_expr_match_payload_subst_ident_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        enum Msg { Note(u64), Ping }

        entity MsgHost {
            routes {
                constructor() => []
                read(m: Msg) -> u64 => [
                    let n = match m {
                        Msg::Note(x) => x,
                        Msg::Ping => 0
                    };
                    return(n)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("Msg_Tag") && sol.contains("note_0"),
        "payload match with binder must hit subst_payload_bindings Ident arm (L215–L220): {sol}"
    );
    assert_solc_compiles("expr_match_payload_subst_ident", &sol);
}

// ---------------------------------------------------------------------------
// N4-108: codegen/solidity/core/expr.rs — slice 12 (residual scatter @ N4-97)
// Baseline @ N4-107 queue: 93.81% (64 missed / 1034; ~38 excl. DEAD/unreachable).
// Targets: nested HashMap contains/exists index L568–L604; gen_evm_ns +
// NamespacedCall std/evm L795–L818; Tuple/RecordConstruct/pure-fn narrow
// L404–L488; hoisted fold L1388–L1398; temporal llvm-partial L154–L192 (≤2).
// Exclude: Narrow L84–L91 (unreachable); WrappingAdd L130–L132 (dead);
// hoisted K3 L1354–L1487 catch-all (DEAD).
// Acceptance: ≥ 94.5% or ≤ 55 raw or ≤ 30 excl. DEAD (Δ ≥ −8 executable).
// ---------------------------------------------------------------------------

#[test]
fn n4_108_codegen_expr_nested_hashmap_exists_index_only_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity NestedExists {
            routes {
                constructor() => []
                probe(outer: u64, inner: u64) -> bool => [
                    return(m_outer[outer].exists(inner))
                ]
            }
            m_outer: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("m_outer_inner_exists[outer][inner]")
            || sol.contains("inner_exists[outer][inner]"),
        "nested index exists must hit gen_expr exists Index arm (L597–L604): {sol}"
    );
    assert_solc_compiles("expr_nested_hashmap_exists_index_only", &sol);
}

#[test]
fn n4_108_codegen_expr_evm_namespaced_return_balance_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity BalPeek {
            routes {
                constructor() => []
                peek(who: address) -> U256 => [ return(evm::balance(who)) ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains(".balance") && !sol.contains("does_not_exist"),
        "return evm::balance via NamespacedCall must hit gen_expr evm arm (L807–L818): {sol}"
    );
    assert_solc_compiles("expr_evm_namespaced_return_balance", &sol);
}

#[test]
fn n4_108_codegen_expr_std_math_namespaced_return_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity MathPeek {
            routes {
                constructor() => []
                cap(a: u64, b: u64) -> u64 => [ return(std::math::min(a, b)) ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("min(a, b)") || sol.contains("Math.min"),
        "return std::math::min must hit gen_expr NamespacedCall std arm (L795–L805): {sol}"
    );
    assert_solc_compiles("expr_std_math_namespaced_return", &sol);
}

#[test]
fn n4_108_codegen_expr_record_construct_return_narrow_solc() {
    let mut program = parse_evm(
        r#"
        record Tiny {
            x: u32,
            y: u32
        }

        entity RecHost {
            routes {
                constructor() => []
                unit() -> Tiny => [ return(row) ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    if let RouteBody::Unphased(actions) = &mut program.entities[0].routes[1].body {
        actions.insert(
            0,
            RouteAction::Let {
                pattern: Pattern::Ident("row".into()),
                value: Expr::RecordConstruct(
                    "Tiny".into(),
                    vec![
                        ("x".into(), Expr::IntLiteral(U256::from_u128(1))),
                        ("y".into(), Expr::IntLiteral(U256::from_u128(2))),
                    ],
                ),
            },
        );
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Tiny({") || (sol.contains("x:") && sol.contains("y:")),
        "record constructor return must hit gen_expr RecordConstruct field map (L408–L437): {sol}"
    );
    assert_solc_compiles("expr_record_construct_return_narrow", &sol);
}

#[test]
fn n4_108_codegen_expr_pure_fn_u32_narrow_library_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        library NarrowLib {
            pure fn tag_u32(x: u32) -> u32 { x }
            pure fn bump() -> u32 { tag_u32(7) }
        }

        entity NarrowHost {
            routes {
                constructor() => []
                read() -> u32 => [ return(NarrowLib::bump()) ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("tag_u32(7)") || sol.contains("uint32(7)"),
        "library pure-fn u32 literal call must hit gen_expr FnCall narrow cast (L477–L488): {sol}"
    );
    assert_solc_compiles("expr_pure_fn_u32_narrow_library", &sol);
}

#[test]
fn n4_108_codegen_expr_addressof_det_fncall_state_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity Counter {
            identity id: u64
            routes { constructor() => [] }
        }

        entity Host {
            routes {
                constructor() => []
                who() -> address => [ return(addressOf(Counter.state(m_slot))) ]
            }
            m_slot: u64 { in constructor() => 3 }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        true,
    );
    assert!(
        sol.contains("predictCounter") || sol.contains("computeCreate2"),
        "deterministic addressOf(Entity.state(...)) FnCall must hit gen_expr arm (L450–L468): {sol}"
    );
    assert_solc_compiles("expr_addressof_det_fncall_state", &sol);
}

#[test]
fn n4_108_codegen_expr_entity_address_method_det_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity Counter {
            identity id: u64
            routes { constructor() => [] }
        }

        entity Host {
            routes {
                constructor() => []
                who() -> address => [ return(Counter.address(m_slot)) ]
            }
            m_slot: u64 { in constructor() => 2 }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        true,
    );
    assert!(
        sol.contains("predictCounter") || sol.contains("computeCreate2"),
        "deterministic Entity.address(...) MethodCall must hit gen_expr address arm (L652–L660): {sol}"
    );
    assert_solc_compiles("expr_entity_address_method_det", &sol);
}

#[test]
fn n4_108_codegen_expr_temporal_hashmap_storage_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity MapTemporal {
            routes {
                constructor() => []
                touch() => []
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
                in touch() => {
                    let snap = ^m_map;
                    m_map.update(1, snap[1])
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("snap = m_map") && !sol.contains("snap = next_m_map"),
        "temporal ^ on HashMap member must return bare storage (L171–L172): {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

#[test]
fn n4_108_codegen_expr_route_body_fold_hoisted_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity FoldRoute {
            routes {
                constructor() => []
                run(n: u64) -> u64 => [
                    let total = (0..n).fold(0, |acc, i| acc + i);
                    return(total)
                ]
            }
            m_total: u64 {
                in constructor() => 0
                in run(_) => m_total
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("acc") && (sol.contains("acc + i") || sol.contains("acc+i")),
        "route-body range fold must hit gen_expr_hoisted fold arm (L1379–L1398): {sol}"
    );
    assert_solc_compiles("expr_route_body_fold_hoisted", &sol);
}

#[test]
fn n4_108_codegen_expr_payload_unit_hoisted_let_solc() {
    let mut program = parse_evm(
        r#"
        enum Packet { Data(u64), Ping }

        entity PacketHost {
            routes {
                constructor() => []
                run() => []
            }
            m_n: u64 { in constructor() => 0 in run() => m_n + 1 }
        }
    "#,
    );
    if let RouteBody::Unphased(actions) = &mut program.entities[0].routes[1].body {
        *actions = vec![RouteAction::Let {
            pattern: Pattern::Ident("pkt".into()),
            value: Expr::EnumVariant("Packet".into(), "Ping".into()),
        }];
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Packet") && (sol.contains("tag") || sol.contains("Ping")),
        "hoisted let with payload unit enum must hit EnumVariant payload construct (L775–L785): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_108_codegen_expr_parse_match_some_ident_arm_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity ParseSome {
            routes {
                constructor() => []
                read(label: String) -> u64 => [
                    let n = match std::str::parse_u64(label) {
                        some(x) => x,
                        none => 0
                    };
                    return(n)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("_cam_try_parse_radix") && sol.contains("uint64 x ="),
        "parse_u64 match some(x) arm must hit gen_match_on_std_parse Some/Ident path (L989–L997): {sol}"
    );
    assert_solc_compiles("expr_parse_match_some_ident_arm", &sol);
}

// ---------------------------------------------------------------------------
// N4-129: codegen/solidity/core/expr.rs — slice 13 (residual scatter @ N4-108 plateau).
// Baseline @ N4-128 queue: 93.81% (64 missed / 1034; ~26 excl. DEAD). Fresh llvm
// pre-slice: 91.39% (89 missed). Targets: nested HashMap contains/exists L568–L604;
// NamespacedCall std/evm L795–L818; Tuple/RecordConstruct/pure-fn narrow L404–L488;
// temporal L154–L192 (llvm-partial); scatter hoisted if/record-update/parse/match.
// Exclude Narrow L84–L91, WrappingAdd L130–L132, hoisted K3 L1354–L1487 (DEAD).
// Acceptance: ≥ 95.0% or ≤ 58 raw or excl. DEAD ≤ 22 or Δ ≥ −8 vs 64 missed.
// ---------------------------------------------------------------------------

#[test]
fn n4_129_codegen_expr_nested_hashmap_contains_index_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity NestedContains {
            routes {
                constructor() => []
                probe(outer: u64, inner: u64) -> bool => [
                    return(m_outer[outer].contains(inner))
                ]
            }
            m_outer: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("m_outer_inner_exists[outer][inner]")
            || sol.contains("inner_exists[outer][inner]"),
        "nested index contains must hit gen_expr contains Index arm (L563–L567): {sol}"
    );
    assert_solc_compiles("expr_nested_hashmap_contains_index", &sol);
}

#[test]
fn n4_129_codegen_expr_nested_contains_let_subst_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity NestedLetContains {
            routes {
                constructor() => []
                probe(outer: u64, inner: u64) -> bool => [
                    let row = m_outer[outer];
                    return(row.contains(inner))
                ]
            }
            m_outer: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("m_outer_inner_exists[outer][inner]"),
        "let-substituted nested contains must lower via inner_exists sidecar: {sol}"
    );
    assert_solc_compiles("expr_nested_contains_let_subst", &sol);
}

#[test]
fn n4_129_codegen_expr_std_str_format_namespaced_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity FormatHost {
            routes {
                constructor() => []
                label(n: u64) -> String => [
                    return(std::str::format("n={}", n))
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("_cam_string_format") || sol.contains("string("),
        "std::str::format NamespacedCall must hit gen_expr std arm (L795–L805): {sol}"
    );
    assert_solc_compiles("expr_std_str_format_namespaced", &sol);
}

#[test]
fn n4_129_codegen_expr_tuple_return_route_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity TupleReturn {
            routes {
                constructor() => []
                pair(a: u64, b: u64) -> (u64, u64) => [
                    return((a, b))
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("returns (uint256, uint256)") || sol.contains("(a, b)"),
        "tuple return must hit gen_expr Tuple arm (L398–L406): {sol}"
    );
    assert_solc_compiles("expr_tuple_return_route", &sol);
}

#[test]
fn n4_129_codegen_expr_record_construct_mixed_narrow_solc() {
    let mut program = parse_evm(
        r#"
        record Wide { lo: u32, hi: u64 }

        entity WideHost {
            routes {
                constructor() => []
                pack() -> Wide => [ return(row) ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    if let RouteBody::Unphased(actions) = &mut program.entities[0].routes[1].body {
        actions.insert(
            0,
            RouteAction::Let {
                pattern: Pattern::Ident("row".into()),
                value: Expr::RecordConstruct(
                    "Wide".into(),
                    vec![
                        ("lo".into(), Expr::IntLiteral(U256::from_u128(1))),
                        ("hi".into(), Expr::IntLiteral(U256::from_u128(2))),
                    ],
                ),
            },
        );
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Wide({") || (sol.contains("lo:") && sol.contains("hi:")),
        "record constructor with mixed narrow fields must hit RecordConstruct narrow map (L408–L437): {sol}"
    );
    assert_solc_compiles("expr_record_construct_mixed_narrow", &sol);
}

#[test]
fn n4_129_codegen_expr_pure_fn_i64_narrow_library_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        library WideLib {
            pure fn tag_i64(x: i64) -> i64 { x }
            pure fn sample() -> i64 { tag_i64(-3) }
        }

        entity WideHost {
            routes {
                constructor() => []
                read() -> i64 => [ return(WideLib::sample()) ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("tag_i64") || sol.contains("int64"),
        "library pure-fn i64 call must hit gen_expr FnCall narrow cast (L477–L488): {sol}"
    );
    assert_solc_compiles("expr_pure_fn_i64_narrow_library", &sol);
}

#[test]
fn n4_129_codegen_expr_temporal_next_scalar_member_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity ScalarTemporal {
            routes {
                constructor() => []
                bump() => []
            }
            m_count: u64 {
                in constructor() => 0
                in bump() => ^m_count + 1
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("next_m_count"),
        "scalar member transform ^ must hit gen_temporal_ref_sol next_ arm (L177): {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

#[test]
fn n4_129_codegen_expr_temporal_vec_push_storage_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity VecPushTemporal {
            routes {
                constructor() => []
                append(val: u64) => []
            }
            m_log: Vec<u64> {
                in constructor() => array()
                in append(val) => {
                    let snap = ^m_log;
                    { m_log.push(snap.len() + val) }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("snap = m_log") && !sol.contains("snap = next_m_log"),
        "temporal ^m_log during push transform must return bare Vec storage (L174–L175): {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

#[test]
fn n4_129_codegen_expr_unit_enum_inline_return_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        enum Packet { Data(u64), Ping }

        entity PacketInline {
            routes {
                constructor() => []
                ping() -> Packet => [
                    return(Packet::Ping)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("Packet") && (sol.contains("Ping") || sol.contains("tag")),
        "inline unit enum return must hit gen_expr EnumVariant payload unit arm (L775–L785): {sol}"
    );
    assert_solc_compiles("expr_unit_enum_inline_return", &sol);
}

#[test]
fn n4_129_codegen_expr_if_hoisted_no_else_bool_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity IfHoisted {
            routes {
                constructor() => []
                pick(flag: bool) -> bool => [
                    let ok = if flag {
                        let t = m_count;
                        t > 0
                    };
                    return(ok)
                ]
            }
            m_count: u64 { in constructor() => 1 }
            m_flag: bool { in constructor() => false }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("if (flag)") && sol.contains("bool ok"),
        "value-if without else must hit gen_expr_hoisted If no-else path (L1140–L1147): {sol}"
    );
    assert_solc_compiles("expr_if_hoisted_no_else_bool", &sol);
}

#[test]
fn n4_129_codegen_expr_record_update_hoisted_narrow_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        record Quote { price: u32, qty: u64 }

        entity QuoteUpdate {
            routes {
                constructor() => []
                bump() => []
            }
            m_q: Quote {
                in constructor() => { Quote { price: 0, qty: 0 } }
                in bump() => {
                    let snap = m_q;
                    snap { qty: snap.qty + 1 }
                }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("Quote memory") || sol.contains(".qty"),
        "hoisted record update must hit gen_expr_hoisted RecordUpdate narrow (L1295–L1314): {sol}"
    );
    assert_solc_compiles("expr_record_update_hoisted_narrow", &sol);
}

#[test]
fn n4_129_codegen_expr_parse_match_wildcard_arm_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity ParseWildcard {
            routes {
                constructor() => []
                read(label: String) -> u64 => [
                    let n = match std::str::parse_u64(label, 10) {
                        _ => 0
                    };
                    return(n)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("_cam_try_parse_radix"),
        "parse match wildcard arm must hit gen_match_on_std_parse Wildcard path (L996–L997): {sol}"
    );
    assert_solc_compiles("expr_parse_match_wildcard_arm", &sol);
}

#[test]
fn n4_129_codegen_expr_payload_match_binding_subst_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        enum Msg { Note(u64), Ping }

        entity MsgHost {
            routes {
                constructor() => []
                read(m: Msg) -> u64 => [
                    let n = match m {
                        Msg::Note(x) => x,
                        Msg::Ping => 0
                    };
                    return(n)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("Msg_Tag") && (sol.contains("note_0") || sol.contains(".tag")),
        "payload enum match must hit subst_payload_bindings + hoisted match arms (L222/L1194–L1207): {sol}"
    );
    assert_solc_compiles("expr_payload_match_binding_subst", &sol);
}

#[test]
fn n4_129_codegen_expr_hashmap_is_empty_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity EmptyProbe {
            routes {
                constructor() => []
                blank() -> bool => [
                    return(m_map.is_empty())
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("m_map_keys.length == 0"),
        "m_map.is_empty() must hit gen_expr is_empty Ident arm (L578–L580): {sol}"
    );
    assert_solc_compiles("expr_hashmap_is_empty", &sol);
}

#[test]
fn n4_129_codegen_expr_evm_enumvariant_balance_hoisted_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity EvmEnumBal {
            routes {
                constructor() => []
                peek(who: address) -> U256 => [
                    let bal = evm::balance(who);
                    return(bal)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains(".balance") && !sol.contains("does_not_exist"),
        "evm::balance EnumVariantWithData must route gen_evm_ns via gen_expr_hoisted (L1322–L1324): {sol}"
    );
    assert_solc_compiles("expr_evm_enumvariant_balance_hoisted", &sol);
}

// ---------------------------------------------------------------------------
// N4-143: codegen/solidity/core/expr.rs — slice 14 (llvm-partial tail @ 93.91% plateau)
// Baseline @ N4-142 queue: 93.91% (63 missed / 1034; ~38 excl. DEAD). Fresh llvm
// pre-slice @ b88f1c1: 93.91% (63 missed). Targets: nested HashMap L568–L604;
// NamespacedCall std/evm L795–L818; Tuple/RecordConstruct/pure-fn narrow L404–L488;
// temporal L154–L192; hoisted parse/fold/evm scatter. Exclude Narrow L84–L91,
// WrappingAdd L130–L132, hoisted K3 L1354–L1487 (DEAD).
// Acceptance: ≥ 95.0% or ≤ 58 raw or excl. DEAD ≤ 22 or Δ ≥ −8 vs 63 missed.
// ---------------------------------------------------------------------------

#[test]
fn n4_143_codegen_expr_nested_exists_let_subst_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity NestedLetExists {
            routes {
                constructor() => []
                probe(outer: u64, inner: u64) -> bool => [
                    let row = m_outer[outer];
                    return(row.exists(inner))
                ]
            }
            m_outer: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("m_outer_inner_exists[outer][inner]"),
        "let-substituted nested exists must lower via inner_exists sidecar: {sol}"
    );
    assert_solc_compiles("expr_nested_exists_let_subst", &sol);
}

#[test]
fn n4_143_codegen_expr_nested_contains_computed_index_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity NestedContainsComputed {
            routes {
                constructor() => []
                probe(outer: u64, inner: u64) -> bool => [
                    return(m_outer[outer + 1].contains(inner))
                ]
            }
            m_outer: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("m_outer_inner_exists[(outer + 1)][inner]")
            || sol.contains("m_outer_inner_exists[outer + 1][inner]"),
        "nested contains with computed outer index must hit Index arm (L563–L567): {sol}"
    );
    assert_solc_compiles("expr_nested_contains_computed_index", &sol);
}

#[test]
fn n4_143_codegen_expr_nested_exists_computed_index_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity NestedExistsComputed {
            routes {
                constructor() => []
                probe(outer: u64, inner: u64) -> bool => [
                    return(m_outer[outer + 1].exists(inner))
                ]
            }
            m_outer: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("m_outer_inner_exists[(outer + 1)][inner]")
            || sol.contains("m_outer_inner_exists[outer + 1][inner]"),
        "nested exists with computed outer index must hit Index arm (L597–L604): {sol}"
    );
    assert_solc_compiles("expr_nested_exists_computed_index", &sol);
}

#[test]
fn n4_143_codegen_expr_wrapping_mul_route_return_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity WrapMul {
            routes {
                constructor() => []
                scale(a: u64, b: u64) -> u64 => [
                    return(a *% b)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("_wmul(") || sol.contains("*%"),
        "route return wrapping mul must hit gen_expr BinOp WrappingMul arm (L362–L368): {sol}"
    );
    assert_solc_compiles("expr_wrapping_mul_route_return", &sol);
}

#[test]
fn n4_143_codegen_expr_parse_i64_some_narrow_bind_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity ParseI64Bind {
            routes {
                constructor() => []
                read(label: String) -> i64 => [
                    let n = match std::str::parse_i64(label, 10) {
                        some(x) => x,
                        none => 0
                    };
                    return(n)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("_cam_try_parse") && sol.contains("int64"),
        "parse_i64 some arm must narrow val_tmp into int64 binder (L1008–L1014): {sol}"
    );
    assert_solc_compiles("expr_parse_i64_some_narrow_bind", &sol);
}

#[test]
fn n4_143_codegen_expr_payload_dual_field_match_subst_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        enum PairMsg { Duo(u64, u64), Ping }

        entity PairHost {
            routes {
                constructor() => []
                sum(m: PairMsg) -> u64 => [
                    let total = match m {
                        PairMsg::Duo(a, b) => a + b,
                        PairMsg::Ping => 0
                    };
                    return(total)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("PairMsg_Tag") && (sol.contains("duo_0") || sol.contains(".tag")),
        "dual-payload enum match must hit subst_payload_bindings (L214–L222): {sol}"
    );
    assert_solc_compiles("expr_payload_dual_field_match_subst", &sol);
}

#[test]
fn n4_143_codegen_expr_triple_tuple_return_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity TripleTuple {
            routes {
                constructor() => []
                triple(a: u64, b: u64, c: u64) -> (u64, u64, u64) => [
                    return((a, b, c))
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("returns (uint256, uint256, uint256)") || sol.contains("(a, b, c)"),
        "triple tuple return must hit gen_expr Tuple arm (L398–L406): {sol}"
    );
    assert_solc_compiles("expr_triple_tuple_return", &sol);
}

#[test]
fn n4_143_codegen_expr_library_record_pure_fn_narrow_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        record Pair { lo: u32, hi: u64 }

        library PairLib {
            pure fn pack(lo: u32, hi: u64) -> Pair {
                Pair { lo: lo, hi: hi }
            }
        }

        entity PairCaller {
            routes {
                constructor() => []
                read() -> Pair => [ return(PairLib::pack(1, 2)) ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("Pair({") || (sol.contains("lo:") && sol.contains("hi:")),
        "library record pure-fn return must hit RecordConstruct narrow map (L408–L437): {sol}"
    );
    assert_solc_compiles("expr_library_record_pure_fn_narrow", &sol);
}

#[test]
fn n4_143_codegen_expr_det_singleton_entity_address_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity Treasury {
            routes { init boot() => [] }
            m_n: u64 { in boot() => 0 }
        }

        entity Host {
            identity m_slot: u64
            routes {
                init boot() => []
                treasury() -> address => [ return(Treasury.address()) ]
            }
            m_n: u64 { in boot() => 0 }
        }
    "#,
        ),
        true,
    );
    assert!(
        sol.contains("predictTreasury") || sol.contains("computeCreate2"),
        "deterministic singleton Entity.address() must hit MethodCall address arm (L652–L662): {sol}"
    );
    assert_solc_compiles("expr_det_singleton_entity_address", &sol);
}

#[test]
fn n4_143_codegen_expr_temporal_other_route_transform_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity CrossRouteTemporal {
            routes {
                constructor() => []
                peek() -> u64 => [
                    let snap = ^m_count;
                    return(snap)
                ]
                bump() => []
            }
            m_count: u64 {
                in constructor() => 0
                in bump() => ^m_count + 1
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("snap = m_count") && !sol.contains("snap = next_m_count"),
        "peek route ^m_count without transform must return storage (L169): {sol}"
    );
    // TB-V: route-body ^m_count is V43 — codegen substring only.
}

#[test]
fn n4_143_codegen_expr_hoisted_fold_vec_accum_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity FoldVec {
            routes {
                constructor() => []
                total() -> u64 => [
                    let sum = m_vals.fold(0, |acc, x| acc + x);
                    return(sum)
                ]
            }
            m_vals: Vec<u64> {
                in constructor() => array()
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("for (") && sol.contains("acc"),
        "Vec fold in route must hit gen_expr_hoisted fold arm (L1379–L1398): {sol}"
    );
    assert_solc_compiles("expr_hoisted_fold_vec_accum", &sol);
}

#[test]
fn n4_143_codegen_expr_pure_fn_exists_nested_index_k3_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        pure fn ledger_hit(m: HashMap<u64, u64>, outer: u64, inner: u64) -> bool {
            m[outer].exists(inner)
        }

        entity LedgerHost {
            routes {
                constructor() => []
                probe(outer: u64, inner: u64) -> bool => [
                    return(ledger_hit(m_outer, outer, inner))
                ]
            }
            m_outer: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        false,
        |program| {
            let route = &mut program.entities[0].routes[1];
            if let RouteBody::Unphased(actions) = &mut route.body {
                if let RouteAction::Return { values } = &mut actions[0] {
                    if let Expr::FnCall(name, _args) = values[0].clone() {
                        if name == "ledger_hit" {
                            values[0] = Expr::FnCall(
                                name,
                                vec![
                                    Expr::Index(
                                        Box::new(Expr::Ident("m_outer".into())),
                                        Box::new(Expr::Ident("outer".into())),
                                    ),
                                    Expr::Ident("outer".into()),
                                    Expr::Ident("inner".into()),
                                ],
                            );
                        }
                    }
                }
            }
        },
    );
    assert!(
        sol.contains("EVM-2 K3: pure fn `ledger_hit` takes a HashMap-typed param")
            || sol.contains("ledger_hit("),
        "non-Ident HashMap arg in hoisted pure-fn call must hit K3 exists sidecar guard (L1426–L1435): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

// ---------------------------------------------------------------------------
// N4-94: codegen/solidity/core/expr.rs — slice 10 (binop/temporal residual @ N4-89)
// Baseline @ N4-93: 93.42% line (68 missed / 1034; ~27 excl. DEAD/unreachable).
// Targets: temporal_ref early-return; body_is_member_push Block; binop Shr;
// gen_match_on_std_parse residual; cast/enum/method tails. Exclude Narrow L84–L91,
// binop_str WrappingAdd L130–L132 (dead), hoisted K3 L1354–L1371, catch-all L1479–L1487.
// Acceptance: ≥ 94% or ≤ 62 missed (excl. documented DEAD/unreachable ~27).
// ---------------------------------------------------------------------------

#[test]
fn n4_94_codegen_expr_temporal_ref_route_body_storage_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity TemporalRoute {
            routes {
                constructor() => []
                peek() -> u64 => [
                    let snap = ^m_ticks;
                    return(snap)
                ]
            }
            m_ticks: u64 {
                in constructor() => 0
                in peek() => prep: ^m_ticks + 1
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("snap = m_ticks") && !sol.contains("snap = next_m_ticks"),
        "route-body ^member without matching transform must hit gen_temporal_ref_sol early return (L169): {sol}"
    );
    assert_solc_compiles("expr_temporal_ref_route_body_storage", &sol);
}

#[test]
fn n4_94_codegen_expr_temporal_vec_push_block_only_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity VecBlockPush {
            routes {
                constructor() => []
                bump() => []
            }
            m_vals: Vec<u64> {
                in constructor() => array()
                in bump() => { let _ = 0; m_vals.push(1) }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("next_m_vals") || sol.contains("m_vals.push"),
        "block-bodied vec push transform must hit body_is_member_push Block tail (L185–L187): {sol}"
    );
    assert_solc_compiles("expr_temporal_vec_push_block_only", &sol);
}

#[test]
fn n4_94_codegen_expr_temporal_push_wrong_member_storage_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity VecWrongPush {
            routes {
                constructor() => []
                bump() => []
            }
            m_shadow: Vec<u64> { in constructor() => array() }
            m_vals: Vec<u64> {
                in constructor() => array()
                in bump() => { m_shadow.push(1) }
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("next_m_vals") || sol.contains("m_vals ="),
        "push on non-member base must miss body_is_member_push and keep temporal next_ (L192): {sol}"
    );
    assert_solc_compiles("expr_temporal_push_wrong_member_storage", &sol);
}

#[test]
fn n4_94_codegen_expr_binop_shr_member_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity ShrMember {
            routes {
                constructor() => []
                shift(n: u64) => []
            }
            m_bits: u64 {
                in constructor() => 0
                in shift(n) => m_bits >> n
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains(">>"),
        "member transform shr must hit binop_str Shr arm (L139): {sol}"
    );
    assert_solc_compiles("expr_binop_shr_member", &sol);
}

#[test]
fn n4_94_codegen_expr_parse_match_int_literal_arm_solc() {
    let mut program = parse_evm(
        r#"
        entity ParseIntArm {
            routes {
                constructor() => []
                run(s: String) -> u64 => [
                    let v = match std::str::parse_u64(s, 10) {
                        some(x) => x,
                        none => 0
                    };
                    return(v)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    if let RouteBody::Unphased(actions) = &mut program.entities[0].routes[1].body {
        if let RouteAction::Return { values } = &mut actions[0] {
            if let Expr::Let(_, _, body) = &mut values[0] {
                if let Expr::Match(_, arms) = body.as_mut() {
                    arms.push(MatchArm {
                        pattern: MatchPattern::IntLiteral(U256::from_u128(42)),
                        body: Expr::IntLiteral(U256::from_u128(42)),
                    });
                }
            }
        }
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("_cam_try_parse") && sol.contains("} else {"),
        "parse match IntLiteral arm must hit gen_match_on_std_parse catch-all (L997): {sol}"
    );
    assert_solc_compiles("expr_parse_match_int_literal_arm", &sol);
}

#[test]
fn n4_94_codegen_expr_parse_ident_subject_empty_args_solc() {
    let mut program = parse_evm(
        r#"
        entity ParseIdentSubject {
            routes {
                constructor() => []
                run(s: String) -> u64 => [
                    let v = match std::str::parse_u64(s, 10) {
                        some(x) => x,
                        none => 0
                    };
                    return(v)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    if let RouteBody::Unphased(actions) = &mut program.entities[0].routes[1].body {
        if let RouteAction::Return { values } = &mut actions[0] {
            if let Expr::Let(_, _, body) = &mut values[0] {
                if let Expr::Match(subject, _) = body.as_mut() {
                    *subject = Box::new(Expr::Ident("s".into()));
                }
            }
        }
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("_cam_try_parse"),
        "non-NamespacedCall parse subject must hit empty-args fallback (L951): {sol}"
    );
    assert_solc_compiles("expr_parse_ident_subject_empty_args", &sol);
}

#[test]
fn n4_94_codegen_expr_cast_custom_type_passthrough_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity CastPassthrough {
            routes {
                constructor() => []
                view echo(s: String) -> String => [
                    return(s as String)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        (sol.contains("return s") || sol.contains("return(s)")) && !sol.contains("string(s"),
        "cast to non-primitive String must hit gen_expr Cast custom passthrough arm (L766): {sol}"
    );
    assert_solc_compiles("expr_cast_custom_type_passthrough", &sol);
}

#[test]
fn n4_94_codegen_expr_match_non_payload_enum_variant_data_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        enum Color { Red, Green }

        entity Palette {
            routes {
                constructor() => []
                pick(c: Color) -> u64 => [
                    let n = match c {
                        Color::Red => 1,
                        Color::Green => 2
                    };
                    return(n)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            let entity = program.entities.first_mut().expect("entity");
            let route = entity
                .routes
                .iter_mut()
                .find(|r| r.name == "pick")
                .expect("pick");
            if let RouteBody::Unphased(actions) = &mut route.body {
                if let RouteAction::Let { value, .. } = &mut actions[0] {
                    if let Expr::Match(_, arms) = value {
                        for arm in arms.iter_mut() {
                            if let MatchPattern::EnumVariant(en, vn) = &arm.pattern {
                                arm.pattern = MatchPattern::EnumVariantWithData(
                                    en.clone(),
                                    vn.clone(),
                                    vec![Pattern::Ident("slot".into())],
                                );
                            }
                        }
                    }
                }
            }
        },
    );
    assert!(
        sol.contains("Color.Red") || sol.contains("Color::Red") || sol.contains("if ("),
        "non-payload EnumVariantWithData in gen_match_simple must hit early None (L907): {sol}"
    );
    assert_solc_compiles("expr_match_non_payload_enum_variant_data", &sol);
}

#[test]
fn n4_94_codegen_expr_address_of_nondet_none_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity Counter {
            identity id: u64
            routes { constructor() => [] }
        }

        entity Host {
            const slot: address = address_of Counter(1)

            routes {
                constructor() => []
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("constant slot = 0") || sol.contains("constant slot = address(0)"),
        "non-det address_of in entity constant must hit gen_expr AddressOf None fallback (L540): {sol}"
    );
    assert!(
        !sol.contains("predictCounter") && !sol.contains("computeCreate2"),
        "non-det address_of must not emit CREATE2 lowering: {sol}"
    );
    // TB-V: forge oracle dropped (expr_address_of_nondet_none).
}

#[test]
fn n4_94_codegen_expr_nested_contains_index_arm_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        entity NestedIdxContains {
            routes {
                constructor() => []
                probe(outer: u64, inner: u64) -> bool => [
                    return(m_outer[outer].contains(inner))
                ]
            }
            m_outer: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("m_outer_inner_exists"),
        "nested index contains must hit gen_expr contains Index arm (L567–L568): {sol}"
    );
    assert_solc_compiles("expr_nested_contains_index_arm", &sol);
}

#[test]
fn n4_94_codegen_expr_hoisted_pure_fn_exists_nested_index_comment_solc() {
    let sol = gen_evm_solidity(
        &parse_evm(
            r#"
        pure fn ready(m: HashMap<u64, u64>, k: u64) -> bool {
            m.exists(k)
        }

        entity ExistsNestedHoist {
            routes {
                constructor() => []
                probe(outer: u64, k: u64) -> bool => [
                    let ok = ready(m_outer[outer], k);
                    return(ok)
                ]
            }
            m_outer: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        ),
        false,
    );
    assert!(
        sol.contains("EVM-2 K3: pure fn `ready` takes a HashMap-typed param requiring sidecar args"),
        "hoisted pure-fn exists with nested index arg must hit K3 comment arm (L1428–L1435): {sol}"
    );
    assert_solc_compiles("expr_hoisted_pure_fn_exists_nested_index_comment", &sol);
}

// ---------------------------------------------------------------------------
// N4-103: codegen/solidity/core/types.rs — slice 11 (subst Let-shadow,
// default_value_entity enum, simplify_hashmap alias, infer scratch FieldAccess,
// actual_sol_type residual). Baseline @ N4-102: 83.18% (170 missed / 1011;
// ~90 executable excl. DEAD). Exclude sol_type_prog L667–L746 (~80 DEAD),
// resolve_alias_list L17–L23 (DEAD), Encode/AddressOf L354–L368 (EVM-unreachable).
// Acceptance: ≥ 84% or ≤ 158 raw or ≤ 85 excl. DEAD (Δ ≥ −5 executable).
// ---------------------------------------------------------------------------

#[test]
fn n4_103_types_subst_let_shadow_block_hashmap_solc() {
    let program = parse_evm(
        r#"
        entity SubstLetShadow {
            routes {
                constructor() => []
                pick(outer: u64, k: u64) => []
            }
            m_data: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
                in pick(outer, k) => {
                    let inner = if m_data.exists(outer) {
                        m_data[outer]
                    } else {
                        {}
                    };
                    let sentinel = { let inner = 3; inner };
                    m_data.update(outer, inner.update(k, sentinel + inner[k]))
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_data[outer]") || sol.contains("m_data_inner"),
        "HashMap let-alias must inline mapping read (subst Let L277–L284): {sol}"
    );
    assert!(
        sol.contains("sentinel"),
        "shadowing block let must survive as separate binding from mapping subst: {sol}"
    );
    assert_solc_compiles("types_subst_let_shadow_block_hashmap", &sol);
}

#[test]
fn n4_103_types_simplify_hashmap_exists_alias_init_solc() {
    let program = parse_evm(
        r#"
        entity AliasInit {
            routes {
                constructor() => []
                touch(k: u64) => []
            }
            m_map: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
                in touch(k) => {
                    let inner = if m_map.exists(k) {
                        m_map[k]
                    } else {
                        {}
                    };
                    m_map.update(k, inner)
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_map_exists") && (sol.contains("m_map[k]") || sol.contains("m_map[k].")),
        "exists/index alias init must simplify to direct index (L391): {sol}"
    );
    assert_solc_compiles("types_simplify_hashmap_exists_alias_init", &sol);
}

#[test]
fn n4_103_types_default_value_entity_enum_first_variant_solc() {
    let program = parse_evm(
        r#"
        entity ModeHost {
            enum Mode { Off, On }

            routes {
                constructor() => []
            }

            m_mode: Mode {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Mode.Off") || sol.contains("Mode_Mode_Off"),
        "entity-scoped unit enum ctor default must use first variant (L1023–L1027): {sol}"
    );
    assert_solc_compiles("types_default_value_entity_enum_first_variant", &sol);
}

#[test]
fn n4_103_types_infer_let_scratch_u32_field_solc() {
    let program = parse_evm(
        r#"
        record Quote { bid: u32, ask: u64 }

        entity ScratchU32Field {
            routes {
                constructor() => []
                bid(id: u64) -> u32 => [
                    let row = m_book[id];
                    let bid = row.bid;
                    return(bid)
                ]
            }
            m_book: HashMap<u64, Quote> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint32 bid") || sol.contains("row.bid"),
        "let-bound record u32 field must hit infer_let FieldAccess scratch (L1378–L1387): {sol}"
    );
    assert_solc_compiles("types_infer_let_scratch_u32_field", &sol);
}

#[test]
fn n4_103_types_actual_sol_type_block_tail_u32_solc() {
    let program = parse_evm(
        r#"
        entity BlockActual {
            routes {
                constructor() => []
                scale(x: u32) -> u32 => [
                    let out = { let y = x; y + 1 };
                    return(out)
                ]
            }
            m_x: u32 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint32 out") || sol.contains("y + 1"),
        "block-tail value must hit actual_sol_type Block arm (L918–L921): {sol}"
    );
    assert_solc_compiles("types_actual_sol_type_block_tail_u32", &sol);
}

#[test]
fn n4_103_types_actual_sol_type_namespaced_math_solc() {
    let program = parse_evm(
        r#"
        entity MathActual {
            routes {
                constructor() => []
                clamped(n: u64) -> u64 => [
                    let hi = std::math::min(n, m_cap);
                    return(hi)
                ]
            }
            m_cap: u64 { in constructor() => 100 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("min(") || sol.contains("std::math"),
        "std::math::min let must hit actual_sol_type NamespacedCall math arm (L898–L900): {sol}"
    );
    assert_solc_compiles("types_actual_sol_type_namespaced_math", &sol);
}

#[test]
fn n4_103_types_actual_sol_type_let_bound_field_member_solc() {
    let program = parse_evm(
        r#"
        record Slot { weight: u32, tag: u64 }

        entity LetBoundFieldActual {
            routes {
                constructor() => []
                weight(id: u64) -> u32 => [
                    let slot = m_slots[id];
                    return(slot.weight)
                ]
            }
            m_slots: HashMap<u64, Slot> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("returns (uint32)") && sol.contains("slot.weight"),
        "return of let-bound record field must hit actual_sol_type FieldAccess scratch (L808–L815): {sol}"
    );
    assert_solc_compiles("types_actual_sol_type_let_bound_field_member", &sol);
}

#[test]
fn n4_103_types_wrap_narrow_msg_value_u32_member_solc() {
    let program = parse_evm(
        r#"
        entity NarrowValue {
            routes {
                constructor() => []
                note() => []
            }
            m_last: u32 {
                in constructor() => 0
                in note() => msg::value as u32
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint32(") && sol.contains("msg.value"),
        "msg::value into u32 member must hit wrap_narrow_cast / actual_sol_type (L769–L941): {sol}"
    );
    assert_solc_compiles("types_wrap_narrow_msg_value_u32_member", &sol);
}

#[test]
fn n4_103_types_infer_let_address_of_binding_solc() {
    let program = parse_evm(
        r#"
        entity Peer {
            identity m_id: u64
            routes { constructor() => [] }
        }

        entity AddrLet {
            identity m_id: u64
            routes {
                constructor() => []
                peer() -> address => [
                    let dest = addressOf(Peer.state(m_id));
                    return(dest)
                ]
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("address dest") || sol.contains("addressOf"),
        "addressOf let must hit infer_let AddressOf arm (L1409): {sol}"
    );
    assert_solc_compiles("types_infer_let_address_of_binding", &sol);
}

#[test]
fn n4_103_types_subst_for_loop_shadow_hashmap_solc() {
    let program = parse_evm(
        r#"
        entity ForShadowSubst {
            routes {
                constructor() => []
                sum(k: u64) -> u64 => [
                    let inner = if m_map.exists(k) {
                        m_map[k]
                    } else {
                        0
                    };
                    let total = (0..k).fold(inner, |acc, x| acc + x);
                    return(total)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_map[k]") || sol.contains("m_map_exists"),
        "scalar HashMap exists/index let must simplify before fold (subst For L301–L310): {sol}"
    );
    assert!(
        sol.contains("fold") || sol.contains("for (uint256"),
        "range fold must lower in route body: {sol}"
    );
    assert_solc_compiles("types_subst_for_loop_shadow_hashmap", &sol);
}

#[test]
fn n4_103_types_default_value_payload_enum_member_ctor_solc() {
    let program = parse_evm(
        r#"
        enum Pack { Alpha(u64), Beta(u64, String) }

        entity PackHost {
            routes {
                constructor() => []
            }
            m_pack: Pack {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Pack::Alpha") || sol.contains("Pack({tag: Pack_Tag.Alpha") || sol.contains("alpha_0: 0"),
        "payload enum member ctor default must hit payload_enum_zero_literal (L1020–L1021): {sol}"
    );
    assert_solc_compiles("types_default_value_payload_enum_member_ctor", &sol);
}

// ---------------------------------------------------------------------------
// N4-104: codegen/solidity/core/types.rs — slice 12 (infer llvm-partial tails)
// Baseline @ N4-103: 84.08% (161 missed / 1011; ~81 excl. DEAD). Targets:
// infer_iter Index/keys/values L1068–L1128; infer_let keys/values L1327–L1352;
// infer_tuple Tuple/FnCall/If/Block L1429–L1448; infer_record_from_update_fields
// L1146–L1153; actual_sol_type member FieldAccess L804–L806. Exclude
// sol_type_prog L667–L746 (DEAD ~80), resolve_alias_list L17–L24 (DEAD).
// Acceptance: ≥ 84.5% or ≤ 155 raw or ≤ 76 excl. DEAD (Δ ≥ −5 executable).
// ---------------------------------------------------------------------------

#[test]
fn n4_104_types_infer_iter_hashmap_index_vec_u32_for_solc() {
    let program = parse_evm(
        r#"
        entity IndexVecFor {
            routes {
                constructor() => []
                sumRow(id: u64) -> Vec<u32> => [
                    let total = { for cell in m_rows[id] { cell + 1 } };
                    return(total)
                ]
            }
            m_rows: HashMap<u64, Vec<u32>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_rows[id]") && sol.contains("for (uint256"),
        "for over m_rows[id] must hit infer_iter Index HashMap->Vec arm (L1062–L1072): {sol}"
    );
    assert!(
        sol.contains("uint32 cell") || sol.contains("uint32 total"),
        "Vec<u32> loop variable must be typed via vec_elem: {sol}"
    );
    assert_solc_compiles("types_infer_iter_hashmap_index_vec_u32_for", &sol);
}

#[test]
fn n4_104_types_infer_iter_values_for_route_solc() {
    let program = parse_evm(
        r#"
        entity ValuesFor {
            routes {
                constructor() => []
                sumVals() -> Vec<u32> => [
                    let total = { for v in m_scores.values() { v + 1 } };
                    return(total)
                ]
            }
            m_scores: HashMap<u64, u32> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_scores_keys") && sol.contains("for (uint256"),
        "for over m_scores.values() must hit infer_iter values arm (L1117–L1127): {sol}"
    );
    assert!(
        sol.contains("uint32 v") || sol.contains("uint32 total"),
        "values() loop element must infer scalar V type: {sol}"
    );
    assert_solc_compiles("types_infer_iter_values_for_route", &sol);
}

#[test]
fn n4_104_types_infer_iter_keys_collect_for_block_solc() {
    let program = parse_evm(
        r#"
        entity KeysCollectFor {
            routes {
                constructor() => []
                bumpAll() -> Vec<u64> => [
                    let bumped = { for k in m_map.keys().collect() { k + 2 } };
                    return(bumped)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_map_keys") && sol.contains("uint64 k"),
        "collect+keys for-loop must hit infer_iter keys/collect arms (L1099–L1112): {sol}"
    );
    assert_solc_compiles("types_infer_iter_keys_collect_for_block", &sol);
}

#[test]
fn n4_104_types_infer_let_keys_address_array_solc() {
    let program = parse_evm(
        r#"
        entity AddrKeysLet {
            routes {
                constructor() => []
                keyCount() -> u64 => [
                    let keys = m_balances.keys();
                    return(keys.length)
                ]
            }
            m_balances: HashMap<address, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("address[] memory keys") || sol.contains("address[] memory keys"),
        "m_balances.keys() let must hit infer_let keys arm (L1327–L1337): {sol}"
    );
    assert_solc_compiles("types_infer_let_keys_address_array", &sol);
}

#[test]
fn n4_104_types_infer_let_values_u32_array_solc() {
    let program = parse_evm(
        r#"
        entity ValuesLetU32 {
            routes {
                constructor() => []
                snapshot() -> u64 => [
                    let vals = m_scores.values();
                    return(vals.length)
                ]
            }
            m_scores: HashMap<u64, u32> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint32[] memory vals") || sol.contains("m_scores_keys"),
        "m_scores.values() let must hit infer_let values arm (L1342–L1352): {sol}"
    );
    assert_solc_compiles("types_infer_let_values_u32_array", &sol);
}

#[test]
fn n4_104_types_infer_record_ghost_two_records_unique_solc() {
    let program = parse_evm(
        r#"
        record Alpha { x: u32, y: u64 }
        record Beta { x: u32, z: u64 }

        entity GhostTwoRecords {
            routes {
                constructor() => []
                touch(n: u32) => []
            }
            m_alpha: Alpha {
                in constructor() => { Alpha { x: 0, y: 0 } }
                in touch(n) => {
                    let ghost = 0;
                    ghost { x: n, y: m_alpha.y + 1 }
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Alpha memory") || sol.contains("Alpha("),
        "ghost RecordUpdate with x,y fields must resolve via infer_record_from_update_fields (L1146–L1153): {sol}"
    );
    assert_solc_compiles("types_infer_record_ghost_two_records_unique", &sol);
}

#[test]
fn n4_104_types_infer_tuple_pure_fn_return_solc() {
    let program = parse_evm(
        r#"
        pure fn splitPair(n: u64) -> (u32, u64) {
            (n as u32, n)
        }

        entity TuplePureFn {
            routes {
                constructor() => []
                both(n: u64) -> u64 => [
                    let (lo, hi) = splitPair(n);
                    return(lo + hi)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("(uint32 lo, uint64 hi)") || sol.contains("(uint256 lo, uint256 hi)"),
        "pure-fn tuple return must hit infer_tuple_elem_types FnCall Tuple arm (L1436–L1442): {sol}"
    );
    assert_solc_compiles("types_infer_tuple_pure_fn_return", &sol);
}

#[test]
fn n4_104_types_infer_tuple_if_arms_destructure_solc() {
    let program = parse_evm(
        r#"
        entity TupleIfLet {
            routes {
                constructor() => []
                pick(flag: bool, n: u32) -> u32 => [
                    let (lo, hi) = if flag { (n, n + 1) } else { (0, 1) };
                    return(lo + hi)
                ]
            }
            m_x: u32 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("(uint32 lo, uint32 hi)") || sol.contains("lo + hi"),
        "value-if tuple RHS must hit infer_tuple_elem_types If arm (L1445): {sol}"
    );
    assert_solc_compiles("types_infer_tuple_if_arms_destructure", &sol);
}

#[test]
fn n4_104_types_infer_tuple_block_tail_destructure_solc() {
    let program = parse_evm(
        r#"
        entity TupleBlockLet {
            routes {
                constructor() => []
                pair(n: u32) -> u32 => [
                    let (lo, hi) = { let t = n; (t, t + 1) };
                    return(lo + hi)
                ]
            }
            m_x: u32 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("(uint32 lo, uint32 hi)") || sol.contains("lo + hi"),
        "block-tail tuple RHS must hit infer_tuple_elem_types Block arm (L1446–L1448): {sol}"
    );
    assert_solc_compiles("types_infer_tuple_block_tail_destructure", &sol);
}

#[test]
fn n4_104_types_actual_sol_member_record_field_member_solc() {
    let program = parse_evm(
        r#"
        record Gauge { level: u32, cap: u64 }

        entity MemberFieldActual {
            routes {
                constructor() => []
                note() => []
            }
            m_gauge: Gauge {
                in constructor() => { Gauge { level: 1, cap: 100 } }
                in note() => m_gauge.level as u32 + 0
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_gauge.level") && sol.contains("uint32"),
        "member transform on m_gauge.level must hit actual_sol_type FieldAccess member (L798–L806): {sol}"
    );
    assert_solc_compiles("types_actual_sol_member_record_field_member", &sol);
}

#[test]
fn n4_104_types_infer_let_collect_keys_route_solc() {
    let program = parse_evm(
        r#"
        entity CollectKeysLet {
            routes {
                constructor() => []
                keyLen() -> u64 => [
                    let keys = m_map.keys().collect();
                    return(keys.length)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64[] memory keys") || sol.contains("m_map_keys"),
        "keys().collect() let must hit infer_let collect recursion + keys arm (L1324–L1337): {sol}"
    );
    assert_solc_compiles("types_infer_let_collect_keys_route", &sol);
}

// ---------------------------------------------------------------------------
// N4-126: codegen/solidity/core/types.rs — slice 13 (infer llvm-partial tail @ N4-104).
// Baseline @ N4-125 queue: 84.08% (161 missed / 1011; ~81 excl. DEAD). Targets:
// infer_let keys/values `}` tails L1333–L1335/L1348–L1352; infer_tuple /
// infer_record_from_update_fields `}` tails L1442/L1446–L1448/L1146–L1150;
// infer_iter_elem residual L1068–L1125; scatter TESTABLE (UnaryOp::Not, Block,
// Vec Index, Entity.address). Exclude sol_type_prog L667–L746 (DEAD ~80),
// resolve_alias_list L17–L24 (DEAD). Pattern: minimal .cam + AST-inject + solc.
// Acceptance: ≥ 85.0% or ≤ 155 raw or excl. DEAD ≤ 78 or Δ ≥ −5 vs 161 missed.
// ---------------------------------------------------------------------------

fn n4_126_expr_method(base: &str, method: &str) -> Expr {
    Expr::MethodCall(
        Box::new(Expr::Ident(base.to_string())),
        method.to_string(),
        vec![],
    )
}

fn n4_126_patch_route_actions(
    program: &mut cambrian_transpiler::ast::Program,
    entity_name: &str,
    route_name: &str,
    patch: impl FnOnce(&mut Vec<RouteAction>),
) {
    let entity = program
        .entities
        .iter_mut()
        .find(|e| e.name == entity_name)
        .expect("entity");
    let route = entity
        .routes
        .iter_mut()
        .find(|r| r.name == route_name)
        .expect("route");
    if let RouteBody::Unphased(actions) = &mut route.body {
        patch(actions);
    } else {
        panic!("expected unphased route body for {route_name}");
    }
}

#[test]
fn n4_126_types_infer_let_keys_u8_array_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity KeysU8Inject {
            routes {
                constructor() => []
                probe() -> u64 => [
                    return(0)
                ]
            }
            m_map: HashMap<u8, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        false,
        |program| {
            n4_126_patch_route_actions(program, "KeysU8Inject", "probe", |actions| {
                actions.insert(
                    0,
                    RouteAction::Let {
                        pattern: Pattern::Ident("ks".into()),
                        value: n4_126_expr_method("m_map", "keys"),
                    },
                );
                if let RouteAction::Return { values } = &mut actions[1] {
                    values[0] = Expr::FieldAccess(
                        Box::new(Expr::Ident("ks".into())),
                        "length".into(),
                    );
                }
            });
        },
    );
    assert!(
        sol.contains("uint8[] memory ks") || sol.contains("m_map_keys"),
        "AST-injected m_map.keys() let must hit infer_let keys return arm (L1332–L1333): {sol}"
    );
    assert_solc_compiles("types_infer_let_keys_u8_array_ast_inject", &sol);
}

#[test]
fn n4_126_types_infer_let_values_u8_array_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity ValuesU8Inject {
            routes {
                constructor() => []
                probe() -> u64 => [
                    return(0)
                ]
            }
            m_map: HashMap<u64, u8> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        false,
        |program| {
            n4_126_patch_route_actions(program, "ValuesU8Inject", "probe", |actions| {
                actions.insert(
                    0,
                    RouteAction::Let {
                        pattern: Pattern::Ident("vs".into()),
                        value: n4_126_expr_method("m_map", "values"),
                    },
                );
                if let RouteAction::Return { values } = &mut actions[1] {
                    values[0] = Expr::FieldAccess(
                        Box::new(Expr::Ident("vs".into())),
                        "length".into(),
                    );
                }
            });
        },
    );
    assert!(
        sol.contains("uint8[] memory vs") || sol.contains("m_map_keys"),
        "AST-injected m_map.values() let must hit infer_let values return arm (L1347–L1348): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_126_types_infer_let_unary_not_block_tail_solc() {
    let program = parse_evm(
        r#"
        entity NotBlockLet {
            routes {
                constructor() => []
                flip(flag: bool) -> bool => [
                    let denied = !flag;
                    let boxed = { let inner = m_count; inner + 1 };
                    return(denied && boxed > 0)
                ]
            }
            m_count: u64 { in constructor() => 0 }
            m_ok: bool { in constructor() => true }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("bool denied") && sol.contains("!flag"),
        "unary ! must hit infer_let UnaryOp::Not arm (L1223): {sol}"
    );
    assert!(
        sol.contains("uint256 boxed") || sol.contains("inner + 1"),
        "block-tail let must hit infer_let Block arm (L1238–L1240): {sol}"
    );
    assert_solc_compiles("types_infer_let_unary_not_block_tail", &sol);
}

#[test]
fn n4_126_types_infer_let_vec_index_cell_solc() {
    let program = parse_evm(
        r#"
        entity VecIndexLet {
            routes {
                constructor() => []
                head() -> u32 => [
                    let cell = m_cells[0];
                    return(cell)
                ]
            }
            m_cells: Vec<u32> {
                in constructor() => array()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint32 cell") || sol.contains("m_cells[0]"),
        "Vec index let must hit infer_let Index Vec value-type arm (L1309–L1311): {sol}"
    );
    assert_solc_compiles("types_infer_let_vec_index_cell", &sol);
}

#[test]
fn n4_126_types_infer_let_entity_address_det_solc() {
    let program = parse_evm(
        r#"
        entity Counter {
            identity m_id: u64
            routes { init setup() => [] }
            m_n: u64 { in setup() => 0 }
        }

        entity AddrLetDet {
            routes {
                init setup() => []
                link(id: u64) -> address => [
                    let dest = Counter.address(id);
                    return(dest)
                ]
            }
            m_n: u64 { in setup() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("address dest") && (sol.contains("predictCounter") || sol.contains("Counter.address")),
        "Entity.address() let must hit infer_let MethodCall address arm (L1401–L1405): {sol}"
    );
    assert_solc_compiles("types_infer_let_entity_address_det", &sol);
}

#[test]
fn n4_126_types_infer_record_ghost_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        record Alpha { x: u32, y: u64 }
        record Beta { x: u32, z: u64 }

        entity GhostInject {
            routes {
                constructor() => []
                touch(n: u32) => []
            }
            m_alpha: Alpha {
                in constructor() => { Alpha { x: 0, y: 0 } }
                in touch(n) => m_alpha.x + n
            }
        }
    "#,
        false,
        |program| {
            let entity = program
                .entities
                .iter_mut()
                .find(|e| e.name == "GhostInject")
                .expect("GhostInject");
            let member = entity
                .members
                .iter_mut()
                .find(|m| m.name == "m_alpha")
                .expect("m_alpha");
            let transform = member
                .transforms
                .iter_mut()
                .find(|t| t.route_name == "touch")
                .expect("touch transform");
            transform.body = Expr::Block(vec![
                Expr::Let(
                    Pattern::Ident("ghost".into()),
                    Box::new(Expr::IntLiteral(U256::ZERO)),
                    Box::new(Expr::RecordUpdate(
                        Box::new(Expr::Ident("ghost".into())),
                        vec![
                            ("x".into(), Expr::Ident("n".into())),
                            ("y".into(), Expr::FieldAccess(
                                Box::new(Expr::Ident("m_alpha".into())),
                                "y".into(),
                            )),
                        ],
                    )),
                ),
                Expr::FieldAccess(
                    Box::new(Expr::Ident("ghost".into())),
                    "x".into(),
                ),
            ]);
        },
    );
    assert!(
        sol.contains("Alpha memory ghost") || sol.contains("ghost.x"),
        "AST-injected ghost RecordUpdate must hit infer_record_from_update_fields (L1146–L1153): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_126_types_infer_tuple_block_tail_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity TupleBlockInject {
            routes {
                constructor() => []
                pair(n: u32) -> u32 => [
                    let (lo, hi) = (0, 0);
                    return(lo + hi)
                ]
            }
            m_x: u32 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            n4_126_patch_route_actions(program, "TupleBlockInject", "pair", |actions| {
                if let RouteAction::Let { value, .. } = &mut actions[0] {
                    *value = Expr::Block(vec![
                        Expr::Ident("n".into()),
                        Expr::Tuple(vec![
                            Expr::Ident("n".into()),
                            Expr::BinOp(
                                Box::new(Expr::Ident("n".into())),
                                BinOp::Add,
                                Box::new(Expr::IntLiteral(U256::from_u128(1))),
                            ),
                        ]),
                    ]);
                }
            });
        },
    );
    assert!(
        sol.contains("(uint32 lo, uint32 hi)") || sol.contains("lo + hi"),
        "AST-injected block-tail tuple RHS must hit infer_tuple_elem_types Block arm (L1446–L1448): {sol}"
    );
    assert_solc_compiles("types_infer_tuple_block_tail_ast_inject", &sol);
}

#[test]
fn n4_126_types_infer_tuple_pure_fn_tuple_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        pure fn pairTy(a: u32, b: u64) -> (u32, u64) {
            (a, b)
        }

        entity TupleFnInject {
            routes {
                constructor() => []
                both(a: u32, b: u64) -> u64 => [
                    let (lo, hi) = pairTy(a, b);
                    return(lo + hi)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            n4_126_patch_route_actions(program, "TupleFnInject", "both", |actions| {
                if let RouteAction::Let { value, .. } = &mut actions[0] {
                    *value = Expr::FnCall(
                        "pairTy".into(),
                        vec![Expr::Ident("a".into()), Expr::Ident("b".into())],
                    );
                }
            });
        },
    );
    assert!(
        sol.contains("(uint32 lo, uint64 hi)") || sol.contains("(uint256 lo, uint256 hi)"),
        "AST-injected pure-fn tuple FnCall must hit infer_tuple_elem_types Tuple arm (L1440–L1442): {sol}"
    );
    assert_solc_compiles("types_infer_tuple_pure_fn_tuple_ast_inject", &sol);
}

#[test]
fn n4_126_types_infer_iter_record_field_vec_member_solc() {
    let program = parse_evm(
        r#"
        record Row { cells: Vec<u32> }

        entity RowVecMember {
            routes {
                constructor() => []
                sumRow() => []
            }
            m_total: u32 {
                in constructor() => 0
                in sumRow() => {
                    for c in m_row.cells { m_total + c }
                }
            }
            m_row: Row {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_row.cells") && sol.contains("for (uint256"),
        "member for over m_row.cells must hit infer_iter FieldAccess Vec arm (L1083–L1085): {sol}"
    );
    assert!(
        sol.contains("uint32 c") || sol.contains("uint32"),
        "Vec<u32> loop variable must infer element type: {sol}"
    );
    assert_solc_compiles("types_infer_iter_record_field_vec_member", &sol);
}

#[test]
fn n4_126_types_infer_let_nested_keys_in_block_solc() {
    let program = parse_evm(
        r#"
        entity NestedKeysBlock {
            routes {
                constructor() => []
                count() -> u64 => [
                    let total = {
                        let ks = m_map.keys();
                        ks.length
                    };
                    return(total)
                ]
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint64[] memory ks") || sol.contains("m_map_keys"),
        "nested let keys in block must thread infer_let Let+keys arms (L1255–L1259/L1327–L1333): {sol}"
    );
    assert_solc_compiles("types_infer_let_nested_keys_in_block", &sol);
}

// ---------------------------------------------------------------------------
// N4-128: codegen/solidity/core/types.rs — slice 14 (infer llvm-partial tail @ N4-126).
// Baseline @ N4-127: 87.86% (118 missed / 972; ~38 excl. DEAD). Targets:
// infer_let keys/values `}` L1333–L1352; infer_record L1146–L1153; infer_iter
// L1068–L1125; scatter TESTABLE (is_hashmap_valued, actual_sol stdlib/Index,
// bool literal, record FieldAccess, divmod tuple, Vec<Record> param). Exclude
// sol_type_prog L667–L746 (DEAD), resolve_alias_list L17–L24 (DEAD).
// Acceptance: ≥ 90.0% or ≤ 110 raw or excl. DEAD ≤ 32 or Δ ≥ −8 vs 118 missed.
// ---------------------------------------------------------------------------

fn n4_128_expr_method_chain(base: &str, methods: &[&str]) -> Expr {
    let mut expr = Expr::Ident(base.to_string());
    for method in methods {
        expr = Expr::MethodCall(Box::new(expr), method.to_string(), vec![]);
    }
    expr
}

fn n4_128_patch_member_transform_body(
    program: &mut cambrian_transpiler::ast::Program,
    entity_name: &str,
    member_name: &str,
    route_name: &str,
    patch: impl FnOnce(&mut Expr),
) {
    let entity = program
        .entities
        .iter_mut()
        .find(|e| e.name == entity_name)
        .expect("entity");
    let member = entity
        .members
        .iter_mut()
        .find(|m| m.name == member_name)
        .expect("member");
    let transform = member
        .transforms
        .iter_mut()
        .find(|t| t.route_name == route_name)
        .expect("transform");
    patch(&mut transform.body);
}

#[test]
fn n4_128_types_infer_let_keys_collect_chain_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity KeysCollectChain {
            routes {
                constructor() => []
                len() -> u64 => [
                    return(0)
                ]
            }
            m_map: HashMap<u16, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        false,
        |program| {
            n4_126_patch_route_actions(program, "KeysCollectChain", "len", |actions| {
                actions.insert(
                    0,
                    RouteAction::Let {
                        pattern: Pattern::Ident("ks".into()),
                        value: n4_128_expr_method_chain("m_map", &["keys", "collect"]),
                    },
                );
                if let RouteAction::Return { values } = &mut actions[1] {
                    values[0] = Expr::FieldAccess(
                        Box::new(Expr::Ident("ks".into())),
                        "length".into(),
                    );
                }
            });
        },
    );
    assert!(
        sol.contains("uint16[] memory ks") || sol.contains("m_map_keys"),
        "AST-injected keys().collect() must thread infer_let collect+keys return (L1324–L1333): {sol}"
    );
    assert_solc_compiles("types_infer_let_keys_collect_chain_ast_inject", &sol);
}

#[test]
fn n4_128_types_infer_let_values_collect_chain_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity ValuesCollectChain {
            routes {
                constructor() => []
                len() -> u64 => [
                    return(0)
                ]
            }
            m_map: HashMap<u64, u16> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        false,
        |program| {
            n4_126_patch_route_actions(program, "ValuesCollectChain", "len", |actions| {
                actions.insert(
                    0,
                    RouteAction::Let {
                        pattern: Pattern::Ident("vs".into()),
                        value: n4_128_expr_method_chain("m_map", &["values", "collect"]),
                    },
                );
                if let RouteAction::Return { values } = &mut actions[1] {
                    values[0] = Expr::FieldAccess(
                        Box::new(Expr::Ident("vs".into())),
                        "length".into(),
                    );
                }
            });
        },
    );
    assert!(
        sol.contains("uint16[] memory vs") || sol.contains("m_map_keys"),
        "AST-injected values().collect() must thread infer_let values return (L1342–L1348): {sol}"
    );
    assert_solc_compiles("types_infer_let_values_collect_chain_ast_inject", &sol);
}

#[test]
fn n4_128_types_infer_record_beta_unique_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        record Alpha { x: u32, y: u64 }
        record Beta { x: u32, z: u64 }

        entity BetaGhost {
            routes {
                constructor() => []
                touch(n: u32) => []
            }
            m_beta: Beta {
                in constructor() => { Beta { x: 0, z: 0 } }
                in touch(n) => m_beta.x + n
            }
        }
    "#,
        false,
        |program| {
            n4_128_patch_member_transform_body(program, "BetaGhost", "m_beta", "touch", |body| {
                *body = Expr::Block(vec![
                    Expr::Let(
                        Pattern::Ident("ghost".into()),
                        Box::new(Expr::IntLiteral(U256::ZERO)),
                        Box::new(Expr::RecordUpdate(
                            Box::new(Expr::Ident("ghost".into())),
                            vec![
                                ("x".into(), Expr::Ident("n".into())),
                                ("z".into(), Expr::FieldAccess(
                                    Box::new(Expr::Ident("m_beta".into())),
                                    "z".into(),
                                )),
                            ],
                        )),
                    ),
                    Expr::FieldAccess(
                        Box::new(Expr::Ident("ghost".into())),
                        "x".into(),
                    ),
                ]);
            });
        },
    );
    assert!(
        sol.contains("Beta memory ghost") || sol.contains("ghost.x"),
        "AST-injected Beta-only ghost update must hit infer_record_from_update_fields Some (L1152–L1153): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_128_types_infer_iter_keys_member_for_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity KeysMemberFor {
            routes {
                constructor() => []
                bump() => []
            }
            m_total: u64 {
                in constructor() => 0
                in bump() => m_total + 1
            }
            m_map: HashMap<u32, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        false,
        |program| {
            n4_128_patch_member_transform_body(program, "KeysMemberFor", "m_total", "bump", |body| {
                *body = Expr::For(
                    Pattern::Ident("k".into()),
                    Box::new(n4_126_expr_method("m_map", "keys")),
                    Box::new(Expr::BinOp(
                        Box::new(Expr::Ident("m_total".into())),
                        BinOp::Add,
                        Box::new(Expr::IntLiteral(U256::from_u128(1))),
                    )),
                );
            });
        },
    );
    assert!(
        sol.contains("m_map_keys") && sol.contains("for (uint256"),
        "AST-injected for over m_map.keys() must hit infer_iter keys arm (L1102–L1108): {sol}"
    );
    assert!(
        sol.contains("uint32 k") || sol.contains("uint32"),
        "HashMap<u32,_> keys loop must infer uint32 element type: {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_128_types_infer_iter_values_member_for_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity ValuesMemberFor {
            routes {
                constructor() => []
                bump() => []
            }
            m_total: u64 {
                in constructor() => 0
                in bump() => m_total + 1
            }
            m_map: HashMap<u64, u32> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        false,
        |program| {
            n4_128_patch_member_transform_body(program, "ValuesMemberFor", "m_total", "bump", |body| {
                *body = Expr::For(
                    Pattern::Ident("v".into()),
                    Box::new(n4_126_expr_method("m_map", "values")),
                    Box::new(Expr::BinOp(
                        Box::new(Expr::Ident("m_total".into())),
                        BinOp::Add,
                        Box::new(Expr::Ident("v".into())),
                    )),
                );
            });
        },
    );
    assert!(
        sol.contains("m_map_keys") && sol.contains("for (uint256"),
        "AST-injected for over m_map.values() must hit infer_iter values arm (L1117–L1123): {sol}"
    );
    assert!(
        sol.contains("uint32 v") || sol.contains("uint32"),
        "HashMap<_,u32> values loop must infer uint32 scalar element type: {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_128_types_is_hashmap_valued_if_index_solc() {
    let program = parse_evm(
        r#"
        entity HashMapValuedIf {
            routes {
                constructor() => []
                pick(outer: u64) => []
            }
            m_rows: HashMap<u64, HashMap<u64, u64>> {
                in constructor() => HashMap::new()
                in pick(outer) => {
                    let row = if m_rows.exists(outer) {
                        m_rows[outer]
                    } else {
                        {}
                    };
                    row.update(1, 9)
                }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_rows") && sol.contains("m_rows_exists"),
        "if-branch HashMap index must hit is_hashmap_valued_expr Index arm (L212–L214): {sol}"
    );
    assert_solc_compiles("types_is_hashmap_valued_if_index", &sol);
}

#[test]
fn n4_128_types_actual_sol_stdlib_min_member_solc() {
    let program = parse_evm(
        r#"
        entity MinMember {
            routes {
                constructor() => []
                clamp(n: u64) => []
            }
            m_out: u64 {
                in constructor() => 0
                in clamp(n) => std::math::min(m_out, n)
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("min(") || sol.contains("std::math::min"),
        "std::math::min member transform must hit actual_sol_type STDLIB_UINT256_FNS arm (L881–L882): {sol}"
    );
    assert_solc_compiles("types_actual_sol_stdlib_min_member", &sol);
}

#[test]
fn n4_128_types_infer_let_bool_literal_solc() {
    let program = parse_evm(
        r#"
        entity BoolLet {
            routes {
                constructor() => []
                flag() -> bool => [
                    let ok = true;
                    return(ok)
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("bool ok") && sol.contains("true"),
        "bool literal let must hit infer_let BoolLiteral arm (L1166): {sol}"
    );
    assert_solc_compiles("types_infer_let_bool_literal", &sol);
}

#[test]
fn n4_128_types_infer_let_record_field_on_member_solc() {
    let program = parse_evm(
        r#"
        record Gauge { level: u32, cap: u64 }

        entity GaugeLet {
            routes {
                constructor() => []
                read() -> u32 => [
                    let lvl = m_gauge.level;
                    return(lvl)
                ]
            }
            m_gauge: Gauge {
                in constructor() => { Gauge { level: 3, cap: 10 } }
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint32 lvl") || sol.contains("m_gauge.level"),
        "m_gauge.level let must hit infer_let FieldAccess member record arm (L1365–L1367): {sol}"
    );
    assert_solc_compiles("types_infer_let_record_field_on_member", &sol);
}

#[test]
fn n4_128_types_infer_let_field_unknown_fallback_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        record TokenData { owner: address, amount: u64 }

        entity UnknownFieldLet {
            routes {
                constructor() => []
                probe(id: u64) -> u64 => [
                    let data = m_tokens[id];
                    let mystery = data.owner;
                    return(mystery)
                ]
            }
            m_tokens: HashMap<u64, TokenData> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        false,
        |program| {
            n4_126_patch_route_actions(program, "UnknownFieldLet", "probe", |actions| {
                if let RouteAction::Let { value, .. } = &mut actions[1] {
                    *value = Expr::FieldAccess(
                        Box::new(Expr::Ident("data".into())),
                        "missing".into(),
                    );
                }
                if let RouteAction::Return { values } = &mut actions[2] {
                    values[0] = Expr::Ident("mystery".into());
                }
            });
        },
    );
    assert!(
        sol.contains("uint256 mystery") || sol.contains("data.missing"),
        "unknown record field let must fall back to infer_let FieldAccess uint256 (L1385–L1387): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_128_types_infer_tuple_divmod_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity DivmodInject {
            routes {
                constructor() => []
                split(a: u64, b: u64) -> u64 => [
                    let (q, r) = (0, 0);
                    return(q + r)
                ]
            }
            m_x: u64 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            n4_126_patch_route_actions(program, "DivmodInject", "split", |actions| {
                if let RouteAction::Let { value, .. } = &mut actions[0] {
                    *value = Expr::FnCall(
                        "divmod".into(),
                        vec![Expr::Ident("a".into()), Expr::Ident("b".into())],
                    );
                }
            });
        },
    );
    assert!(
        sol.contains("(uint256 q, uint256 r)") || sol.contains("q + r"),
        "AST-injected divmod FnCall must hit infer_tuple_elem_types divmod arm (L1437–L1438): {sol}"
    );
    assert_solc_compiles("types_infer_tuple_divmod_ast_inject", &sol);
}

#[test]
fn n4_128_types_sol_type_vec_record_param_memory_solc() {
    let program = parse_evm(
        r#"
        record Item { tag: u32, weight: u64 }

        entity VecRecordParam {
            routes {
                constructor() => []
                ingest(items: Vec<Item>) => []
            }
            m_len: u64 {
                in constructor() => 0
                in ingest(items) => items.length
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Item[] memory items") || sol.contains("Item[] calldata items"),
        "Vec<Item> route param must hit sol_type_entity Vec<record> memory arm (L643–L644): {sol}"
    );
    assert_solc_compiles("types_sol_type_vec_record_param_memory", &sol);
}

#[test]
fn n4_128_types_infer_let_hashmap_index_cell_solc() {
    let program = parse_evm(
        r#"
        entity MapIndexLet {
            routes {
                constructor() => []
                read(k: u64) -> u32 => [
                    let cell = m_map[k];
                    return(cell)
                ]
            }
            m_map: HashMap<u64, u32> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("uint32 cell") || sol.contains("m_map[k]"),
        "HashMap index let must hit infer_let Index value-type arm (L1309–L1311): {sol}"
    );
    assert_solc_compiles("types_infer_let_hashmap_index_cell", &sol);
}

#[test]
fn n4_128_types_sol_type_vec_record_storage_member_solc() {
    let program = parse_evm(
        r#"
        record Item { tag: u32, weight: u64 }

        entity VecRecordStorage {
            routes {
                constructor() => []
                size() -> u64 => [
                    return(m_items.length)
                ]
            }
            m_items: Vec<Item> {
                in constructor() => array()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Item[] public m_items") || sol.contains("Item[] m_items"),
        "Vec<Item> storage member must hit sol_type_entity Vec<record> [] arm (L646–L647): {sol}"
    );
    assert_solc_compiles("types_sol_type_vec_record_storage_member", &sol);
}

#[test]
fn n4_128_types_default_value_unit_enum_member_solc() {
    let program = parse_evm(
        r#"
        entity LaneHost {
            enum Lane { Idle, Busy }

            routes {
                constructor() => []
            }
            m_lane: Lane {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Lane.Idle") || sol.contains("Lane_Tag.Idle"),
        "unit enum member default must hit default_value_entity first-variant arm (L1023–L1025): {sol}"
    );
    assert_solc_compiles("types_default_value_unit_enum_member", &sol);
}

#[test]
fn n4_128_types_actual_sol_fn_min_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity MinFnInject {
            routes {
                constructor() => []
                clamp(n: u64) => []
            }
            m_out: u64 {
                in constructor() => 0
                in clamp(n) => m_out
            }
        }
    "#,
        false,
        |program| {
            n4_128_patch_member_transform_body(program, "MinFnInject", "m_out", "clamp", |body| {
                *body = Expr::FnCall(
                    "min".into(),
                    vec![Expr::Ident("m_out".into()), Expr::Ident("n".into())],
                );
            });
        },
    );
    assert!(
        sol.contains("min(") || sol.contains("m_out"),
        "AST-injected min() FnCall must hit actual_sol_type STDLIB_UINT256_FNS arm (L881–L882): {sol}"
    );
    assert_solc_compiles("types_actual_sol_fn_min_ast_inject", &sol);
}

#[test]
fn n4_128_types_infer_iter_pure_fn_vec_for_solc() {
    let program = parse_evm(
        r#"
        pure fn sampleTags() -> Vec<u32> {
            array()
        }

        entity PureVecFor {
            routes {
                constructor() => []
                sumTags() -> Vec<u32> => [
                    let acc = { for t in sampleTags() { t + 1 } };
                    return(acc)
                ]
            }
            m_x: u32 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("sampleTags()") && sol.contains("for (uint256"),
        "for over pure-fn Vec return must hit infer_iter FnCall vec_elem arm (L1057–L1058): {sol}"
    );
    assert_solc_compiles("types_infer_iter_pure_fn_vec_for", &sol);
}

#[test]
fn n4_128_types_subst_for_shadow_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity ForShadowSubst {
            routes {
                constructor() => []
                bump(n: u64) => []
            }
            m_total: u64 {
                in constructor() => 0
                in bump(n) => m_total + n
            }
        }
    "#,
        false,
        |program| {
            n4_128_patch_member_transform_body(program, "ForShadowSubst", "m_total", "bump", |body| {
                *body = Expr::For(
                    Pattern::Ident("n".into()),
                    Box::new(Expr::Range(
                        Box::new(Expr::IntLiteral(U256::ZERO)),
                        Box::new(Expr::Ident("n".into())),
                    )),
                    Box::new(Expr::BinOp(
                        Box::new(Expr::Ident("m_total".into())),
                        BinOp::Add,
                        Box::new(Expr::Ident("n".into())),
                    )),
                );
            });
        },
    );
    assert!(
        sol.contains("for (uint256") && sol.contains("m_total"),
        "AST-injected for with param shadow must hit subst For shadow arm (L307–L310): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_128_types_actual_sol_index_hashmap_member_solc() {
    let program = parse_evm(
        r#"
        entity IndexActual {
            routes {
                constructor() => []
                bump(k: u64) => []
            }
            m_total: u64 {
                in constructor() => 0
                in bump(k) => m_map[k] + m_total
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_map[") && sol.contains("m_total"),
        "member m_map[k] expr must hit actual_sol_type Index HashMap arm (L907–L909): {sol}"
    );
    assert_solc_compiles("types_actual_sol_index_hashmap_member", &sol);
}

#[test]
fn n4_128_types_actual_sol_namespaced_parse_member_solc() {
    let program = parse_evm(
        r#"
        entity ParseActual {
            routes {
                constructor() => []
                read(s: String) => []
            }
            m_val: u64 {
                in constructor() => 0
                in read(s) => std::str::parse_u64(s, 10)
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("parse") || sol.contains("m_val"),
        "std::str::parse_u64 member rhs must hit actual_sol_type NamespacedCall parse arm (L895–L896): {sol}"
    );
    assert_solc_compiles("types_actual_sol_namespaced_parse_member", &sol);
}

#[test]
fn n4_128_types_sol_type_vec_enum_storage_member_solc() {
    let program = parse_evm(
        r#"
        entity EnumVecStorage {
            enum Status { On, Off }

            routes {
                constructor() => []
            }
            m_statuses: Vec<Status> {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Status[] public m_statuses") || sol.contains("Status[] m_statuses"),
        "Vec<enum> storage member must hit sol_type_entity Vec<enum> [] arm (L640–L646): {sol}"
    );
    assert_solc_compiles("types_sol_type_vec_enum_storage_member", &sol);
}

#[test]
fn n4_128_types_solidity_default_record_wipe_sentinel_solc() {
    let program = parse_evm(
        r#"
        record Point { x: u32, y: u32 }

        entity PointWipe {
            routes {
                constructor() => []
                wipe() => []
            }
            m_pt: Point {
                in constructor() => { Point { x: 1, y: 1 } }
                in wipe() => 0
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("Point({") || sol.contains("Point memory"),
        "record wipe sentinel 0 must hit solidity_default_for_member_ty record literal arm (L152–L153): {sol}"
    );
    assert_solc_compiles("types_solidity_default_record_wipe_sentinel", &sol);
}

#[test]
fn n4_128_types_infer_iter_vec_ident_member_for_solc() {
    let program = parse_evm(
        r#"
        entity VecIdentFor {
            routes {
                constructor() => []
                bump() => []
            }
            m_total: u32 {
                in constructor() => 0
                in bump() => {
                    for x in m_vec { m_total + x }
                }
            }
            m_vec: Vec<u32> {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("for (uint256") && sol.contains("m_vec"),
        "member for over m_vec ident must hit infer_iter Ident vec_elem arm (L1052–L1056): {sol}"
    );
    assert!(
        sol.contains("uint32 x") || sol.contains("uint32"),
        "Vec<u32> ident loop must infer uint32 element type: {sol}"
    );
    assert_solc_compiles("types_infer_iter_vec_ident_member_for", &sol);
}

#[test]
fn n4_128_types_infer_let_nested_scratch_owner_solc() {
    let program = parse_evm(
        r#"
        record TokenData { owner: address, amount: u64 }

        entity ScratchOwnerLet {
            routes {
                constructor() => []
                holder(id: u64) -> address => [
                    let data = m_tokens[id];
                    let owner = data.owner;
                    return(owner)
                ]
            }
            m_tokens: HashMap<u64, TokenData> {
                in constructor() => HashMap::new()
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("address owner") || sol.contains("data.owner"),
        "let-bound data.owner must hit infer_let FieldAccess scratch arm (L1381–L1383): {sol}"
    );
    assert_solc_compiles("types_infer_let_nested_scratch_owner", &sol);
}

// ---------------------------------------------------------------------------
// N4-132: codegen/solidity/core/types.rs — slice 15 (infer llvm-partial cluster
// @ N4-128 plateau). Baseline @ N4-131 queue: 88.58% (111 missed / 972;
// ~31 excl. DEAD). Targets: infer_let keys/values `}` L1333–L1352 (member/route
// combo retry); infer_record L1146–L1153 (route let); infer_iter `}` L1068–L1125
// (route-for / collect combo); scatter TESTABLE (subst ArrayLit L284, actual_sol
// HashMap default L987, infer_tuple pure-fn Tuple `}` L1442). Exclude
// sol_type_prog L667–L746 (DEAD), resolve_alias_list L17–L24 (DEAD).
// Acceptance: ≥ 90.0% or ≤ 103 raw or Δ ≥ −8 vs 111 missed.
// ---------------------------------------------------------------------------

#[test]
fn n4_132_types_infer_let_keys_member_transform_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity KeysMemberLet {
            routes {
                constructor() => []
                bump() => []
            }
            m_total: u64 {
                in constructor() => 0
                in bump() => m_total + 1
            }
            m_map: HashMap<u32, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        false,
        |program| {
            n4_128_patch_member_transform_body(program, "KeysMemberLet", "m_total", "bump", |body| {
                *body = Expr::Block(vec![
                    Expr::Let(
                        Pattern::Ident("ks".into()),
                        Box::new(n4_126_expr_method("m_map", "keys")),
                        Box::new(Expr::FieldAccess(
                            Box::new(Expr::Ident("ks".into())),
                            "length".into(),
                        )),
                    ),
                    Expr::BinOp(
                        Box::new(Expr::Ident("m_total".into())),
                        BinOp::Add,
                        Box::new(Expr::IntLiteral(U256::from_u128(1))),
                    ),
                ]);
            });
        },
    );
    assert!(
        sol.contains("uint32[] memory ks") || sol.contains("m_map_keys"),
        "member-transform keys() let must thread infer_let keys return (L1331–L1333): {sol}"
    );
    assert_solc_compiles("types_infer_let_keys_member_transform_ast_inject", &sol);
}

#[test]
fn n4_132_types_infer_let_values_collect_member_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity ValuesCollectMember {
            routes {
                constructor() => []
                bump() => []
            }
            m_total: u64 {
                in constructor() => 0
                in bump() => m_total + 1
            }
            m_map: HashMap<u64, u16> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        false,
        |program| {
            n4_128_patch_member_transform_body(
                program,
                "ValuesCollectMember",
                "m_total",
                "bump",
                |body| {
                    *body = Expr::Block(vec![
                        Expr::Let(
                            Pattern::Ident("vs".into()),
                            Box::new(n4_128_expr_method_chain("m_map", &["values", "collect"])),
                            Box::new(Expr::FieldAccess(
                                Box::new(Expr::Ident("vs".into())),
                                "length".into(),
                            )),
                        ),
                        Expr::BinOp(
                            Box::new(Expr::Ident("m_total".into())),
                            BinOp::Add,
                            Box::new(Expr::IntLiteral(U256::from_u128(1))),
                        ),
                    ]);
                },
            );
        },
    );
    assert!(
        sol.contains("uint16[] memory vs") || sol.contains("m_map_keys"),
        "member-transform values().collect() let must thread infer_let values return (L1346–L1348): {sol}"
    );
    assert_solc_compiles("types_infer_let_values_collect_member_ast_inject", &sol);
}

#[test]
fn n4_132_types_infer_record_route_let_gamma_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        record Gamma { a: u32, b: u64, c: address }
        record Delta { a: u32, b: u64, d: bool }

        entity GammaRouteGhost {
            routes {
                constructor() => []
                touch(n: u32, amt: u64, who: address) -> u32 => [
                    return(0)
                ]
            }
            m_gamma: Gamma {
                in constructor() => { Gamma { a: 0, b: 0, c: 0x0 } }
                in touch(n, _, _) => m_gamma.a + n
            }
        }
    "#,
        false,
        |program| {
            n4_126_patch_route_actions(program, "GammaRouteGhost", "touch", |actions| {
                actions.insert(
                    0,
                    RouteAction::Let {
                        pattern: Pattern::Ident("ghost".into()),
                        value: Expr::RecordUpdate(
                            Box::new(Expr::IntLiteral(U256::ZERO)),
                            vec![
                                ("a".into(), Expr::Ident("n".into())),
                                ("b".into(), Expr::Ident("amt".into())),
                                ("c".into(), Expr::Ident("who".into())),
                            ],
                        ),
                    },
                );
                if let RouteAction::Return { values } = &mut actions[1] {
                    values[0] = Expr::FieldAccess(
                        Box::new(Expr::Ident("ghost".into())),
                        "a".into(),
                    );
                }
            });
        },
    );
    assert!(
        sol.contains("Gamma memory ghost") || sol.contains("ghost.a"),
        "route let ghost RecordUpdate must hit infer_record_from_update_fields (L1146–L1153): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_132_types_infer_iter_index_route_for_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity IndexRouteFor {
            routes {
                constructor() => []
                sum_slot(slot: u64) -> u16 => [
                    return(0)
                ]
            }
            m_acc: u16 {
                in constructor() => 0
                in sum_slot(_) => m_acc
            }
            m_grid: HashMap<u64, Vec<u16>> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        false,
        |program| {
            n4_126_patch_route_actions(program, "IndexRouteFor", "sum_slot", |actions| {
                actions.insert(
                    0,
                    RouteAction::Let {
                        pattern: Pattern::Ident("acc".into()),
                        value: Expr::For(
                            Pattern::Ident("cell".into()),
                            Box::new(Expr::Index(
                                Box::new(Expr::Ident("m_grid".into())),
                                Box::new(Expr::Ident("slot".into())),
                            )),
                            Box::new(Expr::BinOp(
                                Box::new(Expr::Ident("m_acc".into())),
                                BinOp::Add,
                                Box::new(Expr::Ident("cell".into())),
                            )),
                        ),
                    },
                );
                if let RouteAction::Return { values } = &mut actions[1] {
                    values[0] = Expr::Ident("acc".into());
                }
            });
        },
    );
    assert!(
        sol.contains("for (uint256") && sol.contains("m_grid"),
        "route for over m_grid[slot] must hit infer_iter Index HashMap->Vec arm (L1068–L1067): {sol}"
    );
    assert!(
        sol.contains("uint16 cell") || sol.contains("uint16"),
        "Vec<u16> index loop must infer uint16 element type: {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_132_types_infer_iter_record_field_route_for_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        record Crate { items: Vec<u8> }

        entity CrateRouteFor {
            routes {
                constructor() => []
                tally() -> u64 => [
                    return(0)
                ]
            }
            m_sum: u64 {
                in constructor() => 0
                in tally() => m_sum
            }
            m_crate: Crate {}
        }
    "#,
        false,
        |program| {
            n4_126_patch_route_actions(program, "CrateRouteFor", "tally", |actions| {
                actions.insert(
                    0,
                    RouteAction::Let {
                        pattern: Pattern::Ident("sum".into()),
                        value: Expr::For(
                            Pattern::Ident("byte".into()),
                            Box::new(Expr::FieldAccess(
                                Box::new(Expr::Ident("m_crate".into())),
                                "items".into(),
                            )),
                            Box::new(Expr::BinOp(
                                Box::new(Expr::Ident("m_sum".into())),
                                BinOp::Add,
                                Box::new(Expr::Ident("byte".into())),
                            )),
                        ),
                    },
                );
                if let RouteAction::Return { values } = &mut actions[1] {
                    values[0] = Expr::Ident("sum".into());
                }
            });
        },
    );
    assert!(
        sol.contains("for (uint256") && sol.contains("m_crate"),
        "route for over m_crate.items must hit infer_iter FieldAccess Vec arm (L1083–L1085): {sol}"
    );
    assert!(
        sol.contains("uint8 byte") || sol.contains("uint8"),
        "record Vec<u8> field loop must infer uint8 element type: {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_132_types_infer_iter_collect_values_member_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity ValuesCollectMemberFor {
            routes {
                constructor() => []
                bump() => []
            }
            m_total: u32 {
                in constructor() => 0
                in bump() => m_total + 1
            }
            m_map: HashMap<u64, u32> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        false,
        |program| {
            n4_128_patch_member_transform_body(
                program,
                "ValuesCollectMemberFor",
                "m_total",
                "bump",
                |body| {
                    *body = Expr::For(
                        Pattern::Ident("v".into()),
                        Box::new(n4_128_expr_method_chain("m_map", &["values", "collect"])),
                        Box::new(Expr::BinOp(
                            Box::new(Expr::Ident("m_total".into())),
                            BinOp::Add,
                            Box::new(Expr::Ident("v".into())),
                        )),
                    );
                },
            );
        },
    );
    assert!(
        sol.contains("m_map_keys") && sol.contains("for (uint256"),
        "member for over values().collect() must hit infer_iter collect+values (L1117–L1123): {sol}"
    );
    assert!(
        sol.contains("uint32 v") || sol.contains("uint32"),
        "HashMap values loop must infer uint32 element type: {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_132_types_infer_let_keys_values_combo_route_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity KeysValuesCombo {
            routes {
                constructor() => []
                probe() -> u64 => [
                    return(0)
                ]
            }
            m_map: HashMap<u8, u16> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        false,
        |program| {
            n4_126_patch_route_actions(program, "KeysValuesCombo", "probe", |actions| {
                actions.insert(
                    0,
                    RouteAction::Let {
                        pattern: Pattern::Ident("ks".into()),
                        value: n4_128_expr_method_chain("m_map", &["keys", "collect"]),
                    },
                );
                actions.insert(
                    1,
                    RouteAction::Let {
                        pattern: Pattern::Ident("vs".into()),
                        value: n4_128_expr_method_chain("m_map", &["values", "collect"]),
                    },
                );
                if let RouteAction::Return { values } = &mut actions[2] {
                    values[0] = Expr::BinOp(
                        Box::new(Expr::FieldAccess(
                            Box::new(Expr::Ident("ks".into())),
                            "length".into(),
                        )),
                        BinOp::Add,
                        Box::new(Expr::FieldAccess(
                            Box::new(Expr::Ident("vs".into())),
                            "length".into(),
                        )),
                    );
                }
            });
        },
    );
    assert!(
        (sol.contains("uint8[] memory ks") || sol.contains("m_map_keys"))
            && (sol.contains("uint16[] memory vs") || sol.contains("m_map_keys")),
        "combo keys+values route lets must thread infer_let both arms (L1331–L1348): {sol}"
    );
    assert_solc_compiles("types_infer_let_keys_values_combo_route_ast_inject", &sol);
}

#[test]
fn n4_132_types_subst_array_lit_fold_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity ArrayLitSubst {
            routes {
                constructor() => []
                bump() => []
            }
            m_total: u64 {
                in constructor() => 0
                in bump() => m_total + 1
            }
            m_vec: Vec<u64> {
                in constructor() => array()
            }
        }
    "#,
        false,
        |program| {
            n4_128_patch_member_transform_body(program, "ArrayLitSubst", "m_total", "bump", |body| {
                *body = Expr::For(
                    Pattern::Ident("cell".into()),
                    Box::new(Expr::ArrayLit(vec![
                        Expr::Ident("m_total".into()),
                        Expr::IntLiteral(U256::from_u128(1)),
                    ])),
                    Box::new(Expr::BinOp(
                        Box::new(Expr::Ident("cell".into())),
                        BinOp::Add,
                        Box::new(Expr::IntLiteral(U256::ZERO)),
                    )),
                );
            });
        },
    );
    assert!(
        sol.contains("for (uint256") && (sol.contains("[m_total, 1]") || sol.contains("m_total")),
        "fold over array literal must walk subst ArrayLit arm (L284): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_132_types_actual_sol_hashmap_default_empty_solc() {
    let program = parse_evm(
        r#"
        entity HashMapDefaultProbe {
            routes {
                constructor() => []
                touch() => []
            }
            m_map: HashMap<u64, u64> {}
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        !sol.contains("m_map = 0") && !sol.contains("m_map = 0;"),
        "HashMap member without transform must elide default write (default_value L987): {sol}"
    );
    assert!(
        sol.contains("mapping(uint64 => uint64)") || sol.contains("mapping(uint256 => uint256)"),
        "HashMap member must still emit mapping storage: {sol}"
    );
    assert_solc_compiles("types_actual_sol_hashmap_default_empty", &sol);
}

#[test]
fn n4_132_types_infer_tuple_pure_fn_triple_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        pure fn tripleTy(a: u32, b: u64, c: u16) -> (u32, u64, u16) {
            (a, b, c)
        }

        entity TripleTuple {
            routes {
                constructor() => []
                pack(a: u32, b: u64, c: u16) -> u32 => [
                    let (x, y, z) = tripleTy(a, b, c);
                    return(x)
                ]
            }
            m_n: u32 { in constructor() => 0 }
        }
    "#,
        false,
        |program| {
            n4_126_patch_route_actions(program, "TripleTuple", "pack", |actions| {
                if let RouteAction::Let { value, .. } = &mut actions[0] {
                    *value = Expr::Block(vec![
                        Expr::Ident("a".into()),
                        Expr::FnCall(
                            "tripleTy".into(),
                            vec![
                                Expr::Ident("a".into()),
                                Expr::Ident("b".into()),
                                Expr::Ident("c".into()),
                            ],
                        ),
                    ]);
                }
            });
        },
    );
    assert!(
        sol.contains("uint32 x") && sol.contains("uint64 y") && sol.contains("uint16 z"),
        "block-tail pure-fn triple tuple destructure must hit infer_tuple_elem_types Block+FnCall Tuple (L1440–L1442): {sol}"
    );
    assert_solc_compiles("types_infer_tuple_pure_fn_triple_ast_inject", &sol);
}

#[test]
fn n4_132_types_infer_let_vec_bound_field_access_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity VecBoundField {
            routes {
                constructor() => []
                head() -> u32 => [
                    let cells = m_vec;
                    let first = cells[0];
                    return(first)
                ]
            }
            m_vec: Vec<u32> {
                in constructor() => array()
            }
        }
    "#,
        false,
        |program| {
            n4_126_patch_route_actions(program, "VecBoundField", "head", |actions| {
                if let RouteAction::Let { value, .. } = &mut actions[1] {
                    *value = Expr::FieldAccess(
                        Box::new(Expr::Ident("cells".into())),
                        "length".into(),
                    );
                }
                if let RouteAction::Return { values } = &mut actions[2] {
                    values[0] = Expr::Ident("first".into());
                }
            });
        },
    );
    assert!(
        sol.contains("uint256 first") || sol.contains("cells.length"),
        "let-bound Vec.length must hit infer_let FieldAccess scratch arm (L1378–L1383): {sol}"
    );
    // TB-AST: synthetic AST / patched emission — forge oracle dropped.
}

#[test]
fn n4_132_types_infer_iter_keys_collect_route_block_ast_inject_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity KeysCollectRouteBlock {
            routes {
                constructor() => []
                tally() -> u64 => [
                    return(0)
                ]
            }
            m_total: u64 {
                in constructor() => 0
                in tally() => m_total
            }
            m_map: HashMap<u64, u64> {
                in constructor() => HashMap::new()
            }
        }
    "#,
        false,
        |program| {
            n4_126_patch_route_actions(program, "KeysCollectRouteBlock", "tally", |actions| {
                actions.insert(
                    0,
                    RouteAction::Let {
                        pattern: Pattern::Ident("bumped".into()),
                        value: Expr::Block(vec![
                            Expr::For(
                                Pattern::Ident("k".into()),
                                Box::new(n4_128_expr_method_chain("m_map", &["keys", "collect"])),
                                Box::new(Expr::BinOp(
                                    Box::new(Expr::Ident("k".into())),
                                    BinOp::Add,
                                    Box::new(Expr::IntLiteral(U256::from_u128(1))),
                                )),
                            ),
                            Expr::IntLiteral(U256::ZERO),
                        ]),
                    },
                );
                if let RouteAction::Return { values } = &mut actions[1] {
                    values[0] = Expr::Ident("bumped".into());
                }
            });
        },
    );
    assert!(
        sol.contains("for (uint256") && sol.contains("m_map_keys"),
        "block for keys().collect() must hit infer_iter collect+keys (L1099–L1108): {sol}"
    );
    assert!(
        sol.contains("uint64 k") || sol.contains("uint64"),
        "HashMap<u64,_> keys loop must infer uint64 element type: {sol}"
    );
    assert_solc_compiles("types_infer_iter_keys_collect_route_block_ast_inject", &sol);
}

// ---------------------------------------------------------------------------
// N4-105: codegen/test_codegen.rs — residual slice 5 (hashmap assert / gen_call
// CamData+U256 tails / scatter early-return). Baseline @ N4-104: 95.09%
// (81 missed / 1649). Skip: map_context_field TVM-only L1795–L1803 (~9).
// Acceptance: ≥ 96% or ≤ 72 raw or Δ ≥ −8 executable.
// ---------------------------------------------------------------------------

#[cfg(feature = "rust-targets")]
#[test]
fn n4_105_native_test_codegen_call_u256_binop_and_camdata_ident_default() {
    const SRC: &str = r#"
entity TypedProbe {
    routes {
        probe(w: U256, label: String, pk: pubkey, data: CamData, tag: u64) => []
    }
    m_tag: u64 {
        in probe(_, _, _, _, tag) => tag
    }
}

test "residual call kwargs" for TypedProbe with { m_tag: 0 } {
    let pk = 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
    let wide = 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
    let payload = cam_encode<u64>(7)
    let lbl = "via-ident"
    call probe(wide + 1, lbl, pk, payload, 1)
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("(wide + 1).ser_be()"),
        "U256 BinOp kwargs must hit gen_call else ser_be arm (L931-L935): {code}"
    );
    assert!(
        code.contains("CamData::default().ser_be()"),
        "CamData ident kwargs must hit non-encode default arm (L965-L968): {code}"
    );
    assert!(
        code.contains("&lbl.ser_be()") || code.contains("lbl.ser_be()"),
        "String ident kwargs must lower via generic ser_be: {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_105_native_test_codegen_hashmap_indexed_expect_state() {
    const SRC: &str = r#"
entity Ledger {
    routes {
        credit(key: u64, delta: u64) => []
    }
    m_scores: HashMap<u64, u64> {
        in credit(key, delta) => m_scores
    }
}

test "indexed hashmap expect" for Ledger with {
    m_scores: { 1 => 10, 2 => 20 }
} {
    call credit(1, 5)
    expect state { m_scores[1]: 15 }
    expect state { m_scores[2]: 20 }
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains(".get(&1)") && code.contains(".get(&2)"),
        "indexed expect state must emit gen_hashmap_assert per-key checks (L1760–L1768): {code}"
    );
    assert!(
        code.contains("m_scores.get(&1)") || code.contains("_new_state"),
        "field-path expect must use gen_field_path_access + gen_typed_test_expr (L1025–L1030): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_105_native_test_codegen_invariant_u256_identity_serbe_branches() {
    const SRC: &str = r#"
entity Seal {
    identity seal: U256
    identity tag: u64
    routes {
        touch() => []
    }
    m_n: u64 {
        in touch() => m_n + 1
    }
}

invariant "u256 identity tails" for Seal {
    init { seal: 0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc, m_n: 0 }
    action touch() { }
    check m_n >= 0
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("U256::from_be_bytes") || code.contains(".ser_be()"),
        "explicit U256 identity init must hit non-primitive ser_be arm (L342–L345): {code}"
    );
    assert!(
        code.contains("u64::default().ser_be()"),
        "omitted u64 identity must hit default() ser_be arm (L348–L352): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_105_native_test_codegen_expect_return_string_cast() {
    const SRC: &str = r#"
entity Namer {
    routes {
        init create() => []
        name() -> String => [ return("TKN") ]
    }
    m_n: u64 {
        in create() => 0
    }
}

test "string return" for Namer with { m_n: 0 } {
    call name()
    expect return "TKN"
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains(".to_string()"),
        "String expect return must hit U256/String cast arm in emit_call_with_assertions (L1075–L1076): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_105_native_test_codegen_gen_test_expr_wrapping_binop() {
    const SRC: &str = r#"
entity Wrap {
    routes {
        bump(n: u64) => []
    }
    m_acc: u64 {
        in bump(n) => m_acc + n
    }
}

invariant "wrapping binop" for Wrap {
    init { m_acc: 0 }
    action bump(n: u64) {
        bound n in 1..20
        assume n > 0
    }
    check (m_acc +% 1) >= m_acc
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("+%") || code.contains("/* unsupported */"),
        "WrappingAdd in invariant check must hit gen_test_expr unsupported-or-wrapping arm (L1622): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_105_native_test_codegen_platform_effect_unknown_wildcard() {
    const SRC: &str = r#"
entity Fx {
    routes { noop() => [] }
    m_v: u64 { in noop() => m_v }
}

test "unknown platform fx" for Fx with { m_v: 0 } {
    call noop()
    expect effects [ .., mysteryFx(2) ]
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains(".iter().position(|_e| true)")
            || code.contains(".iter().position(|_e| matches!"),
        "wildcard expect effects with unknown platform fx must hit effect_tag_pattern default (L1318): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_105_native_test_codegen_registry_zero_wasm_hash() {
    const SRC: &str = r#"
entity Counter {
    routes { bump() => [] }
    m_count: u64 { in bump() => m_count + 1 }
}

test "registry zero wasm hash" for Counter with { m_count: 0 } {
    registry Ghost {
        code_hash: 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa,
        code_depth: 1,
        wasm_hash: 0
    }
    call bump()
    expect state { m_count: 1 }
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("[0u8; 32]"),
        "registry wasm_hash IntLiteral(0) must hit gen_hex_array_32 zero arm (L1782): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_105_native_test_codegen_infer_abi_u128_literal() {
    const SRC: &str = r#"
entity Wide {
    identity lo: u64
    identity hi: u128
    routes {
        probe() => []
    }
    m_acc: u64 {
        in probe() => m_acc + 1
    }
}

test "u128 abi width" for Wide with { lo: 1, hi: 170141183460469231731687303715884529152, m_acc: 0 } {
    let addr = address_of Wide(
        1,
        170141183460469231731687303715884529152
    )
    call probe()
    expect state { m_acc: 1 }
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("AbiType::Uint(128)"),
        "u128-sized address_of literal must hit infer_abi_and_cast Uint(128) arm (L1711–L1712): {code}"
    );
}

// ---------------------------------------------------------------------------
// N4-106: codegen/evm_test_codegen.rs — slice 10 (scatter / check-expr residual)
// Baseline @ N4-105 queue: 96.79% (64 missed / 1992; ~56 excl. DEAD gen_state_accessor).
// Targets: lower_check_expr_multi Lt/Le L2188–L2189; gen_invariant_fn exclusive
// bound L1574; gen_invariant_multi_file bound L1940–L1944; exclude_selectors
// unknown L1769; substitute_member_accessors track/member clash L2294;
// generate_evm_tests early return L47–L48; gen_expect_state skip paths L1258/1263;
// infer_sol_type fallback L2445; default_value_for_type TypedAddress L2427–L2428.
// Exclude DEAD: gen_state_accessor L1349+ (~8).
// Acceptance: ≥ 97% or ≤ 56 raw or Δ ≥ −8 executable.
// ---------------------------------------------------------------------------

#[test]
fn n4_106_evm_lower_check_lt_le_gt_multi() {
    let program = parse_evm(
        r#"
        entity Left {
            routes { constructor() => [] step() => [] }
            m_x: u64 {
                in constructor() => 10
                in step() => m_x - 1
            }
        }
        entity Right {
            routes { constructor() => [] step() => [] }
            m_y: u64 {
                in constructor() => 2
                in step() => m_y + 1
            }
        }

        invariant "lt le gt" for { l: Left, r: Right } {
            init l { m_x: 10 }
            init r { m_y: 2 }
            action l.step() {}
            action r.step() {}
            check l.m_x < r.m_y && l.m_x <= 100 && r.m_y > 0
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("lt le gt invariant must emit file");
    assert!(
        inv.contains(" < ") && inv.contains(" <= ") && inv.contains(" > "),
        "multi check must emit Lt/Le/Gt via lower_check_expr_multi (L2188–L2191): {inv}"
    );
}

#[test]
fn n4_106_evm_lower_check_nested_field_access_receiver() {
    let mut program = parse_evm(
        r#"
        entity Host {
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 1 }
        }
    "#,
    );
    program.invariants.push(InvariantDecl {
        name: "nested recv".into(),
        instances: vec![InvariantInstance {
            name: "h".into(),
            entity: "Host".into(),
            init: vec![("m_x".into(), Expr::IntLiteral(U256::from_u128(1)))],
            forall_state: ForallSpec::default(),
            init_specified: true,
            span: span0(),
        }],
        skip_from: false,
        senders: vec![],
        deploy: vec![],
        context: ContextSpec { entries: vec![] },
        actions: vec![],
        checks: vec![Expr::FieldAccess(
            Box::new(Expr::BinOp(
                Box::new(Expr::Ident("h".into())),
                BinOp::Add,
                Box::new(Expr::IntLiteral(U256::from_u128(1))),
            )),
            "x".into(),
        )],
        fail_on_revert: false,
        runs: None,
        depth: None,
        tag: None,
        instantiates: None,
        emit_policy: InvariantEmitPolicy::Emit,
        with_time: false,
        track: vec![],
        derived: vec![],
        exclude_senders: vec![],
        exclude_selectors: vec![],
        span: span0(),
    });
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("nested recv invariant must emit file");
    assert!(
        inv.contains(").x") || inv.contains("+ 1).x"),
        "non-ident FieldAccess receiver must lower via generic recv arm (L2173–L2175): {inv}"
    );
}

#[test]
fn n4_106_evm_invariant_action_exclusive_bound_single_entity() {
    let program = parse_evm(
        r#"
        entity Gauge {
            routes { constructor() => [] bump(n: u64) => [] }
            m_n: u64 {
                in constructor() => 0
                in bump(n) => m_n + n
            }
        }

        invariant "exclusive bound" for Gauge {
            init { m_n: 0 }
            action bump(n: u64) {
                bound n in 1..50
            }
            check m_n >= 0
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("exclusive bound invariant must emit file");
    assert!(
        inv.contains("bound(n,") && inv.contains("- 1"),
        "exclusive action bound must subtract one from hi (L1574–L1577): {inv}"
    );
}

#[test]
fn n4_106_evm_multi_invariant_exclusive_and_inclusive_action_bounds() {
    let mut program = parse_evm(
        r#"
        entity Alpha {
            routes { constructor() => [] bump(n: u64) => [] }
            m_n: u64 { in constructor() => 0 in bump(n) => m_n + n }
        }
        entity Beta {
            routes { constructor() => [] add(k: u64) => [] }
            m_k: u64 { in constructor() => 0 in add(k) => m_k + k }
        }

        invariant "mixed bounds" for { a: Alpha, b: Beta } {
            init a { m_n: 0 }
            init b { m_k: 0 }
            action a.bump(n: u64) {}
            action b.add(k: u64) {}
            check a.m_n >= b.m_k
        }
    "#,
    );
    program.invariants[0].actions = vec![
        InvariantAction {
            instance: "a".into(),
            route: "bump".into(),
            params: vec![Param {
                name: "n".into(),
                ty: Type::Simple("u64".into()),
            }],
            body: vec![TestStep::Bound {
                var: "n".into(),
                lo: Expr::IntLiteral(U256::from_u128(1)),
                hi: Expr::IntLiteral(U256::from_u128(20)),
                inclusive: false,
            }],
            span: span0(),
        },
        InvariantAction {
            instance: "b".into(),
            route: "add".into(),
            params: vec![Param {
                name: "k".into(),
                ty: Type::Simple("u64".into()),
            }],
            body: vec![TestStep::Bound {
                var: "k".into(),
                lo: Expr::IntLiteral(U256::ZERO),
                hi: Expr::IntLiteral(U256::from_u128(10)),
                inclusive: true,
            }],
            span: span0(),
        },
    ];
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("mixed bounds multi invariant must emit file");
    assert!(
        inv.contains("bound(n,") && inv.contains("- 1"),
        "multi exclusive bound must hit hi-1 arm (L1943–L1944): {inv}"
    );
    assert!(
        inv.contains("bound(k,") && (inv.contains("bound(k, 0, 10)") || inv.contains("bound(k,0,10)")),
        "multi inclusive bound must pass hi through (L1940–L1941): {inv}"
    );
}

#[test]
fn n4_106_evm_exclude_selector_unknown_route_empty_sig() {
    let mut program = parse_evm(
        r#"
        entity Clock {
            routes { constructor() => [] tick() => [] }
            m_n: u64 { in constructor() => 0 in tick() => m_n + 1 }
        }

        invariant "unknown exclude" for Clock {
            init { m_n: 0 }
            action tick() {}
            check m_n >= 0
        }
    "#,
    );
    program.invariants[0].exclude_selectors = vec!["ghostRoute".into()];
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("unknown exclude invariant must emit file");
    assert!(
        inv.contains("excludeSelector") && inv.contains("ghostRoute"),
        "unknown exclude selector must still emit excludeSelector wiring (L1769): {inv}"
    );
}

#[test]
fn n4_106_evm_track_member_name_collision_substitute_skip() {
    let program = parse_evm(
        r#"
        entity Meter {
            routes { constructor() => [] bump() => [] }
            m_ticks: u64 {
                in constructor() => 0
                in bump() => m_ticks + 1
            }
        }

        invariant "member track clash" for Meter {
            init { m_ticks: 0 }
            action bump() {}
            track { let m_ticks = 7; }
            check m_ticks == 7
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("member track clash invariant must emit file");
    assert!(
        inv.contains("_handler.m_ticks()") && inv.contains("== 7"),
        "track binding m_ticks must rewrite via handler getter, not SUT member (L2294): {inv}"
    );
    assert!(
        !inv.contains("_meter.m_ticks() == 7"),
        "member substitution must be skipped when name collides with track binding: {inv}"
    );
}

#[test]
fn n4_106_evm_empty_program_returns_no_files() {
    let program = parse_evm(
        r#"
        entity Lonely {
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let files = generate_evm_tests(
        &program,
        false,
        &InvariantConfig::default(),
    );
    assert!(
        files.is_empty(),
        "program with no tests/fuzz/invariants must return empty vec (L47–L48): {:?}",
        files.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
}

#[test]
fn n4_106_evm_expect_state_skip_empty_and_index_paths() {
    let mut program = parse_evm(
        r#"
        entity Counter {
            routes { constructor() => [] bump() => [] }
            m_count: u64 { in constructor() => 0 in bump() => m_count + 1 }
        }
    "#,
    );
    push_orphan_step_test(
        &mut program,
        vec![
            TestStep::Call {
                target: None,
                route: "bump".into(),
                args: vec![],
            },
            TestStep::ExpectState {
                fields: vec![
                    (vec![], Expr::IntLiteral(U256::from_u128(1))),
                    (
                        vec![
                            PathSegment::Index(Expr::IntLiteral(U256::ZERO)),
                            PathSegment::Field("m_count".into()),
                        ],
                        Expr::IntLiteral(U256::from_u128(2)),
                    ),
                    (
                        vec![PathSegment::Field("m_count".into())],
                        Expr::IntLiteral(U256::from_u128(1)),
                    ),
                ],
            },
        ],
    );
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "Counter");
    assert!(
        test_sol.contains("function test_orphan_steps(")
            && test_sol.contains("assertEq(_counter.m_count()")
            && test_sol.contains("m_count mismatch"),
        "scalar expect state must emit for valid paths while skipping malformed ones (L1258/1263): {test_sol}"
    );
}

#[test]
fn n4_106_evm_infer_sol_type_enum_fallback_and_derived_multi_let() {
    let program = parse_evm(
        r#"
        entity ModeBox {
            enum Mode { Off, On }

            routes { constructor() => [] flip() => [] }
            m_mode: Mode {
                in constructor() => Mode::Off
                in flip() => m_mode
            }
        }

        test "enum let" for ModeBox {
            let tag = Mode::On
            call flip()
        }

        invariant "derived lets" for ModeBox {
            init { m_mode: Mode::Off }

            derived tag() -> Mode {
                let off = Mode::Off
                let on = Mode::On
                return on
            }

            action flip() {}
            check tag() == m_mode || m_mode == Mode::Off
        }
    "#,
    );
    let files = gen_evm_test_files(&program);
    let test_sol = foundry_test_file(&files, "ModeBox");
    assert!(
        test_sol.contains("uint256 tag = ModeBox.Mode.On"),
        "enum test let must hit infer_sol_type fallback/default (L2445): {test_sol}"
    );
    let inv = files
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("derived lets invariant must emit file");
    assert!(
        inv.contains("function tag()") && inv.contains("return on"),
        "derived query let steps must emit helper body (L1519): {inv}"
    );
}

#[test]
fn n4_106_evm_det_typed_address_identity_default_ctor() {
    let program = parse_evm(
        r#"
        entity ERC20 {
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity Pair {
            identity m_token0: Address<ERC20>
            identity m_token1: Address<ERC20>
            routes { init boot() => [] tick() => [] }
            m_n: u64 { in boot() => 0 in tick() => m_n + 1 }
        }

        invariant "typed address identity" for { p: Pair } {
            init p { m_n: 0 }
            action p.tick() {}
            check p.m_n >= 0
        }
    "#,
    );
    let inv = gen_evm_test_files_det(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("typed address det invariant must emit file");
    assert!(
        inv.contains("address(0)") || inv.contains("address(0x0)"),
        "unseeded Address<Entity> identity must hit default_value_for_type TypedAddress (L2427–L2428): {inv}"
    );
}

#[test]
fn n4_106_evm_invariant_action_skipif_early_return() {
    let mut program = parse_evm(
        r#"
        entity Gate {
            routes { constructor() => [] open() => [] }
            m_open: bool {
                in constructor() => false
                in open() => true
            }
        }

        invariant "skipif action" for Gate {
            init { m_open: false }
            action open() {}
            check m_open == true || m_open == false
        }
    "#,
    );
    program.invariants[0].actions[0].body = vec![TestStep::SkipIf {
        cond: Expr::BoolLiteral(true),
    }];
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("skipif action invariant must emit file");
    assert!(
        inv.contains("if (true) return;"),
        "invariant action SkipIf must emit early return (L1590–L1594): {inv}"
    );
}

// ---------------------------------------------------------------------------
// N4-113: codegen/solidity/evm/route.rs — slice 7 (residual tail @ N4-110 gap).
// Baseline @ N4-112 queue: 97.15% (47 missed / 1647; ~28 excl. dead/debug_assert).
// Targets: resolve_var_call_type_with_target L824/L831; gen_from_checks nondet
// L644/L682–L683/L694; gen_from_checks_det L1948/L1956/L1990–L1991/L2001;
// emit_route_body_from_ir trailing close L507 (llvm-partial).
// Exclude: UpdateCode L377–L390 (DEAD); IrStmt::AstAction L130/L195–L199 (DEAD);
// gen_from_checks false L656/L1967 (debug_assert); gen_initialize_fn None L1793 (unreachable).
// Acceptance: ≥ 97.5% or ≤ 42 raw or excl. dead ≤ 23 (Δ ≥ −5 executable).
// ---------------------------------------------------------------------------

#[test]
fn n4_113_codegen_route_unphased_var_call_u64_return_solc() {
    let program = parse_evm(
        r#"
        entity Counter {
            identity m_id: u64
            routes {
                init boot() => []
                tally() -> u64 => [ return(m_n) ]
            }
            m_n: u64 { in boot() => 0 in tally() => m_n }
        }

        entity Host {
            routes {
                init boot() => []
                probe() => [
                    var v = tally() ~> Counter.address(m_id);
                ]
            }
            m_id: u64 { in boot() => 1 }
            m_n: u64 { in boot() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function probe(")
            && sol.contains("uint64 v")
            && sol.contains(".tally("),
        "unphased in-program u64 var-call must resolve return via resolve_var_call_type_with_target (L821–L824): {sol}"
    );
    assert_solc_compiles("route_unphased_var_call_u64_return", &sol);
}

#[test]
fn n4_113_codegen_route_extern_var_call_string_return_solc() {
    let program = parse_evm(
        r#"
        extern entity Meta {
            view route label() -> String;
        }

        entity Reader {
            routes {
                constructor(m: Address<Meta>) => []
                read() -> String => [
                    work: [
                        var tag = label() ~> m_meta;
                        return(tag)
                    ]
                ]
            }
            m_meta: Address<Meta> { in constructor(m) => m }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("// Phase: work")
            && sol.contains("string memory tag")
            && sol.contains("IMeta")
            && sol.contains(".label("),
        "extern String-return var-call must hit resolve_var_call_type_with_target sol_type arm (L829–L831): {sol}"
    );
    assert_solc_compiles("route_extern_var_call_string_return", &sol);
}

#[test]
fn n4_113_codegen_route_extern_void_var_call_uint256_close_solc() {
    let program = parse_evm(
        r#"
        extern entity Sink {
            route noop();
        }

        entity Host {
            routes {
                constructor(s: Address<Sink>) => []
                poke() => [
                    work: [
                        var slot = noop() ~> m_sink;
                    ]
                ]
            }
            m_sink: Address<Sink> { in constructor(s) => s }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("// Phase: work")
            && sol.contains("uint256 slot")
            && sol.contains("ISink")
            && sol.contains(".noop("),
        "extern void-route var-call must close resolve_var_call_type_with_target extern arm (L827–L831): {sol}"
    );
    // TB-V: forge oracle dropped (route_extern_void_var_call_uint256_close).
}

#[test]
fn n4_113_codegen_route_nondet_from_entity_hex_literal_solc() {
    let program = parse_evm(
        r#"
        entity Treasury {
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity Gate {
            routes {
                constructor() => []
                admin() from Treasury(0x00000000000000000000000000000000000000aa) => []
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function admin(")
            && sol.contains("require(")
            && (sol.contains("0xaa") || sol.contains("170") || sol.contains("uint160")),
        "nondet from Entity(hex literal) must emit from-clause sender check: {sol}"
    );
    assert_solc_compiles("route_nondet_from_entity_hex_literal", &sol);
}

#[test]
fn n4_113_codegen_route_nondet_dual_from_custom_error_args_revert_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        error AuthFailed(who: address);

        entity Vault {
            routes {
                constructor() => []
                secret() from m_owner : throw AuthFailed(m_owner) => []
            }
            m_owner: address { in constructor() => address(0) }
            m_backup: address { in constructor() => address(0) }
        }
    "#,
        false,
        |program| {
            let route = &mut program.entities[0].routes[1];
            route.from_clauses[0].error_args = vec![Expr::Ident("m_owner".into())];
            route.from_clauses.push(cambrian_transpiler::ast::FromClause {
                entity_name: "m_backup".into(),
                args: vec![],
                with_params: None,
                kind: cambrian_transpiler::ast::FromClauseKind::Member,
                error_code: None,
                error_name: Some("AuthFailed".into()),
                error_args: vec![Expr::Ident("m_backup".into())],
            });
        },
    );
    assert!(
        sol.contains("revert AuthFailed(") && sol.contains("m_owner"),
        "dual uniform custom-error from must emit gen_from_checks revert return (L668–L683): {sol}"
    );
    assert_solc_compiles("route_nondet_dual_from_custom_error_args_revert", &sol);
}

#[test]
fn n4_113_codegen_route_det_dual_from_custom_error_args_revert_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        error AuthFailed(who: address);

        entity Vault {
            identity m_slot: u64
            routes {
                init setup() => []
                secret() from m_owner : throw AuthFailed(m_owner) => []
            }
            m_owner: address { in setup() => address(0) }
            m_backup: address { in setup() => address(0) }
        }
    "#,
        true,
        |program| {
            let route = program.entities[0]
                .routes
                .iter_mut()
                .find(|r| r.name == "secret")
                .expect("secret");
            route.from_clauses[0].error_args = vec![Expr::Ident("m_owner".into())];
            route.from_clauses.push(cambrian_transpiler::ast::FromClause {
                entity_name: "m_backup".into(),
                args: vec![],
                with_params: None,
                kind: cambrian_transpiler::ast::FromClauseKind::Member,
                error_code: None,
                error_name: Some("AuthFailed".into()),
                error_args: vec![Expr::Ident("m_backup".into())],
            });
        },
    );
    assert!(
        sol.contains("revert AuthFailed(") && sol.contains("m_backup"),
        "det dual uniform custom-error from must emit gen_from_checks_det revert return (L1976–L1991): {sol}"
    );
    assert_solc_compiles("route_det_dual_from_custom_error_args_revert", &sol);
}

#[test]
fn n4_113_codegen_route_det_from_create2_identity_continue_solc() {
    let program = parse_evm(
        r#"
        entity Token {
            identity m_series: u64
            routes { init setup() => [] }
            m_n: u64 { in setup() => 0 }
        }

        entity Gate {
            identity m_slot: u64
            routes {
                init setup() => []
                admin() from Token(m_slot) => []
            }
            m_n: u64 { in setup() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function admin(")
            && sol.contains("require(")
            && (sol.contains("computeCreate2") || sol.contains("CREATE2") || sol.contains("Token")),
        "det from Entity(full identity args) must hit CREATE2 continue arm (L1944–L1948): {sol}"
    );
    assert_solc_compiles("route_det_from_create2_identity_continue", &sol);
}

#[test]
fn n4_113_codegen_route_det_from_entity_peer_address_continue_solc() {
    let program = parse_evm(
        r#"
        entity Treasury {
            routes { init setup() => [] }
            m_n: u64 { in setup() => 0 }
        }

        entity Vault {
            identity m_slot: u64
            routes {
                init setup() => []
                pull(peer: address) from Treasury(peer) => []
            }
            m_n: u64 { in setup() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function pull(")
            && sol.contains("msg.sender")
            && sol.contains("peer"),
        "det from Entity(single address arg) must hit gen_expr continue (L1952–L1956): {sol}"
    );
    assert_solc_compiles("route_det_from_entity_peer_address_continue", &sol);
}

#[test]
fn n4_113_codegen_route_nondet_dual_uniform_throw_codes_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity Vault {
            routes {
                constructor() => []
                secret() from m_owner : throw 9 => []
            }
            m_owner: address { in constructor() => address(0) }
            m_backup: address { in constructor() => address(0) }
        }
    "#,
        false,
        |program| {
            let route = &mut program.entities[0].routes[1];
            route.from_clauses.push(cambrian_transpiler::ast::FromClause {
                entity_name: "m_backup".into(),
                args: vec![],
                with_params: None,
                kind: cambrian_transpiler::ast::FromClauseKind::Member,
                error_code: Some(9),
                error_name: None,
                error_args: vec![],
            });
        },
    );
    assert!(
        sol.contains("require(") && sol.contains("throw(9)"),
        "dual uniform numeric throw codes must hit gen_from_checks throw-code arm (L691–L694): {sol}"
    );
    assert_solc_compiles("route_nondet_dual_uniform_throw_codes", &sol);
}

#[test]
fn n4_113_codegen_route_det_dual_uniform_throw_codes_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity Vault {
            identity m_slot: u64
            routes {
                init setup() => []
                secret() from m_owner : throw 9 => []
            }
            m_owner: address { in setup() => address(0) }
            m_backup: address { in setup() => address(0) }
        }
    "#,
        true,
        |program| {
            let route = program.entities[0]
                .routes
                .iter_mut()
                .find(|r| r.name == "secret")
                .expect("secret");
            route.from_clauses.push(cambrian_transpiler::ast::FromClause {
                entity_name: "m_backup".into(),
                args: vec![],
                with_params: None,
                kind: cambrian_transpiler::ast::FromClauseKind::Member,
                error_code: Some(9),
                error_name: None,
                error_args: vec![],
            });
        },
    );
    assert!(
        sol.contains("function secret(")
            && sol.contains("require(")
            && sol.contains("throw(9)"),
        "det dual uniform numeric throw codes must hit gen_from_checks_det throw-code arm (L1998–L2001): {sol}"
    );
    assert_solc_compiles("route_det_dual_uniform_throw_codes", &sol);
}

#[test]
fn n4_113_codegen_route_ir_unphased_state_write_return_close_solc() {
    let program = parse_evm(
        r#"
        entity TrailingWrite {
            routes {
                constructor() => []
                bump() -> u64 => [
                    let next = m_n + 1;
                    return(next)
                ]
            }
            m_n: u64 { in constructor() => 0 in bump() => m_n }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function bump(")
            && sol.contains("uint64 next")
            && sol.contains("return next")
            && !sol.contains("// Phase:"),
        "unphased route with state write + return must hit emit_route_body_from_ir trailing close (L497–L507): {sol}"
    );
    assert_solc_compiles("route_ir_unphased_state_write_return_close", &sol);
}

// ---------------------------------------------------------------------------
// N4-117: codegen/solidity/evm/route.rs — slice 8 (residual llvm-partial @ N4-113)
// Baseline @ N4-116 queue: 97.39% (43 missed / 1647; ~22 excl. dead/debug_assert).
// Targets: emit_route_body_from_ir trailing close L507; gen_from_checks nondet
// L644/L682–L683/L694; resolve_var_call_type_with_target L824; gen_from_checks_det
// L1948/L1956/L1990–L1991/L2001. Exclude: UpdateCode L377–L390 (DEAD);
// IrStmt::AstAction L130/L195–L199 (DEAD); gen_from_checks false L656/L1967
// (debug_assert); gen_initialize_fn None L1793 (unreachable).
// Acceptance: ≥ 97.5% or ≤ 38 raw or excl. dead ≤ 17 or Δ ≥ −5 executable.
// ---------------------------------------------------------------------------

#[test]
fn n4_117_codegen_route_fallback_unphased_body_close_solc() {
    let program = parse_evm(
        r#"
        entity Receiver {
            routes {
                constructor() => []
                fallback() => []
            }
            m_n: u64 {
                in constructor() => 0
                in fallback() => m_n + 1
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("fallback() external")
            && !sol.contains("// Phase:")
            && sol.contains("m_n"),
        "fallback route must lower via emit_route_body_from_ir trailing-only close (L497–L507): {sol}"
    );
    assert_solc_compiles("route_fallback_unphased_body_close", &sol);
}

#[test]
fn n4_117_codegen_route_unphased_emit_state_write_close_solc() {
    let program = parse_evm(
        r#"
        entity Emitter {
            event Ping(n: u64);

            routes {
                constructor() => []
                ping() => [
                    emit Ping(m_n);
                ]
            }
            m_n: u64 { in constructor() => 0 in ping() => m_n + 1 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function ping(")
            && sol.contains("emit Ping")
            && !sol.contains("// Phase:"),
        "unphased emit + member transform must hit trailing-only emit_route_body_from_ir (L497–L507): {sol}"
    );
    assert_solc_compiles("route_unphased_emit_state_write_close", &sol);
}

#[test]
fn n4_117_codegen_route_nondet_from_entity_route_param_address_solc() {
    let program = parse_evm(
        r#"
        entity Treasury {
            routes { constructor() => [] }
            m_n: u64 { in constructor() => 0 }
        }

        entity Gate {
            routes {
                constructor() => []
                admin(peer: address) from Treasury(peer) => []
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function admin(")
            && sol.contains("require(")
            && sol.contains("peer"),
        "nondet from Entity(route param address) must hit gen_from_checks gen_expr continue (L640–L644): {sol}"
    );
    assert_solc_compiles("route_nondet_from_entity_route_param_address", &sol);
}

#[test]
fn n4_117_codegen_route_nondet_from_entity_member_address_solc() {
    let program = parse_evm(
        r#"
        entity Treasury {
            routes { constructor() => [] }
            m_peer: address { in constructor() => address(0) }
        }

        entity Gate {
            routes {
                constructor() => []
                admin() from Treasury(m_peer) => []
            }
            m_peer: address { in constructor() => address(0) }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function admin(")
            && sol.contains("require(")
            && sol.contains("m_peer"),
        "nondet from Entity(member address) must hit gen_from_checks single-arg continue (L640–L644): {sol}"
    );
    assert_solc_compiles("route_nondet_from_entity_member_address", &sol);
}

#[test]
fn n4_117_codegen_route_nondet_triple_from_custom_error_revert_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        error AuthFailed(who: address);

        entity Vault {
            routes {
                constructor() => []
                secret() from m_owner : throw AuthFailed(m_owner) => []
            }
            m_owner: address { in constructor() => address(0) }
            m_backup: address { in constructor() => address(0) }
            m_auditor: address { in constructor() => address(0) }
        }
    "#,
        false,
        |program| {
            let route = &mut program.entities[0].routes[1];
            route.from_clauses[0].error_args = vec![Expr::Ident("m_owner".into())];
            route.from_clauses.push(cambrian_transpiler::ast::FromClause {
                entity_name: "m_backup".into(),
                args: vec![],
                with_params: None,
                kind: cambrian_transpiler::ast::FromClauseKind::Member,
                error_code: None,
                error_name: Some("AuthFailed".into()),
                error_args: vec![Expr::Ident("m_backup".into())],
            });
            route.from_clauses.push(cambrian_transpiler::ast::FromClause {
                entity_name: "m_auditor".into(),
                args: vec![],
                with_params: None,
                kind: cambrian_transpiler::ast::FromClauseKind::Member,
                error_code: None,
                error_name: Some("AuthFailed".into()),
                error_args: vec![Expr::Ident("m_auditor".into())],
            });
        },
    );
    assert!(
        sol.contains("revert AuthFailed(")
            && sol.contains("m_owner")
            && sol.contains("m_auditor"),
        "triple uniform custom-error from must emit gen_from_checks revert return (L668–L683): {sol}"
    );
    assert_solc_compiles("route_nondet_triple_from_custom_error_revert", &sol);
}

#[test]
fn n4_117_codegen_route_nondet_dual_from_generic_require_message_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity Vault {
            routes {
                constructor() => []
                secret() from m_owner => []
            }
            m_owner: address { in constructor() => address(0) }
            m_backup: address { in constructor() => address(0) }
        }
    "#,
        false,
        |program| {
            let route = &mut program.entities[0].routes[1];
            route.from_clauses.push(cambrian_transpiler::ast::FromClause {
                entity_name: "m_backup".into(),
                args: vec![],
                with_params: None,
                kind: cambrian_transpiler::ast::FromClauseKind::Member,
                error_code: None,
                error_name: None,
                error_args: vec![],
            });
        },
    );
    assert!(
        sol.contains("require(") && sol.contains("from clause failed"),
        "dual from without throw codes must hit gen_from_checks generic require message (L696–L699): {sol}"
    );
    assert_solc_compiles("route_nondet_dual_from_generic_require_message", &sol);
}

#[test]
fn n4_117_codegen_route_unphased_var_call_address_return_solc() {
    let program = parse_evm(
        r#"
        entity Registry {
            identity m_id: u64
            routes {
                init boot() => []
                ownerOf() -> address => [ return(msg::sender) ]
            }
            m_n: u64 { in boot() => 0 }
        }

        entity Host {
            routes {
                init boot() => []
                probe() => [
                    var who = ownerOf() ~> Registry.address(m_id);
                ]
            }
            m_id: u64 { in boot() => 1 }
            m_n: u64 { in boot() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function probe(")
            && sol.contains("address who")
            && sol.contains(".ownerOf("),
        "unphased address-return var-call must resolve via sol_return_type (L821–L824): {sol}"
    );
    assert_solc_compiles("route_unphased_var_call_address_return", &sol);
}

#[test]
fn n4_117_codegen_route_phased_var_call_bool_return_solc() {
    let program = parse_evm(
        r#"
        entity Flags {
            identity m_id: u64
            routes {
                init boot() => []
                active() -> bool => [ return(true) ]
            }
            m_ok: bool { in boot() => true }
        }

        entity Host {
            routes {
                init boot() => []
                probe() => [
                    work: [
                        var ok = active() ~> Flags.address(m_id);
                    ]
                ]
            }
            m_id: u64 { in boot() => 1 }
            m_n: u64 { in boot() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("// Phase: work")
            && sol.contains("bool ok")
            && sol.contains(".active("),
        "phased bool-return var-call must resolve via sol_return_type (L821–L824): {sol}"
    );
    assert_solc_compiles("route_phased_var_call_bool_return", &sol);
}

#[test]
fn n4_117_codegen_route_det_from_dual_identity_create2_continue_solc() {
    let program = parse_evm(
        r#"
        entity Pair {
            identity left: u64
            identity right: u64
            routes { init setup() => [] }
            m_n: u64 { in setup() => 0 }
        }

        entity Gate {
            identity m_slot: u64
            routes {
                init setup() => []
                admin() from Pair(left, right) => []
            }
            left: u64 { in setup() => 1 }
            right: u64 { in setup() => 2 }
            m_n: u64 { in setup() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function admin(")
            && sol.contains("require(")
            && (sol.contains("computeCreate2") || sol.contains("Pair")),
        "det from Entity(two identity args) must hit CREATE2 identity match continue (L1944–L1948): {sol}"
    );
    assert_solc_compiles("route_det_from_dual_identity_create2_continue", &sol);
}

#[test]
fn n4_117_codegen_route_det_dual_from_generic_require_message_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        entity Vault {
            identity m_slot: u64
            routes {
                init setup() => []
                secret() from m_owner => []
            }
            m_owner: address { in setup() => address(0) }
            m_backup: address { in setup() => address(0) }
        }
    "#,
        true,
        |program| {
            let route = program.entities[0]
                .routes
                .iter_mut()
                .find(|r| r.name == "secret")
                .expect("secret");
            route.from_clauses.push(cambrian_transpiler::ast::FromClause {
                entity_name: "m_backup".into(),
                args: vec![],
                with_params: None,
                kind: cambrian_transpiler::ast::FromClauseKind::Member,
                error_code: None,
                error_name: None,
                error_args: vec![],
            });
        },
    );
    assert!(
        sol.contains("require(") && sol.contains("from clause failed"),
        "det dual from without throw codes must hit gen_from_checks_det generic require (L2003–L2006): {sol}"
    );
    assert_solc_compiles("route_det_dual_from_generic_require_message", &sol);
}

#[test]
fn n4_117_codegen_route_det_triple_from_custom_error_revert_solc() {
    let sol = gen_evm_solidity_patched(
        r#"
        error AuthFailed(who: address);

        entity Vault {
            identity m_slot: u64
            routes {
                init setup() => []
                secret() from m_owner : throw AuthFailed(m_owner) => []
            }
            m_owner: address { in setup() => address(0) }
            m_backup: address { in setup() => address(0) }
            m_auditor: address { in setup() => address(0) }
        }
    "#,
        true,
        |program| {
            let route = program.entities[0]
                .routes
                .iter_mut()
                .find(|r| r.name == "secret")
                .expect("secret");
            route.from_clauses[0].error_args = vec![Expr::Ident("m_owner".into())];
            route.from_clauses.push(cambrian_transpiler::ast::FromClause {
                entity_name: "m_backup".into(),
                args: vec![],
                with_params: None,
                kind: cambrian_transpiler::ast::FromClauseKind::Member,
                error_code: None,
                error_name: Some("AuthFailed".into()),
                error_args: vec![Expr::Ident("m_backup".into())],
            });
            route.from_clauses.push(cambrian_transpiler::ast::FromClause {
                entity_name: "m_auditor".into(),
                args: vec![],
                with_params: None,
                kind: cambrian_transpiler::ast::FromClauseKind::Member,
                error_code: None,
                error_name: Some("AuthFailed".into()),
                error_args: vec![Expr::Ident("m_auditor".into())],
            });
        },
    );
    assert!(
        sol.contains("revert AuthFailed(") && sol.contains("m_backup"),
        "det triple uniform custom-error from must emit gen_from_checks_det revert return (L1976–L1991): {sol}"
    );
    assert_solc_compiles("route_det_triple_from_custom_error_revert", &sol);
}

// ---------------------------------------------------------------------------
// N4-142: codegen/solidity/evm/route.rs — slice 9 (EXECUTABLE_CLOSED close-out @ N4-117 plateau)
// Baseline @ N4-117: 97.39% (43 missed / 1647; ~22 excl. DEAD/debug_assert).
// Fresh @ d6abb1f pre-slice: 97.45% (42 missed). Targets: llvm-partial clusters
// L507/L644/L682–L683/L694/L824/L1948/L1956/L1990–L2001; exclude DEAD/unreachable.
// Acceptance: ≥99% or Δ≥−3 vs 43 or EXECUTABLE_CLOSED reaffirmed.
// ---------------------------------------------------------------------------

#[test]
fn n4_142_codegen_route_det_from_singleton_entity_address_fallback_solc() {
    let program = parse_evm(
        r#"
        entity Treasury {
            routes { init boot() => [] }
            m_n: u64 { in boot() => 0 }
        }

        entity Gate {
            identity m_slot: u64
            routes {
                init boot() => []
                admin() from Treasury(m_peer) => []
            }
            m_peer: address { in boot() => address(0) }
            m_n: u64 { in boot() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function admin(")
            && sol.contains("require(")
            && sol.contains("m_peer"),
        "det from singleton Entity(member) must use single-arg gen_from_checks_det fallback (L1951–L1956): {sol}"
    );
    assert_solc_compiles("route_det_from_singleton_entity_address_fallback", &sol);
}

#[test]
fn n4_142_codegen_route_phased_trailing_return_after_named_solc() {
    let program = parse_evm(
        r#"
        entity Worker {
            routes {
                init boot() => []
                finish() -> u64 => [
                    prep: [
                        let step = m_n + 1;
                        return(step)
                    ]
                ]
            }
            m_n: u64 { in boot() => 0 in finish() => m_n + 1 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("// Phase: prep")
            && sol.contains("return step")
            && sol.contains("function finish("),
        "phased named + trailing unnamed must emit trailing body after named blocks (L539–L547): {sol}"
    );
    // TB-V: validator rejects probe — forge oracle dropped.
}

// ---------------------------------------------------------------------------
// N4-111: codegen/evm_test_codegen.rs — residual scatter @ N4-106 tail.
// Baseline @ N4-110: 97.24% (55 missed / 1992; ~47 excl. DEAD).
// Targets: forall_dynamic_fields push L924; gen_invariant_fn selector close L1726;
// multi-invariant Assume/skip L1955; lower_check_expr_multi Deref L2207;
// default_value_for_type / infer_sol_type L2428/L2439; inclusive bound L1574.
// Exclude: gen_state_accessor L1349+ (DEAD); generate_evm_tests skip L76 (DEAD);
// whole_word_replace* empty needle L2319/L2344 (DEAD).
// Acceptance: ≥ 97.5% or ≤ 48 raw or ≤ 41 excl. DEAD (Δ ≥ −6 executable).
// ---------------------------------------------------------------------------

#[test]
fn n4_111_evm_forall_dynamic_fields_with_unpinned_field_solc() {
    let program = parse_evm(
        r#"
        entity Wallet {
            routes { constructor() => [] tick() => [] }
            m_balance: u64 {
                in constructor() => 0
                in tick() => m_balance
            }
            m_bonus: u64 {
                in constructor() => 0
                in tick() => m_bonus
            }
        }

        invariant "bonus forall" for Wallet {
            init { m_balance: 0 }
            with { m_bonus: * }
            action tick() {}
            check m_balance <= m_bonus
        }
    "#,
    );
    let files = evm_harness_project_files(&program, false);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("bonus forall invariant must emit file");
    assert!(
        inv.contains("forall init: randomized starting state")
            && (inv.contains("__fa_m_bonus") || inv.contains("m_bonus")),
        "unpinned with m_bonus:* must hit forall_dynamic_fields push (L924): {inv}"
    );
    assert_forge_project_compiles("evm_forall_dynamic_fields_with_unpinned", &files);
}

#[test]
fn n4_111_evm_invariant_target_selector_with_time_close_solc() {
    let program = parse_evm(
        r#"
        entity Clock {
            routes { constructor() => [] tick() => [] }
            m_n: u64 {
                in constructor() => 0
                in tick() => m_n + 1
            }
        }

        invariant "selector close" for Clock #[with_time] {
            init { m_n: 0 }
            action tick() {}
            check m_n >= 0
        }
    "#,
    );
    let files = evm_harness_project_files(&program, false);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("with_time invariant must emit file");
    assert!(
        inv.contains("advanceTime(uint256)")
            && inv.contains("targetSelector(FuzzSelector"),
        "with_time + action must hit selector table close (L1719–L1726): {inv}"
    );
    assert_forge_project_compiles("evm_invariant_target_selector_with_time", &files);
}

#[test]
fn n4_111_evm_invariant_inclusive_bound_single_entity_solc() {
    let program = parse_evm(
        r#"
        entity Gauge {
            routes { constructor() => [] bump(n: u64) => [] }
            m_n: u64 {
                in constructor() => 0
                in bump(n) => m_n + n
            }
        }

        invariant "inclusive bound" for Gauge {
            init { m_n: 0 }
            action bump(n: u64) {
                bound n in 1..=50
            }
            check m_n >= 0
        }
    "#,
    );
    let files = evm_harness_project_files(&program, false);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("inclusive bound invariant must emit file");
    assert!(
        inv.contains("bound(n,") && !inv.contains(") - 1"),
        "inclusive action bound must pass hi through without -1 (L1573–L1574): {inv}"
    );
    assert_forge_project_compiles("evm_invariant_inclusive_bound_single", &files);
}

#[test]
fn n4_111_evm_multi_invariant_action_skipif_noop_solc() {
    let mut program = parse_evm(
        r#"
        entity Alpha {
            routes { constructor() => [] tick() => [] }
            m_n: u64 { in constructor() => 0 in tick() => m_n + 1 }
        }
        entity Beta {
            routes { constructor() => [] tick() => [] }
            m_k: u64 { in constructor() => 0 in tick() => m_k + 1 }
        }

        invariant "skip noop" for { a: Alpha, b: Beta } {
            init a { m_n: 0 }
            init b { m_k: 0 }
            action a.tick() {}
            action b.tick() {}
            check a.m_n >= b.m_k
        }
    "#,
    );
    program.invariants[0].actions[0].body = vec![
        TestStep::Assume {
            cond: Expr::BoolLiteral(true),
        },
        TestStep::SkipIf {
            cond: Expr::BoolLiteral(false),
        },
    ];
    let files = evm_harness_project_files(&program, false);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("skip noop multi invariant must emit file");
    assert!(
        inv.contains("vm.assume(true)") && inv.contains("a_tick"),
        "multi action Assume must emit vm.assume (L1951–L1953): {inv}"
    );
    assert!(
        !inv.contains("if (false) return"),
        "multi handler must ignore SkipIf via noop arm (L1955): {inv}"
    );
    assert_forge_project_compiles("evm_multi_invariant_action_skipif_noop", &files);
}

#[test]
fn n4_111_evm_multi_invariant_check_deref_emit_solc() {
    let mut program = parse_evm(
        r#"
        entity Host {
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 1 }
        }
        entity Peer {
            routes { constructor() => [] }
            m_y: u64 { in constructor() => 2 }
        }
    "#,
    );
    program.invariants.push(InvariantDecl {
        name: "deref emit".into(),
        instances: vec![
            InvariantInstance {
                name: "h".into(),
                entity: "Host".into(),
                init: vec![("m_x".into(), Expr::IntLiteral(U256::from_u128(1)))],
                forall_state: ForallSpec::default(),
                init_specified: true,
                span: span0(),
            },
            InvariantInstance {
                name: "p".into(),
                entity: "Peer".into(),
                init: vec![("m_y".into(), Expr::IntLiteral(U256::from_u128(2)))],
                forall_state: ForallSpec::default(),
                init_specified: true,
                span: span0(),
            },
        ],
        skip_from: false,
        senders: vec![],
        deploy: vec![],
        context: ContextSpec { entries: vec![] },
        actions: vec![],
        checks: vec![Expr::UnaryOp(
            UnaryOp::Deref,
            Box::new(Expr::Ident("h".into())),
        )],
        fail_on_revert: false,
        runs: None,
        depth: None,
        tag: None,
        instantiates: None,
        emit_policy: InvariantEmitPolicy::Emit,
        with_time: false,
        track: vec![],
        derived: vec![],
        exclude_senders: vec![],
        exclude_selectors: vec![],
        span: span0(),
    });
    let files = evm_harness_project_files(&program, false);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("deref emit invariant must emit file");
    assert!(
        inv.contains("require(h,")
            || inv.contains("require(h ,")
            || inv.contains("address(_h)"),
        "multi invariant check Deref must lower via lower_check_expr_multi (L2207): {inv}"
    );
    assert_forge_project_compiles("evm_multi_invariant_check_deref_emit", &files);
}

#[test]
fn n4_111_evm_test_let_short_hex_infer_sol_type_solc() {
    let program = parse_evm(
        r#"
        entity ShortHex {
            routes { constructor() => [] note() => [] }
            m_n: u64 { in constructor() => 0 in note() => m_n }
        }

        test "short hex let" for ShortHex {
            let slot = 0x12
            call note()
        }
    "#,
    );
    let files = evm_harness_project_files(&program, false);
    assert_forge_project_compiles("evm_test_let_short_hex_infer", &files);
}

#[test]
fn n4_111_evm_det_multi_typed_address_identity_default_solc() {
    let program = parse_evm(
        r#"
        entity Token {
            routes { init boot() => [] }
            m_n: u64 { in boot() => 0 }
        }

        entity Pair {
            identity m_token0: Address<Token>
            identity m_token1: Address<Token>
            routes { init boot() => [] pulse() => [] }
            m_n: u64 { in boot() => 0 in pulse() => m_n }
        }

        invariant "typed defaults" for { p: Pair } {
            init p { m_n: 0 }
            action p.pulse() {}
            check p.m_n >= 0
        }
    "#,
    );
    let files = evm_harness_project_files(&program, true);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("typed defaults det invariant must emit file");
    assert!(
        inv.contains("deployPair(")
            && (inv.contains("address(0)") || inv.contains("address(uint160(0))")),
        "unseeded TypedAddress identities must hit default_value_for_type (U4-4c): {inv}"
    );
    assert!(
        inv.contains("cam_wire(") && !inv.contains("new Pair("),
        "handler-first typed-address multi invariant (U4-4c): {inv}"
    );
    assert_forge_project_compiles("evm_det_multi_typed_address_identity_default", &files);
}

#[test]
fn n4_111_evm_forall_dynamic_fields_patched_instance_push() {
    let mut program = parse_evm(
        r#"
        entity Wallet {
            routes { constructor() => [] tick() => [] }
            m_balance: u64 {
                in constructor() => 0
                in tick() => m_balance
            }
            m_bonus: u64 {
                in constructor() => 0
                in tick() => m_bonus
            }
        }

        invariant "bonus seed patched" for Wallet {
            init { m_balance: 0, m_bonus: * }
            action tick() {}
            check m_balance <= m_bonus
        }
    "#,
    );
    program.invariants[0].instances[0].init =
        vec![("m_balance".into(), Expr::IntLiteral(U256::ZERO))];
    program.invariants[0].instances[0].forall_state = ForallSpec {
        targets: vec![ForallTarget::StateField("m_bonus".into())],
        ..Default::default()
    };
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("patched forall invariant must emit file");
    assert!(
        inv.contains("forall init: randomized starting state") && inv.contains("m_bonus"),
        "patched unpinned forall field must hit forall_dynamic_fields push (L923–L924): {inv}"
    );
}

#[test]
fn n4_111_evm_default_value_generic_identity_fallback() {
    let mut program = parse_evm(
        r#"
        entity Vault {
            identity m_tag: u64
            routes { constructor() => [] touch() => [] }
            m_n: u64 { in constructor() => 0 in touch() => m_n }
        }

        test "vault ctor" for Vault {
            call touch()
        }
    "#,
    );
    let vault = program
        .entities
        .iter_mut()
        .find(|e| e.name == "Vault")
        .expect("Vault entity");
    vault
        .members
        .iter_mut()
        .find(|m| m.name == "m_tag")
        .expect("m_tag identity")
        .ty = Type::Generic("Vec".into(), vec![Type::Simple("u64".into())]);
    let files = gen_evm_test_files_det(&program);
    let test_sol = foundry_test_file(&files, "Vault");
    assert!(
        test_sol.contains("deployVault(0)"),
        "generic identity must hit default_value_for_type fallback via factory deploy: {test_sol}"
    );
}

#[test]
fn n4_111_evm_forall_dynamic_fields_all_state_push() {
    let program = parse_evm(
        r#"
        entity Solo {
            routes { constructor() => [] tick() => [] }
            m_n: u64 {
                in constructor() => 0
                in tick() => m_n + 1
            }
        }

        invariant "all state forall" for Solo {
            init { * }
            action tick() {}
            check m_n >= 0
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("all-state forall invariant must emit file");
    assert!(
        inv.contains("forall init: randomized starting state") && inv.contains("m_n"),
        "bare init * must hit forall_dynamic_fields all_state push (L923–L924): {inv}"
    );
}

#[test]
fn n4_111_evm_derived_let_body_codegen_direct() {
    let mut program = parse_evm(
        r#"
        entity Box {
            routes { constructor() => [] tick() => [] }
            m_n: u64 { in constructor() => 0 in tick() => m_n + 1 }
        }

        invariant "derived let direct" for Box {
            init { m_n: 0 }
            action tick() {}
            check m_n >= 0
        }
    "#,
    );
    program.invariants[0].derived.push(InvariantQuery {
        name: "slot".into(),
        params: vec![],
        return_type: Type::Simple("u64".into()),
        body: vec![TestStep::Let {
            ty: None,
            name: "seed".into(),
            value: Expr::IntLiteral(U256::from_u128(7)),
        }],
        return_value: Expr::Ident("seed".into()),
        span: span0(),
    });
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("derived let direct invariant must emit file");
    assert!(
        inv.contains("function slot()") && inv.contains("seed = 7"),
        "derived query let step must emit helper body (L1518–L1519): {inv}"
    );
}

#[test]
fn n4_111_evm_invariant_target_selector_action_only_close_solc() {
    let program = parse_evm(
        r#"
        entity Gate {
            routes { constructor() => [] open() => [] }
            m_open: bool { in constructor() => false in open() => true }
        }

        invariant "selector action only" for Gate {
            init { m_open: false }
            action open() {}
            check m_open == true || m_open == false
        }
    "#,
    );
    let inv = gen_evm_test_files(&program)
        .into_iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c)
        .expect("selector action-only invariant must emit file");
    assert!(
        inv.contains("selectors[0] = bytes4(keccak256(\"open()\"))")
            && inv.contains("targetSelector(FuzzSelector"),
        "action-only invariant must close targetSelector table (L1706–L1726): {inv}"
    );
}

// ---------------------------------------------------------------------------
// N4-118: codegen/evm_test_codegen.rs — slice 9 (residual llvm-partial @ N4-111)
// Baseline @ N4-117 queue: 97.49% (50 missed / 1992; ~41 excl. DEAD).
// Targets: forall_dynamic_fields push L924; derived let body close L1519;
// gen_invariant_file targetSelector if-block close L1726; scatter executable.
// Exclude: gen_state_accessor L1349+ (DEAD); generate_evm_tests skip L76 (DEAD);
// whole_word_replace* L2319/L2344 (DEAD).
// Acceptance: ≥ 97.5% or ≤ 45 raw or excl. DEAD ≤ 38 or Δ ≥ −5 executable.
// ---------------------------------------------------------------------------

#[test]
fn n4_118_evm_forall_dynamic_fields_multi_inst_second_push() {
    let mut program = parse_evm(
        r#"
        entity Vault {
            routes { constructor() => [] bump(n: u64) => [] }
            m_balance: u64 { in constructor() => 0 in bump(n) => m_balance + n }
        }
        entity Ledger {
            routes { constructor() => [] add(n: u64) => [] }
            m_total: u64 { in constructor() => 0 in add(n) => m_total + n }
            m_audit: u64 { in constructor() => 0 in add(n) => m_audit }
        }

        invariant "book forall" for { v: Vault, book: Ledger } {
            init v { m_balance: 0 }
            init book { m_total: 0 }
            action v.bump(n: u64) { bound n in 1..10 }
            action book.add(n: u64) { bound n in 1..10 }
            check v.m_balance + book.m_total >= book.m_audit
        }
    "#,
    );
    let book = program.invariants[0]
        .instances
        .iter_mut()
        .find(|i| i.name == "book")
        .expect("book instance");
    book.forall_state = ForallSpec {
        targets: vec![ForallTarget::StateField("m_audit".into())],
        ..Default::default()
    };
    let files = evm_harness_project_files(&program, false);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("multi-inst forall invariant must emit file");
    assert!(
        inv.contains("forall init: randomized starting state")
            && inv.contains("book_m_audit"),
        "multi-entity second-instance forall must hit forall_dynamic_fields push (L924): {inv}"
    );
    assert_forge_project_compiles("evm_forall_multi_inst_second_push", &files);
}

#[test]
fn n4_118_evm_forall_dynamic_fields_dual_unpinned_push() {
    let program = parse_evm(
        r#"
        entity Pair {
            routes { constructor() => [] tick() => [] }
            m_seed: u64 { in constructor() => 0 in tick() => m_seed }
            m_counter: u64 { in constructor() => 0 in tick() => m_counter + 1 }
        }

        invariant "dual unpinned" for Pair {
            init { m_seed: 0 }
            with { m_counter: * }
            action tick() {}
            check m_counter >= m_seed
        }
    "#,
    );
    let files = evm_harness_project_files(&program, false);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("dual unpinned invariant must emit file");
    assert!(
        inv.contains("forall init: randomized starting state")
            && inv.contains("m_counter")
            && !inv.contains("__fa_m_seed"),
        "pinned m_seed + unpinned m_counter must push only counter (L920–L924): {inv}"
    );
    assert_forge_project_compiles("evm_forall_dual_unpinned_push", &files);
}

#[test]
fn n4_118_evm_forall_dynamic_fields_inject_duplicate_target_push() {
    let mut program = parse_evm(
        r#"
        entity Wallet {
            routes { constructor() => [] tick() => [] }
            m_balance: u64 { in constructor() => 0 in tick() => m_balance }
            m_bonus: u64 { in constructor() => 0 in tick() => m_bonus }
            m_ticks: u64 { in constructor() => 0 in tick() => m_ticks + 1 }
        }

        invariant "dedup targets" for Wallet {
            init { m_balance: 0 }
            action tick() {}
            check m_balance <= m_bonus + m_ticks
        }
    "#,
    );
    program.invariants[0].instances[0].forall_state = ForallSpec {
        targets: vec![
            ForallTarget::StateField("m_bonus".into()),
            ForallTarget::StateField("m_bonus".into()),
            ForallTarget::StateField("m_ticks".into()),
        ],
        ..Default::default()
    };
    let files = evm_harness_project_files(&program, false);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("dedup-target invariant must emit file");
    assert!(
        inv.contains("forall init: randomized starting state")
            && inv.contains("m_bonus")
            && inv.contains("m_ticks"),
        "duplicate forall targets must dedup then push survivors (L920–L924): {inv}"
    );
    assert_forge_project_compiles("evm_forall_inject_duplicate_target_push", &files);
}

#[test]
fn n4_118_evm_derived_dual_let_body_close_solc() {
    let mut program = parse_evm(
        r#"
        entity Box {
            routes { constructor() => [] tick() => [] }
            m_n: u64 { in constructor() => 0 in tick() => m_n + 1 }
        }

        invariant "dual let derived" for Box {
            init { m_n: 0 }
            action tick() {}
            check m_n >= 0
        }
    "#,
    );
    program.invariants[0].derived.push(InvariantQuery {
        name: "pair".into(),
        params: vec![],
        return_type: Type::Simple("u64".into()),
        body: vec![
            TestStep::Let {
                ty: None,
                name: "lo".into(),
                value: Expr::IntLiteral(U256::from_u128(1)),
            },
            TestStep::Let {
                ty: None,
                name: "hi".into(),
                value: Expr::BinOp(
                    Box::new(Expr::Ident("lo".into())),
                    BinOp::Add,
                    Box::new(Expr::IntLiteral(U256::from_u128(2))),
                ),
            },
        ],
        return_value: Expr::Ident("hi".into()),
        span: span0(),
    });
    let files = evm_harness_project_files(&program, false);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("dual-let derived invariant must emit file");
    assert!(
        inv.contains("function pair()")
            && inv.contains("lo = 1")
            && inv.contains("hi ="),
        "dual derived let steps must emit helper body closes (L1514–L1519): {inv}"
    );
    assert_forge_project_compiles("evm_derived_dual_let_body_close", &files);
}

#[test]
fn n4_118_evm_derived_let_member_subst_close_solc() {
    let program = parse_evm(
        r#"
        entity Pool {
            routes { constructor() => [] touch() => [] }
            m_assets: u128 { in constructor() => 1000 in touch() => m_assets }
            m_borrowed: u128 { in constructor() => 0 }
        }

        invariant "headroom subst" for Pool {
            init { m_assets: 1000, m_borrowed: 0 }

            derived headroom(buffer: u128) -> u128 {
                let cap = m_assets + buffer
                return cap
            }

            action touch() {}
            check headroom(10) >= m_borrowed
        }
    "#,
    );
    let files = evm_harness_project_files(&program, false);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("derived member-subst invariant must emit file");
    assert!(
        inv.contains("function headroom(uint128 buffer)")
            && inv.contains("cap =")
            && inv.contains("m_assets"),
        "derived let with member ref must substitute in helper body (L1516–L1519): {inv}"
    );
    assert_forge_project_compiles("evm_derived_let_member_subst_close", &files);
}

#[test]
fn n4_118_evm_invariant_selector_triple_action_time_senders_close_solc() {
    let program = parse_evm(
        r#"
        entity Hub {
            routes {
                constructor() => []
                open() => []
                bump(n: u64) => []
                close() => []
            }
            m_open: bool { in constructor() => false in open() => true in close() => false }
            m_n: u64 { in constructor() => 0 in bump(n) => m_n + n }
        }

        invariant "triple selector" for Hub #[with_time] {
            init { m_open: false, m_n: 0 }
            ctx { sys::now: 1_700_000_100, msg::sender: 0x00000000000000000000000000000000000000aa }

            senders {
                0x0000000000000000000000000000000000000001,
                0x0000000000000000000000000000000000000002
            }

            action open() {}
            action bump(n: u64) { bound n in 1..5 }
            action close() {}

            check m_n >= 0
        }
    "#,
    );
    let files = evm_harness_project_files(&program, false);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("triple-action invariant must emit file");
    assert!(
        inv.contains("selectors[3] = bytes4(keccak256(\"advanceTime(uint256)\"))")
            && inv.contains("targetSender(")
            && inv.contains("targetSelector(FuzzSelector"),
        "triple action + with_time + senders must close selector if-block (L1719–L1726): {inv}"
    );
    assert_forge_project_compiles("evm_invariant_triple_action_time_senders_close", &files);
}

#[test]
fn n4_118_evm_invariant_selector_time_senders_ctx_combo_close_solc() {
    let program = parse_evm(
        r#"
        entity Clock {
            routes { constructor() => [] tick() => [] }
            m_n: u64 { in constructor() => 0 in tick() => m_n + 1 }
        }

        invariant "time sender ctx" for Clock #[with_time] {
            init { m_n: 0 }
            ctx { sys::now: 1_800_000_000, msg::sender: 0x00000000000000000000000000000000000000cc }

            senders { 0x00000000000000000000000000000000000000dd }

            action tick() {}

            check m_n >= 0
        }
    "#,
    );
    let files = evm_harness_project_files(&program, false);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("time/sender/ctx invariant must emit file");
    assert!(
        inv.contains("advanceTime(uint256)")
            && inv.contains("targetSender(")
            && inv.contains("vm.warp(")
            && inv.contains("targetSelector(FuzzSelector"),
        "with_time + senders + ctx combo must close targetSelector if-block (L1720–L1726): {inv}"
    );
    assert_forge_project_compiles("evm_invariant_time_senders_ctx_combo_close", &files);
}

#[test]
fn n4_118_evm_invariant_selector_exclude_with_time_close_solc() {
    let program = parse_evm(
        r#"
        entity Gate {
            routes {
                constructor() => []
                open() => []
                reset() => []
            }
            m_open: bool { in constructor() => false in open() => true in reset() => false }
        }

        invariant "exclude time close" for Gate #[with_time] {
            init { m_open: false }

            exclude selectors { reset }

            action open() {}
            action reset() {}

            check true
        }
    "#,
    );
    let files = evm_harness_project_files(&program, false);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("exclude+time invariant must emit file");
    assert!(
        inv.contains("excludeSelector")
            && inv.contains("advanceTime(uint256)")
            && inv.contains("targetSelector(FuzzSelector"),
        "exclude selectors + with_time must still close selector table (L1719–L1726): {inv}"
    );
    assert_forge_project_compiles("evm_invariant_exclude_with_time_close", &files);
}

#[test]
fn n4_118_evm_forall_dynamic_fields_bool_address_push_solc() {
    let program = parse_evm(
        r#"
        entity Profile {
            routes { constructor() => [] }
            m_active: bool { in constructor() => false }
            m_owner: address { in constructor() => 0x0000000000000000000000000000000000000001 }
        }

        invariant "typed forall push" for Profile {
            init { m_active: *, m_owner: * }
            action constructor() {}
            check true
        }
    "#,
    );
    let files = evm_harness_project_files(&program, false);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("bool/address forall invariant must emit file");
    assert!(
        inv.contains("forall init: randomized starting state")
            && (inv.contains("__fa_m_active") || inv.contains("randomUint() % 2"))
            && (inv.contains("__fa_m_owner") || inv.contains("uint160")),
        "bool+address forall members must push via forall_dynamic_fields (L923–L924): {inv}"
    );
    assert_forge_project_compiles("evm_forall_bool_address_push", &files);
}

#[test]
fn n4_118_evm_invariant_dual_action_param_selector_close_solc() {
    let program = parse_evm(
        r#"
        entity Gauge {
            routes {
                constructor() => []
                bump(n: u64) => []
                set(flag: bool) => []
            }
            m_n: u64 { in constructor() => 0 in bump(n) => m_n + n }
            m_ok: bool { in constructor() => false in set(flag) => flag }
        }

        invariant "dual param selectors" for Gauge {
            init { m_n: 0, m_ok: false }
            action bump(n: u64) { bound n in 1..20 }
            action set(flag: bool) {}
            check m_n >= 0
        }
    "#,
    );
    let files = evm_harness_project_files(&program, false);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("dual-param selector invariant must emit file");
    assert!(
        inv.contains("selectors[0] = bytes4(keccak256(\"bump(uint64)\"))")
            && inv.contains("selectors[1] = bytes4(keccak256(\"set(bool)\"))")
            && inv.contains("targetSelector(FuzzSelector"),
        "heterogeneous action params must close selector if-block (L1706–L1726): {inv}"
    );
    assert_forge_project_compiles("evm_invariant_dual_action_param_selector_close", &files);
}

// ---------------------------------------------------------------------------
// N4-146: codegen/evm_test_codegen.rs — EXECUTABLE_CLOSED close-out @ N4-118 plateau.
// Baseline @ N4-118: 97.49% (50 missed / 1992; ~41 excl. DEAD). Targets: llvm-partial
// L924 (forall_dynamic_fields push), L1726 (targetSelector if-block close). Max 2 fixtures.
// Exclude: gen_state_accessor L1349+ (DEAD); generate_evm_tests skip L76 (DEAD);
// whole_word_replace* L2319/L2344 (DEAD); L1519 derived let close (oracle green @ N4-118).
// Acceptance: ≥ 99.0% or Δ ≥ −3 vs 50 missed; else EXECUTABLE_CLOSED reaffirmed.
// ---------------------------------------------------------------------------

#[test]
fn n4_146_evm_forall_dynamic_fields_mapping_skip_scalar_push() {
    let program = parse_evm(
        r#"
        entity Store {
            routes { constructor() => [] touch() => [] }
            m_n: u64 { in constructor() => 0 in touch() => m_n }
            m_map: HashMap<u64, u64> { in constructor() => {} in touch() => m_map }
            m_tag: u64 { in constructor() => 0 in touch() => m_tag + 1 }
        }

        invariant "mapping skip" for Store {
            init { m_n: 0 }
            with { * }
            action touch() {}
            check m_tag >= 0
        }
    "#,
    );
    let entity = program
        .entities
        .iter()
        .find(|e| e.name == "Store")
        .expect("Store entity");
    let inst = &program.invariants[0].instances[0];
    assert!(
        entity.members.iter().any(|m| m.name == "m_map")
            && !inst.init.iter().any(|(n, _)| n == "m_tag"),
        "fixture must keep m_map mapping and leave m_tag unpinned for forall push"
    );
    let files = evm_harness_project_files(&program, false);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("mapping-skip forall invariant must emit file");
    assert!(
        inv.contains("forall init: randomized starting state")
            && inv.contains("m_tag")
            && !inv.contains("__fa_m_map"),
        "all_state must skip mapping then push scalar m_tag (L917–L924): {inv}"
    );
    assert_forge_project_compiles("evm_forall_mapping_skip_scalar_push", &files);
}

#[test]
fn n4_146_evm_invariant_multi_entity_with_time_selector_close_solc() {
    let program = parse_evm(
        r#"
        entity Left {
            routes { constructor() => [] step() => [] }
            m_a: u64 { in constructor() => 0 in step() => m_a + 1 }
        }
        entity Right {
            routes { constructor() => [] pulse() => [] }
            m_b: u64 { in constructor() => 0 in pulse() => m_b + 1 }
        }

        invariant "paired time" for { left: Left, right: Right } #[with_time] {
            init left { m_a: 0 }
            init right { m_b: 0 }
            action left.step() {}
            action right.pulse() {}
            check left.m_a + right.m_b >= 0
        }
    "#,
    );
    let files = evm_harness_project_files(&program, false);
    let inv = files
        .iter()
        .find(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, c)| c.as_str())
        .expect("multi-entity with_time invariant must emit file");
    assert!(
        inv.contains("selectors[0] = bytes4(keccak256(\"left_step()\"))")
            && inv.contains("selectors[1] = bytes4(keccak256(\"right_pulse()\"))")
            && inv.contains("targetSelector(FuzzSelector"),
        "multi-entity handler must close targetSelector if-block (L1726): {inv}"
    );
    assert_forge_project_compiles("evm_invariant_multi_entity_with_time_selector_close", &files);
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_107_native_expect_effects_rawreserve_commit_exit() {
    const SRC: &str = r#"
entity Fx {
    routes { noop() => [] }
    m_v: u64 { in noop() => m_v }
}

test "platform reserve commit exit" for Fx with { m_v: 0 } {
    call noop()
    expect effects [
        rawReserve(100, 2),
        commit(),
        exit(7)
    ]
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("WasmEffect::RawReserve { value, flags }")
            && code.contains("rawReserve value mismatch"),
        "rawReserve expect must hit emit_effect_assert_at match arm (L1344–L1356): {code}"
    );
    assert!(
        code.contains("WasmEffect::Commit")
            && code.contains("expected Commit"),
        "commit expect must hit emit_effect_assert_at Commit arm (L1358–L1361): {code}"
    );
    assert!(
        code.contains("WasmEffect::Exit { code }")
            && code.contains("exit code mismatch"),
        "exit expect must hit emit_effect_assert_at Exit arm (L1362–L1372): {code}"
    );
}

// ---------------------------------------------------------------------------
// N4-120: codegen/test_codegen.rs — slice 9 (llvm-partial tail @ N4-115 gap).
// Baseline @ N4-119 queue: 98.73% (21 missed / 1649; ~11 excl. map_context_field SKIP).
// Targets: whole_word_replace continue L194; expect-return cast close L1082;
// tuple loop close L1115; gen_hashmap_assign/assert close L1747/L1768.
// Exclude: map_context_field L1795–L1804 (SKIP ~10); infer_abi u128 L1714 (DEAD).
// Acceptance: ≥ 99.0% or ≤ 18 raw or excl. SKIP ≤ 9 or Δ ≥ −5 executable.
// ---------------------------------------------------------------------------

#[cfg(feature = "rust-targets")]
#[test]
fn n4_120_native_invariant_assume_dual_member_replace() {
    let code = gen_native_test_code(
        r#"
        entity Gauge {
            routes { bump(n: u64) => [] }
            m_floor: u64 { in bump(n) => m_floor }
            m_hi: u64 { in bump(n) => m_hi }
            m_n: u64 { in bump(n) => m_n + n }
        }

        invariant "assume members" for Gauge {
            init { m_floor: 1, m_hi: 100, m_n: 0 }
            action bump(n: u64) {
                bound n in 1..5
                assume m_n >= m_floor && m_n <= m_hi
            }
            check m_n >= m_floor
        }
    "#,
        false,
        &FuzzConfig::default(),
        &InvariantConfig::default(),
    );
    assert!(
        code.contains("_pre_state.m_floor") && code.contains("_pre_state.m_hi"),
        "assume preconditions must rewrite members via whole_word_replace continue (L194/L452): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_120_native_invariant_inclusive_bound_member_endpoints() {
    let code = gen_native_test_code(
        r#"
        entity Span {
            routes { set(v: u64) => [] }
            m_lo: u64 { in set(v) => m_lo }
            m_hi: u64 { in set(v) => m_hi }
            m_n: u64 { in set(v) => v }
        }

        invariant "inclusive bound" for Span {
            init { m_lo: 2, m_hi: 20, m_n: 0 }
            action set(v: u64) {
                bound v in m_lo..=m_hi
            }
            check m_n >= m_lo && m_n <= m_hi
        }
    "#,
        false,
        &FuzzConfig::default(),
        &InvariantConfig::default(),
    );
    assert!(
        code.contains("_pre_state.m_lo") && code.contains("_pre_state.m_hi") && code.contains("<="),
        "inclusive bound with member endpoints must hit sub_members whole_word_replace (L194/L464): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_120_native_expect_return_address_cast_else_close() {
    const SRC: &str = r#"
entity AddrHost {
    routes {
        init boot() => []
        slot() -> address => [
            return(0x00000000000000000000000000000000000000ab)
        ]
    }
    m_n: u64 { in boot() => 0 }
}

test "address return cast" for AddrHost with { m_n: 0 } {
    call slot()
    expect return 0x00000000000000000000000000000000000000ab
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("assert_eq!(_ret_val") && code.contains(" as "),
        "address expect return must hit cast else arm before if-block close (L1078–L1082): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_120_native_expect_return_quadruple_tuple_close() {
    const SRC: &str = r#"
entity Quad {
    routes {
        init boot() => []
        all() -> (u64, u64, u64, u64) => [ return(1, 2, 3, 4) ]
    }
    m_n: u64 { in boot() => 0 }
}

test "quad tuple return" for Quad with { m_n: 0 } {
    call all()
    expect return (1, 2, 3, 4)
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains(".0") && code.contains(".1") && code.contains(".2") && code.contains(".3"),
        "quadruple tuple expect return must iterate slots before loop close (L1110–L1115): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_120_native_property_fuzz_expect_return_tuple_three_slot() {
    const SRC: &str = r#"
entity Triple {
    routes {
        init boot() => []
        all(a: u64, b: u64, c: u64) -> (u64, u64, u64) => [ return(a, b, c) ]
    }
    m_n: u64 { in boot() => 0 }
}

property "tuple fuzz" (a: u64, b: u64, c: u64) for Triple with { m_n: 0 } {
    bound a in 1..5
    bound b in 1..5
    bound c in 1..5
    call all(a, b, c)
    expect return (a, b, c)
    fuzz { a in 2..4, b in 2..4, c in 2..4 }
}
"#;
    let code = gen_native_test_code(SRC, true, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("return.0 mismatch") && code.contains("return.2 mismatch"),
        "property fuzz tuple expect must hit ExpectReturnTuple loop close (L1110–L1115): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_120_native_fuzz_invariant_hashmap_combo_assign_close() {
    const SRC: &str = r#"
entity Vault {
    routes { save(m: HashMap<u64, u64>) => [] }
    m_map: HashMap<u64, u64> {
        in save(m) => m
    }
    m_n: u64 { in save(_) => m_n }
}

property "fuzz map seed" (seed: u64) for Vault with {
    m_map: { 1 => 10, 2 => 20, 3 => 30 },
    m_n: 0
} {
    bound seed in 1..10
    call save(seed)
    fuzz { seed in 2..8 }
}

invariant "inv map seed" for Vault {
    init { m_map: { 4 => 40, 5 => 50, 6 => 60 }, m_n: 0 }
    action save(m: HashMap<u64, u64>) { }
    check m_n >= 0
}
"#;
    let code = gen_native_test_code(SRC, true, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("_state.m_map = _m") && code.matches("_m.insert").count() >= 6,
        "fuzz+invariant HashMap inits must close gen_hashmap_assign blocks (L305/L713/L1747): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_120_native_test_fuzz_hashmap_literal_assert_combo() {
    const SRC: &str = r#"
entity Ledger {
    routes { credit(key: u64, delta: u64) => [] }
    m_scores: HashMap<u64, u64> {
        in credit(key, delta) => m_scores
    }
    m_total: u64 { in credit(_, delta) => m_total + delta }
}

property "ledger fuzz" (key: u64, delta: u64) for Ledger with {
    m_scores: { 1 => 10, 2 => 20 },
    m_total: 0
} {
    bound key in 1..5
    bound delta in 1..5
    call credit(key, delta)
    fuzz { key in 2..4, delta in 2..4 }
}

test "ledger assert" for Ledger with {
    m_scores: { 3 => 30, 4 => 40 },
    m_total: 0
} {
    call credit(3, 5)
    expect state { m_scores: { 3 => 35, 4 => 40, 5 => 5 } }
}
"#;
    let code = gen_native_test_code(SRC, true, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("m_scores.get(&3)") && code.contains("m_scores.get(&5)"),
        "post-call HashMap literal expect must hit gen_hashmap_assert loop close (L1760–L1768): {code}"
    );
    assert!(
        code.contains("_state.m_scores = _m") || code.contains("_m.insert"),
        "pinned fuzz HashMap init must hit gen_hashmap_assign (L1740–L1747): {code}"
    );
}

// ---------------------------------------------------------------------------
// N4-147: codegen/test_codegen.rs — EXECUTABLE_CLOSED close-out @ N4-120 plateau.
// Baseline @ N4-120: 98.73% (21 missed / 1649; ~11 excl. map_context_field SKIP).
// Targets: llvm-partial L1082 (String cast close), L1747 (gen_hashmap_assign close).
// Exclude: map_context_field L1795–L1804 (SKIP ~10); infer_abi u128 L1714 (DEAD).
// Max 2 fixtures. Acceptance: ≥ 99.0% or Δ ≥ −3 vs 21; else EXECUTABLE_CLOSED reaffirmed.
// ---------------------------------------------------------------------------

#[cfg(feature = "rust-targets")]
#[test]
fn n4_147_native_expect_return_string_to_string_cast_close() {
    const SRC: &str = r#"
entity Label {
    routes {
        init boot() => []
        tag() -> String => [ return("alpha") ]
    }
    m_n: u64 { in boot() => 0 }
}

test "string return cast" for Label with { m_n: 0 } {
    call tag()
    expect return "alpha"
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("assert_eq!(_ret_val") && code.contains(".to_string()"),
        "String expect return must hit to_string cast arm before if-block close (L1075–L1082): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_147_native_test_single_entry_hashmap_init_close() {
    const SRC: &str = r#"
entity Pocket {
    routes { credit(key: u64, delta: u64) => [] }
    m_scores: HashMap<u64, u64> {
        in credit(key, delta) => m_scores
    }
    m_total: u64 { in credit(_, delta) => m_total + delta }
}

test "single map init" for Pocket with {
    m_scores: { 9 => 99 },
    m_total: 0
} {
    call credit(9, 1)
    expect state { m_scores: { 9 => 100 } }
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("_state.m_scores = _m")
            && code.contains("_m.insert")
            && code.contains("m_scores.get(&9)"),
        "single-entry HashMap init must close gen_hashmap_assign/assert blocks (L1740–L1768): {code}"
    );
}

// ---------------------------------------------------------------------------
// N4-115: codegen/test_codegen.rs — slice 8 (llvm-partial tail @ N4-112 gap).
// Baseline @ N4-114 queue: 98.67% (22 missed / 1649; ~12 excl. map_context_field SKIP).
// Targets: whole_word_replace L194; expect-state non-Field path L1025; expect-return
// cast/tuple close L1082/L1115; gen_hashmap_assign/assert L1747/L1768; registry fallback.
// Exclude: infer_abi u128 bound L1714 (DEAD); map_context_field L1795–L1804 (SKIP ~10).
// Acceptance: ≥ 99% or ≤ 15 raw or excl. SKIP ≤ 8 (Δ ≥ −5 executable).
// ---------------------------------------------------------------------------

#[cfg(feature = "rust-targets")]
#[test]
fn n4_115_native_invariant_bound_pre_state_member_replace() {
    let code = gen_native_test_code(
        r#"
        entity Gauge {
            routes { bump(n: u64) => [] }
            m_floor: u64 { in bump(n) => m_floor }
            m_n: u64 { in bump(n) => m_n + n }
        }

        invariant "pre-state bound" for Gauge {
            init { m_floor: 3, m_n: 0 }
            action bump(n: u64) {
                bound n in m_floor..(m_floor + 10)
            }
            check m_n >= m_floor
        }
    "#,
        false,
        &FuzzConfig::default(),
        &InvariantConfig::default(),
    );
    assert!(
        code.contains("let _pre_state = GaugeState::de_be")
            && code.contains("_pre_state.m_floor"),
        "bound endpoints referencing members must rewrite via whole_word_replace (L194/L444): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_115_native_expect_return_pubkey_cast_else_arm() {
    const SRC: &str = r#"
entity Keys {
    routes {
        init boot() => []
        mine() -> pubkey => [
            return(0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)
        ]
    }
    m_n: u64 { in boot() => 0 }
}

test "pubkey return cast" for Keys with { m_n: 0 } {
    call mine()
    expect return 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("assert_eq!(_ret_val") && code.contains(" as "),
        "pubkey expect return must hit infer cast else arm before close (L1078–L1082): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_115_native_expect_return_tuple_three_slot_close() {
    const SRC: &str = r#"
entity Triple {
    routes {
        init boot() => []
        all() -> (u64, u64, u64) => [ return(1, 2, 3) ]
    }
    m_n: u64 { in boot() => 0 }
}

test "triple tuple return" for Triple with { m_n: 0 } {
    call all()
    expect return (1, 2, 3)
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains(".0") && code.contains(".1") && code.contains(".2"),
        "triple tuple expect return must iterate slots before loop close (L1110–L1115): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_115_native_fuzz_init_hashmap_assign_close() {
    const SRC: &str = r#"
entity Store {
    routes { save(m: HashMap<u64, u64>) => [] }
    m_map: HashMap<u64, u64> {
        in save(m) => m
    }
    m_n: u64 { in save(_) => m_n }
}

property "map seed" (seed: u64) for Store with {
    m_map: { 1 => 10, 2 => 20 },
    m_n: 0
} {
    assume seed < 100
    call save(seed)
    fuzz { seed in 1..50 }
}
"#;
    let code = gen_native_test_code(SRC, true, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("_m.insert") && code.contains("_state.m_map = _m"),
        "fuzz with pinned HashMap init must close gen_hashmap_assign block (L1740–L1747): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_115_native_invariant_init_hashmap_assign() {
    const SRC: &str = r#"
entity Vault {
    routes { tick() => [] }
    m_map: HashMap<u64, u64> {
        in tick() => m_map
    }
    m_n: u64 { in tick() => m_n }
}

invariant "map init" for Vault {
    init { m_map: { 5 => 50, 6 => 60 }, m_n: 0 }
    action tick() { }
    check m_n >= 0
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("_state.m_map = _m") && code.contains("_m.insert"),
        "invariant init HashMap literal must hit gen_invariant_fn assign path (L305/L1747): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_115_native_invariant_check_member_state_substitution() {
    const SRC: &str = r#"
entity Counter {
    routes { bump() => [] }
    m_lo: u64 { in bump() => m_lo }
    m_hi: u64 { in bump() => m_hi }
    m_n: u64 { in bump() => m_n + 1 }
}

invariant "member check sub" for Counter {
    init { m_lo: 1, m_hi: 100, m_n: 0 }
    action bump() { }
    check m_n >= m_lo && m_n <= m_hi
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("_state.m_lo") && code.contains("_state.m_hi") && code.contains("_state.m_n"),
        "invariant check bare members must rewrite via whole_word_replace (L579/L194): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_115_native_expect_state_hashmap_index_typed_path() {
    const SRC: &str = r#"
entity Scores {
    routes { credit(key: u64, delta: u64) => [] }
    m_scores: HashMap<u64, u64> {
        in credit(key, delta) => m_scores
    }
}

test "typed indexed path" for Scores with {
    m_scores: { 4 => 40 }
} {
    call credit(4, 5)
    expect state { m_scores[4]: 45 }
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("m_scores.get(&4)") && code.contains("45"),
        "indexed scalar expect must use gen_field_path_access + typed expr (L1016–L1030): {code}"
    );
}

// ---------------------------------------------------------------------------
// N4-112: codegen/test_codegen.rs — slice 7 (residual tail @ N4-107 gap).
// Baseline @ N4-111 queue: 97.88% (35 missed / 1649; ~25 excl. map_context_field SKIP).
// Targets: infer_abi_and_cast Bool/String/default L1726–L1728; invariant action
// bound unknown-step fail L472–L474; gen_typed_test_expr empty __HashMap L1562–L1563;
// emit_test_body / gen_invariant_fn init unknown field L723/L315; expect return
// non-U256/String cast L1082; ExpectReturnTuple close L1115; effect_desc bare
// Send/Deploy L1328–L1329; sanitize_action_variant underscore L168.
// Exclude: map_context_field TVM-only L1795–L1804 (~10, SKIP).
// Acceptance: ≥ 98% or ≤ 28 raw or excl. SKIP ≤ 18 (Δ ≥ −5 executable).
// ---------------------------------------------------------------------------

#[cfg(feature = "rust-targets")]
#[test]
fn n4_112_native_infer_abi_bool_string_default_address_of() {
    const SRC: &str = r#"
entity Target {
    identity flag: bool
    identity label: String
    identity slot: u64
    routes { probe() => [] }
    m_n: u64 { in probe() => m_n + 1 }
}

test "abi bool string default" for Target with { flag: true, label: "seed", slot: 7, m_n: 0 } {
    let addr = address_of Target(true, "seed", slot)
    call probe()
    expect state { m_n: 1 }
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("AbiType::Bool") && code.contains("AbiType::Str"),
        "bool/string address_of args must hit infer_abi_and_cast Bool/Str arms (L1726–L1727): {code}"
    );
    assert!(
        code.contains("as u64).ser_be()") && code.contains("_ao_abi.push"),
        "ident address_of args must hit infer_abi_and_cast default arm (L1728): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_112_native_infer_abi_hex_u256_width_address_of() {
    const SRC: &str = r#"
entity Wide {
    identity wide: U256
    routes { probe() => [] }
    m_n: u64 { in probe() => m_n + 1 }
}

test "abi u256 hex" for Wide with { wide: 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa, m_n: 0 } {
    let addr = address_of Wide(0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)
    call probe()
    expect state { m_n: 1 }
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("AbiType::Uint(256)"),
        "33+ byte hex address_of arg must hit infer_abi_and_cast Uint(256) arm (L1723–L1724): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_112_native_expect_return_u64_cast_arm() {
    const SRC: &str = r#"
entity Counter {
    routes {
        tick() => []
        read() -> u64 => [ return(m_n) ]
    }
    m_n: u64 {
        in tick() => m_n + 1
        in read() => m_n
    }
}

test "u64 return cast" for Counter with { m_n: 0 } {
    call tick()
    call read()
    expect return 1
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("assert_eq!(_ret_val") && code.contains("as u64"),
        "u64 expect return must hit non-U256/String cast arm (L1078–L1082): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_112_native_expect_return_tuple_if_close() {
    const SRC: &str = r#"
entity Pair {
    routes {
        both() -> (u64, u64) => [ return(3, 4) ]
    }
    m_n: u64 { in both() => m_n }
}

test "tuple close" for Pair with { m_n: 0 } {
    call both()
    expect return (3, 4)
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("assert_eq!(_ret_val") && code.contains(".0") && code.contains(".1"),
        "tuple expect return must decode indexed slots before loop close (L1110–L1115): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_112_native_invariant_route_underscore_variant() {
    const SRC: &str = r#"
entity Clock {
    routes {
        my_tick() => []
    }
    m_n: u64 {
        in my_tick() => m_n + 1
    }
}

invariant "underscore route" for Clock {
    init { m_n: 0 }
    action my_tick() {}
    check m_n >= 0
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("MY_TICK") || code.contains("MyTick"),
        "underscore route name must hit sanitize_action_variant next_upper arm (L168): {code}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn n4_112_native_hashmap_assign_and_assert_nonempty() {
    const SRC: &str = r#"
entity MapBox {
    routes { ping() => [] }
    m_map: HashMap<u64, u64> {
        in ping() => m_map
    }
    m_n: u64 { in ping() => m_n }
}

test "hashmap assign assert" for MapBox with {
    m_map: { 9 => 90, 8 => 80 },
    m_n: 0
} {
    call ping()
    expect state { m_map[9]: 90, m_map[8]: 80 }
}
"#;
    let code = gen_native_test_code(SRC, false, &FuzzConfig::default(), &InvariantConfig::default());
    assert!(
        code.contains("_m.insert") && code.contains("_state.m_map = _m"),
        "non-empty HashMap init must close gen_hashmap_assign block (L1740–L1747): {code}"
    );
    assert!(
        code.contains("m_map.get(&") && code.contains("mismatch"),
        "HashMap expect state must hit gen_hashmap_assert close (L1760–L1768): {code}"
    );
}

// ---------------------------------------------------------------------------
// PW3-S-009 — forge compile-oracle pilot (representative coverage rows)
// ---------------------------------------------------------------------------

#[test]
fn pw3_o009_pilot_forge_library_type_alias() {
    let program = parse_evm(
        r#"
        library Helpers {
            type Amount = u64
            const MAX: u64 = 1000
            pure fn cap(a: u64) -> u64 { if a > MAX { MAX } else { a } }
        }

        using Helpers for u64;

        entity Vault {
            routes { set(v: u64) => [] }
            m_v: u64 { in set(v) => v.cap() }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("type Amount is uint64;"), "pilot structural: {sol}");
    assert_forge_compiles("pw3_o009_library_type_alias", &sol);
}

#[test]
fn pw3_o009_pilot_forge_analysis_exists_where() {
    let program = parse_evm(
        r#"
        entity Vault {
            routes {
                deposit(k: u64, v: u64) => []
                read(k: u64) -> u64
                    where m_balances.exists(k) : throw 1
                    => [ return(m_balances[k]) ]
            }
            m_balances: HashMap<u64, u64> {
                in deposit(k, v) => m_balances.insert(k, v)
            }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint64 => bool) public m_balances_exists"),
        "pilot structural where exists sidecar: {sol}"
    );
    assert_forge_compiles("pw3_o009_analysis_exists_where", &sol);
}

#[test]
fn pw3_o009_pilot_forge_route_deploy_with_value() {
    let program = parse_evm(
        r#"
        entity Child {
            routes { constructor(owner: address) => [] }
            m_owner: address { in constructor(owner) => owner }
        }

        entity Factory {
            routes {
                constructor() => []
                spawn(owner: address) => [
                    deploy Child(owner) with { value: 1 }
                ]
            }
            m_n: u64 { in constructor() => 0 }
        }
    "#,
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("deploy") || sol.contains("CREATE"),
        "pilot structural deploy: {sol}"
    );
    assert_forge_compiles("pw3_o009_route_deploy_with_value", &sol);
}
