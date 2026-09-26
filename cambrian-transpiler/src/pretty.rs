// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use crate::ast::*;

pub fn pretty_print(program: &Program) -> String {
    let mut out = String::new();

    for imp in &program.imports {
        out.push_str(&format!("use {}\n", imp.namespace));
    }
    if !program.imports.is_empty() { out.push('\n'); }

    for ta in &program.type_aliases {
        out.push_str(&format!("type {} = {}\n", ta.name, fmt_type(&ta.ty)));
    }
    if !program.type_aliases.is_empty() { out.push('\n'); }

    for rec in &program.records {
        pp_record(&mut out, rec, 0);
        out.push('\n');
    }

    for en in &program.enums {
        pp_enum(&mut out, en, 0);
        out.push('\n');
    }

    for f in &program.pure_fns {
        pp_pure_fn(&mut out, f);
        out.push('\n');
    }

    for entity in &program.entities {
        pp_entity(&mut out, entity);
        out.push('\n');
    }

    for test in &program.tests {
        pp_test(&mut out, test);
        out.push('\n');
    }

    for prop in &program.properties {
        pp_property(&mut out, prop);
        out.push('\n');
    }

    for inv in &program.invariants {
        pp_invariant(&mut out, inv);
        out.push('\n');
    }

    out
}

fn indent(n: usize) -> String {
    "  ".repeat(n)
}

fn pp_pure_fn(out: &mut String, f: &PureFn) {
    let params: Vec<String> = f.params.iter()
        .map(|p| format!("{}: {}", p.name, fmt_type(&p.ty)))
        .collect();
    out.push_str(&format!("pure fn {}({}) -> {} {{\n", f.name, params.join(", "), fmt_type(&f.return_type)));
    out.push_str(&format!("  {}\n", fmt_expr(&f.body)));
    out.push_str("}\n");
}

fn pp_entity(out: &mut String, entity: &Entity) {
    out.push_str(&format!("entity {} {{\n", entity.name));

    for ta in &entity.type_aliases {
        out.push_str(&format!("{}type {} = {}\n", indent(1), ta.name, fmt_type(&ta.ty)));
    }
    if !entity.type_aliases.is_empty() { out.push('\n'); }

    for rec in &entity.records {
        pp_record(out, rec, 1);
    }

    for en in &entity.enums {
        pp_enum(out, en, 1);
    }

    for c in &entity.constants {
        out.push_str(&format!("{}const {}: {} = {}\n", indent(1), c.name, fmt_type(&c.ty), fmt_expr(&c.value)));
    }
    if !entity.constants.is_empty() { out.push('\n'); }

    for mac in &entity.macros {
        let params: Vec<String> = mac.params.iter()
            .map(|p| format!("{}: {}", p.name, fmt_type(&p.ty)))
            .collect();
        out.push_str(&format!("{}macro {}({}) -> {} = {{\n", indent(1), mac.name, params.join(", "), fmt_type(&mac.return_type)));
        out.push_str(&format!("{}  {}\n", indent(1), fmt_expr(&mac.body)));
        out.push_str(&format!("{}}}\n", indent(1)));
    }
    if !entity.macros.is_empty() { out.push('\n'); }

    if !entity.routes.is_empty() {
        out.push_str(&format!("{}routes {{\n", indent(1)));
        for route in &entity.routes {
            pp_route(out, route, 2);
        }
        out.push_str(&format!("{}}}\n\n", indent(1)));
    }

    for member in &entity.members {
        pp_member(out, member, 1);
    }

    out.push_str("}\n");
}

fn pp_record(out: &mut String, rec: &Record, depth: usize) {
    let fields: Vec<String> = rec.fields.iter()
        .map(|f| format!("{}: {}", f.name, fmt_type(&f.ty)))
        .collect();
    out.push_str(&format!("{}record {} {{ {} }}\n", indent(depth), rec.name, fields.join(", ")));
}

fn pp_enum(out: &mut String, en: &EnumDecl, depth: usize) {
    let variants: Vec<String> = en.variants.iter()
        .map(|v| {
            if v.fields.is_empty() {
                v.name.clone()
            } else {
                let types: Vec<String> = v.fields.iter().map(fmt_type).collect();
                format!("{}({})", v.name, types.join(", "))
            }
        })
        .collect();
    out.push_str(&format!("{}enum {} {{ {} }}\n", indent(depth), en.name, variants.join(", ")));
}

