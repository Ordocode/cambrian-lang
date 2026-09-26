// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Regressions for silent-wrong-code bugs found by the cambrian-book audit
//! (`docs/plans/transpiler-book-audit-bugs.md`). Runtime checks go through
//! `forge test` and skip when `forge` is not installed.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static OUT_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_out_dir(stem: &str) -> PathBuf {
    let n = OUT_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-book-audit-{}-{}-{}",
        stem,
        std::process::id(),
        n
    ))
}

fn transpiler_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cambrian-transpiler"))
}

fn has_forge() -> bool {
    Command::new("forge")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn has_lake() -> bool {
    Command::new("lake")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn write_cam(dir: &Path, name: &str, src: &str) -> PathBuf {
    std::fs::create_dir_all(dir).expect("create temp dir");
    let p = dir.join(name);
    std::fs::write(&p, src).unwrap();
    p
}

fn transpile(cam: &Path, out: &Path, target: &str) -> Output {
    Command::new(transpiler_bin())
        .arg(cam)
        .arg("-o")
        .arg(out)
        .arg("--target")
        .arg(target)
        .output()
        .expect("failed to run transpiler")
}

fn check(cam: &Path, target: &str) -> (bool, String) {
    let o = Command::new(transpiler_bin())
        .arg(cam)
        .arg("--check")
        .arg("--target")
        .arg(target)
        .output()
        .expect("failed to run transpiler");
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    (o.status.success(), log)
}

fn check_src(stem: &str, src: &str, target: &str) -> (bool, String) {
    let dir = unique_out_dir(stem);
    let cam = write_cam(&dir, &format!("{stem}.cam"), src);
    let r = check(&cam, target);
    let _ = std::fs::remove_dir_all(&dir);
    r
}

fn read_sol(out: &Path) -> String {
    let mut s = String::new();
    for e in std::fs::read_dir(out.join("src")).expect("src dir") {
        let p = e.unwrap().path();
        if p.extension().map_or(false, |x| x == "sol") {
            s.push_str(&std::fs::read_to_string(p).unwrap());
        }
    }
    s
}

/// Transpile `src` (which carries its own `test` blocks) to EVM and run
/// `forge test`. Returns `None` when forge is unavailable.
fn forge_test_src(stem: &str, src: &str) -> Option<(String, String)> {
    if !has_forge() {
        eprintln!("skipping {stem}: forge not found in PATH");
        return None;
    }
    let dir = unique_out_dir(stem);
    let _ = std::fs::remove_dir_all(&dir);
    let cam = write_cam(&dir, &format!("{stem}.cam"), src);
    let out = dir.join("out");
    let tp = transpile(&cam, &out, "evm");
    assert!(
        tp.status.success(),
        "transpile failed:\n{}",
        String::from_utf8_lossy(&tp.stderr)
    );
    let sol = read_sol(&out);
    cambrian_transpiler::codegen::evm_test_codegen::install_forge_std(&out)
        .unwrap_or_else(|e| panic!("{e}"));
    let forge = Command::new("forge")
        .args(["test", "-vv", "--root"])
        .arg(&out)
        .output()
        .expect("forge test");
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    Some((sol, log))
}

fn assert_all_pass(log: &str) {
    assert!(
        !log.contains("[FAIL") && log.contains("[PASS]") && !log.contains("Compiler run failed"),
        "forge tests must all pass:\n{log}"
    );
}

// ---------------------------------------------------------------------------
// #1 / #15: private routes and `call`
// ---------------------------------------------------------------------------

const PRIVATE_SENDER_CAM: &str = r#"
entity Counter {
  routes {
    inc() => [ call helper(1) ]
    private helper(x: u64) => []
    view who() -> address => [ return(m_who) ]
  }
  m_h: u64 { in helper(x) => x }
  m_who: address { in helper(x) => msg::sender }
}

test "private helper sees external caller" for Counter {
  let alice = 0x00000000000000000000000000000000000000000000000000000000000000a1
  msg { sender: alice }
  call inc()
  call who()
  expect return alice
}
"#;

#[test]
fn private_route_is_internal_and_keeps_msg_sender() {
    let Some((sol, log)) = forge_test_src("private-sender", PRIVATE_SENDER_CAM) else {
        return;
    };
    assert!(
        sol.contains("function _helper(uint64 x) internal"),
        "private route must be an internal function:\n{sol}"
    );
    assert!(!sol.contains("this._helper"), "call must be internal:\n{sol}");
    assert_all_pass(&log);
}

#[test]
fn call_to_non_private_route_is_public_and_direct() {
    let src = r#"
entity C {
  routes {
    target() => []
    go() => [ call target() ]
  }
  m_who: address { in target() => msg::sender }
}
"#;
    let dir = unique_out_dir("call-public");
    let cam = write_cam(&dir, "c.cam", src);
    let out = dir.join("out");
    assert!(transpile(&cam, &out, "evm").status.success());
    let sol = read_sol(&out);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(sol.contains("function target() public"), "{sol}");
    assert!(sol.contains("        target();"), "{sol}");
    assert!(!sol.contains("this.target"), "{sol}");
}

#[test]
fn v9_view_calling_mutating_private_route() {
    let src = r#"
entity C {
  routes {
    bump() => []
    private helper(x: u64) => []
    view v() -> u64 => [ call helper(1) return(m_x) ]
  }
  m_x: u64 { in bump() => m_x + 1 }
  m_h: u64 { in helper(x) => x }
}
"#;
    let (ok, log) = check_src("v9-view-call", src, "evm");
    assert!(!ok, "{log}");
    assert!(log.contains("[V9]") && log.contains("calls route 'helper'"), "{log}");
}

// ---------------------------------------------------------------------------
// #2 / #3 / #4: temporal refs and effect-argument ordering
// ---------------------------------------------------------------------------

const PHASED_TEMPORAL_CAM: &str = r#"
entity C {
  routes {
    ex() => [ p1: [] ]
    example() => [ p1: [] ]
  }
  m_a: u64 { in ex() => p1: m_a + 1 }
  m_c: u64 { in ex() => p1: ^m_a * 2 }
  m_x: u64 { in example() => p1: m_y + 1 }
  m_y: u64 { in example() => p1: m_x + 1 }
  m_z: u64 { in example() => p1: ^m_x + ^m_y }
}

test "same-phase temporal ref reads post-transform value" for C with { m_a: 5 } {
  call ex()
  expect state { m_a: 6, m_c: 12 }
}

test "same-phase temporal refs of swapped members" for C with { m_x: 1, m_y: 10 } {
  call example()
  expect state { m_x: 11, m_y: 2, m_z: 13 }
}
"#;

#[test]
fn phased_temporal_ref_reads_same_phase_value() {
    let Some((sol, log)) = forge_test_src("phased-temporal", PHASED_TEMPORAL_CAM) else {
        return;
    };
    assert!(sol.contains("next_m_c = (next_m_a * 2)"), "{sol}");
    assert_all_pass(&log);
}

const TEMPORAL_MAP_CAM: &str = r#"
entity T {
  routes {
    bump(k: address) => []
    view snap() -> u64 => [ return(m_snap) ]
  }
  m_map: HashMap<address, u64> { in bump(k) => m_map.insert(k, m_map[k] + 1) }
  m_snap: u64 { in bump(k) => ^m_map[k] }
}

test "temporal map index reads the new entry" for T {
  let k = 0x00000000000000000000000000000000000000000000000000000000000000b2
  call bump(k)
  call bump(k)
  call snap()
  expect return 2
}
"#;

#[test]
fn temporal_map_index_reads_new_entry_evm() {
    let Some((_sol, log)) = forge_test_src("temporal-map", TEMPORAL_MAP_CAM) else {
        return;
    };
    assert_all_pass(&log);
}

#[test]
fn temporal_map_index_uses_map_lookup_lean() {
    let dir = unique_out_dir("temporal-map-lean");
    let cam = write_cam(&dir, "t.cam", TEMPORAL_MAP_CAM);
    let out = dir.join("out");
    let tp = transpile(&cam, &out, "lean");
    assert!(tp.status.success(), "{}", String::from_utf8_lossy(&tp.stderr));
    let lean = std::fs::read_to_string(out.join("Cambrian/Generated/T.lean")).unwrap();
    assert!(
        lean.contains("(Cambrian.AddressMap.lookup (T.Members.M_m_map.bump s ctx inst k) k |>.get!)"),
        "{lean}"
    );
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").is_ok() && has_lake() {
        let b = Command::new("lake").arg("build").current_dir(&out).output().unwrap();
        assert!(
            b.status.success(),
            "lake build failed:\n{}{}",
            String::from_utf8_lossy(&b.stdout),
            String::from_utf8_lossy(&b.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unphased_send_value_reads_precommit_snapshot() {
    let src = r#"
entity Staking {
  routes {
    fund(who: address, amt: U256) => []
    claim() => [
      ~> msg::sender with { value: m_pending[msg::sender] }
    ]
  }
  m_pending: HashMap<address, U256> {
    in fund(who, amt) => m_pending.insert(who, amt)
    in claim() => m_pending.insert(msg::sender, 0)
  }
}
"#;
    let dir = unique_out_dir("unphased-snapshot");
    let cam = write_cam(&dir, "s.cam", src);
    let out = dir.join("out");
    assert!(transpile(&cam, &out, "evm").status.success());
    let sol = read_sol(&out);
    let _ = std::fs::remove_dir_all(&dir);
    let claim = &sol[sol.find("function claim").expect("claim")..];
    let snap = claim
        .find("uint256 _pre_m_pending_0 = m_pending[msg.sender];")
        .unwrap_or_else(|| panic!("missing pre-commit snapshot:\n{sol}"));
    let write = claim.find("m_pending[_cam_tmp").expect("write");
    assert!(snap < write, "snapshot must precede the commit:\n{sol}");
    assert!(sol.contains("call{value: _pre_m_pending_0}"), "{sol}");
}

#[test]
fn unphased_send_args_read_old_value_lets_read_new() {
    let src = r#"
entity Observer {
  routes { notify(n: u64) => [] }
  m_last: u64 { in notify(n) => n }
}
entity Counter {
  routes {
    #[factory_only]
    init setup(o: Address<Observer>) => []
    inc() => [
      let a = m_count;
      notify(m_count) ~> m_obs
      notify(a) ~> m_obs
    ]
  }
  m_obs: Address<Observer> { in setup(o) => o }
  m_count: u64 { in inc() => m_count + 1 }
}
"#;
    let dir = unique_out_dir("unphased-args");
    let cam = write_cam(&dir, "c.cam", src);
    let out = dir.join("out");
    let yaml = dir.join("p.yaml");
    std::fs::write(
        &yaml,
        format!(
            "name: c\ntarget: evm\nsources: [c.cam]\noutput_dir: {}\n",
            out.display()
        ),
    )
    .unwrap();
    let o = Command::new(transpiler_bin())
        .arg("--project")
        .arg(&yaml)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let sol = read_sol(&out);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(sol.contains("uint64 _pre_m_count_0 = m_count;"), "{sol}");
    assert!(sol.contains(".notify(_pre_m_count_0)"), "{sol}");
    assert!(sol.contains("uint64 a = m_count;") && sol.contains(".notify(a)"), "{sol}");
}

// ---------------------------------------------------------------------------
// #5 / #6 / #7: stdlib shadowing, match exhaustiveness, return checks
// ---------------------------------------------------------------------------

const STD_SHADOW_CAM: &str = r#"
pure fn min(a: u64, b: u64) -> u64 { a }
entity E {
  routes {
    view user_min(x: u64, y: u64) -> u64 => [ return(min(x, y)) ]
    view std_min(x: u64, y: u64) -> u64 => [ return(std::math::min(x, y)) ]
  }
}

test "user min is the user function" for E {
  call user_min(9, 2)
  expect return 9
}

test "std::math::min is never shadowed" for E {
  call std_min(9, 2)
  expect return 2
}
"#;

#[test]
fn std_math_call_not_captured_by_user_pure_fn() {
    let Some((sol, log)) = forge_test_src("std-shadow", STD_SHADOW_CAM) else {
        return;
    };
    assert!(sol.contains("_cam_std_min(x, y)"), "{sol}");
    assert_all_pass(&log);
}

#[test]
fn v49_checks_pure_fn_bodies() {
    let src = r#"
pure fn f(x: u64) -> u64 { max(x, 1) }
entity E { routes { view go(x: u64) -> u64 => [ return(f(x)) ] } }
"#;
    let (ok, log) = check_src("v49-pure", src, "evm");
    assert!(!ok && log.contains("[V49]") && log.contains("pure fn 'f'"), "{log}");
}

#[test]
fn v66_non_exhaustive_enum_match() {
    let src = r#"
enum Status { Active, Paused, Closed }
pure fn f(s: Status) -> u64 {
  match s {
    Status::Active => 1,
    Status::Paused => 2
  }
}
entity T {
  routes { go(s: Status) => [] view v() -> u64 => [ return(f(m_s)) ] }
  m_s: Status { in go(s) => s }
}
"#;
    let (ok, log) = check_src("v66-enum", src, "evm");
    assert!(!ok && log.contains("[V66]") && log.contains("Status::Closed"), "{log}");
}

#[test]
fn v66_accepts_exhaustive_and_wildcard_matches() {
    let src = r#"
enum Status { Active, Paused }
pure fn f(s: Status) -> u64 {
  match s {
    Status::Active => 1,
    Status::Paused => 2
  }
}
pure fn g(b: bool) -> u64 { match b { true => 1, false => 0 } }
pure fn h(x: u64) -> u64 { match x { 0 => 1, _ => 2 } }
entity T {
  routes { go(s: Status) => [] view v() -> u64 => [ return(f(m_s) + g(true) + h(3)) ] }
  m_s: Status { in go(s) => s }
}
"#;
    let (_ok, log) = check_src("v66-ok", src, "evm");
    assert!(!log.contains("[V66]"), "{log}");
}

#[test]
fn v66_non_exhaustive_literals_and_bool() {
    let src = r#"
pure fn g(b: bool) -> u64 { match b { true => 1 } }
pure fn h(x: u64) -> u64 { match x { 0 => 1, 1 => 2 } }
entity T { routes { view v() -> u64 => [ return(g(true) + h(3)) ] } }
"#;
    let (ok, log) = check_src("v66-lit", src, "evm");
    assert!(!ok, "{log}");
    assert!(log.contains("pure fn 'g': missing `false`"), "{log}");
    assert!(log.contains("pure fn 'h'"), "{log}");
}

#[test]
fn v67_missing_and_mistyped_returns() {
    let src = r#"
entity T {
  routes {
    bump() => []
    view noreturn() -> U256 => [ ]
    view mism() -> U256 => [ return("not a number") ]
    view two() -> (u64, bool) => [ return(m_x, 1) ]
    view ok() -> u64 => [ return(m_x) ]
    view throws() -> u64 => [ throw 7 ]
  }
  m_x: u64 { in bump() => m_x + 1 }
}
pure fn p() -> String { 5 }
"#;
    let (ok, log) = check_src("v67", src, "evm");
    assert!(!ok, "{log}");
    assert!(log.contains("'noreturn'") && log.contains("never returns a value"), "{log}");
    assert!(log.contains("'mism'") && log.contains("String type but `U256`"), "{log}");
    assert!(log.contains("'two'") && log.contains("numeric type but `bool`"), "{log}");
    assert!(log.contains("pure fn 'p' returns a mistyped value"), "{log}");
    assert!(!log.contains("'ok'") && !log.contains("'throws'"), "{log}");
}

#[test]
fn v9_view_calling_mutating_route_transitively() {
    let src = r#"
entity C {
  routes {
    private inner() => []
    private outer() => [ call inner() ]
    view v() -> u64 => [ call outer() return(m_x) ]
  }
  m_x: u64 { in inner() => m_x + 1 }
}
"#;
    let (ok, log) = check_src("v9-view-call-trans", src, "evm");
    assert!(!ok, "{log}");
    assert!(log.contains("[V9]") && log.contains("calls route 'inner'"), "{log}");
}

// ---------------------------------------------------------------------------
// #8 / #17: V23 is a hard error on EVM (untyped named sends, unknown extern
// routes) instead of a note that let `--check` pass.
// ---------------------------------------------------------------------------

#[test]
fn v23_named_send_to_untyped_address_is_error_on_evm() {
    let src = r#"
entity Shop {
  routes { fulfill(q: u64) => [] }
  m_ok: u64 {}
}
entity Book {
  routes {
    order(shop: address, q: u64) => [ fulfill(q) ~> shop ]
  }
  m_qty: u64 {}
}
"#;
    let (ok, log) = check_src("v23-untyped", src, "evm");
    assert!(!ok, "{log}");
    assert!(log.contains("error [V23]") && log.contains("untyped address"), "{log}");
}

#[test]
fn v23_unknown_extern_route_is_error_on_evm() {
    let src = r#"
extern entity Token {
  route transfer(amount: U256);
}
entity Caller {
  routes {
    #[factory_only]
    constructor(t: Address<Token>) => []
    ping(amount: U256) => [
      transfer(amount) ~> m_token
      nope(amount) ~> m_token
    ]
  }
  m_token: Address<Token> { in constructor(t) => t }
}
"#;
    let (ok, log) = check_src("v23-extern", src, "evm");
    assert!(!ok, "{log}");
    assert!(
        log.contains("error [V23]") && log.contains("'nope' not found on extern entity 'Token'"),
        "{log}"
    );
}

// ---------------------------------------------------------------------------
// #9: type aliases are expanded before EVM lowering (no `uint256` erasure).
// ---------------------------------------------------------------------------

const ALIAS_MAP_CAM: &str = r#"
type Owner = address
type Ledger = HashMap<Owner, u64>
entity Vault {
  type Amount = u64
  record Entry { who: Owner, amt: Amount }
  routes {
    set(o: Owner, n: Amount) => []
    view get(o: Owner) -> Amount => [ return(m_bal[o]) ]
    view last_who() -> Owner => [ return(m_last.who) ]
  }
  m_bal: Ledger { in set(o, n) => m_bal.insert(o, n) }
  m_last: Entry { in set(o, n) => { Entry { who: o, amt: n } } }
}

test "alias-keyed map round-trips" for Vault {
  let alice = 0x00000000000000000000000000000000000000000000000000000000000000a1
  call set(alice, 7)
  call get(alice)
  expect return 7
}
"#;

#[test]
fn type_aliases_resolve_in_evm_types() {
    let Some((sol, log)) = forge_test_src("alias-map", ALIAS_MAP_CAM) else {
        return;
    };
    assert!(sol.contains("mapping(address => uint64) public m_bal"), "{sol}");
    assert!(sol.contains("function set(address o, uint64 n)"), "{sol}");
    assert!(sol.contains("address who;") && sol.contains("uint64 amt;"), "{sol}");
    assert!(sol.contains("returns (address)"), "{sol}");
    assert_all_pass(&log);
}

// ---------------------------------------------------------------------------
// #10: `let` bound to a field of an indexed record gets the field's type.
// ---------------------------------------------------------------------------

#[test]
fn let_from_indexed_record_field_is_typed() {
    let src = r#"
entity Bag {
  record Item { owner: address, amt: u64 }
  routes {
    view peek(i: u64) -> address => [
      let a = m_vec[i].owner;
      return(a)
    ]
  }
  m_vec: Vec<Item> {}
}
"#;
    let dir = unique_out_dir("vec-field-let");
    let cam = write_cam(&dir, "t.cam", src);
    let out = dir.join("out");
    let o = transpile(&cam, &out, "evm");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let sol = read_sol(&out);
    assert!(sol.contains("address a = m_vec[i].owner;"), "{sol}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Stage 2 (#11-#18, #39-#47): `--check` rejects what later fails and stops
// flagging what later succeeds.
// ---------------------------------------------------------------------------

#[test]
fn e05_not_reported_on_receive() {
    let src = r#"
entity V {
  routes { accept receive() => [] }
  m_b: U256 { in receive() => m_b + msg::value }
}
"#;
    let (ok, log) = check_src("e05-receive", src, "evm");
    assert!(ok && !log.contains("[E05]"), "{log}");
}

#[test]
fn e29_view_with_direct_effect_is_error_on_solidity_only() {
    let src = r#"
entity V {
  event Seen(n: u64);
  routes {
    view peek() -> u64 => [ emit Seen(m_n); return(m_n) ]
    bump() => []
  }
  m_n: u64 { in bump() => m_n + 1 }
}
"#;
    let (ok, log) = check_src("e29-emit", src, "evm");
    assert!(!ok && log.contains("error [E29]") && log.contains("an `emit`"), "{log}");
    // Lean threads the world through views, so `emit` in a view is accepted.
    let (ok, log) = check_src("e29-emit-lean", src, "lean");
    assert!(ok && !log.contains("[E29]"), "{log}");
}

#[test]
fn v50_unfolds_type_aliases() {
    let src = r#"
type Amount = u64
entity V {
  routes { set(x: Amount) => [] }
  m_n: u64 { in set(x) => x }
}
"#;
    let (ok, log) = check_src("v50-alias", src, "evm");
    assert!(ok && !log.contains("[V50]"), "{log}");
}

#[test]
fn v55_covers_string_builtins() {
    let src = r#"
pure fn len(s: String) -> u64 { 0 }
using { len } for String;
entity V {
  routes { set(x: u64) => [] }
  m_n: u64 { in set(x) => x }
}
"#;
    let (ok, log) = check_src("v55-string", src, "evm");
    assert!(!ok && log.contains("error [V55]") && log.contains("String::len"), "{log}");
}

#[test]
fn unknown_evm_intrinsic_and_sys_field_are_rejected() {
    let src = r#"
entity V {
  routes { set(x: u64) => [] }
  m_n: U256 { in set(x) => evm::gasleft() }
  m_t: U256 { in set(x) => sys::bogus }
}
"#;
    let (ok, log) = check_src("e16-evm", src, "evm");
    assert!(!ok && log.contains("error [E16]") && log.contains("evm::gasleft"), "{log}");
    let (ok, log) = check_src("l16-evm", src, "lean");
    assert!(!ok, "{log}");
    assert!(log.contains("error [L16]") && log.contains("`sys::bogus`"), "{log}");
}

#[test]
fn e28_unlowered_option_method() {
    let src = r#"
entity V {
  routes { set(x: u64) => [] }
  m_o: Option<u64> { in set(x) => some(x) }
  m_n: u64 { in set(x) => m_o.unwrap_or(0) }
}
"#;
    for target in ["evm", "lean"] {
        let (ok, log) = check_src("e28", src, target);
        assert!(!ok && log.contains("error [E28]") && log.contains("unwrap_or"), "{log}");
    }
}

#[test]
fn v68_record_literal_fields() {
    let src = r#"
record P { a: u64, b: u64 }
entity V {
  routes { set(x: u64) => [] }
  m_p: P { in set(x) => { P { a: x, a: 1, c: 2 } } }
}
"#;
    let (ok, log) = check_src("v68", src, "evm");
    assert!(!ok, "{log}");
    assert!(log.contains("sets field `a` more than once"), "{log}");
    assert!(log.contains("has no field `c`"), "{log}");
    assert!(log.contains("missing field(s) `b`"), "{log}");
}

#[test]
fn v69_mixed_sign_without_common_type() {
    let src = r#"
entity V {
  routes { set(x: i64, y: U256) => [] }
  m_n: U256 { in set(x, y) => y + x }
  m_b: bool { in set(x, y) => y > x }
}
"#;
    let (ok, log) = check_src("v69", src, "evm");
    assert!(!ok, "{log}");
    assert_eq!(log.matches("error [V69]").count(), 2, "{log}");
    // Rule 2 of "Automatic Type Widening": `i64` and `u64` widen to `i128`,
    // and a `u128` comparison is exact under 256-bit signed promotion.
    let widening = r#"
entity V {
  routes { set(x: i64, y: u64, z: u128) => [] }
  m_n: i128 { in set(x, y, z) => x + y }
  m_b: bool { in set(x, y, z) => z > x }
}
"#;
    let (ok, log) = check_src("v69-ok", widening, "evm");
    assert!(ok && !log.contains("[V69]"), "{log}");
}

#[test]
fn v70_transform_arity() {
    let src = r#"
entity V {
  routes { set(x: u64, y: u64) => [] reset(z: u64) => [] }
  m_n: u64 {
    in set(x) => x
    in reset() => 0
  }
}
"#;
    let (ok, log) = check_src("v70", src, "evm");
    assert!(!ok && log.contains("error [V70]") && log.contains("route 'set'"), "{log}");
    assert!(!log.contains("route 'reset'"), "zero bindings are allowed:\n{log}");
}

#[test]
fn single_file_evm_validates_deterministic_model() {
    let src = r#"
entity Pair {
  identity m_a: address
  identity m_b: address
  routes {
    constructor() => []
    ping() from Pair(m_a, m_b) : throw 1 => [ ]
  }
  m_n: u64 {
    in constructor() => 0
    in ping() => m_n + 1
  }
}
"#;
    let (ok, log) = check_src("det-single", src, "evm");
    assert!(ok, "{log}");
    assert!(!log.contains("[V33]"), "{log}");
    assert!(log.contains("warning [V63]") && log.contains("single-file mode"), "{log}");
}

const SIGN_CAST_CAM: &str = r#"
entity C {
  routes {
    setu(x: i64) => []
    seti(x: u64) => []
    view getu() -> u64 => [ return(m_u) ]
    view geti() -> i64 => [ return(m_i) ]
  }
  m_u: u64 { in setu(x) => x as u64 }
  m_i: i64 { in seti(x) => x as i64 }
  m_h: U256 { in setu(x) => evm::keccak256(x) }
}

test "signed to unsigned" for C {
  call setu(5)
  call getu()
  expect return 5
}

test "unsigned to signed" for C {
  call seti(9)
  call geti()
  expect return 9
}
"#;

#[test]
fn signed_unsigned_casts_and_keccak_compile_on_evm() {
    let Some((sol, log)) = forge_test_src("sign-cast", SIGN_CAST_CAM) else {
        return;
    };
    assert!(sol.contains("_toUint64(_camI2U(int256(x)))"), "{sol}");
    assert!(sol.contains("_toInt64(_camU2I(uint256(x)))"), "{sol}");
    assert!(sol.contains("uint256(keccak256(abi.encode(x)))"), "{sol}");
    assert_all_pass(&log);
}

fn write_project(stem: &str, yaml: &str, cam: &str) -> (PathBuf, PathBuf) {
    let dir = unique_out_dir(stem);
    write_cam(&dir, "c.cam", cam);
    let y = dir.join("project.yaml");
    std::fs::write(&y, yaml).unwrap();
    (dir, y)
}

#[test]
fn f7_project_yaml_key_warnings() {
    let yaml = "name: p\ntarget: lean\nsources: [c.cam]\noutput_dir: out\nbogus: 1\n\
                fuzz:\n  shrink: false\n  runz: 3\nlean:\n  intrinsics: garbage\n";
    let cam = "entity V {\n  routes { set(x: u64) => [] }\n  m_n: u64 { in set(x) => x }\n}\n";
    let (dir, y) = write_project("f7", yaml, cam);
    let o = Command::new(transpiler_bin())
        .arg("--project")
        .arg(&y)
        .arg("--check")
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(!o.status.success(), "{log}");
    assert!(log.contains("warning [F7]") && log.contains("`bogus`"), "{log}");
    assert!(log.contains("`fuzz.runz`"), "{log}");
    assert!(log.contains("`fuzz.shrink` is accepted but has no effect"), "{log}");
    assert!(log.contains("error [F2]") && log.contains("lean.intrinsics"), "{log}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_warns_about_ignored_flags() {
    let yaml = "name: p\ntarget: lean\nsources: [c.cam]\noutput_dir: out\n";
    let cam = "entity V {\n  routes { set(x: u64) => [] }\n  m_n: u64 { in set(x) => x }\n}\n";
    let (dir, y) = write_project("cli-flags", yaml, cam);
    let o = Command::new(transpiler_bin())
        .arg("--project")
        .arg(&y)
        .args(["--check", "-o", "x", "--target", "evm", "--dump-ast"])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&o.stderr).to_string();
    for flag in ["-o is ignored", "--target is ignored", "--dump-ast is ignored"] {
        assert!(log.contains(flag), "{flag}:\n{log}");
    }
    let o = Command::new(transpiler_bin())
        .arg(dir.join("c.cam"))
        .args(["--check", "--target", "evm", "--source-map", "--check-lean"])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(log.contains("--source-map is only implemented for Rust targets"), "{log}");
    assert!(log.contains("--check-lean is only meaningful"), "{log}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Stage 3 — EVM compile failures (#19–#32)
// ---------------------------------------------------------------------------

const STAGE3_CAM: &str = r#"
library M {
    pure fn add(a: U256, b: U256) -> U256 { a + b }
    pure fn mul(a: U256, b: U256) -> U256 { a * b }
}
using M for U256;

record Inner { a: u64 }
record Outer { i: Inner, b: u64 }

pure fn wrap8(a: u8) -> u8 { a +% 1 }
pure fn twice(a: u64) -> u64 { let x = a + 1; let x = x * 2; x }

entity C {
    macro known(k: address) -> bool = { m_m.exists(k) }
    routes {
        bump8(a: u8) => []
        bumpi(a: i8) => []
        put(k: address, v: u64) => []
        push(a: u64) => []
        chain(a: U256, b: U256) => []
        nest(a: u64) => []
        hash(s: String) => []
        view get8() -> u8 => [ return(m_u8) ]
        view isneg() -> bool => [ return(m_i8 < 0) ]
        view empty() -> bool => [ return(m_v.is_empty()) ]
        view size() -> u64 => [ return(m_m.len()) ]
        view has(k: address) -> bool => [ return(@known(k)) ]
        view shadow(a: u64) -> u64 => [
            let x = a + 1;
            let x = x * 2;
            return(x + twice(a))
        ]
        view pick(i: u64) -> u64 => [
            let xs = array(10, 20, 30);
            return(xs[i])
        ]
        view fmt(a: u64) -> String => [
            let s = std::str::format("v={}", a);
            return(s)
        ]
        view opt() -> bool => [ let r = match m_o { some(_) => true, none => false }; return(r) ]
        view getx() -> U256 => [ return(m_x) ]
        view inner() -> u64 => [ return(m_n.i.a) ]
    }
    m_u8: u8 { in bump8(a) => wrap8(a) }
    m_i8: i8 { in bumpi(a) => a +% 1 }
    m_m: HashMap<address, u64> { in put(k, v) => m_m.insert(k, v) }
    m_v: Vec<u64> { in push(a) => m_v.push(a) }
    m_o: Option<U256> = none { in chain(a, b) => some(5) }
    m_b: bytes {}
    m_x: U256 { in chain(a, b) => M.add(a, b).mul(2) }
    m_n: Outer { in nest(a) => { let inn = Inner { a: a }; Outer { i: inn, b: 1 } } }
    m_h: Vec<u8> { in hash(s) => std::crypto::sha256(s) }
}

test "u8 wrapping add in pure fn" for C {
    call bump8(255)
    call get8()
    expect return 0
}

test "i8 wrapping add" for C {
    call bumpi(127)
    call isneg()
    expect return true
}

test "vec is_empty" for C {
    call empty()
    expect return true
}

test "hashmap len and exists in macro" for C {
    call put(0x0000000000000000000000000000000000000007, 3)
    call size()
    expect return 1
    call has(0x0000000000000000000000000000000000000007)
    expect return true
}

test "rebound lets" for C {
    call shadow(3)
    expect return 16
}

test "array let" for C {
    call pick(2)
    expect return 30
}

test "format let" for C {
    call fmt(4)
    expect return "v=4"
}

test "option default none then some" for C {
    call opt()
    expect return false
    call chain(2, 3)
    call opt()
    expect return true
    call getx()
    expect return 10
}

test "nested program records" for C {
    call nest(6)
    call inner()
    expect return 6
}

test "sha256 into Vec<u8>" for C {
    call hash("abc")
}
"#;

#[test]
fn stage3_evm_constructs_compile_and_run() {
    let Some((sol, log)) = forge_test_src("stage3", STAGE3_CAM) else {
        return;
    };
    assert!(sol.contains("function _wadd(uint256 a, uint256 b) pure"), "{sol}");
    assert!(sol.contains("_wadds("), "{sol}");
    assert!(sol.contains("m_m_keys.length"), "{sol}");
    assert!(sol.contains("_cam_bytes_to_u8s(abi.encodePacked(sha256("), "{sol}");
    assert!(sol.contains("    Inner i;"), "{sol}");
    assert_all_pass(&log);
}

const SENDERS_CAM: &str = r#"
entity T {
    routes { bump() => [] }
    m_n: u64 { in bump() => m_n + 1 }
}

invariant "grows" for T {
    senders { 0x1111111111111111111111111111111111111111 }
    action bump() {}
    check m_n >= 0
}
"#;

#[test]
fn invariant_senders_accept_full_width_addresses() {
    let Some((_, log)) = forge_test_src("senders", SENDERS_CAM) else {
        return;
    };
    assert!(
        log.contains("[PASS]") && !log.contains("Compiler run failed") && !log.contains("[FAIL"),
        "{log}"
    );
    assert!(log.contains("invariant_"), "{log}");
}

// ---- Stage 4: Lean build failures (#16 Lean half, #33-#38) ----

fn lean_src(stem: &str, src: &str, file: &str, pins: &[&str]) {
    let dir = unique_out_dir(stem);
    let cam = write_cam(&dir, "t.cam", src);
    let out = dir.join("out");
    let tp = transpile(&cam, &out, "lean");
    assert!(tp.status.success(), "{}", String::from_utf8_lossy(&tp.stderr));
    let lean = std::fs::read_to_string(out.join("Cambrian/Generated").join(file)).unwrap();
    for pin in pins {
        assert!(lean.contains(pin), "missing `{pin}` in {file}:\n{lean}");
    }
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").is_ok() && has_lake() {
        let b = Command::new("lake").arg("build").current_dir(&out).output().unwrap();
        assert!(
            b.status.success(),
            "lake build failed:\n{}{}",
            String::from_utf8_lossy(&b.stdout),
            String::from_utf8_lossy(&b.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lean_mixed_sign_arith_widens_to_signed() {
    let src = r#"
pure fn mixed(a: u32, d: i8) -> i64 { a + d }
pure fn mix3(p: u8, q: i8) -> i16 { p * q }
entity T {
    routes { go(a: u32, d: i8) => [] }
    m_x: i64 { in go(a, d) => a + d }
}
"#;
    lean_src(
        "lean-mixed-sign",
        src,
        "Pure.lean",
        &["(((a).zeroExtend 64) + ((d).signExtend 64))", "(((p).zeroExtend 16) * ((q).signExtend 16))"],
    );
    lean_src("lean-mixed-sign-m", src, "T.lean", &["(((a).zeroExtend 64) + ((d).signExtend 64))"]);
}

#[test]
fn lean_library_fns_and_constants_resolve() {
    let src = r#"
library M {
    const K: U256 = 7
    pure fn add3(a: U256, b: U256, c: U256) -> U256 { add(add(a, b), c) }
    pure fn add(a: U256, b: U256) -> U256 { a + b }
}
using M for U256;
entity T {
    routes { go(a: U256, b: U256) => [] }
    m_x: U256 { in go(a, b) => M.add3(a, b, M.K) }
    m_y: U256 { in go(a, b) => a.add(b) }
    m_z: U256 { in go(a, b) => M::add(a, M::K) }
}
"#;
    lean_src(
        "lean-library",
        src,
        "Pure.lean",
        &[
            "def M_K : Cambrian.U256 :=",
            "(Cambrian.Generated.Pure.M_add (Cambrian.castWidth 256 (Cambrian.Generated.Pure.M_add a b)) c)",
        ],
    );
    lean_src(
        "lean-library-t",
        src,
        "T.lean",
        &[
            "(Cambrian.Generated.Pure.M_add3 a b (Cambrian.castWidth 256 Cambrian.Generated.Pure.M_K))",
            "(Cambrian.Generated.Pure.M_add a b)",
        ],
    );
}

#[test]
fn lean_wrapping_ops_truncate_to_member_width() {
    lean_src(
        "lean-wrapping",
        r#"
entity T {
    routes { go(a: u8) => [] }
    m_x: u8 { in go(a) => a +% 1 }
    m_s: i8 { in go(a) => m_s -% 1 }
}
"#,
        "T.lean",
        &["(Cambrian.castWidth 8 (((Cambrian.castWidth 256 (a : BitVec 8)) + ((1 : BitVec 256))) : BitVec 256))"],
    );
}

#[test]
fn lean_range_comprehension_yields_bitvec() {
    lean_src(
        "lean-range",
        r#"
pure fn first_n(n: u32) -> Vec<u32> {
    for i in 0..n { i }
}
entity T {
    routes { go(n: u32) => [] }
    m_v: Vec<u32> { in go(n) => first_n(n) }
}
"#,
        "Pure.lean",
        &["(fun i => (BitVec.ofNat 32 i))"],
    );
}

#[test]
fn lean_record_fields_named_like_keywords() {
    lean_src(
        "lean-record-kw",
        r#"
record R { open: bool, end: u64 }
entity T {
    routes { go(a: u64) => [] view isopen() -> bool => [ return(m_r.open) ] }
    m_r: R { in go(a) => { R { open: true, end: a } } }
}
"#,
        "T.lean",
        &["{ open_ := false, end_ := 0#64 }", "({ open_ := true, end_ := a } : R)"],
    );
}

#[test]
fn lean_transform_widens_narrow_arith() {
    lean_src(
        "lean-widen",
        r#"
entity T {
    routes { go(a: u8, b: u8) => [] }
    m_x: u16 { in go(a, b) => a + b }
}
"#,
        "T.lean",
        &["(Cambrian.castWidth 16 (a + b))"],
    );
}

// ---- Stage 6: found while checking the docs examples ----

#[test]
fn lean_transform_binding_no_params_takes_route_args() {
    lean_src(
        "lean-in-r-unit",
        r#"
entity V {
    routes {
        #[factory_only]
        constructor(a: U256) => []
        go(x: U256) => []
    }
    m_a: U256 { in constructor(a) => a }
    m_b: U256 {
        in constructor() => 7
        in go() => m_b + 1
    }
}
"#,
        "V.lean",
        &["def go (s : V.State) (ctx : Cambrian.MsgCtx) (inst : V.Identity) (_ : Cambrian.U256)"],
    );
}

#[test]
fn string_escapes_decode_and_reencode_on_evm() {
    let src = r#"
entity T {
    routes {
        view nl() -> String => [ return("a\nb\t\\c") ]
        view uni() -> String => [ return("é") ]
        view len() -> U256 => [ return("a\nb".len()) ]
    }
    m_x: u64 {}
}
test "len" for T {
    call len()
    expect return 3
}
"#;
    let dir = unique_out_dir("str-escape");
    let cam = write_cam(&dir, "t.cam", src);
    let out = dir.join("out");
    let tp = transpile(&cam, &out, "evm");
    assert!(tp.status.success(), "{}", String::from_utf8_lossy(&tp.stderr));
    let sol = read_sol(&out);
    assert!(sol.contains(r#""a\nb\t\\c""#), "{sol}");
    assert!(sol.contains(r#"unicode"é""#), "{sol}");
    let _ = std::fs::remove_dir_all(&dir);
    if let Some((_sol, log)) = forge_test_src("str-escape-forge", src) {
        assert_all_pass(&log);
    }
}

#[test]
fn e25_unknown_std_call_is_error_on_evm() {
    let src = r#"
entity T {
    routes { go(a: U256) => [] view v() -> U256 => [ return(std::str::len("a")) ] }
    m_x: U256 { in go(a) => std::math::sqrt(a) }
}
"#;
    let (ok, out) = check_src("e25-evm", src, "evm");
    assert!(!ok, "{out}");
    assert!(out.contains("E25") && out.contains("std::str::len") && out.contains("std::math::sqrt"), "{out}");
}

// ---------------------------------------------------------------------------
// #54-#57: EVM test harness (expect state getters, return lenses, program
// events, usage-typed address lets)
// ---------------------------------------------------------------------------

fn read_test_sol(out: &Path) -> String {
    let mut s = String::new();
    for e in std::fs::read_dir(out.join("test")).expect("test dir") {
        let p = e.unwrap().path();
        if p.extension().map_or(false, |x| x == "sol") {
            s.push_str(&std::fs::read_to_string(p).unwrap());
        }
    }
    s
}

fn transpile_test_sol(stem: &str, src: &str) -> String {
    let dir = unique_out_dir(stem);
    let cam = write_cam(&dir, "t.cam", src);
    let out = dir.join("out");
    let tp = transpile(&cam, &out, "evm");
    assert!(tp.status.success(), "{}", String::from_utf8_lossy(&tp.stderr));
    let sol = read_test_sol(&out);
    let _ = std::fs::remove_dir_all(&dir);
    sol
}

const HARNESS_STATE_SRC: &str = r#"
event Moved(indexed who: address, amount: U256);

record Pool {
    reserve_a: U256,
    reserve_b: U256
}

enum Phase { Open, Closed }

entity T {
    routes {
        #[factory_only]
        constructor() => []
        put(who: address, amount: U256) => [
            emit Moved(who, amount);
        ]
        approve(owner: address, spender: address, amount: U256) => []
        fill(a: U256, b: U256) => []
        close() => []
        pick(v: u64) => []
        view pair() -> (U256, U256) => [ return((m_pool.reserve_a, m_pool.reserve_b)) ]
    }
    m_bal: HashMap<address, U256> {
        in put(who, amount) => m_bal.insert(who, amount)
    }
    m_allow: HashMap<address, HashMap<address, U256>> {
        in approve(owner, spender, amount) => {
            let inner = if m_allow.exists(owner) { m_allow[owner] } else { {} };
            m_allow.update(owner, inner.update(spender, amount))
        }
    }
    m_pool: Pool {
        in constructor() => { let p = Pool { reserve_a: 0, reserve_b: 0 }; p }
        in fill(a, b) => { let p = Pool { reserve_a: a, reserve_b: b }; p }
    }
    m_phase: Phase {
        in constructor() => Phase::Open
        in close() => Phase::Closed
    }
    m_pick: Option<u64> {
        in constructor() => none
        in pick(v) => some(v)
    }
}

test "map key and emit" for T {
    let alice = 0x000000000000000000000000000000000000a11c
    call constructor()
    expect emit Moved(alice, 5)
    call put(alice, 5)
    expect state { m_bal[alice]: 5 }
    expect state { m_bal: { alice => 5 } }
}

test "nested map" for T {
    let alice = 0x000000000000000000000000000000000000a11c
    let bob = 0x000000000000000000000000000000000000b0b0
    call constructor()
    call approve(alice, bob, 7)
    expect state { m_allow[alice][bob]: 7 }
}

test "record field and tuple return" for T {
    call constructor()
    call fill(3, 4)
    expect state { m_pool.reserve_a: 3, m_pool.reserve_b: 4 }
    call pair()
    expect return.0 == 3
    expect return.1 == 4
}

test "enum and option" for T {
    call constructor()
    expect state { m_phase: Phase::Open, m_pick: none }
    call close()
    call pick(9)
    expect state { m_phase: Phase::Closed, m_pick: some(9) }
}
"#;

const HARNESS_RECORD_MAP_SRC: &str = r#"
record Slot {
    owner: address,
    amount: U256,
    tag: Option<u64>
}

entity S {
    event Put(indexed who: address, amount: U256);
    routes {
        #[factory_only]
        constructor() => []
        set(k: u64, who: address, amount: U256) => [
            emit Put(who, amount);
        ]
        own(who: address) => []
    }
    m_slots: HashMap<u64, Slot> {
        in set(k, who, amount) => { let s = Slot { owner: who, amount: amount, tag: some(k) }; m_slots.insert(k, s) }
    }
    m_count: HashMap<u64, U256> {
        in set(k, _, amount) => m_count.insert(k, amount)
    }
    m_owner: address {
        in constructor() => 0x0000000000000000000000000000000000000000 as address
        in own(who) => who
    }
}

test "map of records" for S {
    let carol = 0x000000000000000000000000000000000000cafe
    call constructor()
    expect emit Put(carol, 11)
    call set(3, carol, 11)
    expect state { m_slots[3].owner: carol, m_slots[3].amount: 11, m_slots[3].tag: some(3) }
    expect state { m_count: { 3 => 11 } }
    call own(carol)
    expect state { m_owner: carol }
}
"#;

#[test]
fn expect_state_uses_getter_arguments_and_destructures_structs() {
    let t = transpile_test_sol("harness-state", HARNESS_STATE_SRC);
    assert!(t.contains("_t.m_bal(alice)"), "{t}");
    assert!(t.contains("_t.m_allow(alice, bob)"), "{t}");
    assert!(t.contains("(uint256 _st_0_reserve_a, ) = _t.m_pool();"), "{t}");
    assert!(t.contains("assertEq(uint256(_t.m_phase()), uint256(Phase.Open)"), "{t}");
    assert!(t.contains("(Option_uint64_Tag _st_0_tag, ) = _t.m_pick();"), "{t}");
    assert!(!t.contains("__HashMap"), "{t}");
}

#[test]
fn expect_return_tuple_lens_reads_destructured_slot() {
    let t = transpile_test_sol("harness-lens", HARNESS_STATE_SRC);
    assert!(t.contains("assertEq(_ret_1_0, 3,"), "{t}");
    assert!(!t.contains("_ret_1.0"), "{t}");
}

#[test]
fn expect_emit_program_event_is_unqualified() {
    let t = transpile_test_sol("harness-emit", HARNESS_STATE_SRC);
    assert!(t.contains("emit Moved(alice, 5);"), "{t}");
    let t = transpile_test_sol("harness-emit-entity", HARNESS_RECORD_MAP_SRC);
    assert!(t.contains("emit S.Put(carol, 11);"), "{t}");
}

#[test]
fn untyped_let_used_as_address_is_declared_address() {
    let t = transpile_test_sol("harness-addr-let", HARNESS_STATE_SRC);
    assert!(t.contains("address alice = "), "{t}");
    assert!(!t.contains("uint64 alice"), "{t}");
}

#[test]
fn harness_state_paths_pass_under_forge() {
    for (stem, src) in [
        ("harness-state-forge", HARNESS_STATE_SRC),
        ("harness-record-map-forge", HARNESS_RECORD_MAP_SRC),
    ] {
        if let Some((_sol, log)) = forge_test_src(stem, src) {
            assert_all_pass(&log);
        }
    }
}

#[test]
fn t39_expect_state_on_member_without_getter() {
    let src = r#"
entity N {
    routes {
        #[factory_only]
        constructor() => []
        push(v: U256) => []
    }
    m_items: Vec<U256> {
        in push(v) => m_items.push(v)
    }
}

test "vec member" for N {
    call constructor()
    call push(1)
    expect state { m_items: array(1) }
}
"#;
    let (ok, out) = check_src("t39-evm", src, "evm");
    assert!(!ok && out.contains("T39") && out.contains("m_items"), "{out}");
    let (ok, out) = check_src("t39-lean", src, "lean");
    assert!(ok && !out.contains("T39"), "{out}");
}

const HARNESS_STRING_STATE_SRC: &str = r#"
record Note {
    text: String,
    n: U256,
}

entity S {
    routes {
        #[factory_only]
        constructor() => []
        set(s: String) => []
    }
    m_name: String {
        in set(s) => s
    }
    m_res: Option<String> {
        in set(s) => some(s)
    }
    m_note: Note {
        in set(s) => { Note { text: s, n: 2 } }
    }
}

test "string state" for S {
    call constructor()
    expect state { m_res: none }
    call set("hi")
    expect state { m_name: "hi", m_res: some("hi"), m_note.text: "hi", m_note.n: 2 }
}
"#;

#[test]
fn expect_state_reads_string_members_through_getters() {
    let (ok, out) = check_src("t39-string", HARNESS_STRING_STATE_SRC, "evm");
    assert!(ok && !out.contains("T39"), "{out}");
    if let Some((_sol, log)) = forge_test_src("harness-string-state-forge", HARNESS_STRING_STATE_SRC) {
        assert_all_pass(&log);
    }
}

const HARNESS_VEC_LENS_SRC: &str = r#"
entity V {
    routes {
        #[factory_only]
        constructor() => []
        push(v: U256) => []
        view items() -> Vec<U256> => [ return(m_vec) ]
    }
    m_vec: Vec<U256> {
        in push(v) => m_vec.push(v)
    }
}

test "vec return lens" for V {
    call constructor()
    call push(7)
    call push(8)
    call items()
    expect return.len == 2
    expect return[1] == 8
}
"#;

#[test]
fn expect_return_vec_lens_uses_length_and_index() {
    let t = transpile_test_sol("harness-vec-lens", HARNESS_VEC_LENS_SRC);
    assert!(t.contains("assertEq(_ret_1.length, 2,"), "{t}");
    assert!(t.contains("assertEq(_ret_1[1], 8,"), "{t}");
    if let Some((_sol, log)) = forge_test_src("harness-vec-lens-forge", HARNESS_VEC_LENS_SRC) {
        assert_all_pass(&log);
    }
}

// ---------------------------------------------------------------------------
// #55 (Lean), #58, #59
// ---------------------------------------------------------------------------

#[test]
fn lean_return_and_state_lenses_follow_types() {
    let src = r#"
record Pool {
    reserve_a: U256,
    reserve_b: U256
}

entity L {
    routes {
        #[factory_only]
        constructor() => []
        push(v: U256) => []
        fill(a: U256, b: U256) => []
        view triple() -> (U256, U256, U256) => [ return((1, 2, 3)) ]
        view items() -> Vec<U256> => [ return(m_vec) ]
    }
    m_vec: Vec<U256> {
        in push(v) => m_vec.push(v)
    }
    m_bal: HashMap<U256, U256> {
        in push(v) => m_bal.insert(v, v)
    }
    m_pool: Pool {
        in constructor() => { let p = Pool { reserve_a: 0, reserve_b: 0 }; p }
        in fill(a, b) => { let p = Pool { reserve_a: a, reserve_b: b }; p }
    }
}

test "tuple lenses" for L {
    call constructor()
    call triple()
    expect return.0 == 1
    expect return.1 == 2
    expect return.2 == 3
}

test "vec lenses" for L {
    call constructor()
    call push(7)
    call push(8)
    call items()
    expect return.len == 2
    expect return[1] == 8
    expect state { m_vec[0]: 7, m_bal[8]: 8 }
    call fill(3, 4)
    expect state { m_pool.reserve_a: 3 }
}
"#;
    lean_src(
        "lean-lenses",
        src,
        "LSpec.lean",
        &[
            "((_result_0).snd).snd = 3",
            "((_result_0).length : Cambrian.U256) = 2",
            "(List.getD (_result_0) 1 default) = 8",
            "(List.getD (((Cambrian.Generated.World.l w inst)).m_vec) 0 default) = 7",
        ],
    );
}

#[test]
fn lean_user_types_named_like_generated_ones() {
    let src = r#"
enum State { Idle, Busy }
record Identity { a: U256 }

entity R {
    enum Action { Go, Stop }
    routes {
        #[factory_only]
        constructor() => []
        go() => []
        view st() -> State => [ return(m_s) ]
    }
    m_s: State {
        in constructor() => State::Idle
        in go() => State::Busy
    }
    m_a: Action {
        in constructor() => Action::Stop
        in go() => Action::Go
    }
    m_id: Identity {
        in go() => { let i = Identity { a: 1 }; i }
    }
}

test "reserved names" for R {
    call constructor()
    call go()
    expect state { m_s: State::Busy, m_a: Action::Go }
    call st()
    expect return State::Busy
}
"#;
    lean_src(
        "lean-reserved-program",
        src,
        "R.lean",
        &["m_s : _root_.State", "m_id : _root_.Identity", "_root_.State.busy"],
    );
    let src = r#"
enum Action { Go, Stop }

entity Q {
    enum State { Idle, Busy }
    routes {
        #[factory_only]
        constructor() => []
        go() => []
    }
    m_a: Action {
        in constructor() => Action::Stop
        in go() => Action::Go
    }
    m_s: State {
        in constructor() => State::Idle
        in go() => State::Busy
    }
}

test "reserved names in spec" for Q {
    call constructor()
    call go()
    expect state { m_a: Action::Go, m_s: State::Busy }
}

invariant "phase is a known value" for Q {
    action go() { }
    check m_a == Action::Go || m_a == Action::Stop
}
"#;
    lean_src("lean-reserved-local", src, "Q.lean", &["inductive State_ where", "m_s : Q.State_"]);
    lean_src("lean-reserved-spec", src, "QSpec.lean", &["= _root_.Action.go", "= State_.busy"]);
}

const DEAD_WILDCARD_SRC: &str = r#"
enum Side { Buy, Sell }

pure fn unwrap0(o: Option<U256>) -> U256 {
    match o {
        some(x) => x,
        none => 0,
        _ => 5
    }
}

pure fn sign(s: Side) -> U256 {
    match s {
        Side::Buy => 1,
        Side::Sell => 2,
        _ => 0
    }
}

entity M {
    routes {
        #[factory_only]
        constructor() => []
        view f(s: Side) -> U256 => [ return(sign(s) + unwrap0(none)) ]
    }
    m_x: U256 { in constructor() => 0 }
}

test "dead wildcard" for M {
    call constructor()
    call f(Side::Sell)
    expect return 2
}
"#;

#[test]
fn lean_drops_dead_wildcard_arm() {
    lean_src(
        "lean-dead-wildcard",
        DEAD_WILDCARD_SRC,
        "Pure.lean",
        &[
            "| Side.sell => ((2 : BitVec 256)))",
            "| Option.none => ((0 : BitVec 256)))",
        ],
    );
}

#[test]
fn v47_warns_on_catch_all_after_exhaustive_arms() {
    let (ok, out) = check_src("v47-dead-wildcard", DEAD_WILDCARD_SRC, "lean");
    assert!(ok, "{out}");
    assert!(out.contains("V47") && out.contains("'sign'") && out.contains("'unwrap0'"), "{out}");
    let live = r#"
enum Side { Buy, Sell, Hold }
pure fn sign(s: Side) -> U256 {
    match s {
        Side::Buy => 1,
        Side::Sell => 2,
        _ => 0
    }
}
"#;
    let (_, out) = check_src("v47-live-wildcard", live, "lean");
    assert!(!out.contains("V47"), "{out}");
}

// ---------------------------------------------------------------------------
// #60: repo fixtures under single-file forge
// ---------------------------------------------------------------------------

#[test]
fn repo_fixtures_pass_under_forge() {
    for stem in ["escrow", "escrow_v2", "token"] {
        let entity = std::fs::read_to_string(format!("../contracts/{stem}.cam")).unwrap();
        let tests = std::fs::read_to_string(format!("../contracts/{stem}.test.cam")).unwrap();
        let src = format!("{entity}\n{tests}");
        if let Some((_sol, log)) = forge_test_src(&format!("fixture-{stem}-forge"), &src) {
            assert_all_pass(&log);
        }
    }
}

#[test]
fn factory_init_param_named_owner_does_not_shadow_factory_owner() {
    let src = r#"
entity Owned {
    routes {
        #[factory_only]
        constructor(owner: address) => []
    }
    m_owner: address {
        in constructor(owner) => owner
    }
}
"#;
    let dir = unique_out_dir("factory-owner-param");
    let cam = write_cam(&dir, "t.cam", src);
    let out = dir.join("out");
    let tp = transpile(&cam, &out, "evm");
    assert!(tp.status.success(), "{}", String::from_utf8_lossy(&tp.stderr));
    let factory = read_sol(&out);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(factory.contains("function deployOwned(address _owner)"), "{factory}");
    assert!(factory.contains("_instance.initialize(_owner);"), "{factory}");
    assert!(factory.contains("require(msg.sender == owner ||"), "{factory}");
}

// ---------------------------------------------------------------------------
// Stage 7d: pure-fn purity (V4), B-34 parameter typing, unsigned widen
// ---------------------------------------------------------------------------

#[test]
fn v4_flags_pure_and_library_fns_reading_members() {
    let src = r#"
library L {
    pure fn peek() -> U256 { m_count + 1 }
}
pure fn shadowed(x: U256) -> U256 { let m_count = x; m_count }
pure fn param(m_count: U256) -> U256 { m_count }
pure fn bare() -> U256 { m_count }
entity C {
    routes {
        #[factory_only]
        constructor() => []
    }
    m_count: U256 {
        in constructor() => 0
    }
}
"#;
    let (ok, out) = check_src("v4-members", src, "lean");
    assert!(!ok, "{out}");
    assert!(out.contains("'peek' references entity member 'm_count'"), "{out}");
    assert!(out.contains("'bare' references entity member 'm_count'"), "{out}");
    assert!(!out.contains("'shadowed' references") && !out.contains("'param' references"), "{out}");
}

const B34_SRC: &str = r#"
pure fn wide(a: i128, b: u8) -> i128 { a + (b as i128) }
pure fn narrow(a: i8, b: u8) -> i16 { (a as i16) + (b as i16) }
entity M {
    routes {
        #[factory_only]
        constructor() => []
        view go(x: i8, y: u8) -> i16 => [ return(narrow(x, y)) ]
        view go2(x: i128, y: u8) -> i128 => [ return(wide(x, y)) ]
        view go3(y: u8) -> i16 => [ return(y as i16) ]
    }
    m_x: U256 {
        in constructor() => 0
    }
}
"#;

#[test]
fn lean_pure_fn_params_typed_by_their_own_fn() {
    lean_src(
        "lean-b34",
        B34_SRC,
        "Pure.lean",
        &[
            "(pure (a + (Cambrian.castWidth 128 (b : BitVec 8))))",
            "(pure (((a).signExtend 16) + (Cambrian.castWidth 16 (b : BitVec 8))))",
        ],
    );
}

#[test]
fn lean_unsigned_source_widens_with_zeros() {
    lean_src(
        "lean-u8-as-i16",
        B34_SRC,
        "MRoutes.lean",
        &["return (s, (Cambrian.castWidth 16 (y : BitVec 8)))"],
    );
}

const WIDEN_SRC: &str = r#"
pure fn widen16(a: u8, b: u8) -> u16 { a + b }
pure fn call_mixed(a: u8, b: u8) -> u16 { widen16(a, b) + a }
pure fn ident8(a: i8) -> i8 { a }
pure fn sret(a: i8) -> i32 { ident8(a) }
entity W {
    routes {
        #[factory_only]
        constructor() => []
        put(a: u8, b: u8) => []
        view get(a: u8) -> u64 => [ return(widen16(a, a)) ]
        view get2(a: u8, b: u8) -> u16 => [ return(a + b) ]
    }
    m_x: u32 {
        in put(a, b) => widen16(a, b) + a
    }
    m_s: i32 {
        in put(a, b) => sret(-1) + call_mixed(a, b)
    }
}
"#;

#[test]
fn lean_results_widen_to_declared_width() {
    lean_src(
        "lean-widen-pure",
        WIDEN_SRC,
        "Pure.lean",
        &[
            "(Cambrian.castWidth 16 (a + b))",
            "((Cambrian.Generated.Pure.widen16 a b) + (Cambrian.castWidth 16 (a : BitVec 8)))",
            "(((Cambrian.Generated.Pure.ident8 a)).signExtend 32)",
        ],
    );
    lean_src(
        "lean-widen-member",
        WIDEN_SRC,
        "W.lean",
        &["(Cambrian.castWidth 32 ((Cambrian.Generated.Pure.widen16 a b) + (Cambrian.castWidth 16 (a : BitVec 8))))"],
    );
    lean_src(
        "lean-widen-return",
        WIDEN_SRC,
        "WRoutes.lean",
        &[
            "(s, (Cambrian.castWidth 64 ((Cambrian.Generated.Pure.widen16 a a) : BitVec 16)))",
            "(s, (Cambrian.castWidth 16 (a + b)))",
        ],
    );
}
