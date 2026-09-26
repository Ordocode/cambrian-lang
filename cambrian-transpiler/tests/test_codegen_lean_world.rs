// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! P3 acceptance tests for the Lean target — abstract EVM/blockchain
//! model, world-threaded routes, and same-entity sends.
//!
//! Mirrors the structure of [`test_codegen_lean`] / [`test_codegen_lean_props`]:
//!
//! 1. Library-level: per-program `Cambrian/Generated/World.lean`
//!    shape, per-entity address-derivation defs, retargeted route
//!    entry-points with `(w : World) (inst : Identity) ...`
//!    signatures, send lowering, and theorem prelude shape.
//! 2. L-rule rejections: `L8` / `L9` for unresolvable / failing-route
//!    sends; `L10` informational warning for invariants whose actions
//!    touch sending routes.
//! 3. Optional `lake build` smoke check across the canonical P3
//!    fixtures (Counter / Escrow / PhasedVault / LendingPair plus the
//!    new self-call + multi-instance fixtures), gated on
//!    `CAMBRIAN_TEST_LEAN_BUILD=1`.

use std::path::PathBuf;
use std::process::Command;

use cambrian_transpiler::ast;
use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::ProgramParser;

// ---------------------------------------------------------------------------
// Shared scaffolding
// ---------------------------------------------------------------------------

fn parse(source: &str) -> ast::Program {
    let mut p = ProgramParser::new()
        .parse(source)
        .expect("test fixture must parse");
    ast::normalize_program_types(&mut p);
    p
}

fn extras_for(source: &str, entity: &str) -> std::collections::HashMap<String, String> {
    let backend = LeanBackend::default();
    let program = parse(source);
    backend
        .extra_files(&program, entity)
        .into_iter()
        .collect()
}

fn tempdir(stem: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cambrian-lean-p3-{}-{}",
        stem,
        std::process::id()
    ));
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