fn pp_route(out: &mut String, route: &Route, depth: usize) {
    let params: Vec<String> = route.params.iter()
        .map(|p| format!("{}: {}", p.name, fmt_type(&p.ty)))
        .collect();
    let ret = route.return_type.as_ref()
        .map(|t| format!(" -> {}", fmt_type(t)))
        .unwrap_or_default();
    if let Some(ref tag) = route.recover_tag {
        let kw = if route.name.starts_with("onBounce_") { "onBounce" } else { "recover" };
        out.push_str(&format!("{}{} {}({})\n", indent(depth), kw, tag, params.join(", ")));
    } else {
        if route.factory_only {
            out.push_str(&format!("{}#[factory_only]\n", indent(depth)));
        }
        let prefix = if route.is_init { "init " } else if route.is_pure { "pure " } else if route.is_view { "view " } else if route.is_accept { "accept " } else if route.is_private { "private " } else { "" };
        out.push_str(&format!("{}{}{}({}){}\n", indent(depth), prefix, route.name, params.join(", "), ret));
    }

    if !route.from_clauses.is_empty() {
        let clauses: Vec<String> = route.from_clauses.iter().map(|fc| {
            if matches!(fc.kind, crate::ast::FromClauseKind::Member) {
                return fc.entity_name.clone();
            }
            let args: Vec<String> = fc.args.iter().map(fmt_expr).collect();
            let with_str = match &fc.with_params {
                Some(expr) => format!(" with {}", fmt_expr(expr)),
                None => String::new(),
            };
            format!("{}({}){}", fc.entity_name, args.join(", "), with_str)
        }).collect();
        out.push_str(&format!("{}  from {}\n", indent(depth), clauses.join(" | ")));
    }

    if !route.where_clauses.is_empty() {
        let clauses: Vec<String> = route.where_clauses.iter()
            .map(|wc| format!("{} : throw {}", fmt_expr(&wc.condition), wc.error_code))
            .collect();
        out.push_str(&format!("{}  where {}\n", indent(depth), clauses.join(" && ")));
    }

    out.push_str(&format!("{}  => [", indent(depth)));
    if route.body.is_empty() {
        out.push_str("]\n");
    } else {
        out.push('\n');
        match &route.body {
            RouteBody::Unphased(actions) => {
                for action in actions {
                    pp_route_action(out, action, depth + 2);
                }
            }
            RouteBody::Phased(phases) => {
                for phase in phases {
                    pp_phase_header(out, phase, depth + 2);
                    for action in &phase.actions {
                        pp_route_action(out, action, depth + 3);
                    }
                    out.push_str(&format!("{}]\n", indent(depth + 2)));
                }
            }
            RouteBody::Mixed(phases, actions) => {
                for phase in phases {
                    pp_phase_header(out, phase, depth + 2);
                    for action in &phase.actions {
                        pp_route_action(out, action, depth + 3);
                    }
                    out.push_str(&format!("{}]\n", indent(depth + 2)));
                }
                for action in actions {
                    pp_route_action(out, action, depth + 2);
                }
            }
        }
        out.push_str(&format!("{}]\n", indent(depth + 1)));
    }
}

fn pp_phase_header(out: &mut String, phase: &PhaseBlock, depth: usize) {
    if phase.where_clauses.is_empty() {
        out.push_str(&format!("{}{}: [\n", indent(depth), phase.name));
    } else {
        let parts: Vec<String> = phase.where_clauses.iter()
            .map(|wc| format!("{} : throw {}", fmt_expr(&wc.condition), wc.error_code))
            .collect();
        out.push_str(&format!("{}{} where {}: [\n",
            indent(depth), phase.name, parts.join(" && ")));
    }
}

