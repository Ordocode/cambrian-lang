// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Shared spec-theorem prefix: `let ctx` / `let inst` / `let w` in
//! dependency order.
//!
//! Tests, properties, and invariants all thread `(w, inst, ctx)` through
//! route calls. Binders are emitted before any fragment that might read
//! them so `assume msg::sender` / `init { m: sys::now }` cannot mention
//! an unbound identifier (T-LEAN-EX-010 / B-26 class).
//!
//! Shape:
//! ```text
//! ∀ …,
//!   let ctx  := seeded MsgCtx
//!   let inst := Identity          -- init exprs: no world_var
//!   let w    := World with state  -- init exprs: no world_var
//!   [let w   := block / balance overlay]
//!   (assume hyps) →
//!   body
//! ```

use std::collections::HashSet;

use crate::ast::{ContextSpec, Entity, Expr};

use super::super::core::emitter::push_indent;
use super::super::expr::{
    expr_needs_world, gen_expr, sys_balance_world_update, sys_block_field, LeanExprCtx,
};

/// Extra `∀` binders plus the `let ctx` / world-overlay lines.
pub(crate) struct CtxSetup {
    pub quant: Vec<(String, String)>,
    pub ctx_line: String,
    pub block_line: Option<String>,
    pub balance_line: Option<String>,
}

/// Allocate `desired`, then `desired_1`, … until unique in `used`.
pub(crate) fn alloc_unique(desired: &str, used: &mut HashSet<String>) -> String {
    let mut candidate = desired.to_string();
    let mut n = 0;
    while used.contains(&candidate) {
        n += 1;
        candidate = format!("{}_{}", desired, n);
    }
    used.insert(candidate.clone());
    candidate
}

/// Allocate `ctx_{desired}` (invariant naming; avoids colliding with
/// state-member binders).
pub(crate) fn alloc_ctx_var(desired: &str, used: &mut HashSet<String>) -> String {
    alloc_unique(&format!("ctx_{}", desired), used)
}

/// Build the initial `MsgCtx` / `BlockEnv` / balance seeds from a `ctx { … }`
/// block. `msg::` fields (and a clock mirror of `sys::now`/`timestamp`) seed
/// `let ctx`; `sys::{now,timestamp,chainid,block_number}` overlay `w.block`;
/// `sys::balance` overlays `w.balances` when `self_balance` is `Some`.
///
/// `name_var` is [`alloc_unique`] for properties (bare `sender`) and
/// [`alloc_ctx_var`] for invariants (`ctx_sender`).
pub(crate) fn ctx_setup(
    ctx_spec: &ContextSpec,
    pin_ctx: &LeanExprCtx<'_>,
    used: &mut HashSet<String>,
    self_balance: Option<(&str, &str)>,
    mut name_var: impl FnMut(&str, &mut HashSet<String>) -> String,
) -> CtxSetup {
    let mut quant: Vec<(String, String)> = Vec::new();
    let mut ctx_seeds: Vec<String> = Vec::new();
    let mut block_seeds: Vec<String> = Vec::new();
    let mut balance_line: Option<String> = None;

    for e in &ctx_spec.entries {
        match e.namespace.as_str() {
            "msg" => {
                let lean_field = e.field.as_str();
                match &e.value {
                    None => {
                        let var = name_var(&e.field, used);
                        quant.push((var.clone(), ctx_lean_type("msg", &e.field)));
                        ctx_seeds.push(format!("{} := {}", lean_field, var));
                    }
                    Some(v) => {
                        ctx_seeds.push(format!("{} := {}", lean_field, gen_expr(v, pin_ctx)));
                    }
                }
            }
            "sys" => {
                if let Some((block_field, ty)) = sys_block_field(&e.field) {
                    let val = match &e.value {
                        None => {
                            let var = name_var(&e.field, used);
                            quant.push((var.clone(), ty.to_string()));
                            var
                        }
                        Some(v) => gen_expr(v, pin_ctx),
                    };
                    block_seeds.push(format!("{} := {}", block_field, val));
                    // Clock mirror: init seeds without `world_var` read
                    // `ctx.timestamp` (B-26). `MsgCtx.chainId` is `U256` while
                    // `BlockEnv.chainId` is `BitVec 64`, so only timestamp is
                    // copied onto ctx.
                    if block_field == "timestamp" {
                        ctx_seeds.push(format!("timestamp := {}", val));
                    }
                } else if e.field == "balance" {
                    let val = match &e.value {
                        None => {
                            let var = name_var(&e.field, used);
                            quant.push((var.clone(), "Cambrian.U256".to_string()));
                            var
                        }
                        Some(v) => gen_expr(v, pin_ctx),
                    };
                    if let Some((ename, inst)) = self_balance {
                        balance_line = Some(format!(
                            "let w := {}\n",
                            sys_balance_world_update("w", ename, inst, &val),
                        ));
                    }
                } else if e.value.is_none() {
                    let var = name_var(&e.field, used);
                    quant.push((var, "Cambrian.U256".to_string()));
                }
            }
            _ => {}
        }
    }

    let ctx_line = if ctx_seeds.is_empty() {
        "let ctx : Cambrian.MsgCtx := Cambrian.MsgCtx.default\n".to_string()
    } else {
        format!(
            "let ctx : Cambrian.MsgCtx := {{ Cambrian.MsgCtx.default with {} }}\n",
            ctx_seeds.join(", "),
        )
    };
    let block_line = if block_seeds.is_empty() {
        None
    } else {
        Some(format!(
            "let w := {{ w with block := {{ w.block with {} }} }}\n",
            block_seeds.join(", "),
        ))
    };
    CtxSetup {
        quant,
        ctx_line,
        block_line,
        balance_line,
    }
}