/// Transpile one or more `.cam` paths (merged program) and run `lake build`.
fn run_lake_smoke_fixture(name: &str, rel_paths: &[&str]) {
    let cwd = std::env::current_dir().expect("current_dir");
    let mut cmd = Command::new(transpiler_bin());
    for rel in rel_paths {
        let cam = cwd.join(rel);
        assert!(cam.exists(), "{}: fixture missing: {}", name, cam.display());
        cmd.arg(cam);
    }
    let out_dir = tempdir(&format!("smoke-{}", name));
    let tp = cmd
        .arg("-o")
        .arg(&out_dir)
        .arg("--target")
        .arg("lean")
        .output()
        .expect("invoke transpiler");
    assert!(
        tp.status.success(),
        "{}: transpile failed:\nstdout:\n{}\nstderr:\n{}",
        name,
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
        "{}: lake build failed:\nstdout:\n{}\nstderr:\n{}",
        name,
        String::from_utf8_lossy(&lake.stdout),
        String::from_utf8_lossy(&lake.stderr),
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}

const COUNTER_CAM: &str = r#"
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
"#;

/// Multi-instance entity — exercises non-trivial `Identity` salt
/// derivation and the uniform `Identity → State` storage shape.
const PAIR_CAM: &str = r#"
entity Pair {
    identity left: u64
    identity right: u64

    routes {
        init create() => []
        bump(amount: u64) => []
        sum() -> u64 => [
            return(left + right + m_acc)
        ]
    }

    m_acc: u64 {
        in create() => 0
        in bump(amount) => m_acc + amount
    }
}
"#;

/// Self-call fixture exercising both shapes covered by P3.8:
///   * `var x = bumped(n) ~> Ping.address()` (typed self-call), and
///   * `~> recipient with { value: amount }` (raw transfer).
const SELF_CALL_CAM: &str = r#"
entity Ping {
    routes {
        init create() => []
        bumped(n: u64) -> u64 => [
            return(m_count + n)
        ]
        kick(n: u64) => [
            var bumped_value = bumped(n) ~> Ping.address();
        ]
        payout(recipient: address, amount: u128) => [
            ~> recipient with { value: amount }
        ]
    }
    m_count: u64 {
        in create() => 1
        in bumped(n) => m_count + n
    }
}
"#;

// ---------------------------------------------------------------------------
// P3.1 — Prelude EVM surface
// ---------------------------------------------------------------------------

#[test]
fn lean_p3_prelude_exposes_world_state_and_create2() {
    let extras = extras_for(COUNTER_CAM, "Counter");
    let core = extras
        .get("Cambrian/Core.lean")
        .expect("Cambrian/Core.lean must be in extra_files");
    let evm = extras
        .get("Cambrian/Evm.lean")
        .expect("Cambrian/Evm.lean must be in extra_files");

    assert!(
        evm.contains("structure BlockEnv"),
        "Evm must declare BlockEnv (P3.1):\n{}",
        evm
    );
    assert!(
        evm.contains("structure WorldState"),
        "Evm must declare the polymorphic WorldState carrier (P3.1):\n{}",
        evm
    );
    assert!(
        evm.contains("WorldState.transfer"),
        "Evm must expose WorldState.transfer (P3.1):\n{}",
        evm
    );
    assert!(
        evm.contains("inductive TransferError"),
        "Evm must declare TransferError variants (P3.1):\n{}",
        evm
    );
    assert!(
        evm.contains("create2Address"),
        "Evm must declare create2Address derivation (P3.1):\n{}",
        evm
    );
    assert!(
        evm.contains("def call") && evm.contains("CallParams"),
        "Evm must expose WorldState.call + CallParams (P4a):\n{}",
        evm
    );
    assert!(
        core.contains("AddressMap"),
        "Core must declare AddressMap (P4b):\n{}",
        core
    );
    assert!(
        core.contains("0.0.8-proof"),
        "Cambrian.Version must be bumped for the big-projects audit (P4d):\n{}",
        core
    );
}

// ---------------------------------------------------------------------------
// P3.3 — per-program World module shape
// ---------------------------------------------------------------------------

#[test]
fn lean_p3_world_module_singleton_shape() {
    let extras = extras_for(COUNTER_CAM, "Counter");
    let world = extras
        .get("Cambrian/Generated/World.lean")
        .expect("World.lean must be in extra_files");

    assert!(
        world.contains("structure Storage"),
        "World must declare a Storage record (P3.3):\n{}",
        world
    );
    assert!(
        world.contains("counter : Counter.Identity → Counter.State"),
        "Storage must have one slot per entity, keyed by Identity (P3.3):\n{}",
        world
    );
    assert!(
        world.contains("abbrev World := Cambrian.WorldState Storage Event"),
        "World must be the per-program specialisation of WorldState (P3.3):\n{}",
        world
    );
    assert!(
        world.contains("inductive Event"),
        "World module must declare the ghost event-log `Event` type:\n{}",
        world
    );
    assert!(
        world.contains("def World.default"),
        "World.default factory must be emitted (P3.3):\n{}",
        world
    );
    assert!(
        world.contains("def World.counter"),
        "World.counter projection must be emitted (P3.3):\n{}",
        world
    );
    assert!(
        world.contains("def World.withCounter"),
        "World.withCounter setter must be emitted (P3.3):\n{}",
        world
    );
}

#[test]
fn lean_p3_world_module_multi_instance_shape() {
    let extras = extras_for(PAIR_CAM, "Pair");
    let world = extras
        .get("Cambrian/Generated/World.lean")
        .expect("World.lean must be in extra_files");

    // Identity-keyed storage works uniformly: the per-entity slot is
    // `Identity → State` whether `Identity` has 0 or N fields.
    assert!(
        world.contains("pair : Pair.Identity → Pair.State"),
        "Storage slot key shape is uniform across singleton / multi-instance entities (P3.3):\n{}",
        world
    );
    assert!(
        world.contains("def World.withPair"),
        "Per-entity setter naming follows PascalCase entity name (P3.3):\n{}",
        world
    );
}

// ---------------------------------------------------------------------------
// P3.2 + P3.4 — Identity DecidableEq + per-entity address derivation
// ---------------------------------------------------------------------------

#[test]
fn lean_p3_identity_derives_decidable_eq_singleton() {
    let backend = LeanBackend::default();
    let program = parse(COUNTER_CAM);
    let entity_lean = backend.gen_program(&program);
    assert!(
        entity_lean.contains("deriving Repr, DecidableEq"),
        "Singleton Identity must derive DecidableEq (P3.2):\n{}",
        entity_lean
    );
}

#[test]
fn lean_p3_identity_derives_decidable_eq_multi_instance() {
    let backend = LeanBackend::default();
    let program = parse(PAIR_CAM);
    let entity_lean = backend.gen_program(&program);
    assert!(
        entity_lean.contains("structure Identity")
            && entity_lean.contains("deriving Repr, DecidableEq"),
        "Multi-instance Identity must also derive DecidableEq (P3.2):\n{}",
        entity_lean
    );
}

#[test]
fn lean_p3_address_singleton_signature() {
    let backend = LeanBackend::default();
    let program = parse(COUNTER_CAM);
    let entity_lean = backend.gen_program(&program);
    assert!(
        entity_lean.contains("def address (id : Identity) : Cambrian.Address"),
        "Per-entity address derivation must take an Identity (uniform, even for singletons) (P3.4):\n{}",
        entity_lean
    );
    assert!(
        entity_lean.contains("Cambrian.create2Address"),
        "Address derivation must thread through create2Address (P3.4):\n{}",
        entity_lean
    );
}

#[test]
fn lean_p3_address_multi_instance_salt_uses_identity_fields() {
    let backend = LeanBackend::default();
    let program = parse(PAIR_CAM);
    let entity_lean = backend.gen_program(&program);
    // Multi-field identity → salt mixes both via Cambrian.castWidth /
    // xor; checking that both field names appear is sufficient.
    assert!(
        entity_lean.contains("id.left") && entity_lean.contains("id.right"),
        "Multi-instance address salt must reference every identity field (P3.4):\n{}",
        entity_lean
    );
}

// ---------------------------------------------------------------------------
// P3.5 — Retargeted route entry-points
// ---------------------------------------------------------------------------

#[test]
fn lean_p3_route_entrypoints_take_world_and_identity() {
    let extras = extras_for(COUNTER_CAM, "Counter");
    let routes = extras
        .get("Cambrian/Generated/CounterRoutes.lean")
        .expect("CounterRoutes.lean must be in extra_files");

    assert!(
        routes.contains("def increment (w : Cambrian.Generated.World) (inst : Counter.Identity) (ctx : Cambrian.MsgCtx) (amount : BitVec 64) : Cambrian.Generated.World"),
        "increment entry-point lifted onto World × Identity (P3.5):\n{}",
        routes
    );
    assert!(
        routes.contains("def getCount (w : Cambrian.Generated.World) (inst : Counter.Identity) (ctx : Cambrian.MsgCtx) : Cambrian.Generated.World × BitVec 64"),
        "View entry-point returns World × T (P3.5):\n{}",
        routes
    );
}

// ---------------------------------------------------------------------------
// Design B — per-route `Pre` + `isOk_iff` characterization
// ---------------------------------------------------------------------------

/// A flat fail-mode route (`deposit`, single `where`) and a phased
/// fail-mode route (`process`) on one entity, so a single Routes module
/// exercises both the guard-level and the abstract `Pre` shapes.
const PRE_FIXTURE: &str = r#"
entity Acct {
    routes {
        deposit(amount: u128)
            where amount > 0 : throw 101
        => []
        process(amount: u128)
        => [
            check where (amount > 0) : throw 401 : []
            apply where (amount <= m_cap) : throw 402 : []
        ]
    }
    m_cap: u128 {}
    m_bal: u128 {
        in deposit(amount) => m_bal + amount
    }
    m_total: u128 {
        in process(amount) => apply: m_total + amount
    }
}
"#;

#[test]
fn lean_design_b_emits_pre_and_isok_iff() {
    let extras = extras_for(PRE_FIXTURE, "Acct");
    let routes = extras
        .get("Cambrian/Generated/AcctRoutes.lean")
        .expect("AcctRoutes.lean must be in extra_files");

    // Flat route: guard-level `Pre` (`_pre_<i> … = true`) + a `simp`-proved
    // `isOk_iff`.
    assert!(
        routes.contains("def deposit.Pre (w : Cambrian.Generated.World) (inst : Acct.Identity) (ctx : Cambrian.MsgCtx) (amount : BitVec 128) : Prop :="),
        "deposit.Pre must be emitted with the route's binder list:\n{}",
        routes
    );
    assert!(
        routes.contains("Acct.Local.deposit_pre_0 s ctx inst amount = true"),
        "deposit.Pre must conjoin its Local `_pre_<i>` guard in `= true` form:\n{}",
        routes
    );
    assert!(
        routes.contains("theorem deposit_isOk_iff")
            && routes.contains(
                "simp [cambrian_route_simp, Bool.and_eq_true, Bool.or_eq_true, decide_eq_true_eq]"
            ),
        "deposit_isOk_iff must be discharged by the route simp set + bridge lemmas:\n{}",
        routes
    );
    assert!(
        routes.contains("Decidable (deposit.Pre w inst ctx amount)"),
        "deposit.Pre must carry a Decidable instance:\n{}",
        routes
    );

    // Phased route: abstract `Pre := isOk = true` fallback with `Iff.rfl`.
    assert!(
        routes.contains("def process.Pre")
            && routes.contains("(process w inst ctx amount).isOk = true"),
        "phased process.Pre must use the abstract `isOk = true` fallback:\n{}",
        routes
    );
    assert!(
        routes.contains("theorem process_isOk_iff")
            && routes.contains("↔ process.Pre w inst ctx amount := Iff.rfl"),
        "phased process_isOk_iff must be Iff.rfl:\n{}",
        routes
    );
    assert!(
        routes.contains("Decidable (process.Pre w inst ctx amount)"),
        "process.Pre must carry a Decidable instance:\n{}",
        routes
    );
}

// ---------------------------------------------------------------------------
// P3.7 — Spec theorems thread World + instance
// ---------------------------------------------------------------------------

const COUNTER_WITH_TEST: &str = r#"
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

test "increment adds to count" for Counter with { m_count: 5 } {
    call increment(3)
    expect state { m_count: 8 }
}
"#;

#[test]
fn lean_p3_test_theorem_prelude_constructs_world() {
    let extras = extras_for(COUNTER_WITH_TEST, "Counter");
    let spec = extras
        .get("Cambrian/Generated/CounterSpec.lean")
        .expect("CounterSpec.lean must be in extra_files");

    assert!(
        spec.contains("let inst : Counter.Identity"),
        "P3.7: theorem prelude must bind an Identity:\n{}",
        spec
    );
    assert!(
        spec.contains("let w : Cambrian.Generated.World"),
        "P3.7: theorem prelude must bind a World:\n{}",
        spec
    );
    assert!(
        spec.contains("Cambrian.Generated.World.withCounter Cambrian.Generated.World.default inst"),
        "P3.7: initial World built via per-entity setter on default:\n{}",
        spec
    );
    assert!(
        spec.contains("Counter.Routes.increment w inst ctx"),
        "P3.7: route calls thread (w, inst) explicitly:\n{}",
        spec
    );
    assert!(
        spec.contains("(Cambrian.Generated.World.counter w inst)"),
        "P3.7: expect-state lens projects via World.<entity> w inst:\n{}",
        spec
    );
}

#[test]
fn lean_p3_invariant_step_runs_against_world() {
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

invariant "count never exceeds bound" for Counter {
    init { m_count: 0 }
    action increment(amount: u64) { bound amount in 0..1000 }
    action reset() { }
    check m_count <= 1000000000
}
"#,
        "Counter",
    );
    let spec = extras
        .get("Cambrian/Generated/CounterSpec.lean")
        .expect("CounterSpec.lean must be in extra_files");

    assert!(
        spec.contains("def step (w : Cambrian.Generated.World) (inst : Counter.Identity)"),
        "P3.7: invariant `step` takes (w, inst, ctx):\n{}",
        spec
    );
    assert!(
        spec.contains("def runTrace (w₀ : Cambrian.Generated.World) (inst : Counter.Identity)"),
        "P3.7: invariant `runTrace` folds over (w, inst, ctx):\n{}",
        spec
    );
    assert!(
        spec.contains("Counter.Routes.increment w inst ctx"),
        "P3.7: step dispatch routes through World-keyed entry-points:\n{}",
        spec
    );
}

