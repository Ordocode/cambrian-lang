// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! P0 acceptance tests for the Lean target.
//!
//! Three layers:
//!
//! 1. Library-level: `LeanBackend::gen_project` produces the expected
//!    set of files (lakefile, toolchain pin, vendored prelude,
//!    per-entity Lean file) with sane content. Runs in pure CI.
//! 2. Binary-level: invoke the `cambrian-transpiler` binary against a
//!    tiny in-tree fixture with `--target lean` and verify the same
//!    file structure on disk. Catches CLI/dispatch regressions.
//! 3. Optional `lake build` smoke check, gated on the
//!    `CAMBRIAN_TEST_LEAN_BUILD=1` env var so the suite stays green
//!    without a Lean toolchain present.

use std::path::{Path, PathBuf};
use std::process::Command;

use cambrian_transpiler::ast;
use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;
use cambrian_transpiler::ProgramParser;

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

fn parse(source: &str) -> ast::Program {
    let mut p = ProgramParser::new()
        .parse(source)
        .expect("test fixture must parse");
    ast::normalize_program_types(&mut p);
    p
}

// ---------------------------------------------------------------------------
// Layer 1: library-level — gen_program / extra_files / gen_project
// ---------------------------------------------------------------------------

#[test]
fn lean_p0_gen_program_emits_namespace_and_state() {
    let backend = LeanBackend::default();
    let program = parse(COUNTER_CAM);

    let lean = backend.gen_program(&program);

    assert!(lean.contains("import Cambrian.Prelude"), "should import the prelude:\n{}", lean);
    assert!(lean.contains("namespace Counter"), "should open the entity namespace:\n{}", lean);
    assert!(lean.contains("structure State where"), "should declare State:\n{}", lean);
    assert!(lean.contains("structure Identity where"), "should declare Identity:\n{}", lean);
    // P3.2: Identity must derive DecidableEq so the World-storage
    // `if id' = id then ... else ...` lookup-or-default is decidable.
    assert!(
        lean.contains("deriving Repr, DecidableEq"),
        "Identity should derive DecidableEq alongside Repr (P3.2):\n{}",
        lean
    );
    assert!(
        lean.contains("def State.identity"),
        "should expose the State -> Identity projection:\n{}",
        lean
    );
    assert!(lean.contains("end Counter"), "should close the namespace:\n{}", lean);
}

