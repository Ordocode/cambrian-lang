// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Domain adapter for Lean expression / address lowering.
//!
//! Today the Lean target is always **evm × lean**; [`EvmDomain`] is the
//! sole impl. A future non-EVM Lean adapter swaps the [`LeanDomain`]
//! impl while leaving `lean/expr.rs` (and LeanCore) unchanged.

/// Context for [`LeanDomain::sys_field`] — the subset of expression
/// lowering state that `sys::*` resolution needs (world/instance vars,
/// entity name, numerics mode).
pub struct SysFieldCtx<'a> {
    pub world_var: Option<&'a str>,
    pub instance_var: Option<&'a str>,
    pub entity_name: &'a str,
    pub nat_numerics: bool,
}

/// Domain adapter for Lean expression / address lowering.
pub trait LeanDomain {
    fn msg_field(&self, field: &str, nat_numerics: bool) -> String;
    fn sys_field(&self, field: &str, ctx: &SysFieldCtx<'_>) -> String;
    fn balance_world_update(&self, world: &str, entity: &str, inst: &str, value: &str) -> String;
    /// Lower a CREATE2-style `(E.address (E.Identity.mk …))` term from
    /// already-lowered identity argument terms.
    fn entity_address_term(&self, entity: &str, id_arg_terms: &[String]) -> String;
    fn trace_field(&self, field: &str, trace_acc_var: Option<&str>) -> String;
    fn trace_call(&self, name: &str, route: &str, trace_acc_var: Option<&str>) -> String;
}

/// EVM-domain lowering for Lean expressions (`msg`/`sys`/`trace`/CREATE2).
#[derive(Debug, Clone, Copy, Default)]
pub struct EvmDomain;

impl LeanDomain for EvmDomain {
    fn msg_field(&self, field: &str, nat_numerics: bool) -> String {
        // `MsgCtx` continues to carry `sender` / `value` / `timestamp`
        // verbatim — World mode does not change the per-call message
        // surface. Cross-target-specific msg fields (`msg::pubkey`,
        // `msg::currencies`, `msg::body`) are rejected upstream by the
        // EVM/Lean validator (`E03`, `E04`, `E13`).
        //
        // `MsgCtx` stores `value` / `timestamp` as fixed-width `BitVec`; under
        // `numerics: nat` they enter user-level `Nat` arithmetic/state, so they
        // are projected via `.toNat` at the read boundary. `sender` is an
        // `address` and stays fixed-width.
        match field {
            "sender" => "ctx.sender".to_string(),
            "value" if nat_numerics => "(ctx.value).toNat".to_string(),
            "value" => "ctx.value".to_string(),
            "timestamp" if nat_numerics => "(ctx.timestamp).toNat".to_string(),
            "timestamp" => "ctx.timestamp".to_string(),
            other => format!(
                "(/- TODO(P3): msg::{} is target-specific -/ Cambrian.Unsupported)",
                other,
            ),
        }
    }

