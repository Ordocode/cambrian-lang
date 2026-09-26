// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Pre-commit snapshots for outbound effects.
//!
//! A phase is atomic: its member transforms and its effects all observe
//! the state as the phase found it. Transforms always did — every
//! `next_*` is computed off the pre-phase snapshot before any of them is
//! written back. Effects did not, because on EVM they are emitted after
//! the commit block (checks-effects-interactions) and so read storage
//! that the commit had already overwritten.
//!
//! `UniswapV2Pair.burn` is the case that surfaced it: the payout leg
//! transfers `mul_div(m_balances[this], bal, m_total_supply)` in the very
//! phase that zeroes `m_balances[this]` and drops `m_total_supply` to
//! zero. Every burn divided by zero, so pool shares could not be
//! redeemed at all.
//!
//! The fix captures each such read into a local before the commit and
//! points the effect at it, which keeps the external calls where they
//! were. These tests pin both halves: that outbound effects are
//! snapshotted, and that `return` is not (a route that computes a member
//! and hands it back must report what it just committed — audit
//! hypothesis EVM-H8).

use cambrian_transpiler::ProgramParser;
use cambrian_transpiler::codegen::evm::gen_evm_solidity;

fn parse(src: &str) -> cambrian_transpiler::ast::Program {
    let mut program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("Parse error: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    program
}

fn sol(src: &str) -> String {
    gen_evm_solidity(&parse(src), false)
}

fn route_body<'a>(sol: &'a str, signature: &str) -> &'a str {
    let start = sol
        .find(signature)
        .unwrap_or_else(|| panic!("no `{signature}` in generated Solidity:\n{sol}"));
    let rest = &sol[start..];
    let end = rest.find("\n    }\n").unwrap_or(rest.len());
    &rest[..end]
}

/// The burn shape, reduced: pay out a share computed from two members
/// that the same phase overwrites.
const REDEEMER: &str = r#"
entity Redeemer {
    routes {
        constructor() => []
        redeem(to: address) => [
            payout: [
                transfer(to, mul_div(m_shares[sys::address], 1000, m_total)) ~> m_asset
            ]
        ]
        view totalShares() -> u64 => [ return(m_total) ]
    }
    identity m_asset: address
    m_total: u64 {
        in constructor() => 500
        in redeem(_) => payout: m_total - m_shares[sys::address]
    }
    m_shares: HashMap<address, u64> {
        in redeem(_) => payout: m_shares.update(sys::address, 0)
    }
}

pure fn mul_div(a: u64, b: u64, c: u64) -> u64 {
    (a * b) / c
}
"#;

#[test]
fn outbound_effect_reads_the_pre_commit_value() {
    let out = sol(REDEEMER);
    let body = route_body(&out, "function redeem(address to)");

    assert!(
        body.contains("_pre_m_total"),
        "the scalar member the payout divides by must be snapshotted:\n{body}"
    );
    assert!(
        body.contains("_pre_m_shares"),
        "the mapping read the payout scales by must be snapshotted:\n{body}"
    );
    assert!(
        !body.contains("mul_div(m_shares[address(this)], 1000, m_total)"),
        "the transfer must not read storage the commit has already \
         overwritten — that is the divide-by-zero:\n{body}"
    );
}

#[test]
fn snapshots_are_taken_before_the_commit_and_calls_stay_after_it() {
    let out = sol(REDEEMER);
    let body = route_body(&out, "function redeem(address to)");

    let snapshot = body.find("_pre_m_total").expect("snapshot local");
    let commit = body
        .find("m_total = next_m_total;")
        .expect("scalar commit");
    let call = body
        .find("payable(m_asset).call")
        .expect("outbound call");

    assert!(
        snapshot < commit,
        "the snapshot has to be read before the phase writes over it:\n{body}"
    );
    assert!(
        commit < call,
        "the external call must still follow the storage writes \
         (checks-effects-interactions):\n{body}"
    );
}

/// EVM-H8 regression: a route that computes a member and returns it must
/// report the committed value, not the one it started with.
#[test]
fn return_still_reports_the_committed_value() {
    let out = sol(r#"
entity Matcher {
    routes {
        constructor() => []
        setCode(code: u64) => []
        classify() -> u64 => [
            return(m_result)
        ]
    }
    m_code: u64 {
        in constructor() => 0
        in setCode(code) => code
    }
    m_result: u64 {
        in classify() => {
            match m_code {
                0 => 100,
                1 => 200,
                _ => 999
            }
        }
    }
}
"#);
    let body = route_body(&out, "function classify()");

    assert!(
        !body.contains("_pre_m_result"),
        "`return` is the route's output, not an outbound effect — it must \
         read the member directly:\n{body}"
    );
    assert!(
        body.contains("return m_result;"),
        "expected the return to read the committed member:\n{body}"
    );
}

/// A route with no outbound effect must generate exactly what it did
/// before: the snapshot pass is inert unless something reads state the
/// phase destroys.
#[test]
fn routes_without_outbound_effects_are_untouched() {
    let out = sol(r#"
entity Counter {
    routes {
        constructor() => []
        bump() => []
        view value() -> u64 => [ return(m_n) ]
    }
    m_n: u64 {
        in constructor() => 0
        in bump() => m_n + 1
    }
}
"#);
    assert!(
        !out.contains("_pre_"),
        "no outbound effect, so nothing should be snapshotted:\n{out}"
    );
}
