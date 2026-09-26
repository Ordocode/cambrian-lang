// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM codegen — `EvmActionEmitter` (`ActionEmitter` impl) + typed-addr collectors.

use super::route::{extract_send_option, infer_sig_type};
use super::transform::gen_mapping_transform_split;
use std::collections::HashMap as StdHashMap;
use crate::analysis::SendTarget;
use crate::ast::{Entity, Expr, Pattern, Program, Route, RouteAction, Type};
use crate::codegen::adapter::ActionEmitter;
use crate::codegen::solidity::core::ctx::EmitScope;
use crate::codegen::solidity::core::ctx::EvmCtx;
use crate::codegen::solidity::core::expr::{gen_expr, gen_expr_hoisted};
use crate::codegen::solidity::core::iter::{bind_loop_pattern, resolve_iter_source, IterElem};
use crate::codegen::solidity::core::scratch::EmitScratch;
use crate::codegen::solidity::core::types::infer_tuple_elem_types;
use crate::codegen::solidity::core::types::{
    hex_literal_to_sol_for_ty, infer_let_type_entity, sol_return_type, *,
};
use crate::codegen::types::resolve_target_entity;
use std::cell::RefCell;

pub(super) fn collect_typed_addr_refs_from_action(
    action: &RouteAction,
    entity: &Entity,
    route: &Route,
    defined: &[&str],
    ext: &mut Vec<String>,
    int: &mut Vec<String>,
) {
    match action {
        RouteAction::Let { value, .. } => {
            collect_typed_addr_refs_from_expr(value, entity, route, defined, ext, int);
        }
        RouteAction::Send { dest, .. } => {
            if let Some(en) = resolve_target_entity(dest, entity, route) {
                if defined.contains(&en.as_str()) {
                    if !int.contains(&en) {
                        int.push(en);
                    }
                } else if !ext.contains(&en) {
                    ext.push(en);
                }
            }
        }
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            for a in then_actions {
                collect_typed_addr_refs_from_action(a, entity, route, defined, ext, int);
            }
            for a in else_actions {
                collect_typed_addr_refs_from_action(a, entity, route, defined, ext, int);
            }
        }
        RouteAction::VarCall { dest, .. } => {
            if let Some(en) = resolve_target_entity(dest, entity, route) {
                if defined.contains(&en.as_str()) {
                    if !int.contains(&en) {
                        int.push(en);
                    }
                } else if !ext.contains(&en) {
                    ext.push(en);
                }
            }
        }
        RouteAction::Rescue { action, .. } => {
            collect_typed_addr_refs_from_action(action, entity, route, defined, ext, int);
        }
        RouteAction::For { body, iter, .. } => {
            collect_typed_addr_refs_from_expr(iter, entity, route, defined, ext, int);
            for a in body {
                collect_typed_addr_refs_from_action(a, entity, route, defined, ext, int);
            }
        }
        _ => {}
    }
}

pub(super) fn collect_typed_addr_refs_from_expr(
    expr: &Expr,
    entity: &Entity,
    route: &Route,
    defined: &[&str],
    ext: &mut Vec<String>,
    int: &mut Vec<String>,
) {
    if let Expr::MethodCall(base, _, _) = expr {
        if let Some(en) = resolve_target_entity(base, entity, route) {
            if defined.contains(&en.as_str()) {
                if !int.contains(&en) {
                    int.push(en);
                }
            } else if !ext.contains(&en) {
                ext.push(en);
            }
        }
    }
}

// ===========================================================================
// EvmActionEmitter — ActionEmitter implementation for EVM/Solidity target
// ===========================================================================

pub struct EvmActionEmitter<'a> {
    ctx: &'a EvmCtx,
    scope: EmitScope<'a>,
    scratch: &'a RefCell<EmitScratch>,
}

impl<'a> EvmActionEmitter<'a> {
    pub fn new(ctx: &'a EvmCtx, scope: EmitScope<'a>, scratch: &'a RefCell<EmitScratch>) -> Self {
        Self {
            ctx,
            scope,
            scratch,
        }
    }

