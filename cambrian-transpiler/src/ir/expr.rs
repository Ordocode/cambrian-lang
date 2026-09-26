// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Typed expressions and shared inference / coercion.

use std::collections::HashMap;

use crate::ast::{BinOp, Entity, Expr, Param, Type};

use super::ty::{resolve_type, ResolvedType};

#[derive(Debug, Clone, PartialEq)]
pub struct TypedExpr {
    pub ty: ResolvedType,
    pub kind: TypedExprKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypedExprKind {
    Ident(String),
    IntLiteral(String),
    HexLiteral(String),
    BinLiteral(String),
    BoolLiteral(bool),
    StringLiteral(String),
    BinOp {
        lhs: Box<TypedExpr>,
        op: BinOp,
        rhs: Box<TypedExpr>,
    },
    Coerce {
        kind: CoerceKind,
        expr: Box<TypedExpr>,
        to: ResolvedType,
    },
    Cast {
        expr: Box<TypedExpr>,
        to: ResolvedType,
    },
    MsgField(String),
    SysField(String),
    TemporalRef(String),
    FieldAccess {
        base: Box<TypedExpr>,
        field: String,
    },
    /// Escape hatch during migration — printers may fall back to AST lowering.
    AstPassthrough(Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoerceKind {
    Widen,
    Narrow,
    SignPromote,
    UsizeAlign,
    WidthCast,
}

/// Sentinel `as` target for [`materialize_typed_expr`] → [`lower_expr`] hex round-trip.
pub const IR_HEX_LITERAL_CAST: &str = "__cambrian_ir_hex_literal";
/// Sentinel `as` target for bin-literal IR round-trip.
pub const IR_BIN_LITERAL_CAST: &str = "__cambrian_ir_bin_literal";

/// When `lower_expr` sees `expr as T`, recover explicit [`CoerceKind`] if this is a
/// numeric widening / sign-promotion cast (not a user `as` cast).
pub fn coerce_kind_for_cast(from: &ResolvedType, to: &ResolvedType) -> Option<CoerceKind> {
    let (Some(lt), Some(rt)) = (parse_num_type(from), parse_num_type(to)) else {
        return None;
    };
    if lt == rt {
        return None;
    }
    if !lt.signed && rt.signed {
        return Some(CoerceKind::SignPromote);
    }
    if lt.signed && !rt.signed {
        return if lt.width > rt.width {
            Some(CoerceKind::Narrow)
        } else {
            None
        };
    }
    if lt.width < rt.width {
        return Some(CoerceKind::Widen);
    }
    if lt.width > rt.width {
        return Some(CoerceKind::Narrow);
    }
    None
}

/// Context for portable type inference shared across backends.
///
/// Solidity-specific width rules (`actual_sol_type`) and Lean BitVec numerics
/// remain in their respective printers until C2/C3.
pub struct InferCtx<'entity, 'aliases> {
    pub entity: &'entity Entity,
    pub route_params: &'entity [Param],
    pub aliases: &'aliases HashMap<String, Type>,
}

impl<'entity, 'aliases> InferCtx<'entity, 'aliases> {
    pub fn resolve(&self, ty: &Type) -> ResolvedType {
        resolve_type(ty, self.aliases)
    }
}

/// Infer the type of `expr` using the portable kernel shared by all targets.
pub fn infer_type(expr: &Expr, ctx: &InferCtx<'_, '_>) -> Option<ResolvedType> {
    match expr {
        Expr::Ident(name) => {
            if let Some(m) = ctx.entity.members.iter().find(|m| m.name == *name) {
                return Some(ctx.resolve(&m.ty));
            }
            if let Some(p) = ctx.route_params.iter().find(|p| p.name == *name) {
                return Some(ctx.resolve(&p.ty));
            }
            None
        }
        Expr::TemporalRef(name) => ctx
            .entity
            .members
            .iter()
            .find(|m| m.name == *name)
            .map(|m| ctx.resolve(&m.ty)),
        Expr::IntLiteral(v) => {
            if !v.fits_u128() {
                Some(ResolvedType::simple("U256"))
            } else if v.lo > u128::from(u64::MAX) {
                Some(ResolvedType::simple("u128"))
            } else {
                Some(ResolvedType::simple("u64"))
            }
        }
        Expr::BoolLiteral(_) => Some(ResolvedType::simple("bool")),
        Expr::StringLiteral(_) => Some(ResolvedType::simple("String")),
        Expr::Cast(_, ty) => Some(ctx.resolve(ty)),
        Expr::SysField(field) => match field.as_str() {
            "pubkey" => Some(ResolvedType::simple("pubkey")),
            "now" | "logicaltime" => Some(ResolvedType::simple("u64")),
            "rnd_seed" | "balance" => Some(ResolvedType::simple("u128")),
            "address" => Some(ResolvedType::simple("address")),
            _ => None,
        },
        Expr::MsgField(field) => match field.as_str() {
            "pubkey" => Some(ResolvedType::simple("pubkey")),
            "sender" => Some(ResolvedType::simple("address")),
            "timestamp" | "logicaltime" => Some(ResolvedType::simple("u64")),
            "value" => Some(ResolvedType::simple("u128")),
            "int" | "ext" => Some(ResolvedType::simple("bool")),
            "currencies" => Some(ResolvedType::Generic {
                name: "HashMap".to_string(),
                params: vec![
                    ResolvedType::simple("u32"),
                    ResolvedType::simple("u128"),
                ],
            }),
            _ => None,
        },
        Expr::FnCall(name, args) => match name.as_str() {
            "cam_add" | "cam_sub" | "cam_mul" => args
                .iter()
                .find_map(|a| infer_type(a, ctx)),
            _ => None,
        },
        Expr::BinOp(lhs, _, rhs) => infer_type(lhs, ctx).or_else(|| infer_type(rhs, ctx)),
        Expr::Index(base, _) => {
            if let Some(ResolvedType::Generic { name, params }) = infer_type(base, ctx) {
                if (name == "HashMap" || name == "Map") && params.len() == 2 {
                    return Some(params[1].clone());
                }
                if name == "Vec" && params.len() == 1 {
                    return Some(params[0].clone());
                }
            }
            None
        }
        Expr::FieldAccess(base, field) => {
            let ResolvedType::Simple(rec_name) = infer_type(base, ctx)? else {
                return None;
            };
            let rec = ctx.entity.records.iter().find(|r| r.name == rec_name)?;
            rec.fields
                .iter()
                .find(|f| f.name == *field)
                .map(|f| ctx.resolve(&f.ty))
        }
        Expr::MethodCall(base, method, _) => {
            if method == "cam_get" || method == "get" {
                if let Some(ResolvedType::Generic { name, params }) = infer_type(base, ctx) {
                    if (name == "HashMap" || name == "Map") && params.len() == 2 {
                        return Some(params[1].clone());
                    }
                }
            }
            if method == "clone" {
                return infer_type(base, ctx);
            }
            None
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NumType {
    width: u16,
    signed: bool,
}

fn parse_num_type(ty: &ResolvedType) -> Option<NumType> {
    match ty {
        ResolvedType::Simple(name) => match name.as_str() {
            "u8" => Some(NumType {
                width: 8,
                signed: false,
            }),
            "u16" => Some(NumType {
                width: 16,
                signed: false,
            }),
            "u32" => Some(NumType {
                width: 32,
                signed: false,
            }),
            "u64" => Some(NumType {
                width: 64,
                signed: false,
            }),
            "u128" => Some(NumType {
                width: 128,
                signed: false,
            }),
            "usize" => Some(NumType {
                width: 64,
                signed: false,
            }),
            "U256" | "uint256" => Some(NumType {
                width: 256,
                signed: false,
            }),
            "i8" => Some(NumType {
                width: 8,
                signed: true,
            }),
            "i16" => Some(NumType {
                width: 16,
                signed: true,
            }),
            "i32" => Some(NumType {
                width: 32,
                signed: true,
            }),
            "i64" => Some(NumType {
                width: 64,
                signed: true,
            }),
            "i128" => Some(NumType {
                width: 128,
                signed: true,
            }),
            _ => None,
        },
        _ => None,
    }
}

fn num_type_name(nt: &NumType) -> ResolvedType {
    if nt.width == 256 {
        ResolvedType::simple("U256")
    } else if nt.signed {
        ResolvedType::simple(format!("i{}", nt.width))
    } else {
        ResolvedType::simple(format!("u{}", nt.width))
    }
}

/// Rust-style numeric widening ladder for binary operands.
fn common_type(a: &NumType, b: &NumType) -> Option<NumType> {
    if a.signed == b.signed {
        Some(NumType {
            width: a.width.max(b.width),
            signed: a.signed,
        })
    } else {
        let (u, s) = if a.signed { (b, a) } else { (a, b) };
        if u.width >= 128 {
            None
        } else {
            let needed = (u.width * 2).max(s.width);
            if needed > 128 {
                None
            } else {
                Some(NumType {
                    width: needed,
                    signed: true,
                })
            }
        }
    }
}

/// Whether `expr` is an integer literal in the AST (matches RustCore `is_int_literal`).
pub fn is_int_literal_expr(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::IntLiteral(_)
    )
}

/// Whether `expr` has `.len()` / `as usize` shape (matches RustCore `expr_is_usize`).
pub fn expr_is_usize_shape(expr: &Expr) -> bool {
    match expr {
        Expr::MethodCall(_, method, _) if method == "len" => true,
        Expr::Cast(inner, Type::Simple(name)) if name == "usize" => expr_is_usize_shape(inner),
        _ => false,
    }
}

fn coerce_to(target: &ResolvedType, expr: TypedExpr, kind: CoerceKind) -> TypedExpr {
    TypedExpr {
        ty: target.clone(),
        kind: TypedExprKind::Coerce {
            kind,
            expr: Box::new(expr),
            to: target.clone(),
        },
    }
}

/// Widen or sign-promote `lhs` / `rhs` so both operands share a common numeric
/// type. Emits explicit [`TypedExprKind::Coerce`] nodes; language printers may
/// elide these when the host language performs implicit casts.
/// Widen/sign-promote/`usize` align binop operands. Mirrors RustCore
/// `widen_operands` + `align_usize_binop_operands` (source of truth for P6).
pub fn coerce_binop_operands(
    lhs: TypedExpr,
    rhs: TypedExpr,
    op: &BinOp,
    lhs_ast: &Expr,
    rhs_ast: &Expr,
) -> (TypedExpr, TypedExpr) {
    let l_lit = is_int_literal_expr(lhs_ast);
    let r_lit = is_int_literal_expr(rhs_ast);

    match (l_lit, r_lit) {
        (true, true) => return align_usize_operands(lhs, rhs, op, lhs_ast, rhs_ast),
        (true, false) => {
            if let Some(rt) = parse_num_type(&rhs.ty) {
                if rt.width == 256 {
                    return align_usize_operands(
                        coerce_to(&num_type_name(&rt), lhs, CoerceKind::Widen),
                        rhs,
                        op,
                        lhs_ast,
                        rhs_ast,
                    );
                }
            }
            return align_usize_operands(lhs, rhs, op, lhs_ast, rhs_ast);
        }
        (false, true) => {
            if let Some(lt) = parse_num_type(&lhs.ty) {
                if lt.width == 256 {
                    return align_usize_operands(
                        lhs,
                        coerce_to(&num_type_name(&lt), rhs, CoerceKind::Widen),
                        op,
                        lhs_ast,
                        rhs_ast,
                    );
                }
            }
            return align_usize_operands(lhs, rhs, op, lhs_ast, rhs_ast);
        }
        (false, false) => {}
    }

    let (Some(lt), Some(rt)) = (parse_num_type(&lhs.ty), parse_num_type(&rhs.ty)) else {
        return align_usize_operands(lhs, rhs, op, lhs_ast, rhs_ast);
    };

    if lt == rt {
        return align_usize_operands(lhs, rhs, op, lhs_ast, rhs_ast);
    }

    let target = match common_type(&lt, &rt) {
        Some(t) => t,
        None => return align_usize_operands(lhs, rhs, op, lhs_ast, rhs_ast),
    };
    let target_ty = num_type_name(&target);

    let lhs = if lt != target {
        let kind = if !lt.signed && target.signed {
            CoerceKind::SignPromote
        } else {
            CoerceKind::Widen
        };
        coerce_to(&target_ty, lhs, kind)
    } else {
        lhs
    };

    let rhs = if rt != target {
        let kind = if !rt.signed && target.signed {
            CoerceKind::SignPromote
        } else {
            CoerceKind::Widen
        };
        coerce_to(&target_ty, rhs, kind)
    } else {
        rhs
    };

    align_usize_operands(lhs, rhs, op, lhs_ast, rhs_ast)
}

fn align_usize_operands(
    lhs: TypedExpr,
    rhs: TypedExpr,
    op: &BinOp,
    lhs_ast: &Expr,
    rhs_ast: &Expr,
) -> (TypedExpr, TypedExpr) {
    if !matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) {
        return (lhs, rhs);
    }
    let l_usize = expr_is_usize_shape(lhs_ast);
    let r_usize = expr_is_usize_shape(rhs_ast);
    if l_usize && !r_usize {
        (
            coerce_to(&ResolvedType::simple("u64"), lhs, CoerceKind::UsizeAlign),
            rhs,
        )
    } else if r_usize && !l_usize {
        (
            lhs,
            coerce_to(&ResolvedType::simple("u64"), rhs, CoerceKind::UsizeAlign),
        )
    } else {
        (lhs, rhs)
    }
}

pub(crate) fn infer_binop_type(lhs: &ResolvedType, rhs: &ResolvedType) -> ResolvedType {
    match (
        parse_num_type(lhs),
        parse_num_type(rhs),
    ) {
        (Some(l), Some(r)) => common_type(&l, &r)
            .map(|t| num_type_name(&t))
            .unwrap_or_else(|| lhs.clone()),
        _ => lhs.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::BinOp;

    fn parse(src: &str) -> crate::ast::Program {
        crate::ProgramParser::new()
            .parse(src)
            .unwrap_or_else(|e| panic!("parse failed: {e}"))
    }

    #[test]
    fn infer_member_ident() {
        let program = parse(
            r#"
            entity E {
                routes { noop() => [] }
                m_count: u64 { in noop() => 0 }
            }
            "#,
        );
        let entity = &program.entities[0];
        let ctx = InferCtx {
            entity,
            route_params: &[],
            aliases: &HashMap::new(),
        };
        let ty = infer_type(&Expr::Ident("m_count".to_string()), &ctx);
        assert_eq!(ty, Some(ResolvedType::simple("u64")));
    }

    #[test]
    fn binop_u8_u64_widens_u8() {
        let u8_ty = ResolvedType::simple("u8");
        let u64_ty = ResolvedType::simple("u64");
        let lhs = TypedExpr {
            ty: u8_ty,
            kind: TypedExprKind::Ident("a".to_string()),
        };
        let rhs = TypedExpr {
            ty: u64_ty,
            kind: TypedExprKind::Ident("b".to_string()),
        };
        let lhs_ast = Expr::Ident("a".to_string());
        let rhs_ast = Expr::Ident("b".to_string());
        let (lhs, rhs) = coerce_binop_operands(lhs, rhs, &BinOp::Add, &lhs_ast, &rhs_ast);
        assert!(matches!(
            lhs.kind,
            TypedExprKind::Coerce {
                kind: CoerceKind::Widen,
                ..
            }
        ));
        assert!(matches!(rhs.kind, TypedExprKind::Ident(_)));
        assert_eq!(lhs.ty, ResolvedType::simple("u64"));
        assert_eq!(rhs.ty, ResolvedType::simple("u64"));
    }

    #[test]
    fn coerce_kind_for_cast_u8_to_u64_is_widen() {
        assert_eq!(
            coerce_kind_for_cast(&ResolvedType::simple("u8"), &ResolvedType::simple("u64")),
            Some(CoerceKind::Widen)
        );
    }

    #[test]
    fn binop_mixed_sign_promotes() {
        let lhs = TypedExpr {
            ty: ResolvedType::simple("u32"),
            kind: TypedExprKind::Ident("a".to_string()),
        };
        let rhs = TypedExpr {
            ty: ResolvedType::simple("i32"),
            kind: TypedExprKind::Ident("b".to_string()),
        };
        let lhs_ast = Expr::Ident("a".to_string());
        let rhs_ast = Expr::Ident("b".to_string());
        let (lhs, _) = coerce_binop_operands(lhs, rhs, &BinOp::Add, &lhs_ast, &rhs_ast);
        assert!(matches!(
            lhs.kind,
            TypedExprKind::Coerce {
                kind: CoerceKind::SignPromote,
                ..
            }
        ));
        assert_eq!(lhs.ty, ResolvedType::simple("i64"));
    }

    #[test]
    fn binop_usize_len_aligns() {
        let lhs_ast = Expr::MethodCall(
            Box::new(Expr::Ident("xs".to_string())),
            "len".to_string(),
            vec![],
        );
        let rhs_ast = Expr::IntLiteral(cambrian_core::U256::ONE);
        let lhs = TypedExpr {
            ty: ResolvedType::simple("usize"),
            kind: TypedExprKind::AstPassthrough(Box::new(lhs_ast.clone())),
        };
        let rhs = TypedExpr {
            ty: ResolvedType::simple("u64"),
            kind: TypedExprKind::IntLiteral("1".to_string()),
        };
        let (lhs, _) = coerce_binop_operands(lhs, rhs, &BinOp::Add, &lhs_ast, &rhs_ast);
        assert!(matches!(
            lhs.kind,
            TypedExprKind::Coerce {
                kind: CoerceKind::UsizeAlign,
                ..
            }
        ));
    }
}
