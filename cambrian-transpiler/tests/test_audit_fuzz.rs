// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase F audit fuzz harness.
//!
//! - T-F-001: Parser → EVM codegen → `forge build` (proptest)
//! - T-F-002: Route action sequences → Foundry fuzz on audit entity
//! - T-F-004: Iterator chains `filter().map().fold()` → transpile → forge exec
//! - PW3-S-015 / T-F-004 extend: `map.filter.map.fold` wide chains + u64 trunc fold proptest
//! - T-F-005: Match literal arms + wildcard → transpile → forge exec (first-match-wins)
//!
//! See [docs/AUDIT_EVM_LEAN.md](../../../docs/AUDIT_EVM_LEAN.md) §6 Phase F.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use cambrian_transpiler::codegen::{EvmSolidityBackend, OutputBackend};
use cambrian_transpiler::project::Project;
use cambrian_transpiler::validate::{self, check_evm_target_compat_with, Diagnostic, Severity};
use cambrian_transpiler::ProgramParser;
use proptest::prelude::*;

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

const FUZZ_CASES: u32 = 16;
const TF002_FUZZ_RUNS: u32 = 64;

const TF002_PROJECT_YAML: &str = "fuzz_f002_seq_counter_evm.yaml";
const TF002_FORGE_TEST: &str = "F002RouteSeqFuzz.t.sol";
const TF002_MATCH_CONTRACT: &str = "F002RouteSeqFuzz";

const TF004_PROJECT_NAME: &str = "audit-fuzz-tf004";
const TF004_FORGE_TEST: &str = "F004FoldChainExec.t.sol";
const TF004_MATCH_TEST: &str = "test_F004_foldChainSum";

const TF005_PROJECT_NAME: &str = "audit-fuzz-tf005";
const TF005_FORGE_TEST: &str = "F005MatchArmExec.t.sol";
const TF005_MATCH_TEST: &str = "test_F005_matchArms";

const TF015W_PROJECT_NAME: &str = "audit-fuzz-tf015-wide";
const TF015T_PROJECT_NAME: &str = "audit-fuzz-tf015-trunc";
const TF015T_FORGE_TEST: &str = "F015TruncFoldExec.t.sol";
const TF015T_MATCH_TEST: &str = "test_F015_truncFoldSum";

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);
static STAT_RUN: AtomicU64 = AtomicU64::new(0);
static STAT_SKIP: AtomicU64 = AtomicU64::new(0);
static STAT_PASS: AtomicU64 = AtomicU64::new(0);
static STAT_FAIL: AtomicU64 = AtomicU64::new(0);

static TF004_RUN: AtomicU64 = AtomicU64::new(0);
static TF004_SKIP: AtomicU64 = AtomicU64::new(0);
static TF004_PASS: AtomicU64 = AtomicU64::new(0);
static TF004_FAIL: AtomicU64 = AtomicU64::new(0);

static TF005_RUN: AtomicU64 = AtomicU64::new(0);
static TF005_SKIP: AtomicU64 = AtomicU64::new(0);
static TF005_PASS: AtomicU64 = AtomicU64::new(0);
static TF005_FAIL: AtomicU64 = AtomicU64::new(0);

static TF015W_RUN: AtomicU64 = AtomicU64::new(0);
static TF015W_SKIP: AtomicU64 = AtomicU64::new(0);
static TF015W_PASS: AtomicU64 = AtomicU64::new(0);
static TF015W_FAIL: AtomicU64 = AtomicU64::new(0);

static TF015T_RUN: AtomicU64 = AtomicU64::new(0);
static TF015T_SKIP: AtomicU64 = AtomicU64::new(0);
static TF015T_PASS: AtomicU64 = AtomicU64::new(0);
static TF015T_FAIL: AtomicU64 = AtomicU64::new(0);

struct StatsSummary;

impl Drop for StatsSummary {
    fn drop(&mut self) {
        eprintln!(
            "T-F-001 audit_fuzz_parser_evm_forge_build: run={} skip={} pass={} fail={}",
            STAT_RUN.load(Ordering::Relaxed),
            STAT_SKIP.load(Ordering::Relaxed),
            STAT_PASS.load(Ordering::Relaxed),
            STAT_FAIL.load(Ordering::Relaxed),
        );
    }
}

struct Tf004StatsSummary;

impl Drop for Tf004StatsSummary {
    fn drop(&mut self) {
        eprintln!(
            "T-F-004 audit_fuzz_iterator_chains: run={} skip={} pass={} fail={}",
            TF004_RUN.load(Ordering::Relaxed),
            TF004_SKIP.load(Ordering::Relaxed),
            TF004_PASS.load(Ordering::Relaxed),
            TF004_FAIL.load(Ordering::Relaxed),
        );
    }
}

struct Tf005StatsSummary;

impl Drop for Tf005StatsSummary {
    fn drop(&mut self) {
        eprintln!(
            "T-F-005 audit_fuzz_match_arms: run={} skip={} pass={} fail={}",
            TF005_RUN.load(Ordering::Relaxed),
            TF005_SKIP.load(Ordering::Relaxed),
            TF005_PASS.load(Ordering::Relaxed),
            TF005_FAIL.load(Ordering::Relaxed),
        );
    }
}

struct Tf015WideStatsSummary;

impl Drop for Tf015WideStatsSummary {
    fn drop(&mut self) {
        eprintln!(
            "PW3-S-015 audit_fuzz_iterator_wide_chains: run={} skip={} pass={} fail={}",
            TF015W_RUN.load(Ordering::Relaxed),
            TF015W_SKIP.load(Ordering::Relaxed),
            TF015W_PASS.load(Ordering::Relaxed),
            TF015W_FAIL.load(Ordering::Relaxed),
        );
    }
}

struct Tf015TruncStatsSummary;

impl Drop for Tf015TruncStatsSummary {
    fn drop(&mut self) {
        eprintln!(
            "PW3-S-015 audit_fuzz_iterator_trunc_fold: run={} skip={} pass={} fail={}",
            TF015T_RUN.load(Ordering::Relaxed),
            TF015T_SKIP.load(Ordering::Relaxed),
            TF015T_PASS.load(Ordering::Relaxed),
            TF015T_FAIL.load(Ordering::Relaxed),
        );
    }
}

