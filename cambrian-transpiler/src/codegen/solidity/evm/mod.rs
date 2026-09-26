// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM/Solidity adapter — entities, routes, factory, emitter.
//!
//! ## CEI ordering (adapter policy)
//!
//! Route bodies follow **Checks → Effects → Interactions**:
//! 1. **Checks** — route-level / per-phase `where` clauses and `from` sender verification
//! 2. **Effects** — member transforms / SSTORE (`gen_member_updates*`)
//! 3. **Interactions** — sends, deploys, external calls (`gen_action*`)
//!
//! Unphased routes emit checks, then all member updates, then all actions.
//! Phased routes repeat (checks → updates → actions) per phase.

pub mod analysis;
pub mod emitter;
pub mod entity;
pub mod expr;
pub mod factory;
pub mod interface;
pub mod route;
pub mod transform;

use crate::ast::{Program, Type};
use std::cell::RefCell;

use crate::codegen::solidity::core::ctx::EvmCtx;
use crate::codegen::solidity::core::scratch::EmitScratch;
use crate::codegen::solidity::core::option::{
    gen_option_structs_for_named, gen_option_structs_primitive,
};
use crate::codegen::solidity::core::state::is_payload_enum;
use crate::codegen::solidity::core::types::{gen_checked_downcast_helpers, sol_type, sol_type_entity};

pub use emitter::EvmActionEmitter;

use crate::codegen::solidity::core::library::gen_program_libraries;
use crate::codegen::solidity::core::pure::{gen_program_pure_fns, gen_stdlib_helpers};
use emitter::collect_typed_addr_refs_from_action;
use entity::{gen_entity_contract, gen_payload_enum_struct};
use factory::{gen_factory_contract, gen_factory_interface};
use interface::{gen_extern_interface_decl, gen_interface_decl};

pub fn gen_evm_solidity(program: &Program, deterministic: bool) -> String {
    gen_evm_solidity_opts(program, deterministic, true)
}

