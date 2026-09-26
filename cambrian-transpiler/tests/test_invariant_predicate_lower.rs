// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! INV-TYPED Tier A — invariant `check` → Foundry `require(...)` emission oracle.
//! See `docs/plans/invariant-check-typed-lowering.md`.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use cambrian_transpiler::ast::Program;
use cambrian_transpiler::codegen::evm_test_codegen::generate_evm_tests;
use cambrian_transpiler::codegen::gen_evm_solidity;
use cambrian_transpiler::project::InvariantConfig;
use cambrian_transpiler::using_rewrite::apply_using_rewrites;
use cambrian_transpiler::ProgramParser;

const ADDR_BEEF: &str =
    "0x000000000000000000000000000000000000000000000000000000000000beef";
const ADDR_A01: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000a01";
const ADDR_A02: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000a02";
const ADDR_OWNER: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000001";

fn parse_evm(src: &str) -> Program {
    let mut program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    apply_using_rewrites(&mut program);
    program
}

fn invariant_handler_sol(program: &Program) -> String {
    generate_evm_tests(program, true, &InvariantConfig::default())
        .into_iter()
        .find(|(path, _)| path.starts_with("test/Invariant_"))
        .map(|(_, content)| content)
        .expect("invariant must emit test/Invariant_*.t.sol")
}

fn require_lines(sol: &str) -> Vec<&str> {
    sol.lines()
        .map(str::trim)
        .filter(|l| l.starts_with("require("))
        .collect()
}

fn join_requires(sol: &str) -> String {
    require_lines(sol).join("\n")
}

/// Bare decimal `48879` is `0xbeef` mis-lowering; invariant address checks must not use it.
fn assert_no_mislowered_beef_decimal(requires: &str) {
    assert!(
        !requires.contains("== 48879") && !requires.contains("==48879"),
        "address literal 0xbeef must not lower as decimal 48879:\n{requires}"
    );
}

fn assert_address_coercion_present(requires: &str) {
    assert!(
        requires.contains("address(") || requires.contains("uint160"),
        "expected address coercion in require:\n{requires}"
    );
}

fn assert_not_require_true(requires: &str) {
    assert!(
        !requires.contains("require(true,"),
        "invariant check must not fall back to require(true):\n{requires}"
    );
}

// ---------------------------------------------------------------------------
// pin@HEAD — regression pins (must pass on current hybrid lowerer)
// ---------------------------------------------------------------------------

#[test]
fn inv_addr_01_route_return_eq_hex_literal() {
    let program = parse_evm(&format!(
        r#"
        entity Pair {{
            routes {{
                constructor() => []
                view token0() -> address => [ return(m_token0) ]
            }}
            m_token0: address {{ in constructor() => {ADDR_BEEF} }}
        }}
        invariant "token0 pinned" for Pair {{
            init {{ m_token0: {ADDR_BEEF} }}
            action constructor() {{}}
            check token0() == {ADDR_BEEF}
        }}
    "#
    ));
    let requires = join_requires(&invariant_handler_sol(&program));
    assert_no_mislowered_beef_decimal(&requires);
    assert_address_coercion_present(&requires);
}

#[test]
fn inv_qual_01_sut_qualified_member_and_route() {
    let program = parse_evm(
        r#"
        entity Token {
            routes {
                constructor() => []
                view totalSupply() -> U256 => [ return(m_supply) ]
            }
            m_supply: U256 { in constructor() => 0 }
        }
        invariant "qualified" for Token {
            init { m_supply: 0 }
            action constructor() {}
            check m_supply == totalSupply()
        }
    "#,
    );
    let requires = join_requires(&invariant_handler_sol(&program));
    assert!(
        requires.contains("_token.m_supply()") && requires.contains("_token.totalSupply()"),
        "inline check must emit SUT-qualified accessors (no substitute pass):\n{requires}"
    );
}