// ---------------------------------------------------------------------------
// Throwing invariants — executable `match runTrace` shape (Option A)
// ---------------------------------------------------------------------------

/// A throwing route makes the invariant a fail-surface. Its theorem must
/// *compute* the final world with a `match` (only `trace` is quantified),
/// not `∀ w_final, runTrace … = .ok w_final → …` (which forced sampling a
/// non-`Sampleable` `World`). A `view`-route call in a `check` must lower to
/// the route's returned value (`… .snd`), not a bare — auto-bound — name.
const THROWING_INVARIANT_CAM: &str = r#"
entity Vault {
    routes {
        withdraw(amount: u64) where (amount <= m_balance) : throw 1 => []
        balance() -> u64 => [
            return(m_balance)
        ]
    }
    m_balance: u64 {
        in withdraw(amount) => m_balance - amount
    }
}

invariant "balance readable" for Vault {
    init { m_balance: 100 }
    action withdraw(amount: u64) { bound amount in 0..50 }
    check balance() == m_balance
}
"#;

#[test]
fn lean_invariant_throwing_uses_executable_match_shape() {
    let extras = extras_for(THROWING_INVARIANT_CAM, "Vault");
    let spec = extras
        .get("Cambrian/Generated/VaultSpec.lean")
        .expect("VaultSpec.lean must be in extra_files");

    // Executable shape: compute the world via okImplies, quantify only over `trace`.
    assert!(
        (spec.contains("Cambrian.RouteResult.okImplies")
            || spec.contains("RouteResult.okImplies"))
            && spec.contains("runTrace w inst ctx trace"),
        "throwing invariant must use okImplies(runTrace …) (PN-104):\n{spec}",
    );
    // The old world-quantified shape must be gone (nothing to sample a World).
    assert!(
        !spec.contains("∀ (w_final"),
        "throwing invariant must not quantify over a final `World`:\n{spec}",
    );
}

