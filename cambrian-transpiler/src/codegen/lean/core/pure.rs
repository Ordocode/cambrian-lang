// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean codegen — top-level pure functions (P1.5 / P1.6).
//!
//! All program-scope `pure fn` definitions land in a single
//! `Cambrian/Generated/Pure.lean` file under the
//! `Cambrian.Generated.Pure` namespace. Bodies use the same expression
//! lowering as entity routes, with a synthesised entity-less context
//! (no `s.<m>` resolution, no `^m_a` references).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use crate::ast::{
    map_expr, map_program_exprs, Entity, EnumDecl, EnumVariant, Expr, LibraryDecl, Pattern, Program,
    PureFn, Record, Span, Type, UsingItems,
};

use super::super::expr::LeanExprCtx;
use super::super::LeanProfile;
use super::emitter::{doc_comment, push_deriving, push_indent};
use super::types::sanitize_variant;
use super::types::{lower_type, LeanTypeCtx};

/// Lean symbol for a library-scoped `pure fn` (`TokenSpec::sum` → `TokenSpec_sum`).
pub(crate) fn library_pure_lean_symbol(lib: &str, fn_name: &str) -> String {
    format!(
        "{}_{}",
        super::types::lean_safe_ident(lib),
        super::types::lean_safe_ident(fn_name)
    )
}

/// Flatten every `library` into program-scope pure fns named
/// `Lib_name` (constants become zero-parameter fns) and rewrite each
/// reference form to a plain call of that fn: `Lib.fn(..)`, `Lib::fn(..)`,
/// `using Lib for T` sugar (already a bare `fn(..)`), `Lib.K` / `Lib::K`,
/// and unqualified sibling calls / constants inside a library body. The
/// flattened fns then share the free-fn machinery (fail-mode propagation,
/// callee-first ordering, argument width coercion).
pub(crate) fn resolve_library_refs(program: &Program) -> Cow<'_, Program> {
    if program.libraries.is_empty() {
        return Cow::Borrowed(program);
    }
    let lib_fns: HashMap<String, HashSet<String>> = program
        .libraries
        .iter()
        .map(|l| (l.name.clone(), l.pure_fns.iter().map(|f| f.name.clone()).collect()))
        .collect();
    let lib_consts: HashMap<String, HashSet<String>> = program
        .libraries
        .iter()
        .map(|l| (l.name.clone(), l.constants.iter().map(|c| c.name.clone()).collect()))
        .collect();
    let free_fns: HashSet<String> = program.pure_fns.iter().map(|f| f.name.clone()).collect();
    let using_libs: Vec<String> = program
        .using_decls
        .iter()
        .filter_map(|u| match &u.items {
            UsingItems::Library(l) => Some(l.clone()),
            UsingItems::Functions(_) => None,
        })
        .collect();
    let call = |lib: &str, name: &str, args: Vec<Expr>| {
        Expr::FnCall(library_pure_lean_symbol(lib, name), args)
    };

    let mut out = program.clone();
    let mut flattened: Vec<PureFn> = Vec::new();
    for lib in std::mem::take(&mut out.libraries) {
        let own_fns = &lib_fns[&lib.name];
        let own_consts = &lib_consts[&lib.name];
        for c in lib.constants {
            let mut body = c.value;
            map_expr(&mut body, &mut |e| {
                if let Expr::Ident(name) = e {
                    if own_consts.contains(name) {
                        *e = call(&lib.name, name, vec![]);
                    }
                }
            });
            flattened.push(PureFn {
                name: library_pure_lean_symbol(&lib.name, &c.name),
                params: vec![],
                return_type: c.ty,
                body,
                span: c.span,
            });
        }
        for mut f in lib.pure_fns {
            let params: HashSet<String> = f.params.iter().map(|p| p.name.clone()).collect();
            map_expr(&mut f.body, &mut |e| match e {
                Expr::FnCall(name, args) if own_fns.contains(name) && !free_fns.contains(name) => {
                    *e = call(&lib.name, name, std::mem::take(args));
                }
                Expr::Ident(name) if own_consts.contains(name) && !params.contains(name) => {
                    *e = call(&lib.name, name, vec![]);
                }
                _ => {}
            });
            f.name = library_pure_lean_symbol(&lib.name, &f.name);
            flattened.push(f);
        }
    }
    out.using_decls
        .retain(|u| !matches!(&u.items, UsingItems::Library(l) if lib_fns.contains_key(l)));
    map_program_exprs(&mut out, |e| match e {
        Expr::MethodCall(base, name, args) => {
            if let Expr::Ident(lib) = base.as_ref() {
                if lib_fns.get(lib).is_some_and(|fns| fns.contains(name)) {
                    let lib = lib.clone();
                    *e = call(&lib, &name.clone(), std::mem::take(args));
                }
            }
        }
        Expr::NamespacedCall { namespace, name, args, .. }
            if lib_fns.get(namespace).is_some_and(|fns| fns.contains(name)) =>
        {
            let lib = namespace.clone();
            *e = call(&lib, &name.clone(), std::mem::take(args));
        }
        Expr::EnumVariantWithData(lib, name, args)
            if lib_fns.get(lib).is_some_and(|fns| fns.contains(name)) =>
        {
            let lib = lib.clone();
            *e = call(&lib, &name.clone(), std::mem::take(args));
        }
        Expr::FieldAccess(base, name) => {
            if let Expr::Ident(lib) = base.as_ref() {
                if lib_consts.get(lib).is_some_and(|cs| cs.contains(name)) {
                    *e = call(lib, name, vec![]);
                }
            }
        }
        Expr::EnumVariant(lib, name) if lib_consts.get(lib).is_some_and(|cs| cs.contains(name)) => {
            *e = call(lib, name, vec![]);
        }
        Expr::FnCall(name, args) if !free_fns.contains(name) => {
            if let Some(lib) = using_libs
                .iter()
                .find(|l| lib_fns.get(*l).is_some_and(|fns| fns.contains(name)))
            {
                *e = call(lib, name, std::mem::take(args));
            }
        }
        _ => {}
    });
    out.pure_fns.extend(flattened);
    Cow::Owned(out)
}