#[test]
fn inv_addr_02_member_eq_hex_literal() {
    let program = parse_evm(&format!(
        r#"
        entity Vault {{
            routes {{ constructor() => [] }}
            m_owner: address {{ in constructor() => {ADDR_OWNER} }}
        }}
        invariant "owner pinned" for Vault {{
            init {{ m_owner: {ADDR_OWNER} }}
            action constructor() {{}}
            check m_owner == {ADDR_OWNER}
        }}
    "#
    ));
    let requires = join_requires(&invariant_handler_sol(&program));
    assert_address_coercion_present(&requires);
    assert_not_require_true(&requires);
}

#[test]
fn inv_sum_01_balance_of_two_addresses_plus_zero() {
    let program = parse_evm(&format!(
        r#"
        entity Token {{
            routes {{
                constructor() => []
                view balanceOf(who: address) -> U256 => [ return(0) ]
            }}
            m_supply: U256 {{ in constructor() => 0 }}
        }}
        invariant "sum pinned" for Token {{
            init {{ m_supply: 0 }}
            action constructor() {{}}
            check balanceOf({ADDR_A01}) + balanceOf({ADDR_A02}) == 0
        }}
    "#
    ));
    let requires = join_requires(&invariant_handler_sol(&program));
    assert!(
        !requires.contains("2561") && !requires.contains("2562"),
        "address args must not lower as bare decimals:\n{requires}"
    );
    assert_address_coercion_present(&requires);
}

#[test]
fn inv_route_01_total_supply_eq_zero() {
    let program = parse_evm(
        r#"
        entity Token {
            routes {
                constructor() => []
                view totalSupply() -> U256 => [ return(m_supply) ]
            }
            m_supply: U256 { in constructor() => 0 }
        }
        invariant "supply pinned" for Token {
            init { m_supply: 0 }
            action constructor() {}
            check totalSupply() == 0
        }
    "#,
    );
    let requires = join_requires(&invariant_handler_sol(&program));
    assert!(requires.contains("== 0"), "numeric compare:\n{requires}");
    assert_not_require_true(&requires);
}

#[test]
fn inv_ne_01_member_ne_zero_address() {
    let program = parse_evm(&format!(
        r#"
        entity Vault {{
            routes {{ constructor() => [] }}
            m_asset: address {{ in constructor() => {ADDR_OWNER} }}
        }}
        invariant "asset ne" for Vault {{
            init {{ m_asset: {ADDR_OWNER} }}
            action constructor() {{}}
            check m_asset != 0x0000000000000000000000000000000000000000000000000000000000000000
        }}
    "#
    ));
    let requires = join_requires(&invariant_handler_sol(&program));
    assert_address_coercion_present(&requires);
    assert!(requires.contains("!="), "Ne compare:\n{requires}");
}

#[test]
fn inv_multi_01_qualified_member_eq_hex() {
    let program = parse_evm(&format!(
        r#"
        entity Tok {{
            routes {{ constructor() => [] }}
            m_owner: address {{ in constructor() => {ADDR_OWNER} }}
        }}
        entity Fac {{
            routes {{ constructor() => [] }}
            m_x: u64 {{ in constructor() => 0 }}
        }}
        invariant "multi member" for {{ tok: Tok, fac: Fac }} {{
            init tok {{ m_owner: {ADDR_OWNER} }}
            init fac {{ m_x: 0 }}
            action tok.constructor() {{}}
            action fac.constructor() {{}}
            check tok.m_owner == {ADDR_OWNER}
        }}
    "#
    ));
    let requires = join_requires(&invariant_handler_sol(&program));
    assert!(requires.contains("_tok.m_owner()"), "multi member:\n{requires}");
    assert_address_coercion_present(&requires);
}

// ---------------------------------------------------------------------------
// red@HEAD — drivers for INV-TYPED Phase 2 (expected to fail until unified lowerer)
// ---------------------------------------------------------------------------