#[test]
fn lean_p0_extra_files_carry_lake_scaffolding_and_prelude() {
    let backend = LeanBackend::default();
    let program = parse(COUNTER_CAM);

    let extras = backend.extra_files(&program, "Counter");
    let by_path: std::collections::HashMap<_, _> =
        extras.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();

    let lakefile = by_path
        .get("lakefile.toml")
        .expect("lakefile.toml must be in extra_files");
    assert!(lakefile.contains("name = \"cambrian-generated\""), "lakefile name:\n{}", lakefile);
    assert!(lakefile.contains("[[lean_lib]]"), "lakefile must declare a lean_lib:\n{}", lakefile);

    let toolchain = by_path
        .get("lean-toolchain")
        .expect("lean-toolchain must be in extra_files");
    assert!(toolchain.starts_with("leanprover/lean4:"), "toolchain pin:\n{}", toolchain);
    assert!(toolchain.ends_with('\n'), "toolchain file must end with a newline");

    let root = by_path
        .get("Cambrian.lean")
        .expect("library root Cambrian.lean must be in extra_files");
    assert!(root.contains("import Cambrian.Prelude"), "root re-imports prelude:\n{}", root);
    assert!(
        root.contains("import Cambrian.Generated.Counter"),
        "root re-imports the generated entity:\n{}",
        root
    );

    let prelude = by_path
        .get("Cambrian/Prelude.lean")
        .expect("vendored prelude must be in extra_files");
    assert!(
        prelude.contains("import Cambrian.Core") && prelude.contains("import Cambrian.Evm"),
        "prelude facade must import Core + Evm:\n{}",
        prelude
    );

    let core = by_path
        .get("Cambrian/Core.lean")
        .expect("vendored Core must be in extra_files");
    assert!(core.contains("namespace Cambrian"), "Core must open Cambrian namespace");
    assert!(core.contains("abbrev U256"), "Core must define U256");
    assert!(core.contains("abbrev Address"), "Core must define Address");
    assert!(core.contains("inductive ThrowCode"), "Core must define ThrowCode");
    assert!(core.contains("abbrev RouteResult"), "Core must define RouteResult");
    assert!(
        core.contains("instDecidableOkImplies")
            && core.contains("instDecidableOkAnd")
            && core.contains("instDecidableErrCodeIs"),
        "PN-104 combinators need named Decidable so Plausible can synthesize Testable:\n{}",
        core
    );

    let evm = by_path
        .get("Cambrian/Evm.lean")
        .expect("vendored Evm must be in extra_files");
    assert!(evm.contains("structure MsgCtx"), "Evm must define MsgCtx");
    assert!(evm.contains("structure SysCtx"), "Evm must define SysCtx");

    for path in [
        "Cambrian/Core.lean",
        "Cambrian/Evm.lean",
        "Cambrian/Prelude.lean",
        "Cambrian/SimpAttrs.lean",
    ] {
        let body = by_path.get(path).unwrap_or_else(|| panic!("{path}"));
        assert!(
            body.starts_with("-- SPDX-License-Identifier: UNLICENSED\n"),
            "{path} must be tagged UNLICENSED like generated Solidity:\n{body}"
        );
        assert!(
            !body.contains("GPL-3.0-only"),
            "{path} must not ship the repo GPL identifier"
        );
    }
}

#[test]
fn lean_p0_gen_project_emits_per_entity_files() {
    use std::collections::BTreeSet;

    // Two-entity program: P0 is single-entity-only at the *route*
    // level, but `gen_project` should still emit one Lean file per
    // entity in the merged program.
    let two_entities = format!(
        "{}\n{}",
        COUNTER_CAM,
        r#"
entity Counter2 {
    routes { increment(amount: u64) => [] }
    m_count: u64 { in increment(amount) => m_count + amount }
}
"#
    );

    let dir = tempdir("lean-gen-project");
    let yaml_path = dir.join("project.yaml");
    let cam_path = dir.join("twocounters.cam");
    std::fs::write(&cam_path, two_entities).unwrap();
    std::fs::write(
        &yaml_path,
        r#"target: lean
sources:
  - twocounters.cam
"#,
    )
    .unwrap();

    let project = Project::load(&yaml_path).expect("project loads");
    let backend = LeanBackend::default();
    let files = backend.gen_project(&project);

    let paths: BTreeSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    assert!(paths.contains("lakefile.toml"));
    assert!(paths.contains("lean-toolchain"));
    assert!(paths.contains("Cambrian.lean"));
    assert!(paths.contains("Cambrian/Prelude.lean"));
    assert!(paths.contains("Cambrian/Core.lean"));
    assert!(paths.contains("Cambrian/Evm.lean"));
    assert!(paths.contains("Cambrian/Generated/Counter.lean"));
    assert!(paths.contains("Cambrian/Generated/Counter2.lean"));
    // P3.3 + P3.5 — per-program World module and per-entity Routes
    // module both land in the project layout.
    assert!(
        paths.contains("Cambrian/Generated/World.lean"),
        "P3.3: per-program World module must be emitted:\n{:?}",
        paths
    );
    assert!(
        paths.contains("Cambrian/Generated/CounterRoutes.lean")
            && paths.contains("Cambrian/Generated/Counter2Routes.lean"),
        "P3.5: per-entity Routes module must be emitted for each entity that has routes:\n{:?}",
        paths
    );

    let root = files
        .iter()
        .find(|(p, _)| p == "Cambrian.lean")
        .map(|(_, c)| c.as_str())
        .unwrap();
    assert!(
        root.contains("import Cambrian.Generated.Counter\n")
            && root.contains("import Cambrian.Generated.Counter2\n"),
        "library root must import every generated entity:\n{}",
        root
    );
    assert!(
        root.contains("import Cambrian.Generated.World\n"),
        "library root must import the World module (P3.3):\n{}",
        root
    );
}