    pub(super) fn emit_send_with_target(
        &self,
        message: &Option<String>,
        args: &[Expr],
        dest: &Expr,
        send_options: Option<&Expr>,
        target: &SendTarget,
        entity: &Entity,
        route: &Route,
        _program: &Program,
        indent: &str,
    ) -> String {
        self.emit_send_impl(
            message,
            args,
            dest,
            send_options,
            target,
            entity,
            route,
            indent,
        )
    }

    pub(super) fn emit_var_call_with_target(
        &self,
        name: &str,
        message: &str,
        args: &[Expr],
        dest: &Expr,
        send_options: Option<&Expr>,
        target: &SendTarget,
        entity: &Entity,
        route: &Route,
        program: &Program,
        indent: &str,
    ) -> String {
        self.emit_var_call_impl(
            name,
            message,
            args,
            dest,
            send_options,
            target,
            entity,
            route,
            program,
            indent,
        )
    }

    fn emit_send_impl(
        &self,
        message: &Option<String>,
        args: &[Expr],
        dest: &Expr,
        send_options: Option<&Expr>,
        target: &SendTarget,
        entity: &Entity,
        route: &Route,
        indent: &str,
    ) -> String {
        let ctx = self.ctx;
        let dest_expr = gen_expr(dest, ctx, &self.scope, self.scratch)
            .unwrap_or_else(|| "address(0)".to_string());
        let dest_expr = super::super::core::types::coerce_to_address_sol(
            dest,
            &dest_expr,
            entity,
            ctx,
            self.scratch,
        );
        let value_expr = extract_send_option(send_options, "value")
            .and_then(|e| gen_expr(e, ctx, &self.scope, self.scratch))
            .unwrap_or_else(|| "0".to_string());
        let raw_data_expr = extract_send_option(send_options, "data")
            .or_else(|| extract_send_option(send_options, "payload"));
        let data_provided = raw_data_expr.is_some();
        let payload_expr = raw_data_expr
            .and_then(|e| gen_expr(e, ctx, &self.scope, self.scratch))
            .unwrap_or_else(|| "\"\"".to_string());

        let target_entity =
            super::route::resolve_send_target(target, dest, &entity.name, entity, route);

        if let Some(ref msg_name_raw) = message {
            let msg_name = sol_sanitize_ident(msg_name_raw);
            let require_msg = format!("named send {} failed", msg_name);
            let arg_values: Vec<String> = args
                .iter()
                .map(|a| {
                    gen_expr(a, ctx, &self.scope, self.scratch).unwrap_or_else(|| "0".to_string())
                })
                .collect();

            if let Some(target) = target_entity {
                let iface = ctx.interface_cast_name(&target);
                let call_args = arg_values.join(", ");
                if value_expr == "0" {
                    return format!(
                        "{}{}({}).{}({});\n",
                        indent, iface, dest_expr, msg_name, call_args
                    );
                }
                return format!(
                    "{}{}({}).{}{{value: {}}}({});\n",
                    indent, iface, dest_expr, msg_name, value_expr, call_args
                );
            }

            let arg_sig: Vec<&str> = args
                .iter()
                .map(|a| infer_sig_type(a, self.scratch))
                .collect();
            let signature = format!("{}({})", msg_name_raw, arg_sig.join(","));
            let payload = if arg_values.is_empty() {
                format!("abi.encodeWithSignature(\"{}\")", signature)
            } else {
                format!(
                    "abi.encodeWithSignature(\"{}\", {})",
                    signature,
                    arg_values.join(", ")
                )
            };
            format!(
                "{}{{ (bool ok, ) = payable({}).call{{value: {}}}({}); require(ok, \"{}\"); }}\n",
                indent, dest_expr, value_expr, payload, require_msg
            )
        } else {
            let require_msg = "transfer failed";
            let calldata = if data_provided {
                payload_expr
            } else {
                format!("bytes({})", payload_expr)
            };
            format!(
                "{}{{ (bool ok, ) = payable({}).call{{value: {}}}({}); require(ok, \"{}\"); }}\n",
                indent, dest_expr, value_expr, calldata, require_msg
            )
        }
    }