fn program_has_pure_defs(program: &Program) -> bool {
    !program.pure_fns.is_empty()
        || program
            .libraries
            .iter()
            .any(|lib| !lib.pure_fns.is_empty())
}

/// True when generated Lean modules should `import Cambrian.Generated.Pure`.
pub(crate) fn program_needs_pure_import(program: &Program) -> bool {
    program_has_pure_defs(program)
        || !program.enums.is_empty()
        || !program.records.is_empty()
}

/// Generate the contents of `Cambrian/Generated/Pure.lean`. Returns
/// `None` when the program has no pure fns, no program-scope enums,
/// and no program-scope records — callers should skip emission
/// rather than write an empty file.
pub fn gen_pure_module(program: &Program, profile: LeanProfile) -> Option<String> {
    if !program_has_pure_defs(program)
        && program.enums.is_empty()
        && program.records.is_empty()
    {
        return None;
    }
    let mut out = String::new();
    out.push_str("/-\n");
    out.push_str("  Auto-generated by cambrian-transpiler — Lean target (P1).\n");
    out.push_str(
        "  Top-level program types and pure functions live here under `Cambrian.Generated.Pure`\n",
    );
    out.push_str(
        "  (program-scope enums / records are emitted at the top of the file so every\n  entity module can refer to them through this import).\n",
    );
    out.push_str("-/\n\n");
    out.push_str("import Cambrian.Prelude\n\n");
    out.push_str("set_option linter.unusedVariables false\n\n");

    let placeholder_entity = synthesise_placeholder_entity();

    // Program-scope enums / records.
    //
    // Legacy: emitted outside any namespace (bare top-level names), so
    // `<Entity>.lean` can refer to them without qualification.
    //
    // Escrow (B6, spec §15.11): nested under the fixed
    // `Cambrian.Generated.Types` namespace so they fall inside the pinned
    // digest zone (root `Cambrian` is already reserved). This file is
    // already imported before every user (each `<Entity>.lean` imports
    // `Cambrian.Generated.Pure`), so no import-graph change is needed.
    // Reference sites are requalified via `resolve_user_type` (types) and
    // `qualify_enum_ref` (variant literals) under the same flag.
    //
    // Emit in dependency order (a record referencing an enum, or an enum
    // with a record payload, must follow its dependency — Lean has no
    // forward references outside `mutual`).
    let predictable = profile.predictable;
    let has_top_types = !program.records.is_empty() || !program.enums.is_empty();
    if predictable && has_top_types {
        out.push_str("namespace Cambrian.Generated.Types\n\n");
    }
    for item in crate::analysis::ProgramGraphs::build(program).resolve_program_types(program) {
        match item {
            super::types::TypeItem::Record(rec) => {
                emit_top_level_record(&mut out, program, rec, profile)
            }
            super::types::TypeItem::Enum(en) => emit_top_level_enum(&mut out, program, en, profile),
        }
    }
    if predictable && has_top_types {
        out.push_str("end Cambrian.Generated.Types\n\n");
    }

    out.push_str("namespace Cambrian.Generated.Pure\n\n");
    // Callee-first, for the same reason the types above are: Lean has no
    // forward references. Source order breaks as soon as a project imports a
    // library it calls into.
    for f in crate::analysis::pure_order::order_pure_fns(&program.pure_fns) {
        emit_pure_fn(&mut out, program, &placeholder_entity, f, profile, &f.name);
    }
    for lib in &program.libraries {
        emit_library_pure_fns(&mut out, program, &placeholder_entity, lib, profile);
    }
    out.push_str("end Cambrian.Generated.Pure\n");
    Some(out)
}