// ---------------------------------------------------------------------------
// Layer 2: binary-level — invoke the CLI with `--target lean`
// ---------------------------------------------------------------------------

fn transpiler_bin() -> PathBuf {
    // `CARGO_BIN_EXE_*` rather than a walk up from `current_exe()`: the two
    // are the same path under the classic `target/debug/deps` layout, but a
    // cargo configured with a split build directory puts the test executable
    // somewhere else entirely, and the walk then points at a file that does
    // not exist. The failure reads as "No such file or directory" from the
    // spawn, which names neither the binary nor the layout.
    PathBuf::from(env!("CARGO_BIN_EXE_cambrian-transpiler"))
}

fn tempdir(stem: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cambrian-lean-{}-{}",
        stem,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn lean_p0_cli_target_lean_writes_buildable_layout() {
    let dir = tempdir("cli-counter");
    let cam_path = dir.join("counter.cam");
    let out_dir = dir.join("out");
    std::fs::write(&cam_path, COUNTER_CAM).unwrap();

    let output = Command::new(transpiler_bin())
        .arg(&cam_path)
        .arg("-o")
        .arg(&out_dir)
        .arg("--target")
        .arg("lean")
        .output()
        .expect("invoke transpiler");
    assert!(
        output.status.success(),
        "transpiler failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    for rel in [
        "lakefile.toml",
        "lean-toolchain",
        "Cambrian.lean",
        "Cambrian/Prelude.lean",
        "Cambrian/Core.lean",
        "Cambrian/Evm.lean",
        "Cambrian/Generated/Counter.lean",
    ] {
        let p = out_dir.join(rel);
        assert!(p.exists(), "expected {} to exist after transpiling", p.display());
    }

    let entity_lean = std::fs::read_to_string(out_dir.join("Cambrian/Generated/Counter.lean"))
        .expect("read generated entity file");
    assert!(entity_lean.contains("namespace Counter"), "entity file content:\n{}", entity_lean);

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Layer 3: optional `lake build` smoke check
// ---------------------------------------------------------------------------

fn lake_available() -> bool {
    Command::new("lake")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn lean_p0_lake_build_smoke() {
    // Opt-in only: requires Lean toolchain on PATH and an explicit
    // env var to keep CI green without one.
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() != Ok("1") {
        eprintln!(
            "skipping lake-build smoke (set CAMBRIAN_TEST_LEAN_BUILD=1 to enable)"
        );
        return;
    }
    if !lake_available() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }

    let dir = tempdir("lake-smoke");
    let cam_path = dir.join("counter.cam");
    let out_dir = dir.join("out");
    std::fs::write(&cam_path, COUNTER_CAM).unwrap();

    let tp_out = Command::new(transpiler_bin())
        .arg(&cam_path)
        .arg("-o")
        .arg(&out_dir)
        .arg("--target")
        .arg("lean")
        .output()
        .expect("invoke transpiler");
    assert!(tp_out.status.success(), "transpile failed: {:?}", tp_out);

    let lake_out = Command::new("lake")
        .arg("build")
        .current_dir(&out_dir)
        .output()
        .expect("invoke lake");
    assert!(
        lake_out.status.success(),
        "lake build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&lake_out.stdout),
        String::from_utf8_lossy(&lake_out.stderr),
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[allow(dead_code)]
fn _force_path_use(_: &Path) {}

// ---------------------------------------------------------------------------
// P1 acceptance tests
//
// These complement the P0 layer above:
// 1. Structural assertions on the lowered surface for each exit
//    fixture (Counter / Escrow / PhasedVault / phased_two_member).
// 2. L-rule rejections via the validator + CLI exit code.
// 3. Optional `lake build` smoke check across all four fixtures
//    (gated on `CAMBRIAN_TEST_LEAN_BUILD=1`).
// ---------------------------------------------------------------------------

const ESCROW_PATH: &str = "../contracts/escrow.cam";
const PHASED_VAULT_PATH: &str = "../contracts/phased_vault.cam";
const PHASED_TWO_MEMBER_PATH: &str = "tests/fixtures/phased_two_member.cam";

fn read_fixture(rel: &str) -> String {
    let mut p = std::env::current_dir().expect("cwd");
    p.push(rel);
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("read fixture {}: {}", p.display(), e))
}

#[test]
fn lean_p1_counter_emits_member_and_route_modules() {
    let backend = LeanBackend::default();
    let program = parse(COUNTER_CAM);
    let entity_lean = backend.gen_program(&program);
    // P3.5: route definitions now live in a sibling
    // `Cambrian/Generated/<E>Routes.lean` (the entity file holds only
    // State / Identity / per-member transforms). Pull both pieces
    // out via `extra_files`.
    let extras = backend.extra_files(&program, "Counter");
    let routes_lean = extras
        .iter()
        .find(|(p, _)| p == "Cambrian/Generated/CounterRoutes.lean")
        .map(|(_, c)| c.as_str())
        .expect("CounterRoutes.lean must be in extra_files (P3.5)");

    // State carries the typed member field.
    assert!(
        entity_lean.contains("m_count : BitVec 64"),
        "State should typed m_count:\n{}",
        entity_lean
    );

    // Per-(member, route) defs land in `Counter.Members.M_m_count`
    // and keep their pre-P3 signature `(s : E.State) (ctx : MsgCtx) ...`.
    assert!(
        entity_lean.contains("namespace Counter.Members.M_m_count"),
        "should open per-member namespace:\n{}",
        entity_lean
    );
    // P4d.audit: member transforms now thread `inst : E.Identity`
    // after `ctx` so `sys::address` can resolve to `(E.address inst)`
    // even outside the route entry-point context.
    assert!(
        entity_lean.contains("def increment (s : Counter.State) (ctx : Cambrian.MsgCtx) (inst : Counter.Identity) (amount : BitVec 64) : BitVec 64"),
        "increment transform signature:\n{}",
        entity_lean
    );
    assert!(
        entity_lean.contains("def reset (s : Counter.State) (ctx : Cambrian.MsgCtx) (inst : Counter.Identity) : BitVec 64"),
        "reset transform signature:\n{}",
        entity_lean
    );

    // P3.5: route entry-points are keyed by `(w : World)
    // (inst : Identity) (ctx : MsgCtx) …` and return World (or
    // World × T for views).
    assert!(
        routes_lean.contains("def increment (w : Cambrian.Generated.World) (inst : Counter.Identity) (ctx : Cambrian.MsgCtx) (amount : BitVec 64) : Cambrian.Generated.World"),
        "increment route entry-point lifted onto World (P3.5):\n{}",
        routes_lean
    );
    // View routes produce `World × T` and the inner body builds `(s,
    // payload)` before `wrap_with_world` rewraps into World.
    assert!(
        routes_lean.contains("def getCount (w : Cambrian.Generated.World) (inst : Counter.Identity) (ctx : Cambrian.MsgCtx) : Cambrian.Generated.World × BitVec 64"),
        "getCount view route returns World × payload:\n{}",
        routes_lean
    );
    assert!(
        routes_lean.contains("(s, s.m_count)"),
        "getCount inner body must still construct (state, payload):\n{}",
        routes_lean
    );
    assert!(
        !routes_lean.contains("RouteResult"),
        "Counter has no `where` / `throw` so no RouteResult should appear:\n{}",
        routes_lean
    );
}

#[test]
fn lean_p1_escrow_emits_pre_predicates_and_throws() {
    let backend = LeanBackend::default();
    let src = read_fixture(ESCROW_PATH);
    let program = parse(&src);
    let extras = backend.extra_files(&program, "Escrow");
    let routes = extras
        .iter()
        .find(|(p, _)| p == "Cambrian/Generated/EscrowRoutes.lean")
        .map(|(_, c)| c.as_str())
        .expect("EscrowRoutes.lean must be in extra_files (P3.5)");

    // Per-where-clause predicate naming.
    assert!(
        routes.contains("def fund_pre_0 "),
        "first `fund` where lowers to fund_pre_0:\n{}",
        routes
    );
    assert!(
        routes.contains("def fund_pre_1 "),
        "second `fund` where lowers to fund_pre_1:\n{}",
        routes
    );

    // Throw-codes from `where … : throw N` get plumbed through.
    assert!(
        routes.contains("Cambrian.ThrowCode.ofNat 10"),
        "ERROR_WRONG_STATE (10) should appear in throw chain:\n{}",
        routes
    );
    assert!(
        routes.contains("Cambrian.ThrowCode.ofNat 11"),
        "ERROR_NOT_BUYER (11) should appear in throw chain:\n{}",
        routes
    );

    // Failing routes return a `RouteResult World` wrapper.
    assert!(
        routes.contains("def fund (w : Cambrian.Generated.World) (inst : Escrow.Identity) (ctx : Cambrian.MsgCtx) : Cambrian.RouteResult (Cambrian.Generated.World)"),
        "fund route is wrapped because of where-clauses:\n{}",
        routes
    );

    // P3.8: raw `~> dest` sends inside unphased route bodies now
    // lower to `Cambrian.WorldState.transfer`. The old "skipped"
    // comment is gone for these — the call is real code.
    assert!(
        routes.contains("Cambrian.WorldState.transfer"),
        "outgoing unphased sends should lower to WorldState.transfer (P3.8):\n{}",
        routes
    );

    // Pure fns referenced from a where condition resolve through the
    // `Cambrian.Generated.Pure.*` namespace.
    assert!(
        routes.contains("Cambrian.Generated.Pure.is_party"),
        "pure fn calls should be qualified:\n{}",
        routes
    );

    // View route `getStatus` returns a tuple of all four members
    // alongside the (post-call) World.
    assert!(
        routes.contains("def getStatus (w : Cambrian.Generated.World) (inst : Escrow.Identity) (ctx : Cambrian.MsgCtx) : Cambrian.Generated.World × (BitVec 8 × Cambrian.U256 × BitVec 64 × Bool)"),
        "getStatus view return type lifted onto World (P3.5):\n{}",
        routes
    );
}

#[test]
fn lean_p1_phased_vault_emits_phase_fns_and_skips_gosh() {
    let backend = LeanBackend::default();
    let src = read_fixture(PHASED_VAULT_PATH);
    let program = parse(&src);
    let extras = backend.extra_files(&program, "PhasedVault");
    let routes = extras
        .iter()
        .find(|(p, _)| p == "Cambrian/Generated/PhasedVaultRoutes.lean")
        .map(|(_, c)| c.as_str())
        .expect("PhasedVaultRoutes.lean must be in extra_files (P3.5)");

    // Each phase gets its own function — per-phase helpers stay on
    // `(s : E.State) (ctx : MsgCtx)` (only the entry-point flips
    // onto `World × Identity`).
    assert!(
        routes.contains("def deposit_save (s : PhasedVault.State)"),
        "deposit_save phase fn:\n{}",
        routes
    );
    assert!(
        routes.contains("def deposit_finish (s : PhasedVault.State)"),
        "deposit_finish phase fn:\n{}",
        routes
    );
    // Top-level entry now operates on World.
    assert!(
        routes.contains("def deposit (w : Cambrian.Generated.World) (inst : PhasedVault.Identity) (ctx : Cambrian.MsgCtx)"),
        "deposit entry-point lifted onto World (P3.5):\n{}",
        routes
    );

    // `withdraw` has a route-level `where` ⇒ wrapped return.
    assert!(
        routes.contains("def withdraw (w : Cambrian.Generated.World) (inst : PhasedVault.Identity) (ctx : Cambrian.MsgCtx) (amount : BitVec 128) : Cambrian.RouteResult (Cambrian.Generated.World)"),
        "withdraw is wrapped:\n{}",
        routes
    );

    // gosh:: effects render as comments only (not supported on Lean).
    assert!(
        routes.contains("-- not supported on Lean: namespaced effect (gosh::* etc.)")
            || routes.contains("-- not supported on Lean: gosh::"),
        "gosh::* should be marked unsupported on Lean:\n{}",
        routes
    );
}

#[test]
fn lean_p1_phased_two_member_resolves_temporal_ref() {
    let backend = LeanBackend::default();
    let src = read_fixture(PHASED_TWO_MEMBER_PATH);
    let program = parse(&src);
    let entity_lean = backend.gen_program(&program);
    let extras = backend.extra_files(&program, "TwoMember");
    let routes = extras
        .iter()
        .find(|(p, _)| p == "Cambrian/Generated/TwoMemberRoutes.lean")
        .map(|(_, c)| c.as_str())
        .expect("TwoMemberRoutes.lean must be in extra_files (P3.5)");

    // m_b's mirror transform body should resolve `^m_a` via the
    // pre-mirror snapshot (i.e. `s.m_a`, since m_a has no transform
    // in the `mirror` phase — see lean_expr::gen_temporal_ref). The
    // per-member transform lives in the entity file, not routes.
    let body = extract_def_body(&entity_lean, "def bump_mirror (s : TwoMember.State)")
        .expect("bump_mirror member def must exist");
    assert!(
        body.contains("s.m_a"),
        "bump_mirror should fall back to s.m_a (no transform at mirror):\n{}",
        body
    );

    // Phase composition: `bump` chains both Local phase functions.
    assert!(
        routes.contains("TwoMember.Local.bump_inc"),
        "bump entry must call Local.bump_inc:\n{}",
        routes
    );
    assert!(
        routes.contains("TwoMember.Local.bump_mirror"),
        "bump entry must call Local.bump_mirror:\n{}",
        routes
    );
}

/// Extract the body of the *first* def whose signature line starts
/// with `header`. Returns the lines after the `:= …` until the next
/// blank line (Lean code uses blank lines to separate top-level
/// declarations, which is sufficient for our generated output).
fn extract_def_body<'a>(source: &'a str, header: &str) -> Option<&'a str> {
    let start = source.find(header)?;
    let after_header = &source[start..];
    let body_start = after_header.find("\n")? + 1;
    let body_slice = &after_header[body_start..];
    let end = body_slice.find("\n\n").unwrap_or(body_slice.len());
    Some(&body_slice[..end])
}

// ---------------------------------------------------------------------------
// L-rule rejections
// ---------------------------------------------------------------------------

#[test]
fn lean_range_fold_bounds_are_nat_not_bitvec() {
    // `(0..7).fold` / `(0..n).fold` under a `-> U256` pure fn must lower
    // `List.range` bounds as `Nat`. Ambient `expected_bitvec_width` from
    // the return type must not ascribe `(7 : BitVec 256)` into that slot.
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../contracts/fold_evm.cam"),
    )
    .expect("fold_evm.cam");
    let program = parse(&src);
    let extras = LeanBackend::default().extra_files(&program, "FoldDemo");
    let pure = extras
        .iter()
        .find(|(p, _)| p == "Cambrian/Generated/Pure.lean")
        .map(|(_, c)| c.as_str())
        .expect("Pure.lean");
    assert!(
        pure.contains("List.range (7 - 0)") && !pure.contains("List.range ((7 : BitVec"),
        "literal range bounds must stay Nat:\n{}",
        pure
    );
    assert!(
        pure.contains("List.range ((n).toNat - 0)")
            && !pure.contains("List.range ((n).toNat - (0 : BitVec"),
        "mixed BitVec/literal range bounds must coerce only the BitVec side:\n{}",
        pure
    );
}