fn pp_route_action(out: &mut String, action: &RouteAction, depth: usize) {
    match action {
        RouteAction::Let { pattern, value } => {
            out.push_str(&format!("{}let {} = {};\n", indent(depth), fmt_pattern(pattern), fmt_expr(value)));
        }
        RouteAction::Return { values } => {
            let vals: Vec<String> = values.iter().map(fmt_expr).collect();
            out.push_str(&format!("{}return({})\n", indent(depth), vals.join(", ")));
        }
        RouteAction::Send { message, args, dest, send_options } => {
            let a: Vec<String> = args.iter().map(fmt_expr).collect();
            let opts_str = match send_options {
                None => String::new(),
                Some(e) => format!(" with {}", fmt_expr(e)),
            };
            match message {
                Some(name) => out.push_str(&format!("{}{}({}) ~> {}{}\n", indent(depth), name, a.join(", "), fmt_expr(dest), opts_str)),
                None => out.push_str(&format!("{}~> {}{}\n", indent(depth), fmt_expr(dest), opts_str)),
            }
        }
        RouteAction::Effect { namespace, name, args } => {
            let a: Vec<String> = args.iter().map(fmt_expr).collect();
            out.push_str(&format!("{}{}::{}({})\n", indent(depth), namespace, name, a.join(", ")));
        }
        RouteAction::Conditional { condition, then_actions, else_actions } => {
            out.push_str(&format!("{}if {} => [\n", indent(depth), fmt_expr(condition)));
            for a in then_actions {
                pp_route_action(out, a, depth + 1);
            }
            out.push_str(&format!("{}]\n", indent(depth)));
            if !else_actions.is_empty() {
                out.push_str(&format!("{}else [\n", indent(depth)));
                for a in else_actions {
                    pp_route_action(out, a, depth + 1);
                }
                out.push_str(&format!("{}]\n", indent(depth)));
            }
        }
        RouteAction::Deploy { entity, send_options, constructor_args } => {
            let opts_str = match send_options {
                None => String::new(),
                Some(e) => format!(" with {}", fmt_expr(e)),
            };
            let args_str = if constructor_args.is_empty() {
                String::new()
            } else {
                format!(" ({})", constructor_args.iter().map(fmt_expr).collect::<Vec<_>>().join(", "))
            };
            out.push_str(&format!("{}deploy {}{}{}\n", indent(depth), entity, args_str, opts_str));
        }
        RouteAction::Rescue { tag, action } => {
            out.push_str(&format!("{}rescue {}: ", indent(depth), tag));
            let mut inner = String::new();
            pp_route_action(&mut inner, action, 0);
            out.push_str(inner.trim_start());
        }
        RouteAction::Throw { error_code } => {
            out.push_str(&format!("{}throw {}\n", indent(depth), error_code));
        }
        RouteAction::ThrowCustom { name, args } => {
            let a: Vec<String> = args.iter().map(fmt_expr).collect();
            if args.is_empty() {
                out.push_str(&format!("{}throw {}\n", indent(depth), name));
            } else {
                out.push_str(&format!("{}throw {}({})\n", indent(depth), name, a.join(", ")));
            }
        }
        RouteAction::CallRoute { name, args } => {
            let a: Vec<String> = args.iter().map(fmt_expr).collect();
            out.push_str(&format!("{}call {}({})\n", indent(depth), name, a.join(", ")));
        }
        RouteAction::VarCall { name, message, args, dest, send_options } => {
            let a: Vec<String> = args.iter().map(|e| fmt_expr(e)).collect();
            let opts_str = match send_options {
                None => String::new(),
                Some(e) => format!(" with {}", fmt_expr(e)),
            };
            out.push_str(&format!("{}var {} = {}({}) ~> {}{};\n", indent(depth), name, message, a.join(", "), fmt_expr(dest), opts_str));
        }
        RouteAction::UpdateCode { update_args, callback_route, callback_args } => {
            let ua: Vec<String> = update_args.iter().map(fmt_expr).collect();
            let ca: Vec<String> = callback_args.iter().map(fmt_expr).collect();
            out.push_str(&format!("{}gosh::updateCode({}) with {}({})\n",
                indent(depth), ua.join(", "), callback_route, ca.join(", ")));
        }
        RouteAction::For { pattern, iter, body } => {
            out.push_str(&format!("{}for {} in {} => [\n",
                indent(depth), fmt_pattern(pattern), fmt_expr(iter)));
            for a in body {
                pp_route_action(out, a, depth + 1);
            }
            out.push_str(&format!("{}]\n", indent(depth)));
        }
        RouteAction::Emit { event_name, args } => {
            let a: Vec<String> = args.iter().map(fmt_expr).collect();
            out.push_str(&format!("{}emit {}({});\n", indent(depth), event_name, a.join(", ")));
        }
    }
}

fn needs_block_wrap(expr: &Expr) -> bool {
    matches!(expr, Expr::Let(..) | Expr::If(..) | Expr::Match(..) | Expr::For(..))
}

fn pp_member(out: &mut String, member: &Member, depth: usize) {
    if member.is_identity {
        out.push_str(&format!("{}identity {}: {}\n", indent(depth), member.name, fmt_type(&member.ty)));
        return;
    }
    let default = member.default_value.as_ref()
        .map(|v| format!(" = {}", fmt_expr(v)))
        .unwrap_or_default();
    out.push_str(&format!("{}{}: {}{} {{\n", indent(depth), member.name, fmt_type(&member.ty), default));
    for t in &member.transforms {
        let params: Vec<String> = t.params.iter().map(fmt_pattern).collect();
        let phase_prefix = t.phase.as_ref().map(|p| format!("{}: ", p)).unwrap_or_default();
        if needs_block_wrap(&t.body) {
            out.push_str(&format!("{}in {}({}) => {}{{ {} }}\n", indent(depth + 1), t.route_name, params.join(", "), phase_prefix, fmt_expr(&t.body)));
        } else {
            out.push_str(&format!("{}in {}({}) => {}{}\n", indent(depth + 1), t.route_name, params.join(", "), phase_prefix, fmt_expr(&t.body)));
        }
    }
    out.push_str(&format!("{}}}\n", indent(depth)));
}