fn emit_top_level_record(out: &mut String, program: &Program, rec: &Record, profile: LeanProfile) {
    let type_ctx = LeanTypeCtx::top_level(program, profile);
    doc_comment(out, 0, &format!("Program-scope record `{}`.", rec.name));
    out.push_str(&format!("structure {} where\n", rec.name));
    for f in &rec.fields {
        push_indent(out, 1);
        out.push_str(&format!(
            "{} : {}\n",
            super::types::lean_safe_ident(&f.name),
            lower_type(&f.ty, &type_ctx)
        ));
    }
    let derives = super::deceq::instances_filtered(
        type_ctx.use_predictable_profile,
        true,
        true,
        &["Repr", "Inhabited", "BEq", "DecidableEq"],
    );
    push_deriving(out, type_ctx.use_predictable_profile, &derives);
    let field_names: Vec<String> = rec
        .fields
        .iter()
        .map(|f| super::types::lean_safe_ident(&f.name))
        .collect();
    crate::codegen::lean::evm::maybe_emit_struct_eq_instances(
        out,
        type_ctx.use_predictable_profile,
        &rec.name,
        &field_names,
    );
}

fn emit_top_level_enum(out: &mut String, program: &Program, en: &EnumDecl, profile: LeanProfile) {
    let type_ctx = LeanTypeCtx::top_level(program, profile);
    doc_comment(out, 0, &format!("Program-scope enum `{}`.", en.name));
    out.push_str(&format!("inductive {} where\n", en.name));
    for v in &en.variants {
        emit_top_level_enum_variant(out, &en.name, v, &type_ctx);
    }
    let ctors: Vec<(String, usize)> = en
        .variants
        .iter()
        .map(|v| (sanitize_variant(&v.name), v.fields.len()))
        .collect();
    // A data-carrying enum's `deriving BEq` elaborates to an equation-compiler
    // matcher (`instBEq<T>.beq.match_1`); under predictable profile drop it too and emit an
    // explicit term-level Bool matrix. A nullary enum keeps `deriving BEq` (it
    // reduces to a `ctorIdx` comparison — predictable, not a matcher) — P4.
    let is_data = ctors.iter().any(|(_, arity)| *arity > 0);
    let derives = super::deceq::instances_filtered(
        type_ctx.use_predictable_profile,
        true,
        is_data,
        &["Repr", "Inhabited", "BEq", "DecidableEq"],
    );
    push_deriving(out, type_ctx.use_predictable_profile, &derives);
    crate::codegen::lean::evm::maybe_emit_enum_eq_instances(
        out,
        type_ctx.use_predictable_profile,
        &en.name,
        &ctors,
        is_data,
    );
}