#[test]
fn lean_invariant_view_call_in_check_lowers_to_route_return() {
    let extras = extras_for(THROWING_INVARIANT_CAM, "Vault");
    let spec = extras
        .get("Cambrian/Generated/VaultSpec.lean")
        .expect("VaultSpec.lean must be in extra_files");

    // A `view`-route call in a `check` reads the route's returned value.
    assert!(
        spec.contains("(Vault.Routes.balance w inst ctx).snd"),
        "view-route call in a check must lower to `(…Routes.<name> w inst ctx).snd`:\n{spec}",
    );
    // Regression guard: it must not fall through to a bare (auto-bound) name.
    assert!(
        !spec.contains("(balance == "),
        "view call must not be emitted as a bare identifier:\n{spec}",
    );
}

/// Multi-entity invariant: same executable-`match` reshape as the
/// single-entity case, but the theorem threads *both* instance identities
/// (`inst_v`, `inst_t`) into `runTrace` and quantifies only over `trace`.
/// Qualified check refs (`v.m_balance` / `t.m_total`) read the per-instance
/// slot out of the shared world (`(w.storage.<field> inst_…).<member>`), and
/// the `Action` inductive carries one constructor per `<inst>.<route>`.
const MULTI_INVARIANT_CAM: &str = r#"
entity Vault {
    routes {
        deposit(amount: u64) where (amount > 0) : throw 1 => []
    }
    m_balance: u64 { in deposit(amount) => m_balance + amount }
}

entity Treasury {
    routes { credit(amount: u64) => [] }
    m_total: u64 { in credit(amount) => m_total + amount }
}

invariant "sums stay nonneg" for { v: Vault, t: Treasury } {
    init v { m_balance: 0 }
    init t { m_total: 0 }
    action v.deposit(amount: u64) { bound amount in 0..1000 }
    action t.credit(amount: u64) { bound amount in 0..1000 }
    check v.m_balance >= 0
    check t.m_total >= 0
}
"#;

#[test]
fn lean_multi_invariant_uses_executable_match_shape() {
    let extras = extras_for(MULTI_INVARIANT_CAM, "Vault");
    let spec = extras
        .get("Cambrian/Generated/VaultSpec.lean")
        .expect("VaultSpec.lean must be in extra_files");

    // Executable shape threading both instances; only `trace` is quantified.
    assert!(
        (spec.contains("Cambrian.RouteResult.okImplies")
            || spec.contains("RouteResult.okImplies"))
            && spec.contains("runTrace w ctx inst_v inst_t trace"),
        "multi-entity invariant must use okImplies(runTrace …) (PN-104):\n{spec}",
    );
    assert!(
        !spec.contains("∀ (w_final"),
        "multi-entity invariant must not quantify over a final `World`:\n{spec}",
    );
    // Qualified check refs read the per-instance slot from the shared world.
    assert!(
        spec.contains("(w.storage.vault inst_v).m_balance")
            && spec.contains("(w.storage.treasury inst_t).m_total"),
        "qualified check refs must read `(w.storage.<field> inst_…).<member>`:\n{spec}",
    );
    // One `Action` constructor per `<inst>.<route>`, both sampleable
    // (single-file mode keeps numerics as `BitVec`).
    assert!(
        spec.contains("| v_deposit (amount : BitVec 64)")
            && spec.contains("| t_credit (amount : BitVec 64)"),
        "multi-entity `Action` must carry one constructor per `<inst>.<route>`:\n{spec}",
    );
}

// ---------------------------------------------------------------------------
// P3.8 — Same-entity sends and raw transfers
// ---------------------------------------------------------------------------