#[test]
fn inv_multi_02_qualified_route_call_address_arg() {
    let program = parse_evm(&format!(
        r#"
        entity Tok {{
            routes {{
                constructor() => []
                view balanceOf(who: address) -> U256 => [ return(0) ]
            }}
            m_x: U256 {{ in constructor() => 0 }}
        }}
        entity Vlt {{
            routes {{
                constructor() => []
                view totalAssets() -> U256 => [ return(m_total) ]
            }}
            m_total: U256 {{ in constructor() => 0 }}
        }}
        invariant "multi route addr" for {{ tok: Tok, vlt: Vlt }} {{
            init tok {{ m_x: 0 }}
            init vlt {{ m_total: 0 }}
            action tok.constructor() {{}}
            action vlt.constructor() {{}}
            check tok.balanceOf({ADDR_A01}) + vlt.totalAssets() == 0
        }}
    "#
    ));
    let requires = join_requires(&invariant_handler_sol(&program));
    assert!(
        requires.contains("_tok.balanceOf("),
        "multi route call must target instance handle:\n{requires}"
    );
    assert_address_coercion_present(&requires);
    assert!(
        !requires.contains("2561"),
        "address route arg must not be bare decimal:\n{requires}"
    );
}

#[test]
fn inv_lit_left_01_hex_eq_route_return() {
    let program = parse_evm(&format!(
        r#"
        entity Pair {{
            routes {{
                constructor() => []
                view token0() -> address => [ return(m_token0) ]
            }}
            m_token0: address {{ in constructor() => {ADDR_BEEF} }}
        }}
        invariant "literal left" for Pair {{
            init {{ m_token0: {ADDR_BEEF} }}
            action constructor() {{}}
            check {ADDR_BEEF} == token0()
        }}
    "#
    ));
    let requires = join_requires(&invariant_handler_sol(&program));
    assert_no_mislowered_beef_decimal(&requires);
    assert_address_coercion_present(&requires);
}

#[test]
fn inv_nolower_01_no_silent_true_fallback() {
    let program = parse_evm(
        r#"
        entity E {
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 0 }
        }
        invariant "no true" for E {
            init { m_x: 0 }
            action constructor() {}
            check m_x == 0
        }
    "#,
    );
    let requires = join_requires(&invariant_handler_sol(&program));
    assert_not_require_true(&requires);
}

// ---------------------------------------------------------------------------
// Tier A expansion (§9.4) — pin@HEAD after invariant_predicate_lower wiring
// ---------------------------------------------------------------------------

#[test]
fn inv_sum_02_three_term_add_with_route_return() {
    let program = parse_evm(&format!(
        r#"
        entity Token {{
            routes {{
                constructor() => []
                view balanceOf(who: address) -> U256 => [ return(0) ]
                view totalSupply() -> U256 => [ return(m_supply) ]
            }}
            m_supply: U256 {{ in constructor() => 0 }}
        }}
        invariant "sum3" for Token {{
            init {{ m_supply: 0 }}
            action constructor() {{}}
            check balanceOf({ADDR_A01}) + balanceOf({ADDR_A02}) + totalSupply() == 0
        }}
    "#
    ));
    let requires = join_requires(&invariant_handler_sol(&program));
    assert_address_coercion_present(&requires);
    assert!(
        !requires.contains("2561") && !requires.contains("2562"),
        "address args must not lower as bare decimals:\n{requires}"
    );
    assert_not_require_true(&requires);
}

#[test]
fn inv_from_01_address_args_in_and_chain() {
    let program = parse_evm(&format!(
        r#"
        entity Gate {{
            routes {{
                constructor() => []
                view canSend(who: address) -> bool => [ return(true) ]
                view canReceive(who: address) -> bool => [ return(true) ]
            }}
            m_x: u64 {{ in constructor() => 0 }}
        }}
        invariant "from chain" for Gate {{
            init {{ m_x: 0 }}
            action constructor() {{}}
            check canSend({ADDR_A01}) && canReceive({ADDR_A02})
        }}
    "#
    ));
    let requires = join_requires(&invariant_handler_sol(&program));
    assert_address_coercion_present(&requires);
    assert!(requires.contains("&&"), "logical And:\n{requires}");
    assert_not_require_true(&requires);
}

