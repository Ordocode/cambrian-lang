// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! LeanCore emission of entity `State` / `Identity` / `State.default`.
//!
//! Pure structural surface — address / CREATE2 derivation stays in the
//! Lean-EVM adapter (`evm/address.rs`).

use std::collections::HashSet;

use crate::ast::{Entity, Member, Program, Type};

use super::super::expr::{gen_expr, LeanExprCtx};
use super::super::LeanProfile;
use super::deceq;
use super::emitter::{doc_comment, push_deriving, push_indent};
use super::map_analysis::hashmap_members_needing_keys;
use super::types::{default_for_type, lower_type, LeanTypeCtx};

/// Emit `<Entity>.State` with all members (identity + non-identity).
pub fn emit_state_struct(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    profile: LeanProfile,
) {
    doc_comment(out, 0, "Entity state. Identity members live alongside non-identity ones; the `Identity` projection picks them out for address derivation in P3.");
    out.push_str("structure State where\n");
    let ctx = LeanTypeCtx::for_entity(
        program,
        &entity.name,
        &entity.records,
        &entity.enums,
        &entity.type_aliases,
        profile,
    );
    // Lean 4 accepts empty `structure Foo where deriving …` —
    // no placeholder field needed. Constructor stays `State.mk`.
    let iterated_maps = hashmap_members_needing_keys(entity);
    for m in &entity.members {
        push_indent(out, 1);
        out.push_str(&format!("{} : {}\n", m.name, lower_type(&m.ty, &ctx)));
        if let Type::Generic(name, params) = &m.ty {
            if name == "HashMap" && params.len() == 2 && iterated_maps.contains(&m.name) {
                push_indent(out, 1);
                out.push_str(&format!(
                    "{}_keys : List ({})\n",
                    m.name,
                    lower_type(&params[0], &ctx),
                ));
            }
        }
    }
    push_deriving(out, ctx.use_predictable_profile, &["Repr"]);
}

/// Emit `<Entity>.Identity`, `State.identity`, and escrow `DecidableEq` if needed.
pub fn emit_identity_struct(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    profile: LeanProfile,
) {
    let identity_members: Vec<&Member> = entity.members.iter().filter(|m| m.is_identity).collect();
    doc_comment(out, 0, "Identity members projected from `State`. P3 composes this with `Entity.address` and uses it as the storage-map key in `Cambrian.Generated.World`.");
    out.push_str("structure Identity where\n");
    let ctx = LeanTypeCtx::for_entity(
        program,
        &entity.name,
        &entity.records,
        &entity.enums,
        &entity.type_aliases,
        profile,
    );
    for m in &identity_members {
        push_indent(out, 1);
        out.push_str(&format!("{} : {}\n", m.name, lower_type(&m.ty, &ctx)));
    }
    // P3.2: `DecidableEq` is required so World storage's
    // `fun id' => if id' = id then ... else ...` lookup path is
    // decidable, and so user proofs that case-split on `instance`
    // work natively. Singletons (empty Identity) derive it trivially.
    // Escrow (B3): drop `deriving DecidableEq`, emit an explicit term-level
    // `instDecidableEqIdentity` (byte-identical kernel name).
    let derives = deceq::deceq_filtered(ctx.use_predictable_profile, true, &["Repr", "DecidableEq"]);
    push_deriving(out, ctx.use_predictable_profile, &derives);
    let field_names: Vec<String> = identity_members.iter().map(|m| m.name.clone()).collect();
    crate::codegen::lean::evm::maybe_emit_struct_deceq(
        out,
        ctx.use_predictable_profile,
        "Identity",
        &field_names,
    );

    doc_comment(out, 0, "Project the identity members out of `State`. Singleton when no identity members are declared.");
    out.push_str("def State.identity (s : State) : Identity :=\n");
    if identity_members.is_empty() {
        // Empty `structure Identity where deriving Repr` has a
        // 0-arity constructor `Identity.mk`. Use it explicitly —
        // `{}` works too but only when Lean can infer the type, and
        // we don't want to depend on inference inside member-update
        // contexts later (P3).
        out.push_str("  Identity.mk\n\n");
    } else {
        out.push_str("  {");
        let parts: Vec<String> = identity_members
            .iter()
            .map(|m| format!(" {} := s.{}", m.name, m.name))
            .collect();
        out.push_str(&parts.join(","));
        out.push_str(" }\n\n");
    }
    let _ = ctx;
}

/// Emit `<Entity>.State.default`.
pub fn emit_state_default(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    profile: LeanProfile,
) {
    let type_ctx = LeanTypeCtx::for_entity(
        program,
        &entity.name,
        &entity.records,
        &entity.enums,
        &entity.type_aliases,
        profile,
    );
    let expr_ctx = LeanExprCtx {
        type_ctx: LeanTypeCtx {
            program: type_ctx.program,
            proof_helpers: type_ctx.proof_helpers,
            use_nat_numerics: type_ctx.use_nat_numerics,
            overflow_panic: type_ctx.overflow_panic,
            use_predictable_profile: type_ctx.use_predictable_profile,
            deterministic_addresses: type_ctx.deterministic_addresses,
            entity_name: type_ctx.entity_name,
            local_records: type_ctx.local_records,
            local_enums: type_ctx.local_enums,
            local_aliases: type_ctx.local_aliases,
        },
        entity,
        local_enums: &entity.enums,
        pure_fns: &program.pure_fns,
        lets: HashSet::new(),
        route_params: HashSet::new(),
        route_name: None,
        phase_name: None,
        route_arg_names: vec![],
        in_transform: false,
        state_var: "s".to_string(),
        derived_helpers: HashSet::new(),
        world_var: None,
        instance_var: None,
        qualified_instances: std::collections::HashMap::new(),
        deploy_bindings: std::collections::HashMap::new(),
        hashmap_idents: std::collections::HashSet::new(),
        nat_idents: std::collections::HashSet::new(),
        trace_acc_var: None,
        expected_bitvec_width: None,
        expected_signed: None,
        expected_collection: None,
        msg_sender_override: None,
        pure_fn: None,
    };
    doc_comment(out, 0, "Canonical default state. Identity members default the same way non-identity members do; deployment-time values overwrite them via the constructor route.");
    out.push_str("def State.default : State :=\n");
    if entity.members.is_empty() {
        out.push_str("  State.mk\n\n");
        return;
    }
    out.push_str("  {");
    let iterated_maps = hashmap_members_needing_keys(entity);
    let mut parts: Vec<String> = Vec::new();
    for m in &entity.members {
        parts.push(match &m.default_value {
            Some(expr) => format!(" {} := {}", m.name, gen_expr(expr, &expr_ctx)),
            None => format!(" {} := {}", m.name, default_for_type(&m.ty, &type_ctx)),
        });
        if let Type::Generic(name, params) = &m.ty {
            if name == "HashMap" && params.len() == 2 && iterated_maps.contains(&m.name) {
                let _ = params;
                parts.push(format!(" {}_keys := []", m.name));
            }
        }
    }
    out.push_str(&parts.join(","));
    out.push_str(" }\n\n");
}
