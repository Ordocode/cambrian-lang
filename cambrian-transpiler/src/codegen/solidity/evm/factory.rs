// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM codegen — deterministic-mode factory contract + CREATE2 helpers.

use super::route::{extract_send_option, find_init_route, has_non_identity_init_params};
use crate::ast::{Entity, Expr, Member, Program};
use crate::codegen::solidity::core::ctx::{EmitScope, EvmCtx};
use crate::codegen::solidity::core::expr::gen_expr;
use crate::codegen::solidity::core::scratch::EmitScratch;
use crate::codegen::solidity::core::types::*;
use std::cell::RefCell;

/// Generate deploy action that goes through the factory.
pub(super) fn gen_deploy_via_factory(
    target_entity_name: &str,
    send_options: Option<&Expr>,
    constructor_args: &[Expr],
    entity: &Entity,
    _program: &Program,
    ctx: &EvmCtx,
    scratch: &RefCell<EmitScratch>,
) -> String {
    let scope = EmitScope::for_entity(entity);
    let value_expr = extract_send_option(send_options, "value")
        .and_then(|e| gen_expr(e, ctx, &scope, scratch))
        .unwrap_or_else(|| "0".to_string());
    let arg_strs: Vec<String> = constructor_args
        .iter()
        .map(|a| gen_expr(a, ctx, &scope, scratch).unwrap_or_else(|| "0".to_string()))
        .collect();
    let args_str = arg_strs.join(", ");
    let fn_name = format!("deploy{}", target_entity_name);
    // EVM-H13: unique temp per deploy so two `deploy Vault` in one route compile.
    let tmp = format!(
        "_deployed_{}_{}",
        target_entity_name.to_lowercase(),
        scratch.borrow_mut().next_tmp().trim_start_matches('_')
    );

    if value_expr == "0" {
        format!(
            "        address {} = ICambrianFactory(_factory).{}({});\n",
            tmp, fn_name, args_str
        )
    } else {
        format!(
            "        address {} = ICambrianFactory(_factory).{}{{value: {}}}({});\n",
            tmp, fn_name, value_expr, args_str
        )
    }
}

// ---------------------------------------------------------------------------
// Factory generation
// ---------------------------------------------------------------------------