#[test]
fn inv_nest_01_grouped_sum_eq_route_return() {
    let program = parse_evm(&format!(
        r#"
        entity Token {{
            routes {{
                constructor() => []
                view balanceOf(who: address) -> U256 => [ return(0) ]
                view totalSupply() -> U256 => [ return(m_supply) ]
            }}
            m_supply: U256 {{ in constructor() => 0 }}
        }}
        invariant "nest" for Token {{
            init {{ m_supply: 0 }}
            action constructor() {{}}
            check (balanceOf({ADDR_A01}) + balanceOf({ADDR_A02})) == totalSupply()
        }}
    "#
    ));
    let requires = join_requires(&invariant_handler_sol(&program));
    assert_address_coercion_present(&requires);
    assert!(
        !requires.contains("2561"),
        "grouped add must not coerce address args to decimal:\n{requires}"
    );
}

#[test]
fn inv_len_01_vec_member_len_multi() {
    let program = parse_evm(
        r#"
        entity Fac {
            routes { constructor() => [] }
            m_all_pairs: Vec<address> { in constructor() => array() }
        }
        entity Tok {
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 0 }
        }
        invariant "len" for { fac: Fac, tok: Tok } {
            init tok { m_x: 0 }
            action fac.constructor() {}
            action tok.constructor() {}
            check fac.m_all_pairs.len() == 0
        }
    "#,
    );
    let requires = join_requires(&invariant_handler_sol(&program));
    assert!(
        requires.contains(".length()") && requires.contains("_fac.m_all_pairs"),
        "Vec member len on multi instance:\n{requires}"
    );
    assert_not_require_true(&requires);
}

#[test]
fn inv_cmp_01_multi_member_ge_literal() {
    let program = parse_evm(
        r#"
        entity Tok {
            routes { constructor() => [] }
            m_total_supply: U256 { in constructor() => 100 }
        }
        entity Fac {
            routes { constructor() => [] }
            m_x: u64 { in constructor() => 0 }
        }
        invariant "cmp" for { tok: Tok, fac: Fac } {
            init tok { m_total_supply: 100 }
            init fac { m_x: 0 }
            action tok.constructor() {}
            action fac.constructor() {}
            check tok.m_total_supply >= 100
        }
    "#,
    );
    let requires = join_requires(&invariant_handler_sol(&program));
    assert!(
        requires.contains("_tok.m_total_supply()") && requires.contains(">= 100"),
        "multi member Ge compare:\n{requires}"
    );
}

#[test]
fn inv_index_01_hashmap_address_key() {
    let program = parse_evm(&format!(
        r#"
        entity Ledger {{
            routes {{ constructor() => [] }}
            m_balances: HashMap<address, U256> {{ in constructor() => {{}} }}
        }}
        invariant "index" for Ledger {{
            init {{ m_balances: {{}} }}
            action constructor() {{}}
            check m_balances[{ADDR_A01}] == 0
        }}
    "#
    ));
    let requires = join_requires(&invariant_handler_sol(&program));
    assert_address_coercion_present(&requires);
    assert!(
        requires.contains(".m_balances(") && requires.contains("== 0"),
        "HashMap mapping getter with address key:\n{requires}"
    );
}

#[test]
fn inv_bool_01_member_eq_false_and_not() {
    let program = parse_evm(
        r#"
        entity Pausable {
            routes { constructor() => [] }
            m_paused: bool { in constructor() => false }
        }
        invariant "bool eq" for Pausable {
            init { m_paused: false }
            action constructor() {}
            check m_paused == false
        }
    "#,
    );
    let requires = join_requires(&invariant_handler_sol(&program));
    assert!(
        requires.contains("== false") || requires.contains("== false"),
        "bool member compare:\n{requires}"
    );
    assert_not_require_true(&requires);

    let program_not = parse_evm(
        r#"
        entity Pausable {
            routes { constructor() => [] }
            m_paused: bool { in constructor() => false }
        }
        invariant "bool not" for Pausable {
            init { m_paused: false }
            action constructor() {}
            check !m_paused
        }
    "#,
    );
    let requires_not = join_requires(&invariant_handler_sol(&program_not));
    assert!(
        requires_not.contains("!(") || requires_not.contains("! "),
        "UnaryOp Not:\n{requires_not}"
    );
}