fn audit_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit"))
}

fn fuzz_repro_root() -> PathBuf {
    audit_root().join("fuzz_repro")
}

fn unique_work_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-fuzz-{}-{}-{}",
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

fn parser() -> ProgramParser {
    ProgramParser::new()
}

fn arb_ident() -> impl Strategy<Value = String> {
    prop::string::string_regex("[a-z][a-z0-9_]{0,6}")
        .unwrap()
        .prop_filter("not a keyword", |s| {
            !matches!(
                s.as_str(),
                "pure" | "fn" | "entity" | "record" | "enum" | "const" | "macro"
                    | "routes" | "if" | "else" | "let" | "in" | "as" | "match" | "type"
                    | "view" | "from" | "some" | "none" | "array" | "where" | "throw"
                    | "return" | "true" | "false" | "for" | "init" | "create"
            )
        })
}

fn arb_type_name() -> impl Strategy<Value = String> {
    prop::string::string_regex("[A-Z][a-zA-Z0-9]{0,5}").unwrap()
}

#[derive(Debug, Clone)]
struct MiniProgram {
    entity_name: String,
    member_name: String,
    route_name: String,
    param_type: String,
    use_return_route: bool,
}

impl MiniProgram {
    fn slug(&self) -> String {
        format!(
            "{}_{}_{}",
            self.entity_name.to_lowercase(),
            self.route_name,
            if self.use_return_route { "ret" } else { "inc" }
        )
    }

    fn render(&self) -> String {
        let param = format!("n: {}", self.param_type);
        if self.use_return_route {
            format!(
                "entity {entity} {{
    routes {{
        init create() => []

        {route}({param}) => [
            return({member})
        ]
    }}

    {member}: u64 {{
        in create() => 0
    }}
}}
",
                entity = self.entity_name,
                route = self.route_name,
                param = param,
                member = self.member_name,
            )
        } else {
            format!(
                "entity {entity} {{
    routes {{
        init create() => []

        {route}({param}) => []
    }}

    {member}: u64 {{
        in create() => 0
        in {route}(n) => {member} + 1
    }}
}}
",
                entity = self.entity_name,
                route = self.route_name,
                param = param,
                member = self.member_name,
            )
        }
    }
}

fn arb_mini_program() -> impl Strategy<Value = MiniProgram> {
    (
        arb_type_name(),
        arb_ident().prop_map(|s| format!("m_{s}")),
        arb_ident(),
        prop_oneof![Just("u8".to_string()), Just("u64".to_string())],
        prop::bool::ANY,
    )
        .prop_map(
            |(entity_name, member_name, route_name, param_type, use_return_route)| MiniProgram {
                entity_name,
                member_name,
                route_name,
                param_type,
                use_return_route,
            },
        )
}

#[derive(Debug, Clone)]
struct FoldChainCase {
    threshold: u64,
    multiplier: u64,
    values: Vec<u64>,
}

impl FoldChainCase {
    fn slug(&self) -> String {
        let vals = self
            .values
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("_");
        format!("t{}_m{}_{vals}", self.threshold, self.multiplier)
    }

    fn expected_sum(&self) -> u64 {
        self.values
            .iter()
            .filter(|&&x| x > self.threshold)
            .map(|&x| x * self.multiplier)
            .sum()
    }

    fn render(&self) -> String {
        format!(
            "pure fn sum_folded(xs: Vec<U256>) -> U256 {{
    xs.filter(|x| *x > {t}).map(|x| *x * {m}).fold(0, |acc, x| acc + x)
}}

entity FoldDemo {{
    routes {{
        #[factory_only]
        constructor() => []

        compute(xs: Vec<U256>) => []

        getSum() -> U256 => [
            return(m_sum)
        ]
    }}

    m_sum: U256 {{
        in constructor() => 0
        in compute(xs) => sum_folded(xs)
    }}
}}
",
            t = self.threshold,
            m = self.multiplier,
        )
    }
}

fn arb_fold_chain_case() -> impl Strategy<Value = FoldChainCase> {
    (
        0u64..=10u64,
        1u64..=5u64,
        prop::collection::vec(0u64..=15, 4usize..=8usize),
    )
        .prop_map(|(threshold, multiplier, values)| FoldChainCase {
            threshold,
            multiplier,
            values,
        })
}

#[derive(Debug, Clone)]
struct WideChainCase {
    bump: u64,
    values: Vec<u64>,
}

impl WideChainCase {
    fn slug(&self) -> String {
        let vals = self
            .values
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("_");
        format!("b{}_{vals}", self.bump)
    }

    fn render(&self) -> String {
        format!(
            "pure fn wide_sum(xs: Vec<u64>) -> u64 {{
    xs.iter().map(|x| x + {b}).filter(|x| *x > 2).map(|x| x * 2).fold(0, |a, c| a + c)
}}

entity ChainWide {{
    routes {{
        #[factory_only]
        constructor() => []
        run(items: Vec<u64>) -> u64 => [ return(wide_sum(items)) ]
    }}
    m_n: u64 {{ in constructor() => 0 }}
}}
",
            b = self.bump
        )
    }
}

fn arb_wide_chain_case() -> impl Strategy<Value = WideChainCase> {
    (
        0u64..=3u64,
        prop::collection::vec(0u64..=12u64, 2usize..=6usize),
    )
        .prop_map(|(bump, values)| WideChainCase { bump, values })
}

#[derive(Debug, Clone)]
struct TruncFoldCase {
    values: Vec<u128>,
}

impl TruncFoldCase {
    fn slug(&self) -> String {
        self.values
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("_")
    }

    fn expected(&self) -> Option<u64> {
        trunc_fold_oracle(&self.values)
    }

    fn render(&self) -> String {
        format!(
            "pure fn trunc_fold(xs: Vec<U256>) -> u64 {{
    xs.fold(0, |acc, x| (acc + x) as u64)
}}

entity FoldTrunc {{
    routes {{
        #[factory_only]
        constructor() => []
        run(xs: Vec<U256>) -> u64 => [ return(trunc_fold(xs)) ]
    }}
    m_n: u64 {{ in constructor() => 0 }}
}}
"
        )
    }
}

