// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! P2 acceptance tests for the Lean target — single-entity properties.
//!
//! Layered like the P1 suite:
//!
//! 1. Library-level: `LeanBackend::extra_files` produces the per-entity
//!    `Cambrian/Generated/<E>Spec.lean` file with the right Lean
//!    surface (namespaces, theorem statements, `:= by sorry`).
//! 2. L-rule rejections: the validator hard-rejects `expect throw` on
//!    non-failing routes (`L5`) and `expect return` on routes without
//!    a return type (`L6`); `skip from` raises an `L7` warning.
//! 3. Optional `lake build` smoke check across all four exit fixtures
//!    (Counter / Escrow / LendingPair / a phased fixture) gated on
//!    `CAMBRIAN_TEST_LEAN_BUILD=1`.

use std::path::PathBuf;
use std::process::Command;

use cambrian_transpiler::ast;
use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::ProgramParser;

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

fn parse(source: &str) -> ast::Program {
    let mut p = ProgramParser::new()
        .parse(source)
        .expect("test fixture must parse");
    ast::normalize_program_types(&mut p);
    p
}

fn tempdir(stem: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("cambrian-lean-p2-{}-{}", stem, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn transpiler_bin() -> PathBuf {
    // `CARGO_BIN_EXE_*` rather than a walk up from `current_exe()`: the two
    // are the same path under the classic `target/debug/deps` layout, but a
    // cargo configured with a split build directory puts the test executable
    // somewhere else entirely, and the walk then points at a file that does
    // not exist. The failure reads as "No such file or directory" from the
    // spawn, which names neither the binary nor the layout.
    PathBuf::from(env!("CARGO_BIN_EXE_cambrian-transpiler"))
}

fn lake_available() -> bool {
    Command::new("lake")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Tiny self-contained Counter program with one test, one fuzz, one
/// property, and two invariants — enough to exercise every code path
/// in the spec emitter without depending on contracts/.
const COUNTER_WITH_SPECS: &str = r#"
entity Counter {
    routes {
        increment(amount: u64) => []
        reset() => []
        getCount() -> u64 => [
            return(m_count)
        ]
    }

    m_count: u64 {
        in increment(amount) => m_count + amount
        in reset() => 0
    }
}

test "increment adds to count" for Counter with { m_count: 5 } {
    call increment(3)
    expect state { m_count: 8 }
}

test "getCount returns current" for Counter with { m_count: 99 } {
    call getCount()
    expect return 99
}

property "increment from zero" (amount: u64) for Counter with { m_count: 0 } {
    call increment(amount)
    expect state { m_count: amount }

    fuzz { amount in 0..1000 }
}

property "reset always zeroes" (start: u64) for Counter {
    call reset()
    expect state { m_count: 0 }

    fuzz { start in 0..10000 }
}

invariant "count never exceeds bound" for Counter {
    init { m_count: 0 }

    action increment(amount: u64) {
        bound amount in 0..1000
    }
    action reset() { }

    check m_count <= 1000000000
}
"#;

// ---------------------------------------------------------------------------
// Layer 1: library-level — file shape
// ---------------------------------------------------------------------------

fn extras_for(source: &str, entity: &str) -> std::collections::HashMap<String, String> {
    let backend = LeanBackend::default();
    let program = parse(source);
    backend.extra_files(&program, entity).into_iter().collect()
}

#[test]
fn lean_p2_emits_per_entity_spec_file() {
    let extras = extras_for(COUNTER_WITH_SPECS, "Counter");

    let spec = extras
        .get("Cambrian/Generated/CounterSpec.lean")
        .expect("CounterSpec.lean must be in extra_files");

    assert!(
        spec.contains("import Cambrian.Prelude"),
        "spec file imports prelude:\n{}",
        spec
    );
    assert!(
        spec.contains("import Cambrian.Generated.Counter"),
        "spec file imports the generated entity:\n{}",
        spec
    );
    assert!(
        spec.contains("namespace Counter.Spec.Tests"),
        "tests namespace:\n{}",
        spec
    );
    assert!(
        spec.contains("namespace Counter.Spec.Properties"),
        "properties namespace:\n{}",
        spec
    );
    assert!(
        spec.contains("namespace Counter.Spec.Invariants"),
        "invariants namespace:\n{}",
        spec
    );
    // `test`, `property`, and `invariant` theorems now all carry an
    // auto-discharge reflection ladder; invariants additionally emit the
    // per-invariant `invByCases` scaffolding tactic. A bare `:= by sorry`
    // is no longer emitted — every proof goes through a `first | … | sorry`
    // ladder that degrades gracefully.
    assert!(
        !spec.contains(":= by sorry"),
        "theorems must use the reflection ladder, not a bare `:= by sorry`:\n{}",
        spec
    );
    assert!(
        spec.contains("scoped macro \"invByCases\""),
        "invariants must emit the `invByCases` scaffolding tactic:\n{}",
        spec
    );
    assert!(
        spec.contains("(invByCases; done)") && spec.contains("| sorry"),
        "invariant theorems must run the invByCases ladder with a sorry fallback:\n{}",
        spec
    );
    assert!(
        spec.contains("cambrian_route_simp"),
        "theorems must carry the reflection proof ladder:\n{}",
        spec
    );
}

#[test]
fn lean_p2_root_imports_spec_file_when_present() {
    let extras = extras_for(COUNTER_WITH_SPECS, "Counter");
    let root = extras
        .get("Cambrian.lean")
        .expect("Cambrian.lean must be in extra_files");
    assert!(
        root.contains("import Cambrian.Generated.CounterSpec"),
        "root must re-import the per-entity spec file:\n{}",
        root
    );
}

#[test]
fn lean_p2_root_omits_spec_import_when_no_specs() {
    let extras = extras_for(
        r#"
entity Counter {
    routes { increment(amount: u64) => [] }
    m_count: u64 { in increment(amount) => m_count + amount }
}
"#,
        "Counter",
    );
    let root = extras
        .get("Cambrian.lean")
        .expect("Cambrian.lean must be in extra_files");
    assert!(
        !root.contains("CounterSpec"),
        "root must not reference a spec file when none was emitted:\n{}",
        root
    );
    assert!(
        !extras.contains_key("Cambrian/Generated/CounterSpec.lean"),
        "no spec file should have been emitted: keys = {:?}",
        extras.keys().collect::<Vec<_>>(),
    );
}

#[test]
fn lean_p2_test_with_throw_uses_match_branch() {
    // `fund` has `where … : throw N` clauses ⇒ wrapped in
    // `Cambrian.RouteResult`. The test emitter must lower
    // `expect throw 11` as a `match` against `.err`.
    let extras = extras_for(
        r#"
entity E {
    routes {
        fund() where m_owner == 0x0 : throw 11 => []
    }
    m_owner: address { in fund() => 0x0 }
}

test "fund wrong owner throws" for E with { m_owner: 0x1 } {
    call fund()
    expect throw 11
}
"#,
        "E",
    );
    let spec = extras
        .get("Cambrian/Generated/ESpec.lean")
        .expect("ESpec.lean must be in extra_files");

    assert!(
        spec.contains("match"),
        "wrapped test must lower to a match expression:\n{}",
        spec
    );
    assert!(
        spec.contains("n_throw_0 = 11")
            || spec.contains("n = 11")
            || spec.contains(".err 11")
            || spec.contains("ofNat 11"),
        "throw code 11 must appear in the assertion (PL-F16 n_throw_0):\n{}",
        spec
    );
    // LG-007b: `expect throw` ⇒ `SpecStatementShape::needs_sorry_stub` — compile-safe
    // `:= by sorry` at theorem level (no simp ladder on specialized error arms).
    assert!(
        spec.contains(":= by sorry\n"),
        "expect-throw test must ship sorry-only proof (LG-007b):\n{}",
        spec
    );
    assert!(
        !spec.contains("cambrian_route_simp"),
        "expect-throw test must not run the reflection simp ladder:\n{}",
        spec
    );
    assert!(
        !spec.contains("n = 11 := by"),
        "proof must not glue := by onto the throw-code goal line (LG-007b):\n{}",
        spec
    );
}

#[test]
fn lean_p2_property_emits_quantified_theorem_without_sampling_bounds() {
    let extras = extras_for(COUNTER_WITH_SPECS, "Counter");
    let spec = extras
        .get("Cambrian/Generated/CounterSpec.lean")
        .expect("CounterSpec.lean must be in extra_files");

    // Isolate the Properties namespace so the invariant section (which
    // legitimately contains `<= 1000000000`) doesn't confuse the
    // sampling-bound assertions.
    let start = spec
        .find("namespace Counter.Spec.Properties")
        .expect("properties namespace present");
    let end = spec[start..]
        .find("end Counter.Spec.Properties")
        .map(|o| start + o)
        .expect("properties namespace closed");
    let props = &spec[start..end];

    assert!(
        props.contains("∀ (amount"),
        "property theorem must quantify over its parameter:\n{}",
        props
    );
    // The redesign's core win: sampling `bound`s live only in the derived
    // fuzz harness, never in the Lean theorem. The endpoints `1000` /
    // `10000` and any `.toNat`-style refinement must be absent here.
    assert!(
        !props.contains(".toNat"),
        "property theorem must NOT carry sampling-bound refinements:\n{}",
        props
    );
    assert!(
        !props.contains("1000") && !props.contains("10000"),
        "sampling-range endpoints must NOT leak into the Lean theorem:\n{}",
        props
    );
}

#[test]
fn lean_forall_property_quantifies_state_field_and_ctx() {
    let extras = extras_for(
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

property "reset zeroes from any start" for Counter with { m_count: * } {
    call reset()
    expect state { m_count: 0 }
}

property "incr from any sender" (amount: u64) for Counter with { m_count: 0 } ctx { msg::sender: * } {
    call increment(amount)
    expect state { m_count: amount }

    fuzz { amount in 0..10 }
}
"#,
        "Counter",
    );
    let spec = extras
        .get("Cambrian/Generated/CounterSpec.lean")
        .expect("CounterSpec.lean must be in extra_files");

    // The forall state field is quantified and seeds the initial state.
    assert!(
        spec.contains("∀ (m_count : BitVec 64)"),
        "forall state field must be a quantified variable:\n{}",
        spec
    );
    assert!(
        spec.contains("Counter.State.default with m_count := m_count"),
        "forall state field must seed the initial world from the quantified var:\n{}",
        spec
    );
    // The forall context param is quantified and seeds the initial ctx.
    assert!(
        spec.contains("(sender : Cambrian.Address)"),
        "forall ctx param must be a quantified variable:\n{}",
        spec
    );
    assert!(
        spec.contains("Cambrian.MsgCtx.default with sender := sender"),
        "forall ctx param must seed the initial MsgCtx:\n{}",
        spec
    );
}

#[test]
fn lean_forall_invariant_quantifies_init_field() {
    let extras = extras_for(
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

invariant "stays bounded from any start" for Counter {
    init { m_count: * }

    action reset() { }

    check m_count <= 1000000000
}
"#,
        "Counter",
    );
    let spec = extras
        .get("Cambrian/Generated/CounterSpec.lean")
        .expect("CounterSpec.lean must be in extra_files");

    let start = spec
        .find("namespace Counter.Spec.Invariants")
        .expect("invariants namespace present");
    let inv = &spec[start..];
    assert!(
        inv.contains("∀ (m_count : BitVec 64)"),
        "invariant theorem must quantify over the forall-ized init field:\n{}",
        inv
    );
    assert!(
        inv.contains("Counter.State.default with m_count := m_count"),
        "invariant init world must use the quantified var:\n{}",
        inv
    );
}

/// "Randomize everything except this field": a bare `*` forall-izes every
/// state member, while an explicit pin holds the listed field fixed. On Lean
/// that means a `∀` over the non-pinned members only.
#[test]
fn lean_forall_invariant_all_but_pinned_field() {
    let extras = extras_for(
        r#"
entity Pair {
    routes {
        bump(amount: u64) => []
        reset() => []
    }
    m_a: u64 {
        in bump(amount) => m_a + amount
        in reset() => 0
    }
    m_b: u64 {
        in bump(amount) => m_b + amount
        in reset() => 0
    }
}

invariant "b bounded from any start but a" for Pair {
    init { *, m_a: 0 }

    action reset() { }

    check m_b >= 0
}
"#,
        "Pair",
    );
    let spec = extras
        .get("Cambrian/Generated/PairSpec.lean")
        .expect("PairSpec.lean must be in extra_files");

    let start = spec
        .find("namespace Pair.Spec.Invariants")
        .expect("invariants namespace present");
    let inv = &spec[start..];

    // The non-pinned member is universally quantified...
    assert!(
        inv.contains("∀ (m_b : BitVec 64)"),
        "bare '*' must quantify the non-pinned member m_b:\n{}",
        inv
    );
    assert!(
        inv.contains("with") && inv.contains("m_b := m_b"),
        "the quantified var must seed the initial world:\n{}",
        inv
    );
    // ...while the pinned member is NOT quantified (it stays fixed at 0).
    assert!(
        !inv.contains("∀ (m_a : BitVec 64)"),
        "the pinned member m_a must not be quantified:\n{}",
        inv
    );
}

#[test]
fn lean_invariant_ctx_quantifies_sender_and_pins_value() {
    let extras = extras_for(
        r#"
entity Pair {
    routes {
        bump(amount: u64) => []
        reset() => []
    }
    m_a: u64 {
        in bump(amount) => m_a + amount
        in reset() => 0
    }
}

invariant "bounded from any sender" for Pair {
    init { m_a: 0 }
    ctx { msg::sender: *, msg::value: 0 }

    action reset() { }

    check m_a >= 0
}
"#,
        "Pair",
    );
    let spec = extras
        .get("Cambrian/Generated/PairSpec.lean")
        .expect("PairSpec.lean must be in extra_files");
    let start = spec
        .find("namespace Pair.Spec.Invariants")
        .expect("invariants namespace present");
    let inv = &spec[start..];

    // Forall msg::sender is `∀`-bound and seeds the initial MsgCtx ...
    assert!(
        inv.contains("(ctx_sender : Cambrian.Address)"),
        "forall msg::sender must be a quantified MsgCtx var:\n{}",
        inv
    );
    assert!(
        inv.contains("Cambrian.MsgCtx.default with") && inv.contains("sender := ctx_sender"),
        "the quantified sender must seed the initial MsgCtx:\n{}",
        inv
    );
    // ... and the concrete msg::value pin seeds the ctx with a literal.
    assert!(
        inv.contains("value :="),
        "the concrete msg::value pin must seed the initial MsgCtx:\n{}",
        inv
    );
}

/// `ctx { sys::now: * }` on a property quantifies a `BitVec 64` and seeds the
/// initial world's `BlockEnv` (sys reads `w.block`, not a threaded `SysCtx`).
/// A concrete `sys::chainid` pin seeds the same block.
#[test]
fn lean_property_ctx_sys_seeds_world_block() {
    let extras = extras_for(
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

property "incr from any time" (amount: u64) for Counter with { m_count: 0 } ctx { sys::now: *, sys::chainid: 7 } {
    call increment(amount)
    expect state { m_count: amount }

    fuzz { amount in 0..10 }
}
"#,
        "Counter",
    );
    let spec = extras
        .get("Cambrian/Generated/CounterSpec.lean")
        .expect("CounterSpec.lean must be in extra_files");

    // sys::now forall → BitVec 64 quantifier seeding w.block.timestamp.
    assert!(
        spec.contains("(now : BitVec 64)"),
        "forall sys::now must be a quantified BitVec 64 var:\n{}",
        spec
    );
    assert!(
        spec.contains("w with block := { w.block with") && spec.contains("timestamp := now"),
        "forall sys::now must seed the initial world block timestamp:\n{}",
        spec
    );
    // sys::chainid pin → block.chainId seeded with a literal.
    assert!(
        spec.contains("chainId := 7"),
        "concrete sys::chainid pin must seed the block chainId:\n{}",
        spec
    );
    // No dead SysCtx local should be emitted anymore.
    assert!(
        !spec.contains("Cambrian.SysCtx"),
        "the dead SysCtx local must not be emitted in specs:\n{}",
        spec
    );
}

/// Invariant `ctx { sys::now: * }` quantifies a `BitVec 64` and seeds the
/// initial world's block (single-entity path).
#[test]
fn lean_invariant_ctx_sys_seeds_world_block() {
    let extras = extras_for(
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

invariant "bounded from any time" for Counter {
    init { m_count: 0 }
    ctx { sys::now: * }

    action reset() { }

    check m_count >= 0
}
"#,
        "Counter",
    );
    let spec = extras
        .get("Cambrian/Generated/CounterSpec.lean")
        .expect("CounterSpec.lean must be in extra_files");
    let start = spec
        .find("namespace Counter.Spec.Invariants")
        .expect("invariants namespace present");
    let inv = &spec[start..];

    assert!(
        inv.contains("(ctx_now : BitVec 64)"),
        "forall sys::now must be a quantified BitVec 64 var:\n{}",
        inv
    );
    assert!(
        inv.contains("w with block := { w.block with") && inv.contains("timestamp := ctx_now"),
        "forall sys::now must seed the initial invariant world block:\n{}",
        inv
    );
}

/// `ctx { sys::balance: * }` quantifies a `U256` and seeds the entity's own
/// balance via a pointwise `w.balances` update — on both properties and
/// single-entity invariants. A concrete pin seeds a literal.
#[test]
fn lean_ctx_sys_balance_seeds_world_balances() {
    let extras = extras_for(
        r#"
entity Vault {
    routes {
        deposit(amount: u64) => []
        getBal() -> U256 => [ return(sys::balance) ]
    }
    m_seen: u64 {
        in deposit(amount) => m_seen + amount
    }
}

property "balance forall" for Vault with { m_seen: 0 } ctx { sys::balance: * } {
    call deposit(1)
    expect state { m_seen: 1 }
}

invariant "bal pinned inv" for Vault {
    init { m_seen: 0 }
    ctx { sys::balance: 1000 }
    action deposit(amount: u64) { bound amount in 0..10 }
    check m_seen >= 0
}
"#,
        "Vault",
    );
    let spec = extras
        .get("Cambrian/Generated/VaultSpec.lean")
        .expect("VaultSpec.lean must be in extra_files");

    // Property: forall sys::balance → ∀ U256 var seeding w.balances at self addr.
    assert!(
        spec.contains("(balance : Cambrian.U256)"),
        "forall sys::balance must be a quantified U256 var:\n{}",
        spec
    );
    assert!(
        spec.contains("balances := fun a => if a = Vault.address inst then balance else"),
        "forall sys::balance must seed w.balances at the entity's own address:\n{}",
        spec
    );
    // Invariant: concrete sys::balance pin → literal seed on w.balances.
    let start = spec
        .find("namespace Vault.Spec.Invariants")
        .expect("invariants namespace present");
    let inv = &spec[start..];
    assert!(
        inv.contains("balances := fun a => if a = Vault.address inst then 1000 else"),
        "concrete sys::balance pin must seed w.balances with the literal:\n{}",
        inv
    );
}

/// A body-level `sys { now: N }` step (in a `test`/property body) re-binds the
/// world block (`w.block.timestamp`) rather than emitting the dead `SysCtx`
/// local — so subsequent calls in the trace observe the updated clock.
#[test]
fn lean_body_sys_step_updates_world_block() {
    let extras = extras_for(
        r#"
entity Vault {
    routes { deposit(amount: u64) => [] }
    m_balance: u64 { in deposit(amount) => m_balance + amount }
}

test "deposit at fixed time" for Vault with { m_balance: 0 } {
    sys { now: 1000 }
    call deposit(100)
    expect state { m_balance: 100 }
}
"#,
        "Vault",
    );
    let spec = extras
        .get("Cambrian/Generated/VaultSpec.lean")
        .expect("VaultSpec.lean must be in extra_files");
    assert!(
        spec.contains("let w := { w with block := { w.block with timestamp := 1000 } }"),
        "body `sys {{ now }}` step must re-bind the world block:\n{}",
        spec
    );
    assert!(
        !spec.contains("Cambrian.SysCtx"),
        "the dead SysCtx local must not be emitted for body sys steps:\n{}",
        spec
    );
}

/// Multi-entity invariant `ctx { sys::now: * }` quantifies a `BitVec 64` and
/// seeds the (shared) initial world block — exercising the multi-entity emit
/// path (distinct from the single-entity one).
#[test]
fn lean_invariant_multi_ctx_sys_seeds_world_block() {
    let extras = extras_for(
        r#"
entity Vault {
    routes { ping() => [] }
    m_a: u64 { in ping() => 0 }
}

entity Treasury {
    routes { tap() => [] }
    m_b: u64 { in tap() => 0 }
}

invariant "two-entity time" for { v: Vault, t: Treasury } {
    init v { m_a: 0 }
    init t { m_b: 0 }
    ctx { sys::now: * }
    action v.ping() { }
    action t.tap() { }
    check v.m_a == 0
}
"#,
        "Vault",
    );
    let spec = extras
        .get("Cambrian/Generated/VaultSpec.lean")
        .expect("VaultSpec.lean must be in extra_files");
    let start = spec
        .find("namespace Vault.Spec.Invariants")
        .expect("invariants namespace present");
    let inv = &spec[start..];
    assert!(
        inv.contains("(ctx_now : BitVec 64)"),
        "multi-entity forall sys::now must be a quantified BitVec 64 var:\n{}",
        inv
    );
    assert!(
        inv.contains("w with block := { w.block with") && inv.contains("timestamp := ctx_now"),
        "multi-entity forall sys::now must seed the initial world block:\n{}",
        inv
    );
}

#[test]
fn lean_p2_invariant_emits_action_inductive_and_step() {
    let extras = extras_for(COUNTER_WITH_SPECS, "Counter");
    let spec = extras
        .get("Cambrian/Generated/CounterSpec.lean")
        .expect("CounterSpec.lean must be in extra_files");

    assert!(
        spec.contains("inductive Action"),
        "invariant must declare an Action inductive:\n{}",
        spec
    );
    assert!(
        spec.contains("def step"),
        "invariant must declare a step function:\n{}",
        spec
    );
    assert!(
        spec.contains("def runTrace"),
        "invariant must declare a runTrace function:\n{}",
        spec
    );
    assert!(
        spec.contains("theorem count_never_exceeds_bound"),
        "invariant theorem name should match the slugified invariant name:\n{}",
        spec
    );
}

/// A trace-using invariant emits the minimal accumulator + validity
/// machinery and threads a `traceValid` hypothesis into the theorem.
const COUNTER_TRACE_INVARIANT: &str = r#"
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

invariant "trace bounded" for Counter {
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
"#;

#[test]
fn lean_invariant_trace_emits_accumulator_and_valid_fold() {
    let extras = extras_for(COUNTER_TRACE_INVARIANT, "Counter");
    let spec = extras
        .get("Cambrian/Generated/CounterSpec.lean")
        .expect("CounterSpec.lean must be in extra_files");

    // Minimal accumulator: only referenced fields (len / count_* / last).
    assert!(
        spec.contains("structure TraceAcc"),
        "trace-using invariant must emit a TraceAcc structure:\n{spec}"
    );
    assert!(
        spec.contains("len :")
            && spec.contains("count_increment")
            && spec.contains("count_reset")
            && spec.contains("last :"),
        "TraceAcc must carry the referenced len/count/last fields:\n{spec}"
    );
    assert!(
        spec.contains("inductive LastAction"),
        "lastWas usage must emit a LastAction inductive:\n{spec}"
    );
    assert!(
        spec.contains("def accInit") && spec.contains("def accUpdate"),
        "accumulator init/update must be emitted:\n{spec}"
    );
    assert!(
        spec.contains("def finalAcc"),
        "a check that reads trace:: must emit a finalAcc fold:\n{spec}"
    );

    // Validity predicates and the exclude-semantics hypothesis.
    assert!(
        spec.contains("def stepValid"),
        "assume conditions must lower to a stepValid predicate:\n{spec}"
    );
    assert!(
        spec.contains("def traceValid"),
        "assume conditions must lower to a traceValid fold:\n{spec}"
    );
    assert!(
        spec.contains("traceValid w inst ctx accInit trace = true →"),
        "the theorem must gain a traceValid exclude hypothesis:\n{spec}"
    );

    // Accessor lowering reads from the in-scope accumulator.
    assert!(
        spec.contains("acc.len"),
        "trace::length should lower to acc.len:\n{spec}"
    );
    assert!(
        spec.contains("acc.count_increment") && spec.contains("acc.count_reset"),
        "trace::count(r) should lower to acc.count_r:\n{spec}"
    );
    assert!(
        spec.contains("acc.last") && spec.contains("LastAction.reset"),
        "trace::lastWas(r) should compare acc.last to LastAction.r:\n{spec}"
    );
}

/// An invariant *without* any `assume` must not emit the trace/validity
/// machinery — output stays as it was before the feature.
#[test]
fn lean_invariant_without_assume_omits_trace_machinery() {
    let extras = extras_for(COUNTER_WITH_SPECS, "Counter");
    let spec = extras
        .get("Cambrian/Generated/CounterSpec.lean")
        .expect("CounterSpec.lean must be in extra_files");

    assert!(
        !spec.contains("def stepValid"),
        "no-assume invariant must not emit stepValid:\n{spec}"
    );
    assert!(
        !spec.contains("def traceValid"),
        "no-assume invariant must not emit traceValid:\n{spec}"
    );
    assert!(
        !spec.contains("structure TraceAcc"),
        "no-assume invariant must not emit a TraceAcc:\n{spec}"
    );
    assert!(
        !spec.contains("traceValid"),
        "no-assume invariant theorem must not mention traceValid:\n{spec}"
    );
}

/// Direct4 shape: a valued `pure` route returns `World × T`; invariant `step`
/// must project `.fst`. A `check` that names action parameters must bind them
/// by folding over `trace` (they are not post-trace world fields).
const VALUED_PURE_INVARIANT: &str = r#"
entity Math {
    routes {
        pure add(x: u64, y: u64) -> u64 => [
            return(x + y)
        ]
    }
}

invariant "args positive" for Math {
    action add(x: u64, y: u64) {
        bound x in 1..100
        bound y in 1..100
    }
    check x > 0
}
"#;

#[test]
fn lean_invariant_valued_pure_route_projects_world_fst() {
    let extras = extras_for(VALUED_PURE_INVARIANT, "Math");
    let spec = extras
        .get("Cambrian/Generated/MathSpec.lean")
        .expect("MathSpec.lean must be in extra_files");

    assert!(
        spec.contains("(Math.Routes.add w inst ctx x y).fst"),
        "valued pure route in `step` must project `.fst` onto World:\n{spec}",
    );
    assert!(
        !spec.contains("| .add x y =>\n    Math.Routes.add w inst ctx x y\n"),
        "valued pure route must not be returned as World × T from `step`:\n{spec}",
    );
}

#[test]
fn lean_invariant_check_binds_action_params_over_trace() {
    let extras = extras_for(VALUED_PURE_INVARIANT, "Math");
    let spec = extras
        .get("Cambrian/Generated/MathSpec.lean")
        .expect("MathSpec.lean must be in extra_files");

    assert!(
        spec.contains("(∀ (a : Action), a ∈ trace → (match a with | .add x y =>"),
        "check mentioning action params must quantify over trace actions:\n{spec}",
    );
    assert!(
        spec.contains("| .add x y => (x > 0)") || spec.contains("| .add x y => (x > 0))"),
        "match arm must bind action params used in the check:\n{spec}",
    );
}

#[test]
fn lean_p4_invariant_emits_multi_entity() {
    let extras = extras_for(
        r#"
entity Vault {
    routes { ping() => [] }
    m_a: u64 { in ping() => 0 }
}

entity Treasury {
    routes { tap() => [] }
    m_b: u64 { in tap() => 0 }
}

invariant "two-entity placeholder" for { v: Vault, t: Treasury } {
    init v { m_a: 0 }
    init t { m_b: 0 }
    action v.ping() { }
    action t.tap() { }
    check v.m_a == 0
}
"#,
        "Vault",
    );
    let spec = extras
        .get("Cambrian/Generated/VaultSpec.lean")
        .expect("VaultSpec.lean must be in extra_files");
    assert!(
        spec.contains("inductive Action") && spec.contains("v_ping"),
        "multi-entity invariant must emit Action constructors (P4):\n{}",
        spec
    );
    assert!(
        !spec.contains("TODO(P4): multi-entity"),
        "multi-entity invariant must not be skipped after P4:\n{}",
        spec
    );
    assert!(
        spec.contains("theorem two_entity_placeholder"),
        "multi-entity invariant must emit a theorem (P4):\n{}",
        spec
    );
}

// ---------------------------------------------------------------------------
// Layer 2: L-rule rejections (L5 / L6 / L7)
// ---------------------------------------------------------------------------

#[test]
fn lean_p2_l5_rejects_expect_throw_on_non_failing_route() {
    use cambrian_transpiler::validate::{check_lean_target_compat, Severity};
    let prog = parse(
        r#"
entity Counter {
    routes { reset() => [] }
    m_count: u64 { in reset() => 0 }
}

test "reset throws" for Counter {
    call reset()
    expect throw 1
}
"#,
    );
    let diags = check_lean_target_compat(&prog);
    assert!(
        diags
            .iter()
            .any(|d| d.code == "L5" && matches!(d.severity, Severity::Error)),
        "expected an L5 error: {:?}",
        diags
    );
}

#[test]
fn lean_p2_l6_rejects_expect_return_on_mutating_route() {
    use cambrian_transpiler::validate::{check_lean_target_compat, Severity};
    let prog = parse(
        r#"
entity Counter {
    routes { reset() => [] }
    m_count: u64 { in reset() => 0 }
}

test "reset returns 0" for Counter {
    call reset()
    expect return 0
}
"#,
    );
    let diags = check_lean_target_compat(&prog);
    assert!(
        diags
            .iter()
            .any(|d| d.code == "L6" && matches!(d.severity, Severity::Error)),
        "expected an L6 error: {:?}",
        diags
    );
}

#[test]
fn lean_p2_l7_warns_on_skip_from() {
    use cambrian_transpiler::validate::{check_lean_target_compat, Severity};
    let prog = parse(
        r#"
entity Counter {
    routes { reset() => [] }
    m_count: u64 { in reset() => 0 }
}

test "skip from variant" for Counter skip from {
    call reset()
}
"#,
    );
    let diags = check_lean_target_compat(&prog);
    assert!(
        diags
            .iter()
            .any(|d| d.code == "L7" && matches!(d.severity, Severity::Warning)),
        "expected an L7 warning: {:?}",
        diags
    );
}

#[test]
fn lean_p2_l5_l6_pass_for_valid_shapes() {
    use cambrian_transpiler::validate::check_lean_target_compat;
    let prog = parse(COUNTER_WITH_SPECS);
    let diags = check_lean_target_compat(&prog);
    assert!(
        !diags.iter().any(|d| d.code == "L5"),
        "no L5 diagnostic for valid shapes: {:?}",
        diags
    );
    assert!(
        !diags.iter().any(|d| d.code == "L6"),
        "no L6 diagnostic for valid shapes: {:?}",
        diags
    );
}

// ---------------------------------------------------------------------------
// `lean.proof_helpers: false` kill switch
// ---------------------------------------------------------------------------

/// Run the Lean backend in project mode with `lean.proof_helpers: false`
/// and return all generated files keyed by relative path.
fn gen_project_no_helpers(source: &str) -> std::collections::HashMap<String, String> {
    use cambrian_transpiler::project::Project;
    let dir = tempdir("no-helpers");
    std::fs::write(dir.join("counter.cam"), source).unwrap();
    std::fs::write(
        dir.join("project.yaml"),
        "name: counter\ntarget: lean\nsources:\n  - counter.cam\nlean:\n  proof_helpers: false\n",
    )
    .unwrap();
    let project = Project::load(&dir.join("project.yaml")).expect("load project");
    let files: std::collections::HashMap<String, String> = LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    files
}

#[test]
fn lean_proof_helpers_off_strips_all_proof_assistance() {
    let files = gen_project_no_helpers(COUNTER_WITH_SPECS);

    let spec = files
        .iter()
        .find(|(p, _)| p.ends_with("CounterSpec.lean"))
        .map(|(_, c)| c.as_str())
        .expect("CounterSpec.lean generated");

    // Spec theorems degrade to statement-only stubs.
    assert!(
        spec.contains(":= by sorry"),
        "spec theorems must ship `:= by sorry` when helpers are off:\n{}",
        spec
    );
    // None of the proof-assistance scaffolding may appear.
    for needle in [
        "invByCases",
        "scoped macro",
        "| sorry",
        "cambrian_route_simp",
        "maxHeartbeats",
    ] {
        assert!(
            !spec.contains(needle),
            "spec must not contain `{needle}` when helpers are off:\n{spec}",
        );
    }

    // The route file must carry only semantic defs — no reflection lemmas
    // or `cambrian_*_simp` attribute tags.
    let routes = files
        .iter()
        .find(|(p, _)| p.ends_with("CounterRoutes.lean"))
        .map(|(_, c)| c.as_str())
        .expect("CounterRoutes.lean generated");
    for needle in [
        "attribute [cambrian_route_simp]",
        "attribute [cambrian_pre_simp]",
        "_isOk_iff",
        ".Pre :",
    ] {
        assert!(
            !routes.contains(needle),
            "route file must not contain `{needle}` when helpers are off:\n{routes}",
        );
    }
}

#[test]
fn lean_proof_helpers_default_on_emits_ladders() {
    // Sanity counterpart: the same program with the default config keeps
    // the reflection ladder + invByCases (guards against the kill switch
    // silently latching on).
    let extras = extras_for(COUNTER_WITH_SPECS, "Counter");
    let spec = extras
        .get("Cambrian/Generated/CounterSpec.lean")
        .expect("CounterSpec.lean must be in extra_files");
    assert!(spec.contains("invByCases"));
    assert!(spec.contains("cambrian_route_simp"));
}

// ---------------------------------------------------------------------------
// `lean.numerics: nat` — unsigned scalars → `Nat`, signed → `Int`.
// ---------------------------------------------------------------------------

/// Emit `source` through the project-mode Lean backend with
/// `lean.numerics: nat` and return every generated file keyed by its
/// relative path. Project mode is required: nat lowering only exists on
/// the `gen_project` path (single-file mode keeps BitVec numerics).
fn gen_project_nat(source: &str, stem: &str) -> std::collections::HashMap<String, String> {
    use cambrian_transpiler::project::Project;

    let dir = tempdir(stem);
    std::fs::write(dir.join("counter.cam"), source).unwrap();
    std::fs::write(
        dir.join("project.yaml"),
        "name: counter\ntarget: lean\nsources:\n  - counter.cam\nlean:\n  numerics: nat\n",
    )
    .unwrap();

    let project = Project::load(&dir.join("project.yaml")).expect("load nat project");
    let files: std::collections::HashMap<String, String> = LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    files
}

#[test]
fn lean_numerics_nat_lowers_scalars_to_nat() {
    let files = gen_project_nat(COUNTER_WITH_SPECS, "numerics-nat-scalars");

    // Property theorem: the `amount` binder is `Nat`, and the statement is
    // free of any fixed-width leakage (`BitVec`, `castWidth`, `#N` literals).
    let spec = files
        .get("Cambrian/Generated/CounterSpec.lean")
        .expect("CounterSpec.lean must be generated");
    assert!(
        spec.contains("∀ (amount : Nat)"),
        "nat-mode property binder must be `Nat`:\n{spec}",
    );
    assert!(
        !spec.contains("BitVec"),
        "nat-mode spec must not mention BitVec:\n{spec}",
    );
    assert!(
        !spec.contains("castWidth"),
        "nat-mode spec must not emit castWidth:\n{spec}",
    );

    // Entity file: the state member type + default lower to `Nat` / `0`
    // (not `0#64`). The CREATE2 salt / initCodeHash infra intentionally
    // stays `BitVec 256`, so we don't assert BitVec-freedom here.
    let entity = files
        .get("Cambrian/Generated/Counter.lean")
        .expect("Counter.lean must be generated");
    assert!(
        entity.contains("m_count : Nat"),
        "state member lowers to Nat:\n{entity}",
    );
    assert!(
        !entity.contains("0#64"),
        "nat-mode default must be `0`, not `0#64`:\n{entity}",
    );

    // Routes / member transforms: arithmetic stays plain `Nat` — no
    // `BitVec` types and no `castWidth` width-promotion.
    let routes = files
        .get("Cambrian/Generated/CounterRoutes.lean")
        .expect("CounterRoutes.lean must be generated");
    assert!(
        !routes.contains("BitVec") && !routes.contains("castWidth"),
        "nat-mode routes must be BitVec-/castWidth-free:\n{routes}",
    );
}

// ---------------------------------------------------------------------------
// `lean.intrinsics` — opaque (default) vs executable prelude variants.
// ---------------------------------------------------------------------------

/// Generate `COUNTER_WITH_SPECS` in project mode with an explicit `lean:`
/// block and return every emitted file keyed by relative path.
fn gen_project_lean(stem: &str, lean_block: &str) -> std::collections::HashMap<String, String> {
    use cambrian_transpiler::project::Project;

    let dir = tempdir(stem);
    std::fs::write(dir.join("counter.cam"), COUNTER_WITH_SPECS).unwrap();
    std::fs::write(
        dir.join("project.yaml"),
        format!("name: counter\ntarget: lean\nsources:\n  - counter.cam\n{lean_block}"),
    )
    .unwrap();

    let project = Project::load(&dir.join("project.yaml")).expect("load project");
    let files: std::collections::HashMap<String, String> = LeanBackend::default()
        .gen_project(&project)
        .into_iter()
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    files
}

#[test]
fn lean_intrinsics_default_opaque_main() {
    let files = gen_project_lean("intrinsics-default", "");

    // Default: the main Evm module keeps the opaque models + injectivity axiom
    // (FV-sound) and carries none of the executable machinery.
    let main = files.get("Cambrian/Evm.lean").expect("main Evm");
    assert!(
        main.contains("opaque keccak256"),
        "default main Evm keeps opaque keccak256:\n{main}",
    );
    assert!(
        main.contains("axiom create2Address_injective"),
        "default main Evm keeps the injectivity axiom:\n{main}",
    );
    assert!(
        !main.contains("class CamHashable"),
        "default main Evm must not carry executable models:\n{main}",
    );
}

#[test]
fn lean_intrinsics_executable_flag_switches_main_prelude() {
    let files = gen_project_lean("intrinsics-exec", "lean:\n  intrinsics: executable\n");

    let main = files.get("Cambrian/Evm.lean").expect("main Evm");
    assert!(
        main.contains("@[irreducible] def keccak256"),
        "`intrinsics: executable` makes the main Evm computable:\n{main}",
    );
    assert!(
        !main.contains("opaque keccak256"),
        "executable main Evm drops the opaque models:\n{main}",
    );
    assert!(
        !main.contains("axiom create2Address_injective"),
        "executable main Evm drops the injectivity axiom:\n{main}",
    );
}

// ---------------------------------------------------------------------------
// Layer 3: optional `lake build` smoke across the full P2 surface
// ---------------------------------------------------------------------------

#[test]
fn lean_p2_lake_build_smoke_counter_specs() {
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() != Ok("1") {
        eprintln!("skipping lake-build smoke (set CAMBRIAN_TEST_LEAN_BUILD=1 to enable)");
        return;
    }
    if !lake_available() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }

    let dir = tempdir("smoke-counter-specs");
    let cam_path = dir.join("counter.cam");
    let out_dir = dir.join("out");
    std::fs::write(&cam_path, COUNTER_WITH_SPECS).unwrap();

    let tp = Command::new(transpiler_bin())
        .arg(&cam_path)
        .arg("-o")
        .arg(&out_dir)
        .arg("--target")
        .arg("lean")
        .output()
        .expect("invoke transpiler");
    assert!(
        tp.status.success(),
        "transpile failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&tp.stdout),
        String::from_utf8_lossy(&tp.stderr),
    );

    let lake = Command::new("lake")
        .arg("build")
        .current_dir(&out_dir)
        .output()
        .expect("invoke lake");
    assert!(
        lake.status.success(),
        "lake build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&lake.stdout),
        String::from_utf8_lossy(&lake.stderr),
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lean_invariant_valued_pure_route_lake_build() {
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() != Ok("1") {
        eprintln!("skipping lake-build smoke (set CAMBRIAN_TEST_LEAN_BUILD=1 to enable)");
        return;
    }
    if !lake_available() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }

    let dir = tempdir("smoke-valued-pure-inv");
    let cam_path = dir.join("valued_pure_inv.cam");
    let out_dir = dir.join("out");
    std::fs::write(&cam_path, VALUED_PURE_INVARIANT).unwrap();

    let tp = Command::new(transpiler_bin())
        .arg(&cam_path)
        .arg("-o")
        .arg(&out_dir)
        .arg("--target")
        .arg("lean")
        .output()
        .expect("invoke transpiler");
    assert!(
        tp.status.success(),
        "transpile failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&tp.stdout),
        String::from_utf8_lossy(&tp.stderr),
    );

    let lake = Command::new("lake")
        .arg("build")
        .current_dir(&out_dir)
        .output()
        .expect("invoke lake");
    assert!(
        lake.status.success(),
        "lake build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&lake.stdout),
        String::from_utf8_lossy(&lake.stderr),
    );

    let _ = std::fs::remove_dir_all(&dir);
}

const MACRO_IN_WHERE: &str = r#"
entity Gate {
    routes {
        constructor() => []
        bump(x: u64)
        where @inc(x) > 0 : throw 1
        => []
    }
    macro inc(x: u64) -> u64 = { x + 1 }
    m_n: u64 { in constructor() => 0 in bump(x) => @inc(x) }
}
"#;

#[test]
fn lean_macro_in_where_lake_build() {
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() != Ok("1") {
        eprintln!("skipping lake-build smoke (set CAMBRIAN_TEST_LEAN_BUILD=1 to enable)");
        return;
    }
    if !lake_available() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }

    let dir = tempdir("smoke-macro-where");
    let cam_path = dir.join("gate.cam");
    let out_dir = dir.join("out");
    std::fs::write(&cam_path, MACRO_IN_WHERE).unwrap();

    let tp = Command::new(transpiler_bin())
        .arg(&cam_path)
        .arg("-o")
        .arg(&out_dir)
        .arg("--target")
        .arg("lean")
        .output()
        .expect("invoke transpiler");
    assert!(
        tp.status.success(),
        "transpile failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&tp.stdout),
        String::from_utf8_lossy(&tp.stderr),
    );

    let lake = Command::new("lake")
        .arg("build")
        .current_dir(&out_dir)
        .output()
        .expect("invoke lake");
    assert!(
        lake.status.success(),
        "lake build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&lake.stdout),
        String::from_utf8_lossy(&lake.stderr),
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Spec-prefix binder order (T-LEAN-EX-010 / B-26 class)
// ---------------------------------------------------------------------------

/// Fail if `ctx` / introducing `w` is used before its binder.
fn prefix_binder_violations(spec: &str) -> Vec<String> {
    let mut violations = Vec::new();
    let mut ctx_bound = false;
    for (i, line) in spec.lines().enumerate() {
        let t = line.trim();
        if t.starts_with("/--") || t.starts_with("/-") {
            continue;
        }
        if t.starts_with("theorem ") || t.starts_with("abbrev ") {
            ctx_bound = false;
        }
        let is_ctx_let = t.starts_with("let ctx : Cambrian.MsgCtx") || t.starts_with("let ctx :=");
        let is_w_intro = t.starts_with("let w : Cambrian.Generated.World");
        if is_w_intro {
            if let Some(rhs) = t.splitn(2, ":=").nth(1) {
                if rhs.contains("w.block") || rhs.contains("w.balances") {
                    violations.push(format!("L{}: introducing let w RHS mentions w: {t}", i + 1));
                }
            }
        }
        if !is_ctx_let && !ctx_bound && t.contains("ctx.") {
            violations.push(format!("L{}: ctx. used before let ctx: {t}", i + 1));
        }
        if is_ctx_let {
            ctx_bound = true;
        }
    }
    violations
}

fn assert_clean_prefix(spec: &str) {
    let v = prefix_binder_violations(spec);
    assert!(
        v.is_empty(),
        "prefix use-before-bind:\n{}\n\n{spec}",
        v.join("\n")
    );
}

fn spec_of(source: &str, entity: &str, file: &str) -> String {
    extras_for(source, entity)
        .get(file)
        .cloned()
        .unwrap_or_else(|| panic!("{file} missing"))
}

const PREFIX_PROBE: &str = r#"
entity SenderProbe {
    routes {
        ping() => []
    }
    m_owner: address {
        in ping() => m_owner
    }
    m_stamp: u64 {
        in ping() => m_stamp
    }
    m_paid: U256 {
        in ping() => m_paid
    }
}
"#;

#[test]
fn spec_prefix_p1_assume_msg_sender_binds_ctx_first() {
    let src = format!(
        r#"
{PREFIX_PROBE}
property "SENDER" for SenderProbe with {{ * }} {{
    assume msg::sender == m_owner
    call ping()
    expect state {{ m_owner: m_owner }}
}}
"#
    );
    let spec = spec_of(
        &src,
        "SenderProbe",
        "Cambrian/Generated/SenderProbeSpec.lean",
    );
    assert_clean_prefix(&spec);
    let ctx_pos = spec.find("let ctx : Cambrian.MsgCtx").expect("let ctx");
    let use_pos = spec.find("ctx.sender").expect("ctx.sender");
    assert!(
        ctx_pos < use_pos,
        "P1: let ctx must precede assume ctx.sender:\n{spec}"
    );
}

#[test]
fn spec_prefix_p2_forall_sender_seeds_ctx_before_assume() {
    let src = format!(
        r#"
{PREFIX_PROBE}
property "SENDER" for SenderProbe with {{ * }} ctx {{ msg::sender: * }} {{
    assume msg::sender == m_owner
    call ping()
    expect state {{ m_owner: m_owner }}
}}
"#
    );
    let spec = spec_of(
        &src,
        "SenderProbe",
        "Cambrian/Generated/SenderProbeSpec.lean",
    );
    assert_clean_prefix(&spec);
    assert!(
        spec.contains("(sender : Cambrian.Address)"),
        "P2: forall sender:\n{spec}"
    );
    assert!(
        spec.contains("MsgCtx.default with sender := sender"),
        "P2: ctx seeded from forall var:\n{spec}"
    );
    let ctx_pos = spec.find("let ctx : Cambrian.MsgCtx").expect("let ctx");
    let use_pos = spec.find("ctx.sender").expect("ctx.sender");
    assert!(ctx_pos < use_pos, "P2: seeded ctx before premise:\n{spec}");
}

#[test]
fn spec_prefix_p3_assume_msg_value() {
    let src = format!(
        r#"
{PREFIX_PROBE}
property "VALUE" for SenderProbe {{
    assume msg::value > 0
    call ping()
    expect state {{ m_owner: m_owner }}
}}
"#
    );
    let spec = spec_of(
        &src,
        "SenderProbe",
        "Cambrian/Generated/SenderProbeSpec.lean",
    );
    assert_clean_prefix(&spec);
    let ctx_pos = spec.find("let ctx : Cambrian.MsgCtx").expect("let ctx");
    let use_pos = spec.find("ctx.value").expect("ctx.value");
    assert!(ctx_pos < use_pos, "P3: let ctx before ctx.value:\n{spec}");
}

#[test]
fn spec_prefix_p4_assume_sys_now_reads_world_block() {
    let src = format!(
        r#"
{PREFIX_PROBE}
property "NOW" for SenderProbe with {{ * }} {{
    assume sys::now >= 0
    call ping()
    expect state {{ m_owner: m_owner }}
}}
"#
    );
    let spec = spec_of(
        &src,
        "SenderProbe",
        "Cambrian/Generated/SenderProbeSpec.lean",
    );
    assert_clean_prefix(&spec);
    let w_pos = spec
        .find("let w : Cambrian.Generated.World")
        .expect("let w");
    let use_pos = spec.find("w.block.timestamp").expect("w.block.timestamp");
    assert!(w_pos < use_pos, "P4: assume sys::now after let w:\n{spec}");
}

#[test]
fn spec_prefix_p5_init_sys_now_uses_ctx_timestamp() {
    let src = format!(
        r#"
{PREFIX_PROBE}
property "STAMP" for SenderProbe with {{ m_stamp: sys::now, m_owner: * }} {{
    call ping()
    expect state {{ m_stamp: m_stamp }}
}}
"#
    );
    let spec = spec_of(
        &src,
        "SenderProbe",
        "Cambrian/Generated/SenderProbeSpec.lean",
    );
    assert_clean_prefix(&spec);
    // Forall `m_owner` plus pin `m_stamp` share one `{ State.default with … }`
    // line, so `m_stamp` is not the first field after `with`.
    let init_line = spec
        .lines()
        .find(|l| l.contains("m_stamp :="))
        .expect("m_stamp init");
    assert!(
        init_line.contains("ctx.timestamp"),
        "P5: init sys::now via ctx.timestamp:\n{init_line}\n{spec}"
    );
    assert!(
        !init_line.contains("w.block"),
        "P5: must not self-ref w:\n{init_line}"
    );
}

#[test]
fn spec_prefix_t1_test_init_sys_now() {
    let src = format!(
        r#"
{PREFIX_PROBE}
test "stamp" for SenderProbe with {{ m_stamp: sys::now }} {{
    call ping()
    expect state {{ m_stamp: m_stamp }}
}}
"#
    );
    let spec = spec_of(
        &src,
        "SenderProbe",
        "Cambrian/Generated/SenderProbeSpec.lean",
    );
    assert_clean_prefix(&spec);
    let ctx_pos = spec.find("let ctx : Cambrian.MsgCtx").expect("let ctx");
    let w_pos = spec
        .find("let w : Cambrian.Generated.World")
        .expect("let w");
    assert!(ctx_pos < w_pos, "T1: ctx before w:\n{spec}");
    let init_line = spec
        .lines()
        .find(|l| l.contains("with m_stamp :="))
        .expect("m_stamp init");
    assert!(
        init_line.contains("ctx.timestamp") && !init_line.contains("w.block"),
        "T1: {init_line}"
    );
}

#[test]
fn spec_prefix_t2_test_init_msg_sender() {
    let src = format!(
        r#"
{PREFIX_PROBE}
test "owner" for SenderProbe with {{ m_owner: msg::sender }} {{
    call ping()
    expect state {{ m_owner: m_owner }}
}}
"#
    );
    let spec = spec_of(
        &src,
        "SenderProbe",
        "Cambrian/Generated/SenderProbeSpec.lean",
    );
    assert_clean_prefix(&spec);
    let ctx_pos = spec.find("let ctx : Cambrian.MsgCtx").expect("let ctx");
    let w_pos = spec
        .find("let w : Cambrian.Generated.World")
        .expect("let w");
    assert!(ctx_pos < w_pos, "T2: ctx before w:\n{spec}");
    assert!(
        spec.contains("ctx.sender"),
        "T2: init from msg::sender:\n{spec}"
    );
}

#[test]
fn spec_prefix_i1_invariant_clock_mirror() {
    let src = r#"
entity InitNow {
    routes {
        constructor() => []
        tick() => []
    }
    m_stamp: u64 { in constructor() => 0 in tick() => m_stamp + 1 }
}

invariant "stamp starts at now" for InitNow {
    init { m_stamp: sys::now }
    ctx { sys::now: * }
    action tick() { }
    check m_stamp >= 0
}
"#;
    let spec = spec_of(src, "InitNow", "Cambrian/Generated/InitNowSpec.lean");
    assert_clean_prefix(&spec);
    assert!(
        spec.contains("timestamp := ctx_now") || spec.contains("timestamp := now"),
        "I1: ctx must mirror sys::now onto timestamp:\n{spec}"
    );
    let init_line = spec
        .lines()
        .find(|l| l.contains("with m_stamp :="))
        .expect("m_stamp init");
    assert!(
        init_line.contains("ctx.timestamp") && !init_line.contains("w.block"),
        "I1: {init_line}"
    );
}

#[test]
fn spec_prefix_c1_counter_specs_ctx_before_inst() {
    let extras = extras_for(COUNTER_WITH_SPECS, "Counter");
    let spec = extras
        .get("Cambrian/Generated/CounterSpec.lean")
        .expect("CounterSpec");
    assert_clean_prefix(spec);
    let tests = spec.split("namespace Counter.Spec.Tests").nth(1).unwrap();
    let ctx = tests.find("let ctx : Cambrian.MsgCtx").expect("ctx");
    let inst = tests.find("let inst : Counter.Identity").expect("inst");
    assert!(ctx < inst, "C1 tests: ctx before inst:\n{tests}");
}

#[test]
fn spec_prefix_p1_lake_build_optional() {
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() != Ok("1") {
        eprintln!("skipping prefix P1 lake smoke (set CAMBRIAN_TEST_LEAN_BUILD=1)");
        return;
    }
    if !lake_available() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }
    let src = format!(
        r#"
{PREFIX_PROBE}
property "SENDER" for SenderProbe with {{ * }} {{
    assume msg::sender == m_owner
    call ping()
    expect state {{ m_owner: m_owner }}
}}
"#
    );
    let dir = tempdir("smoke-prefix-p1");
    let cam_path = dir.join("probe.cam");
    let out_dir = dir.join("out");
    std::fs::write(&cam_path, src).unwrap();
    let tp = Command::new(transpiler_bin())
        .arg(&cam_path)
        .arg("-o")
        .arg(&out_dir)
        .arg("--target")
        .arg("lean")
        .output()
        .expect("invoke transpiler");
    assert!(
        tp.status.success(),
        "transpile failed:\n{}",
        String::from_utf8_lossy(&tp.stderr)
    );
    let lake = Command::new("lake")
        .arg("build")
        .current_dir(&out_dir)
        .output()
        .expect("invoke lake");
    assert!(
        lake.status.success(),
        "P1 lake build failed:\n{}\n{}",
        String::from_utf8_lossy(&lake.stdout),
        String::from_utf8_lossy(&lake.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lg_f05_prefix_let_before_assume_hypothesis() {
    let src = r#"
entity Tok {
    routes {
        transfer(amount: U256) -> U256 => [
            return(m_total)
        ]
    }
    m_total: U256 {
        in transfer(amount) => m_total - amount
    }
}

property "bounded transfer" (amount: U256) for Tok {
    let supply = 1000
    assume amount > 0 && amount <= supply
    call transfer(amount)
    expect return supply - amount
}
"#;
    let extras = extras_for(src, "Tok");
    let spec = extras
        .get("Cambrian/Generated/TokSpec.lean")
        .expect("TokSpec");
    let props = spec
        .split("namespace Tok.Spec.Properties")
        .nth(1)
        .expect("properties namespace");
    let let_pos = props
        .find("let supply := 1000")
        .expect("prefix let supply");
    let hyp_pos = props
        .find("(amount > 0)")
        .expect("assume hypothesis");
    assert!(
        let_pos < hyp_pos,
        "LG-F05: prefix let must precede assume → hypothesis:\n{props}"
    );
    assert!(
        props.matches("let supply := 1000").count() == 1,
        "prefix let must not be duplicated in body:\n{props}"
    );
}

#[test]
fn lg_f05_hoist_transitive_let_chain_before_assume() {
    let src = r#"
entity Tok {
    routes {
        transfer(amount: U256) -> U256 => [ return(m_total) ]
    }
    m_total: U256 { in transfer(amount) => m_total - amount }
}

property "chained bound" (amount: U256) for Tok {
    let base = 1000
    let cap = base + 1
    assume amount > 0 && amount <= cap
    call transfer(amount)
    expect return cap - amount
}
"#;
    let extras = extras_for(src, "Tok");
    let props = extras
        .get("Cambrian/Generated/TokSpec.lean")
        .expect("TokSpec")
        .split("namespace Tok.Spec.Properties")
        .nth(1)
        .expect("properties namespace");
    let base_pos = props.find("let base := 1000").expect("base prefix");
    let cap_pos = props.find("let cap :=").expect("cap prefix");
    let hyp_pos = props.find("(amount > 0)").expect("assume hyp");
    assert!(
        base_pos < cap_pos && cap_pos < hyp_pos,
        "LG-F05: transitive lets must topo-sort before assume:\n{props}"
    );
    assert_eq!(
        props.matches("let cap :=").count(),
        1,
        "hoisted let must not repeat in body:\n{props}"
    );
}

#[test]
fn lg_f05_hoist_nonleading_let_before_assume() {
    let src = r#"
entity Tok {
    routes {
        init setup() => []
        transfer(amount: U256) -> U256 => [ return(m_total) ]
    }
    m_total: U256 {
        in setup() => 1000
        in transfer(amount) => m_total - amount
    }
}

property "bound after setup call" (amount: U256) for Tok {
    call setup()
    let supply = 1000
    assume amount > 0 && amount <= supply
    call transfer(amount)
    expect return supply - amount
}
"#;
    let extras = extras_for(src, "Tok");
    let props = extras
        .get("Cambrian/Generated/TokSpec.lean")
        .expect("TokSpec")
        .split("namespace Tok.Spec.Properties")
        .nth(1)
        .expect("properties namespace");
    let supply_pos = props.find("let supply := 1000").expect("supply prefix");
    let hyp_pos = props.find("(amount > 0)").expect("assume hyp");
    let setup_pos = props.find("Tok.Routes.setup").expect("setup call in body");
    assert!(
        supply_pos < hyp_pos && hyp_pos < setup_pos,
        "LG-F05: assume-hyp lets hoist before hypotheses; call stays in body:\n{props}"
    );
}

#[test]
fn typed_lit_lean_spec_let_pins_declared_width() {
    let src = r#"
entity Tok {
    routes {
        get() -> u64 => [ return(m_n) ]
    }
    m_n: u64 { in get() => m_n }
}

test "typed cap" for Tok {
    let cap: u64 = 255
    call get()
    expect return cap
}
"#;
    let extras = extras_for(src, "Tok");
    let spec = extras
        .get("Cambrian/Generated/TokSpec.lean")
        .expect("TokSpec");
    assert!(
        spec.contains("let cap := (255 : BitVec 64)"),
        "typed u64 spec let must ascribe BitVec width: {spec}"
    );
}

const EXPECT_PREDICATE_CAM: &str = r#"
entity Counter {
    routes {
        view getCount() -> u64 => [return(m_count)]
        bump() => []
    }
    m_count: u64 { in bump() => m_count + 1 }
}

property "holds" for Counter {
    call bump()
    call getCount()
    expect return != 0
    expect return > 0 && m_count >= 1
    expect m_count + 0 == m_count
}
"#;

#[test]
fn expect_predicate_lake_build_smoke() {
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() != Ok("1") {
        eprintln!("skipping lake-build smoke (set CAMBRIAN_TEST_LEAN_BUILD=1 to enable)");
        return;
    }
    if !lake_available() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }

    let dir = tempdir("expect-predicate");
    let cam_path = dir.join("pred.cam");
    let out_dir = dir.join("out");
    std::fs::write(&cam_path, EXPECT_PREDICATE_CAM).unwrap();

    let tp = Command::new(transpiler_bin())
        .arg(&cam_path)
        .arg("-o")
        .arg(&out_dir)
        .arg("--target")
        .arg("lean")
        .output()
        .expect("invoke transpiler");
    assert!(
        tp.status.success(),
        "transpile failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&tp.stdout),
        String::from_utf8_lossy(&tp.stderr),
    );

    let lake = Command::new("lake")
        .arg("build")
        .current_dir(&out_dir)
        .output()
        .expect("invoke lake");
    assert!(
        lake.status.success(),
        "lake build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&lake.stdout),
        String::from_utf8_lossy(&lake.stderr),
    );
    let _ = std::fs::remove_dir_all(&dir);
}

