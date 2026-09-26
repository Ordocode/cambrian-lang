// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Per-rule validation units (P7).
//!
//! Each [`RuleEntry`](super::registry::RuleEntry) is independently
//! dispatchable via its binding. Shared walk helpers live alongside the
//! rules that use them; bindings live only in [`RULES`].
//!
//! Module naming: files are named after what the rule *means*, not after
//! rule codes — `tvm_constructs` (TVM-model constructs off TVM/Container),
//! `evm_domain` (EVM world-model), `evm_namespace` (`evm::*` off EVM),
//! `solidity_lang` / `lean_lang` (carrier-language expressivity),
//! `lean_evm_pair` (adapter-pair contracts), `receive_fallback`,
//! `fuzz_backend_support` (W7), plus the universal `events` / `errors` /
//! `universal` clusters.
//!
//! Phase M PM-031: every [`RuleEntry`] carries exactly one diagnostic
//! code. Same-binding families share a cached walk via [`emit_cached`]
//! so splitting does not re-walk the AST once per code.

mod container_compat;
mod errors;
mod events;
mod evm_domain;
mod evm_namespace;
mod evm_predicate;
mod fuzz_backend_support;
mod invariant_identity;
mod lean_evm_pair;
mod lean_lang;
mod multi_entity_invariants;
mod receive_fallback;
mod solidity_lang;
mod tvm_constructs;
mod universal;

use std::cell::RefCell;

use crate::target::{Domain, Language};

use super::registry::{RuleBinding, RuleEntry, ValidateCtx};
use super::Diagnostic;

struct FamilyCacheEntry {
    program_ptr: usize,
    family: &'static str,
    deterministic: bool,
    diags: Vec<Diagnostic>,
}

thread_local! {
    static FAMILY_CACHE: RefCell<Option<FamilyCacheEntry>> = const { RefCell::new(None) };
}

/// Drop any cached family walk (call at validate / target-compat boundaries).
pub(super) fn clear_family_cache() {
    FAMILY_CACHE.with(|c| {
        *c.borrow_mut() = None;
    });
}