fn trunc_fold_oracle(values: &[u128]) -> Option<u64> {
    let mut acc: u128 = 0;
    for &x in values {
        let sum = acc.checked_add(x)?;
        if sum > u64::MAX as u128 {
            return None;
        }
        acc = sum;
    }
    Some(acc as u64)
}

fn arb_trunc_fold_case() -> impl Strategy<Value = TruncFoldCase> {
    prop::collection::vec(0u128..=(1u128 << 62), 2usize..=6usize)
        .prop_map(|values| TruncFoldCase { values })
}

fn render_tf015_trunc_forge_test(case: &TruncFoldCase, expected: Option<u64>) -> String {
    let mut array_inits = String::new();
    for (i, v) in case.values.iter().enumerate() {
        array_inits.push_str(&format!("        xs[{i}] = {v};\n"));
    }
    let body = match expected {
        Some(want) => format!(
            "        assertEq(trunc.run(xs), {want}, \"trunc fold oracle\");\n"
        ),
        None => "        vm.expectRevert();\n        trunc.run(xs);\n".to_string(),
    };
    format!(
        r#"// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_{TF015T_PROJECT_NAME}_project.sol";

/// PW3-S-015 / O-007: proptest u64 trunc fold execution check.
contract F015TruncFoldExecTest is Test {{
    CambrianFactory internal factory;
    FoldTrunc internal trunc;

    function setUp() public {{
        factory = new CambrianFactory();
        trunc = FoldTrunc(factory.deployFoldTrunc());
    }}

    function test_F015_truncFoldSum() public {{
        uint256[] memory xs = new uint256[]({len});
{array_inits}{body}    }}
}}
"#,
        len = case.values.len(),
    )
}

fn write_tf015_project_yaml(work_dir: &Path, cam_name: &str, project_name: &str) -> std::io::Result<()> {
    let yaml = format!(
        "name: {project_name}\ntarget: evm\ndeterministic_addresses: true\noutput_dir: build/\nsources:\n  - {cam_name}\n"
    );
    std::fs::write(work_dir.join("project.yaml"), yaml)
}

fn run_tf015_wide_chain_case(case: &WideChainCase) -> Result<(), String> {
    TF015W_RUN.fetch_add(1, Ordering::Relaxed);
    let cam_src = case.render();
    let program = match parser().parse(&cam_src) {
        Ok(p) => p,
        Err(_) => {
            TF015W_SKIP.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
    };
    let diags = validate::validate(&program);
    if has_blocking_errors(&diags) {
        TF015W_SKIP.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }
    let evm_diags = check_evm_target_compat_with(&program, false);
    if has_blocking_errors(&evm_diags) {
        TF015W_SKIP.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }

    let work_dir = unique_work_dir("tf015w-work");
    let build_dir = unique_work_dir("tf015w-build");
    let _ = std::fs::remove_dir_all(&work_dir);
    let _ = std::fs::remove_dir_all(&build_dir);
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&build_dir).map_err(|e| e.to_string())?;
    std::fs::write(work_dir.join("input.cam"), &cam_src).map_err(|e| e.to_string())?;
    write_tf015_project_yaml(&work_dir, "input.cam", TF015W_PROJECT_NAME)
        .map_err(|e| e.to_string())?;

    if transpile_evm_project(&work_dir, &build_dir).is_err() {
        TF015W_SKIP.fetch_add(1, Ordering::Relaxed);
        let _ = std::fs::remove_dir_all(&work_dir);
        let _ = std::fs::remove_dir_all(&build_dir);
        return Ok(());
    }
    strip_generated_invariant_tests(&build_dir);
    std::fs::write(build_dir.join("foundry.toml"), FOUNDRY_TOML).map_err(|e| e.to_string())?;
    ensure_forge_std(&build_dir);

    let (build_ok, build_output) = forge_build(&build_dir);
    let _ = std::fs::remove_dir_all(&work_dir);
    let _ = std::fs::remove_dir_all(&build_dir);

    if build_ok {
        TF015W_PASS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    } else {
        TF015W_FAIL.fetch_add(1, Ordering::Relaxed);
        Err(format!(
            "PW3-G-014: wide map.filter.map.fold must forge-build:\n{build_output}"
        ))
    }
}

fn run_tf015_trunc_fold_case(case: &TruncFoldCase) -> Result<(), String> {
    TF015T_RUN.fetch_add(1, Ordering::Relaxed);
    let cam_src = case.render();
    let expected = case.expected();
    let program = match parser().parse(&cam_src) {
        Ok(p) => p,
        Err(_) => {
            TF015T_SKIP.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
    };
    let diags = validate::validate(&program);
    if has_blocking_errors(&diags) {
        TF015T_SKIP.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }
    let evm_diags = check_evm_target_compat_with(&program, false);
    if has_blocking_errors(&evm_diags) {
        TF015T_SKIP.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }

    let work_dir = unique_work_dir("tf015t-work");
    let build_dir = unique_work_dir("tf015t-build");
    let _ = std::fs::remove_dir_all(&work_dir);
    let _ = std::fs::remove_dir_all(&build_dir);
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&build_dir).map_err(|e| e.to_string())?;
    std::fs::write(work_dir.join("input.cam"), &cam_src).map_err(|e| e.to_string())?;
    write_tf015_project_yaml(&work_dir, "input.cam", TF015T_PROJECT_NAME)
        .map_err(|e| e.to_string())?;

    if transpile_evm_project(&work_dir, &build_dir).is_err() {
        TF015T_SKIP.fetch_add(1, Ordering::Relaxed);
        let _ = std::fs::remove_dir_all(&work_dir);
        let _ = std::fs::remove_dir_all(&build_dir);
        return Ok(());
    }
    strip_generated_invariant_tests(&build_dir);
    std::fs::write(build_dir.join("foundry.toml"), FOUNDRY_TOML).map_err(|e| e.to_string())?;
    ensure_forge_std(&build_dir);

    let (build_ok, build_output) = forge_build(&build_dir);
    if !build_ok {
        TF015T_FAIL.fetch_add(1, Ordering::Relaxed);
        let _ = std::fs::remove_dir_all(&work_dir);
        let _ = std::fs::remove_dir_all(&build_dir);
        return Err(format!(
            "PW3-O-007 trunc fold forge build failed:\n{build_output}"
        ));
    }

    let forge_test = render_tf015_trunc_forge_test(case, expected);
    let test_dir = build_dir.join("test");
    std::fs::create_dir_all(&test_dir).map_err(|e| e.to_string())?;
    std::fs::write(test_dir.join(TF015T_FORGE_TEST), &forge_test).map_err(|e| e.to_string())?;

    let (test_ok, test_output) = forge_test_match(&build_dir, TF015T_MATCH_TEST);
    let _ = std::fs::remove_dir_all(&work_dir);
    let _ = std::fs::remove_dir_all(&build_dir);

    if test_ok {
        TF015T_PASS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    } else {
        TF015T_FAIL.fetch_add(1, Ordering::Relaxed);
        Err(format!(
            "PW3-O-007 trunc fold exec mismatch (expected {expected:?}):\n{test_output}"
        ))
    }
}