#[test]
fn lean_p3_send_self_call_lowers_to_routes_call() {
    let extras = extras_for(SELF_CALL_CAM, "Ping");
    let routes = extras
        .get("Cambrian/Generated/PingRoutes.lean")
        .expect("PingRoutes.lean must be in extra_files");

    assert!(
        routes.contains("Ping.Routes.bumped w id' ctx'"),
        "P3.8: `var x = bumped(n) ~> Ping.address()` must lower to a typed self-call:\n{}",
        routes
    );
    assert!(
        routes.contains("let id' : Ping.Identity := {}"),
        "P3.8: singleton self-target builds the empty Identity:\n{}",
        routes
    );
    assert!(
        routes.contains("ctx with sender := Ping.address inst"),
        "P3.8: self-call must swap ctx.sender to the contract's own address:\n{}",
        routes
    );
}

#[test]
fn lean_p3_raw_transfer_lowers_to_worldstate_transfer() {
    let extras = extras_for(SELF_CALL_CAM, "Ping");
    let routes = extras
        .get("Cambrian/Generated/PingRoutes.lean")
        .expect("PingRoutes.lean must be in extra_files");

    assert!(
        routes.contains("Cambrian.WorldState.transfer w (Ping.address inst)"),
        "P3.8: `~> recipient with {{ value: V }}` must lower to WorldState.transfer:\n{}",
        routes
    );
    assert!(
        routes.contains("zeroExtend 256")
            || routes.contains("castWidth 256")
            || routes.contains("BitVec.ofNat 256"),
        "P3.8: value argument must be widened to U256 (BitVec 256):\n{}",
        routes
    );
}

// ---------------------------------------------------------------------------
// P3.9 — L8 / L9 / L10 validator rejections
// ---------------------------------------------------------------------------

#[test]
fn lean_p4_l8_allows_cross_entity_typed_send() {
    use cambrian_transpiler::validate::{check_lean_target_compat, Severity};
    let prog = parse(
        r#"
entity Treasury {
    routes {
        constructor() => []
        credit(amount: u64) => []
    }
    m_total: u64 {
        in constructor() => 0
        in credit(amount) => m_total + amount
    }
}

entity Vault {
    routes {
        constructor() => []
        deposit(amount: u64) => [
            credit(amount) ~> Treasury.address()
        ]
    }
    m_balance: u64 {
        in constructor() => 0
    }
}
"#,
    );
    let diags = check_lean_target_compat(&prog);
    assert!(
        !diags
            .iter()
            .any(|d| d.code == "L8" && matches!(d.severity, Severity::Error)),
        "L8 must allow cross-entity typed send to in-program entity after P4: {:?}",
        diags
    );
}

#[test]
fn lean_p3_l9_rejects_capture_from_failing_route_without_rescue() {
    use cambrian_transpiler::validate::{check_lean_target_compat, Severity};
    let prog = parse(
        r#"
entity Caller {
    routes {
        init create() => []
        risky() -> u64
            where m_v == 0 : throw 7
        => [ return(m_v) ]
        bad_capture() => [
            var y = risky() ~> Caller.address();
        ]
    }
    m_v: u64 { in create() => 0 }
}
"#,
    );
    let diags = check_lean_target_compat(&prog);
    assert!(
        diags
            .iter()
            .any(|d| d.code == "L9" && matches!(d.severity, Severity::Error)),
        "expected an L9 error for failing-route capture without rescue: {:?}",
        diags
    );
}

#[test]
fn lean_p3_l10_warns_on_invariants_touching_sending_routes() {
    use cambrian_transpiler::validate::{check_lean_target_compat, Severity};
    let prog = parse(
        r#"
entity Ping {
    routes {
        init create() => []
        bumped(n: u64) -> u64 => [
            return(m_count + n)
        ]
        kick(n: u64) => [
            var bumped_value = bumped(n) ~> Ping.address();
        ]
    }
    m_count: u64 {
        in create() => 1
        in bumped(n) => m_count + n
    }
}

invariant "kick-bounded" for Ping {
    init { m_count: 0 }
    action kick(n: u64) { bound n in 0..100 }
    check m_count <= 1000000
}
"#,
    );
    let diags = check_lean_target_compat(&prog);
    assert!(
        diags
            .iter()
            .any(|d| d.code == "L10" && matches!(d.severity, Severity::Warning)),
        "expected an L10 warning for an invariant whose action routes contain sends: {:?}",
        diags
    );
}

#[test]
fn lean_p3_l8_allows_same_entity_self_call() {
    use cambrian_transpiler::validate::check_lean_target_compat;
    let prog = parse(SELF_CALL_CAM);
    let diags = check_lean_target_compat(&prog);
    assert!(
        !diags.iter().any(|d| d.code == "L8"),
        "L8 should not fire for same-entity self-calls (P3 happy path): {:?}",
        diags
    );
}

// ---------------------------------------------------------------------------
// Layer 3 — opt-in `lake build` smoke across the P3 fixture set
// ---------------------------------------------------------------------------