#[test]
fn lean_p4_l1_allows_hashmap_member() {
    use cambrian_transpiler::validate::{check_lean_target_compat, Severity};
    let prog = parse(
        r#"
entity Map {
    routes { put(k: address, v: U256) => [] }
    m_data: HashMap<address, U256> {}
}
"#,
    );
    let diags = check_lean_target_compat(&prog);
    assert!(
        !diags
            .iter()
            .any(|d| d.code == "L1" && matches!(d.severity, Severity::Error)),
        "L1 must not error on HashMap members after P4: {:?}",
        diags
    );
    let backend = LeanBackend::default();
    let entity_lean = backend.gen_program(&prog);
    assert!(
        entity_lean.contains("Cambrian.AddressMap"),
        "HashMap member must lower to AddressMap (P4):\n{}",
        entity_lean
    );
}

#[test]
fn lean_p4_l3_allows_extern_entity() {
    use cambrian_transpiler::validate::{check_lean_target_compat, Severity};
    let prog = parse(
        r#"
extern entity Token {
    route transfer(to: address, amount: U256);
}
entity Caller {
    routes { ping() => [] }
}
"#,
    );
    let diags = check_lean_target_compat(&prog);
    assert!(
        !diags
            .iter()
            .any(|d| d.code == "L3" && matches!(d.severity, Severity::Error)),
        "L3 must not error on extern entity after P4: {:?}",
        diags
    );
    let files: std::collections::HashMap<_, _> = LeanBackend::default()
        .extra_files(&prog, "Caller")
        .into_iter()
        .collect();
    assert!(
        files.contains_key("Cambrian/Generated/Extern.lean"),
        "extern entity must emit Extern.lean (P4)"
    );
}

