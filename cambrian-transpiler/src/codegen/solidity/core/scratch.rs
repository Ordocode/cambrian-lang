// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM codegen — per-emission scratch state (temp names, let-binding stack).

use crate::ast::{Expr, Type};
use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
    /// Test-codegen bridge: `generate_evm_tests` resets this once per program.
    static ACTIVE_SCRATCH: RefCell<EmitScratch> = RefCell::new(EmitScratch::new());
}

/// Per-program emission scratch: temp counter and let-binding type stack.
pub struct EmitScratch {
    pub tmp_cnt: u32,
    let_binding_types: HashMap<String, Vec<String>>,
    /// Stack of component local names for `let name = (a, b, …)` bindings.
    tuple_binding_components: HashMap<String, Vec<Vec<String>>>,
    /// Stack of `(enum_name, variant_name)` for payload-enum `let` bindings.
    payload_enum_bindings: HashMap<String, Vec<(String, String)>>,
    hoisted_locals: HashMap<String, String>,
    /// Stack of inlined HashMap expressions for route-level `let`
    /// bindings that cannot become Solidity locals.
    hashmap_alias_exprs: HashMap<String, Vec<Expr>>,
    /// Set when a route action emits `revert(...)` without binding locals
    /// (e.g. unsupported `let v = evm::unknown(...)`). Later `return` actions
    /// in the same route are skipped to avoid undeclared identifiers.
    route_unreachable: bool,
    /// Member transform currently being lowered (`emit_transforms_sol`).
    transform_member_name: Vec<String>,
    transform_member_ty: Vec<Type>,
}

impl EmitScratch {
    pub fn new() -> Self {
        Self {
            tmp_cnt: 0,
            let_binding_types: HashMap::new(),
            tuple_binding_components: HashMap::new(),
            payload_enum_bindings: HashMap::new(),
            hoisted_locals: HashMap::new(),
            hashmap_alias_exprs: HashMap::new(),
            route_unreachable: false,
            transform_member_name: Vec::new(),
            transform_member_ty: Vec::new(),
        }
    }

    pub fn clear_route_unreachable(&mut self) {
        self.route_unreachable = false;
    }

    pub fn mark_route_unreachable(&mut self) {
        self.route_unreachable = true;
    }

    pub fn is_route_unreachable(&self) -> bool {
        self.route_unreachable
    }

    pub fn next_tmp(&mut self) -> String {
        let n = self.tmp_cnt;
        self.tmp_cnt += 1;
        format!("_cam_tmp{}", n)
    }

    pub fn set_hoisted_locals(&mut self, locals: HashMap<String, String>) {
        self.hoisted_locals = locals;
    }

    pub fn clear_hoisted_locals(&mut self) {
        self.hoisted_locals.clear();
    }

    pub fn is_hoisted_local(&self, name: &str) -> bool {
        self.hoisted_locals.contains_key(name)
    }

    pub fn push_let_binding(&mut self, name: &str, ty: &str) {
        self.let_binding_types
            .entry(name.to_string())
            .or_default()
            .push(ty.to_string());
    }

    pub fn pop_let_binding(&mut self, name: &str) {
        if let Some(stack) = self.let_binding_types.get_mut(name) {
            stack.pop();
        }
    }

    pub fn lookup_let_binding(&self, name: &str) -> Option<String> {
        self.let_binding_types
            .get(name)
            .and_then(|stack| stack.last().cloned())
    }

    pub fn push_tuple_binding(&mut self, name: &str, components: Vec<String>) {
        self.tuple_binding_components
            .entry(name.to_string())
            .or_default()
            .push(components);
    }

    pub fn pop_tuple_binding(&mut self, name: &str) {
        if let Some(stack) = self.tuple_binding_components.get_mut(name) {
            stack.pop();
        }
    }

    pub fn lookup_tuple_component(&self, name: &str, idx: usize) -> Option<String> {
        self.tuple_binding_components
            .get(name)
            .and_then(|stack| stack.last())
            .and_then(|comps| comps.get(idx).cloned())
    }

    pub fn push_payload_enum_binding(&mut self, name: &str, enum_name: &str, variant: &str) {
        self.payload_enum_bindings
            .entry(name.to_string())
            .or_default()
            .push((enum_name.to_string(), variant.to_string()));
    }

    pub fn pop_payload_enum_binding(&mut self, name: &str) {
        if let Some(stack) = self.payload_enum_bindings.get_mut(name) {
            stack.pop();
        }
    }

    pub fn lookup_payload_enum_binding(&self, name: &str) -> Option<(String, String)> {
        self.payload_enum_bindings
            .get(name)
            .and_then(|stack| stack.last().cloned())
    }

    pub fn push_hashmap_alias(&mut self, name: &str, expr: Expr) {
        self.hashmap_alias_exprs
            .entry(name.to_string())
            .or_default()
            .push(expr);
    }

    pub fn pop_hashmap_alias(&mut self, name: &str) {
        if let Some(stack) = self.hashmap_alias_exprs.get_mut(name) {
            stack.pop();
        }
    }

    pub fn lookup_hashmap_alias(&self, name: &str) -> Option<Expr> {
        self.hashmap_alias_exprs
            .get(name)
            .and_then(|stack| stack.last().cloned())
    }

    pub fn push_transform_member(&mut self, name: &str, ty: &Type) {
        self.transform_member_name.push(name.to_string());
        self.transform_member_ty.push(ty.clone());
    }

    pub fn pop_transform_member(&mut self) {
        self.transform_member_name.pop();
        self.transform_member_ty.pop();
    }

    pub fn lookup_transform_member_record(&self) -> Option<String> {
        self.transform_member_ty
            .last()
            .and_then(|ty| match ty {
                Type::Simple(rec) => Some(rec.clone()),
                _ => None,
            })
    }

    pub fn lookup_transform_member_name(&self) -> Option<String> {
        self.transform_member_name.last().cloned()
    }
}

pub(crate) fn reset_active_scratch() {
    ACTIVE_SCRATCH.with(|c| *c.borrow_mut() = EmitScratch::new());
}

pub(crate) fn with_active_scratch<F, R>(f: F) -> R
where
    F: FnOnce(&RefCell<EmitScratch>) -> R,
{
    ACTIVE_SCRATCH.with(|c| f(c))
}
