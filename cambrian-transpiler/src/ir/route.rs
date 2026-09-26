// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Route-level IR nodes.

use crate::analysis::SendTarget;
use crate::ast::{Expr, FromClause, Pattern, RouteAction, WhereClause};

use super::expr::TypedExpr;

#[derive(Debug, Clone, PartialEq)]
pub struct ProgramIr {
    pub routes: std::collections::HashMap<(String, String), RouteIr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RouteIr {
    pub entity: String,
    pub name: String,
    /// Whether the route can fail under evm×lean fail-closure policy.
    pub fail_mode: bool,
    /// `has_unphased_sends || phased_needs_world_thread`.
    pub needs_world: bool,
    pub phases: Vec<PhaseIr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhaseIr {
    /// `None` = unphased body.
    pub name: Option<String>,
    pub from_guards: Vec<FromClause>,
    pub where_guards: Vec<WhereClause>,
    /// Member transforms in temporal dependency order.
    pub transforms: Vec<IrTransform>,
    pub stmts: Vec<IrStmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IrTransform {
    pub member: String,
    pub phase: Option<String>,
    pub body: TypedExpr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IrStmt {
    Let {
        pattern: Pattern,
        value: TypedExpr,
    },
    Return {
        values: Vec<TypedExpr>,
    },
    Throw {
        code: u32,
    },
    ThrowCustom {
        name: String,
        args: Vec<TypedExpr>,
    },
    Emit {
        event: String,
        args: Vec<TypedExpr>,
    },
    Conditional {
        cond: TypedExpr,
        then_stmts: Vec<IrStmt>,
        else_stmts: Vec<IrStmt>,
    },
    For {
        pattern: Pattern,
        iter: TypedExpr,
        body: Vec<IrStmt>,
    },
    Effect {
        namespace: String,
        name: String,
        args: Vec<TypedExpr>,
    },
    Send {
        message: Option<String>,
        args: Vec<TypedExpr>,
        dest: TypedExpr,
        target: SendTarget,
        send_options: Option<Expr>,
    },
    VarCall {
        name: String,
        message: String,
        args: Vec<TypedExpr>,
        dest: TypedExpr,
        target: SendTarget,
        send_options: Option<Expr>,
    },
    Deploy {
        entity: String,
        constructor_args: Vec<TypedExpr>,
        send_options: Option<Expr>,
    },
    CallRoute {
        name: String,
        args: Vec<TypedExpr>,
    },
    Rescue {
        tag: String,
        action: Box<IrStmt>,
    },
    UpdateCode {
        update_args: Vec<TypedExpr>,
        callback_route: String,
        callback_args: Vec<TypedExpr>,
    },
    /// Escape hatch during migration.
    AstAction(RouteAction),
}
