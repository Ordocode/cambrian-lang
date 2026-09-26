// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! P4a acceptance — cross-entity sends, deploy, `from Entity`, relaxed L8.

use cambrian_transpiler::ast;
use cambrian_transpiler::codegen::{LeanBackend, OutputBackend};
use cambrian_transpiler::target::Target;
use cambrian_transpiler::validate::{check_lean_target_compat, check_target_compat, Severity};
use cambrian_transpiler::ProgramParser;

fn parse(source: &str) -> ast::Program {
    let mut p = ProgramParser::new()
        .parse(source)
        .expect("fixture must parse");
    ast::normalize_program_types(&mut p);
    p
}

fn extras(source: &str, entity: &str) -> std::collections::HashMap<String, String> {
    let backend = LeanBackend::default();
    let program = parse(source);
    backend
        .extra_files(&program, entity)
        .into_iter()
        .collect()
}

const CROSS_SEND_CAM: &str = include_str!("../../contracts/lean_cross_send.cam");

const DEPLOY_CAM: &str = include_str!("../../contracts/lean_deploy.cam");

#[test]
fn lean_p4a_cross_entity_send_lowers_world_call() {
    let extras = extras(CROSS_SEND_CAM, "Vault");
    let routes = extras
        .get("Cambrian/Generated/VaultRoutes.lean")
        .expect("VaultRoutes.lean");
    assert!(
        routes.contains("Treasury.Routes.credit"),
        "cross-entity typed send must target callee route (P4a):\n{}",
        routes
    );
    assert!(
        routes.contains("Treasury.Routes.credit w"),
        "cross-entity typed send must call callee route with world + identity (P4a):\n{}",
        routes
    );
}

#[test]
fn lean_p4a_relaxed_l8_allows_treasury_address() {
    let prog = parse(CROSS_SEND_CAM);
    let errs: Vec<_> = check_lean_target_compat(&prog)
        .into_iter()
        .filter(|d| d.severity == Severity::Error && d.code == "L8")
        .collect();
    assert!(
        errs.is_empty(),
        "L8 must allow typed sends to in-program entities (P4a): {:?}",
        errs
    );
}

#[test]
fn lean_p4a_deploy_lowers_world_with() {
    let extras = extras(DEPLOY_CAM, "Factory");
    let routes = extras
        .get("Cambrian/Generated/FactoryRoutes.lean")
        .expect("FactoryRoutes.lean");
    assert!(
        routes.contains("World.withChild"),
        "deploy must install instance via World.with<Child> (P4a):\n{}",
        routes
    );
    assert!(
        routes.contains("_deployed_"),
        "deploy must bind predicted address (P4a):\n{}",
        routes
    );
}

#[test]
fn lean_emit_lowers_to_ghost_event_log() {
    // `emit Name(args)` lowers to `Cambrian.WorldState.emit` appending a
    // typed `Cambrian.Generated.Event` constructor to the ghost log, and
    // forces the route onto the world-threaded path (no `-- skipped`).
    let src = include_str!("../../contracts/erc20_events_evm.cam");

    // World module declares the per-program Event inductive with one
    // (entity-prefixed) constructor per declared event.
    let world = extras(src, "Erc20Events")
        .get("Cambrian/Generated/World.lean")
        .cloned()
        .expect("World.lean");
    assert!(
        world.contains("inductive Event where")
            && world.contains("| Erc20Events_Transfer")
            && world.contains("| Erc20Events_Approval"),
        "World must declare an Event constructor per declared event:\n{}",
        world
    );
    assert!(
        world.contains("abbrev World := Cambrian.WorldState Storage Event"),
        "World abbrev must carry the Event log type:\n{}",
        world
    );
    assert!(
        world.contains("events   := []"),
        "World.default must seed an empty event log:\n{}",
        world
    );

    let routes = extras(src, "Erc20Events")
        .get("Cambrian/Generated/Erc20EventsRoutes.lean")
        .cloned()
        .expect("Erc20EventsRoutes.lean");
    assert!(
        routes.contains(
            "Cambrian.WorldState.emit w (Cambrian.Generated.Event.Erc20Events_Transfer ctx.sender to amount)"
        ),
        "emit must append a typed event to the ghost log:\n{}",
        routes
    );
    assert!(
        !routes.contains("skipped: emit"),
        "emit must no longer be dropped to a skipped comment:\n{}",
        routes
    );
}