fn emit_top_level_enum_variant(
    out: &mut String,
    enum_name: &str,
    v: &EnumVariant,
    ctx: &LeanTypeCtx<'_>,
) {
    push_indent(out, 1);
    if v.fields.is_empty() {
        out.push_str(&format!("| {}\n", sanitize_variant(&v.name)));
    } else {
        let parts: Vec<String> = v.fields.iter().map(|t| lower_type(t, ctx)).collect();
        out.push_str(&format!(
            "| {} : {} → {}\n",
            sanitize_variant(&v.name),
            parts.join(" → "),
            enum_name,
        ));
    }
}

fn emit_library_pure_fns(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    lib: &LibraryDecl,
    profile: LeanProfile,
) {
    for c in &lib.constants {
        let sym = library_pure_lean_symbol(&lib.name, &c.name);
        doc_comment(
            out,
            0,
            &format!("Library `{}` constant `{}`.", lib.name, c.name),
        );
        let as_fn = PureFn {
            name: c.name.clone(),
            params: vec![],
            return_type: c.ty.clone(),
            body: c.value.clone(),
            span: c.span.clone(),
        };
        emit_pure_fn(out, program, entity, &as_fn, profile, &sym);
    }
    // Callee-first like program pure fns; sibling calls are `Lib::fn` here.
    let probe: Vec<PureFn> = lib
        .pure_fns
        .iter()
        .map(|f| {
            let mut g = f.clone();
            map_expr(&mut g.body, &mut |e| {
                if let Expr::NamespacedCall { namespace, name, args, .. } = e {
                    if *namespace == lib.name {
                        *e = Expr::FnCall(name.clone(), std::mem::take(args));
                    }
                }
            });
            g
        })
        .collect();
    let ordered: Vec<&PureFn> = crate::analysis::pure_order::order_pure_fns(&probe)
        .into_iter()
        .filter_map(|p| lib.pure_fns.iter().find(|f| f.name == p.name))
        .collect();
    for f in ordered {
        let sym = library_pure_lean_symbol(&lib.name, &f.name);
        doc_comment(
            out,
            0,
            &format!("Library `{}` pure fn `{}`.", lib.name, f.name),
        );
        emit_pure_fn(out, program, entity, f, profile, &sym);
    }
}