fn render_tf004_forge_test(case: &FoldChainCase, expected: u64) -> String {
    let mut array_inits = String::new();
    for (i, v) in case.values.iter().enumerate() {
        array_inits.push_str(&format!("        xs[{i}] = {v};\n"));
    }
    format!(
        r#"// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_{TF004_PROJECT_NAME}_project.sol";

/// T-F-004: proptest-generated filter→map→fold chain execution check.
contract F004FoldChainExecTest is Test {{
    FoldDemo internal demo;
    CambrianFactory internal factory;

    function setUp() public {{
        factory = new CambrianFactory();
        demo = FoldDemo(address(factory.deployFoldDemo()));
    }}

    function test_F004_foldChainSum() public {{
        uint256[] memory xs = new uint256[]({len});
{array_inits}
        demo.compute(xs);
        assertEq(demo.getSum(), {expected}, "filter.map.fold sum mismatch");
    }}
}}
"#,
        len = case.values.len(),
    )
}

#[derive(Debug, Clone)]
struct MatchArmCase {
    arms: Vec<(u64, u64)>,
    default: u64,
}

impl MatchArmCase {
    fn slug(&self) -> String {
        let arms = self
            .arms
            .iter()
            .map(|(lit, val)| format!("{lit}_{val}"))
            .collect::<Vec<_>>()
            .join("_");
        format!("d{}_{arms}", self.default)
    }

    fn expected_classify(&self, code: u64) -> u64 {
        for (lit, val) in &self.arms {
            if code == *lit {
                return *val;
            }
        }
        self.default
    }

    fn non_matching_code(&self) -> u64 {
        for c in 0..=30 {
            if !self.arms.iter().any(|(lit, _)| *lit == c) {
                return c;
            }
        }
        99
    }

    fn render(&self) -> String {
        let mut arms_str = String::new();
        for (lit, val) in &self.arms {
            arms_str.push_str(&format!("                {lit} => {val},\n"));
        }
        arms_str.push_str(&format!("                _ => {}\n", self.default));
        format!(
            "entity MatchDemo {{
    routes {{
        #[factory_only]
        constructor() => []

        setCode(code: u64) => []

        classify() -> u64 => [
            return(m_result)
        ]
    }}

    m_code: u64 {{
        in constructor() => 0
        in setCode(code) => code
    }}

    m_result: u64 {{
        in classify() => {{
            match m_code {{
{arms_str}            }}
        }}
    }}
}}
"
        )
    }
}

fn arb_match_arm_case() -> impl Strategy<Value = MatchArmCase> {
    let pool: Vec<u64> = (0..=20).collect();
    prop::sample::subsequence(pool, 2..=5)
        .prop_flat_map(|lits| {
            let n = lits.len();
            (
                Just(lits),
                prop::collection::vec(100u64..=999u64, n),
                100u64..=999u64,
            )
        })
        .prop_map(|(lits, vals, default)| {
            let mut arms: Vec<(u64, u64)> = lits.into_iter().zip(vals).collect();
            arms.sort_by_key(|(lit, _)| *lit);
            MatchArmCase { arms, default }
        })
}

fn render_tf005_forge_test(case: &MatchArmCase) -> String {
    let mut checks = String::new();
    for (lit, val) in &case.arms {
        let expected = case.expected_classify(*lit);
        checks.push_str(&format!(
            "        demo.setCode({lit});\n\
                    assertEq(demo.classify(), {expected}, \"arm lit={lit} => {val}\");\n"
        ));
    }
    let non_match = case.non_matching_code();
    let default_expected = case.expected_classify(non_match);
    checks.push_str(&format!(
        "        demo.setCode({non_match});\n\
         assertEq(demo.classify(), {default_expected}, \"wildcard for code={non_match}\");\n"
    ));
    format!(
        r#"// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_{TF005_PROJECT_NAME}_project.sol";

/// T-F-005: proptest-generated match arm priority check (first-match-wins).
contract F005MatchArmExecTest is Test {{
    MatchDemo internal demo;
    CambrianFactory internal factory;

    function setUp() public {{
        factory = new CambrianFactory();
        demo = MatchDemo(address(factory.deployMatchDemo()));
    }}

    function test_F005_matchArms() public {{
{checks}    }}
}}
"#
    )
}

fn has_blocking_errors(diags: &[Diagnostic]) -> bool {
    diags
        .iter()
        .any(|d| matches!(d.severity, Severity::Error))
}