pub fn fmt_type(ty: &Type) -> String {
    match ty {
        Type::Simple(name) => name.clone(),
        Type::Generic(name, params) => {
            let p: Vec<String> = params.iter().map(fmt_type).collect();
            format!("{}<{}>", name, p.join(", "))
        }
        Type::Tuple(elems) => {
            let e: Vec<String> = elems.iter().map(fmt_type).collect();
            format!("({})", e.join(", "))
        }
        Type::TypedAddress(entity) => format!("Address<{}>", entity),
    }
}

pub fn fmt_expr(expr: &Expr) -> String {
    match expr {
        Expr::IntLiteral(v) => {
            if !v.fits_u128() {
                format!("0x{}", crate::ast::u256_hex_digits(v))
            } else {
                v.to_display_decimal()
            }
        }
        Expr::StringLiteral(s) => format!("\"{}\"", s),
        Expr::BytesLiteral(bytes) => {
            format!("b\"{}\"", String::from_utf8_lossy(bytes))
        }
        Expr::BoolLiteral(b) => b.to_string(),
        Expr::ArrayLit(elems) => {
            let es: Vec<String> = elems.iter().map(fmt_expr).collect();
            format!("array({})", es.join(", "))
        }
        Expr::EmptyCollection => "{}".to_string(),
        Expr::Ident(name) => name.clone(),
        Expr::TemporalRef(name) => format!("^{}", name),
        Expr::MacroRef(name, args) => {
            let a: Vec<String> = args.iter().map(fmt_expr).collect();
            format!("@{}({})", name, a.join(", "))
        }
        Expr::MsgField(field) => format!("msg::{}", field),
        Expr::SysField(field) => format!("sys::{}", field),
        Expr::TraceField(field) => format!("trace::{}", field),
        Expr::TraceCall { name, route } => format!("trace::{}({})", name, route),
        Expr::BinOp(lhs, op, rhs) => {
            let op_str = match op {
                BinOp::Add => "+", BinOp::Sub => "-", BinOp::Mul => "*",
                BinOp::Div => "/", BinOp::Mod => "%",
                BinOp::WrappingAdd => "+%", BinOp::WrappingSub => "-%", BinOp::WrappingMul => "*%",
                BinOp::BitAnd => "&", BinOp::BitOr => "|", BinOp::BitXor => "^",
                BinOp::Shl => "<<", BinOp::Shr => ">>",
                BinOp::Eq => "==", BinOp::Ne => "!=",
                BinOp::Lt => "<", BinOp::Le => "<=", BinOp::Gt => ">", BinOp::Ge => ">=",
                BinOp::And => "&&", BinOp::Or => "||",
            };
            format!("({} {} {})", fmt_expr(lhs), op_str, fmt_expr(rhs))
        }
        Expr::UnaryOp(op, inner) => {
            let op_str = match op {
                UnaryOp::Not => "!", UnaryOp::Neg => "-", UnaryOp::Deref => "*",
            };
            format!("{}{}", op_str, fmt_expr(inner))
        }
        Expr::FieldAccess(base, field) => format!("{}.{}", fmt_expr(base), field),
        Expr::Index(base, idx) => format!("{}[{}]", fmt_expr(base), fmt_expr(idx)),
        Expr::MethodCall(base, method, args) => {
            let a: Vec<String> = args.iter().map(fmt_expr).collect();
            if a.is_empty() {
                format!("{}.{}()", fmt_expr(base), method)
            } else {
                format!("{}.{}({})", fmt_expr(base), method, a.join(", "))
            }
        }
        Expr::FnCall(name, args) => {
            let a: Vec<String> = args.iter().map(fmt_expr).collect();
            format!("{}({})", name, a.join(", "))
        }
        Expr::If(cond, then_br, else_br) => {
            if let Some(e) = else_br {
                format!("if {} {{ {} }} else {{ {} }}", fmt_expr(cond), fmt_expr(then_br), fmt_expr(e))
            } else {
                format!("if {} {{ {} }}", fmt_expr(cond), fmt_expr(then_br))
            }
        }
        Expr::Let(pat, val, body) => {
            format!("let {} = {}; {}", fmt_pattern(pat), fmt_expr(val), fmt_expr(body))
        }
        Expr::Block(exprs) => {
            let es: Vec<String> = exprs.iter().map(fmt_expr).collect();
            format!("{{ {} }}", es.join("; "))
        }
        Expr::RecordConstruct(name, fields) => {
            let fs: Vec<String> = fields.iter()
                .map(|(n, v)| format!("{}: {}", n, fmt_expr(v)))
                .collect();
            format!("{} {{ {} }}", name, fs.join(", "))
        }
        Expr::RecordUpdate(base, fields) => {
            let fs: Vec<String> = fields.iter()
                .map(|(n, v)| format!("{}: {}", n, fmt_expr(v)))
                .collect();
            format!("{} {{ {} }}", fmt_expr(base), fs.join(", "))
        }
        Expr::Closure(params, body) => {
            let ps: Vec<String> = params.iter().map(fmt_pattern).collect();
            format!("|{}| {}", ps.join(", "), fmt_expr(body))
        }
        Expr::Cast(inner, ty) => format!("{} as {}", fmt_expr(inner), fmt_type(ty)),
        Expr::Tuple(elems) => {
            let es: Vec<String> = elems.iter().map(fmt_expr).collect();
            format!("({})", es.join(", "))
        }
        Expr::Match(subject, arms) => {
            let a: Vec<String> = arms.iter()
                .map(|arm| format!("{} => {}", fmt_match_pattern(&arm.pattern), fmt_expr(&arm.body)))
                .collect();
            format!("match {} {{ {} }}", fmt_expr(subject), a.join(", "))
        }
        Expr::EnumVariant(enum_name, variant) => format!("{}::{}", enum_name, variant),
        Expr::EnumVariantWithData(enum_name, variant, args) => {
            let a: Vec<String> = args.iter().map(fmt_expr).collect();
            format!("{}::{}({})", enum_name, variant, a.join(", "))
        }
        Expr::Some(inner) => format!("some({})", fmt_expr(inner)),
        Expr::None => "none".to_string(),
        Expr::Range(start, end) => format!("{}..{}", fmt_expr(start), fmt_expr(end)),
        Expr::For(pat, iter, body) => {
            format!("for {} in {} {{ {} }}", fmt_pattern(pat), fmt_expr(iter), fmt_expr(body))
        }
        Expr::NamespacedCall { namespace, name, args, type_params } => {
            let a: Vec<String> = args.iter().map(fmt_expr).collect();
            if type_params.is_empty() {
                format!("{}::{}({})", namespace, name, a.join(", "))
            } else {
                let tp: Vec<String> = type_params.iter().map(fmt_type).collect();
                format!("{}::{}::<{}>({})", namespace, name, tp.join(", "), a.join(", "))
            }
        }
        Expr::AddressOf { entity_name, args, with_params } => {
            let a: Vec<String> = args.iter().map(fmt_expr).collect();
            if with_params.is_empty() {
                format!("address_of {}({})", entity_name, a.join(", "))
            } else {
                let wp: Vec<String> = with_params.iter()
                    .map(|(k, v)| format!("{}: {}", k, fmt_expr(v)))
                    .collect();
                format!("address_of {}({}) with {{ {} }}", entity_name, a.join(", "), wp.join(", "))
            }
        }
        Expr::Encode { target_type, value } => {
            format!("cam_encode<{}>({})", fmt_type(target_type), fmt_expr(value))
        }
    }
}