fn emit_pure_fn(
    out: &mut String,
    program: &Program,
    entity: &Entity,
    f: &PureFn,
    profile: LeanProfile,
    def_name: &str,
) {
    let type_ctx = LeanTypeCtx::top_level(program, profile);
    if def_name == f.name {
        doc_comment(out, 0, &format!("Pure fn `{}`.", f.name));
    }
    out.push_str(&format!("def {}", def_name));
    for p in &f.params {
        out.push_str(&format!(
            " ({} : {})",
            super::types::lean_safe_ident(&p.name),
            lower_type(&p.ty, &type_ctx),
        ));
    }
    let fail = super::super::expr::pure_fn_forces_fail(f, &program.pure_fns);
    if fail {
        out.push_str(&format!(
            " : Cambrian.RouteResult ({}) :=\n  ",
            lower_type(&f.return_type, &type_ctx)
        ));
    } else {
        out.push_str(&format!(
            " : {} :=\n  ",
            lower_type(&f.return_type, &type_ctx)
        ));
    }
    let mut params = HashSet::new();
    let mut hashmap_idents = HashSet::new();
    for p in &f.params {
        params.insert(p.name.clone());
        if matches!(&p.ty, Type::Generic(g, _) if g == "HashMap") {
            hashmap_idents.insert(p.name.clone());
        }
    }
    let expr_ctx = LeanExprCtx {
        type_ctx: LeanTypeCtx {
            program: type_ctx.program,
            proof_helpers: type_ctx.proof_helpers,
            use_nat_numerics: type_ctx.use_nat_numerics,
            overflow_panic: type_ctx.overflow_panic,
            use_predictable_profile: type_ctx.use_predictable_profile,
            deterministic_addresses: type_ctx.deterministic_addresses,
            entity_name: type_ctx.entity_name,
            local_records: type_ctx.local_records,
            local_enums: type_ctx.local_enums,
            local_aliases: type_ctx.local_aliases,
        },
        entity,
        local_enums: &entity.enums,
        pure_fns: &program.pure_fns,
        lets: HashSet::new(),
        route_params: params,
        route_name: None,
        phase_name: None,
        route_arg_names: vec![],
        in_transform: false,
        state_var: "s".to_string(),
        derived_helpers: HashSet::new(),
        world_var: None,
        instance_var: None,
        qualified_instances: std::collections::HashMap::new(),
        deploy_bindings: std::collections::HashMap::new(),
        hashmap_idents,
        nat_idents: HashSet::new(),
        trace_acc_var: None,
        expected_bitvec_width: super::super::expr::bitvec_width_of_type(&f.return_type, &type_ctx),
        expected_signed: Some(super::types::type_is_signed(
            &f.return_type,
            &type_ctx,
        )),
        expected_collection: super::super::expr::collection_shape_of_type(
            &f.return_type,
            &type_ctx,
        ),
        msg_sender_override: None,
        pure_fn: Some(f),
    };
    // Escrow: expand a tuple-pattern `let` in the body so it no longer
    // elaborates to a matcher (`Cambrian.Generated.Pure.<f>.match_1`). A pure fn
    // is emitted straight through `gen_expr` and passed through NO rewriter
    // before (defect L2: `rewrite_route_body` is only reachable from route /
    // member emitters). The rewrite is a byte-identical no-op for every body
    // without a tuple-let, so those keep the single-line framing; only an
    // actually-rewritten (multi-line) body indents its continuation lines
    // (mirror of `emit_transform_def`; the header above already emitted the
    // first line's indent).
    let body_expr = if fail {
        super::super::expr::gen_expr_as_route_result(&f.body, &expr_ctx)
    } else {
        let term = super::super::expr::gen_expr_as_type(&f.body, &f.return_type, &expr_ctx);
        match (
            super::super::expr::arith_result_width(&f.body, &expr_ctx),
            expr_ctx.expected_bitvec_width,
        ) {
            (Some(src), Some(dst)) if src != dst && !type_ctx.use_nat_numerics => {
                super::super::expr::coerce_result_to_width(&f.body, &term, dst, &expr_ctx)
            }
            _ => term,
        }
    };
    let rewritten = super::super::evm::rewrite_pure_fn_body(&body_expr);
    if rewritten != body_expr {
        for (i, line) in rewritten.split_inclusive('\n').enumerate() {
            if i > 0 && !line.trim().is_empty() {
                out.push_str("  ");
            }
            out.push_str(line);
        }
    } else {
        out.push_str(&body_expr);
    }
    out.push_str("\n\n");
}

/// We need *some* `&Entity` to satisfy [`LeanExprCtx`]. Pure-fn bodies
/// never reference `s.<m>` (validator already enforces no member /
/// temporal-ref usage outside entities), so a synthetic empty entity
/// is fine.
fn synthesise_placeholder_entity() -> Entity {
    Entity {
        name: "<pure_fn>".to_string(),
        records: vec![],
        enums: vec![],
        type_aliases: vec![],
        constants: vec![],
        macros: vec![],
        routes: vec![],
        members: vec![],
        events: vec![],
        errors: vec![],
        span: Span::none(),
    }
}

#[allow(dead_code)]
fn _silence_unused(_: &Pattern) {}