#[test]
fn lean_p3_lake_build_smoke() {
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() != Ok("1") {
        eprintln!(
            "skipping lake-build smoke (set CAMBRIAN_TEST_LEAN_BUILD=1 to enable)"
        );
        return;
    }
    if !lake_available() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }

    let fixtures: &[(&str, &[&str])] = &[
        // P0–P3 exit fixtures
        ("counter", &["../contracts/counter.cam"]),
        ("predictable", &["../contracts/escrow.cam"]),
        ("phased_vault", &["../contracts/phased_vault.cam"]),
        ("phased_two_member", &["tests/fixtures/phased_two_member.cam"]),
        ("self_call", &["../contracts/lean_self_call.cam"]),
        // route-body `call` → synchronous same-instance self-invocation
        // (callees emitted before callers).
        ("call", &["../contracts/lean_call.cam"]),
        // send-bearing route with a trailing `return(...)` → the payload
        // must survive into the `(w, <payload>)` tail.
        ("send_return", &["../contracts/lean_send_return.cam"]),
        // effects nested inside `if`-actions in world-threaded routes →
        // each branch threads `w` (send/transfer fail-propagates via Except).
        ("conditional", &["../contracts/lean_conditional.cam"]),
        ("pair", &["../contracts/lean_pair.cam"]),
        // P4a
        ("deploy", &["../contracts/lean_deploy.cam"]),
        ("det_deploy", &["../contracts/det_guardian.cam", "../contracts/det_locker.cam"]),
        ("cross_send", &["../contracts/lean_cross_send.cam"]),
        // P4b
        ("map", &["../contracts/lean_map.cam"]),
        // P4c — HashMap ops + iteration parity
        ("keys_evm", &["../contracts/keys_evm.cam"]),
        ("registry", &["../contracts/registry.cam"]),
        (
            "escrow_two_vaults",
            &[
                "../contracts/escrow_two_vaults.cam",
                "../contracts/escrow_two_vaults.invariant.cam",
            ],
        ),
        // P4c+ — fixtures unblocked by the route / cast / iter fixes
        // collected while auditing every `.cam` against the Lean
        // target (see commit notes). The set is intentionally broad
        // so the next regression on these paths is caught here
        // rather than via the user-facing audit script.
        ("airdrop_evm", &["../contracts/airdrop_evm.cam"]),
        ("fold_evm", &["../contracts/fold_evm.cam"]),
        ("erc20_errors_evm", &["../contracts/erc20_errors_evm.cam"]),
        // erc20_events_evm exercises the ghost event log (`emit` →
        // `Cambrian.WorldState.emit` + per-program `Event` inductive).
        ("erc20_events_evm", &["../contracts/erc20_events_evm.cam"]),
        ("eth_vault_evm", &["../contracts/eth_vault_evm.cam"]),
        ("staking", &["../contracts/staking.cam"]),
        ("voting", &["../contracts/voting.cam"]),
        ("wallet", &["../contracts/wallet.cam"]),
        ("payment_channel", &["../contracts/payment_channel.cam"]),
        ("phased_predictable", &["../contracts/phased_predictable.cam"]),
        ("enum_data", &["../contracts/enum_data.cam"]),
        ("det_reentrancy_order", &["../contracts/det_reentrancy_order.cam"]),
        ("invariant_lending", &[
            "../contracts/invariant_lending.cam",
            "../contracts/invariant_lending.invariant.cam",
        ]),
        ("counter_fuzz", &[
            "../contracts/counter.cam",
            "../contracts/counter.fuzz.cam",
        ]),
        ("counter_invariant", &[
            "../contracts/counter.cam",
            "../contracts/counter.invariant.cam",
        ]),
    ];

    for (name, paths) in fixtures {
        run_lake_smoke_fixture(name, paths);
    }
}

/// Regression for the error-condition reflection layer: transpile the
/// combined `Bank` (unphased) + `Gate` (from-guarded) + `Vault` (phased)
/// fixture, then check the committed `lean_reflection_proofs.lean` against
/// it. The proofs reflect `(route …).isOk` into the codegen-emitted
/// `_pre_<i>` guards via the `cambrian_route_simp` / `cambrian_pre_simp`
/// simp sets and the `Cambrian.RouteResult` plumbing lemmas. The fixture's
/// concrete `test` blocks additionally exercise the auto-discharged proof
/// ladder (member/except/bitvec simp sets); the build must close them all
/// without falling back to `sorry`. Gated on the same
/// `CAMBRIAN_TEST_LEAN_BUILD=1` env var as the other Lean smoke checks.
#[test]
fn lean_reflection_proofs_build() {
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() != Ok("1") {
        eprintln!(
            "skipping reflection-proofs build (set CAMBRIAN_TEST_LEAN_BUILD=1 to enable)"
        );
        return;
    }
    if !lake_available() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }

    let cwd = std::env::current_dir().expect("current_dir");
    let cam = cwd.join("tests/fixtures/lean_reflection.cam");
    let proofs = cwd.join("tests/fixtures/lean_reflection_proofs.lean");
    assert!(cam.exists(), "fixture missing: {}", cam.display());
    assert!(proofs.exists(), "proof file missing: {}", proofs.display());

    let out_dir = tempdir("reflection-proofs");
    let tp = Command::new(transpiler_bin())
        .arg(&cam)
        .arg("-o")
        .arg(&out_dir)
        .arg("--target")
        .arg("lean")
        .output()
        .expect("invoke transpiler");
    assert!(
        tp.status.success(),
        "reflection: transpile failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&tp.stdout),
        String::from_utf8_lossy(&tp.stderr),
    );

    // Build the generated library so the proof file's imports resolve.
    let lake = Command::new("lake")
        .arg("build")
        .current_dir(&out_dir)
        .output()
        .expect("invoke lake");
    assert!(
        lake.status.success(),
        "reflection: lake build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&lake.stdout),
        String::from_utf8_lossy(&lake.stderr),
    );

    // The fixture's concrete `test` blocks must be auto-discharged by the
    // reflection proof ladder — a fresh build of the generated spec files
    // must not emit any `declaration uses 'sorry'` warning. (The fixture
    // has no `property`/`invariant` stubs, so the only possible source is
    // a `test` proof falling through to the `sorry` fallback.)
    let build_log = format!(
        "{}{}",
        String::from_utf8_lossy(&lake.stdout),
        String::from_utf8_lossy(&lake.stderr),
    );
    assert!(
        !build_log.contains("uses 'sorry'"),
        "reflection: a concrete `test` fell through to `sorry` (proof codegen regression):\n{}",
        build_log,
    );

    let proof_dst = out_dir.join("Proofs.lean");
    std::fs::copy(&proofs, &proof_dst).expect("copy proof file");

    // Type-check the proofs against the freshly built generated project.
    let check = Command::new("lake")
        .arg("env")
        .arg("lean")
        .arg(&proof_dst)
        .current_dir(&out_dir)
        .output()
        .expect("invoke lake env lean");
    assert!(
        check.status.success(),
        "reflection: proof check failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr),
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}

