// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Foundry test harness helpers for `CambrianFactory` deploy (BUG-U4 / U4-6 Step 1).
//!
//! Product factory contract codegen lives in `solidity/evm/factory.rs`; this module
//! only emits **calls** into the generated `_{project}_project.sol` factory.

/// Solidity local holding the auto-generated `CambrianFactory` (BUG-U4 U4-4).
pub const CAMBRIAN_FACTORY_VAR: &str = "_cambrianFactory";

pub fn project_sol_file(project_name: &str) -> String {
    format!("_{}_project.sol", project_name)
}

pub fn emit_deterministic_project_import(project_name: &str, out: &mut String) {
    out.push_str(&format!(
        "import \"../src/{}\";\n",
        project_sol_file(project_name)
    ));
}

pub fn emit_cambrian_factory_field(out: &mut String) {
    out.push_str(&format!(
        "    CambrianFactory internal {};\n",
        CAMBRIAN_FACTORY_VAR
    ));
}

pub fn emit_cambrian_factory_new(out: &mut String) {
    out.push_str(&format!(
        "        {} = new CambrianFactory();\n",
        CAMBRIAN_FACTORY_VAR
    ));
}

pub fn factory_deploy_fn_name(entity_name: &str) -> String {
    format!("deploy{}", entity_name)
}

pub fn factory_predict_fn_name(entity_name: &str) -> String {
    format!("predict{}", entity_name)
}

/// `_cambrianFactory.predict{Entity}(identity…)` for forward sibling refs (U4-4c).
pub fn format_factory_predict_call(entity_name: &str, identity_args: &[String]) -> String {
    let fn_name = factory_predict_fn_name(entity_name);
    if identity_args.is_empty() {
        format!("{}.{}()", CAMBRIAN_FACTORY_VAR, fn_name)
    } else {
        format!(
            "{}.{}({})",
            CAMBRIAN_FACTORY_VAR,
            fn_name,
            identity_args.join(", ")
        )
    }
}

/// `assert(address(_inst) == address(_cambrianFactory.predict{Entity}(…)))` after deploy.
pub fn emit_factory_predict_assert(
    entity_name: &str,
    var_name: &str,
    identity_args: &[String],
    out: &mut String,
) {
    let predict = format_factory_predict_call(entity_name, identity_args);
    out.push_str(&format!(
        "        assert(address({}) == address({}));\n",
        var_name, predict
    ));
}

/// `factory.deploy{Entity}(identity…, init…)` cast to the entity type.
pub fn emit_factory_deploy_entity(
    entity_name: &str,
    var_name: &str,
    deploy_args: &[String],
    out: &mut String,
) {
    let fn_name = factory_deploy_fn_name(entity_name);
    if deploy_args.is_empty() {
        out.push_str(&format!(
            "        {} = {}(address({}.{}()));\n",
            var_name,
            entity_name,
            CAMBRIAN_FACTORY_VAR,
            fn_name
        ));
    } else {
        out.push_str(&format!(
            "        {} = {}(address({}.{}({})));\n",
            var_name,
            entity_name,
            CAMBRIAN_FACTORY_VAR,
            fn_name,
            deploy_args.join(", ")
        ));
    }
}