fn fmt_match_pattern(pat: &MatchPattern) -> String {
    match pat {
        MatchPattern::EnumVariant(e, v) => format!("{}::{}", e, v),
        MatchPattern::EnumVariantWithData(e, v, bindings) => {
            let bs: Vec<String> = bindings.iter().map(fmt_pattern).collect();
            format!("{}::{}({})", e, v, bs.join(", "))
        }
        MatchPattern::Some(inner) => format!("some({})", fmt_pattern(inner)),
        MatchPattern::None => "none".to_string(),
        MatchPattern::Wildcard => "_".to_string(),
        MatchPattern::IntLiteral(n) => n.to_string(),
        MatchPattern::BoolLiteral(b) => b.to_string(),
        MatchPattern::Ident(name) => name.clone(),
    }
}

pub fn fmt_pattern(pat: &Pattern) -> String {
    match pat {
        Pattern::Ident(name) => name.clone(),
        Pattern::Wildcard => "_".to_string(),
        Pattern::Tuple(pats) => {
            let ps: Vec<String> = pats.iter().map(fmt_pattern).collect();
            format!("({})", ps.join(", "))
        }
        Pattern::Deref(inner) => format!("*{}", fmt_pattern(inner)),
        Pattern::Some(inner) => format!("some({})", fmt_pattern(inner)),
        Pattern::None => "none".to_string(),
    }
}