    fn emit_var_call_impl(
        &self,
        name: &str,
        message: &str,
        args: &[Expr],
        dest: &Expr,
        send_options: Option<&Expr>,
        target: &SendTarget,
        entity: &Entity,
        route: &Route,
        program: &Program,
        indent: &str,
    ) -> String {
        let ctx = self.ctx;
        let dest_expr = gen_expr(dest, ctx, &self.scope, self.scratch)
            .unwrap_or_else(|| "address(0)".to_string());
        let dest_expr = super::super::core::types::coerce_to_address_sol(
            dest,
            &dest_expr,
            entity,
            ctx,
            self.scratch,
        );
        let arg_values: Vec<String> = args
            .iter()
            .map(|a| gen_expr(a, ctx, &self.scope, self.scratch).unwrap_or_else(|| "0".to_string()))
            .collect();
        let call_args = arg_values.join(", ");

        let target_name =
            super::route::resolve_send_target(target, dest, &entity.name, entity, route);
        let target_entity = target_name
            .as_ref()
            .and_then(|tn| program.entities.iter().find(|e| e.name == *tn));
        let target_route = target_entity.and_then(|te| {
            te.routes
                .iter()
                .find(|r| r.name == message && !r.is_private)
        });

        let extern_route = if target_route.is_none() {
            target_name
                .as_ref()
                .and_then(|tn| program.extern_entities.iter().find(|e| e.name == *tn))
                .and_then(|ext| ext.routes.iter().find(|r| r.name == message))
        } else {
            None
        };

        let ret_ty = if let Some(tr) = target_route {
            tr.return_type
                .as_ref()
                .map(|ty| sol_return_type(target_entity.unwrap(), ty, self.ctx))
                .unwrap_or_else(|| "uint256".to_string())
        } else if let Some(er) = extern_route {
            er.return_type
                .as_ref()
                .map(|ty| match ty {
                    Type::Tuple(items) => items
                        .iter()
                        .map(|t| sol_type(t, true))
                        .collect::<Vec<_>>()
                        .join(", "),
                    _ => sol_type(ty, true),
                })
                .unwrap_or_else(|| "uint256".to_string())
        } else {
            "uint256".to_string()
        };

        let value_expr = extract_send_option(send_options, "value")
            .and_then(|e| gen_expr(e, ctx, &self.scope, self.scratch));

        if let Some(ref tn) = target_name {
            let iface = ctx.interface_cast_name(tn);
            let call = if let Some(ref val) = value_expr {
                format!(
                    "{}({}).{}{{value: {}}}({})",
                    iface, dest_expr, message, val, call_args
                )
            } else {
                format!("{}({}).{}({})", iface, dest_expr, message, call_args)
            };
            // The binding name may be a Solidity reserved word (`var after =
            // ...`); reads already lower through `sol_sanitize_ident` in
            // `gen_expr`, so the declaration/assignment must match.
            let san = sol_sanitize_ident(name);
            if self.scratch.borrow_mut().is_hoisted_local(name) {
                format!("{}{} = {};\n", indent, san, call)
            } else {
                format!("{}{} {} = {};\n", indent, ret_ty, san, call)
            }
        } else {
            crate::codegen::types::unresolved_var_call(name, &dest_expr)
        }
    }