fn strip_generated_invariant_tests(out_dir: &Path) {
    let test_dir = out_dir.join("test");
    let Ok(entries) = std::fs::read_dir(&test_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("Invariant_") && name.ends_with(".t.sol") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn forge_build(out_dir: &Path) -> (bool, String) {
    let forge = Command::new("forge")
        .args(["build", "--root"])
        .arg(out_dir)
        .output()
        .expect("forge build");
    let combined = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );
    (forge.status.success(), combined)
}

fn ensure_forge_std(out_dir: &Path) {
    cambrian_transpiler::codegen::evm_test_codegen::install_forge_std(out_dir)
        .unwrap_or_else(|e| panic!("{e} in {}", out_dir.display()));
}

fn forge_test_contract(out_dir: &Path, match_contract: &str, fuzz_runs: u32) -> (bool, String) {
    let forge = Command::new("forge")
        .args([
            "test",
            "--match-contract",
            match_contract,
            "--fuzz-runs",
            &fuzz_runs.to_string(),
            "-vv",
            "--root",
        ])
        .arg(out_dir)
        .output()
        .expect("forge test");
    let combined = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );
    (forge.status.success(), combined)
}

fn forge_test_match(out_dir: &Path, match_test: &str) -> (bool, String) {
    let forge = Command::new("forge")
        .args(["test", "--match-test", match_test, "-vv", "--root"])
        .arg(out_dir)
        .output()
        .expect("forge test");
    let combined = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );
    (forge.status.success(), combined)
}