/// Render the comma-separated body of a `with { ... }` / `init { ... }`
/// clause, combining concrete pins with forall (`*`) markers. Returns `None`
/// when there is nothing to print.
fn fmt_with_entries(pins: &[(String, Expr)], forall: &ForallSpec) -> Option<String> {
    let mut entries: Vec<String> = Vec::new();
    for (name, value) in pins {
        entries.push(format!("{}: {}", name, fmt_expr(value)));
    }
    if forall.all_state {
        entries.push("*".to_string());
    }
    for t in &forall.targets {
        match t {
            ForallTarget::StateField(f) => entries.push(format!("{}: *", f)),
            ForallTarget::Context { namespace, field } => {
                entries.push(format!("{}::{}: *", namespace, field))
            }
        }
    }
    if entries.is_empty() {
        None
    } else {
        Some(entries.join(", "))
    }
}

/// Render the body of a dedicated `ctx { ... }` block (concrete pins +
/// `*` foralls). Returns `None` when empty.
fn fmt_ctx_spec(ctx: &ContextSpec) -> Option<String> {
    if ctx.is_empty() {
        return None;
    }
    let entries: Vec<String> = ctx.entries.iter().map(|e| match &e.value {
        Some(v) => format!("{}::{}: {}", e.namespace, e.field, fmt_expr(v)),
        None => format!("{}::{}: *", e.namespace, e.field),
    }).collect();
    Some(entries.join(", "))
}

fn pp_catalog_link_attrs(out: &mut String, tag: &Option<String>, instantiates: &Option<String>) {
    if let Some(stem) = instantiates {
        out.push_str(&format!("#[instantiates(\"{}\")]\n", stem));
    }
    if let Some(tag) = tag {
        out.push_str(&format!("#[tag(\"{}\")]\n", tag));
    }
}

fn pp_property(out: &mut String, prop: &PropertyDecl) {
    pp_catalog_link_attrs(out, &prop.tag, &prop.instantiates);
    let params: Vec<String> = prop.params.iter()
        .map(|p| format!("{}: {}", p.name, fmt_type(&p.ty)))
        .collect();
    if params.is_empty() {
        out.push_str(&format!("property \"{}\" for {}", prop.name, prop.entity_name));
    } else {
        out.push_str(&format!("property \"{}\" ({}) for {}", prop.name, params.join(", "), prop.entity_name));
    }
    if let Some(s) = fmt_with_entries(&prop.init_state, &prop.forall_state) {
        out.push_str(&format!(" with {{ {} }}", s));
    }
    if let Some(s) = fmt_ctx_spec(&prop.context) {
        out.push_str(&format!(" ctx {{ {} }}", s));
    }
    out.push_str(" {\n");
    for step in &prop.body {
        pp_test_step(out, step);
    }
    for inst in &prop.instances {
        pp_property_instance(out, inst);
    }
    out.push_str("}\n");
}

fn pp_property_instance(out: &mut String, inst: &PropertyInstance) {
    out.push_str("  ");
    if let Some(runs) = inst.runs {
        out.push_str(&format!("#[runs({})] ", runs));
    }
    if inst.skip_from {
        out.push_str("#[skip_from] ");
    }
    let kw = match inst.kind {
        PropertyInstanceKind::Test => "test",
        PropertyInstanceKind::Fuzz => "fuzz",
    };
    out.push_str(kw);
    if let Some(name) = &inst.name {
        out.push_str(&format!(" \"{}\"", name));
    }
    out.push_str(" { ");
    let binds: Vec<String> = inst.bindings.iter().map(|(name, arg)| match arg {
        InstanceArg::Concrete(v) => format!("{}: {}", name, fmt_expr(v)),
        InstanceArg::Range { lo, hi, inclusive } => format!(
            "{} in {}{}{}",
            name,
            fmt_expr(lo),
            if *inclusive { "..=" } else { ".." },
            fmt_expr(hi),
        ),
    }).collect();
    out.push_str(&binds.join(", "));
    out.push_str(" }");
    if let Some(s) = fmt_with_entries(&inst.init_state, &inst.forall_state) {
        out.push_str(&format!(" with {{ {} }}", s));
    }
    if let Some(s) = fmt_ctx_spec(&inst.context) {
        out.push_str(&format!(" ctx {{ {} }}", s));
    }
    out.push('\n');
}