    /// Element type + binding shape for an action-level `for pat in iter`.
    pub(super) fn action_for_iter_elem(
        &self,
        iter: &Expr,
        route: &Route,
        entity: &Entity,
        idx: &str,
    ) -> (Vec<String>, String, IterElem) {
        let ctx = self.ctx;
        let scope = &self.scope;
        let scratch = self.scratch;
        let (setup, count_expr, factory) = if let Expr::Ident(name) = iter {
            let param_vec_elem =
                route
                    .params
                    .iter()
                    .find(|p| p.name == *name)
                    .and_then(|p| match &p.ty {
                        Type::Generic(g, params) if g == "Vec" && params.len() == 1 => {
                            Some(sol_type_entity(entity, &params[0], true, ctx))
                        }
                        _ => None,
                    });
            if let Some(elem_ty) = param_vec_elem {
                let count_expr = format!("{}.length", name);
                let nm = name.clone();
                let factory: Box<dyn Fn(&str) -> IterElem> =
                    Box::new(move |idx: &str| IterElem::Scalar {
                        ty: elem_ty.clone(),
                        rhs: format!("{}[{}]", nm, idx),
                    });
                (Vec::new(), count_expr, factory)
            } else {
                resolve_iter_source(iter, entity, ctx, scope, scratch)
            }
        } else {
            resolve_iter_source(iter, entity, ctx, scope, scratch)
        };
        let elem = factory(idx);
        (setup, count_expr, elem)
    }
}