/// Transpile a `project.lean.yaml` (multi-entity project mode) and
/// run `lake build` in the configured output dir. Mirrors
/// [`run_lake_smoke_fixture`] but goes through `--project` instead of
/// listing individual `.cam` paths, so it exercises the same surface
/// the audit ran against (`examples/governor/project.lean.yaml`,
/// `examples/uniswap-v2/project.lean.yaml`).
fn run_lake_smoke_project(name: &str, yaml_rel: &str) {
    let cwd = std::env::current_dir().expect("current_dir");
    let yaml = cwd.join(yaml_rel);
    assert!(yaml.exists(), "{}: project yaml missing: {}", name, yaml.display());

    // Wipe any prior `build-lean/` so we're testing a from-scratch
    // transpile. The project yaml writes into a fixed `output_dir`
    // relative to its parent dir.
    let project_dir = yaml.parent().expect("yaml has parent");
    let out_dir = project_dir.join("build-lean");
    let _ = std::fs::remove_dir_all(&out_dir);

    let tp = Command::new(transpiler_bin())
        .arg("--project")
        .arg(&yaml)
        .output()
        .expect("invoke transpiler");
    assert!(
        tp.status.success(),
        "{}: transpile failed:\nstdout:\n{}\nstderr:\n{}",
        name,
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
        "{}: lake build failed:\nstdout:\n{}\nstderr:\n{}",
        name,
        String::from_utf8_lossy(&lake.stdout),
        String::from_utf8_lossy(&lake.stderr),
    );
}

/// Smoke test pinning the `examples/governor` and `examples/uniswap-v2`
/// audit results: both multi-entity projects transpile to Lean and
/// `lake build` from scratch. Gated on the same `CAMBRIAN_TEST_LEAN_BUILD=1`
/// env var as [`lean_p3_lake_build_smoke`] so the default `cargo test`
/// run stays fast.
#[test]
fn lean_big_projects_lake_build_smoke() {
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() != Ok("1") {
        eprintln!(
            "skipping big-projects lake-build smoke (set CAMBRIAN_TEST_LEAN_BUILD=1 to enable)"
        );
        return;
    }
    if !lake_available() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }

    let projects: &[(&str, &str)] = &[
        ("governor", "../examples/governor/project.lean.yaml"),
        ("uniswap-v2", "../examples/uniswap-v2/project.lean.yaml"),
    ];

    for (name, yaml) in projects {
        run_lake_smoke_project(name, yaml);
    }
}

// ---------------------------------------------------------------------------
// Pre-commit snapshots — outbound effects read the state a phase began with
//
// A phase is atomic: its member transforms and its effects observe the state
// as it stood when the phase began. The EVM backend captures every member
// read an effect performs against same-phase-committed state into
// `_pre_<member>_<i>` locals ahead of the `let s := { s with … }` commit
// (`collect_precommit_snapshots`); these tests pin the Lean mirror. The
// motivating divergence: `UniswapV2Pair.burn` pays out
// `mul_div(m_balances[this], bal0, m_total_supply)` in the phase that zeroes
// both, so the Lean model transferred 0 while the EVM contract pays the
// pre-burn share.
// ---------------------------------------------------------------------------

/// Phased route whose `payout` phase both commits `m_shares` / `m_total`
/// and sends an amount computed from them — the burn shape.
const PRECOMMIT_PHASED_CAM: &str = r#"
extern entity Token {
    route transfer(to: address, amount: U256);
}

entity Pool {
    routes {
        constructor(t: Address<Token>) => []
        burnish(to: address) => [
            prepare: []
            payout: [
                transfer(to, m_shares[msg::sender] * m_total) ~> m_token
            ]
        ]
    }

    m_token: Address<Token> { in constructor(t) => t }
    m_total: U256 { in burnish(_) => payout: 0 }
    m_shares: HashMap<address, U256> {
        in burnish(_) => payout: { m_shares.update(msg::sender, 0) }
    }
}
"#;

#[test]
fn lean_phased_send_reads_precommit_snapshots() {
    let extras = extras_for(PRECOMMIT_PHASED_CAM, "Pool");
    let routes = extras
        .get("Cambrian/Generated/PoolRoutes.lean")
        .expect("PoolRoutes.lean must be in extra_files");

    // Both reads are snapshotted ahead of the commit…
    assert!(
        routes.contains("let _pre_m_shares_0 :="),
        "the mapping read must be captured into a pre-commit local:\n{routes}"
    );
    assert!(
        routes.contains("let _pre_m_total_1 := s.m_total"),
        "the scalar read must be captured into a pre-commit local:\n{routes}"
    );

    // …the snapshot lets precede the `{ s with … }` transform commit
    // (checked inside the `burnish` def — other routes commit too)…
    let burnish = &routes[routes.find("def burnish").unwrap()..];
    let snap_at = burnish.find("let _pre_m_shares_0").unwrap();
    let commit_at = burnish
        .find("{ s with")
        .expect("the payout phase must commit its transforms");
    assert!(
        snap_at < commit_at,
        "snapshot lets must be bound before the transform commit:\n{routes}"
    );

    // …and the send reads the locals, not the committed state.
    let send_line = routes
        .lines()
        .find(|l| l.contains("Token.Routes.transfer"))
        .expect("the extern send must be emitted");
    assert!(
        send_line.contains("_pre_m_shares_0") && send_line.contains("_pre_m_total_1"),
        "the send amount must read the pre-commit locals:\n{send_line}\n---\n{routes}"
    );
    assert!(
        !send_line.contains("s.m_total") && !send_line.contains("s.m_shares"),
        "the send must not read the post-commit state:\n{send_line}\n---\n{routes}"
    );
}