/// Generate the ICambrianFactory interface.
pub(super) fn gen_factory_interface(
    program: &Program,
    ctx: &EvmCtx,
    _scratch: &RefCell<EmitScratch>,
) -> String {
    let mut out = "interface ICambrianFactory {\n".to_string();

    for entity in &program.entities {
        let identity_params: Vec<String> = entity
            .members
            .iter()
            .filter(|m| m.is_identity)
            .map(|m| format!("{} {}_", sol_type_entity(entity, &m.ty, true, ctx), m.name))
            .collect();

        let init_params: Vec<String> = find_init_route(entity)
            .map(|r| {
                r.params
                    .iter()
                    .map(|p| {
                        format!(
                            "{} {}",
                            sol_type_entity(entity, &p.ty, true, ctx),
                            sol_sanitize_ident(&p.name)
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        let all_deploy_params: Vec<&str> = identity_params
            .iter()
            .map(|s| s.as_str())
            .chain(init_params.iter().map(|s| s.as_str()))
            .collect();

        out.push_str(&format!(
            "    function deploy{}({}) external payable returns (address);\n",
            entity.name,
            all_deploy_params.join(", ")
        ));

        let predict_params = identity_params.join(", ");
        out.push_str(&format!(
            "    function predict{}({}) external view returns (address);\n",
            entity.name, predict_params
        ));
    }

    out.push_str("}\n");
    out
}

/// Generate the CambrianFactory contract.
pub(super) fn gen_factory_contract(
    program: &Program,
    ctx: &EvmCtx,
    _scratch: &RefCell<EmitScratch>,
) -> String {
    let mut out = "contract CambrianFactory {\n".to_string();
    out.push_str("    event Deployed(string entityType, address instance);\n\n");
    // Only the factory owner (bootstrap / tests) or contracts previously
    // deployed by this factory may call deployX — EOAs cannot occupy
    // identity-only CREATE2 slots (EVM-H2).
    out.push_str("    address public owner;\n");
    out.push_str("    mapping(address => bool) public isDeployed;\n\n");
    out.push_str("    constructor() {\n");
    out.push_str("        owner = msg.sender;\n");
    out.push_str("    }\n\n");

    for entity in &program.entities {
        let identity_members: Vec<&Member> =
            entity.members.iter().filter(|m| m.is_identity).collect();

        let identity_params: Vec<String> = identity_members
            .iter()
            .map(|m| format!("{} {}_", sol_type_entity(entity, &m.ty, true, ctx), m.name))
            .collect();

        let init_params: Vec<String> = find_init_route(entity)
            .map(|r| {
                r.params
                    .iter()
                    .map(|p| {
                        format!(
                            "{} {}",
                            sol_type_entity(entity, &p.ty, true, ctx),
                            factory_init_param(&p.name)
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        let all_deploy_params: Vec<&str> = identity_params
            .iter()
            .map(|s| s.as_str())
            .chain(init_params.iter().map(|s| s.as_str()))
            .collect();

        // deploy function
        out.push_str(&format!(
            "    function deploy{}({}) external payable returns (address) {{\n",
            entity.name,
            all_deploy_params.join(", ")
        ));
        out.push_str(
            "        require(msg.sender == owner || isDeployed[msg.sender], \"CambrianFactory: unauthorized\");\n",
        );

        // Constructor call: forward msg.value to the CREATE2 instance (EVM-H1).
        let ctor_args: Vec<String> = std::iter::once("address(this)".to_string())
            .chain(identity_members.iter().map(|m| format!("{}_", m.name)))
            .collect();

        let create_value = if ctx.allow_constructor_payable {
            "msg.value"
        } else {
            "0"
        };
        out.push_str(&format!(
            "        {} _instance = new {}{{salt: bytes32(0), value: {}}}({});\n",
            entity.name,
            entity.name,
            create_value,
            ctor_args.join(", ")
        ));

        // Call initialize() if needed
        let needs_init = has_non_identity_init_params(entity);
        if needs_init {
            let init_route = find_init_route(entity).unwrap();
            let init_arg_names: Vec<String> =
                init_route.params.iter().map(|p| factory_init_param(&p.name)).collect();
            out.push_str(&format!(
                "        _instance.initialize({});\n",
                init_arg_names.join(", ")
            ));
        }

        out.push_str("        isDeployed[address(_instance)] = true;\n");
        out.push_str(&format!(
            "        emit Deployed(\"{}\", address(_instance));\n",
            entity.name
        ));
        out.push_str("        return address(_instance);\n");
        out.push_str("    }\n\n");

        // predict function
        let predict_params = identity_params.join(", ");
        out.push_str(&format!(
            "    function predict{}({}) external view returns (address) {{\n",
            entity.name, predict_params
        ));

        let mut abi_encode_args = vec!["address(this)".to_string()];
        abi_encode_args.extend(identity_members.iter().map(|m| format!("{}_", m.name)));

        out.push_str(
            "        // EIP-1014 CREATE2: keccak256(0xff || factory || salt || initcodehash)\n",
        );
        out.push_str(&format!(
            "        return address(uint160(uint256(keccak256(abi.encodePacked(\n\
             \x20           bytes1(0xff), address(this), bytes32(0),\n\
             \x20           keccak256(abi.encodePacked(type({}).creationCode, abi.encode({})))\n\
             \x20       )))));\n",
            entity.name,
            abi_encode_args.join(", ")
        ));

        out.push_str("    }\n\n");
    }

    out.push_str("}\n");
    out
}

/// Init-route parameter as a `deployX` parameter. Names of factory state and
/// locals get a `_` prefix: `owner` would otherwise turn the authorization
/// check into `msg.sender == <argument>`.
fn factory_init_param(name: &str) -> String {
    let name = sol_sanitize_ident(name);
    if matches!(name.as_str(), "owner" | "isDeployed" | "_instance") {
        format!("_{}", name)
    } else {
        name
    }
}
