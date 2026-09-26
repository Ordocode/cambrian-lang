// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean-EVM adapter: world, sends, deploy, dispatch, specs, tests.

pub mod address;
#[cfg(feature = "plausible")]
pub mod bounds;
pub mod deploy;
pub mod dispatch;
pub mod domain;
#[cfg(feature = "predictable-profile")]
pub mod predictable;
#[path = "extern.rs"]
pub mod extern_;
pub mod invariant;
pub mod invariant_shape;
#[cfg(feature = "plausible")]
pub mod plausible;
pub mod property;
pub mod send;
pub mod spec;
pub mod spec_prefix;
pub mod spec_steps;
pub mod test;
pub mod world;

pub use domain::{EvmDomain, LeanDomain, SysFieldCtx};

pub(crate) fn rewrite_route_body(body: &str, predictable: bool) -> String {
    #[cfg(feature = "predictable-profile")]
    {
        predictable::rewrite_route_body(body, predictable)
    }
    #[cfg(not(feature = "predictable-profile"))]
    {
        let _ = predictable;
        body.to_string()
    }
}

pub(crate) fn rewrite_member_body(body: &str, predictable: bool) -> String {
    #[cfg(feature = "predictable-profile")]
    {
        predictable::rewrite_member_body(body, predictable)
    }
    #[cfg(not(feature = "predictable-profile"))]
    {
        let _ = predictable;
        body.to_string()
    }
}

pub(crate) fn rewrite_pure_fn_body(body: &str) -> String {
    #[cfg(feature = "predictable-profile")]
    {
        predictable::rewrite_pure_fn_body(body)
    }
    #[cfg(not(feature = "predictable-profile"))]
    {
        body.to_string()
    }
}

pub(crate) fn tuple_pattern_punt(site: &str, detail: &str) -> ! {
    #[cfg(feature = "predictable-profile")]
    {
        predictable::tuple_pattern_punt(site, detail)
    }
    #[cfg(not(feature = "predictable-profile"))]
    {
        unreachable!("predictable-profile disabled: {site}: {detail}")
    }
}

pub(crate) fn closure_tuple_binder(names: &[String], slot: usize) -> (String, String) {
    #[cfg(feature = "predictable-profile")]
    {
        predictable::closure_tuple_binder(names, slot)
    }
    #[cfg(not(feature = "predictable-profile"))]
    {
        let _ = (names, slot);
        unreachable!("predictable-profile disabled")
    }
}

pub(crate) fn maybe_emit_struct_eq_instances(
    out: &mut String,
    use_predictable: bool,
    ty: &str,
    field_names: &[String],
) {
    #[cfg(feature = "predictable-profile")]
    if use_predictable {
        crate::codegen::lean::core::deceq::emit_struct_deceq(out, ty, field_names);
        crate::codegen::lean::core::deceq::emit_struct_beq(out, ty, field_names);
    }
    #[cfg(not(feature = "predictable-profile"))]
    {
        let _ = (out, use_predictable, ty, field_names);
    }
}

pub(crate) fn maybe_emit_struct_deceq(
    out: &mut String,
    use_predictable: bool,
    ty: &str,
    field_names: &[String],
) {
    #[cfg(feature = "predictable-profile")]
    if use_predictable {
        crate::codegen::lean::core::deceq::emit_struct_deceq(out, ty, field_names);
    }
    #[cfg(not(feature = "predictable-profile"))]
    {
        let _ = (out, use_predictable, ty, field_names);
    }
}

pub(crate) fn maybe_emit_enum_eq_instances(
    out: &mut String,
    use_predictable: bool,
    ty: &str,
    ctors: &[(String, usize)],
    is_data: bool,
) {
    #[cfg(feature = "predictable-profile")]
    if use_predictable {
        crate::codegen::lean::core::deceq::emit_enum_deceq(out, ty, ctors);
        if is_data {
            crate::codegen::lean::core::deceq::emit_enum_beq(out, ty, ctors);
        }
    }
    #[cfg(not(feature = "predictable-profile"))]
    {
        let _ = (out, use_predictable, ty, ctors, is_data);
    }
}