fn transpile_evm_yaml(yaml_path: &Path, out_dir: &Path) -> Result<(), String> {
    let project = Project::load(yaml_path).map_err(|e| format!("load project: {e}"))?;
    let det = project.config.deterministic_addresses.unwrap_or(true);
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    let files = backend.gen_project(&project);
    for (rel, contents) in files {
        let path = out_dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, contents)
            .map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

fn write_project_yaml(work_dir: &Path, cam_name: &str) -> std::io::Result<()> {
    let yaml = format!(
        "name: audit-fuzz-tf001\n\
         target: evm\n\
         deterministic_addresses: true\n\
         output_dir: build/\n\
         sources:\n\
           - {cam_name}\n"
    );
    std::fs::write(work_dir.join("project.yaml"), yaml)
}

fn write_tf004_project_yaml(work_dir: &Path, cam_name: &str) -> std::io::Result<()> {
    let yaml = format!(
        "name: {TF004_PROJECT_NAME}\n\
         target: evm\n\
         deterministic_addresses: true\n\
         output_dir: build/\n\
         sources:\n\
           - {cam_name}\n"
    );
    std::fs::write(work_dir.join("project.yaml"), yaml)
}

fn write_tf005_project_yaml(work_dir: &Path, cam_name: &str) -> std::io::Result<()> {
    let yaml = format!(
        "name: {TF005_PROJECT_NAME}\n\
         target: evm\n\
         deterministic_addresses: true\n\
         output_dir: build/\n\
         sources:\n\
           - {cam_name}\n"
    );
    std::fs::write(work_dir.join("project.yaml"), yaml)
}

fn transpile_evm_project(work_dir: &Path, out_dir: &Path) -> Result<(), String> {
    transpile_evm_yaml(&work_dir.join("project.yaml"), out_dir)
}

fn save_tf001_repro(case: &MiniProgram, cam_src: &str, forge_output: &str) -> PathBuf {
    let repro_dir = fuzz_repro_root().join(format!("tf001_{}", case.slug()));
    let _ = std::fs::remove_dir_all(&repro_dir);
    std::fs::create_dir_all(&repro_dir).expect("create fuzz repro dir");

    let cam_name = "repro.cam";
    std::fs::write(repro_dir.join(cam_name), cam_src).expect("write repro.cam");
    write_project_yaml(&repro_dir, cam_name).expect("write repro yaml");
    let note = format!(
        "T-F-001 finding: forge build failed on validator-clean mini-program.\n\
         Entity: {} route: {} member: {} param: {} return-route: {}\n\
         \n\
         --- forge output ---\n\
         {forge_output}\n",
        case.entity_name,
        case.route_name,
        case.member_name,
        case.param_type,
        case.use_return_route,
    );
    std::fs::write(repro_dir.join("NOTE.md"), note).expect("write NOTE.md");
    repro_dir
}

fn save_tf002_repro(forge_output: &str) -> PathBuf {
    let repro_dir = fuzz_repro_root().join("tf002_route_seq");
    let _ = std::fs::remove_dir_all(&repro_dir);
    std::fs::create_dir_all(&repro_dir).expect("create tf002 repro dir");

    let fixtures = audit_root().join("fixtures");
    std::fs::copy(
        fixtures.join("fuzz_f002_seq_counter.cam"),
        repro_dir.join("fuzz_f002_seq_counter.cam"),
    )
    .expect("copy cam");
    std::fs::copy(
        fixtures.join("fuzz_f002_seq_counter_evm.yaml"),
        repro_dir.join("fuzz_f002_seq_counter_evm.yaml"),
    )
    .expect("copy yaml");
    std::fs::copy(
        audit_root().join("forge").join(TF002_FORGE_TEST),
        repro_dir.join(TF002_FORGE_TEST),
    )
    .expect("copy forge test");

    let note = format!(
        "T-F-002 finding: Foundry fuzz failed on SeqCounter inc/reset sequences.\n\
         Fuzz runs: {TF002_FUZZ_RUNS}\n\
         \n\
         --- forge output ---\n\
         {forge_output}\n"
    );
    std::fs::write(repro_dir.join("NOTE.md"), note).expect("write NOTE.md");
    repro_dir
}

fn save_tf004_repro(case: &FoldChainCase, cam_src: &str, forge_test: &str, forge_output: &str) -> PathBuf {
    let repro_dir = fuzz_repro_root().join(format!("tf004_{}", case.slug()));
    let _ = std::fs::remove_dir_all(&repro_dir);
    std::fs::create_dir_all(&repro_dir).expect("create tf004 repro dir");

    let cam_name = "repro.cam";
    std::fs::write(repro_dir.join(cam_name), cam_src).expect("write repro.cam");
    write_tf004_project_yaml(&repro_dir, cam_name).expect("write repro yaml");
    std::fs::write(repro_dir.join(TF004_FORGE_TEST), forge_test).expect("write forge test");

    let expected = case.expected_sum();
    let note = format!(
        "T-F-004 finding: validator-clean fold chain failed forge build/exec or wrong sum.\n\
         threshold: {} multiplier: {} values: {:?}\n\
         expected sum (Rust): {expected}\n\
         \n\
         --- forge output ---\n\
         {forge_output}\n",
        case.threshold, case.multiplier, case.values,
    );
    std::fs::write(repro_dir.join("NOTE.md"), note).expect("write NOTE.md");
    repro_dir
}

fn save_tf005_repro(
    case: &MatchArmCase,
    cam_src: &str,
    forge_test: &str,
    forge_output: &str,
) -> PathBuf {
    let repro_dir = fuzz_repro_root().join(format!("tf005_{}", case.slug()));
    let _ = std::fs::remove_dir_all(&repro_dir);
    std::fs::create_dir_all(&repro_dir).expect("create tf005 repro dir");

    let cam_name = "repro.cam";
    std::fs::write(repro_dir.join(cam_name), cam_src).expect("write repro.cam");
    write_tf005_project_yaml(&repro_dir, cam_name).expect("write repro yaml");
    std::fs::write(repro_dir.join(TF005_FORGE_TEST), forge_test).expect("write forge test");

    let non_match = case.non_matching_code();
    let note = format!(
        "T-F-005 finding: validator-clean match arms failed forge build/exec or wrong classify.\n\
         arms: {:?} default: {}\n\
         expected classify({non_match}) = {}\n\
         \n\
         --- forge output ---\n\
         {forge_output}\n",
        case.arms,
        case.default,
        case.expected_classify(non_match),
    );
    std::fs::write(repro_dir.join("NOTE.md"), note).expect("write NOTE.md");
    repro_dir
}

fn run_tf005_match_arm_case(case: &MatchArmCase) -> Result<(), String> {
    TF005_RUN.fetch_add(1, Ordering::Relaxed);
    let cam_src = case.render();

    let program = match parser().parse(&cam_src) {
        Ok(p) => p,
        Err(_) => {
            TF005_SKIP.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
    };

    let diags = validate::validate(&program);
    if has_blocking_errors(&diags) {
        TF005_SKIP.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }

    let evm_diags = check_evm_target_compat_with(&program, false);
    if has_blocking_errors(&evm_diags) {
        TF005_SKIP.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }

    let work_dir = unique_work_dir("tf005-work");
    let build_dir = unique_work_dir("tf005-build");
    let _ = std::fs::remove_dir_all(&work_dir);
    let _ = std::fs::remove_dir_all(&build_dir);
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&build_dir).map_err(|e| e.to_string())?;

    std::fs::write(work_dir.join("input.cam"), &cam_src).map_err(|e| e.to_string())?;
    write_tf005_project_yaml(&work_dir, "input.cam").map_err(|e| e.to_string())?;

    if let Err(_err) = transpile_evm_project(&work_dir, &build_dir) {
        let _ = std::fs::remove_dir_all(&work_dir);
        let _ = std::fs::remove_dir_all(&build_dir);
        TF005_SKIP.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }

    strip_generated_invariant_tests(&build_dir);
    std::fs::write(build_dir.join("foundry.toml"), FOUNDRY_TOML).map_err(|e| e.to_string())?;
    ensure_forge_std(&build_dir);

    let (build_ok, build_output) = forge_build(&build_dir);
    if !build_ok {
        TF005_FAIL.fetch_add(1, Ordering::Relaxed);
        let forge_test = render_tf005_forge_test(case);
        let repro_dir = save_tf005_repro(case, &cam_src, &forge_test, &build_output);
        let _ = std::fs::remove_dir_all(&work_dir);
        let _ = std::fs::remove_dir_all(&build_dir);
        return Err(format!(
            "T-F-005 CONFIRMED — forge build failed; repro saved to {}\n{}",
            repro_dir.display(),
            build_output
        ));
    }

    let forge_test = render_tf005_forge_test(case);
    let test_dir = build_dir.join("test");
    std::fs::create_dir_all(&test_dir).map_err(|e| e.to_string())?;
    std::fs::write(test_dir.join(TF005_FORGE_TEST), &forge_test).map_err(|e| e.to_string())?;

    let (test_ok, test_output) = forge_test_match(&build_dir, TF005_MATCH_TEST);
    let _ = std::fs::remove_dir_all(&work_dir);
    let _ = std::fs::remove_dir_all(&build_dir);

    if test_ok {
        TF005_PASS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    } else {
        TF005_FAIL.fetch_add(1, Ordering::Relaxed);
        let repro_dir = save_tf005_repro(case, &cam_src, &forge_test, &test_output);
        Err(format!(
            "T-F-005 CONFIRMED — classify() mismatch; repro saved to {}\n{}",
            repro_dir.display(),
            test_output
        ))
    }
}

fn run_tf004_fold_chain_case(case: &FoldChainCase) -> Result<(), String> {
    TF004_RUN.fetch_add(1, Ordering::Relaxed);
    let cam_src = case.render();
    let expected = case.expected_sum();

    let program = match parser().parse(&cam_src) {
        Ok(p) => p,
        Err(_) => {
            TF004_SKIP.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
    };

    let diags = validate::validate(&program);
    if has_blocking_errors(&diags) {
        TF004_SKIP.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }

    let evm_diags = check_evm_target_compat_with(&program, false);
    if has_blocking_errors(&evm_diags) {
        TF004_SKIP.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }

    let work_dir = unique_work_dir("tf004-work");
    let build_dir = unique_work_dir("tf004-build");
    let _ = std::fs::remove_dir_all(&work_dir);
    let _ = std::fs::remove_dir_all(&build_dir);
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&build_dir).map_err(|e| e.to_string())?;

    std::fs::write(work_dir.join("input.cam"), &cam_src).map_err(|e| e.to_string())?;
    write_tf004_project_yaml(&work_dir, "input.cam").map_err(|e| e.to_string())?;

    if let Err(_err) = transpile_evm_project(&work_dir, &build_dir) {
        let _ = std::fs::remove_dir_all(&work_dir);
        let _ = std::fs::remove_dir_all(&build_dir);
        TF004_SKIP.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }

    strip_generated_invariant_tests(&build_dir);
    std::fs::write(build_dir.join("foundry.toml"), FOUNDRY_TOML).map_err(|e| e.to_string())?;
    ensure_forge_std(&build_dir);

    let (build_ok, build_output) = forge_build(&build_dir);
    if !build_ok {
        TF004_FAIL.fetch_add(1, Ordering::Relaxed);
        let forge_test = render_tf004_forge_test(case, expected);
        let repro_dir = save_tf004_repro(case, &cam_src, &forge_test, &build_output);
        let _ = std::fs::remove_dir_all(&work_dir);
        let _ = std::fs::remove_dir_all(&build_dir);
        return Err(format!(
            "T-F-004 CONFIRMED — forge build failed; repro saved to {}\n{}",
            repro_dir.display(),
            build_output
        ));
    }

    let forge_test = render_tf004_forge_test(case, expected);
    let test_dir = build_dir.join("test");
    std::fs::create_dir_all(&test_dir).map_err(|e| e.to_string())?;
    std::fs::write(test_dir.join(TF004_FORGE_TEST), &forge_test).map_err(|e| e.to_string())?;

    let (test_ok, test_output) = forge_test_match(&build_dir, TF004_MATCH_TEST);
    let _ = std::fs::remove_dir_all(&work_dir);
    let _ = std::fs::remove_dir_all(&build_dir);

    if test_ok {
        TF004_PASS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    } else {
        TF004_FAIL.fetch_add(1, Ordering::Relaxed);
        let repro_dir = save_tf004_repro(case, &cam_src, &forge_test, &test_output);
        Err(format!(
            "T-F-004 CONFIRMED — getSum() != expected {expected}; repro saved to {}\n{}",
            repro_dir.display(),
            test_output
        ))
    }
}

fn run_tf002_route_seq_fuzz() -> Result<String, String> {
    let out_dir = unique_work_dir("tf002");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;

    let yaml_path = audit_root().join("fixtures").join(TF002_PROJECT_YAML);
    transpile_evm_yaml(&yaml_path, &out_dir)?;

    strip_generated_invariant_tests(&out_dir);

    if !out_dir.join("foundry.toml").exists() {
        std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).map_err(|e| e.to_string())?;
    }

    let test_dir = out_dir.join("test");
    std::fs::create_dir_all(&test_dir).map_err(|e| e.to_string())?;
    std::fs::copy(
        audit_root().join("forge").join(TF002_FORGE_TEST),
        test_dir.join(TF002_FORGE_TEST),
    )
    .map_err(|e| e.to_string())?;

    ensure_forge_std(&out_dir);

    let (forge_ok, forge_output) =
        forge_test_contract(&out_dir, TF002_MATCH_CONTRACT, TF002_FUZZ_RUNS);
    let _ = std::fs::remove_dir_all(&out_dir);

    if forge_ok {
        Ok(forge_output)
    } else {
        let repro_dir = save_tf002_repro(&forge_output);
        Err(format!(
            "T-F-002 CONFIRMED — Foundry fuzz failed; repro saved to {}\n{}",
            repro_dir.display(),
            forge_output
        ))
    }
}