fn ctx_lean_type(namespace: &str, field: &str) -> String {
    match (namespace, field) {
        ("msg", "sender") => "Cambrian.Address".to_string(),
        ("msg", "value") => "Cambrian.U256".to_string(),
        _ => "Cambrian.U256".to_string(),
    }
}

/// `let inst : <E>.Identity := …` — every identity member is filled
/// (init pin or `State.default` fallback). Test/property style.
pub(crate) fn format_init_instance(
    entity: &Entity,
    init: &[(String, Expr)],
    ctx: &LeanExprCtx<'_>,
) -> String {
    format_init_instance_named(entity, init, "inst", ctx)
}

pub(crate) fn format_init_instance_named(
    entity: &Entity,
    init: &[(String, Expr)],
    inst_var: &str,
    ctx: &LeanExprCtx<'_>,
) -> String {
    let id_members: Vec<&crate::ast::Member> =
        entity.members.iter().filter(|m| m.is_identity).collect();
    if id_members.is_empty() {
        return format!("let {} : {}.Identity := {{}}", inst_var, entity.name);
    }
    let pieces: Vec<String> = id_members
        .iter()
        .map(|m| {
            let init_expr = init.iter().find(|(n, _)| n == &m.name).map(|(_, e)| e);
            match init_expr {
                Some(e) => format!("{} := {}", m.name, gen_expr(e, ctx)),
                None => format!("{} := ({}.State.default).{}", m.name, entity.name, m.name),
            }
        })
        .collect();
    format!(
        "let {} : {}.Identity := {{ {} }}",
        inst_var,
        entity.name,
        pieces.join(", "),
    )
}

fn is_identity_member(entity: &Entity, name: &str) -> bool {
    entity
        .members
        .iter()
        .any(|m| m.is_identity && m.name == name)
}

/// One or two `let w` lines. World-only init fields (`sys::balance` /
/// `chainid` / `blockNumber`) cannot sit on the introducing `let w` RHS;
/// they rebind after a default world exists.
pub(crate) fn format_world_lets(
    entity: &Entity,
    init: &[(String, Expr)],
    init_ctx: &LeanExprCtx<'_>,
    world_ctx: &LeanExprCtx<'_>,
) -> Vec<String> {
    let needs_world = init
        .iter()
        .any(|(n, e)| !is_identity_member(entity, n) && expr_needs_world(entity, e));
    if !needs_world {
        return vec![format_init_world(entity, init, init_ctx)];
    }
    vec![
        format!(
            "let w : Cambrian.Generated.World := Cambrian.Generated.World.with{} Cambrian.Generated.World.default inst ({}.State.default)",
            entity.name, entity.name
        ),
        format_init_world_rebind(entity, init, world_ctx),
    ]
}

fn format_init_world(entity: &Entity, init: &[(String, Expr)], ctx: &LeanExprCtx<'_>) -> String {
    let state_expr = state_expr(entity, init, ctx);
    format!(
        "let w : Cambrian.Generated.World := Cambrian.Generated.World.with{} Cambrian.Generated.World.default inst ({})",
        entity.name, state_expr,
    )
}

fn format_init_world_rebind(
    entity: &Entity,
    init: &[(String, Expr)],
    ctx: &LeanExprCtx<'_>,
) -> String {
    let state_expr = state_expr(entity, init, ctx);
    format!(
        "let w := Cambrian.Generated.World.with{} w inst ({})",
        entity.name, state_expr,
    )
}

pub(crate) fn state_expr(entity: &Entity, init: &[(String, Expr)], ctx: &LeanExprCtx<'_>) -> String {
    state_expr_named(entity, init, "inst", ctx)
}

/// Like [`state_expr`], but identity members project from `inst_var` (e.g.
/// `t0_inst`) instead of the harness-default `inst` binder.
pub(crate) fn state_expr_named(
    entity: &Entity,
    init: &[(String, Expr)],
    inst_var: &str,
    ctx: &LeanExprCtx<'_>,
) -> String {
    let mut state_updates: Vec<String> = Vec::new();
    for m in &entity.members {
        if m.is_identity {
            state_updates.push(format!("{} := {}.{}", m.name, inst_var, m.name));
            continue;
        }
        if let Some((_, e)) = init.iter().find(|(n, _)| n == &m.name) {
            state_updates.push(format!("{} := {}", m.name, gen_expr(e, ctx)));
        }
    }
    if state_updates.is_empty() {
        format!("{}.State.default", entity.name)
    } else {
        format!(
            "{{ {}.State.default with {} }}",
            entity.name,
            state_updates.join(", "),
        )
    }
}

/// Emit `let ctx` / `let inst` / `let w` / overlays at `indent`.
pub(crate) fn emit_prefix_lets(
    out: &mut String,
    indent: usize,
    setup: &CtxSetup,
    inst_line: &str,
    world_lets: &[String],
) {
    push_indent(out, indent);
    out.push_str(&setup.ctx_line);
    push_indent(out, indent);
    out.push_str(inst_line);
    out.push('\n');
    for line in world_lets {
        push_indent(out, indent);
        out.push_str(line);
        if !line.ends_with('\n') {
            out.push('\n');
        }
    }
    if let Some(bl) = &setup.block_line {
        push_indent(out, indent);
        out.push_str(bl);
    }
    if let Some(bl) = &setup.balance_line {
        push_indent(out, indent);
        out.push_str(bl);
    }
}