    fn sys_field(&self, field: &str, ctx: &SysFieldCtx<'_>) -> String {
        // `World` / `BlockEnv` store numeric `sys::*` fields as fixed-width
        // `BitVec`. Under `numerics: nat` they flow into user-level `Nat`
        // arithmetic and state, so numeric reads are projected via `.toNat` at
        // this boundary. `sys::address` is an `address` and stays fixed-width.
        let natw = |s: String| {
            if ctx.nat_numerics {
                format!("({}).toNat", s)
            } else {
                s
            }
        };
        match field {
            "balance" => {
                // P3.6: in World mode, `sys::balance` resolves to the
                // running balance of *this* contract instance, derived
                // via `<E>.address inst`. Outside World mode (e.g. spec
                // expressions evaluated against a bare state, or per-
                // phase fn bodies) we fall back to the P1 placeholder.
                match (ctx.world_var, ctx.instance_var) {
                    (Some(w), Some(inst)) => natw(format!(
                        "({}.balances ({}.address {}))",
                        w, ctx.entity_name, inst
                    )),
                    _ => "(/- TODO(P3): sys::balance outside World mode -/ Cambrian.Unsupported)"
                        .to_string(),
                }
            }
            // `sys::address` — this contract's address. Derived from
            // `(E.address inst)` whenever `inst` is in scope (entry-point
            // bodies, member transforms, and `_pre_<i>` predicates all
            // thread `inst : E.Identity` explicitly). Outside any
            // entity-instance context (top-level pure fns, spec helpers
            // without an instance) we fall back to a self-documenting
            // placeholder.
            "address" => match ctx.instance_var {
                Some(inst) => format!("({}.address {})", ctx.entity_name, inst),
                None => "(/- sys::address outside instance scope -/ ctx.sender)".to_string(),
            },
            // `sys::timestamp` — surfaced both as a route-body precondition
            // and as a `BlockEnv` field in World mode. We pull from
            // `BlockEnv` when available so route bodies and member transforms
            // see the world-advanced clock; everywhere else we fall back to
            // the per-call `MsgCtx.timestamp` (matches the EVM source).
            // `sys::timestamp` — surfaced both as a route-body precondition
            // (typically vs. a U256 deadline; coerced at the binop site)
            // and as the new value of a `u64` member (e.g.
            // `m_block_timestamp_last`). Emitted at its native `BitVec 64`
            // width so member-assignment stays type-correct; callers that
            // need U256 widen explicitly via `Cambrian.castWidth 256 …`
            // (auto-inserted by `gen_binop`'s comparison-arm and
            // `coerce_arg`'s pure-fn-arg arm).
            // `sys::now` is an alias of `sys::timestamp` everywhere else in the
            // toolchain (`sys_block_field`, desugar, EVM codegen); keep them unified.
            "now" | "timestamp" => natw(match ctx.world_var {
                Some(w) => format!("{}.block.timestamp", w),
                None => "ctx.timestamp".to_string(),
            }),
            // `sys::chainid` / `sys::blockNumber` live on `BlockEnv` in
            // World mode; outside, we fall back to `ctx.chainId` (added to
            // `MsgCtx` in P4d) / a u256-zero placeholder for `blockNumber`.
            "chainid" | "chainId" => natw(match ctx.world_var {
                Some(w) => format!("(({}.block.chainId).zeroExtend 256)", w),
                None => "ctx.chainId".to_string(),
            }),
            // World `BlockEnv.number` is `BitVec 64`; widen like `chainId` so
            // the term matches the 256-bit width model (`bitvec_width` /
            // UPSTREAM B-24). Ctx-mode already uses a U256 placeholder.
            "blockNumber" | "block_number" | "number" => natw(match ctx.world_var {
                Some(w) => format!("(({}.block.number).zeroExtend 256)", w),
                None => "(0#256 : Cambrian.U256)".to_string(),
            }),
            // Other sys::* fields aren't modelled. Emit a placeholder so
            // Lean refuses to build at the offending site rather than
            // silently miscompiling.
            other => format!(
                "(/- TODO: sys::{} not modelled in Lean -/ Cambrian.Unsupported)",
                other,
            ),
        }
    }

    fn balance_world_update(&self, world: &str, entity: &str, inst: &str, value: &str) -> String {
        // `sys::balance` reads `w.balances (E.address inst)`, so a
        // quantified/pinned starting balance is injected by pointwise-
        // updating the `balances` function at the entity's own address.
        format!(
            "{{ {w} with balances := fun a => if a = {e}.address {inst} then {v} else {w}.balances a }}",
            w = world,
            e = entity,
            inst = inst,
            v = value,
        )
    }

    fn entity_address_term(&self, entity: &str, id_arg_terms: &[String]) -> String {
        let id_ctor = format!("({}.Identity.mk {})", entity, id_arg_terms.join(" "));
        format!("({}.address {})", entity, id_ctor)
    }

    fn trace_field(&self, field: &str, trace_acc_var: Option<&str>) -> String {
        // Lower a `trace::length` accessor against the in-scope `TraceAcc`
        // accumulator (`<acc>.len`). Falls back to a build-breaking placeholder
        // when no accumulator is in scope.
        match (trace_acc_var, field) {
            (Some(acc), "length") => format!("{}.len", acc),
            _ => format!(
                "(/- trace::{} has no Lean lowering here -/ Cambrian.Unsupported)",
                field,
            ),
        }
    }

    fn trace_call(&self, name: &str, route: &str, trace_acc_var: Option<&str>) -> String {
        // Lower a `trace::count(route)` / `trace::lastWas(route)` accessor
        // against the in-scope `TraceAcc` accumulator.
        match (trace_acc_var, name) {
            (Some(acc), "count") => format!("{}.count_{}", acc, route),
            (Some(acc), "lastWas") => format!("({}.last == LastAction.{})", acc, route),
            _ => format!(
                "(/- trace::{}({}) has no Lean lowering here -/ Cambrian.Unsupported)",
                name, route,
            ),
        }
    }
}