#[test]
fn lean_call_lowers_to_synchronous_self_invocation() {
    // `call <route>(args)` lowers to a synchronous, same-instance,
    // same-`ctx` invocation of the entity's own route: write `s` back to
    // `w`, invoke `<Self>.Routes.<name> w inst ctx args`, re-read `s`.
    // A failing callee reached from a failing caller propagates via `←`.
    let src = include_str!("../../contracts/lean_call.cam");
    let routes = extras(src, "Accum")
        .get("Cambrian/Generated/AccumRoutes.lean")
        .cloned()
        .expect("AccumRoutes.lean");

    // The total helper is invoked with the *current* inst + ctx (no
    // identity record, no ctx swap, no value), and not dropped.
    assert!(
        routes.contains("Accum.Routes.bump w inst ctx amount"),
        "call must invoke the target route with current inst + ctx:\n{}",
        routes
    );
    assert!(
        !routes.contains("skipped: call"),
        "call must no longer be dropped to a skipped comment:\n{}",
        routes
    );

    // A failing callee, reached from a failing caller, propagates via `←`.
    assert!(
        routes.contains("let w ← Accum.Routes.checkedBump w inst ctx amount"),
        "failing callee must propagate synchronously via bind (`←`):\n{}",
        routes
    );

    // Callees must be emitted before their callers (Lean has no forward
    // references for plain `def`s).
    let bump_idx = routes.find("def bump ").expect("bump def");
    let bump_twice_idx = routes.find("def bumpTwice ").expect("bumpTwice def");
    let checked_idx = routes.find("def checkedBump ").expect("checkedBump def");
    let guarded_idx = routes.find("def guardedBump ").expect("guardedBump def");
    assert!(
        bump_idx < bump_twice_idx && checked_idx < guarded_idx,
        "call targets must be emitted before their callers:\n{}",
        routes
    );
}

#[test]
fn lean_return_in_send_bearing_route_keeps_payload() {
    // A `return(...)` in a world-threaded (send-bearing) view route must
    // be lowered into the `(w, <payload>)` tail, not dropped to
    // `default` / a placeholder comment.
    let src = include_str!("../../contracts/lean_send_return.cam");
    let routes = extras(src, "Counter")
        .get("Cambrian/Generated/CounterRoutes.lean")
        .cloned()
        .expect("CounterRoutes.lean");

    assert!(
        routes.contains("(w, bv)"),
        "send-bearing route must return the captured payload `(w, bv)`:\n{}",
        routes
    );
    assert!(
        !routes.contains("early-return in send-bearing route deferred"),
        "the placeholder return comment must be gone:\n{}",
        routes
    );
    assert!(
        !routes.contains("(w, default)"),
        "the return value must not be dropped to `default`:\n{}",
        routes
    );
}