#[test]
fn inv_handler_01_track_binding_eq_route() {
    let program = parse_evm(
        r#"
        entity Token {
            routes {
                constructor() => []
                view totalSupply() -> U256 => [ return(m_supply) ]
            }
            m_supply: U256 { in constructor() => 0 }
        }
        invariant "handler" for Token {
            init { m_supply: 0 }
            action constructor() {}
            track { let s = totalSupply(); }
            check s == totalSupply()
        }
    "#,
    );
    let requires = join_requires(&invariant_handler_sol(&program));
    assert!(
        requires.contains("_handler.s()") && requires.contains("totalSupply()"),
        "track binding rewritten to handler getter:\n{requires}"
    );
    assert_not_require_true(&requires);
}

#[test]
fn inv_pf_01_pure_fn_mixed_address_u64_args() {
    let program = parse_evm(&format!(
        r#"
        pure fn pf(who: address, n: u64) -> U256 {{
            0
        }}
        entity E {{
            routes {{ constructor() => [] }}
            m_x: U256 {{ in constructor() => 0 }}
        }}
        invariant "pf" for E {{
            init {{ m_x: 0 }}
            action constructor() {{}}
            check pf({ADDR_A01}, 5) == 0
        }}
    "#
    ));
    let requires = join_requires(&invariant_handler_sol(&program));
    assert_address_coercion_present(&requires);
    assert!(
        requires.contains("pf(") && requires.contains("== 0"),
        "pure fn call in check:\n{requires}"
    );
}

#[test]
fn inv_u64_01_narrow_numeric_add() {
    let program = parse_evm(
        r#"
        entity C {
            routes { constructor() => [] }
            m_count: u64 { in constructor() => 4 }
        }
        invariant "u64" for C {
            init { m_count: 4 }
            action constructor() {}
            check m_count + 1 == 5
        }
    "#,
    );
    let requires = join_requires(&invariant_handler_sol(&program));
    assert!(
        requires.contains("+ 1") && requires.contains("== 5"),
        "u64 arithmetic compare:\n{requires}"
    );
    assert_not_require_true(&requires);
}

// ---------------------------------------------------------------------------
// Tier B — forge build gate (opt-in: CAMBRIAN_TEST_COVERAGE_FORGE=1)
// ---------------------------------------------------------------------------

const COVERAGE_FOUNDRY_TOML: &str = r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
evm_version = "prague"
optimizer = false
"#;

fn coverage_forge_enabled() -> bool {
    std::env::var("CAMBRIAN_TEST_COVERAGE_FORGE").as_deref() == Ok("1")
}

fn has_forge() -> bool {
    Command::new("forge")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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
        .args(["install", "foundry-rs/forge-std", "--no-git"])
        .current_dir(out_dir)
        .status()
        .unwrap_or_else(|e| panic!("forge install failed to start: {e}"));
    assert!(status.success(), "forge install forge-std failed");
}