/// Unphased world-threaded route: the raw `~>` transfer's `value:` reads a
/// member the route's (unphased) transforms commit.
const PRECOMMIT_UNPHASED_CAM: &str = r#"
entity Escrowish {
    routes {
        constructor() => []
        drain(recipient: address) => [
            ~> recipient with { value: m_balance }
        ]
    }

    m_balance: u128 { in drain(_) => 0 }
}
"#;

#[test]
fn lean_unphased_transfer_value_reads_precommit_snapshot() {
    let extras = extras_for(PRECOMMIT_UNPHASED_CAM, "Escrowish");
    let routes = extras
        .get("Cambrian/Generated/EscrowishRoutes.lean")
        .expect("EscrowishRoutes.lean must be in extra_files");

    assert!(
        routes.contains("let _pre_m_balance_0 := s.m_balance"),
        "the transfer value read must be captured pre-commit:\n{routes}"
    );
    let transfer_line = routes
        .lines()
        .find(|l| l.contains("Cambrian.WorldState.transfer"))
        .expect("the raw transfer must be emitted");
    assert!(
        transfer_line.contains("_pre_m_balance_0") && !transfer_line.contains("s.m_balance"),
        "the transfer must read the pre-commit local:\n{transfer_line}\n---\n{routes}"
    );
}

// ---------------------------------------------------------------------------
// Multi-entity invariants — sibling-instance pins
//
// `init vlt { m_asset: tok }` pins an identity (or state) member at a sibling
// instance declared in the same `for { … }` block: the pin means "that
// instance's address". Lowering the bare ident through `gen_expr` emitted an
// unbound `tok` into the theorem statement (`lake build` failure) — the Lean
// twin of the Foundry-harness sibling-pin bug fixed in `evm_test_codegen`.
// ---------------------------------------------------------------------------

const SIBLING_PIN_CAM: &str = r#"
entity Tok {
    routes {
        constructor() => []
        view balanceOf(who: address) -> U256 => [ return(m_bals[who]) ]
    }
    m_bals: HashMap<address, U256> {}
    identity m_token_id: u64
}

entity Vlt {
    routes {
        constructor() => []
        poke() => [
            var b = balanceOf(sys::address) ~> m_asset;
        ]
    }
    identity m_asset: Address<Tok>
    m_count: U256 { in poke() => m_count + 1 }
}

invariant "count only grows" for { vlt: Vlt, tok: Tok } {
    init tok { m_token_id: 7 }
    init vlt { m_asset: tok }
    action vlt.poke() {
    }
    check vlt.m_count >= 0
}
"#;

#[test]
fn lean_invariant_sibling_pin_renders_sibling_address() {
    let extras = extras_for(SIBLING_PIN_CAM, "Tok");
    let spec = extras
        .values()
        .find(|f| f.contains("count only grows") || f.contains("inst_vlt"))
        .expect("the multi-entity invariant spec must be emitted");

    // The pin renders the sibling's address, not the raw instance name…
    assert!(
        spec.contains("m_asset := (Tok.address inst_tok)"),
        "a sibling-instance pin must lower to the sibling's address:\n{spec}"
    );
    assert!(
        !spec.contains("m_asset := tok"),
        "the raw instance name must not leak into the statement (unbound ident):\n{spec}"
    );

    // …and the referenced binder is declared before its reader, even though
    // `vlt` is declared first in the `for { … }` block.
    let tok_at = spec
        .find("let inst_tok")
        .expect("inst_tok binder must be declared");
    let vlt_at = spec
        .find("let inst_vlt")
        .expect("inst_vlt binder must be declared");
    assert!(
        tok_at < vlt_at,
        "the pinned sibling's binder must precede its reader:\n{spec}"
    );
}

/// Control: a send whose arguments read no same-phase-committed member emits
/// no snapshot lets — untouched routes stay byte-identical.
const PRECOMMIT_CONTROL_CAM: &str = r#"
extern entity Token {
    route transfer(to: address, amount: U256);
}

entity Pool {
    routes {
        constructor(t: Address<Token>) => []
        pay(to: address, amount: U256) => [
            transfer(to, amount) ~> m_token
        ]
        tally(n: U256) => []
    }

    m_token: Address<Token> { in constructor(t) => t }
    m_total: U256 { in tally(n) => m_total + n }
}
"#;

#[test]
fn lean_send_without_committed_overlap_has_no_snapshots() {
    let extras = extras_for(PRECOMMIT_CONTROL_CAM, "Pool");
    let routes = extras
        .get("Cambrian/Generated/PoolRoutes.lean")
        .expect("PoolRoutes.lean must be in extra_files");

    assert!(
        !routes.contains("_pre_"),
        "no snapshot lets may appear when effects read no committed member:\n{routes}"
    );
}