fn pp_invariant(out: &mut String, inv: &InvariantDecl) {
    pp_catalog_link_attrs(out, &inv.tag, &inv.instantiates);
    if inv.is_single_entity() {
        out.push_str(&format!("invariant \"{}\" for {}", inv.name, inv.entity_name()));
    } else {
        let parts: Vec<String> = inv.instances.iter()
            .map(|i| format!("{}: {}", i.name, i.entity))
            .collect();
        out.push_str(&format!("invariant \"{}\" for {{ {} }}", inv.name, parts.join(", ")));
    }
    if inv.fail_on_revert {
        out.push_str(" #[fail_on_revert]");
    }
    out.push_str(" {\n");
    if inv.is_single_entity() {
        let inst = &inv.instances[0];
        if let Some(s) = fmt_with_entries(&inst.init, &inst.forall_state) {
            out.push_str(&format!("  init {{ {} }}\n", s));
        }
    } else {
        for inst in &inv.instances {
            if let Some(s) = fmt_with_entries(&inst.init, &inst.forall_state) {
                out.push_str(&format!("  init {} {{ {} }}\n", inst.name, s));
            }
        }
    }
    if let Some(s) = fmt_ctx_spec(&inv.context) {
        out.push_str(&format!("  ctx {{ {} }}\n", s));
    }
    if !inv.deploy.is_empty() {
        out.push_str("  deploy { ");
        let s: Vec<String> = inv.deploy.iter().map(fmt_expr).collect();
        out.push_str(&s.join(", "));
        out.push_str(" }\n");
    }
    if !inv.senders.is_empty() {
        out.push_str("  senders { ");
        let s: Vec<String> = inv.senders.iter().map(fmt_expr).collect();
        out.push_str(&s.join(", "));
        out.push_str(" }\n");
    }
    for action in &inv.actions {
        let params: Vec<String> = action.params.iter()
            .map(|p| format!("{}: {}", p.name, fmt_type(&p.ty)))
            .collect();
        if inv.is_single_entity() {
            out.push_str(&format!("  action {}({}) {{\n", action.route, params.join(", ")));
        } else {
            out.push_str(&format!("  action {}.{}({}) {{\n", action.instance, action.route, params.join(", ")));
        }
        for step in &action.body {
            out.push_str("  ");
            pp_test_step(out, step);
        }
        out.push_str("  }\n");
    }
    for check in &inv.checks {
        out.push_str(&format!("  check {}\n", fmt_expr(check)));
    }
    out.push_str("}\n");
}

fn pp_test(out: &mut String, test: &TestDecl) {
    pp_catalog_link_attrs(out, &test.tag, &test.instantiates);
    out.push_str(&format!("test \"{}\" for {}", test.name, test.entity_name));
    if test.skip_from {
        out.push_str(" skip from");
    }
    if !test.init_state.is_empty() {
        out.push_str(" with { ");
        let fields: Vec<String> = test.init_state.iter()
            .map(|(name, value)| format!("{}: {}", name, fmt_expr(value)))
            .collect();
        out.push_str(&fields.join(", "));
        out.push_str(" }");
    }
    out.push_str(" {\n");
    for step in &test.body {
        pp_test_step(out, step);
    }
    out.push_str("}\n");
}