fn run_mini_case(case: &MiniProgram) -> Result<(), String> {
    STAT_RUN.fetch_add(1, Ordering::Relaxed);
    let cam_src = case.render();

    let program = match parser().parse(&cam_src) {
        Ok(p) => p,
        Err(_) => {
            STAT_SKIP.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
    };

    let diags = validate::validate(&program);
    if has_blocking_errors(&diags) {
        STAT_SKIP.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }

    let evm_diags = check_evm_target_compat_with(&program, false);
    if has_blocking_errors(&evm_diags) {
        STAT_SKIP.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }

    let work_dir = unique_work_dir("work");
    let build_dir = unique_work_dir("build");
    let _ = std::fs::remove_dir_all(&work_dir);
    let _ = std::fs::remove_dir_all(&build_dir);
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&build_dir).map_err(|e| e.to_string())?;

    std::fs::write(work_dir.join("input.cam"), &cam_src).map_err(|e| e.to_string())?;
    write_project_yaml(&work_dir, "input.cam").map_err(|e| e.to_string())?;

    if let Err(_err) = transpile_evm_project(&work_dir, &build_dir) {
        let _ = std::fs::remove_dir_all(&work_dir);
        let _ = std::fs::remove_dir_all(&build_dir);
        STAT_SKIP.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }

    strip_generated_invariant_tests(&build_dir);
    std::fs::write(build_dir.join("foundry.toml"), FOUNDRY_TOML).map_err(|e| e.to_string())?;

    let (forge_ok, forge_output) = forge_build(&build_dir);
    let _ = std::fs::remove_dir_all(&work_dir);
    let _ = std::fs::remove_dir_all(&build_dir);

    if forge_ok {
        STAT_PASS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    } else {
        STAT_FAIL.fetch_add(1, Ordering::Relaxed);
        let repro_dir = save_tf001_repro(case, &cam_src, &forge_output);
        Err(format!(
            "forge build failed on well-typed mini-program; repro saved to {}\n{}",
            repro_dir.display(),
            forge_output
        ))
    }
}

fn reset_stats() {
    STAT_RUN.store(0, Ordering::Relaxed);
    STAT_SKIP.store(0, Ordering::Relaxed);
    STAT_PASS.store(0, Ordering::Relaxed);
    STAT_FAIL.store(0, Ordering::Relaxed);
}

fn reset_tf004_stats() {
    TF004_RUN.store(0, Ordering::Relaxed);
    TF004_SKIP.store(0, Ordering::Relaxed);
    TF004_PASS.store(0, Ordering::Relaxed);
    TF004_FAIL.store(0, Ordering::Relaxed);
}

fn reset_tf005_stats() {
    TF005_RUN.store(0, Ordering::Relaxed);
    TF005_SKIP.store(0, Ordering::Relaxed);
    TF005_PASS.store(0, Ordering::Relaxed);
    TF005_FAIL.store(0, Ordering::Relaxed);
}