#[test]
fn lean_effects_inside_conditional_branches_survive() {
    // Effects nested inside an `if`-action of a world-threaded route used
    // to be dropped to `-- skipped: action`. Each branch must now thread
    // `w` so the send/emit/throw survive.
    let src = include_str!("../../contracts/lean_conditional.cam");
    let routes = extras(src, "Switch")
        .get("Cambrian/Generated/SwitchRoutes.lean")
        .cloned()
        .expect("SwitchRoutes.lean");

    assert!(
        !routes.contains("skipped: action"),
        "conditional branches must no longer drop their effects:\n{}",
        routes
    );
    assert!(
        routes.contains("Cambrian.WorldState.transfer"),
        "the transfer inside the `then` branch must survive:\n{}",
        routes
    );
    assert!(
        routes.contains("Cambrian.WorldState.emit w (Cambrian.Generated.Event.Switch_Toggled"),
        "the emits inside both branches must survive:\n{}",
        routes
    );
    // Send-bearing route (`ping`): transfer always fail-propagates, so
    // both branches thread `w` in the Except monad via `let w ← (if …)`.
    assert!(
        routes.contains("let w ← (if")
            && routes.contains("throw (Cambrian.ThrowCode.ofNat 90)"),
        "send-bearing conditional must bind `let w ← (if …)` and fail-propagate transfer:\n{}",
        routes
    );
    // Fail route (`guard`) threads each branch in the Except monad and a
    // branch `throw` propagates via `←`.
    assert!(
        routes.contains("throw (Cambrian.ThrowCode.ofNat 2)"),
        "fail-mode conditional must keep the branch throw:\n{}",
        routes
    );
    // `bare` has no route-level fail surface — its only `throw` is nested
    // inside the `if`, yet `route_fail_mode` recurses so the signature is
    // `RouteResult`-wrapped and the throw is emitted (not dropped).
    assert!(
        routes.contains("def bare")
            && routes.contains("def bare")
            && routes.split("def bare").nth(1).map_or(false, |tail| {
                let sig_end = tail.find(":=").unwrap_or(tail.len());
                tail[..sig_end].contains("Cambrian.RouteResult")
            }),
        "throw nested in a conditional must make `bare` a `RouteResult` fail surface:\n{}",
        routes
    );
    assert!(
        routes.contains("throw (Cambrian.ThrowCode.ofNat 7)"),
        "the conditional-nested throw in `bare` must be emitted, not dropped:\n{}",
        routes
    );
    // `throw TooLow(n)` (custom error in a send-bearing route's `else`)
    // must lower to a stable, non-zero code derived from the error name
    // (FNV-1a, matching `cambrian_function_id`) — never the `ofNat 0`
    // placeholder, and never a `TODO`/`skipped` comment.
    let expected = fnv1a_32("TooLow");
    assert_ne!(expected, 0, "FNV of an error name should be non-zero");
    assert!(
        routes.contains(&format!("throw (Cambrian.ThrowCode.ofNat {}) -- TooLow", expected)),
        "custom error `TooLow` must lower to its derived code {}:\n{}",
        expected,
        routes
    );
    assert!(
        !routes.contains("ThrowCode.ofNat 0")
            && !routes.contains("TODO(P4): CustomErr"),
        "custom errors must not fall back to the ofNat 0 placeholder:\n{}",
        routes
    );
}

#[test]
fn lean_conditional_only_return_keeps_payload() {
    // A view route whose only `return(...)` lives inside a conditional
    // must lift its payload into a value-producing `if … then … else …`
    // term rather than collapsing to `(s, default)` / `(w, default)`.

    // Non-threaded path: `classify` on the `Switch` entity.
    let switch_routes = extras(include_str!("../../contracts/lean_conditional.cam"), "Switch")
        .get("Cambrian/Generated/SwitchRoutes.lean")
        .cloned()
        .expect("SwitchRoutes.lean");
    let classify = switch_routes
        .split("def classify")
        .nth(1)
        .map(|t| t.split("\n\n").next().unwrap_or(t).to_string())
        .expect("classify def");
    assert!(
        classify.contains("if (n >")
            && classify.contains("then")
            && classify.contains("else")
            && (classify.contains("then (2)")
                || classify.contains("then ((2")
                || classify.contains("then (2 :")),
        "classify must lift its nested returns into an if/else payload term:\n{}",
        classify
    );
    assert!(
        !classify.contains("(s, default)"),
        "classify's payload must not collapse to `default`:\n{}",
        classify
    );

    // World-threaded path: `kickThenChoose` on the `Counter` entity
    // captures `bv` then returns it from inside a conditional.
    let counter_routes = extras(include_str!("../../contracts/lean_send_return.cam"), "Counter")
        .get("Cambrian/Generated/CounterRoutes.lean")
        .cloned()
        .expect("CounterRoutes.lean");
    let kick = counter_routes
        .split("def kickThenChoose")
        .nth(1)
        .map(|t| t.split("\n\n").next().unwrap_or(t).to_string())
        .expect("kickThenChoose def");
    assert!(
        kick.contains("(w, if (n >")
            && kick.contains("then (bv)")
            && kick.contains("else"),
        "kickThenChoose must lift its conditional return into the (w, …) tail:\n{}",
        kick
    );
    assert!(
        !kick.contains("(w, default)"),
        "kickThenChoose's payload must not collapse to `default`:\n{}",
        kick
    );
}