pub fn gen_evm_solidity_opts(
    program: &Program,
    deterministic: bool,
    allow_constructor_payable: bool,
) -> String {
    let expanded = crate::codegen::solidity::core::alias::expand_type_aliases(program);
    let program: &Program = &expanded;
    let ctx = EvmCtx::build(program, deterministic, allow_constructor_payable);
    let scratch = RefCell::new(EmitScratch::new());
    let mut out = String::new();
    // Generated Solidity is the user's. UNLICENSED = no extra license from
    // the compiler (see README / docs/STDLIB.md).
    out.push_str("// SPDX-License-Identifier: UNLICENSED\n");
    out.push_str("pragma solidity ^0.8.24;\n\n");

    let mut sol_imports: Vec<&str> = program
        .extern_entities
        .iter()
        .filter_map(|e| e.solidity_import.as_deref())
        .collect();
    sol_imports.sort();
    sol_imports.dedup();
    if !sol_imports.is_empty() {
        for path in &sol_imports {
            out.push_str(&format!("import \"{}\";\n", path));
        }
        out.push('\n');
    }

    let defined_entity_names: Vec<&str> =
        program.entities.iter().map(|e| e.name.as_str()).collect();

    let mut ext_interfaces: Vec<String> = vec![];
    let mut int_interfaces: Vec<String> = vec![];

    let push_iface =
        |en: &String, defined: &[&str], ext: &mut Vec<String>, int: &mut Vec<String>| {
            if defined.contains(&en.as_str()) {
                if !int.contains(en) {
                    int.push(en.clone());
                }
            } else if !ext.contains(en) {
                ext.push(en.clone());
            }
        };

    for entity in &program.entities {
        for route in &entity.routes {
            for param in &route.params {
                if let Type::TypedAddress(en) = &param.ty {
                    push_iface(
                        en,
                        &defined_entity_names,
                        &mut ext_interfaces,
                        &mut int_interfaces,
                    );
                }
            }
            let actions = route.body.all_actions();
            for action in actions {
                collect_typed_addr_refs_from_action(
                    action,
                    entity,
                    route,
                    &defined_entity_names,
                    &mut ext_interfaces,
                    &mut int_interfaces,
                );
            }
        }
        for member in &entity.members {
            if let Type::TypedAddress(en) = &member.ty {
                push_iface(
                    en,
                    &defined_entity_names,
                    &mut ext_interfaces,
                    &mut int_interfaces,
                );
            }
        }
    }

    if deterministic {
        out.push_str(&gen_factory_interface(program, &ctx, &scratch));
        out.push('\n');
    }

    for iface_name in &ext_interfaces {
        if let Some(ext) = program
            .extern_entities
            .iter()
            .find(|e| e.name == *iface_name)
        {
            if ext.solidity_import.is_some() {
                continue;
            }
            out.push_str(&gen_extern_interface_decl(ext));
            out.push('\n');
        } else {
            debug_assert!(
                false,
                "EVM-6 M1: typed send to undeclared external entity '{}' should have been rejected by validator E22",
                iface_name,
            );
            out.push_str(&format!(
                "interface I{} {{\n    // EVM-6 M1: external entity '{}' is not declared via `extern entity {} {{ ... }}`; this stub will not compile\n}}\n\n",
                iface_name, iface_name, iface_name
            ));
        }
    }

    for iface_name in &int_interfaces {
        if let Some(target_entity) = program.entities.iter().find(|e| e.name == *iface_name) {
            out.push_str(&gen_interface_decl(iface_name, target_entity, &ctx));
            out.push('\n');
        }
    }

    out.push_str(&gen_option_structs_primitive(program, &ctx));

    let graphs = crate::analysis::ProgramGraphs::build(program);
    let program_scope = crate::codegen::solidity::core::library::synthetic_entity("program");
    for item in graphs.resolve_program_types(program) {
        match item {
            crate::analysis::TypeItem::Record(rec) => {
                out.push_str(&format!("struct {} {{\n", rec.name));
                for field in &rec.fields {
                    out.push_str(&format!(
                        "    {} {};\n",
                        sol_type_entity(&program_scope, &field.ty, false, &ctx),
                        field.name
                    ));
                }
                out.push_str("}\n\n");
                out.push_str(&gen_option_structs_for_named(program, &ctx, &rec.name));
            }
            crate::analysis::TypeItem::Enum(e) => {
                if is_payload_enum(e) {
                    out.push_str(&gen_payload_enum_struct(e, ""));
                    out.push('\n');
                } else {
                    let variants: Vec<&str> = e.variants.iter().map(|v| v.name.as_str()).collect();
                    out.push_str(&format!(
                        "enum {} {{ {} }}\n\n",
                        e.name,
                        variants.join(", ")
                    ));
                }
                out.push_str(&gen_option_structs_for_named(program, &ctx, &e.name));
            }
        }
    }

    for ev in &program.events {
        let params: Vec<String> = ev
            .params
            .iter()
            .map(|p| {
                let ty = sol_type(&p.ty, true);
                if p.indexed {
                    format!("{} indexed {}", ty, p.name)
                } else {
                    format!("{} {}", ty, p.name)
                }
            })
            .collect();
        out.push_str(&format!("event {}({});\n", ev.name, params.join(", ")));
    }
    if !program.events.is_empty() {
        out.push('\n');
    }

    for er in &program.errors {
        let params: Vec<String> = er
            .params
            .iter()
            .map(|p| {
                let ty = sol_type(&p.ty, true);
                format!("{} {}", ty, p.name)
            })
            .collect();
        out.push_str(&format!("error {}({});\n", er.name, params.join(", ")));
    }
    if !program.errors.is_empty() {
        out.push('\n');
    }

    out.push_str(&gen_stdlib_helpers(program));
    out.push_str(gen_checked_downcast_helpers());
    if entity::pure_code_uses_wrapping_ops(program) {
        out.push_str(&entity::gen_wrapping_op_helpers(true));
    }
    out.push_str(&gen_program_libraries(program, &ctx, &scratch));
    out.push_str(&gen_program_pure_fns(program, &ctx, &scratch));

    for entity in &program.entities {
        out.push_str(&gen_entity_contract(entity, program, &ctx, &scratch));
        out.push('\n');
    }

    if deterministic {
        out.push_str(&gen_factory_contract(program, &ctx, &scratch));
        out.push('\n');
    }

    out
}