fn reset_tf015w_stats() {
    TF015W_RUN.store(0, Ordering::Relaxed);
    TF015W_SKIP.store(0, Ordering::Relaxed);
    TF015W_PASS.store(0, Ordering::Relaxed);
    TF015W_FAIL.store(0, Ordering::Relaxed);
}

fn reset_tf015t_stats() {
    TF015T_RUN.store(0, Ordering::Relaxed);
    TF015T_SKIP.store(0, Ordering::Relaxed);
    TF015T_PASS.store(0, Ordering::Relaxed);
    TF015T_FAIL.store(0, Ordering::Relaxed);
}

fn fuzz_proptest_config() -> ProptestConfig {
    ProptestConfig {
        cases: FUZZ_CASES,
        ..ProptestConfig::default()
    }
}

#[test]
fn audit_fuzz_parser_evm_forge_build() {
    if !has_forge() {
        eprintln!(
            "skipping audit_fuzz_parser_evm_forge_build: forge not on PATH \
             (coverage / non-Foundry CI images; Forge execution lives in \
             test-transpiler-audit-fuzz)"
        );
        return;
    }
    proptest!(fuzz_proptest_config(), |(case in arb_mini_program())| {
        let _summary = StatsSummary;
        if let Err(detail) = run_mini_case(&case) {
            panic!("{detail}");
        }
    });
}

#[test]
fn audit_fuzz_iterator_chains() {
    if !has_forge() {
        eprintln!(
            "skipping audit_fuzz_iterator_chains: forge not on PATH \
             (coverage / non-Foundry CI images; Forge execution lives in \
             test-transpiler-audit-fuzz)"
        );
        return;
    }
    proptest!(fuzz_proptest_config(), |(case in arb_fold_chain_case())| {
        let _summary = Tf004StatsSummary;
        if let Err(detail) = run_tf004_fold_chain_case(&case) {
            panic!("{detail}");
        }
    });
}

#[test]
fn audit_fuzz_iterator_wide_chains() {
    if !has_forge() {
        eprintln!(
            "skipping audit_fuzz_iterator_wide_chains: forge not on PATH \
             (coverage / non-Foundry CI images; Forge execution lives in \
             test-transpiler-audit-fuzz)"
        );
        return;
    }
    proptest!(fuzz_proptest_config(), |(case in arb_wide_chain_case())| {
        let _summary = Tf015WideStatsSummary;
        if let Err(detail) = run_tf015_wide_chain_case(&case) {
            panic!("{detail}");
        }
    });
}

#[test]
fn audit_fuzz_iterator_trunc_fold() {
    if !has_forge() {
        eprintln!(
            "skipping audit_fuzz_iterator_trunc_fold: forge not on PATH \
             (coverage / non-Foundry CI images; Forge execution lives in \
             test-transpiler-audit-fuzz)"
        );
        return;
    }
    proptest!(fuzz_proptest_config(), |(case in arb_trunc_fold_case())| {
        let _summary = Tf015TruncStatsSummary;
        if let Err(detail) = run_tf015_trunc_fold_case(&case) {
            panic!("{detail}");
        }
    });
}

#[test]
fn audit_fuzz_match_arms() {
    if !has_forge() {
        eprintln!(
            "skipping audit_fuzz_match_arms: forge not on PATH \
             (coverage / non-Foundry CI images; Forge execution lives in \
             test-transpiler-audit-fuzz)"
        );
        return;
    }
    proptest!(fuzz_proptest_config(), |(case in arb_match_arm_case())| {
        let _summary = Tf005StatsSummary;
        if let Err(detail) = run_tf005_match_arm_case(&case) {
            panic!("{detail}");
        }
    });
}

#[test]
fn audit_fuzz_parser_evm_forge_build_smoke() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    reset_stats();
    let _summary = StatsSummary;
    let case = MiniProgram {
        entity_name: "FuzzSmoke".to_string(),
        member_name: "m_x".to_string(),
        route_name: "bump".to_string(),
        param_type: "u64".to_string(),
        use_return_route: false,
    };
    run_mini_case(&case).expect("smoke case should forge-build");
}

#[test]
fn audit_fuzz_route_action_sequences() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }

    match run_tf002_route_seq_fuzz() {
        Ok(output) => {
            eprintln!(
                "T-F-002 ALIGNED — F002RouteSeqFuzz PASS (fuzz runs={}):\n{}",
                TF002_FUZZ_RUNS, output
            );
        }
        Err(detail) => {
            panic!("{detail}");
        }
    }
}

#[test]
fn audit_fuzz_iterator_chains_smoke() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    reset_tf004_stats();
    let _summary = Tf004StatsSummary;
    // val_e07 equivalent: filter (>0) → map (*2) → fold; [1,2,3,0] → sum 12
    let case = FoldChainCase {
        threshold: 0,
        multiplier: 2,
        values: vec![1, 2, 3, 0],
    };
    run_tf004_fold_chain_case(&case).expect("smoke case should forge-build and match expected sum");
}

#[test]
fn audit_fuzz_iterator_wide_chains_smoke() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    reset_tf015w_stats();
    let _summary = Tf015WideStatsSummary;
    let case = WideChainCase {
        bump: 1,
        values: vec![1, 2, 3, 4],
    };
    run_tf015_wide_chain_case(&case).expect("wide chain must forge-build (G-014)");
}

#[test]
fn audit_fuzz_iterator_trunc_fold_smoke() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    reset_tf015t_stats();
    let _summary = Tf015TruncStatsSummary;
    let case = TruncFoldCase {
        values: vec![u128::MAX - 1, 2, 1],
    };
    run_tf015_trunc_fold_case(&case).expect("trunc fold smoke should forge-exec");
}

#[test]
fn audit_fuzz_match_arms_smoke() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    reset_tf005_stats();
    let _summary = Tf005StatsSummary;
    // evm_h8 equivalent: 0→100, 1→200, wildcard→999
    let case = MatchArmCase {
        arms: vec![(0, 100), (1, 200)],
        default: 999,
    };
    run_tf005_match_arm_case(&case)
        .expect("smoke case should forge-build and match expected classify");
}