#[test]
fn lean_throw_inside_for_loop_propagates() {
    // A `throw` nested inside a `for` loop must propagate, not be dropped
    // to `-- skipped: action-level for-loop`. `route_fail_mode` recurses
    // into the loop, so the route is a `RouteResult` fail surface and the
    // loop must fold *monadically* (`foldlM`) in the failure monad —
    // both on the non-threaded (`s`) and world-threaded (`w`) paths. The
    // world-threaded case previously emitted invalid Lean (`throw` inside
    // `Id.run do`).
    let routes = extras(include_str!("../../contracts/lean_conditional.cam"), "Switch")
        .get("Cambrian/Generated/SwitchRoutes.lean")
        .cloned()
        .expect("SwitchRoutes.lean");

    assert!(
        !routes.contains("skipped: action-level for-loop"),
        "for-loop bodies that can fail must no longer be dropped:\n{}",
        routes
    );

    // Non-send route `scanAll`: folds over `s` with `foldlM` and the
    // branch throw is emitted inside the loop.
    let scan = routes
        .split("def scanAll")
        .nth(1)
        .map(|t| t.split("\n\n").next().unwrap_or(t).to_string())
        .expect("scanAll def");
    assert!(
        scan.contains(".foldlM (fun (s :")
            && scan.contains("throw (Cambrian.ThrowCode.ofNat 8)"),
        "scanAll must fold monadically over `s` and keep its loop throw:\n{}",
        scan
    );

    // Send-bearing route `broadcast`: folds over `w` with `foldlM` (NOT
    // `Id.run do`, which cannot throw) so the branch throw + send revert
    // propagate.
    let bc = routes
        .split("def broadcast")
        .nth(1)
        .map(|t| t.split("\n\n").next().unwrap_or(t).to_string())
        .expect("broadcast def");
    assert!(
        bc.contains(".foldlM (fun (w : Cambrian.Generated.World)") && !bc.contains("Id.run"),
        "broadcast must fold monadically over `w` (no `Id.run`):\n{}",
        bc
    );
    assert!(
        bc.contains("throw (Cambrian.ThrowCode.ofNat 11)"),
        "broadcast must keep the `else`-branch throw inside the loop:\n{}",
        bc
    );
}