#[test]
fn lean_p4_cli_hashmap_transpiles() {
    let dir = tempdir("cli-hashmap");
    let cam_path = dir.join("map.cam");
    let out_dir = dir.join("out");
    std::fs::write(
        &cam_path,
        r#"
entity Map {
    routes { put(k: address, v: U256) => [] }
    m_data: HashMap<address, U256> {}
}
"#,
    )
    .unwrap();

    let output = Command::new(transpiler_bin())
        .arg(&cam_path)
        .arg("-o")
        .arg(&out_dir)
        .arg("--target")
        .arg("lean")
        .output()
        .expect("invoke transpiler");
    assert!(
        output.status.success(),
        "transpiler must accept HashMap members on lean target after P4.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let map_lean = std::fs::read_to_string(out_dir.join("Cambrian/Generated/Map.lean"))
        .expect("Map.lean");
    assert!(
        map_lean.contains("Cambrian.AddressMap"),
        "generated Map.lean must use AddressMap:\n{}",
        map_lean
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// `lake build` smoke check across the four exit fixtures.
// ---------------------------------------------------------------------------

#[test]
fn lean_p1_lake_build_smoke_all_fixtures() {
    if std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() != Ok("1") {
        eprintln!("skipping lake-build smoke (set CAMBRIAN_TEST_LEAN_BUILD=1 to enable)");
        return;
    }
    if !lake_available() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }

    let fixtures: &[(&str, &str)] = &[
        ("counter", "../contracts/counter.cam"),
        ("predictable", "../contracts/escrow.cam"),
        ("phased_vault", "../contracts/phased_vault.cam"),
        ("phased_two_member", "tests/fixtures/phased_two_member.cam"),
    ];

    for (name, rel) in fixtures {
        let cam = std::env::current_dir().unwrap().join(rel);
        assert!(cam.exists(), "fixture missing: {}", cam.display());

        let out_dir = tempdir(&format!("smoke-{}", name));

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
}
