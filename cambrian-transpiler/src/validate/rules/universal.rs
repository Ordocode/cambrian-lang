// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Universal (target-less) validation cluster — V/T/I/W rules except events/errors/W7.

use crate::ast::Program;

use super::super::entity;
use super::super::registry::ValidateCtx;
use super::super::test_checks;
use super::super::warnings;
use super::super::{
    check_action_for_body, check_file_imports, check_imports, check_libraries,
    check_namespace_usage, check_temporal_dag, check_using_decls, check_var_call_scoping,
    set_program_pure_fn_names, Diagnostic,
};
use super::family_runners;

/// Codes this cluster may emit (events/errors/W7 owned elsewhere).
#[allow(dead_code)] // documentation / future registry helpers
pub const CODES: &[&str] = &[
    "V1", "V2", "V3", "V4", "V7", "V8", "V9", "V10", "V11", "V12", "V13", "V14", "V15", "V16",
    "V17", "V20", "V21", "V22", "V23", "V24", "V25", "V26", "V27", "V28", "V29", "V30", "V31",
    "V32", "V42", "V43", "V44", "V45", "V47", "V48", "V49", "V50", "V51", "V52", "V53",
    "V54", "V55", "V56", "V57", "V58", "V59", "V60", "V64", "V65", "V66", "V67", "V68", "V69", "V70", "T1", "T2", "T3", "T4", "T5", "T6", "T8", "T9", "T10", "T11",
    "T12", "T15", "T16", "T17", "T18", "T19", "T20", "T21", "T22", "T23", "T24", "T25", "T26",
    "T27", "T28", "T29", "T30", "T31", "T32", "T33", "T37", "T38", "I1", "I2", "I3", "I4", "I5", "I6",
    "I8",
    "I9", "I10", "I11", "I12", "I13", "I14", "I15", "I16", "W1", "W2", "W3", "W4", "W5", "W6",
    "W8", "W9", "W11", "W12", "W13", "W14", "W15",
];

pub fn collect(ctx: &ValidateCtx<'_>, diags: &mut Vec<Diagnostic>) {
    let program: &Program = ctx.program;
    set_program_pure_fn_names(program);

    let state = entity::pure_fn_state_names(program);
    for pure_fn in &program.pure_fns {
        entity::check_pure_fn_purity(pure_fn, &state, diags);
    }
    entity::check_bare_stdlib_calls_pure_fns(program, diags);

    for entity in &program.entities {
        entity::check_route_references(entity, diags);
        check_temporal_dag(entity, diags);
        entity::check_constant_values(entity, diags);
        entity::check_undefined_refs(
            entity,
            &program.pure_fns,
            &program.entities,
            &program.libraries,
            &program.type_aliases,
            diags,
        );
        entity::check_temporal_ref_placement(entity, diags);
        entity::check_bounce_handlers(entity, diags);
        entity::check_rescue_bounce_false(entity, diags);
        entity::check_deprecated_bounce_handlers(entity, diags);
        entity::check_phase_consistency(entity, diags);
        entity::check_identity_members(entity, diags);
        entity::check_from_clause_uniform_throw(entity, diags);
        entity::check_factory_only_placement(entity, diags);
        entity::check_empty_string_as_address(entity, diags);
        warnings::lint_unused_members(entity, diags);
        warnings::lint_unused_constants(entity, diags);
    }

    for entity in &program.entities {
        entity::check_typed_send_destinations(
            entity,
            &program.entities,
            &program.extern_entities,
            diags,
        );
        entity::check_action_targets(entity, &program.entities, &program.extern_entities, diags);
    }

    entity::check_extern_entities(program, diags);

    for entity in &program.entities {
        entity::check_update_code_terminal(entity, diags);
    }

    for entity in &program.entities {
        check_var_call_scoping(entity, diags);
    }

    for entity in &program.entities {
        check_action_for_body(entity, diags);
    }

    for entity in &program.entities {
        entity::check_duplicate_deploys(entity, diags);
        entity::check_match_arm_priority_entity(entity, diags);
        entity::check_match_exhaustive_entity(entity, program, diags);
        entity::check_route_returns(entity, program, diags);
        entity::check_record_literals_entity(entity, program, diags);
        entity::check_mixed_sign_entity(entity, program, diags);
        entity::check_transform_arity(entity, diags);
        entity::check_if_requires_else_entity(entity, diags);
        entity::check_unused_route_lets_entity(entity, diags);
    }
    entity::check_recursive_records(program, diags);
    entity::check_let_some_nested_patterns(program, diags);
    entity::check_option_arithmetic(program, diags);
    for pure_fn in &program.pure_fns {
        entity::check_match_arm_priority_pure_fn(pure_fn, diags);
        entity::check_match_exhaustive_pure_fn(pure_fn, program, diags);
        entity::check_pure_fn_return(pure_fn, program, diags);
        entity::check_record_literals_pure_fn(pure_fn, program, diags);
        entity::check_mixed_sign_pure_fn(pure_fn, program, diags);
        entity::check_if_requires_else_pure_fn(pure_fn, diags);
        entity::check_unused_lets_pure_fn(pure_fn, diags);
    }

    warnings::lint_unused_pure_fns(program, diags);
    warnings::lint_duplicate_user_types(program, diags);
    check_imports(program, diags);
    check_namespace_usage(program, diags);
    check_file_imports(program, diags);
    check_using_decls(program, diags);
    check_libraries(program, diags);

    for test in &program.tests {
        test_checks::validate_test(test, program, diags);
        if let Some(entity) = program.entities.iter().find(|e| e.name == test.entity_name) {
            warnings::lint_ctor_before_msg(
                &format!("test \"{}\"", test.name),
                entity,
                &test.body,
                diags,
            );
        }
    }

    for fuzz in &program.fuzz_tests {
        let param_names: std::collections::HashSet<&str> =
            fuzz.params.iter().map(|p| p.name.as_str()).collect();
        test_checks::validate_typed_spec_lets(
            &format!("fuzz \"{}\"", fuzz.name),
            &fuzz.body,
            &param_names,
            diags,
        );
        if let Some(entity) = program.entities.iter().find(|e| e.name == fuzz.entity_name) {
            test_checks::validate_expect_preds(
                &format!("fuzz \"{}\"", fuzz.name),
                &fuzz.body,
                entity,
                &program.entities,
                &param_names,
                diags,
            );
        }
    }

    for prop in &program.properties {
        test_checks::validate_property(prop, &program.entities, diags);
        if let Some(entity) = program.entities.iter().find(|e| e.name == prop.entity_name) {
            warnings::lint_ctor_before_msg(
                &format!("property \"{}\"", prop.name),
                entity,
                &prop.body,
                diags,
            );
        }
    }

    for inv in &program.invariants {
        test_checks::validate_invariant(inv, &program.entities, diags);
    }
}