fn pp_test_step(out: &mut String, step: &TestStep) {
    match step {
        TestStep::Let { name, ty, value } => {
            if let Some(t) = ty {
                out.push_str(&format!("  let {}: {} = {}\n", name, fmt_type(t), fmt_expr(value)));
            } else {
                out.push_str(&format!("  let {} = {}\n", name, fmt_expr(value)));
            }
        }
        TestStep::SetContext { namespace, fields } => {
            out.push_str(&format!("  {} {{ ", namespace));
            let fs: Vec<String> = fields.iter()
                .map(|(name, value)| format!("{}: {}", name, fmt_expr(value)))
                .collect();
            out.push_str(&fs.join(", "));
            out.push_str(" }\n");
        }
        TestStep::SetRegistry { entity_name, code_hash, code_depth, wasm_hash } => {
            out.push_str(&format!("  registry {} {{ code_hash: {}, code_depth: {}, wasm_hash: {} }}\n",
                entity_name, fmt_expr(code_hash), fmt_expr(code_depth), fmt_expr(wasm_hash)));
        }
        TestStep::Call { target, route, args } => {
            let a: Vec<String> = args.iter().map(fmt_expr).collect();
            let qualifier = match target {
                Some(t) => format!("{}.", t),
                None => String::new(),
            };
            out.push_str(&format!("  call {}{}({})\n", qualifier, route, a.join(", ")));
        }
        TestStep::DeployPeer { binding, entity, args, init_state } => {
            let a: Vec<String> = args.iter().map(fmt_expr).collect();
            out.push_str(&format!("  deploy {} = {}({})", binding, entity, a.join(", ")));
            if !init_state.is_empty() {
                let fs: Vec<String> = init_state.iter()
                    .map(|(name, value)| format!("{}: {}", name, fmt_expr(value)))
                    .collect();
                out.push_str(&format!(" with {{ {} }}", fs.join(", ")));
            }
            out.push('\n');
        }
        TestStep::ExpectState { fields } => {
            out.push_str("  expect state { ");
            let fs: Vec<String> = fields.iter()
                .map(|(path, value)| format!("{}: {}", pp_field_path(path), fmt_expr(value)))
                .collect();
            out.push_str(&fs.join(", "));
            out.push_str(" }\n");
        }
        TestStep::ExpectThrow { code } => {
            out.push_str(&format!("  expect throw {}\n", code));
        }
        TestStep::ExpectEmit { event_name, args } => {
            let vs: Vec<String> = args.iter().map(fmt_expr).collect();
            out.push_str(&format!("  expect emit {}({})\n", event_name, vs.join(", ")));
        }
        TestStep::ExpectReturn { value } => {
            out.push_str(&format!("  expect return {}\n", fmt_expr(value)));
        }
        TestStep::ExpectReturnTuple { values } => {
            let vs: Vec<String> = values.iter().map(fmt_expr).collect();
            out.push_str(&format!("  expect return ({})\n", vs.join(", ")));
        }
        TestStep::ExpectReturnLens { path, value } => {
            out.push_str(&format!("  expect return.{} == {}\n", pp_field_path(path), fmt_expr(value)));
        }
        TestStep::ExpectPred { cond } => {
            out.push_str(&format!("  expect {}\n", fmt_expr(cond)));
        }
        TestStep::ExpectEffects { elements } => {
            out.push_str("  expect effects [");
            let parts: Vec<String> = elements.iter().map(|el| match el {
                TestEffectElement::Effect(e) => fmt_test_effect(e),
                TestEffectElement::Wildcard => "..".to_string(),
            }).collect();
            out.push_str(&parts.join(", "));
            out.push(']');
            out.push('\n');
        }
        TestStep::Assume { cond } => {
            out.push_str(&format!("  assume {}\n", fmt_expr(cond)));
        }
        TestStep::Bound { var, lo, hi, inclusive } => {
            let op = if *inclusive { "..=" } else { ".." };
            out.push_str(&format!("  bound {} in {}{}{}\n", var, fmt_expr(lo), op, fmt_expr(hi)));
        }
        TestStep::SkipIf { cond } => {
            out.push_str(&format!("  skip if {}\n", fmt_expr(cond)));
        }
        TestStep::AdvanceTime { secs } => {
            out.push_str(&format!("  advanceTime({})\n", fmt_expr(secs)));
        }
    }
}

fn pp_field_path(path: &[PathSegment]) -> String {
    let mut s = String::new();
    for seg in path {
        match seg {
            PathSegment::Field(name) => {
                if !s.is_empty() { s.push('.'); }
                s.push_str(name);
            }
            PathSegment::Index(key_expr) => {
                s.push('[');
                s.push_str(&fmt_expr(key_expr));
                s.push(']');
            }
            PathSegment::TupleIndex(idx) => {
                s.push('.');
                s.push_str(&idx.to_string());
            }
        }
    }
    s
}

fn fmt_test_effect(eff: &TestEffect) -> String {
    match eff {
        TestEffect::PlatformEffect { name, args } => {
            let a: Vec<String> = args.iter().map(fmt_expr).collect();
            format!("{}({})", name, a.join(", "))
        }
        TestEffect::Send { message, args, dest, send_options } => {
            let mut s = String::new();
            if let Some(msg) = message {
                let a: Vec<String> = args.iter().map(fmt_expr).collect();
                s.push_str(&format!("{}({}) ", msg, a.join(", ")));
            }
            s.push_str(&format!("~> {}", fmt_expr(dest)));
            if let Some(opts) = send_options {
                s.push_str(&format!(" with {}", fmt_expr(opts)));
            }
            s
        }
        TestEffect::Deploy { entity, send_options } => {
            let mut s = format!("deploy {}", entity);
            if let Some(opts) = send_options {
                s.push_str(&format!(" with {}", fmt_expr(opts)));
            }
            s
        }
    }
}