fn shared_forge_std_root() -> std::path::PathBuf {
    static CACHE: OnceLock<std::path::PathBuf> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let dir = std::env::temp_dir().join(format!(
                "cambrian-inv-typed-forge-std-{}",
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
    std::os::unix::fs::symlink(&src, &dst).unwrap();
    #[cfg(not(unix))]
    {
        fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
            std::fs::create_dir_all(dst)?;
            for entry in std::fs::read_dir(src)? {
                let p = entry.path();
                let t = dst.join(entry.file_name());
                if p.is_dir() {
                    copy_dir(&p, &t)?;
                } else {
                    std::fs::copy(&p, &t)?;
                }
            }
            Ok(())
        }
        copy_dir(&src, &dst).unwrap();
    }
}

fn evm_harness_project_files(program: &Program) -> Vec<(String, String)> {
    const PROJECT_FILE: &str = "_Harness_project.sol";
    let code = gen_evm_solidity(program, true);
    let mut files = vec![(format!("src/{PROJECT_FILE}"), code)];
    for entity in &program.entities {
        let stub = format!(
            "// SPDX-License-Identifier: UNLICENSED\npragma solidity ^0.8.24;\nimport \"./{}\";\n",
            PROJECT_FILE
        );
        files.push((format!("src/{}.sol", entity.name), stub));
    }
    files.extend(generate_evm_tests(program, true, &InvariantConfig::default()));
    files
}

fn assert_inv_forge_compiles(label: &str, program: &Program) {
    if !coverage_forge_enabled() {
        eprintln!("skip inv_typed forge gate {label} (set CAMBRIAN_TEST_COVERAGE_FORGE=1)");
        return;
    }
    if !has_forge() {
        panic!("CAMBRIAN_TEST_COVERAGE_FORGE=1 but `forge` not on PATH ({label})");
    }
    let files = evm_harness_project_files(program);
    let out_dir = std::env::temp_dir().join(format!(
        "cambrian-inv-typed-forge-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&out_dir);
    for (rel, contents) in &files {
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
        .unwrap();
    let _ = std::fs::remove_dir_all(&out_dir);
    assert!(
        output.status.success(),
        "forge build rejected {label}:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
}

#[test]
fn inv_typed_forge_addr_route_eq_hex() {
    let program = parse_evm(&format!(
        r#"
        entity Pair {{
            routes {{
                constructor() => []
                view token0() -> address => [ return(m_token0) ]
            }}
            m_token0: address {{ in constructor() => {ADDR_BEEF} }}
        }}
        invariant "token0 pinned" for Pair {{
            init {{ m_token0: {ADDR_BEEF} }}
            action constructor() {{}}
            check token0() == {ADDR_BEEF}
        }}
    "#
    ));
    assert_inv_forge_compiles("inv_addr_01", &program);
}

#[test]
fn inv_typed_forge_sum_two_balance_of() {
    let program = parse_evm(&format!(
        r#"
        entity Token {{
            routes {{
                constructor() => []
                view balanceOf(who: address) -> U256 => [ return(0) ]
            }}
            m_supply: U256 {{ in constructor() => 0 }}
        }}
        invariant "sum" for Token {{
            init {{ m_supply: 0 }}
            action constructor() {{}}
            check balanceOf({ADDR_A01}) + balanceOf({ADDR_A02}) == 0
        }}
    "#
    ));
    assert_inv_forge_compiles("inv_sum_01", &program);
}

#[test]
fn inv_typed_forge_multi_route_address_arg() {
    let program = parse_evm(&format!(
        r#"
        entity Tok {{
            routes {{
                boot() => []
                view balanceOf(who: address) -> U256 => [ return(0) ]
            }}
            m_x: U256 {{ in boot() => 0 }}
        }}
        entity Vlt {{
            routes {{
                boot() => []
                view totalAssets() -> U256 => [ return(m_total) ]
            }}
            m_total: U256 {{ in boot() => 0 }}
        }}
        invariant "multi route" for {{ tok: Tok, vlt: Vlt }} {{
            init tok {{ m_x: 0 }}
            init vlt {{ m_total: 0 }}
            action tok.boot() {{}}
            action vlt.boot() {{}}
            check tok.balanceOf({ADDR_A01}) + vlt.totalAssets() == 0
        }}
    "#
    ));
    assert_inv_forge_compiles("inv_multi_02", &program);
}

#[test]
fn inv_typed_forge_hashmap_address_index() {
    let program = parse_evm(&format!(
        r#"
        entity Ledger {{
            routes {{ constructor() => [] }}
            m_balances: HashMap<address, U256> {{ in constructor() => {{}} }}
        }}
        invariant "index" for Ledger {{
            init {{ m_balances: {{}} }}
            action constructor() {{}}
            check m_balances[{ADDR_A01}] == 0
        }}
    "#
    ));
    assert_inv_forge_compiles("inv_index_01", &program);
}