family_runners! {
    "universal", collect,
    run_v1 => "V1",
    run_v2 => "V2",
    run_v3 => "V3",
    run_v4 => "V4",
    run_v7 => "V7",
    run_v8 => "V8",
    run_v9 => "V9",
    run_v10 => "V10",
    run_v11 => "V11",
    run_v12 => "V12",
    run_v13 => "V13",
    run_v14 => "V14",
    run_v15 => "V15",
    run_v16 => "V16",
    run_v17 => "V17",
    run_v20 => "V20",
    run_v21 => "V21",
    run_v22 => "V22",
    run_v23 => "V23",
    run_v24 => "V24",
    run_v25 => "V25",
    run_v26 => "V26",
    run_v27 => "V27",
    run_v28 => "V28",
    run_v29 => "V29",
    run_v30 => "V30",
    run_v31 => "V31",
    run_v32 => "V32",
    run_v42 => "V42",
    run_v43 => "V43",
    run_v44 => "V44",
    run_v45 => "V45",
    run_v47 => "V47",
    run_v48 => "V48",
    run_v49 => "V49",
    run_v50 => "V50",
    run_v51 => "V51",
    run_v52 => "V52",
    run_v53 => "V53",
    run_v54 => "V54",
    run_v55 => "V55",
    run_v56 => "V56",
    run_v57 => "V57",
    run_v58 => "V58",
    run_v59 => "V59",
    run_v60 => "V60",
    run_v64 => "V64",
    run_v65 => "V65",
    run_v66 => "V66",
    run_v67 => "V67",
    run_v68 => "V68",
    run_v69 => "V69",
    run_v70 => "V70",
    run_t1 => "T1",
    run_t2 => "T2",
    run_t3 => "T3",
    run_t4 => "T4",
    run_t5 => "T5",
    run_t6 => "T6",
    run_t8 => "T8",
    run_t9 => "T9",
    run_t10 => "T10",
    run_t11 => "T11",
    run_t12 => "T12",
    run_t15 => "T15",
    run_t16 => "T16",
    run_t17 => "T17",
    run_t18 => "T18",
    run_t19 => "T19",
    run_t20 => "T20",
    run_t21 => "T21",
    run_t22 => "T22",
    run_t23 => "T23",
    run_t24 => "T24",
    run_t25 => "T25",
    run_t26 => "T26",
    run_t27 => "T27",
    run_t28 => "T28",
    run_t29 => "T29",
    run_t30 => "T30",
    run_t31 => "T31",
    run_t38 => "T38",
    run_i1 => "I1",
    run_i2 => "I2",
    run_i3 => "I3",
    run_i4 => "I4",
    run_i5 => "I5",
    run_i6 => "I6",
    run_i8 => "I8",
    run_i9 => "I9",
    run_i10 => "I10",
    run_i11 => "I11",
    run_i12 => "I12",
    run_i13 => "I13",
    run_i14 => "I14",
    run_i15 => "I15",
    run_i16 => "I16",
    run_w1 => "W1",
    run_w2 => "W2",
    run_w3 => "W3",
    run_w4 => "W4",
    run_w5 => "W5",
    run_w6 => "W6",
    run_w8 => "W8",
    run_w9 => "W9",
    run_w11 => "W11",
    run_w13 => "W13",
    run_w14 => "W14",
    run_w15 => "W15",
}