impl<'a> ActionEmitter for EvmActionEmitter<'a> {
    fn emit_let(
        &self,
        pattern: &Pattern,
        value: &Expr,
        entity: &Entity,
        _route: &Route,
        _program: &Program,
        indent: &str,
    ) -> String {
        let ctx = self.ctx;
        let scratch = self.scratch;
        let inferred_tuple = if matches!(pattern, Pattern::Tuple(_)) {
            infer_tuple_elem_types(value, entity, ctx, scratch)
        } else {
            None
        };
        let (stmts, val_str) = gen_expr_hoisted(value, entity, ctx, &self.scope, self.scratch);
        let mut let_ty = match value {
            Expr::StringLiteral(_) => "string memory".to_string(),
            Expr::BoolLiteral(_) => "bool".to_string(),
            Expr::RecordConstruct(rec, _) => format!("{} memory", rec),
            _ => infer_let_type_entity(value, entity, ctx, self.scratch),
        };
        // EVM-H17: address-shaped hex in a route returning `bytes32` must
        // bind as `bytes32`, not `uint256` / `address`.
        if let Some(ret_ty) = &_route.return_type {
            if matches!(ret_ty, crate::ast::Type::Simple(n) if n == "bytes32") {
                if let Expr::IntLiteral(v) = value {
                    if crate::ast::u256_is_address_shaped(v) {
                        let_ty = "bytes32".to_string();
                    }
                }
            }
        }
        let val_str = if let_ty == "bool" && val_str == "0" {
            "false".to_string()
        } else {
            val_str
        };
        let mut out = String::new();
        for s in &stmts {
            out.push_str(&format!("{}{}\n", indent, s));
        }
        if stmts.iter().any(|s| s.trim_start().starts_with("revert(")) {
            scratch.borrow_mut().mark_route_unreachable();
            return out;
        }
        match pattern {
            Pattern::Ident(name) => {
                if let Expr::MethodCall(base, method, args) = value {
                    if (method == "insert" || method == "update") && args.len() == 2 {
                        if let Expr::Ident(map_name) = base.as_ref() {
                            if entity
                                .members
                                .iter()
                                .any(|m| m.name == *map_name && is_mapping_type(&m.ty))
                            {
                                let mut bindings = StdHashMap::new();
                                let (setup, writes) = gen_mapping_transform_split(
                                    map_name,
                                    value,
                                    entity,
                                    &mut bindings,
                                    ctx,
                                    &self.scope,
                                    scratch,
                                );
                                for s in setup {
                                    out.push_str(&format!("{}{}\n", indent, s));
                                }
                                for s in writes {
                                    out.push_str(&format!("{}{}\n", indent, s));
                                }
                                scratch
                                    .borrow_mut()
                                    .push_hashmap_alias(name, Expr::Ident(map_name.clone()));
                                return out;
                            }
                        }
                    }
                }
                let simplified_val = simplify_hashmap_let_alias_init(value)
                    .unwrap_or_else(|| value.clone());
                if is_hashmap_valued_expr(&simplified_val, entity) {
                    if let Some(alias_ty) =
                        infer_hashmap_slot_sol_ty(&simplified_val, entity, ctx, scratch)
                    {
                        scratch.borrow_mut().push_let_binding(name, &alias_ty);
                    }
                    scratch.borrow_mut().push_hashmap_alias(name, simplified_val);
                    return out;
                }
                if let Expr::Tuple(elems) = value {
                    if tuple_contains_hashmap_slot(elems, entity) {
                        return out;
                    }
                }
                if let Some(elem_tys) = infer_tuple_elem_types(value, entity, ctx, scratch) {
                    if elem_tys.len() > 1 {
                        out.push_str(&format!(
                            "{}{}\n",
                            indent,
                            crate::codegen::solidity::core::expr::emit_tuple_ident_destructure(
                                name,
                                &elem_tys,
                                &val_str,
                                scratch,
                            )
                        ));
                        return out;
                    }
                }
                let val_str = match value {
                    Expr::IntLiteral(v) if let_ty == "address" => hex_literal_to_sol_for_ty(
                        &crate::ast::u256_hex_digits(v),
                        Some("address"),
                    ),
                    Expr::IntLiteral(v) if let_ty == "bytes32" => hex_literal_to_sol_for_ty(
                        &crate::ast::u256_hex_digits(v),
                        Some("bytes32"),
                    ),
                    _ => maybe_narrow_cast(&let_ty, &val_str, value, entity, ctx, scratch),
                };
                let san = sol_sanitize_ident(name);
                scratch.borrow_mut().push_let_binding(name, &let_ty);
                if let Expr::EnumVariantWithData(en, var, _) = value {
                    scratch
                        .borrow_mut()
                        .push_payload_enum_binding(name, en, var);
                }
                if scratch.borrow_mut().is_hoisted_local(name) {
                    // Declared at function scope (T-EVM-ST-001).
                    out.push_str(&format!("{}{} = {};\n", indent, san, val_str));
                } else {
                    out.push_str(&format!("{}{} {} = {};\n", indent, let_ty, san, val_str));
                }
            }
            Pattern::Tuple(pats) => {
                // Phase EVM-15 H4 (Cluster C): emit Solidity's
                // native tuple-destructure
                // (`(uint256 a, uint256 b) = expr;`). Wildcard slots
                // use the gap form (`,`).
                //
                // Phase EVM-P0-A (closes EVM_GAPS § 1.12): per-slot
                // type inference. Pre-P0-A every slot was hardcoded
                // to `uint256`, so a `let (owner, count) = read();`
                // whose RHS returned `(address, uint256)` got
                // `(uint256 owner, uint256 count) = read();` and
                // solc rejected the destructure. Now we walk the
                // RHS through `infer_tuple_elem_types` and fall
                // back to `uint256` per-slot only when the shape
                // is unrecognised.
                let mut binders: Vec<String> = Vec::with_capacity(pats.len());
                for (i, pat) in pats.iter().enumerate() {
                    let slot_ty = inferred_tuple
                        .as_ref()
                        .and_then(|tys| tys.get(i).cloned())
                        .unwrap_or_else(|| "uint256".to_string());
                    match pat {
                        Pattern::Ident(n) => {
                            binders.push(format!("{} {}", slot_ty, sol_sanitize_ident(n)));
                            scratch.borrow_mut().push_let_binding(n, &slot_ty);
                        }
                        Pattern::Wildcard => binders.push(String::new()),
                        _ => binders.push(String::new()),
                    }
                }
                out.push_str(&format!(
                    "{}({}) = {};\n",
                    indent,
                    binders.join(", "),
                    val_str
                ));
            }
            Pattern::Wildcard => {
                // `let _ = expr` — side effects only; discard the value.
            }
            _ => {
                out.push_str(&format!(
                    "{}revert(\"Cambrian EVM target: unsupported let pattern\");\n",
                    indent
                ));
            }
        }
        out
    }