/// Run `produce` once per (program, family, deterministic) and emit diags
/// matching `code`, preserving relative order within that code.
pub(super) fn emit_cached(
    family: &'static str,
    ctx: &ValidateCtx<'_>,
    code: &'static str,
    diags: &mut Vec<Diagnostic>,
    produce: fn(&ValidateCtx<'_>, &mut Vec<Diagnostic>),
) {
    FAMILY_CACHE.with(|cell| {
        let mut slot = cell.borrow_mut();
        let ptr = ctx.program as *const _ as usize;
        let hit = matches!(
            &*slot,
            Some(e)
                if e.program_ptr == ptr
                    && e.family == family
                    && e.deterministic == ctx.deterministic
        );
        if !hit {
            let mut all = Vec::new();
            produce(ctx, &mut all);
            *slot = Some(FamilyCacheEntry {
                program_ptr: ptr,
                family,
                deterministic: ctx.deterministic,
                diags: all,
            });
        }
        for d in &slot.as_ref().expect("family cache populated").diags {
            if d.code == code {
                diags.push(d.clone());
            }
        }
    });
}

/// Declare one thin runner per diagnostic code that filters a shared family walk.
macro_rules! family_runners {
    ($family:literal, $collect:path, $($run:ident => $code:literal),+ $(,)?) => {
        $(
            pub fn $run(ctx: &super::super::registry::ValidateCtx<'_>, diags: &mut Vec<super::super::Diagnostic>) {
                super::emit_cached($family, ctx, $code, diags, $collect);
            }
        )+
    };
}
pub(crate) use family_runners;

macro_rules! rule {
    ($code:literal, $binding:expr, $run:path) => {
        RuleEntry {
            codes: &[$code],
            binding: $binding,
            run: $run,
        }
    };
}

/// Full rule table. One code per entry (PM-031). Family order matches the
/// pre-split clusters; within a family, code order matches the former
/// `CODES` arrays so filtered emission stays stable relative to that list.
/// The validate snapshot compares sorted dumps, so walk interleaving changes
/// are fine.
pub static RULES: &[RuleEntry] = &[
    // --- Universal (target-less validate + all targets) ---
    rule!("V1", RuleBinding::Universal, universal::run_v1),
    rule!("V2", RuleBinding::Universal, universal::run_v2),
    rule!("V3", RuleBinding::Universal, universal::run_v3),
    rule!("V4", RuleBinding::Universal, universal::run_v4),
    rule!("V7", RuleBinding::Universal, universal::run_v7),
    rule!("V8", RuleBinding::Universal, universal::run_v8),
    rule!("V9", RuleBinding::Universal, universal::run_v9),
    rule!("V10", RuleBinding::Universal, universal::run_v10),
    rule!("V11", RuleBinding::Universal, universal::run_v11),
    rule!("V12", RuleBinding::Universal, universal::run_v12),
    rule!("V13", RuleBinding::Universal, universal::run_v13),
    rule!("V14", RuleBinding::Universal, universal::run_v14),
    rule!("V15", RuleBinding::Universal, universal::run_v15),
    rule!("V16", RuleBinding::Universal, universal::run_v16),
    rule!("V17", RuleBinding::Universal, universal::run_v17),
    rule!("V20", RuleBinding::Universal, universal::run_v20),
    rule!("V21", RuleBinding::Universal, universal::run_v21),
    rule!("V22", RuleBinding::Universal, universal::run_v22),
    rule!("V23", RuleBinding::Universal, universal::run_v23),
    rule!("V24", RuleBinding::Universal, universal::run_v24),
    rule!("V25", RuleBinding::Universal, universal::run_v25),
    rule!("V26", RuleBinding::Universal, universal::run_v26),
    rule!("V27", RuleBinding::Universal, universal::run_v27),
    rule!("V28", RuleBinding::Universal, universal::run_v28),
    rule!("V29", RuleBinding::Universal, universal::run_v29),
    rule!("V30", RuleBinding::Universal, universal::run_v30),
    rule!("V31", RuleBinding::Universal, universal::run_v31),
    rule!("V32", RuleBinding::Universal, universal::run_v32),
    rule!("V42", RuleBinding::Universal, universal::run_v42),
    rule!("V43", RuleBinding::Universal, universal::run_v43),
    rule!("V44", RuleBinding::Universal, universal::run_v44),
    rule!("V45", RuleBinding::Universal, universal::run_v45),
    rule!("V47", RuleBinding::Universal, universal::run_v47),
    rule!("V48", RuleBinding::Universal, universal::run_v48),
    rule!("V49", RuleBinding::Universal, universal::run_v49),
    rule!("V50", RuleBinding::Universal, universal::run_v50),
    rule!("V51", RuleBinding::Universal, universal::run_v51),
    rule!("V52", RuleBinding::Universal, universal::run_v52),
    rule!("V53", RuleBinding::Universal, universal::run_v53),
    rule!("V54", RuleBinding::Universal, universal::run_v54),
    rule!("V55", RuleBinding::Universal, universal::run_v55),
    rule!("V56", RuleBinding::Universal, universal::run_v56),
    rule!("V57", RuleBinding::Universal, universal::run_v57),
    rule!("V58", RuleBinding::Universal, universal::run_v58),
    rule!("V59", RuleBinding::Universal, universal::run_v59),
    rule!("V60", RuleBinding::Universal, universal::run_v60),
    rule!("V64", RuleBinding::Universal, universal::run_v64),
    rule!("V65", RuleBinding::Universal, universal::run_v65),
    rule!("V66", RuleBinding::Universal, universal::run_v66),
    rule!("V67", RuleBinding::Universal, universal::run_v67),
    rule!("V68", RuleBinding::Universal, universal::run_v68),
    rule!("V69", RuleBinding::Universal, universal::run_v69),
    rule!("V70", RuleBinding::Universal, universal::run_v70),
    rule!("T1", RuleBinding::Universal, universal::run_t1),
    rule!("T2", RuleBinding::Universal, universal::run_t2),
    rule!("T3", RuleBinding::Universal, universal::run_t3),
    rule!("T4", RuleBinding::Universal, universal::run_t4),
    rule!("T5", RuleBinding::Universal, universal::run_t5),
    rule!("T6", RuleBinding::Universal, universal::run_t6),
    rule!("T8", RuleBinding::Universal, universal::run_t8),
    rule!("T9", RuleBinding::Universal, universal::run_t9),
    rule!("T10", RuleBinding::Universal, universal::run_t10),
    rule!("T11", RuleBinding::Universal, universal::run_t11),
    rule!("T12", RuleBinding::Universal, universal::run_t12),
    rule!("T15", RuleBinding::Universal, universal::run_t15),
    rule!("T16", RuleBinding::Universal, universal::run_t16),
    rule!("T17", RuleBinding::Universal, universal::run_t17),
    rule!("T18", RuleBinding::Universal, universal::run_t18),
    rule!("T19", RuleBinding::Universal, universal::run_t19),
    rule!("T20", RuleBinding::Universal, universal::run_t20),
    rule!("T21", RuleBinding::Universal, universal::run_t21),
    rule!("T22", RuleBinding::Universal, universal::run_t22),
    rule!("T23", RuleBinding::Universal, universal::run_t23),
    rule!("T24", RuleBinding::Universal, universal::run_t24),
    rule!("T25", RuleBinding::Universal, universal::run_t25),
    rule!("T26", RuleBinding::Universal, universal::run_t26),
    rule!("T27", RuleBinding::Universal, universal::run_t27),
    rule!("T28", RuleBinding::Universal, universal::run_t28),
    rule!("T29", RuleBinding::Universal, universal::run_t29),
    rule!("T30", RuleBinding::Universal, universal::run_t30),
    rule!("T31", RuleBinding::Universal, universal::run_t31),
    rule!("T38", RuleBinding::Universal, universal::run_t38),
    rule!("I1", RuleBinding::Universal, universal::run_i1),
    rule!("I2", RuleBinding::Universal, universal::run_i2),
    rule!("I3", RuleBinding::Universal, universal::run_i3),
    rule!("I4", RuleBinding::Universal, universal::run_i4),
    rule!("I5", RuleBinding::Universal, universal::run_i5),
    rule!("I6", RuleBinding::Universal, universal::run_i6),
    rule!("I8", RuleBinding::Universal, universal::run_i8),
    rule!("I9", RuleBinding::Universal, universal::run_i9),
    rule!("I10", RuleBinding::Universal, universal::run_i10),
    rule!("I11", RuleBinding::Universal, universal::run_i11),
    rule!("I12", RuleBinding::Universal, universal::run_i12),
    rule!("I13", RuleBinding::Universal, universal::run_i13),
    rule!("I14", RuleBinding::Universal, universal::run_i14),
    rule!("I15", RuleBinding::Universal, universal::run_i15),
    rule!("I16", RuleBinding::Universal, universal::run_i16),
    rule!("W1", RuleBinding::Universal, universal::run_w1),
    rule!("W2", RuleBinding::Universal, universal::run_w2),
    rule!("W3", RuleBinding::Universal, universal::run_w3),
    rule!("W4", RuleBinding::Universal, universal::run_w4),
    rule!("W5", RuleBinding::Universal, universal::run_w5),
    rule!("W6", RuleBinding::Universal, universal::run_w6),
    rule!("W8", RuleBinding::Universal, universal::run_w8),
    rule!("W9", RuleBinding::Universal, universal::run_w9),
    rule!("W11", RuleBinding::Universal, universal::run_w11),
    rule!("W13", RuleBinding::Universal, universal::run_w13),
    rule!("W14", RuleBinding::Universal, universal::run_w14),
    rule!("W15", RuleBinding::Universal, universal::run_w15),
    rule!("V34", RuleBinding::Universal, events::run_v34),
    rule!("V35", RuleBinding::Universal, events::run_v35),
    rule!("V38", RuleBinding::Universal, errors::run_v38),
    rule!("V39", RuleBinding::Universal, errors::run_v39),
    // --- Targeted (every concrete target; not target-less validate) ---
    rule!("W7", RuleBinding::Targeted, fuzz_backend_support::run),
    rule!(
        "E01",
        RuleBinding::NotDomains(&[Domain::Tvm]),
        tvm_constructs::run_e01
    ),
    rule!(
        "E02",
        RuleBinding::NotDomains(&[Domain::Tvm]),
        tvm_constructs::run_e02
    ),
    rule!(
        "E06",
        RuleBinding::NotDomains(&[Domain::Tvm]),
        tvm_constructs::run_e06
    ),
    rule!(
        "E03",
        RuleBinding::NotDomains(&[Domain::Tvm, Domain::Container]),
        tvm_constructs::run_e03
    ),
    rule!(
        "E04",
        RuleBinding::NotDomains(&[Domain::Tvm, Domain::Container]),
        tvm_constructs::run_e04
    ),
    rule!(
        "E05",
        RuleBinding::NotDomains(&[Domain::Tvm, Domain::Container]),
        tvm_constructs::run_e05
    ),
    rule!(
        "E10",
        RuleBinding::NotDomains(&[Domain::Tvm, Domain::Container]),
        tvm_constructs::run_e10
    ),
    rule!(
        "E11",
        RuleBinding::NotDomains(&[Domain::Tvm, Domain::Container]),
        tvm_constructs::run_e11
    ),
    rule!(
        "E13",
        RuleBinding::NotDomains(&[Domain::Tvm, Domain::Container]),
        tvm_constructs::run_e13
    ),
    rule!(
        "E14",
        RuleBinding::NotDomains(&[Domain::Tvm, Domain::Container]),
        tvm_constructs::run_e14
    ),
    rule!(
        "E15",
        RuleBinding::NotDomains(&[Domain::Tvm, Domain::Container]),
        tvm_constructs::run_e15
    ),
    rule!(
        "E26",
        RuleBinding::NotDomains(&[Domain::Tvm, Domain::Container]),
        tvm_constructs::run_e26
    ),
    rule!(
        "E24",
        RuleBinding::Domains(&[Domain::Container]),
        container_compat::run_e24
    ),
    rule!(
        "E25",
        RuleBinding::Domains(&[Domain::Container, Domain::Evm]),
        container_compat::run_e25
    ),
    // --- EVM world-model rules (all EVM-domain cores) ---
    rule!(
        "E09",
        RuleBinding::Domains(&[Domain::Evm]),
        evm_domain::run_e09
    ),
    rule!(
        "E16",
        RuleBinding::Domains(&[Domain::Evm]),
        evm_domain::run_e16
    ),
    rule!(
        "E22",
        RuleBinding::Domains(&[Domain::Evm]),
        evm_domain::run_e22
    ),
    rule!(
        "E28",
        RuleBinding::Domains(&[Domain::Evm]),
        evm_domain::run_e28
    ),
    rule!(
        "V33",
        RuleBinding::Domains(&[Domain::Evm]),
        evm_domain::run_v33
    ),
    rule!(
        "V62",
        RuleBinding::Domains(&[Domain::Evm]),
        evm_domain::run_v62
    ),
    rule!(
        "V63",
        RuleBinding::Domains(&[Domain::Evm]),
        evm_domain::run_v63
    ),
    rule!(
        "V36",
        RuleBinding::Domains(&[Domain::Evm]),
        evm_domain::run_v36
    ),
    rule!(
        "W10",
        RuleBinding::Domains(&[Domain::Evm]),
        evm_domain::run_w10
    ),
    rule!(
        "V40",
        RuleBinding::Domains(&[Domain::Evm]),
        receive_fallback::run_v40
    ),
    rule!(
        "V41",
        RuleBinding::Domains(&[Domain::Evm]),
        receive_fallback::run_v41
    ),
    // --- TVM-domain rules ---
    rule!(
        "I7",
        RuleBinding::Domains(&[Domain::Tvm]),
        multi_entity_invariants::run
    ),
    rule!(
        "I18",
        RuleBinding::Domains(&[Domain::Evm]),
        invariant_identity::run
    ),
    // --- EVM-model constructs off the EVM domain ---
    rule!(
        "E12",
        RuleBinding::NotDomains(&[Domain::Evm]),
        evm_namespace::run
    ),
    // --- Carrier-language expressivity ---
    rule!(
        "I17",
        RuleBinding::Language(Language::Solidity),
        evm_predicate::run
    ),
    rule!(
        "E07",
        RuleBinding::Language(Language::Solidity),
        solidity_lang::run_e07
    ),
    rule!(
        "E08",
        RuleBinding::Language(Language::Solidity),
        solidity_lang::run_e08
    ),
    rule!(
        "E17",
        RuleBinding::Language(Language::Solidity),
        solidity_lang::run_e17
    ),
    rule!(
        "E18",
        RuleBinding::Language(Language::Solidity),
        solidity_lang::run_e18
    ),
    rule!(
        "E19",
        RuleBinding::Language(Language::Solidity),
        solidity_lang::run_e19
    ),
    rule!(
        "E20",
        RuleBinding::Language(Language::Solidity),
        solidity_lang::run_e20
    ),
    rule!(
        "E21",
        RuleBinding::Language(Language::Solidity),
        solidity_lang::run_e21
    ),
    rule!(
        "E23",
        RuleBinding::Language(Language::Solidity),
        solidity_lang::run_e23
    ),
    rule!(
        "E29",
        RuleBinding::Language(Language::Solidity),
        solidity_lang::run_e29
    ),
    rule!(
        "T39",
        RuleBinding::Language(Language::Solidity),
        solidity_lang::run_t39
    ),
    rule!(
        "L1",
        RuleBinding::Language(Language::Lean),
        lean_lang::run_l1
    ),
    rule!(
        "L2",
        RuleBinding::Language(Language::Lean),
        lean_lang::run_l2
    ),
    rule!(
        "L5",
        RuleBinding::Language(Language::Lean),
        lean_lang::run_l5
    ),
    rule!(
        "L6",
        RuleBinding::Language(Language::Lean),
        lean_lang::run_l6
    ),
    rule!(
        "L7",
        RuleBinding::Language(Language::Lean),
        lean_lang::run_l7
    ),
    rule!(
        "L13",
        RuleBinding::Language(Language::Lean),
        lean_lang::run_l13
    ),
    rule!(
        "L14",
        RuleBinding::Language(Language::Lean),
        lean_lang::run_l14
    ),
    rule!(
        "L15",
        RuleBinding::Language(Language::Lean),
        lean_lang::run_l15
    ),
    rule!(
        "L16",
        RuleBinding::Language(Language::Lean),
        lean_lang::run_l16
    ),
    // --- Adapter-pair contracts ---
    rule!(
        "L8",
        RuleBinding::Pair(Domain::Evm, Language::Lean),
        lean_evm_pair::run_l8
    ),
    rule!(
        "L9",
        RuleBinding::Pair(Domain::Evm, Language::Lean),
        lean_evm_pair::run_l9
    ),
    rule!(
        "L10",
        RuleBinding::Pair(Domain::Evm, Language::Lean),
        lean_evm_pair::run_l10
    ),
    rule!(
        "L11",
        RuleBinding::Pair(Domain::Evm, Language::Lean),
        lean_evm_pair::run_l11
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// All E-codes and L-codes that the family walks can emit.
    const EVM_FAMILY_CODES: &[&str] = &[
        "E01", "E02", "E03", "E04", "E05", "E06", "E07", "E08", "E09", "E10", "E11", "E12", "E13",
        "E14", "E15", "E16", "E17", "E18", "E19", "E20", "E21", "E22", "E23", "E24", "E25", "E26",
        "E28", "E29",
    ];
    const LEAN_FAMILY_CODES: &[&str] = &[
        "L1", "L2", "L5", "L6", "L7", "L8", "L9", "L10", "L11", "L13", "L14", "L15", "L16",
    ];

    #[test]
    fn rule_codes_unique_across_entries() {
        let mut seen = HashSet::new();
        for entry in RULES {
            assert_eq!(
                entry.codes.len(),
                1,
                "RuleEntry must carry exactly one code (PM-031), got {:?}",
                entry.codes
            );
            for code in entry.codes {
                assert!(
                    seen.insert(*code),
                    "diagnostic code {code} appears in more than one RuleEntry"
                );
            }
            assert!(!entry.codes.is_empty(), "RuleEntry with empty codes");
        }
    }

    #[test]
    fn evm_and_lean_code_lists_registered() {
        for code in EVM_FAMILY_CODES.iter().chain(LEAN_FAMILY_CODES.iter()) {
            assert!(
                RULES.iter().any(|e| e.codes.contains(code)),
                "{code} missing from RULES"
            );
        }
    }
}