/// FNV-1a 32-bit — mirror of `codegen::types::cambrian_function_id`, used
/// to predict the `ThrowCode` a custom error lowers to.
fn fnv1a_32(name: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in name.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

#[test]
fn lean_l11_internal_call_to_failing_route_requires_fail_surface() {
    // Nested `call` to a failing helper auto-promotes the wrapper to a
    // fail surface (T-X-010 / T-VAL-003) — that shape is *not* L11.
    let call_promoted = r#"
entity E {
    routes {
        init create() => []
        wrap(n: u64) => [ call risky(n) ]
        private risky(n: u64) where n < 10 : throw 2 => []
    }
    m_x: u64 { in create() => 0  in risky(n) => m_x + n }
}
"#;
    let errs: Vec<_> = check_lean_target_compat(&parse(call_promoted))
        .into_iter()
        .filter(|d| d.code == "L11")
        .collect();
    assert!(
        errs.is_empty(),
        "call-wrapper auto-promotion must clear L11: {:?}",
        errs
    );

    // Fire-and-forget self-send to a failing route still uses silent
    // recovery unless the caller is a fail surface — that remains L11.
    let bad = r#"
entity E {
    routes {
        init create() => []
        wrap(n: u64) => [ risky(n) ~> E.address() ]
        private risky(n: u64) where n < 10 : throw 2 => []
    }
    m_x: u64 { in create() => 0  in risky(n) => m_x + n }
}
"#;
    let errs: Vec<_> = check_lean_target_compat(&parse(bad))
        .into_iter()
        .filter(|d| d.code == "L11" && d.severity == Severity::Error)
        .collect();
    assert_eq!(errs.len(), 1, "expected one L11 error, got: {:?}", errs);

    // Making the caller a fail surface (its own `where`) clears L11.
    let ok = r#"
entity E {
    routes {
        init create() => []
        wrap(n: u64) where n > 0 : throw 1 => [ risky(n) ~> E.address() ]
        private risky(n: u64) where n < 10 : throw 2 => []
    }
    m_x: u64 { in create() => 0  in risky(n) => m_x + n }
}
"#;
    let errs: Vec<_> = check_lean_target_compat(&parse(ok))
        .into_iter()
        .filter(|d| d.code == "L11")
        .collect();
    assert!(errs.is_empty(), "fail-surface caller must clear L11: {:?}", errs);
}

#[test]
fn lean_e26_rescue_errors_unsupported() {
    let src = include_str!("../../contracts/bouncer.cam");
    let errs: Vec<_> = check_target_compat(&parse(src), Target::Lean, false)
        .into_iter()
        .filter(|d| d.code == "E26" && d.severity == Severity::Error)
        .collect();
    assert!(
        !errs.is_empty(),
        "rescue routes must raise E26 on Lean (Acki Nacki-only bounce)",
    );
    let l12: Vec<_> = check_lean_target_compat(&parse(src))
        .into_iter()
        .filter(|d| d.code == "L12")
        .collect();
    assert!(l12.is_empty(), "L12 is superseded by E26: {:?}", l12);
}

#[test]
fn lean_phased_deploy_only_route_threads_world() {
    // Regression: a phased route whose only effect is a `deploy` (no
    // `~>` send) must still take the world-threaded path so the deploy
    // is lowered, instead of being dropped to a `-- skipped` comment.
    let src = r#"
entity Child {
    identity m_id: u64
    routes {
        constructor() => []
    }
}

entity Factory {
    m_count: u64 { in spawn(_) =>
        prep: m_count + 1
    }
    routes {
        constructor() => []
        spawn(id: u64) => [
            prep: []
            launch: [
                deploy Child(id)
            ]
        ]
    }
}
"#;
    let extras = extras(src, "Factory");
    let routes = extras
        .get("Cambrian/Generated/FactoryRoutes.lean")
        .expect("FactoryRoutes.lean");
    assert!(
        routes.contains("World.withChild"),
        "phased deploy-only route must install instance via World.with<Child>:\n{}",
        routes
    );
    assert!(
        !routes.contains("skipped: deploy"),
        "phased deploy must not be dropped to a skipped comment:\n{}",
        routes
    );
    // The phase transform must still precede the deploy (transform-then-effect).
    let count_idx = routes
        .find("m_count :=")
        .expect("prep-phase transform must be emitted");
    let deploy_idx = routes
        .find("World.withChild")
        .expect("deploy must be emitted");
    assert!(
        count_idx < deploy_idx,
        "prep-phase transform must precede the launch-phase deploy:\n{}",
        routes
    );
}

#[test]
fn lean_phased_no_send_route_threads_inst_and_from_check() {
    // Regression: a phased route *without* sends takes the per-phase
    // `def` path. Those phase fns must (a) accept `inst` in scope (the
    // member-transform / per-phase-`where` calls reference it) and
    // (b) the entry point must still emit the `from`-clause sender
    // check. Both were previously broken/missing on this path.
    let src = r#"
entity Gated {
    identity m_owner: address
    m_count: u64 { in bump(_) =>
        prep: m_count + 1
    }
    routes {
        constructor() => []
        bump(n: u64)
            from m_owner : throw 7
        => [
            prep: []
            done: []
        ]
    }
}
"#;
    let extras = extras(src, "Gated");
    let routes = extras
        .get("Cambrian/Generated/GatedRoutes.lean")
        .expect("GatedRoutes.lean");
    // Per-phase fn must take `inst`.
    assert!(
        routes.contains("def bump_prep (s : Gated.State) (ctx : Cambrian.MsgCtx) (inst : Gated.Identity)"),
        "per-phase fn must accept `inst` so transform/where calls resolve:\n{}",
        routes
    );
    // Entry point must pass `inst` when invoking the phase fns.
    assert!(
        routes.contains("bump_prep s ctx inst"),
        "phase fn must be called with `inst`:\n{}",
        routes
    );
    // `from m_owner` sender check must be present on the entry point.
    assert!(
        routes.contains("ctx.sender == s.m_owner")
            && routes.contains("Cambrian.ThrowCode.ofNat 7"),
        "phased route must still emit the `from` sender check:\n{}",
        routes
    );
}

#[test]
fn lean_p4a_from_entity_emits_sender_check() {
    let from_src = format!(
        "{}\n{}",
        include_str!("../../contracts/det_guardian.cam"),
        include_str!("../../contracts/det_locker.cam"),
    );
    let extras = extras(&from_src, "Locker");
    let routes = extras
        .get("Cambrian/Generated/LockerRoutes.lean")
        .expect("LockerRoutes.lean");
    assert!(
        routes.contains("acceptPing"),
        "Locker must emit acceptPing route (P4a):\n{}",
        routes
    );
    assert!(
        routes.contains("ctx.sender"),
        "from Guardian(m_id) must compare msg.sender (P4a):\n{}",
        routes
    );
    assert!(
        routes.contains("Guardian.address")
            || routes.contains("ctx.sender == s.m_id"),
        "from Guardian(m_id) must compare to CREATE2 address or identity member (P4a):\n{}",
        routes
    );
}

#[test]
fn lean_p4d_l8_accepts_dynamic_dest_via_dispatch() {
    // P4d: `~> dest` where `dest : address` is now lowered through the
    // generated `Cambrian.Generated.Dispatch.Untyped.<msg>` opaque axiom
    // when *some* in-program entity exposes a route matching `<msg>`.
    // The previous L8 rejection has been lifted; a truly unknown message
    // still errors below.
    let accepted = r#"
entity E {
    routes {
        init create() => []
        bad(dest: address) => [
            ping() ~> dest
        ]
        ping() => []
    }
    m_x: u64 { in create() => 0 }
}
"#;
    let prog = parse(accepted);
    let errs: Vec<_> = check_lean_target_compat(&prog)
        .into_iter()
        .filter(|d| d.code == "L8" && d.severity == Severity::Error)
        .collect();
    assert!(
        errs.is_empty(),
        "P4d: plain-address ~> dest with matching in-program route must lower via the dispatch axiom, not error L8:\n{:?}",
        errs,
    );

    // `wat` isn't defined anywhere → the axiom can't be synthesised.
    // Validator still rejects the send to keep the failure mode local
    // to codegen (no axiom-omitted Lean module emitted blindly).
    let rejected = r#"
entity E {
    routes {
        init create() => []
        bad(dest: address) => [
            wat() ~> dest
        ]
    }
    m_x: u64 { in create() => 0 }
}
"#;
    let prog = parse(rejected);
    let errs: Vec<_> = check_lean_target_compat(&prog)
        .into_iter()
        .filter(|d| d.code == "L8" && d.severity == Severity::Error)
        .collect();
    assert!(
        !errs.is_empty(),
        "P4d: ~> dest must still error L8 when no in-program route matches the message name",
    );
}