    fn emit_return(
        &self,
        values: &[Expr],
        _entity: &Entity,
        _route: &Route,
        _program: &Program,
        indent: &str,
    ) -> String {
        let ctx = self.ctx;
        let scratch = self.scratch;
        if scratch.borrow().is_route_unreachable() {
            return String::new();
        }
        if values.is_empty() {
            return format!("{}return;\n", indent);
        }
        // Phase EVM-P0-B follow-up: cast each return value to the
        // declared route return type so narrow integer signatures
        // (e.g. `-> u8`) accept `uint256`-typed expressions like
        // `match` results / stdlib helper calls without solc rejecting
        // the implicit conversion.
        let ret_slot_ty = |idx: usize| -> Option<String> {
            match _route.return_type.as_ref()? {
                Type::Tuple(items) if values.len() == items.len() => items
                    .get(idx)
                    .map(|t| sol_type_entity(_entity, t, true, ctx)),
                Type::Tuple(_) => None,
                t if values.len() == 1 && idx == 0 => Some(sol_type_entity(_entity, t, true, ctx)),
                _ => None,
            }
        };
        let scope_for_slot = |slot: usize| -> EmitScope<'_> {
            match (_route.return_type.as_ref(), values.len()) {
                (Some(Type::Tuple(items)), _) if slot < items.len() => {
                    self.scope.with_expected_ty(Some(&items[slot]))
                }
                (Some(t), 1) if slot == 0 => self.scope.with_expected_ty(Some(t)),
                _ => self.scope,
            }
        };
        let mut out = String::new();
        if values.len() == 1 {
            let scope = scope_for_slot(0);
            let (stmts, v) = gen_expr_hoisted(&values[0], _entity, ctx, &scope, self.scratch);
            for s in &stmts {
                out.push_str(&format!("{}{}\n", indent, s));
            }
            let v = match ret_slot_ty(0) {
                Some(ty) => coerce_return_value(&ty, v, &values[0], _entity, ctx, scratch),
                None => v,
            };
            out.push_str(&format!("{}return {};\n", indent, v));
        } else {
            let mut val_strs = vec![];
            for (i, v) in values.iter().enumerate() {
                let scope = scope_for_slot(i);
                let (stmts, vs) = gen_expr_hoisted(v, _entity, ctx, &scope, self.scratch);
                for s in &stmts {
                    out.push_str(&format!("{}{}\n", indent, s));
                }
                let vs = match ret_slot_ty(i) {
                    Some(ty) => coerce_return_value(&ty, vs, v, _entity, ctx, scratch),
                    None => vs,
                };
                val_strs.push(vs);
            }
            out.push_str(&format!("{}return ({});\n", indent, val_strs.join(", ")));
        }
        out
    }

    fn emit_throw(&self, error_code: u32, indent: &str) -> String {
        format!("{}revert(\"throw({})\");\n", indent, error_code)
    }

    fn emit_send(
        &self,
        message: &Option<String>,
        args: &[Expr],
        dest: &Expr,
        send_options: Option<&Expr>,
        entity: &Entity,
        route: &Route,
        program: &Program,
        indent: &str,
    ) -> String {
        self.emit_send_with_target(
            message,
            args,
            dest,
            send_options,
            &SendTarget::Raw,
            entity,
            route,
            program,
            indent,
        )
    }

    fn emit_deploy(
        &self,
        target_entity: &str,
        send_options: Option<&Expr>,
        constructor_args: &[Expr],
        _entity: &Entity,
        _route: &Route,
        _program: &Program,
        indent: &str,
    ) -> String {
        let ctx = self.ctx;
        let value_expr = extract_send_option(send_options, "value")
            .and_then(|e| gen_expr(e, ctx, &self.scope, self.scratch))
            .unwrap_or_else(|| "0".to_string());
        let arg_strs: Vec<String> = constructor_args
            .iter()
            .map(|a| gen_expr(a, ctx, &self.scope, self.scratch).unwrap_or_else(|| "0".to_string()))
            .collect();
        let args_str = arg_strs.join(", ");
        if value_expr == "0" {
            format!(
                "{}address _deployed_{} = address(new {}({}));\n",
                indent,
                target_entity.to_lowercase(),
                target_entity,
                args_str
            )
        } else {
            format!(
                "{}address _deployed_{} = address(new {}{{value: {}}}({}));\n",
                indent,
                target_entity.to_lowercase(),
                target_entity,
                value_expr,
                args_str
            )
        }
    }

    fn emit_call_route(
        &self,
        name: &str,
        args: &[Expr],
        entity: &Entity,
        _route: &Route,
        indent: &str,
    ) -> String {
        let ctx = self.ctx;
        let arg_strs: Vec<String> = args
            .iter()
            .map(|a| gen_expr(a, ctx, &self.scope, self.scratch).unwrap_or_else(|| "0".to_string()))
            .collect();
        let target_route = entity.routes.iter().find(|r| r.name == *name);
        let prefix = if target_route.map_or(false, |r| r.is_private) {
            "_"
        } else {
            ""
        };
        format!(
            "{}{}{}({});\n",
            indent,
            prefix,
            sol_sanitize_ident(name),
            arg_strs.join(", ")
        )
    }

    fn emit_effect(
        &self,
        namespace: &str,
        name: &str,
        args: &[Expr],
        _entity: &Entity,
        _route: &Route,
        indent: &str,
    ) -> String {
        let ctx = self.ctx;
        let arg_strs: Vec<String> = args
            .iter()
            .map(|a| gen_expr(a, ctx, &self.scope, self.scratch).unwrap_or_else(|| "0".to_string()))
            .collect();
        format!(
            "{}// EVM: {}::{}({}) — no-op on EVM\n",
            indent,
            namespace,
            name,
            arg_strs.join(", ")
        )
    }

    fn format_conditional(
        &self,
        condition: &Expr,
        then_code: &str,
        else_code: &str,
        _entity: &Entity,
        _route: &Route,
        indent: &str,
    ) -> String {
        let ctx = self.ctx;
        let cond = gen_expr(condition, ctx, &self.scope, self.scratch)
            .unwrap_or_else(|| "false".to_string());
        let mut out = format!("{}if ({}) {{\n", indent, cond);
        out.push_str(then_code);
        out.push_str(&format!("{}}}", indent));
        if !else_code.is_empty() {
            out.push_str(" else {\n");
            out.push_str(else_code);
            out.push_str(&format!("{}}}", indent));
        }
        out.push('\n');
        out
    }

    fn format_rescue(
        &self,
        tag: &str,
        _inner_code: &str,
        _inner_action: &RouteAction,
        _entity: &Entity,
        _route: &Route,
        _program: &Program,
        indent: &str,
    ) -> String {
        // E26 rejects rescue/recover on the EVM domain. Force-codegen
        // must not emit try/catch (that lowering was withdrawn).
        format!(
            "{}// E26: rescue '{}' is Acki Nacki-only (TVM bounce); not lowered on EVM\n",
            indent, tag
        )
    }

    fn format_for(
        &self,
        pattern: &Pattern,
        iter: &Expr,
        body_code: &str,
        entity: &Entity,
        route: &Route,
        indent: &str,
    ) -> String {
        let scratch = self.scratch;
        let inner_indent = format!("{}    ", indent);
        let idx = scratch.borrow_mut().next_tmp();
        let count_tmp = scratch.borrow_mut().next_tmp();
        let (setup, count_expr, elem) = self.action_for_iter_elem(iter, route, entity, &idx);
        let bind_decls = match bind_loop_pattern(pattern, &elem) {
            Ok(d) => d,
            Err(diag) => {
                let escaped = diag.replace('\\', "\\\\").replace('"', "\\\"");
                return format!("{}revert(\"{}\");\n", indent, escaped);
            }
        };

        let mut out = String::new();
        for s in &setup {
            out.push_str(indent);
            out.push_str(s);
            out.push('\n');
        }
        out.push_str(&format!(
            "{}uint256 {} = {};\n",
            indent, count_tmp, count_expr
        ));
        out.push_str(&format!(
            "{}for (uint256 {} = 0; {} < {}; ++{}) {{\n",
            indent, idx, idx, count_tmp, idx
        ));
        for d in bind_decls {
            out.push_str(&inner_indent);
            out.push_str(&d);
            out.push('\n');
        }
        out.push_str(body_code);
        out.push_str(&format!("{}}}\n", indent));
        out
    }

    fn emit_var_call(
        &self,
        name: &str,
        message: &str,
        args: &[Expr],
        dest: &Expr,
        send_options: Option<&Expr>,
        entity: &Entity,
        route: &Route,
        program: &Program,
        indent: &str,
    ) -> String {
        self.emit_var_call_with_target(
            name,
            message,
            args,
            dest,
            send_options,
            &SendTarget::Raw,
            entity,
            route,
            program,
            indent,
        )
    }

    fn emit_emit(
        &self,
        event_name: &str,
        args: &[Expr],
        entity: &Entity,
        _route: &Route,
        program: &Program,
        indent: &str,
    ) -> String {
        let ctx = self.ctx;
        let scratch = self.scratch;
        // Phase EVM-P0-C: lower `emit Foo(args);` to Solidity's
        // `emit Foo(args);`. Look up the event declaration so we can
        // cast each argument to its declared parameter type — Solidity
        // is strict about narrow-int / address conversions in event
        // arg positions just like in function calls.
        let decl = entity
            .events
            .iter()
            .find(|e| e.name == event_name)
            .or_else(|| program.events.iter().find(|e| e.name == event_name));

        let mut rendered_args: Vec<String> = Vec::with_capacity(args.len());
        let mut hoisted: Vec<String> = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let (stmts, mut s) = gen_expr_hoisted(a, entity, ctx, &self.scope, self.scratch);
            hoisted.extend(stmts);
            if let Some(d) = decl {
                if let Some(p) = d.params.get(i) {
                    let target = sol_type_entity(entity, &p.ty, true, ctx);
                    s = maybe_narrow_cast(&target, &s, a, entity, ctx, scratch);
                }
            }
            rendered_args.push(s);
        }
        let mut out = String::new();
        for h in hoisted {
            out.push_str(&format!("{}{}\n", indent, h));
        }
        out.push_str(&format!(
            "{}emit {}({});\n",
            indent,
            event_name,
            rendered_args.join(", ")
        ));
        out
    }

    fn emit_throw_custom(
        &self,
        name: &str,
        args: &[Expr],
        entity: &Entity,
        _route: &Route,
        program: &Program,
        indent: &str,
    ) -> String {
        let ctx = self.ctx;
        let scratch = self.scratch;
        // Phase EVM-P0-D: lower `throw Foo(args);` to Solidity's
        // `revert Foo(args);`. Look up the error declaration so we
        // can cast each argument to its declared parameter type.
        let decl = entity
            .errors
            .iter()
            .find(|e| e.name == name)
            .or_else(|| program.errors.iter().find(|e| e.name == name));

        let mut rendered_args: Vec<String> = Vec::with_capacity(args.len());
        let mut hoisted: Vec<String> = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let (stmts, mut s) = gen_expr_hoisted(a, entity, ctx, &self.scope, self.scratch);
            hoisted.extend(stmts);
            if let Some(d) = decl {
                if let Some(p) = d.params.get(i) {
                    let target = sol_type_entity(entity, &p.ty, true, ctx);
                    s = maybe_narrow_cast(&target, &s, a, entity, ctx, scratch);
                }
            }
            rendered_args.push(s);
        }
        let mut out = String::new();
        for h in hoisted {
            out.push_str(&format!("{}{}\n", indent, h));
        }
        out.push_str(&format!(
            "{}revert {}({});\n",
            indent,
            name,
            rendered_args.join(", ")
        ));
        out
    }
}
