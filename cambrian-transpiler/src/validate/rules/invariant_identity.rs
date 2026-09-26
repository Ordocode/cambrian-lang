// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! I18: multi-instance invariants must give each on-chain instance of the same
//! entity a distinct CREATE2 identity tuple (U4-4c). EVM + Lean; revm backlog.

use std::collections::{HashMap, HashSet};

use crate::ast::{Entity, Expr, InvariantDecl, InvariantInstance};
use crate::codegen::evm_test_codegen::instance_deploy_order_report;

use super::super::registry::ValidateCtx;
use super::super::Diagnostic;

pub const CODES: &[&str] = &["I18"];

pub fn run(ctx: &ValidateCtx<'_>, diags: &mut Vec<Diagnostic>) {
    for inv in &ctx.program.invariants {
        if inv.is_single_entity() {
            continue;
        }
        check_invariant_instance_identity(inv, &ctx.program.entities, diags);
    }
}

fn check_invariant_instance_identity(
    inv: &InvariantDecl,
    entities: &[Entity],
    diags: &mut Vec<Diagnostic>,
) {
    let instance_names: HashSet<&str> = inv.instances.iter().map(|i| i.name.as_str()).collect();
    let mut by_entity: HashMap<&str, Vec<&InvariantInstance>> = HashMap::new();
    for inst in &inv.instances {
        by_entity.entry(inst.entity.as_str()).or_default().push(inst);
    }

    for (entity_name, instances) in &by_entity {
        if instances.len() < 2 {
            continue;
        }
        let entity = entities.iter().find(|e| e.name == *entity_name);
        let Some(entity) = entity else {
            continue;
        };
        let identity_members: Vec<_> = entity.members.iter().filter(|m| m.is_identity).collect();
        if identity_members.is_empty() {
            diags.push(Diagnostic::error(
                "I18",
                format!(
                    "invariant \"{}\": entity '{}' has {} instances but no identity members — \
                     distinct CREATE2 addresses require `identity` fields (pin distinct values per instance)",
                    inv.name,
                    entity_name,
                    instances.len()
                ),
            ));
            continue;
        }

        let mut seen_keys: HashMap<String, &str> = HashMap::new();
        for inst in instances {
            match resolve_identity_tuple_key(entity, &inst.init, &instance_names) {
                Ok(key) => {
                    if let Some(other) = seen_keys.get(&key) {
                        diags.push(Diagnostic::error(
                            "I18",
                            format!(
                                "invariant \"{}\": instances '{}' and '{}' of entity '{}' \
                                 share the same identity tuple ({}) — pin distinct identity values",
                                inv.name,
                                other,
                                inst.name,
                                entity_name,
                                key
                            ),
                        ));
                    } else {
                        seen_keys.insert(key, inst.name.as_str());
                    }
                }
                Err(msg) => {
                    diags.push(Diagnostic::error(
                        "I18",
                        format!(
                            "invariant \"{}\": instance '{}' of entity '{}': {}",
                            inv.name,
                            inst.name,
                            entity_name,
                            msg
                        ),
                    ));
                }
            }
        }
    }

    let entity_for_inst: Vec<(String, &Entity)> = inv
        .instances
        .iter()
        .filter_map(|inst| {
            entities
                .iter()
                .find(|e| e.name == inst.entity)
                .map(|e| (inst.name.clone(), e))
        })
        .collect();
    if entity_for_inst.len() < 2 {
        return;
    }
    let inits: Vec<&[(String, Expr)]> = inv
        .instances
        .iter()
        .map(|i| i.init.as_slice())
        .collect();
    let report = instance_deploy_order_report(&entity_for_inst, &inits);
    if !report.topological {
        diags.push(Diagnostic::error(
            "I18",
            format!(
                "invariant \"{}\": instance deploy-order graph has a cycle through identity/sibling edges",
                inv.name
            ),
        ));
    }
}

fn resolve_identity_tuple_key(
    entity: &Entity,
    init: &[(String, Expr)],
    instance_names: &HashSet<&str>,
) -> Result<String, String> {
    let mut parts = Vec::new();
    for m in entity.members.iter().filter(|m| m.is_identity) {
        let part = if let Some((_, expr)) = init.iter().find(|(n, _)| n == &m.name) {
            identity_pin_key(expr, instance_names)?
        } else {
            format!("default({})", default_identity_key(&m.ty))
        };
        parts.push(part);
    }
    Ok(parts.join(","))
}

fn identity_pin_key(expr: &Expr, instance_names: &HashSet<&str>) -> Result<String, String> {
    match expr {
        Expr::IntLiteral(n) => Ok(format!("i:{n}")),
        Expr::BoolLiteral(b) => Ok(format!("b:{b}")),
        Expr::StringLiteral(s) => Ok(format!("s:{s}")),
        Expr::BytesLiteral(b) => Ok(format!("bytes:{b:?}")),
        Expr::Ident(name) if instance_names.contains(name.as_str()) => {
            Ok(format!("@inst:{name}"))
        }
        Expr::Ident(name) => Ok(format!("id:{name}")),
        _ => Err(
            "identity member init pin must be a literal, constant identifier, or sibling instance name"
                .to_string(),
        ),
    }
}

fn default_identity_key(ty: &crate::ast::Type) -> String {
    use crate::ast::Type;
    match ty {
        Type::Simple(name) => match name.as_str() {
            "u8" | "u16" | "u32" | "u64" | "u128" | "U256" | "i8" | "i16" | "i32" | "i64"
            | "i128" => "0".to_string(),
            "bool" => "false".to_string(),
            "address" | "pubkey" => "0".to_string(),
            other => other.to_string(),
        },
        Type::Generic(name, _) => name.clone(),
        Type::TypedAddress(entity) => format!("addr({entity})"),
        Type::Tuple(ts) => format!(
            "({})",
            ts.iter()
                .map(default_identity_key)
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}
