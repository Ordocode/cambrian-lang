// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean codegen — `deploy Entity(args)` lowering (P4a.4).

use std::collections::HashSet;

use crate::ast::{Entity, Expr, Member, Program};

use super::super::expr::{gen_expr, LeanExprCtx};
use super::world::{entity_deployed_field_name, entity_field_name};
use crate::codegen::{collect_transforms, order_transforms_temporally};

/// Lower `deploy Target(args)` inside a world-threaded route body.
/// Installs `(id, state)` into `w.storage.<target>` and binds
/// `_deployed_addr` to `Target.address id`.
///
/// Matches EVM CREATE2 occupancy: a second deploy at the same identity
/// throws (ThrowCode `91`) rather than silently overwriting (T-X-001).
/// Non-identity constructor args are projected into `state_init` via the
/// target's init-route member transforms (T-X-003).
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_deploy(
    program: &Program,
    caller_entity: &Entity,
    target_name: &str,
    constructor_args: &[Expr],
    send_options: Option<&Expr>,
    ctx: &LeanExprCtx<'_>,
    fail_mode: bool,
) -> String {
    let Some(target) = program.entities.iter().find(|e| e.name == target_name) else {
        return format!(
            "-- deploy: unknown entity '{}' (validator should have caught this)\n",
            target_name,
        );
    };

    let id_fields: Vec<&Member> = target.members.iter().filter(|m| m.is_identity).collect();
    let id_count = id_fields.len();
    let id_record = if id_fields.is_empty() {
        "{}".to_string()
    } else {
        let pieces: Vec<String> = id_fields
            .iter()
            .zip(constructor_args.iter())
            .map(|(m, e)| format!("{} := {}", m.name, gen_expr(e, ctx)))
            .collect();
        format!("{{ {} }}", pieces.join(", "))
    };

    // Bind init-route parameters so transform bodies can reference them
    // as bare Lean identifiers (T-X-003).
    let init_route = target
        .routes
        .iter()
        .find(|r| r.is_init)
        .or_else(|| target.routes.iter().find(|r| r.name == "constructor"));
    let init_args: &[Expr] = constructor_args.get(id_count..).unwrap_or(&[]);
    let mut init_lets = String::new();
    let mut init_param_names: HashSet<String> = HashSet::new();
    if let Some(init) = init_route {
        for (param, arg) in init.params.iter().zip(init_args.iter()) {
            let pname = super::super::core::types::lean_safe_ident(&param.name);
            init_lets.push_str(&format!("let {} := {}\n", pname, gen_expr(arg, ctx)));
            init_param_names.insert(param.name.clone());
        }
    }

    let mut state_fields: Vec<String> = Vec::new();
    for m in &target.members {
        if m.is_identity {
            if let Some(arg) = constructor_args
                .iter()
                .zip(id_fields.iter())
                .find(|(_, mem)| mem.name == m.name)
                .map(|(e, _)| e)
            {
                state_fields.push(format!("{} := {}", m.name, gen_expr(arg, ctx)));
            }
        }
    }

    // Project init-route member transforms into `state_init`.
    if let Some(init) = init_route {
        let (orders, _) = crate::validate::build_temporal_orders(target);
        let transforms = order_transforms_temporally(
            collect_transforms(target, &init.name, None),
            &orders,
            &init.name,
        );
        let deploy_sender = format!("{}.address inst", caller_entity.name);
        let member_ctx = LeanExprCtx::for_spec(
            program,
            target,
            "s",
            init_param_names.clone(),
            init_param_names,
            ctx.type_ctx.profile(),
        )
        .with_msg_sender_override(deploy_sender);
        for rt in transforms {
            state_fields.push(format!(
                "{} := {}",
                rt.member.name,
                gen_expr(&rt.transform.body, &member_ctx),
            ));
        }
    }

    let state_init = if state_fields.is_empty() {
        format!("{}.State.default", target.name)
    } else {
        format!(
            "{{ {}.State.default with {} }}",
            target.name,
            state_fields.join(", ")
        )
    };

    let field = entity_field_name(&caller_entity.name);
    let target_field = entity_field_name(&target.name);
    let deployed_field = entity_deployed_field_name(&target.name);
    let writeback = format!(
        "let w := Cambrian.Generated.World.with{} w inst s",
        caller_entity.name,
    );
    let id_let = format!("let id' : {}.Identity := {}", target.name, id_record);
    let addr_let = format!(
        "let _deployed_{} : Cambrian.Address := {}.address id'",
        target_field, target.name,
    );
    // CREATE2 collision: identity already live ⇒ revert (EVM ground truth).
    let occupancy = format!(
        "if w.storage.{} id' then throw (Cambrian.ThrowCode.ofNat 91)",
        deployed_field,
    );
    let install = format!(
        "let w := Cambrian.Generated.World.with{} w id' {}",
        target.name, state_init,
    );
    let reread = format!("let s := w.storage.{} inst", field);

    // `send_options` is the whole `{ value: v }` record; extract the value
    // expression before widening (same as typed Send / VarCall).
    // Fund the target before occupancy/install (W2-BC-01 / T-ARCH-011).
    let deploy_ctx_let = format!(
        "let deploy_ctx := {{ ctx with sender := {}.address inst }}\n",
        caller_entity.name,
    );
    let deploy_core = if let Some(v) = super::send::extract_send_value(send_options) {
        let value_term = super::send::widen_send_value(&v, ctx);
        // Keep the `WorldState.call` bind on one physical line so the
        // predictable-profile B4 rewriter can classify the `(w, _)` tuple
        // bind (multi-line `fun w => do` bodies punt as unclassified).
        let deploy_ctx = format!(
            "let deploy_ctx := {{ ctx with sender := {}.address inst }}",
            caller_entity.name,
        );
        let occupancy_guard = format!(
            "(if w.storage.{} id' then throw (Cambrian.ThrowCode.ofNat 91) else pure ())",
            deployed_field,
        );
        let call_body = format!(
            "fun w => (do {}; {}; {}; pure (w, ()))",
            deploy_ctx, occupancy_guard, install.trim(),
        );
        let call_params = format!(
            "let callParams : Cambrian.WorldState.CallParams := {{ to := {}.address id', src := {}.address inst, value := {} }}\n",
            target.name, caller_entity.name, value_term,
        );
        if fail_mode {
            format!(
                "{call_params}let (w, _) ← Cambrian.WorldState.call w callParams {call_body}\n",
            )
        } else {
            format!(
                "{call_params}let w := (Cambrian.exceptGetD (Cambrian.WorldState.call w callParams {call_body}) (w, ())).fst\n",
            )
        }
    } else {
        format!("{}{}\n{}\n", deploy_ctx_let, occupancy, install)
    };

    format!(
        "{}\n{}{}\n{}\n{}{}",
        writeback, init_lets, id_let, addr_let, deploy_core, reread,
    )
}
