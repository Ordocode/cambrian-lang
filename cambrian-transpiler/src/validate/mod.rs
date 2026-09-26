// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use crate::ast::*;
use std::collections::{HashMap, HashSet};

mod catalog_links;
mod entity;
mod names;
mod registry;
mod rules;
mod test_checks;
mod warnings;

pub use catalog_links::{
    catalog_pre_merge_check, check_catalog_links, check_instantiates_goal_divergence,
    check_noop_instantiates,
};
pub(crate) use entity::arms_cover_all;
pub use names::check_reserved_names;
pub use registry::{rule_binding, run_rules, RuleBinding, RuleEntry, ValidateCtx, RULES};

thread_local! {
    /// Phase EVM-P0-A: pure-fn names visible in the program currently
    /// being validated. Populated at the entry of every public
    /// validation entry point (`validate`,
    /// `check_evm_target_compat_with`, …) and consulted by the E23
    /// shape gate (`is_inferable_tuple_rhs`).  Cleared between calls.
    static PROGRAM_PURE_FN_NAMES: std::cell::RefCell<std::collections::HashSet<String>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

pub(super) fn set_program_pure_fn_names(program: &Program) {
    PROGRAM_PURE_FN_NAMES.with(|c| {
        let mut s = c.borrow_mut();
        s.clear();
        for pf in &program.pure_fns {
            s.insert(pf.name.clone());
        }
    });
}

#[derive(Debug, Clone, PartialEq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Parse,
    Project,
    Validate,
    Target,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceSpan {
    pub byte_start: usize,
    pub byte_end: usize,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
    pub phase: Phase,
    pub path: Option<String>,
    pub span: Option<SourceSpan>,
    pub hint: Option<String>,
    pub unexpected: Option<String>,
    pub expected: Option<Vec<String>>,
    pub suppressed_by: Option<String>,
    pub downgraded: bool,
}

impl Diagnostic {
    pub fn error(code: &'static str, message: String) -> Self {
        Diagnostic {
            severity: Severity::Error,
            code,
            message,
            phase: Phase::Validate,
            path: None,
            span: None,
            hint: None,
            unexpected: None,
            expected: None,
            suppressed_by: None,
            downgraded: false,
        }
    }
    pub fn warning(code: &'static str, message: String) -> Self {
        Diagnostic {
            severity: Severity::Warning,
            code,
            message,
            phase: Phase::Validate,
            path: None,
            span: None,
            hint: None,
            unexpected: None,
            expected: None,
            suppressed_by: None,
            downgraded: false,
        }
    }

    pub fn is_actionable(&self) -> bool {
        self.suppressed_by.is_none()
    }

    /// Attach path + line/column from a parse-stamped AST span when the
    /// originating file is still in the parse-time table.
    pub fn with_span(mut self, span: Span) -> Self {
        if let Some((path, src)) = file_source(span.file_id) {
            let mapper = crate::sourcemap::SourceMapper::new(&src);
            self.path = Some(path);
            self.span = Some(SourceSpan {
                byte_start: span.start,
                byte_end: span.end.max(span.start),
                line: mapper.line_of(span.start),
                column: mapper.column_of(span.start),
            });
        }
        self
    }
}

/// Validate a parsed Program, returning all diagnostics.
///
/// Runs only [`RuleBinding::Universal`] rules (no target selected yet).
/// Target-scoped rules run via [`check_target_compat`].
pub fn validate(program: &Program) -> Vec<Diagnostic> {
    rules::clear_family_cache();
    let mut diags = check_catalog_links(program);
    diags.extend(check_instantiates_goal_divergence(program));
    diags.extend(check_noop_instantiates(program));
    let ctx = ValidateCtx::universal_only(program);
    for entry in RULES {
        if entry.binding.applies_universal_only() {
            (entry.run)(&ctx, &mut diags);
        }
    }
    rules::clear_family_cache();
    diags
}

// ---------------------------------------------------------------------------
// F1, F3, F6: Project-level configuration validation (coverage gates).
// F4 / F5 are packaging errors from the project loader (`project.rs`).
// ---------------------------------------------------------------------------

/// Validate the project YAML configuration against the chosen target.
/// Diagnostics use codes F1/F3 and are intended to be merged into the main
/// diagnostic stream by the project-mode entry point.
pub fn validate_project_config(config: &crate::project::ProjectConfig) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    // F1: revm_tests.coverage.enabled requires revm_tests.enabled.
    if let Some(rt) = &config.revm_tests {
        if let Some(cov) = &rt.coverage {
            if cov.enabled && !rt.enabled {
                diags.push(Diagnostic::error(
                    "F1",
                    "revm_tests.coverage.enabled requires revm_tests.enabled = true".to_string(),
                ));
            }
            // F3: revm coverage engine.
            if cov.enabled && !is_known_coverage_engine(&cov.engine) {
                diags.push(Diagnostic::error(
                    "F3",
                    format!(
                        "revm_tests.coverage.engine: unknown engine '{}', expected 'libfuzzer' or 'afl'",
                        cov.engine
                    ),
                ));
            }
        }
    }

    // F2: lean.emission_profile, when present, must be exactly "default" or
    // "predictable". Fail loud on anything else — no silent fallback to the
    // default (that default only applies when the field is
    // *absent*, not when it is set to something unrecognized).
    // Same for lean.numerics: absent / overflow-wrap / overflow-panic / nat.
    if let Some(lean) = &config.lean {
        if let Some(profile) = &lean.emission_profile {
            if profile != "default" && profile != "predictable" {
                diags.push(Diagnostic::error(
                    "F2",
                    format!(
                        "lean.emission_profile: unknown profile '{}', expected 'default' or 'predictable'",
                        profile
                    ),
                ));
            }
            #[cfg(not(feature = "predictable-profile"))]
            if profile == "predictable" {
                diags.push(Diagnostic::error(
                    "F2",
                    format!(
                        "lean.emission_profile: profile '{}' requires the \
                         'predictable-profile' feature (disabled in this build), \
                         expected 'default'",
                        profile
                    ),
                ));
            }
        }
        if let Some(numerics) = &lean.numerics {
            let ok = matches!(
                numerics.as_str(),
                "nat"
                    | "Nat"
                    | "NAT"
                    | "overflow-wrap"
                    | "overflow-panic"
                    | "bitvec" // legacy alias for overflow-wrap
                    | "BitVec"
            );
            if !ok {
                diags.push(Diagnostic::error(
                    "F2",
                    format!(
                        "lean.numerics: unknown mode '{}', expected 'nat', \
                         'overflow-wrap', or 'overflow-panic'",
                        numerics
                    ),
                ));
            }
        }
    }

    for w in &config.key_warnings {
        diags.push(Diagnostic::warning("F7", format!("project.yaml: {w}")));
    }
    if let Some(intrinsics) = config.lean.as_ref().and_then(|l| l.intrinsics.as_deref()) {
        if intrinsics != "opaque" && intrinsics != "executable" {
            diags.push(Diagnostic::error(
                "F2",
                format!(
                    "lean.intrinsics: unknown mode '{}', expected 'opaque' or 'executable'",
                    intrinsics
                ),
            ));
        }
    }

    // F6: EVM target rejects explicit `deterministic_addresses: false`.
    if matches!(
        crate::target::Target::from_name(&config.target),
        Some(crate::target::Target::Evm)
    ) && config.deterministic_addresses == Some(false)
    {
        diags.push(Diagnostic::error(
            "F6",
            "deterministic_addresses: false is not supported for target evm — omit the field (defaults to true) or set true (CREATE2 factory deploy is mandatory)".to_string(),
        ));
    }

    // F1 + F3: ackinacki.coverage.{enabled, engine}.
    if let Some(ack) = &config.ackinacki {
        if let Some(cov) = &ack.coverage {
            let is_ackinacki = matches!(
                crate::target::Target::from_name(&config.target),
                Some(crate::target::Target::AckiNacki)
            );
            if cov.enabled && !is_ackinacki {
                diags.push(Diagnostic::error(
                    "F1",
                    "ackinacki.coverage.enabled requires target = \"ackinacki\"".to_string(),
                ));
            }
            if cov.enabled && !is_known_coverage_engine(&cov.engine) {
                diags.push(Diagnostic::error(
                    "F3",
                    format!(
                        "ackinacki.coverage.engine: unknown engine '{}', expected 'libfuzzer' or 'afl'",
                        cov.engine
                    ),
                ));
            }
        }
    }

    diags
}

fn is_known_coverage_engine(engine: &str) -> bool {
    matches!(engine, "libfuzzer" | "afl")
}

// ---------------------------------------------------------------------------
// V3: Temporal ^member dependencies form a DAG
// ---------------------------------------------------------------------------

pub use crate::analysis::TemporalOrder;

/// Build temporal DAG for all routes, check for cycles, return computation order.
///
/// Thin wrapper over [`crate::analysis::build_temporal_orders`] that converts
/// kernel `TemporalError`s into validate `Diagnostic`s.
pub fn build_temporal_orders(entity: &Entity) -> (Vec<TemporalOrder>, Vec<Diagnostic>) {
    let (orders, errs) = crate::analysis::build_temporal_orders(entity);
    let diags = errs
        .into_iter()
        .map(|e| Diagnostic::error(e.code, e.message))
        .collect();
    (orders, diags)
}

pub(super) fn check_temporal_dag(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    let (_, dag_diags) = build_temporal_orders(entity);
    diags.extend(dag_diags);
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

pub(crate) fn collect_pattern_names<'a>(pat: &'a Pattern, names: &mut HashSet<&'a str>) {
    match pat {
        Pattern::Ident(name) => {
            names.insert(name.as_str());
        }
        Pattern::Wildcard | Pattern::None => {}
        Pattern::Tuple(pats) => {
            for p in pats {
                collect_pattern_names(p, names);
            }
        }
        Pattern::Deref(inner) | Pattern::Some(inner) => collect_pattern_names(inner, names),
    }
}

pub(crate) fn collect_match_pattern_names<'a>(pat: &'a MatchPattern, names: &mut HashSet<&'a str>) {
    match pat {
        MatchPattern::Ident(name) => {
            names.insert(name.as_str());
        }
        MatchPattern::EnumVariantWithData(_, _, pats) => {
            for p in pats {
                collect_pattern_names(p, names);
            }
        }
        MatchPattern::Some(inner) => {
            collect_pattern_names(inner, names);
        }
        MatchPattern::Wildcard
        | MatchPattern::IntLiteral(_)
        | MatchPattern::BoolLiteral(_)
        | MatchPattern::EnumVariant(_, _)
        | MatchPattern::None => {}
    }
}

// ---------------------------------------------------------------------------
// V21/W4: Import validation
// ---------------------------------------------------------------------------

pub(super) fn check_imports(program: &Program, diags: &mut Vec<Diagnostic>) {
    let allowed = ["gosh"];
    let mut seen: HashSet<&str> = HashSet::new();
    for import in &program.imports {
        if !allowed.contains(&import.namespace.as_str()) {
            diags.push(Diagnostic::error(
                "V21",
                format!(
                    "Unknown SDK namespace '{}'. Supported: {}",
                    import.namespace,
                    allowed.join(", ")
                ),
            ));
        }
        if seen.contains(import.namespace.as_str()) {
            diags.push(Diagnostic::warning(
                "W4",
                format!("Duplicate import 'use {}'", import.namespace),
            ));
        }
        seen.insert(&import.namespace);
    }
}

// ---------------------------------------------------------------------------
// Phase Library-1: cross-file `import "..."` validation
// ---------------------------------------------------------------------------
//
// Unresolved `import "..."` is a loader `ProjectError::Io` (no V-code).
// F4 (loader-time) — imported file declares `entity` / `test` / `fuzz` /
//                     `invariant`. Surfaced by project.rs::
//                     merge_tagged_programs.
// W8 (this fn)     — same path imported twice from the same file.

pub(super) fn check_file_imports(program: &Program, diags: &mut Vec<Diagnostic>) {
    let mut seen: HashSet<&str> = HashSet::new();
    for fi in &program.file_imports {
        if !seen.insert(fi.path.as_str()) {
            diags.push(Diagnostic::warning(
                "W8",
                format!(
                    "Duplicate import \"{}\" (path imported more than once)",
                    fi.path
                ),
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Phase Library-2: `using ... for T;` validation (V55 / V56 / V57)
// ---------------------------------------------------------------------------
//
// V55 — `using` method name collides with a built-in for the receiver
//       type (e.g. `len` on `Vec`, `insert` on `HashMap`, `as_bytes` on
//       `String`).
// V56 — `using` references an undeclared `pure fn` (or library).
// V57 — `using` first argument of the referenced `pure fn` does not
//       match the `for` type. Receiver-type alignment is what makes
//       `recv.method(args)` actually lower correctly.
//
// V44 remains reserved (if-scoped let/var escape). V45 is duplicate
// `deploy`. V46 retired (TYPED-LIT-0) — explicit address typing only.

pub(super) fn check_using_decls(program: &Program, diags: &mut Vec<Diagnostic>) {
    // Build the set of all visible `pure fn` names — including those
    // inside `library` decls, since `using SomeLib for T;` references
    // them transitively.
    let mut fn_by_name: HashMap<&str, &PureFn> = HashMap::new();
    for pf in &program.pure_fns {
        fn_by_name.insert(pf.name.as_str(), pf);
    }
    let mut lib_by_name: HashMap<&str, &LibraryDecl> = HashMap::new();
    for lib in &program.libraries {
        lib_by_name.insert(lib.name.as_str(), lib);
        for pf in &lib.pure_fns {
            // Library-scoped pure fns are not auto-visible at program
            // scope; we look them up via the library's `using` form
            // separately.
            let _ = pf;
        }
    }

    for ud in &program.using_decls {
        let target_ty = &ud.target_type;

        // Build the list of `pure fn`s pulled in by this directive.
        let fns: Vec<&PureFn> = match &ud.items {
            UsingItems::Functions(names) => {
                let mut out = Vec::new();
                for name in names {
                    if let Some(pf) = fn_by_name.get(name.as_str()) {
                        out.push(*pf);
                    } else {
                        diags.push(Diagnostic::error(
                            "V56",
                            format!(
                                "`using {{ {} }} for {}`: pure function '{}' is not declared",
                                names.join(", "),
                                pretty_type(target_ty),
                                name
                            ),
                        ));
                    }
                }
                out
            }
            UsingItems::Library(lib_name) => {
                if let Some(lib) = lib_by_name.get(lib_name.as_str()) {
                    lib.pure_fns.iter().collect()
                } else {
                    diags.push(Diagnostic::error(
                        "V56",
                        format!(
                            "`using {} for {}`: library '{}' is not declared",
                            lib_name,
                            pretty_type(target_ty),
                            lib_name
                        ),
                    ));
                    Vec::new()
                }
            }
        };

        // V57: every fn referenced must take the `for` type as its
        // first parameter. Without this, `recv.fn(args)` cannot lower
        // to `fn(recv, args)` consistently.
        for pf in &fns {
            let Some(first) = pf.params.first() else {
                diags.push(Diagnostic::error(
                    "V57",
                    format!(
                        "`using` references pure function '{}', but it has no parameters \
                         (a method-call form `recv.{}(args)` needs at least one parameter to bind the receiver)",
                        pf.name, pf.name
                    ),
                ));
                continue;
            };
            if !types_compatible_for_using(&first.ty, target_ty) {
                diags.push(Diagnostic::error(
                    "V57",
                    format!(
                        "`using ... for {}`: pure function '{}' takes first argument of type {}, \
                         which does not match the `for` type",
                        pretty_type(target_ty),
                        pf.name,
                        pretty_type(&first.ty)
                    ),
                ));
            }
        }

        // V55: method-name collision with a built-in for the receiver
        // type. The collision list is intentionally conservative —
        // every name we know is intercepted by the existing
        // MethodCall lowering pipeline.
        for pf in &fns {
            if let Some(builtin) = is_builtin_method_for_type(target_ty, &pf.name) {
                diags.push(Diagnostic::error(
                    "V55",
                    format!(
                        "`using` method '{}' on type {} collides with built-in '{}'",
                        pf.name,
                        pretty_type(target_ty),
                        builtin
                    ),
                ));
            }
        }
    }
}

fn types_compatible_for_using(a: &Type, b: &Type) -> bool {
    // We compare on a normalised view: u256 / U256 / uint256 all match.
    use Type::*;
    let na = normalize_for_using(a);
    let nb = normalize_for_using(b);
    match (&na, &nb) {
        (Simple(x), Simple(y)) => x == y,
        (TypedAddress(x), TypedAddress(y)) => x == y,
        _ => na == nb,
    }
}

fn normalize_for_using(ty: &Type) -> Type {
    match ty {
        Type::Simple(s) => {
            let canon = match s.as_str() {
                "uint256" | "U256" | "u256" => "u256",
                "Address" | "address" => "address",
                other => other,
            };
            Type::Simple(canon.to_string())
        }
        _ => ty.clone(),
    }
}

fn is_builtin_method_for_type(ty: &Type, method: &str) -> Option<&'static str> {
    let canon = normalize_for_using(ty);
    match &canon {
        Type::Simple(name) if name == "String" => match method {
            "len" | "length" => Some("String::len"),
            "split" => Some("String::split"),
            "is_empty" => Some("String::is_empty"),
            _ => None,
        },
        Type::Generic(name, _) if name == "Vec" => match method {
            "len" | "length" => Some("Vec::len"),
            "is_empty" => Some("Vec::is_empty"),
            "push" => Some("Vec::push"),
            "iter" => Some("Vec::iter"),
            "enumerate" => Some("Vec::enumerate"),
            "filter" => Some("Vec::filter"),
            "map" => Some("Vec::map"),
            "take" => Some("Vec::take"),
            "fold" => Some("Vec::fold"),
            "collect" => Some("Vec::collect"),
            _ => None,
        },
        Type::Generic(name, _) if name == "HashMap" => match method {
            "insert" => Some("HashMap::insert"),
            "update" => Some("HashMap::update"),
            "remove" => Some("HashMap::remove"),
            "exists" => Some("HashMap::exists"),
            "cam_get" => Some("HashMap::cam_get"),
            "is_empty" => Some("HashMap::is_empty"),
            "contains" => Some("HashMap::contains"),
            "keys" => Some("HashMap::keys"),
            "values" => Some("HashMap::values"),
            "iter" => Some("HashMap::iter"),
            _ => None,
        },
        Type::Generic(name, _) if name == "Option" => match method {
            "is_some" | "is_none" | "unwrap_or" => Some("Option built-in"),
            _ => None,
        },
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Phase Library-3: `library` body validation (V47 / V48)
// ---------------------------------------------------------------------------
//
// V47 — `library` body may only contain `pure fn`, `const`, `type`.
//       Already enforced syntactically by the grammar; this check is
//       defence-in-depth and catches duplicate names *within* a library.
// V48 — `library` `pure fn` body references state / temporals / msg /
//       sys / macros. Reuses the existing V4 walker for pure-fn purity.

pub(super) fn check_libraries(program: &Program, diags: &mut Vec<Diagnostic>) {
    let state = entity::pure_fn_state_names(program);
    let mut seen_libs: HashSet<&str> = HashSet::new();
    for lib in &program.libraries {
        if !seen_libs.insert(lib.name.as_str()) {
            diags.push(Diagnostic::error(
                "V47",
                format!("Duplicate library '{}'", lib.name),
            ));
        }

        // V47 — names within a library must be unique among pure_fns /
        // constants / type_aliases.
        let mut seen_fns: HashSet<&str> = HashSet::new();
        for pf in &lib.pure_fns {
            if !seen_fns.insert(pf.name.as_str()) {
                diags.push(Diagnostic::error(
                    "V47",
                    format!(
                        "Library '{}': duplicate pure function '{}'",
                        lib.name, pf.name
                    ),
                ));
            }
        }
        let mut seen_consts: HashSet<&str> = HashSet::new();
        for c in &lib.constants {
            if !seen_consts.insert(c.name.as_str()) {
                diags.push(Diagnostic::error(
                    "V47",
                    format!("Library '{}': duplicate constant '{}'", lib.name, c.name),
                ));
            }
        }
        let mut seen_aliases: HashSet<&str> = HashSet::new();
        for ta in &lib.type_aliases {
            if !seen_aliases.insert(ta.name.as_str()) {
                diags.push(Diagnostic::error(
                    "V47",
                    format!("Library '{}': duplicate type alias '{}'", lib.name, ta.name),
                ));
            }
        }

        // V48 — library pure_fns must remain pure (no state, no
        // temporals, no msg/sys/macros). Reuse the existing pure-fn
        // checker by feeding each library fn through it.
        for pf in &lib.pure_fns {
            entity::check_pure_fn_purity(pf, &state, diags);
        }
    }
}

// ---------------------------------------------------------------------------
// V22: Namespace usage without import
// ---------------------------------------------------------------------------

pub(super) fn check_namespace_usage(program: &Program, diags: &mut Vec<Diagnostic>) {
    let imported: HashSet<&str> = program
        .imports
        .iter()
        .map(|i| i.namespace.as_str())
        .collect();

    for entity in &program.entities {
        for route in &entity.routes {
            for wc in &route.where_clauses {
                check_ns_expr(&wc.condition, &imported, &entity.name, &route.name, diags);
            }
            for action in route.body.all_actions() {
                check_ns_action(action, &imported, &entity.name, &route.name, diags);
            }
        }
        for member in &entity.members {
            for t in &member.transforms {
                check_ns_expr(&t.body, &imported, &entity.name, &member.name, diags);
            }
        }
        for mac in &entity.macros {
            check_ns_expr(&mac.body, &imported, &entity.name, &mac.name, diags);
        }
    }
}

fn ns_always_in_scope(namespace: &str) -> bool {
    namespace.starts_with("std::")
}

fn check_ns_expr(
    expr: &Expr,
    imported: &HashSet<&str>,
    entity_name: &str,
    ctx: &str,
    diags: &mut Vec<Diagnostic>,
) {
    match expr {
        Expr::NamespacedCall {
            namespace, args, ..
        } => {
            if !ns_always_in_scope(namespace) && !imported.contains(namespace.as_str()) {
                diags.push(Diagnostic::error(
                    "V22",
                    format!(
                        "Namespace '{}::' used in {}.{} without 'use {}' import",
                        namespace, entity_name, ctx, namespace
                    ),
                ));
            }
            for a in args {
                check_ns_expr(a, imported, entity_name, ctx, diags);
            }
        }
        Expr::EnumVariantWithData(ns, _, args) if imported.contains(ns.as_str()) => {
            for a in args {
                check_ns_expr(a, imported, entity_name, ctx, diags);
            }
        }
        Expr::BinOp(l, _, r) => {
            check_ns_expr(l, imported, entity_name, ctx, diags);
            check_ns_expr(r, imported, entity_name, ctx, diags);
        }
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) | Expr::Some(e) => {
            check_ns_expr(e, imported, entity_name, ctx, diags);
        }
        Expr::Index(e, idx) => {
            check_ns_expr(e, imported, entity_name, ctx, diags);
            check_ns_expr(idx, imported, entity_name, ctx, diags);
        }
        Expr::MethodCall(e, _, args) => {
            check_ns_expr(e, imported, entity_name, ctx, diags);
            for a in args {
                check_ns_expr(a, imported, entity_name, ctx, diags);
            }
        }
        Expr::MacroRef(_, args) => {
            for a in args {
                check_ns_expr(a, imported, entity_name, ctx, diags);
            }
        }
        Expr::FnCall(_, args) | Expr::EnumVariantWithData(_, _, args) => {
            for a in args {
                check_ns_expr(a, imported, entity_name, ctx, diags);
            }
        }
        Expr::If(c, t, e) => {
            check_ns_expr(c, imported, entity_name, ctx, diags);
            check_ns_expr(t, imported, entity_name, ctx, diags);
            if let Some(el) = e {
                check_ns_expr(el, imported, entity_name, ctx, diags);
            }
        }
        Expr::Let(_, val, body) | Expr::For(_, val, body) => {
            check_ns_expr(val, imported, entity_name, ctx, diags);
            check_ns_expr(body, imported, entity_name, ctx, diags);
        }
        Expr::Block(stmts) | Expr::ArrayLit(stmts) | Expr::Tuple(stmts) => {
            for s in stmts {
                check_ns_expr(s, imported, entity_name, ctx, diags);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                check_ns_expr(v, imported, entity_name, ctx, diags);
            }
        }
        Expr::RecordUpdate(base, fields) => {
            check_ns_expr(base, imported, entity_name, ctx, diags);
            for (_, v) in fields {
                check_ns_expr(v, imported, entity_name, ctx, diags);
            }
        }
        Expr::Match(subject, arms) => {
            check_ns_expr(subject, imported, entity_name, ctx, diags);
            for arm in arms {
                check_ns_expr(&arm.body, imported, entity_name, ctx, diags);
            }
        }
        Expr::Closure(_, body) => check_ns_expr(body, imported, entity_name, ctx, diags),
        Expr::Range(s, e) => {
            check_ns_expr(s, imported, entity_name, ctx, diags);
            check_ns_expr(e, imported, entity_name, ctx, diags);
        }
        _ => {}
    }
}

fn check_ns_action(
    action: &RouteAction,
    imported: &HashSet<&str>,
    entity_name: &str,
    route_name: &str,
    diags: &mut Vec<Diagnostic>,
) {
    match action {
        RouteAction::Effect {
            namespace,
            name,
            args,
        } => {
            if !ns_always_in_scope(namespace) && !imported.contains(namespace.as_str()) {
                diags.push(Diagnostic::error(
                    "V22",
                    format!(
                        "Namespace '{}::' used in {}.{} without 'use {}' import",
                        namespace, entity_name, route_name, namespace
                    ),
                ));
            }
            let deprecated_effects = ["setcode", "setCurrentCode", "setWasmHash", "resetStorage"];
            if namespace == "gosh" && deprecated_effects.contains(&name.as_str()) {
                diags.push(Diagnostic::warning("W6",
                    format!("'gosh::{}(...)' is deprecated in route '{}' of entity '{}'. Use 'gosh::updateCode(code, hash) with callback(...)' instead.",
                        name, route_name, entity_name)));
            }
            for a in args {
                check_ns_expr(a, imported, entity_name, route_name, diags);
            }
        }
        RouteAction::Let { value, .. } => {
            check_ns_expr(value, imported, entity_name, route_name, diags)
        }
        RouteAction::Return { values } => {
            for v in values {
                check_ns_expr(v, imported, entity_name, route_name, diags);
            }
        }
        RouteAction::Send {
            dest,
            args,
            send_options,
            ..
        } => {
            check_ns_expr(dest, imported, entity_name, route_name, diags);
            for a in args {
                check_ns_expr(a, imported, entity_name, route_name, diags);
            }
            if let Some(opts) = send_options {
                check_ns_expr(opts, imported, entity_name, route_name, diags);
            }
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            check_ns_expr(condition, imported, entity_name, route_name, diags);
            for a in then_actions {
                check_ns_action(a, imported, entity_name, route_name, diags);
            }
            for a in else_actions {
                check_ns_action(a, imported, entity_name, route_name, diags);
            }
        }
        RouteAction::Deploy {
            constructor_args,
            send_options,
            ..
        } => {
            for a in constructor_args {
                check_ns_expr(a, imported, entity_name, route_name, diags);
            }
            if let Some(opts) = send_options {
                check_ns_expr(opts, imported, entity_name, route_name, diags);
            }
        }
        RouteAction::Rescue { action, .. } => {
            check_ns_action(action, imported, entity_name, route_name, diags)
        }
        RouteAction::Throw { .. } => {}
        RouteAction::ThrowCustom { args, .. } => {
            for a in args {
                check_ns_expr(a, imported, entity_name, route_name, diags);
            }
        }
        RouteAction::CallRoute { args, .. } => {
            for a in args {
                check_ns_expr(a, imported, entity_name, route_name, diags);
            }
        }
        RouteAction::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            for a in update_args {
                check_ns_expr(a, imported, entity_name, route_name, diags);
            }
            for a in callback_args {
                check_ns_expr(a, imported, entity_name, route_name, diags);
            }
        }
        RouteAction::VarCall {
            args,
            dest,
            send_options,
            ..
        } => {
            for a in args {
                check_ns_expr(a, imported, entity_name, route_name, diags);
            }
            check_ns_expr(dest, imported, entity_name, route_name, diags);
            if let Some(opts) = send_options {
                check_ns_expr(opts, imported, entity_name, route_name, diags);
            }
        }
        RouteAction::For { iter, body, .. } => {
            check_ns_expr(iter, imported, entity_name, route_name, diags);
            for a in body {
                check_ns_action(a, imported, entity_name, route_name, diags);
            }
        }
        RouteAction::Emit { args, .. } => {
            for a in args {
                check_ns_expr(a, imported, entity_name, route_name, diags);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// V29: action-level `for` body restrictions
//
// An action-level `for <pat> in <iter> => [ <body> ]` runs its body
// once per element. Because the body emits actions in iteration
// order, we must preserve Cambrian's checks-effects-interactions
// invariant on EVM and Acki Nacki. Two specific shapes are unsafe:
//
//   1. `var x = msg(args) ~> dest;` inside the body — synchronous
//      cross-contract calls that capture a return value cannot be
//      sequenced safely across iterations (each capture must be
//      consumed in the same iteration; carrying captured values
//      across iterations would silently violate ordering on EVM).
//   2. `return(...)` and `gosh::updateCode(...) with cb(...)` — both
//      are terminal actions whose semantics ("return from the route"
//      / "atomic code upgrade") do not extend to per-iteration use
//      inside a loop.
//
// All other actions (Send, Deploy, Throw, Effect, Conditional, Let,
// CallRoute, Rescue, nested For) are allowed.
// ---------------------------------------------------------------------------

pub(super) fn check_action_for_body(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    for route in &entity.routes {
        for action in route.body.all_actions() {
            check_for_body_action(action, &entity.name, &route.name, diags);
        }
    }
}

fn check_for_body_action(
    action: &RouteAction,
    entity_name: &str,
    route_name: &str,
    diags: &mut Vec<Diagnostic>,
) {
    if let RouteAction::For { body, .. } = action {
        for inner in body {
            check_for_body_inner(inner, entity_name, route_name, diags);
            check_for_body_action(inner, entity_name, route_name, diags);
        }
    } else if let RouteAction::Conditional {
        then_actions,
        else_actions,
        ..
    } = action
    {
        for a in then_actions {
            check_for_body_action(a, entity_name, route_name, diags);
        }
        for a in else_actions {
            check_for_body_action(a, entity_name, route_name, diags);
        }
    } else if let RouteAction::Rescue { action: inner, .. } = action {
        check_for_body_action(inner, entity_name, route_name, diags);
    }
}

fn check_for_body_inner(
    action: &RouteAction,
    entity_name: &str,
    route_name: &str,
    diags: &mut Vec<Diagnostic>,
) {
    match action {
        RouteAction::VarCall { name, .. } => {
            diags.push(Diagnostic::error("V29",
                format!("var call '{}' is not allowed inside an action-level `for` loop in route '{}' of entity '{}'. \
                         Synchronous capturing cross-contract calls cannot be sequenced across iterations safely; \
                         move the captured computation outside the loop or replace it with a fire-and-forget send.",
                    name, route_name, entity_name)));
        }
        RouteAction::Return { .. } => {
            diags.push(Diagnostic::error("V29",
                format!("`return(...)` is not allowed inside an action-level `for` loop in route '{}' of entity '{}'. \
                         A return must be the last action in a route, not inside a loop iteration.",
                    route_name, entity_name)));
        }
        RouteAction::UpdateCode { .. } => {
            diags.push(Diagnostic::error("V29",
                format!("`gosh::updateCode(...) with ...` is not allowed inside an action-level `for` loop in route '{}' of entity '{}'. \
                         Code upgrades are terminal actions and must execute exactly once.",
                    route_name, entity_name)));
        }
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            for a in then_actions {
                check_for_body_inner(a, entity_name, route_name, diags);
            }
            for a in else_actions {
                check_for_body_inner(a, entity_name, route_name, diags);
            }
        }
        RouteAction::Rescue { action: inner, .. } => {
            check_for_body_inner(inner, entity_name, route_name, diags);
        }
        RouteAction::For { body, .. } => {
            for a in body {
                check_for_body_inner(a, entity_name, route_name, diags);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// V24/V25: VarCall scoping and define-before-use
// ---------------------------------------------------------------------------

pub(super) fn check_var_call_scoping(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    let member_names: HashSet<&str> = entity.members.iter().map(|m| m.name.as_str()).collect();

    for route in &entity.routes {
        let route_params: HashSet<&str> = route.params.iter().map(|p| p.name.as_str()).collect();
        let mut defined_vars: Vec<String> = Vec::new();

        let actions = route.body.all_actions();
        for action in &actions {
            collect_var_defs_and_check(
                action,
                entity,
                route,
                &route_params,
                &member_names,
                &mut defined_vars,
                diags,
            );
        }

        if defined_vars.is_empty() {
            // Even when no `var` exists, per-phase wheres still need to be
            // structurally checked elsewhere (e.g. EVM compat); but no scoping
            // work is needed here.
            continue;
        }

        let all_var_names: HashSet<&str> = defined_vars.iter().map(|s| s.as_str()).collect();

        // (a) Member transforms: existing rule.
        for member in &entity.members {
            for transform in &member.transforms {
                if transform.route_name != route.name {
                    continue;
                }
                let allowed_vars = if let Some(ref phase) = transform.phase {
                    let phase_vars = collect_var_defs_up_to_phase(route, phase);
                    phase_vars.into_iter().collect::<HashSet<_>>()
                } else {
                    HashSet::new()
                };
                let allowed_refs: HashSet<&str> = allowed_vars.iter().map(|s| s.as_str()).collect();
                check_var_refs_in_expr(
                    &transform.body,
                    &all_var_names,
                    &allowed_refs,
                    &entity.name,
                    &route.name,
                    &member.name,
                    diags,
                );
            }
        }

        // (b) Per-phase where clauses: may reference vars defined in
        //     STRICTLY EARLIER phases (same rule as transforms tagged at the
        //     same phase). A per-phase where on phase Pᵢ may NOT reference a
        //     var defined inside Pᵢ itself or in any later phase.
        if let RouteBody::Phased(phases) | RouteBody::Mixed(phases, _) = &route.body {
            for phase in phases {
                if phase.where_clauses.is_empty() {
                    continue;
                }
                let allowed_vars = collect_var_defs_up_to_phase(route, &phase.name);
                let allowed_refs: HashSet<&str> = allowed_vars.iter().map(|s| s.as_str()).collect();
                for wc in &phase.where_clauses {
                    check_phase_where_var_refs(
                        &wc.condition,
                        &all_var_names,
                        &allowed_refs,
                        &entity.name,
                        &route.name,
                        &phase.name,
                        diags,
                    );
                }
            }
        }

        // (c) Route-level where clauses: must NOT reference any var (vars
        //     are not yet defined when route-level wheres run). Emit V27
        //     with a clear remediation hint.
        for wc in &route.where_clauses {
            check_route_where_no_var_refs(
                &wc.condition,
                &all_var_names,
                &entity.name,
                &route.name,
                diags,
            );
        }
    }
}

fn check_phase_where_var_refs(
    expr: &Expr,
    all_var_names: &HashSet<&str>,
    allowed: &HashSet<&str>,
    entity_name: &str,
    route_name: &str,
    phase_name: &str,
    diags: &mut Vec<Diagnostic>,
) {
    match expr {
        Expr::Ident(name)
            if all_var_names.contains(name.as_str()) && !allowed.contains(name.as_str()) =>
        {
            diags.push(Diagnostic::error(
                "V28",
                format!(
                    "var '{}' referenced in per-phase 'where' clause of phase '{}' in {}.{} \
                     is not available at this point (defined in same or later phase). \
                     Per-phase wheres may only reference vars defined in STRICTLY EARLIER phases.",
                    name, phase_name, entity_name, route_name
                ),
            ));
        }
        Expr::BinOp(l, _, r) => {
            check_phase_where_var_refs(
                l,
                all_var_names,
                allowed,
                entity_name,
                route_name,
                phase_name,
                diags,
            );
            check_phase_where_var_refs(
                r,
                all_var_names,
                allowed,
                entity_name,
                route_name,
                phase_name,
                diags,
            );
        }
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) | Expr::Some(e) => {
            check_phase_where_var_refs(
                e,
                all_var_names,
                allowed,
                entity_name,
                route_name,
                phase_name,
                diags,
            );
        }
        Expr::Index(b, i) => {
            check_phase_where_var_refs(
                b,
                all_var_names,
                allowed,
                entity_name,
                route_name,
                phase_name,
                diags,
            );
            check_phase_where_var_refs(
                i,
                all_var_names,
                allowed,
                entity_name,
                route_name,
                phase_name,
                diags,
            );
        }
        Expr::MethodCall(b, _, args) => {
            check_phase_where_var_refs(
                b,
                all_var_names,
                allowed,
                entity_name,
                route_name,
                phase_name,
                diags,
            );
            for a in args {
                check_phase_where_var_refs(
                    a,
                    all_var_names,
                    allowed,
                    entity_name,
                    route_name,
                    phase_name,
                    diags,
                );
            }
        }
        Expr::FnCall(_, args) | Expr::MacroRef(_, args) => {
            for a in args {
                check_phase_where_var_refs(
                    a,
                    all_var_names,
                    allowed,
                    entity_name,
                    route_name,
                    phase_name,
                    diags,
                );
            }
        }
        Expr::If(c, t, e) => {
            check_phase_where_var_refs(
                c,
                all_var_names,
                allowed,
                entity_name,
                route_name,
                phase_name,
                diags,
            );
            check_phase_where_var_refs(
                t,
                all_var_names,
                allowed,
                entity_name,
                route_name,
                phase_name,
                diags,
            );
            if let Some(e) = e {
                check_phase_where_var_refs(
                    e,
                    all_var_names,
                    allowed,
                    entity_name,
                    route_name,
                    phase_name,
                    diags,
                );
            }
        }
        _ => {}
    }
}

fn check_route_where_no_var_refs(
    expr: &Expr,
    all_var_names: &HashSet<&str>,
    entity_name: &str,
    route_name: &str,
    diags: &mut Vec<Diagnostic>,
) {
    match expr {
        Expr::Ident(name) if all_var_names.contains(name.as_str()) => {
            diags.push(Diagnostic::error("V27",
                format!(
                    "where clauses on {}.{} cannot reference var '{}' (vars are not yet defined when route-level wheres run); \
                     use a per-phase where on a later phase, or a conditional throw inside the var-defining phase.",
                    entity_name, route_name, name)));
        }
        Expr::BinOp(l, _, r) => {
            check_route_where_no_var_refs(l, all_var_names, entity_name, route_name, diags);
            check_route_where_no_var_refs(r, all_var_names, entity_name, route_name, diags);
        }
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) | Expr::Some(e) => {
            check_route_where_no_var_refs(e, all_var_names, entity_name, route_name, diags);
        }
        Expr::Index(b, i) => {
            check_route_where_no_var_refs(b, all_var_names, entity_name, route_name, diags);
            check_route_where_no_var_refs(i, all_var_names, entity_name, route_name, diags);
        }
        Expr::MethodCall(b, _, args) => {
            check_route_where_no_var_refs(b, all_var_names, entity_name, route_name, diags);
            for a in args {
                check_route_where_no_var_refs(a, all_var_names, entity_name, route_name, diags);
            }
        }
        Expr::FnCall(_, args) | Expr::MacroRef(_, args) => {
            for a in args {
                check_route_where_no_var_refs(a, all_var_names, entity_name, route_name, diags);
            }
        }
        Expr::If(c, t, e) => {
            check_route_where_no_var_refs(c, all_var_names, entity_name, route_name, diags);
            check_route_where_no_var_refs(t, all_var_names, entity_name, route_name, diags);
            if let Some(e) = e {
                check_route_where_no_var_refs(e, all_var_names, entity_name, route_name, diags);
            }
        }
        _ => {}
    }
}

fn collect_var_defs_and_check(
    action: &RouteAction,
    entity: &Entity,
    route: &Route,
    route_params: &HashSet<&str>,
    member_names: &HashSet<&str>,
    defined_vars: &mut Vec<String>,
    diags: &mut Vec<Diagnostic>,
) {
    match action {
        RouteAction::VarCall { name, .. } => {
            let ctx = format!("{}.{}", entity.name, route.name);
            if route_params.contains(name.as_str()) {
                diags.push(Diagnostic::error(
                    "V25",
                    format!(
                        "var '{}' in {} shadows route parameter '{}'",
                        name, ctx, name
                    ),
                ));
            }
            if member_names.contains(name.as_str()) {
                diags.push(Diagnostic::error(
                    "V25",
                    format!("var '{}' in {} shadows state member '{}'", name, ctx, name),
                ));
            }
            if defined_vars.contains(name) {
                diags.push(Diagnostic::error(
                    "V25",
                    format!("duplicate var '{}' in {}", name, ctx),
                ));
            }
            defined_vars.push(name.clone());
        }
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            for a in then_actions {
                collect_var_defs_and_check(
                    a,
                    entity,
                    route,
                    route_params,
                    member_names,
                    defined_vars,
                    diags,
                );
            }
            for a in else_actions {
                collect_var_defs_and_check(
                    a,
                    entity,
                    route,
                    route_params,
                    member_names,
                    defined_vars,
                    diags,
                );
            }
        }
        RouteAction::Rescue { action, .. } => {
            collect_var_defs_and_check(
                action,
                entity,
                route,
                route_params,
                member_names,
                defined_vars,
                diags,
            );
        }
        _ => {}
    }
}

fn collect_var_defs_up_to_phase(route: &Route, target_phase: &str) -> Vec<String> {
    let mut vars = Vec::new();
    if let RouteBody::Phased(phases) = &route.body {
        for phase in phases {
            if phase.name == target_phase {
                break;
            }
            for action in &phase.actions {
                collect_var_names_in_action(action, &mut vars);
            }
        }
    }
    vars
}

fn collect_var_names_in_action(action: &RouteAction, vars: &mut Vec<String>) {
    match action {
        RouteAction::VarCall { name, .. } => vars.push(name.clone()),
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            for a in then_actions {
                collect_var_names_in_action(a, vars);
            }
            for a in else_actions {
                collect_var_names_in_action(a, vars);
            }
        }
        RouteAction::Rescue { action, .. } => collect_var_names_in_action(action, vars),
        _ => {}
    }
}

fn collect_var_defs_for_route(route: &Route) -> Vec<String> {
    let mut vars = Vec::new();
    for action in route.body.all_actions() {
        collect_var_names_in_action(action, &mut vars);
    }
    vars
}

fn check_var_refs_in_expr(
    expr: &Expr,
    all_var_names: &HashSet<&str>,
    allowed: &HashSet<&str>,
    entity_name: &str,
    route_name: &str,
    member_name: &str,
    diags: &mut Vec<Diagnostic>,
) {
    match expr {
        Expr::Ident(name)
            if all_var_names.contains(name.as_str()) && !allowed.contains(name.as_str()) =>
        {
            diags.push(Diagnostic::error("V26",
                format!(
                    "var '{}' referenced in member '{}' transform for route '{}' in entity '{}' \
                     is not available at this point (defined in same or later phase, or in unphased route)",
                    name, member_name, route_name, entity_name)));
        }
        Expr::BinOp(l, _, r) => {
            check_var_refs_in_expr(
                l,
                all_var_names,
                allowed,
                entity_name,
                route_name,
                member_name,
                diags,
            );
            check_var_refs_in_expr(
                r,
                all_var_names,
                allowed,
                entity_name,
                route_name,
                member_name,
                diags,
            );
        }
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) | Expr::Some(e) => {
            check_var_refs_in_expr(
                e,
                all_var_names,
                allowed,
                entity_name,
                route_name,
                member_name,
                diags,
            );
        }
        Expr::Index(b, i) => {
            check_var_refs_in_expr(
                b,
                all_var_names,
                allowed,
                entity_name,
                route_name,
                member_name,
                diags,
            );
            check_var_refs_in_expr(
                i,
                all_var_names,
                allowed,
                entity_name,
                route_name,
                member_name,
                diags,
            );
        }
        Expr::MethodCall(b, _, args) => {
            check_var_refs_in_expr(
                b,
                all_var_names,
                allowed,
                entity_name,
                route_name,
                member_name,
                diags,
            );
            for a in args {
                check_var_refs_in_expr(
                    a,
                    all_var_names,
                    allowed,
                    entity_name,
                    route_name,
                    member_name,
                    diags,
                );
            }
        }
        Expr::FnCall(_, args) | Expr::MacroRef(_, args) => {
            for a in args {
                check_var_refs_in_expr(
                    a,
                    all_var_names,
                    allowed,
                    entity_name,
                    route_name,
                    member_name,
                    diags,
                );
            }
        }
        Expr::If(c, t, e) => {
            check_var_refs_in_expr(
                c,
                all_var_names,
                allowed,
                entity_name,
                route_name,
                member_name,
                diags,
            );
            check_var_refs_in_expr(
                t,
                all_var_names,
                allowed,
                entity_name,
                route_name,
                member_name,
                diags,
            );
            if let Some(e) = e {
                check_var_refs_in_expr(
                    e,
                    all_var_names,
                    allowed,
                    entity_name,
                    route_name,
                    member_name,
                    diags,
                );
            }
        }
        Expr::Let(_, val, body) => {
            check_var_refs_in_expr(
                val,
                all_var_names,
                allowed,
                entity_name,
                route_name,
                member_name,
                diags,
            );
            check_var_refs_in_expr(
                body,
                all_var_names,
                allowed,
                entity_name,
                route_name,
                member_name,
                diags,
            );
        }
        Expr::Block(items) => {
            for item in items {
                check_var_refs_in_expr(
                    item,
                    all_var_names,
                    allowed,
                    entity_name,
                    route_name,
                    member_name,
                    diags,
                );
            }
        }
        Expr::RecordConstruct(_, fields) | Expr::RecordUpdate(_, fields) => {
            for (_, v) in fields {
                check_var_refs_in_expr(
                    v,
                    all_var_names,
                    allowed,
                    entity_name,
                    route_name,
                    member_name,
                    diags,
                );
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Target-compat orchestrator (P1)
// ---------------------------------------------------------------------------

/// Run the rule families that apply to `target`:
/// - **E-rules** when `target.models_evm()` (EVM domain — covers both `--target
///   evm` and `--target lean`);
/// - **L-rules** when `target.language()` is Lean;
/// - **non-EVM E12** when the domain is not EVM.
///
/// Existing `check_*_target_compat*` functions remain public for unit tests;
/// CLI / project dispatch should call this orchestrator only.
pub fn check_target_compat(
    program: &Program,
    target: crate::target::Target,
    deterministic: bool,
) -> Vec<Diagnostic> {
    rules::clear_family_cache();
    let mut diags = Vec::new();
    let ctx = ValidateCtx::with_target(program, target, deterministic);
    // Skip Universal — those already ran in `validate()`. Run every other
    // rule whose binding applies to `target` (domain / language / pair /
    // Targeted / NotDomain).
    for entry in RULES {
        if matches!(entry.binding, RuleBinding::Universal) {
            continue;
        }
        if entry.binding.applies(target) {
            (entry.run)(&ctx, &mut diags);
        }
    }
    rules::clear_family_cache();
    diags
}

// ---------------------------------------------------------------------------
// Non-EVM target compatibility checks
// ---------------------------------------------------------------------------

/// Reject `evm::*` field references when emitting for a non-EVM target.
/// `evm::` is the EVM-specific environment-access prefix (`evm::timestamp`,
/// `evm::block_number`, ...) and has no analogue on Acki Nacki / Substrate /
/// other targets. The mirror counterpart is `gosh::` for TVM-only calls,
/// which `check_evm_target_compat` warns about when used on EVM.
pub fn check_non_evm_target_compat(program: &Program, target_name: &str) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for entity in &program.entities {
        for route in &entity.routes {
            for wc in &route.where_clauses {
                check_no_evm_field(
                    &wc.condition,
                    &entity.name,
                    &route.name,
                    target_name,
                    &mut diags,
                );
            }
            for action in route.body.all_actions() {
                check_no_evm_field_in_action(
                    action,
                    &entity.name,
                    &route.name,
                    target_name,
                    &mut diags,
                );
            }
        }
        for member in &entity.members {
            for tr in &member.transforms {
                check_no_evm_field(
                    &tr.body,
                    &entity.name,
                    &tr.route_name,
                    target_name,
                    &mut diags,
                );
            }
        }
        for mac in &entity.macros {
            check_no_evm_field(&mac.body, &entity.name, &mac.name, target_name, &mut diags);
        }
    }
    for pf in &program.pure_fns {
        check_no_evm_field(&pf.body, "pure_fn", &pf.name, target_name, &mut diags);
    }
    diags
}

fn check_no_evm_field(
    expr: &Expr,
    entity_name: &str,
    route_name: &str,
    target_name: &str,
    diags: &mut Vec<Diagnostic>,
) {
    match expr {
        // `evm::*(args)` is reserved for EVM-specific state-changing
        // intrinsics (mirrors how `gosh::*` is rejected on EVM via E02).
        // Reject any usage when emitting for a non-EVM target. Both
        // `NamespacedCall { namespace == "evm" }` (turbofish form) and
        // `EnumVariantWithData("evm", ...)` (bare form) parse routes
        // through here.
        Expr::NamespacedCall {
            namespace, name, ..
        } if namespace == "evm" => {
            diags.push(Diagnostic::error("E12", format!(
                "entity '{}' route '{}': evm::{}(...) is EVM-target-specific and cannot be used when target='{}'",
                entity_name, route_name, name, target_name)));
        }
        Expr::EnumVariantWithData(en, name, args) if en == "evm" => {
            diags.push(Diagnostic::error("E12", format!(
                "entity '{}' route '{}': evm::{}(...) is EVM-target-specific and cannot be used when target='{}'",
                entity_name, route_name, name, target_name)));
            for a in args {
                check_no_evm_field(a, entity_name, route_name, target_name, diags);
            }
        }
        Expr::BinOp(l, _, r) => {
            check_no_evm_field(l, entity_name, route_name, target_name, diags);
            check_no_evm_field(r, entity_name, route_name, target_name, diags);
        }
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Some(e) | Expr::Cast(e, _) => {
            check_no_evm_field(e, entity_name, route_name, target_name, diags);
        }
        Expr::Index(b, i) => {
            check_no_evm_field(b, entity_name, route_name, target_name, diags);
            check_no_evm_field(i, entity_name, route_name, target_name, diags);
        }
        Expr::MethodCall(b, _, args) => {
            check_no_evm_field(b, entity_name, route_name, target_name, diags);
            for a in args {
                check_no_evm_field(a, entity_name, route_name, target_name, diags);
            }
        }
        Expr::FnCall(_, args)
        | Expr::ArrayLit(args)
        | Expr::Tuple(args)
        | Expr::EnumVariantWithData(_, _, args)
        | Expr::MacroRef(_, args) => {
            for a in args {
                check_no_evm_field(a, entity_name, route_name, target_name, diags);
            }
        }
        Expr::NamespacedCall { args, .. } => {
            for a in args {
                check_no_evm_field(a, entity_name, route_name, target_name, diags);
            }
        }
        Expr::If(c, t, e) => {
            check_no_evm_field(c, entity_name, route_name, target_name, diags);
            check_no_evm_field(t, entity_name, route_name, target_name, diags);
            if let Some(e) = e {
                check_no_evm_field(e, entity_name, route_name, target_name, diags);
            }
        }
        Expr::Let(_, v, b) => {
            check_no_evm_field(v, entity_name, route_name, target_name, diags);
            check_no_evm_field(b, entity_name, route_name, target_name, diags);
        }
        Expr::Block(items) => {
            for it in items {
                check_no_evm_field(it, entity_name, route_name, target_name, diags);
            }
        }
        Expr::Match(s, arms) => {
            check_no_evm_field(s, entity_name, route_name, target_name, diags);
            for arm in arms {
                check_no_evm_field(&arm.body, entity_name, route_name, target_name, diags);
            }
        }
        Expr::RecordConstruct(_, fields) | Expr::RecordUpdate(_, fields) => {
            for (_, v) in fields {
                check_no_evm_field(v, entity_name, route_name, target_name, diags);
            }
        }
        Expr::Range(a, b) | Expr::For(_, a, b) => {
            check_no_evm_field(a, entity_name, route_name, target_name, diags);
            check_no_evm_field(b, entity_name, route_name, target_name, diags);
        }
        Expr::Closure(_, body) => {
            check_no_evm_field(body, entity_name, route_name, target_name, diags);
        }
        _ => {}
    }
}

fn check_no_evm_field_in_action(
    action: &RouteAction,
    entity_name: &str,
    route_name: &str,
    target_name: &str,
    diags: &mut Vec<Diagnostic>,
) {
    match action {
        RouteAction::Send {
            args,
            dest,
            send_options,
            ..
        } => {
            for a in args {
                check_no_evm_field(a, entity_name, route_name, target_name, diags);
            }
            check_no_evm_field(dest, entity_name, route_name, target_name, diags);
            if let Some(o) = send_options {
                check_no_evm_field(o, entity_name, route_name, target_name, diags);
            }
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            check_no_evm_field(condition, entity_name, route_name, target_name, diags);
            for a in then_actions {
                check_no_evm_field_in_action(a, entity_name, route_name, target_name, diags);
            }
            for a in else_actions {
                check_no_evm_field_in_action(a, entity_name, route_name, target_name, diags);
            }
        }
        RouteAction::Return { values } => {
            for v in values {
                check_no_evm_field(v, entity_name, route_name, target_name, diags);
            }
        }
        RouteAction::Let { value, .. } => {
            check_no_evm_field(value, entity_name, route_name, target_name, diags);
        }
        RouteAction::Effect { args, .. } => {
            for a in args {
                check_no_evm_field(a, entity_name, route_name, target_name, diags);
            }
        }
        RouteAction::Deploy {
            send_options,
            constructor_args,
            ..
        } => {
            if let Some(o) = send_options {
                check_no_evm_field(o, entity_name, route_name, target_name, diags);
            }
            for a in constructor_args {
                check_no_evm_field(a, entity_name, route_name, target_name, diags);
            }
        }
        RouteAction::Rescue { action, .. } => {
            check_no_evm_field_in_action(action, entity_name, route_name, target_name, diags);
        }
        RouteAction::CallRoute { args, .. } => {
            for a in args {
                check_no_evm_field(a, entity_name, route_name, target_name, diags);
            }
        }
        RouteAction::VarCall {
            args,
            dest,
            send_options,
            ..
        } => {
            for a in args {
                check_no_evm_field(a, entity_name, route_name, target_name, diags);
            }
            check_no_evm_field(dest, entity_name, route_name, target_name, diags);
            if let Some(o) = send_options {
                check_no_evm_field(o, entity_name, route_name, target_name, diags);
            }
        }
        RouteAction::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            for a in update_args {
                check_no_evm_field(a, entity_name, route_name, target_name, diags);
            }
            for a in callback_args {
                check_no_evm_field(a, entity_name, route_name, target_name, diags);
            }
        }
        RouteAction::Throw { .. } => {}
        RouteAction::ThrowCustom { args, .. } => {
            for a in args {
                check_no_evm_field(a, entity_name, route_name, target_name, diags);
            }
        }
        RouteAction::For { iter, body, .. } => {
            check_no_evm_field(iter, entity_name, route_name, target_name, diags);
            for a in body {
                check_no_evm_field_in_action(a, entity_name, route_name, target_name, diags);
            }
        }
        RouteAction::Emit { args, .. } => {
            for a in args {
                check_no_evm_field(a, entity_name, route_name, target_name, diags);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// EVM-domain compatibility checks (E-rules)
//
// These apply whenever `Target::models_evm()` is true — i.e. both
// `--target evm` (Solidity) and `--target lean` (Lean modelling EVM).
// ---------------------------------------------------------------------------

pub fn check_evm_target_compat(program: &Program) -> Vec<Diagnostic> {
    check_evm_target_compat_with(program, false)
}

/// Phase EVM-2 K1: deterministic-aware variant. Pass `true` when the
/// project / backend opts in via `deterministic_addresses: true` so
/// E16 doesn't reject `Expr::AddressOf` (which lowers to a real
/// CREATE2 expression in that mode).
pub fn check_evm_target_compat_with(program: &Program, deterministic: bool) -> Vec<Diagnostic> {
    set_program_pure_fn_names(program);
    let mut diags = Vec::new();

    // Phase EVM-2 K2 (E18-E21): type-erasure rejection. Walk every
    // type-bearing declaration / expression site and emit a focused
    // E-rule diagnostic when a shape would silently lower to
    // `bytes` / `uint256`.
    check_evm_compat_program_types(program, &mut diags);

    for imp in &program.imports {
        if imp.namespace == "gosh" {
            diags.push(Diagnostic::warning(
                "E01",
                "use gosh: gosh namespace has limited EVM support; most gosh:: calls become no-ops"
                    .to_string(),
            ));
        }
    }

    // Phase EVM-6 M1 (E22): typed sends to undeclared external entities.
    check_evm_extern_entity_refs(program, &mut diags);

    // Phase EVM-6 M2 (V33, closes EVM_GAPS §1.26): from-clause arity
    // check. Non-deterministic mode requires single-arg, deterministic
    // mode requires args.len() == identity_count of the target entity.
    check_evm_from_clause_arity(program, deterministic, &mut diags);

    if deterministic {
        for entity in &program.entities {
            entity::check_factory_only_required_on_init(entity, &mut diags);
            entity::check_init_route_msg_sender_under_factory(entity, &mut diags);
        }
    }

    // Phase EVM-P0-E (V40/V41): `receive` / `fallback` route shape on EVM.
    check_receive_fallback(program, &mut diags);

    for entity in &program.entities {
        for route in &entity.routes {
            if !route.from_clauses.is_empty() {
                diags.push(Diagnostic::warning("E09",
                    format!(
                        "entity '{}' route '{}': from-clause sender verification maps to require(msg.sender == ...) on EVM",
                        entity.name, route.name)));
            }

            // V40 requires `accept` on `receive`, so it carries meaning there.
            if route.is_accept && route.name != "receive" {
                diags.push(Diagnostic::warning(
                    "E05",
                    format!(
                        "entity '{}' route '{}': 'accept' modifier has no EVM equivalent (ignored)",
                        entity.name, route.name
                    ),
                ));
            }

            if let Some(tag) = &route.recover_tag {
                diags.push(Diagnostic::error(
                    "E26",
                    format!(
                        "entity '{}' route '{}': `recover {}` is TVM-specific bounce handling (Acki Nacki only). The EVM domain has no analogue and no try/catch / contained-failure lowering.",
                        entity.name, route.name, tag
                    ),
                ));
            }

            check_evm_compat_actions(
                deterministic,
                route.body.all_actions().as_slice(),
                &entity.name,
                &route.name,
                &mut diags,
            );

            for wc in &route.where_clauses {
                check_evm_compat_expr(
                    &wc.condition,
                    &entity.name,
                    &route.name,
                    deterministic,
                    &mut diags,
                );
            }
        }

        for member in &entity.members {
            for transform in &member.transforms {
                check_evm_compat_expr(
                    &transform.body,
                    &entity.name,
                    &transform.route_name,
                    deterministic,
                    &mut diags,
                );
            }
        }

        // Phase EVM-2 K1 (E17): HashMap member-transform shape check.
        check_evm_compat_member_transforms(entity, &mut diags);

        for mac in &entity.macros {
            check_evm_compat_expr(
                &mac.body,
                &entity.name,
                &mac.name,
                deterministic,
                &mut diags,
            );
        }
    }

    for pf in &program.pure_fns {
        check_evm_compat_expr(&pf.body, "pure_fn", &pf.name, deterministic, &mut diags);
    }

    for test in &program.tests {
        for step in &test.body {
            match step {
                crate::ast::TestStep::SetRegistry { entity_name, .. } => {
                    diags.push(Diagnostic::warning(
                        "E10",
                        format!(
                            "test '{}': registry {} block is TVM-specific, skipped on EVM target",
                            test.name, entity_name
                        ),
                    ));
                }
                crate::ast::TestStep::ExpectEffects { elements } => {
                    for elem in elements {
                        if let crate::ast::TestEffectElement::Effect(
                            crate::ast::TestEffect::PlatformEffect { name, .. },
                        ) = elem
                        {
                            diags.push(Diagnostic::warning(
                                "E11",
                                format!(
                                    "test '{}': effect '{}' is TVM-specific, skipped on EVM target",
                                    test.name, name
                                ),
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Same E10/E11 scan over property bodies (the canonical home of
    // fuzz/property logical steps).
    for prop in &program.properties {
        for step in &prop.body {
            match step {
                crate::ast::TestStep::SetRegistry { entity_name, .. } => {
                    diags.push(Diagnostic::warning("E10",
                        format!(
                            "property '{}': registry {} block is TVM-specific, skipped on EVM target",
                            prop.name, entity_name)));
                }
                crate::ast::TestStep::ExpectEffects { elements } => {
                    for elem in elements {
                        if let crate::ast::TestEffectElement::Effect(
                            crate::ast::TestEffect::PlatformEffect { name, .. },
                        ) = elem
                        {
                            diags.push(Diagnostic::warning("E11",
                                format!(
                                    "property '{}': effect '{}' is TVM-specific, skipped on EVM target",
                                    prop.name, name)));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    diags
}

// ---------------------------------------------------------------------------
// E22: Phase EVM-6 M1 — typed sends to undeclared external entities.
//
// When a `~> dest` (named send) or `var x = msg(args) ~> dest` (var-call)
// targets an entity name that's neither in `program.entities` nor
// `program.extern_entities`, the EVM backend would emit an empty
// `interface IExt { /* add route signatures as needed */ }` placeholder
// — which Solidity then rejects when the call site references a member
// the interface doesn't declare. Promote to a hard validator error so
// users either declare the foreign entity via `extern entity X { ... }`
// or include the entity in the project.
// ---------------------------------------------------------------------------

fn check_evm_extern_entity_refs(program: &Program, diags: &mut Vec<Diagnostic>) {
    use std::collections::HashSet;
    let known: HashSet<&str> = program
        .entities
        .iter()
        .map(|e| e.name.as_str())
        .chain(program.extern_entities.iter().map(|e| e.name.as_str()))
        .collect();

    for entity in &program.entities {
        for route in &entity.routes {
            for action in route.body.all_actions() {
                check_evm_extern_action(action, entity, route, &known, diags);
            }
        }
    }
}

fn check_evm_extern_action(
    action: &RouteAction,
    entity: &Entity,
    route: &Route,
    known: &std::collections::HashSet<&str>,
    diags: &mut Vec<Diagnostic>,
) {
    match action {
        RouteAction::Send {
            message: Some(msg),
            dest,
            ..
        } => {
            if let Some(target) = entity::resolve_dest_entity_for_evm(dest, entity, route) {
                if !known.contains(target.as_str()) {
                    diags.push(Diagnostic::error("E22",
                        format!(
                            "entity '{}' route '{}': named send '{}' targets undeclared external entity '{}'. \
                             Declare via `extern entity {} {{ route {}(...); }}` or include the entity in the project.",
                            entity.name, route.name, msg, target, target, msg)));
                }
            }
        }
        RouteAction::VarCall { dest, message, .. } => {
            if let Some(target) = entity::resolve_dest_entity_for_evm(dest, entity, route) {
                if !known.contains(target.as_str()) {
                    diags.push(Diagnostic::error("E22",
                        format!(
                            "entity '{}' route '{}': var call '{}' targets undeclared external entity '{}'. \
                             Declare via `extern entity {} {{ route {}(...) -> T; }}` or include the entity in the project.",
                            entity.name, route.name, message, target, target, message)));
                }
            }
        }
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            for a in then_actions {
                check_evm_extern_action(a, entity, route, known, diags);
            }
            for a in else_actions {
                check_evm_extern_action(a, entity, route, known, diags);
            }
        }
        RouteAction::Rescue { action, .. } => {
            check_evm_extern_action(action, entity, route, known, diags);
        }
        RouteAction::For { body, .. } => {
            for a in body {
                check_evm_extern_action(a, entity, route, known, diags);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// V33: Phase EVM-6 M2 — `from Entity(args)` arity check (EVM_GAPS §1.26).
//
// Non-deterministic mode: the EVM lowering requires a single address arg
// (literal or computed); anything else silently lowers to literal `false`
// (always-revert). Deterministic mode: args.len() must equal the target
// entity's identity-member count, which feeds the CREATE2 address
// computation. Mismatches in either mode produce always-failing routes
// with no codegen diagnostic — V33 promotes both to hard validator
// errors.
//
// Skipped when the target is `extern entity` (signature unknown) or
// unknown (V23 already covers it).
// ---------------------------------------------------------------------------

fn check_evm_from_clause_arity(
    program: &Program,
    deterministic: bool,
    diags: &mut Vec<Diagnostic>,
) {
    for entity in &program.entities {
        for route in &entity.routes {
            for fc in &route.from_clauses {
                if fc.kind != FromClauseKind::Entity {
                    continue;
                }
                let Some(target) = program.entities.iter().find(|e| e.name == fc.entity_name)
                else {
                    // V23 / W5 already cover unknown targets, and extern
                    // entity references are intentionally skipped (we
                    // don't know their identity count).
                    continue;
                };

                if deterministic {
                    let identity_count = target.members.iter().filter(|m| m.is_identity).count();
                    if fc.args.len() != identity_count {
                        diags.push(Diagnostic::error("V33",
                            format!(
                                "entity '{}' route '{}': `from {}({})` has {} argument(s); deterministic-addresses mode requires exactly {} (one per identity member of '{}').",
                                entity.name, route.name, fc.entity_name,
                                if fc.args.is_empty() { "" } else { "..." },
                                fc.args.len(), identity_count, fc.entity_name,
                            )));
                    }
                } else if fc.args.len() != 1 {
                    diags.push(Diagnostic::error("V33",
                        format!(
                            "entity '{}' route '{}': `from {}({})` has {} argument(s); without `deterministic_addresses: true` the EVM lowering only supports a single address argument. Set `deterministic_addresses: true` in project.yaml to use multi-arg from-clauses.",
                            entity.name, route.name, fc.entity_name,
                            if fc.args.is_empty() { "" } else { "..." },
                            fc.args.len(),
                        )));
                }
            }
        }
    }
}

fn check_evm_compat_actions(
    deterministic: bool,
    actions: &[&RouteAction],
    entity_name: &str,
    route_name: &str,
    diags: &mut Vec<Diagnostic>,
) {
    for action in actions {
        match action {
            RouteAction::Effect {
                namespace,
                name,
                args,
            } => {
                if namespace == "gosh" {
                    match name.as_str() {
                        "setcode" | "setCurrentCode" => {
                            diags.push(Diagnostic::warning("E06",
                                format!(
                                    "entity '{}' route '{}': gosh::{} — on-chain code upgrade not supported on EVM target",
                                    entity_name, route_name, name)));
                        }
                        _ => {
                            diags.push(Diagnostic::warning(
                                "E02",
                                format!(
                                    "entity '{}' route '{}': gosh::{} has no EVM equivalent",
                                    entity_name, route_name, name
                                ),
                            ));
                        }
                    }
                }
                for a in args {
                    check_evm_compat_expr(a, entity_name, route_name, deterministic, diags);
                }
            }
            RouteAction::Let { pattern, value } => {
                check_evm_compat_expr(value, entity_name, route_name, deterministic, diags);
                // Phase EVM-P0-A (closes EVM_GAPS § 1.12): tuple-let
                // destructure whose RHS shape can't be type-inferred
                // is rejected with E23 — the codegen would otherwise
                // emit `(uint256 a, uint256 b) = some_call();` and
                // silently widen e.g. an `address` head to uint256.
                if let Pattern::Tuple(_) = pattern {
                    if !is_inferable_tuple_rhs(value) {
                        diags.push(Diagnostic::error("E23",
                            format!(
                                "entity '{}' route '{}': `let (a, b, ...) = <expr>` destructure on EVM requires the RHS to be a tuple literal, a known pure-fn returning a tuple, or `divmod(...)`. \
                                 Other shapes silently widen every slot to `uint256`. Bind the call to a single name and project the components separately, e.g. `let r = ...; let a = r.0;`.",
                                entity_name, route_name)));
                    }
                }
            }
            RouteAction::Return { values } => {
                for v in values {
                    check_evm_compat_expr(v, entity_name, route_name, deterministic, diags);
                }
            }
            RouteAction::Conditional {
                condition,
                then_actions,
                else_actions,
            } => {
                check_evm_compat_expr(condition, entity_name, route_name, deterministic, diags);
                let then_refs: Vec<&RouteAction> = then_actions.iter().collect();
                let else_refs: Vec<&RouteAction> = else_actions.iter().collect();
                check_evm_compat_actions(deterministic, &then_refs, entity_name, route_name, diags);
                check_evm_compat_actions(deterministic, &else_refs, entity_name, route_name, diags);
            }
            RouteAction::Send {
                dest,
                args,
                send_options,
                ..
            } => {
                check_evm_compat_expr(dest, entity_name, route_name, deterministic, diags);
                for a in args {
                    check_evm_compat_expr(a, entity_name, route_name, deterministic, diags);
                }
                if let Some(opts) = send_options {
                    check_evm_compat_expr(opts, entity_name, route_name, deterministic, diags);
                }
            }
            RouteAction::Deploy {
                send_options,
                constructor_args,
                ..
            } => {
                if let Some(opts) = send_options {
                    check_evm_compat_expr(opts, entity_name, route_name, deterministic, diags);
                }
                for a in constructor_args {
                    check_evm_compat_expr(a, entity_name, route_name, deterministic, diags);
                }
            }
            RouteAction::Rescue { tag, action } => {
                diags.push(Diagnostic::error(
                    "E26",
                    format!(
                        "entity '{}' route '{}': `rescue {}` is TVM-specific (async bounce recovery). The EVM domain has no analogue and no try/catch lowering. Use --target ackinacki, or remove the rescue (`where`/`throw` still abort the route).",
                        entity_name, route_name, tag
                    ),
                ));
                let inner = [action.as_ref()];
                check_evm_compat_actions(deterministic, &inner, entity_name, route_name, diags);
            }
            _ => {}
        }
    }
}

/// Phase EVM-P0-A (closes EVM_GAPS § 1.12): mirror of the codegen
/// `infer_tuple_elem_types` shape gate — accept only RHS forms whose
/// per-slot Solidity type the codegen can infer. Other shapes get
/// rejected by E23 so users explicitly bind-then-project rather than
/// having every slot silently widen to `uint256`.
fn is_inferable_tuple_rhs(rhs: &Expr) -> bool {
    match rhs {
        Expr::Tuple(_) => true,
        Expr::FnCall(name, _) => {
            // `divmod` is a stdlib helper (`(uint256, uint256)` known
            // statically) and any user-defined pure fn whose return
            // type we can read from the program registry passes.
            // The validator runs before codegen state is set up, so
            // we re-read the program's pure-fn list directly via the
            // `KNOWN_TUPLE_FNS` thread-local set when populated. Today
            // we only need divmod + a structural pure-fn lookup: the
            // pure-fn check is delegated to the compiled-out lookup,
            // and the rest of the recognised set is just `Tuple` /
            // `If` / `Block`. Recognising bare `FnCall` shapes
            // catches over-approximation cases — codegen still falls
            // back to `uint256` per-slot when the registry is empty,
            // but the validator already accepted the call shape.
            name == "divmod" || PROGRAM_PURE_FN_NAMES.with(|c| c.borrow().contains(name))
        }
        Expr::If(_, then, _) => is_inferable_tuple_rhs(then),
        Expr::Block(items) => items.last().map_or(false, is_inferable_tuple_rhs),
        _ => false,
    }
}

/// True if `iter` is an iterator-method chain we cannot lower to Solidity
/// (e.g. `xs.iter()`, `xs.filter(|...| ...).map(|...| ...)`,
/// `xs.iter().fold(init, |...| ...)`).
///
/// The EVM lowering of `for x in iter { body }` only supports two forms:
/// `Range` (e.g. `0..n`) and a direct `Vec<T>`-typed expression. Chains
/// of functional combinators carry closures that cannot be inlined at
/// codegen time without first-class function support, so we keep the
/// `E07` warning for those.
fn is_iterator_method_chain(iter: &Expr) -> bool {
    matches!(
        iter,
        Expr::MethodCall(_, name, _)
            if matches!(
                name.as_str(),
                "iter" | "into_iter" | "map" | "filter" | "filter_map"
                | "fold" | "reduce" | "enumerate" | "zip" | "take"
                | "skip" | "chain" | "flat_map" | "flatten" | "rev"
                | "sum" | "product" | "max" | "min" | "any" | "all"
                | "count" | "position" | "find"
            )
    )
}

/// True when `expr` is a `.fold` over an EVM-12-supported fused chain
/// (`filter` / `map` / `iter` / `values` / `enumerate` / `take` stages),
/// matching `codegen::evm_iter::parse_iter_chain`.
fn is_evm12_fused_fold_chain(expr: &Expr) -> bool {
    let Expr::MethodCall(base, name, args) = expr else {
        return false;
    };
    if name != "fold" || args.len() != 2 || !matches!(&args[1], Expr::Closure(p, _) if p.len() == 2)
    {
        return false;
    }
    let mut cursor = base.as_ref();
    let mut stages = 0usize;
    loop {
        match cursor {
            Expr::MethodCall(b, m, a) if m == "iter" && a.is_empty() => {
                stages += 1;
                cursor = b.as_ref();
            }
            Expr::MethodCall(b, m, a) if m == "values" && a.is_empty() => {
                stages += 1;
                cursor = b.as_ref();
            }
            Expr::MethodCall(b, m, a) if m == "enumerate" && a.is_empty() => {
                stages += 1;
                cursor = b.as_ref();
            }
            Expr::MethodCall(b, m, a)
                if m == "filter"
                    && a.len() == 1
                    && matches!(&a[0], Expr::Closure(p, _) if p.len() == 1) =>
            {
                stages += 1;
                cursor = b.as_ref();
            }
            Expr::MethodCall(b, m, a)
                if m == "map"
                    && a.len() == 1
                    && matches!(&a[0], Expr::Closure(p, _) if p.len() == 1) =>
            {
                stages += 1;
                cursor = b.as_ref();
            }
            Expr::MethodCall(b, m, a) if m == "take" && a.len() == 1 => {
                stages += 1;
                cursor = b.as_ref();
            }
            Expr::MethodCall(_, m, _) if is_unsupported_iter_combinator(m) => {
                return false;
            }
            _ => break,
        }
    }
    stages > 0
}

fn is_unsupported_iter_combinator(name: &str) -> bool {
    matches!(
        name,
        "filter_map"
            | "into_iter"
            | "reduce"
            | "zip"
            | "skip"
            | "chain"
            | "flat_map"
            | "flatten"
            | "rev"
            | "sum"
            | "product"
            | "max"
            | "min"
            | "any"
            | "all"
            | "count"
            | "position"
            | "find"
    )
}

/// Walk an EVM-12-fused chain, checking leaf expressions inside stage
/// closures / `take` args without emitting bare-closure E07 warnings.
fn check_evm12_fused_chain_leaves(
    expr: &Expr,
    entity_name: &str,
    route_name: &str,
    deterministic: bool,
    diags: &mut Vec<Diagnostic>,
) {
    match expr {
        Expr::MethodCall(b, m, args)
            if matches!(m.as_str(), "iter" | "values" | "enumerate") && args.is_empty() =>
        {
            check_evm12_fused_chain_leaves(b, entity_name, route_name, deterministic, diags);
        }
        Expr::MethodCall(b, m, args)
            if (m == "filter" || m == "map")
                && args.len() == 1
                && matches!(&args[0], Expr::Closure(p, _) if p.len() == 1) =>
        {
            check_evm12_fused_chain_leaves(b, entity_name, route_name, deterministic, diags);
            if let Expr::Closure(_, body) = &args[0] {
                check_evm_compat_expr(body, entity_name, route_name, deterministic, diags);
            }
        }
        Expr::MethodCall(b, m, args) if m == "take" && args.len() == 1 => {
            check_evm12_fused_chain_leaves(b, entity_name, route_name, deterministic, diags);
            check_evm_compat_expr(&args[0], entity_name, route_name, deterministic, diags);
        }
        _ => check_evm_compat_expr(expr, entity_name, route_name, deterministic, diags),
    }
}

fn check_evm_compat_expr(
    expr: &Expr,
    entity_name: &str,
    route_name: &str,
    deterministic: bool,
    diags: &mut Vec<Diagnostic>,
) {
    match expr {
        Expr::MsgField(f) if f == "pubkey" => {
            diags.push(Diagnostic::error("E03",
                format!(
                    "entity '{}' route '{}': msg::pubkey is TVM-specific (no EVM equivalent — addresses are derived from secp256k1 keys but the pubkey itself is not exposed). Remove the reference or guard it behind a target check.",
                    entity_name, route_name)));
        }
        Expr::MsgField(f) if f == "currencies" => {
            diags.push(Diagnostic::error("E04",
                format!(
                    "entity '{}' route '{}': msg::currencies is TVM-specific (ECC-7 currency map). EVM has no analogue; use msg::value for the native asset.",
                    entity_name, route_name)));
        }
        Expr::MsgField(f) if f == "body" => {
            diags.push(Diagnostic::error("E13",
                format!(
                    "entity '{}' route '{}': msg::body is TVM-specific (raw inbound cell). On EVM there is no equivalent — calldata is already decoded into typed parameters by the dispatcher.",
                    entity_name, route_name)));
        }
        Expr::SysField(f) if f == "pubkey" || f == "seqno" => {
            diags.push(Diagnostic::error("E14",
                format!(
                    "entity '{}' route '{}': sys::{} is TVM-specific (contract keypair / external-message seqno). EVM contracts have no keypair and no replay-protection seqno.",
                    entity_name, route_name, f)));
        }
        Expr::SysField(f)
            if !matches!(
                f.as_str(),
                "now"
                    | "timestamp"
                    | "address"
                    | "logicaltime"
                    | "block_number"
                    | "rnd_seed"
                    | "prevrandao"
                    | "pubkey"
                    | "balance"
                    | "seqno"
            ) =>
        {
            diags.push(Diagnostic::error("E24",
                format!(
                    "entity '{}' route '{}': unknown system field `sys::{}` is not available on the Container target",
                    entity_name, route_name, f)));
        }
        // Closures-as-values are not lowerable on EVM (Solidity has no
        // first-class functions). The only well-supported loop form is
        // `for x in iter { body }` where the iterator is a `Range` or a
        // `Vec<T>` — that case is handled below and recurses into the
        // body without warning.
        Expr::Closure(_, body) => {
            diags.push(Diagnostic::error("E07",
                format!(
                    "entity '{}' route '{}': closure-as-value not lowerable to Solidity (use `for x in iter {{ ... }}` directly, or keep the algorithm in a TVM-only pure fn)",
                    entity_name, route_name)));
            check_evm_compat_expr(body, entity_name, route_name, deterministic, diags);
        }
        Expr::For(_, iter, body) => {
            // Lowerable shapes: `for x in start..end` (Range) or
            // `for x in <vec-typed expression>`. Iterator-method chains
            // like `xs.iter().filter(...).map(...)` cannot be lowered
            // because Solidity has no first-class functions and we'd have
            // to inline closures — keep warning for those.
            if is_iterator_method_chain(iter) {
                diags.push(Diagnostic::warning("E07",
                    format!(
                        "entity '{}' route '{}': `for` over an iterator-method chain (.iter()/.map()/.filter()/.fold()/...) is not lowerable to Solidity; iterate over a `Vec<T>` or a `start..end` range instead",
                        entity_name, route_name)));
            }
            // Walk into the iterator's sub-expressions, but skip the
            // `Range` wrapper itself — `start..end` is a valid `for`
            // iterator on EVM, only its endpoints need scanning.
            match iter.as_ref() {
                Expr::Range(lo, hi) => {
                    check_evm_compat_expr(lo, entity_name, route_name, deterministic, diags);
                    check_evm_compat_expr(hi, entity_name, route_name, deterministic, diags);
                }
                _ => check_evm_compat_expr(iter, entity_name, route_name, deterministic, diags),
            }
            check_evm_compat_expr(body, entity_name, route_name, deterministic, diags);
        }
        // A bare `Range` outside a `for` (e.g. as a function argument or
        // let-binding) has no Solidity analogue; only the `for x in s..e`
        // form is supported.
        Expr::Range(lo, hi) => {
            diags.push(Diagnostic::error("E07",
                format!(
                    "entity '{}' route '{}': bare `start..end` range is only lowerable as a `for` iterator on EVM",
                    entity_name, route_name)));
            check_evm_compat_expr(lo, entity_name, route_name, deterministic, diags);
            check_evm_compat_expr(hi, entity_name, route_name, deterministic, diags);
        }
        Expr::EnumVariantWithData(en, v, args) => {
            // The grammar collapses `<ns>::<name>(args)` into
            // `EnumVariantWithData` regardless of whether `<ns>` is a
            // user-defined enum or a reserved namespace (`gosh::`,
            // `evm::`). The reserved namespaces are dispatched through
            // their own lowering tables, not as enum variants.
            if en == "gosh" {
                // Phase EVM-2 K1: `gosh::name(args)` in expression
                // position lowers to literal `0` today (silent
                // miscompile), so promote to a hard error. The
                // action-position form (RouteAction::Effect) keeps
                // E02 warning because it lowers to a no-op comment,
                // which is harmless.
                diags.push(Diagnostic::error("E15",
                    format!(
                        "entity '{}' route '{}': `gosh::{}(...)` has no EVM equivalent in expression position. Move the call to action position (e.g. as a top-level effect) or guard it behind a target check.",
                        entity_name, route_name, v)));
            } else if en == "evm" {
                if !crate::codegen::solidity::core::expr::evm_ns_call_supported(v, args.len()) {
                    push_e16_unknown_evm(entity_name, route_name, v, args.len(), diags);
                }
            } else {
                // Phase EVM-4 J1-J3: payload-bearing user enums now
                // lower to a tagged-union struct, so this is no longer
                // a silent miscompile. Keep the legacy E08 warning as
                // an informational hint about the storage-bloat
                // trade-off (one field slot per payload position).
                diags.push(Diagnostic::warning("E08",
                    format!(
                        "entity '{}' route '{}': enum variant {}::{} with data lowers to a tagged-union struct on EVM (one field slot per payload position; see docs/EVM_GAPS.md § 1.31)",
                        entity_name, route_name, en, v)));
            }
            for a in args {
                check_evm_compat_expr(a, entity_name, route_name, deterministic, diags);
            }
        }
        Expr::NamespacedCall {
            namespace, name, args, ..
        } if namespace == "evm"
            && !crate::codegen::solidity::core::expr::evm_ns_call_supported(name, args.len()) =>
        {
            push_e16_unknown_evm(entity_name, route_name, name, args.len(), diags);
            for a in args {
                check_evm_compat_expr(a, entity_name, route_name, deterministic, diags);
            }
        }
        Expr::MsgField(f)
            if crate::codegen::solidity::core::expr::sol_msg_field(f).is_none()
                && !matches!(f.as_str(), "pubkey" | "currencies" | "body") =>
        {
            diags.push(Diagnostic::error(
                "E16",
                format!(
                    "entity '{}' route '{}': `msg::{}` has no EVM lowering",
                    entity_name, route_name, f
                ),
            ));
        }
        Expr::SysField(f)
            if crate::codegen::solidity::core::expr::sol_sys_field(f).is_none()
                && !matches!(f.as_str(), "pubkey" | "seqno") =>
        {
            diags.push(Diagnostic::error(
                "E16",
                format!(
                    "entity '{}' route '{}': `sys::{}` has no EVM lowering",
                    entity_name, route_name, f
                ),
            ));
        }
        Expr::NamespacedCall {
            namespace, name, ..
        } if namespace == "gosh" => {
            // Phase EVM-2 K1: `gosh::*` in expression position lowers
            // to literal `0` (silent miscompile) — promote to error.
            diags.push(Diagnostic::error("E15",
                format!(
                    "entity '{}' route '{}': `gosh::{}(...)` has no EVM equivalent in expression position. Move the call to action position (e.g. as a top-level effect) or guard it behind a target check.",
                    entity_name, route_name, name)));
        }
        Expr::BinOp(l, _, r) => {
            check_evm_compat_expr(l, entity_name, route_name, deterministic, diags);
            check_evm_compat_expr(r, entity_name, route_name, deterministic, diags);
        }
        Expr::UnaryOp(_, e) => {
            check_evm_compat_expr(e, entity_name, route_name, deterministic, diags)
        }
        Expr::FieldAccess(b, _) => {
            check_evm_compat_expr(b, entity_name, route_name, deterministic, diags)
        }
        Expr::Index(b, i) => {
            check_evm_compat_expr(b, entity_name, route_name, deterministic, diags);
            check_evm_compat_expr(i, entity_name, route_name, deterministic, diags);
        }
        // `<iter>.fold(init, |acc, x| body)` lowers to an imperative
        // for-loop with an accumulator on EVM (see
        // `cambrian-transpiler/src/codegen/evm.rs::gen_fold_loop`). The
        // supported iterator shapes mirror `Expr::For`: a `Range`
        // (`start..end`) or a `Vec<T>`-typed expression. Iterator-method
        // chain prefixes that EVM-12 fuses (`xs.filter(...).map(...).fold(...)`)
        // are also lowerable — only unsupported combinators keep E07.
        Expr::MethodCall(b, name, args)
            if name == "fold"
                && args.len() == 2
                && matches!(&args[1], Expr::Closure(p, _) if p.len() == 2) =>
        {
            if is_iterator_method_chain(b) && !is_evm12_fused_fold_chain(expr) {
                diags.push(Diagnostic::warning("E07",
                    format!(
                        "entity '{}' route '{}': `.fold` over an iterator-method chain (.iter()/.map()/.filter()/...) is not lowerable to Solidity; fold over a `Vec<T>` or a `start..end` range instead",
                        entity_name, route_name)));
            }
            // Recurse into init and the closure body, but skip the
            // immediate `Range` wrapper of `b` (only its endpoints need
            // scanning) and skip the closure-as-value warning since the
            // surrounding `.fold` shape is the supported lowering site.
            match b.as_ref() {
                Expr::Range(lo, hi) => {
                    check_evm_compat_expr(lo, entity_name, route_name, deterministic, diags);
                    check_evm_compat_expr(hi, entity_name, route_name, deterministic, diags);
                }
                _ if is_evm12_fused_fold_chain(expr) => {
                    // Walk chain stages without flagging combinator closures
                    // as bare closure-as-value (they are inlined by EVM-12).
                    check_evm12_fused_chain_leaves(
                        b,
                        entity_name,
                        route_name,
                        deterministic,
                        diags,
                    );
                }
                _ => check_evm_compat_expr(b, entity_name, route_name, deterministic, diags),
            }
            check_evm_compat_expr(&args[0], entity_name, route_name, deterministic, diags);
            if let Expr::Closure(_, body) = &args[1] {
                check_evm_compat_expr(body, entity_name, route_name, deterministic, diags);
            }
        }
        Expr::MethodCall(b, method, args) => {
            if let Some(hint) = evm_unlowered_method_hint(method) {
                diags.push(Diagnostic::error(
                    "E28",
                    format!(
                        "entity '{}' route '{}': `.{}()` has no Solidity or Lean lowering; {}",
                        entity_name, route_name, method, hint
                    ),
                ));
            }
            check_evm_compat_expr(b, entity_name, route_name, deterministic, diags);
            for a in args {
                check_evm_compat_expr(a, entity_name, route_name, deterministic, diags);
            }
        }
        Expr::FnCall(_, args) | Expr::MacroRef(_, args) => {
            for a in args {
                check_evm_compat_expr(a, entity_name, route_name, deterministic, diags);
            }
        }
        Expr::If(c, t, e) => {
            check_evm_compat_expr(c, entity_name, route_name, deterministic, diags);
            check_evm_compat_expr(t, entity_name, route_name, deterministic, diags);
            if let Some(e) = e {
                check_evm_compat_expr(e, entity_name, route_name, deterministic, diags);
            }
        }
        Expr::Let(_, val, body) => {
            check_evm_compat_expr(val, entity_name, route_name, deterministic, diags);
            check_evm_compat_expr(body, entity_name, route_name, deterministic, diags);
        }
        Expr::Block(items) => {
            for item in items {
                check_evm_compat_expr(item, entity_name, route_name, deterministic, diags);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                check_evm_compat_expr(v, entity_name, route_name, deterministic, diags);
            }
        }
        Expr::RecordUpdate(b, fields) => {
            check_evm_compat_expr(b, entity_name, route_name, deterministic, diags);
            for (_, v) in fields {
                check_evm_compat_expr(v, entity_name, route_name, deterministic, diags);
            }
        }
        Expr::Tuple(items) | Expr::ArrayLit(items) => {
            for item in items {
                check_evm_compat_expr(item, entity_name, route_name, deterministic, diags);
            }
        }
        Expr::Cast(e, _) | Expr::Some(e) => {
            check_evm_compat_expr(e, entity_name, route_name, deterministic, diags);
        }
        Expr::Match(s, arms) => {
            check_evm_compat_expr(s, entity_name, route_name, deterministic, diags);
            for arm in arms {
                check_evm_compat_expr(&arm.body, entity_name, route_name, deterministic, diags);
            }
        }
        // Phase EVM-2 K1 (E16): expressions that have no defined EVM
        // lowering. Each one previously fell through to `gen_expr =>
        // None` and the hoisted `/* unsupported expr */ 0` sentinel
        // (silent miscompile). Lowering them is a separate phase
        // entirely; for now we reject them at the validator boundary
        // so a `--target evm` build fails fast with a focused error
        // instead of producing arithmetic on `0`.
        Expr::Encode { value, .. } => {
            diags.push(Diagnostic::error(
                "E16",
                format!(
                    "entity '{}' route '{}': `encode<...>(...)` is a test-only intrinsic that has no EVM equivalent in entity / route bodies (it lowers to ABI scratch outside test contexts)",
                    entity_name, route_name,
                ),
            ));
            check_evm_compat_expr(value, entity_name, route_name, deterministic, diags);
        }
        Expr::AddressOf {
            entity_name: ent,
            args,
            with_params,
        } => {
            if !deterministic {
                diags.push(Diagnostic::error(
                    "E16",
                    format!(
                        "entity '{}' route '{}': `address_of {}(..)` requires `deterministic_addresses: true` in project.yaml on the EVM target — outside deterministic mode the address cannot be predicted statically",
                        entity_name, route_name, ent,
                    ),
                ));
            }
            for a in args {
                check_evm_compat_expr(a, entity_name, route_name, deterministic, diags);
            }
            for (_, v) in with_params {
                check_evm_compat_expr(v, entity_name, route_name, deterministic, diags);
            }
        }
        Expr::NamespacedCall {
            namespace,
            name,
            args,
            ..
        } if crate::codegen::stdlib::is_std_namespace(namespace)
            && !crate::codegen::stdlib::is_supported_std_call(namespace, name) =>
        {
            diags.push(Diagnostic::error("E25",
                format!(
                    "entity '{}' route '{}': unsupported standard-library call `{}::{}`; see docs/STDLIB.md for the Phase-1 allowlist",
                    entity_name, route_name, namespace, name)));
            for a in args {
                check_evm_compat_expr(a, entity_name, route_name, deterministic, diags);
            }
        }
        Expr::NamespacedCall {
            namespace,
            name,
            args,
            ..
        } if !crate::codegen::stdlib::is_std_namespace(namespace)
            && namespace != "evm"
            && namespace != "gosh" =>
        {
            diags.push(Diagnostic::error(
                "E16",
                format!(
                    "entity '{}' route '{}': `{}::{}(..)` namespace has no EVM lowering. Use an `evm::` intrinsic or move platform-specific code behind a target check.",
                    entity_name, route_name, namespace, name,
                ),
            ));
            for a in args {
                check_evm_compat_expr(a, entity_name, route_name, deterministic, diags);
            }
        }
        _ => {}
    }
}

/// E28: method names that reach EVM-domain codegen verbatim (`using`
/// rewrites have already turned user methods into calls).
fn evm_unlowered_method_hint(method: &str) -> Option<&'static str> {
    match method {
        "unwrap" | "unwrap_or" | "expect" => {
            Some("destructure the `Option` with `match` or `let some(x) = ...`")
        }
        "is_some" | "is_none" => Some("test the `Option` with `match` (`some(_)` / `none` arms)"),
        "get" => Some("index the collection with `m[k]` (or `m.contains(k)` for membership)"),
        "set" => Some("update a map member with `m.insert(k, v)` in a member transform"),
        _ => None,
    }
}

fn push_e16_unknown_evm(
    entity_name: &str,
    route_name: &str,
    name: &str,
    arity: usize,
    diags: &mut Vec<Diagnostic>,
) {
    diags.push(Diagnostic::error(
        "E16",
        format!(
            "entity '{}' route '{}': `evm::{}` with {} argument(s) is not an EVM intrinsic (supported: ecrecover, keccak256, keccak256Packed, sha256, ripemd160, balance, blockhash)",
            entity_name, route_name, name, arity
        ),
    ));
}

// ---------------------------------------------------------------------------
// Phase EVM-2 K1 (E17) / Phase M PM-006 (L15): HashMap member-transform shape
//
// Solidity `gen_mapping_transform_split` recognises a fixed set of shapes for
// HashMap members: `m.insert/update/remove(...)`, nested versions,
// `if cond { .. } else { .. }`, `block { .. }`, the bare member
// identifier (identity), `EmptyCollection`, and `HashMap::new()`.
// Anything else falls through to a `// mapping <m> transform: no-op`
// comment — silent miscompile. E17 lifts that to a hard error on the
// Solidity core. Lean lowers the same recognised shapes via
// `lean/core/hashmap.rs`; ill-shaped bodies become ill-typed member defs
// (or silently wrong maps), so L15 mirrors E17 on `Language(Lean)`.
// ---------------------------------------------------------------------------

pub(crate) fn is_recognised_hashmap_transform(body: &Expr, member_name: &str) -> bool {
    match body {
        // Insert/update/remove on the member (or any nested member-like
        // base — gen_mapping_transform_split tolerates a let-substituted
        // alias too).
        Expr::MethodCall(_, m, _) if m == "insert" || m == "update" || m == "remove" => true,
        Expr::If(_, then_b, else_b) => {
            is_recognised_hashmap_transform(then_b, member_name)
                && else_b
                    .as_ref()
                    .map_or(true, |e| is_recognised_hashmap_transform(e, member_name))
        }
        Expr::Block(items) => items
            .iter()
            .all(|i| is_recognised_hashmap_transform(i, member_name)),
        Expr::Ident(name) if name == member_name => true,
        Expr::EmptyCollection => true,
        Expr::EnumVariantWithData(en, var, _) if en == "HashMap" && var == "new" => true,
        // `let x = ...; <body>` — codegen substitutes inside the body,
        // so the body's shape is what matters.
        Expr::Let(_, _, body) => is_recognised_hashmap_transform(body, member_name),
        _ => false,
    }
}

fn check_hashmap_member_transforms(
    entity: &Entity,
    code: &'static str,
    backend: &str,
    consequence: &str,
    diags: &mut Vec<Diagnostic>,
) {
    for member in &entity.members {
        if !matches!(&member.ty, Type::Generic(name, _) if name == "HashMap") {
            continue;
        }
        for t in &member.transforms {
            if !is_recognised_hashmap_transform(&t.body, &member.name) {
                diags.push(Diagnostic::error(
                    code,
                    format!(
                        "entity '{}' member '{}' transform `in {}(..)`: HashMap member transform body shape is not recognised by the {} backend ({}). Use `m.insert(k, v)` / `m.update(k, v)` / `m.remove(k)` (optionally wrapped in `if`, `block`, or `let`) instead.",
                        entity.name, member.name, t.route_name, backend, consequence,
                    ),
                ));
            }
        }
    }
}

fn check_evm_compat_member_transforms(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    check_hashmap_member_transforms(entity, "E17", "EVM", "would emit a no-op", diags);
}

fn check_lean_hashmap_member_transforms(entity: &Entity, diags: &mut Vec<Diagnostic>) {
    check_hashmap_member_transforms(
        entity,
        "L15",
        "Lean",
        "would lower to an ill-typed or silently wrong member def",
        diags,
    );
}

// ---------------------------------------------------------------------------
// Phase EVM-2 K2 (E18-E21): type-erasure E-rules
//
// `sol_type` and `sol_type_entity` silently erase several type shapes
// to `bytes` / `uint256` (see `docs/EVM_GAPS.md` § 1.20-1.22, 1.24).
// On the EVM target we now reject those shapes at the validator
// boundary so the user sees a focused error instead of a contract
// that compiles but loses type information.
// ---------------------------------------------------------------------------

const PRIMITIVE_SOL_TYPE_NAMES: &[&str] = &[
    // Cambrian unsigned / signed integer aliases.
    "u8", "u16", "u32", "u64", "u128", "U256", "usize", "i8", "i16", "i32", "i64", "i128",
    // Solidity-native names users sometimes write directly.
    "uint8", "uint16", "uint32", "uint64", "uint128", "uint256", "int8", "int16", "int32", "int64",
    "int128", "int256", "bytes32",
    // `bytes4` is here for one reason: ERC-165 answers on a four-byte
    // interface id, and every other width answers on the wrong ABI selector.
    // The other fixed widths stay out until something needs them — an
    // unused primitive is a lowering path nobody exercises.
    "bytes4", // Common scalars + cell-shaped opaques.
    "bool", "String", "CamData", "bytes", "address", "pubkey",
];

/// Phase EVM-2 K2 (E21): the set of Solidity types `sol_type` knows
/// how to emit a real `T(v)` cast for. Anything else falls through
/// to a silent identity emission.
const ALLOWED_CAST_SOL_TYPES: &[&str] =
    &["uint256", "int256", "bool", "address", "bytes32", "bytes4"];

fn is_known_user_type(name: &str, program: &Program, entity: Option<&Entity>) -> bool {
    if PRIMITIVE_SOL_TYPE_NAMES.contains(&name) {
        return true;
    }
    if program.records.iter().any(|r| r.name == name) {
        return true;
    }
    if program.enums.iter().any(|e| e.name == name) {
        return true;
    }
    if program.type_aliases.iter().any(|a| a.name == name) {
        return true;
    }
    if let Some(entity) = entity {
        if entity.records.iter().any(|r| r.name == name) {
            return true;
        }
        if entity.enums.iter().any(|e| e.name == name) {
            return true;
        }
        if entity.type_aliases.iter().any(|a| a.name == name) {
            return true;
        }
    }
    if program.entities.iter().any(|e| e.name == name) {
        // Entity names are usable as `address(Entity)` analogue contexts;
        // bare `Entity` member type isn't really a Cambrian shape but we
        // keep the door open.
        return true;
    }
    false
}

/// Phase EVM-2 K2: position context for type validation. Tuples are
/// legitimately emitted as Solidity multi-return at route / pure-fn
/// return sites (see `sol_return_types` in `codegen/evm.rs`); rejecting
/// them there would forbid a supported pattern. Every other position
/// (member storage, record / enum field, parameter, cast target, …)
/// silently erases tuples to `bytes`, which is the silent miscompile
/// we're closing out.
#[derive(Clone, Copy, PartialEq)]
enum TypePos {
    /// Multi-return splitting applies here — tuples are OK.
    ReturnType,
    /// Anything else: storage / param / record field / cast target.
    Storage,
}

fn check_evm_compat_type(
    ty: &Type,
    site: &str,
    program: &Program,
    entity: Option<&Entity>,
    diags: &mut Vec<Diagnostic>,
) {
    check_evm_compat_type_pos(ty, site, TypePos::Storage, program, entity, diags);
}

fn check_evm_compat_type_pos(
    ty: &Type,
    site: &str,
    pos: TypePos,
    program: &Program,
    entity: Option<&Entity>,
    diags: &mut Vec<Diagnostic>,
) {
    match ty {
        Type::Tuple(items) => {
            if pos == TypePos::Storage {
                diags.push(Diagnostic::error(
                    "E18",
                    format!(
                        "{}: tuple types are not supported on the EVM target outside of multi-return position (silent erasure to `bytes`). Define a `record` instead so the field types survive in the emitted Solidity struct.",
                        site,
                    ),
                ));
            }
            // Recurse into the tuple's element types (each element
            // still needs to be a valid Solidity type even in a
            // multi-return).
            for it in items {
                check_evm_compat_type_pos(it, site, TypePos::Storage, program, entity, diags);
            }
        }
        Type::Generic(name, params) => match name.as_str() {
            "Vec" | "HashMap" | "Option" => {
                for p in params {
                    check_evm_compat_type_pos(p, site, TypePos::Storage, program, entity, diags);
                }
            }
            _ => {
                diags.push(Diagnostic::error(
                        "E19",
                        format!(
                            "{}: generic type `{}<...>` has no EVM lowering (only `Vec<T>` / `HashMap<K, V>` / `Option<T>` are supported; everything else silently erases to `bytes`).",
                            site, name,
                        ),
                    ));
            }
        },
        Type::Simple(name) => {
            if !is_known_user_type(name, program, entity) {
                diags.push(Diagnostic::error(
                    "E20",
                    format!(
                        "{}: type `{}` does not resolve to a primitive, record, enum, or type alias visible from the EVM target (would silently erase to `uint256`).",
                        site, name,
                    ),
                ));
            }
        }
        Type::TypedAddress(_) => {}
    }
}

fn resolve_alias_for_validate(ty: &Type, program: &Program, entity: Option<&Entity>) -> Type {
    if let Type::Simple(name) = ty {
        if let Some(entity) = entity {
            if let Some(a) = entity.type_aliases.iter().find(|a| a.name == *name) {
                return resolve_alias_for_validate(&a.ty, program, Some(entity));
            }
        }
        if let Some(a) = program.type_aliases.iter().find(|a| a.name == *name) {
            return resolve_alias_for_validate(&a.ty, program, entity);
        }
    }
    ty.clone()
}

fn check_evm_compat_cast_target(
    ty: &Type,
    site: &str,
    program: &Program,
    entity: Option<&Entity>,
    diags: &mut Vec<Diagnostic>,
) {
    let resolved = resolve_alias_for_validate(ty, program, entity);
    let lowered = match &resolved {
        Type::Simple(name) => match name.as_str() {
            "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "U256" | "uint8" | "uint16"
            | "uint32" | "uint64" | "uint128" | "uint256" => "uint256",
            "i8" | "i16" | "i32" | "i64" | "i128" | "int8" | "int16" | "int32" | "int64"
            | "int128" | "int256" => "int256",
            "bool" => "bool",
            "address" => "address",
            "pubkey" | "bytes32" => "bytes32",
            "bytes4" => "bytes4",
            _ => "_other_",
        },
        _ => "_other_",
    };
    if !ALLOWED_CAST_SOL_TYPES.contains(&lowered) {
        diags.push(Diagnostic::error(
            "E21",
            format!(
                "{}: `as {}` cast is not supported on EVM (silent identity drop). Cast targets must be one of: u8/u16/u32/u64/u128/U256, i8/i16/i32/i64/i128, bool, address, pubkey.",
                site,
                pretty_type(ty),
            ),
        ));
    }
    // Recurse into the type itself (catches e.g. `as Tuple(...)`,
    // `as MyUnknown`).
    check_evm_compat_type(ty, site, program, entity, diags);
}

pub(super) fn pretty_type(ty: &Type) -> String {
    match ty {
        Type::Simple(s) => s.clone(),
        Type::Generic(n, ps) => format!(
            "{}<{}>",
            n,
            ps.iter().map(pretty_type).collect::<Vec<_>>().join(", ")
        ),
        Type::Tuple(ts) => format!(
            "({})",
            ts.iter().map(pretty_type).collect::<Vec<_>>().join(", ")
        ),
        Type::TypedAddress(n) => format!("address({})", n),
    }
}

fn check_evm_compat_expr_types(
    expr: &Expr,
    site: &str,
    program: &Program,
    entity: Option<&Entity>,
    diags: &mut Vec<Diagnostic>,
) {
    match expr {
        Expr::Cast(inner, ty) => {
            check_evm_compat_cast_target(ty, site, program, entity, diags);
            check_evm_compat_expr_types(inner, site, program, entity, diags);
        }
        Expr::BinOp(l, _, r) => {
            check_evm_compat_expr_types(l, site, program, entity, diags);
            check_evm_compat_expr_types(r, site, program, entity, diags);
        }
        Expr::UnaryOp(_, e) => check_evm_compat_expr_types(e, site, program, entity, diags),
        Expr::FieldAccess(b, _) => check_evm_compat_expr_types(b, site, program, entity, diags),
        Expr::Index(b, k) => {
            check_evm_compat_expr_types(b, site, program, entity, diags);
            check_evm_compat_expr_types(k, site, program, entity, diags);
        }
        Expr::MethodCall(b, _, args) => {
            check_evm_compat_expr_types(b, site, program, entity, diags);
            for a in args {
                check_evm_compat_expr_types(a, site, program, entity, diags);
            }
        }
        Expr::FnCall(_, args) | Expr::MacroRef(_, args) => {
            for a in args {
                check_evm_compat_expr_types(a, site, program, entity, diags);
            }
        }
        Expr::NamespacedCall {
            args, type_params, ..
        } => {
            for a in args {
                check_evm_compat_expr_types(a, site, program, entity, diags);
            }
            for t in type_params {
                check_evm_compat_type(t, site, program, entity, diags);
            }
        }
        Expr::EnumVariantWithData(_, _, args) => {
            for a in args {
                check_evm_compat_expr_types(a, site, program, entity, diags);
            }
        }
        Expr::If(c, t, e) => {
            check_evm_compat_expr_types(c, site, program, entity, diags);
            check_evm_compat_expr_types(t, site, program, entity, diags);
            if let Some(e) = e {
                check_evm_compat_expr_types(e, site, program, entity, diags);
            }
        }
        Expr::Let(_, v, b) => {
            check_evm_compat_expr_types(v, site, program, entity, diags);
            check_evm_compat_expr_types(b, site, program, entity, diags);
        }
        Expr::Match(s, arms) => {
            check_evm_compat_expr_types(s, site, program, entity, diags);
            for a in arms {
                check_evm_compat_expr_types(&a.body, site, program, entity, diags);
            }
        }
        Expr::Block(items) | Expr::ArrayLit(items) | Expr::Tuple(items) => {
            for i in items {
                check_evm_compat_expr_types(i, site, program, entity, diags);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, e) in fields {
                check_evm_compat_expr_types(e, site, program, entity, diags);
            }
        }
        Expr::RecordUpdate(b, fields) => {
            check_evm_compat_expr_types(b, site, program, entity, diags);
            for (_, e) in fields {
                check_evm_compat_expr_types(e, site, program, entity, diags);
            }
        }
        Expr::Some(e) | Expr::Closure(_, e) => {
            check_evm_compat_expr_types(e, site, program, entity, diags);
        }
        Expr::Range(s, e) => {
            check_evm_compat_expr_types(s, site, program, entity, diags);
            check_evm_compat_expr_types(e, site, program, entity, diags);
        }
        Expr::For(_, it, body) => {
            check_evm_compat_expr_types(it, site, program, entity, diags);
            check_evm_compat_expr_types(body, site, program, entity, diags);
        }
        Expr::AddressOf {
            args, with_params, ..
        } => {
            for a in args {
                check_evm_compat_expr_types(a, site, program, entity, diags);
            }
            for (_, v) in with_params {
                check_evm_compat_expr_types(v, site, program, entity, diags);
            }
        }
        Expr::Encode { value, target_type } => {
            check_evm_compat_type(target_type, site, program, entity, diags);
            check_evm_compat_expr_types(value, site, program, entity, diags);
        }
        _ => {}
    }
}

fn check_evm_compat_action_types(
    action: &RouteAction,
    site: &str,
    program: &Program,
    entity: Option<&Entity>,
    diags: &mut Vec<Diagnostic>,
) {
    match action {
        RouteAction::Send {
            args,
            dest,
            send_options,
            ..
        } => {
            for a in args {
                check_evm_compat_expr_types(a, site, program, entity, diags);
            }
            check_evm_compat_expr_types(dest, site, program, entity, diags);
            if let Some(o) = send_options {
                check_evm_compat_expr_types(o, site, program, entity, diags);
            }
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            check_evm_compat_expr_types(condition, site, program, entity, diags);
            for a in then_actions {
                check_evm_compat_action_types(a, site, program, entity, diags);
            }
            for a in else_actions {
                check_evm_compat_action_types(a, site, program, entity, diags);
            }
        }
        RouteAction::Return { values } => {
            for v in values {
                check_evm_compat_expr_types(v, site, program, entity, diags);
            }
        }
        RouteAction::Let { value, .. } => {
            check_evm_compat_expr_types(value, site, program, entity, diags);
        }
        RouteAction::Effect { args, .. } => {
            for a in args {
                check_evm_compat_expr_types(a, site, program, entity, diags);
            }
        }
        RouteAction::Deploy {
            send_options,
            constructor_args,
            ..
        } => {
            if let Some(o) = send_options {
                check_evm_compat_expr_types(o, site, program, entity, diags);
            }
            for a in constructor_args {
                check_evm_compat_expr_types(a, site, program, entity, diags);
            }
        }
        RouteAction::Rescue { action, .. } => {
            check_evm_compat_action_types(action, site, program, entity, diags);
        }
        RouteAction::CallRoute { args, .. } => {
            for a in args {
                check_evm_compat_expr_types(a, site, program, entity, diags);
            }
        }
        RouteAction::VarCall {
            args,
            dest,
            send_options,
            ..
        } => {
            for a in args {
                check_evm_compat_expr_types(a, site, program, entity, diags);
            }
            check_evm_compat_expr_types(dest, site, program, entity, diags);
            if let Some(o) = send_options {
                check_evm_compat_expr_types(o, site, program, entity, diags);
            }
        }
        RouteAction::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            for a in update_args {
                check_evm_compat_expr_types(a, site, program, entity, diags);
            }
            for a in callback_args {
                check_evm_compat_expr_types(a, site, program, entity, diags);
            }
        }
        RouteAction::For { iter, body, .. } => {
            check_evm_compat_expr_types(iter, site, program, entity, diags);
            for a in body {
                check_evm_compat_action_types(a, site, program, entity, diags);
            }
        }
        RouteAction::Throw { .. } => {}
        RouteAction::ThrowCustom { args, .. } => {
            for a in args {
                check_evm_compat_expr_types(a, site, program, entity, diags);
            }
        }
        RouteAction::Emit { args, .. } => {
            for a in args {
                check_evm_compat_expr_types(a, site, program, entity, diags);
            }
        }
    }
}

fn check_evm_compat_program_types(program: &Program, diags: &mut Vec<Diagnostic>) {
    // Program-level
    for rec in &program.records {
        for f in &rec.fields {
            let site = format!("record '{}' field '{}'", rec.name, f.name);
            check_evm_compat_type(&f.ty, &site, program, None, diags);
        }
    }
    for e in &program.enums {
        for v in &e.variants {
            for (i, fty) in v.fields.iter().enumerate() {
                let site = format!("enum '{}' variant '{}' position {}", e.name, v.name, i);
                check_evm_compat_type(fty, &site, program, None, diags);
            }
        }
    }
    for ta in &program.type_aliases {
        let site = format!("type alias '{}'", ta.name);
        check_evm_compat_type(&ta.ty, &site, program, None, diags);
    }
    for pf in &program.pure_fns {
        for p in &pf.params {
            let site = format!("pure fn '{}' param '{}'", pf.name, p.name);
            check_evm_compat_type(&p.ty, &site, program, None, diags);
        }
        let site = format!("pure fn '{}' return type", pf.name);
        check_evm_compat_type_pos(
            &pf.return_type,
            &site,
            TypePos::ReturnType,
            program,
            None,
            diags,
        );
        let site = format!("pure fn '{}' body", pf.name);
        check_evm_compat_expr_types(&pf.body, &site, program, None, diags);
    }
    // Entity-level
    for entity in &program.entities {
        for rec in &entity.records {
            for f in &rec.fields {
                let site = format!(
                    "entity '{}' record '{}' field '{}'",
                    entity.name, rec.name, f.name
                );
                check_evm_compat_type(&f.ty, &site, program, Some(entity), diags);
            }
        }
        for e in &entity.enums {
            for v in &e.variants {
                for (i, fty) in v.fields.iter().enumerate() {
                    let site = format!(
                        "entity '{}' enum '{}' variant '{}' position {}",
                        entity.name, e.name, v.name, i
                    );
                    check_evm_compat_type(fty, &site, program, Some(entity), diags);
                }
            }
        }
        for ta in &entity.type_aliases {
            let site = format!("entity '{}' type alias '{}'", entity.name, ta.name);
            check_evm_compat_type(&ta.ty, &site, program, Some(entity), diags);
        }
        for member in &entity.members {
            let site = format!("entity '{}' member '{}'", entity.name, member.name);
            check_evm_compat_type(&member.ty, &site, program, Some(entity), diags);
            if let Some(default) = &member.default_value {
                check_evm_compat_expr_types(default, &site, program, Some(entity), diags);
            }
            for t in &member.transforms {
                let site = format!(
                    "entity '{}' member '{}' transform `in {}(..)`",
                    entity.name, member.name, t.route_name
                );
                check_evm_compat_expr_types(&t.body, &site, program, Some(entity), diags);
            }
        }
        for route in &entity.routes {
            for p in &route.params {
                let site = format!(
                    "entity '{}' route '{}' param '{}'",
                    entity.name, route.name, p.name
                );
                check_evm_compat_type(&p.ty, &site, program, Some(entity), diags);
            }
            if let Some(rt) = &route.return_type {
                let site = format!(
                    "entity '{}' route '{}' return type",
                    entity.name, route.name
                );
                check_evm_compat_type_pos(
                    rt,
                    &site,
                    TypePos::ReturnType,
                    program,
                    Some(entity),
                    diags,
                );
            }
            let site = format!("entity '{}' route '{}' body", entity.name, route.name);
            for action in route.body.all_actions() {
                check_evm_compat_action_types(action, &site, program, Some(entity), diags);
            }
            for w in &route.where_clauses {
                check_evm_compat_expr_types(&w.condition, &site, program, Some(entity), diags);
            }
        }
        for mac in &entity.macros {
            for p in &mac.params {
                let site = format!(
                    "entity '{}' macro '{}' param '{}'",
                    entity.name, mac.name, p.name
                );
                check_evm_compat_type(&p.ty, &site, program, Some(entity), diags);
            }
            let site = format!("entity '{}' macro '{}' return type", entity.name, mac.name);
            check_evm_compat_type_pos(
                &mac.return_type,
                &site,
                TypePos::ReturnType,
                program,
                Some(entity),
                diags,
            );
            let site = format!("entity '{}' macro '{}' body", entity.name, mac.name);
            check_evm_compat_expr_types(&mac.body, &site, program, Some(entity), diags);
        }
    }
}

// ---------------------------------------------------------------------------
// Phase EVM-P0-C: events / emit validation
//   V34 — `emit Name(args)` references an undeclared event.
//   V35 — `emit Name(args)` arity / type mismatch with the declaration.
//   V36 — at most three `indexed` parameters per event (non-anonymous limit).
// ---------------------------------------------------------------------------

pub(super) fn check_events(program: &Program, diags: &mut Vec<Diagnostic>) {
    // V36: indexed-arity limit on every declared event (program- or
    // entity-scope).
    for ev in &program.events {
        check_event_indexed_count(ev, "<program>", diags);
    }
    for entity in &program.entities {
        for ev in &entity.events {
            check_event_indexed_count(ev, &entity.name, diags);
        }
    }

    // V34/V35: every `emit Name(args)` action must resolve to a declared
    // event with matching arity and (best-effort) parameter types.
    for entity in &program.entities {
        for route in &entity.routes {
            for action in route.body.all_actions() {
                check_emit_action_recursive(action, entity, program, diags);
            }
        }
    }
}

fn check_event_indexed_count(ev: &crate::ast::EventDecl, scope: &str, diags: &mut Vec<Diagnostic>) {
    let n_indexed = ev.params.iter().filter(|p| p.indexed).count();
    if n_indexed > 3 {
        diags.push(Diagnostic::error(
            "V36",
            format!(
                "event '{}' in {} has {} indexed parameters; Solidity allows at most 3 (non-anonymous events)",
                ev.name, scope, n_indexed
            ),
        ));
    }
}

fn lookup_event<'a>(
    name: &str,
    entity: &'a Entity,
    program: &'a Program,
) -> Option<&'a crate::ast::EventDecl> {
    entity
        .events
        .iter()
        .find(|e| e.name == name)
        .or_else(|| program.events.iter().find(|e| e.name == name))
}

fn check_emit_action_recursive(
    action: &RouteAction,
    entity: &Entity,
    program: &Program,
    diags: &mut Vec<Diagnostic>,
) {
    match action {
        RouteAction::Emit { event_name, args } => match lookup_event(event_name, entity, program) {
            None => diags.push(Diagnostic::error(
                "V34",
                format!(
                    "entity '{}': emit references undeclared event '{}'",
                    entity.name, event_name
                ),
            )),
            Some(decl) => {
                if decl.params.len() != args.len() {
                    diags.push(Diagnostic::error(
                            "V35",
                            format!(
                                "entity '{}': emit '{}' arity mismatch — declaration takes {} arg(s), got {}",
                                entity.name,
                                event_name,
                                decl.params.len(),
                                args.len()
                            ),
                        ));
                }
            }
        },
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            for a in then_actions {
                check_emit_action_recursive(a, entity, program, diags);
            }
            for a in else_actions {
                check_emit_action_recursive(a, entity, program, diags);
            }
        }
        RouteAction::For { body, .. } => {
            for a in body {
                check_emit_action_recursive(a, entity, program, diags);
            }
        }
        RouteAction::Rescue { action, .. } => {
            check_emit_action_recursive(action, entity, program, diags);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Phase EVM-P0-D: custom errors / throw validation
//   V38 — `throw Name(args)` references an undeclared error.
//   V39 — `throw Name(args)` arity mismatch with the declaration.
// ---------------------------------------------------------------------------

pub(super) fn check_errors(program: &Program, diags: &mut Vec<Diagnostic>) {
    for entity in &program.entities {
        // Action-level `throw CustomErr(args)`.
        for route in &entity.routes {
            for action in route.body.all_actions() {
                check_throw_custom_recursive(action, entity, program, diags);
            }
            // `where` clause `: throw CustomErr(args)`.
            for wc in &route.where_clauses {
                if let Some(name) = &wc.error_name {
                    check_error_ref(name, &wc.error_args, entity, program, diags);
                }
            }
            // `from` clause `: throw CustomErr(args)`.
            for fc in &route.from_clauses {
                if let Some(name) = &fc.error_name {
                    check_error_ref(name, &fc.error_args, entity, program, diags);
                }
            }
            // Phased per-phase where clauses.
            if let Some(phases) = route.body.phases() {
                for phase in phases {
                    for wc in &phase.where_clauses {
                        if let Some(name) = &wc.error_name {
                            check_error_ref(name, &wc.error_args, entity, program, diags);
                        }
                    }
                }
            }
        }
    }
}

fn lookup_error<'a>(
    name: &str,
    entity: &'a Entity,
    program: &'a Program,
) -> Option<&'a crate::ast::ErrorDecl> {
    entity
        .errors
        .iter()
        .find(|e| e.name == name)
        .or_else(|| program.errors.iter().find(|e| e.name == name))
}

fn check_error_ref(
    name: &str,
    args: &[Expr],
    entity: &Entity,
    program: &Program,
    diags: &mut Vec<Diagnostic>,
) {
    match lookup_error(name, entity, program) {
        None => diags.push(Diagnostic::error(
            "V38",
            format!(
                "entity '{}': throw references undeclared error '{}'",
                entity.name, name
            ),
        )),
        Some(decl) => {
            if decl.params.len() != args.len() {
                diags.push(Diagnostic::error(
                    "V39",
                    format!(
                        "entity '{}': throw '{}' arity mismatch — declaration takes {} arg(s), got {}",
                        entity.name,
                        name,
                        decl.params.len(),
                        args.len()
                    ),
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Phase EVM-P0-E: `receive` / `fallback` route validation
//   V40 — `receive` / `fallback` route shape (no params; `receive` must be `accept`).
//   V41 — duplicate `receive` or `fallback` per entity.
// ---------------------------------------------------------------------------

pub(super) fn check_receive_fallback(program: &Program, diags: &mut Vec<Diagnostic>) {
    for entity in &program.entities {
        let mut receive_count = 0usize;
        let mut fallback_count = 0usize;
        for route in &entity.routes {
            if route.name != "receive" && route.name != "fallback" {
                continue;
            }
            if !route.params.is_empty() {
                diags.push(Diagnostic::error(
                    "V40",
                    format!(
                        "entity '{}': '{}' route must take no parameters",
                        entity.name, route.name
                    ),
                ));
            }
            if route.return_type.is_some() {
                diags.push(Diagnostic::error(
                    "V40",
                    format!(
                        "entity '{}': '{}' route must not declare a return type",
                        entity.name, route.name
                    ),
                ));
            }
            if route.is_view || route.is_pure {
                diags.push(Diagnostic::error(
                    "V40",
                    format!(
                        "entity '{}': '{}' route cannot be view or pure",
                        entity.name, route.name
                    ),
                ));
            }
            if route.name == "receive" {
                if !route.is_accept {
                    diags.push(Diagnostic::error(
                        "V40",
                        format!(
                            "entity '{}': 'receive' route must be declared with the `accept` modifier",
                            entity.name
                        ),
                    ));
                }
                receive_count += 1;
            } else {
                fallback_count += 1;
            }
        }
        if receive_count > 1 {
            diags.push(Diagnostic::error(
                "V41",
                format!(
                    "entity '{}': duplicate 'receive' route ({} declared); only one allowed",
                    entity.name, receive_count
                ),
            ));
        }
        if fallback_count > 1 {
            diags.push(Diagnostic::error(
                "V41",
                format!(
                    "entity '{}': duplicate 'fallback' route ({} declared); only one allowed",
                    entity.name, fallback_count
                ),
            ));
        }
    }
}

fn check_throw_custom_recursive(
    action: &RouteAction,
    entity: &Entity,
    program: &Program,
    diags: &mut Vec<Diagnostic>,
) {
    match action {
        RouteAction::ThrowCustom { name, args } => {
            check_error_ref(name, args, entity, program, diags);
        }
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            for a in then_actions {
                check_throw_custom_recursive(a, entity, program, diags);
            }
            for a in else_actions {
                check_throw_custom_recursive(a, entity, program, diags);
            }
        }
        RouteAction::For { body, .. } => {
            for a in body {
                check_throw_custom_recursive(a, entity, program, diags);
            }
        }
        RouteAction::Rescue { action, .. } => {
            check_throw_custom_recursive(action, entity, program, diags);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Lean-language compatibility checks (L-rules)
//
// Apply when `Target::language()` is Lean. L1–L8 are primarily language /
// expressivity constraints. L9–L11 encode **evm × lean pair semantics**
// (atomic World-step model of EVM calls). `rescue`/`recover` is **E26**
// (Acki Nacki bounce only) — they would need revisiting for a future
// non-EVM Lean domain adapter.
// ---------------------------------------------------------------------------

/// Lean language validator hook (P1.7 + P2.6).
///
/// Emits the `L*` diagnostic family for shapes the Lean backend can't
/// lower yet:
///
/// * `L1` — unsupported generics beyond `Vec`/`HashMap`/`Option` in a
///   member type, route param, route return, record field, top-level
///   pure-fn signature, constant, or type-alias target.
/// * `L14` — bare type identifier that resolves to no primitive,
///   record, enum, or alias visible on the Lean core (would emit an
///   opaque identifier and only fail at `lake build`).
/// * `L2` — `for`-loop / `fold` / iterator-method-chain in entity or
///   pure-fn bodies. Lifted to P4 once we have a story for bounded
///   iteration that plays nicely with proofs.
/// * `L3` — `extern entity` declarations. P4 will axiomatise the
///   foreign surface; for P1 we hard-reject so generated specs only
///   cover code we control.
/// * `L4` — multi-entity invariants (`for { v: V, t: T } …`).
///   Warning, not error: emitting a TODO comment is fine because
///   the generated entity files are independent.
/// * `L5` — `expect throw N` against a route whose Lean lowering is
///   total (no `where` / `throw`). The theorem would be vacuous
///   because the route can't fail, so we hard-reject in P2.
/// * `L6` — `expect return …` against a route that doesn't declare
///   a `return_type`. The lowered call has no value to bind, so the
///   assertion has no Lean meaning.
/// * `L7` — `skip from` on a `test` / `fuzz` / `invariant`. Currently
///   not honoured by the Lean backend (`from`-clauses still gate
///   route bodies). Warning so users know to either drop `skip from`
///   or wait for a future phase.
/// * `L15` — HashMap member-transform body shape not recognised by the
///   Lean map lowering (mirror of Solidity `E17`).
pub fn check_lean_target_compat(program: &Program) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    // L1 / L14 — unsupported generics and unknown simple types.
    for entity in &program.entities {
        for m in &entity.members {
            check_lean_type(
                &m.ty,
                &format!("entity '{}' member '{}'", entity.name, m.name),
                program,
                Some(entity),
                &mut diags,
            );
        }
        for route in &entity.routes {
            for p in &route.params {
                check_lean_type(
                    &p.ty,
                    &format!(
                        "entity '{}' route '{}' parameter '{}'",
                        entity.name, route.name, p.name
                    ),
                    program,
                    Some(entity),
                    &mut diags,
                );
            }
            if let Some(rt) = &route.return_type {
                check_lean_type(
                    rt,
                    &format!(
                        "entity '{}' route '{}' return type",
                        entity.name, route.name
                    ),
                    program,
                    Some(entity),
                    &mut diags,
                );
            }
        }
        for rec in &entity.records {
            for f in &rec.fields {
                check_lean_type(
                    &f.ty,
                    &format!(
                        "entity '{}' record '{}' field '{}'",
                        entity.name, rec.name, f.name
                    ),
                    program,
                    Some(entity),
                    &mut diags,
                );
            }
        }
        for c in &entity.constants {
            check_lean_type(
                &c.ty,
                &format!("entity '{}' constant '{}'", entity.name, c.name),
                program,
                Some(entity),
                &mut diags,
            );
        }
        for ta in &entity.type_aliases {
            check_lean_type(
                &ta.ty,
                &format!("entity '{}' type alias '{}'", entity.name, ta.name),
                program,
                Some(entity),
                &mut diags,
            );
        }
    }
    for rec in &program.records {
        for f in &rec.fields {
            check_lean_type(
                &f.ty,
                &format!("top-level record '{}' field '{}'", rec.name, f.name),
                program,
                None,
                &mut diags,
            );
        }
    }
    for ta in &program.type_aliases {
        check_lean_type(
            &ta.ty,
            &format!("top-level type alias '{}'", ta.name),
            program,
            None,
            &mut diags,
        );
    }
    for pf in &program.pure_fns {
        for p in &pf.params {
            check_lean_type(
                &p.ty,
                &format!("pure fn '{}' parameter '{}'", pf.name, p.name),
                program,
                None,
                &mut diags,
            );
        }
        check_lean_type(
            &pf.return_type,
            &format!("pure fn '{}' return type", pf.name),
            program,
            None,
            &mut diags,
        );
    }

    // L15 — HashMap member-transform shapes (Lean mirror of E17).
    for entity in &program.entities {
        check_lean_hashmap_member_transforms(entity, &mut diags);
    }

    // L2 — loops / iterator chains in entity / pure-fn bodies.
    for entity in &program.entities {
        for route in &entity.routes {
            for action in route.body.all_actions() {
                check_lean_no_loops_in_action(
                    action,
                    &format!("entity '{}' route '{}'", entity.name, route.name),
                    &mut diags,
                );
            }
            for wc in &route.where_clauses {
                check_lean_no_loops_in_expr(
                    &wc.condition,
                    &format!("entity '{}' route '{}' where", entity.name, route.name),
                    &mut diags,
                );
            }
        }
        for member in &entity.members {
            for tr in &member.transforms {
                check_lean_no_loops_in_expr(
                    &tr.body,
                    &format!(
                        "entity '{}' member '{}' transform for route '{}'",
                        entity.name, member.name, tr.route_name,
                    ),
                    &mut diags,
                );
            }
        }
        for c in &entity.constants {
            check_lean_no_loops_in_expr(
                &c.value,
                &format!("entity '{}' constant '{}'", entity.name, c.name),
                &mut diags,
            );
        }
    }
    for pf in &program.pure_fns {
        check_lean_no_loops_in_expr(&pf.body, &format!("pure fn '{}'", pf.name), &mut diags);
    }

    // L3 / L4 lifted in P4b — extern entities and multi-entity
    // invariants are lowered by the Lean backend.

    // L5 / L6 — sanity-check `expect throw` / `expect return` against
    // the most recently invoked route inside each test / fuzz /
    // invariant action body. We walk steps in order so each
    // assertion is paired with the correct call.
    for t in &program.tests {
        let where_ = format!("test '{}'", t.name);
        check_lean_expect_targets(program, &t.entity_name, &t.body, &where_, &mut diags);
    }
    for p in &program.properties {
        let where_ = format!("property '{}'", p.name);
        check_lean_expect_targets(program, &p.entity_name, &p.body, &where_, &mut diags);
    }
    for inv in &program.invariants {
        for action in &inv.actions {
            let entity_name = inv
                .instances
                .iter()
                .find(|i| i.name == action.instance)
                .map(|i| i.entity.clone())
                .unwrap_or_default();
            let where_ = format!(
                "invariant '{}' action '{}.{}'",
                inv.name, action.instance, action.route,
            );
            check_lean_expect_targets(program, &entity_name, &action.body, &where_, &mut diags);
        }
    }

    // L7 — `skip from` is not yet honoured on Lean. Warn for every
    // declaration that uses it. Spec generators also emit a TODO
    // comment so the warning has a paper trail in the output.
    for t in &program.tests {
        if t.skip_from {
            diags.push(Diagnostic::warning(
                "L7",
                format!(
                    "test '{}': 'skip from' is not yet honoured by the Lean target — `from`-checks remain in place",
                    t.name,
                ),
            ));
        }
    }
    for p in &program.properties {
        for inst in &p.instances {
            if inst.skip_from {
                let inst_label = inst.name.clone().unwrap_or_else(|| "<unnamed>".to_string());
                diags.push(Diagnostic::warning(
                    "L7",
                    format!(
                        "property '{}' instance '{}': 'skip from' is not yet honoured by the Lean target — `from`-checks remain in place",
                        p.name, inst_label,
                    ),
                ));
            }
        }
    }
    for inv in &program.invariants {
        if inv.skip_from {
            diags.push(Diagnostic::warning(
                "L7",
                format!(
                    "invariant '{}': 'skip from' is not yet honoured by the Lean target — `from`-checks remain in place",
                    inv.name,
                ),
            ));
        }
    }

    // L8 / L9 — typed-send target resolution + failing-route capture
    // safety. Both walk every entity's route bodies (incl. phased and
    // for-bodies) since the Lean codegen reaches into all of them.
    // L10 — flag invariants whose action routes contain `~>` so users
    // know the abstract atomic model glosses over re-entrancy.
    for entity in &program.entities {
        for route in &entity.routes {
            match &route.body {
                RouteBody::Unphased(actions) => {
                    check_lean_send_actions(
                        program,
                        entity,
                        route,
                        actions,
                        false,
                        &HashMap::new(),
                        &mut diags,
                    );
                }
                RouteBody::Phased(phases) => {
                    for phase in phases {
                        check_lean_send_actions(
                            program,
                            entity,
                            route,
                            &phase.actions,
                            false,
                            &HashMap::new(),
                            &mut diags,
                        );
                    }
                }
                RouteBody::Mixed(phases, trailing) => {
                    for phase in phases {
                        check_lean_send_actions(
                            program,
                            entity,
                            route,
                            &phase.actions,
                            false,
                            &HashMap::new(),
                            &mut diags,
                        );
                    }
                    check_lean_send_actions(
                        program,
                        entity,
                        route,
                        trailing,
                        false,
                        &HashMap::new(),
                        &mut diags,
                    );
                }
            }
        }
    }
    for inv in &program.invariants {
        check_lean_invariant_sends(program, inv, &mut diags);
    }

    // L13 — `std::crypto` (and future unsupported `std::` modules) have
    // no Lean lowering; reject at validate time instead of emitting
    // `Cambrian.Unsupported` in generated specs.
    for entity in &program.entities {
        for route in &entity.routes {
            for action in route.body.all_actions() {
                check_lean_std_compat_action(
                    program,
                    action,
                    &format!("entity '{}' route '{}'", entity.name, route.name),
                    &mut diags,
                );
            }
            for wc in &route.where_clauses {
                check_lean_std_compat_expr(
                    program,
                    &wc.condition,
                    &format!("entity '{}' route '{}' where", entity.name, route.name),
                    &mut diags,
                );
            }
        }
        for member in &entity.members {
            for tr in &member.transforms {
                check_lean_std_compat_expr(
                    program,
                    &tr.body,
                    &format!(
                        "entity '{}' member '{}' transform for route '{}'",
                        entity.name, member.name, tr.route_name,
                    ),
                    &mut diags,
                );
            }
            if let Some(d) = &member.default_value {
                check_lean_std_compat_expr(
                    program,
                    d,
                    &format!("entity '{}' member '{}' default", entity.name, member.name),
                    &mut diags,
                );
            }
        }
        for c in &entity.constants {
            check_lean_std_compat_expr(
                program,
                &c.value,
                &format!("entity '{}' constant '{}'", entity.name, c.name),
                &mut diags,
            );
        }
        for mac in &entity.macros {
            check_lean_std_compat_expr(
                program,
                &mac.body,
                &format!("entity '{}' macro '{}'", entity.name, mac.name),
                &mut diags,
            );
        }
    }
    for pf in &program.pure_fns {
        check_lean_std_compat_expr(
            program,
            &pf.body,
            &format!("pure fn '{}'", pf.name),
            &mut diags,
        );
    }
    for lib in &program.libraries {
        for pf in &lib.pure_fns {
            check_lean_std_compat_expr(
                program,
                &pf.body,
                &format!("library '{}' pure fn '{}'", lib.name, pf.name),
                &mut diags,
            );
        }
    }

    diags
}

/// Mirrors `LeanDomain::msg_field`. TVM-only fields are reported by E03 /
/// E04 / E13 instead.
fn lean_msg_field_supported(field: &str) -> bool {
    matches!(
        field,
        "sender" | "value" | "timestamp" | "pubkey" | "currencies" | "body"
    )
}

/// Mirrors `LeanDomain::sys_field`. TVM-only fields are reported by E14.
fn lean_sys_field_supported(field: &str) -> bool {
    matches!(
        field,
        "balance"
            | "address"
            | "now"
            | "timestamp"
            | "chainid"
            | "chainId"
            | "blockNumber"
            | "block_number"
            | "number"
            | "pubkey"
            | "seqno"
    )
}

fn check_lean_std_compat_expr(
    program: &Program,
    expr: &Expr,
    where_: &str,
    diags: &mut Vec<Diagnostic>,
) {
    match expr {
        // The grammar parses single-level `evm::f(args)` as an enum variant.
        Expr::EnumVariantWithData(ns, name, args)
            if ns == "evm"
                && !crate::codegen::lean::expr::lean_namespaced_call_supported(
                    ns,
                    name,
                    args.len(),
                ) =>
        {
            diags.push(Diagnostic::error(
                "L16",
                format!(
                    "{}: `evm::{}` with {} argument(s) has no Lean lowering",
                    where_,
                    name,
                    args.len(),
                ),
            ));
            for a in args {
                check_lean_std_compat_expr(program, a, where_, diags);
            }
        }
        Expr::MsgField(f) if !lean_msg_field_supported(f) => {
            diags.push(Diagnostic::error(
                "L16",
                format!("{}: `msg::{}` is not modelled on the Lean target", where_, f),
            ));
        }
        Expr::SysField(f) if !lean_sys_field_supported(f) => {
            diags.push(Diagnostic::error(
                "L16",
                format!("{}: `sys::{}` is not modelled on the Lean target", where_, f),
            ));
        }
        Expr::NamespacedCall {
            namespace,
            name,
            args,
            ..
        } => {
            if namespace == "std::crypto" {
                diags.push(Diagnostic::error(
                    "L13",
                    format!(
                        "{}: `std::crypto::{}` has no Lean lowering (guard behind a target check or use `evm::` on EVM-only paths)",
                        where_, name,
                    ),
                ));
            } else if !crate::codegen::lean::expr::lean_namespaced_call_supported(
                namespace,
                name,
                args.len(),
            ) && !crate::codegen::lean::expr::lean_library_pure_call_supported(
                program,
                namespace,
                name,
                args.len(),
            ) {
                // UPSTREAM B-33: fail loud at validate instead of emitting
                // `Cambrian.Unsupported` (`opaque Type`) into a value slot.
                diags.push(Diagnostic::error(
                    "L16",
                    format!(
                        "{}: `{}::{}` with {} argument(s) has no Lean lowering",
                        where_,
                        namespace,
                        name,
                        args.len(),
                    ),
                ));
            }
            for a in args {
                check_lean_std_compat_expr(program, a, where_, diags);
            }
        }
        Expr::BinOp(l, _, r) => {
            check_lean_std_compat_expr(program, l, where_, diags);
            check_lean_std_compat_expr(program, r, where_, diags);
        }
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Cast(e, _) | Expr::Some(e) => {
            check_lean_std_compat_expr(program, e, where_, diags);
        }
        Expr::Index(b, k) => {
            check_lean_std_compat_expr(program, b, where_, diags);
            check_lean_std_compat_expr(program, k, where_, diags);
        }
        Expr::MethodCall(b, _, args) => {
            check_lean_std_compat_expr(program, b, where_, diags);
            for a in args {
                check_lean_std_compat_expr(program, a, where_, diags);
            }
        }
        Expr::If(c, t, e) => {
            check_lean_std_compat_expr(program, c, where_, diags);
            check_lean_std_compat_expr(program, t, where_, diags);
            if let Some(el) = e {
                check_lean_std_compat_expr(program, el, where_, diags);
            }
        }
        Expr::Let(_, v, b) | Expr::For(_, v, b) => {
            check_lean_std_compat_expr(program, v, where_, diags);
            check_lean_std_compat_expr(program, b, where_, diags);
        }
        Expr::Match(s, arms) => {
            check_lean_std_compat_expr(program, s, where_, diags);
            for arm in arms {
                check_lean_std_compat_expr(program, &arm.body, where_, diags);
            }
        }
        Expr::Tuple(items) | Expr::ArrayLit(items) | Expr::Block(items) => {
            for i in items {
                check_lean_std_compat_expr(program, i, where_, diags);
            }
        }
        Expr::RecordConstruct(_, fields) | Expr::RecordUpdate(_, fields) => {
            for (_, v) in fields {
                check_lean_std_compat_expr(program, v, where_, diags);
            }
        }
        Expr::Closure(_, body) => check_lean_std_compat_expr(program, body, where_, diags),
        Expr::Range(a, b) => {
            check_lean_std_compat_expr(program, a, where_, diags);
            check_lean_std_compat_expr(program, b, where_, diags);
        }
        Expr::FnCall(_, args) | Expr::EnumVariantWithData(_, _, args) | Expr::MacroRef(_, args) => {
            for a in args {
                check_lean_std_compat_expr(program, a, where_, diags);
            }
        }
        _ => {}
    }
}

fn check_lean_std_compat_action(program: &Program, action: &RouteAction, where_: &str, diags: &mut Vec<Diagnostic>) {
    match action {
        RouteAction::Return { values } => {
            for v in values {
                check_lean_std_compat_expr(program, v, where_, diags);
            }
        }
        RouteAction::Send {
            args,
            dest,
            send_options,
            ..
        }
        | RouteAction::VarCall {
            args,
            dest,
            send_options,
            ..
        } => {
            for a in args {
                check_lean_std_compat_expr(program, a, where_, diags);
            }
            check_lean_std_compat_expr(program, dest, where_, diags);
            if let Some(opts) = send_options {
                check_lean_std_compat_expr(program, opts, where_, diags);
            }
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            check_lean_std_compat_expr(program, condition, where_, diags);
            for a in then_actions {
                check_lean_std_compat_action(program, a, where_, diags);
            }
            for a in else_actions {
                check_lean_std_compat_action(program, a, where_, diags);
            }
        }
        RouteAction::Let { value, .. } => check_lean_std_compat_expr(program, value, where_, diags),
        RouteAction::Effect { args, .. } => {
            for a in args {
                check_lean_std_compat_expr(program, a, where_, diags);
            }
        }
        RouteAction::Deploy {
            constructor_args,
            send_options,
            ..
        } => {
            for a in constructor_args {
                check_lean_std_compat_expr(program, a, where_, diags);
            }
            if let Some(opts) = send_options {
                check_lean_std_compat_expr(program, opts, where_, diags);
            }
        }
        RouteAction::Rescue { action, .. } => check_lean_std_compat_action(program, action, where_, diags),
        RouteAction::CallRoute { args, .. }
        | RouteAction::Emit { args, .. }
        | RouteAction::ThrowCustom { args, .. } => {
            for a in args {
                check_lean_std_compat_expr(program, a, where_, diags);
            }
        }
        RouteAction::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            for a in update_args {
                check_lean_std_compat_expr(program, a, where_, diags);
            }
            for a in callback_args {
                check_lean_std_compat_expr(program, a, where_, diags);
            }
        }
        RouteAction::For { iter, body, .. } => {
            check_lean_std_compat_expr(program, iter, where_, diags);
            for a in body {
                check_lean_std_compat_action(program, a, where_, diags);
            }
        }
        RouteAction::Throw { .. } => {}
    }
}

// ---------------------------------------------------------------------------
// L8–L11 — send-target / fail-surface safety (evm × lean pair semantics)
//
// These rules encode assumptions of the current Lean-EVM adapter (atomic
// World steps, synchronous typed sends). `rescue`/`recover` is rejected
// on the EVM domain by **E26** (Acki Nacki bounce only); it is not a
// Lean/EVM fail-surface. They are *not* pure Lean-language constraints
// and would need revisiting for a future non-EVM Lean domain.
// ---------------------------------------------------------------------------

/// Walk a single `action` (recursing through `Rescue`, `Conditional`,
/// `For`, etc.) and emit `L8` / `L9` diagnostics for any `Send` or
/// `VarCall` whose dest can't be lowered.
///
/// * `L8` (error): typed `send msg(...) ~> dest` / `var x = msg(...) ~> dest`
///   where `dest` doesn't statically resolve to a same-entity address.
///   In P3 we only support self-calls; cross-entity targets land in
///   P4. Raw `~> dest` (no message, no var) is allowed for any
///   address-typed expression — it lowers to `WorldState.transfer`
///   without entity resolution.
/// * `L9` (error, **evm×lean pair**): `var x = msg(args) ~> dest` where the
///   target route can fail (`lean_route_can_fail`) and the enclosing route
///   is not a fail surface. In atomic EVM/Lean step semantics the failure
///   must propagate (`where`/`throw` on the caller). `rescue` is E26 on
///   this domain and is not a workaround.
/// * `L11` (error, **evm×lean pair**): fire-and-forget / sync self-call to a
///   failing same-entity route without a fail surface.
fn check_lean_send_actions(
    program: &Program,
    entity: &Entity,
    route: &Route,
    actions: &[RouteAction],
    inside_rescue: bool,
    outer_let_env: &HashMap<String, Expr>,
    diags: &mut Vec<Diagnostic>,
) {
    let mut let_env = outer_let_env.clone();
    for action in actions {
        check_lean_send_action(
            program,
            entity,
            route,
            action,
            inside_rescue,
            &let_env,
            diags,
        );
        if let RouteAction::Let {
            pattern: Pattern::Ident(name),
            value,
        } = action
        {
            let_env.insert(name.clone(), value.clone());
        }
    }
}

fn check_lean_send_action(
    program: &Program,
    entity: &Entity,
    route: &Route,
    action: &RouteAction,
    inside_rescue: bool,
    let_env: &HashMap<String, Expr>,
    diags: &mut Vec<Diagnostic>,
) {
    match action {
        RouteAction::Send {
            message: Some(msg),
            dest,
            ..
        } => {
            let target =
                crate::analysis::classify_message_dest(dest, msg, entity, route, program, let_env);
            if !lean_send_dest_resolves(&target) {
                diags.push(Diagnostic::error(
                    "L8",
                    format!(
                        "entity '{}' route '{}': typed send `{}(...) ~> dest` requires `dest` to statically resolve to `<Entity>.address(...)` for an in-program entity; extern / dynamic-address targets need `extern entity` (P4b) or a literal address",
                        entity.name, route.name, msg,
                    ),
                ));
            } else if matches!(target, crate::analysis::SendTarget::DynamicUntyped { .. })
                && extern_route_is_ambiguous(program, msg)
            {
                diags.push(Diagnostic::error(
                    "L8",
                    format!(
                        "entity '{}' route '{}': typed send `{}(...) ~> dest` matches multiple extern entity routes named `{}`",
                        entity.name, route.name, msg, msg,
                    ),
                ));
            } else if matches!(target, crate::analysis::SendTarget::DynamicUntyped { .. })
                && lean_untyped_dispatch_needed(dest, entity, route, let_env, program)
                && !any_in_program_route(program, msg)
            {
                diags.push(Diagnostic::error(
                    "L8",
                    format!(
                        "entity '{}' route '{}': typed send `{}(...) ~> dest` targets a plain-`address` ident but no in-program entity declares route `{}` — dispatch axiom can't be synthesised",
                        entity.name, route.name, msg, msg,
                    ),
                ));
            } else if matches!(target, crate::analysis::SendTarget::SameEntity { .. }) {
                // L11 — fire-and-forget self-send to a failing route.
                if let Some(target) = entity.routes.iter().find(|r| &r.name == msg) {
                    check_lean_internal_call_fail_surface(
                        program,
                        entity,
                        route,
                        target,
                        msg,
                        "send",
                        inside_rescue,
                        diags,
                    );
                }
            }
        }
        RouteAction::CallRoute { name, .. } => {
            // L11 — synchronous internal `call <route>(...)` to a route
            // that can fail. Same-entity calls model Solidity internal
            // calls, whose reverts bubble up to the caller; the Lean
            // lowering only propagates that revert when the caller is a
            // fail surface (otherwise it silently recovers via
            // `Cambrian.exceptGetD`).
            if let Some(target) = entity.routes.iter().find(|r| &r.name == name) {
                check_lean_internal_call_fail_surface(
                    program,
                    entity,
                    route,
                    target,
                    name,
                    "call",
                    inside_rescue,
                    diags,
                );
            }
        }
        RouteAction::Send { message: None, .. } => {
            // Raw value transfer — no entity resolution needed.
        }
        RouteAction::VarCall {
            name,
            message,
            dest,
            ..
        } => {
            let target = crate::analysis::classify_message_dest(
                dest, message, entity, route, program, let_env,
            );
            if !lean_send_dest_resolves(&target) {
                diags.push(Diagnostic::error(
                    "L8",
                    format!(
                        "entity '{}' route '{}': `var {} = {}(...) ~> dest` requires `dest` to statically resolve to `<Entity>.address(...)` for an in-program entity",
                        entity.name, route.name, name, message,
                    ),
                ));
                return;
            }
            if matches!(target, crate::analysis::SendTarget::DynamicUntyped { .. })
                && extern_route_is_ambiguous(program, message)
            {
                diags.push(Diagnostic::error(
                    "L8",
                    format!(
                        "entity '{}' route '{}': `var {} = {}(...) ~> dest` matches multiple extern entity routes named `{}`",
                        entity.name, route.name, name, message, message,
                    ),
                ));
                return;
            }
            if matches!(target, crate::analysis::SendTarget::DynamicUntyped { .. })
                && lean_untyped_dispatch_needed(dest, entity, route, let_env, program)
                && !any_in_program_route(program, message)
            {
                diags.push(Diagnostic::error(
                    "L8",
                    format!(
                        "entity '{}' route '{}': `var {} = {}(...) ~> dest` targets a plain-`address` ident but no in-program entity declares route `{}` — dispatch axiom can't be synthesised",
                        entity.name, route.name, name, message, message,
                    ),
                ));
                return;
            }
            // L9 — failing-target capture needs a fail surface (not `rescue`;
            // E26 rejects rescue/recover on the EVM domain).
            if let Some(target) = entity.routes.iter().find(|r| &r.name == message) {
                if lean_route_can_fail(program, entity, target) && !inside_rescue {
                    diags.push(Diagnostic::error(
                        "L9",
                        format!(
                            "entity '{}' route '{}': `var {} = {}(...) ~> {}.address(...)` captures from a failing route — give '{}' a `where`/`throw` so the failure propagates (`rescue` is Acki Nacki-only, E26)",
                            entity.name, route.name, name, message, entity.name, route.name,
                        ),
                    ));
                }
            }
        }
        RouteAction::Rescue { action: inner, .. } => {
            // E26 already rejects `rescue` on the EVM domain. Recurse so
            // inner sends still see L8; `inside_rescue` is retained only
            // as a walk flag for force-codegen.
            check_lean_send_action(program, entity, route, inner, true, let_env, diags);
        }
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            check_lean_send_actions(
                program,
                entity,
                route,
                then_actions,
                inside_rescue,
                let_env,
                diags,
            );
            check_lean_send_actions(
                program,
                entity,
                route,
                else_actions,
                inside_rescue,
                let_env,
                diags,
            );
        }
        RouteAction::For { body, .. } => {
            check_lean_send_actions(program, entity, route, body, inside_rescue, let_env, diags);
        }
        _ => {}
    }
}

/// L11 helper — emit an error when an internal (same-entity) `call` /
/// fire-and-forget self-send targets a route that can fail while the
/// enclosing route is neither a fail surface nor inside a `rescue`.
///
/// Same-entity invocations model Solidity *internal* calls: a revert in
/// the callee bubbles up and reverts the caller. The Lean lowering only
/// reproduces that when the caller is a fail surface (it binds the call
/// via `←`); otherwise it falls back to `Cambrian.exceptGetD`, which
/// silently swallows the failure and diverges from on-chain semantics.
/// `rescue` is not a Lean/EVM workaround (E26).
fn check_lean_internal_call_fail_surface(
    program: &Program,
    entity: &Entity,
    route: &Route,
    target: &Route,
    callee: &str,
    verb: &str,
    inside_rescue: bool,
    diags: &mut Vec<Diagnostic>,
) {
    if lean_route_can_fail(program, entity, target)
        && !inside_rescue
        && !lean_route_can_fail(program, entity, route)
    {
        diags.push(Diagnostic::error(
            "L11",
            format!(
                "entity '{}' route '{}': `{} {}(...)` invokes a route that can fail, but '{}' is not a fail surface — the revert would be silently recovered. Give '{}' a `where`/`throw` so the failure propagates (`rescue` is Acki Nacki-only, E26).",
                entity.name, route.name, verb, callee, route.name, route.name,
            ),
        ));
    }
}

/// True when `dest` statically resolves to a known target —
/// either `<E>.address(...)` syntactically, an in-scope ident whose
/// type is `Address<Entity>`, or a plain `address`-typed ident that
/// can be dispatched dynamically via opaque axiom. The codegen side
/// (`super::lean_send::classify_dest`) needs to agree on this set;
/// see the matching extension there.
fn lean_send_dest_resolves(target: &crate::analysis::SendTarget) -> bool {
    !matches!(target, crate::analysis::SendTarget::Raw)
}

/// True iff `dest` is an ident whose static type is plain `address`
/// (not `Address<Entity>`). Used by L8 to require a matching in-program
/// route when the dispatch is untyped (`Cambrian.Generated.Dispatch.Untyped.<msg>`).
fn lean_untyped_dispatch_needed(
    dest: &Expr,
    entity: &Entity,
    route: &Route,
    let_env: &HashMap<String, Expr>,
    program: &Program,
) -> bool {
    matches!(
        lean_dest_ident_type(dest, entity, route, let_env, program),
        Some(crate::ast::Type::Simple(ref s)) if s == "address"
    )
}

/// True iff any entity in `program` declares a route named `msg`.
/// Used by L8 to check that the untyped dispatch axiom has at least
/// one in-program signature to synthesise itself from.
fn any_in_program_route(program: &Program, msg: &str) -> bool {
    program
        .entities
        .iter()
        .any(|e| e.routes.iter().any(|r| r.name == msg))
}

fn extern_route_is_ambiguous(program: &Program, msg: &str) -> bool {
    program
        .extern_entities
        .iter()
        .filter(|e| e.routes.iter().any(|r| r.name == msg))
        .count()
        > 1
}

/// Classify an `Ident` dest's static Cambrian type, if we can find one.
/// Returns `Some(Type)` when the ident is a statically resolved `let`,
/// route parameter, or entity member.
fn lean_dest_ident_type(
    dest: &Expr,
    entity: &Entity,
    route: &Route,
    let_env: &HashMap<String, Expr>,
    program: &Program,
) -> Option<crate::ast::Type> {
    let Expr::Ident(name) = dest else {
        return None;
    };
    if let Some(rhs) = let_env.get(name) {
        if let Some((entity, _)) =
            crate::analysis::send_target::resolve_entity_address(rhs, program)
        {
            return Some(crate::ast::Type::TypedAddress(entity));
        }
    }
    if let Some(p) = route.params.iter().find(|p| &p.name == name) {
        return Some(p.ty.clone());
    }
    if let Some(m) = entity.members.iter().find(|m| &m.name == name) {
        return Some(m.ty.clone());
    }
    None
}

/// L10 — informational warning for invariants whose declared
/// `action`s pick routes that contain `~>` (any send). The Lean
/// model uses atomic `WorldState → WorldState` semantics, so any
/// re-entrancy properties such an invariant is trying to express
/// are stated against the post-atomic state, not against a
/// fine-grained interleaving trace.
fn check_lean_invariant_sends(
    program: &Program,
    inv: &crate::ast::InvariantDecl,
    diags: &mut Vec<Diagnostic>,
) {
    let mut flagged = std::collections::HashSet::new();
    for ia in &inv.actions {
        // Resolve the invariant action to its entity + route. For
        // single-entity invariants `instance` is the synthesised
        // `_self`; multi-entity ones (already `L4`-warned) use named
        // instances.
        let entity_name = inv
            .instances
            .iter()
            .find(|i| i.name == ia.instance)
            .map(|i| i.entity.as_str())
            .unwrap_or("");
        let Some(entity) = program.entities.iter().find(|e| e.name == entity_name) else {
            continue;
        };
        let Some(route) = entity.routes.iter().find(|r| r.name == ia.route) else {
            continue;
        };
        if route_contains_send(route) {
            let key = format!("{}::{}", entity_name, route.name);
            if flagged.insert(key) {
                diags.push(Diagnostic::warning(
                    "L10",
                    format!(
                        "invariant '{}' steps through `{}.{}` which contains `~>` actions; Lean P3 lowers these to atomic `WorldState → WorldState` transitions (no fine-grained re-entrancy modelled)",
                        inv.name, entity_name, route.name,
                    ),
                ));
            }
        }
    }
}

fn route_contains_send(route: &Route) -> bool {
    route
        .body
        .all_actions()
        .iter()
        .any(|a| action_contains_send(a))
}

fn action_contains_send(a: &RouteAction) -> bool {
    match a {
        RouteAction::Send { .. } | RouteAction::VarCall { .. } => true,
        RouteAction::Rescue { action, .. } => action_contains_send(action),
        RouteAction::Conditional {
            then_actions,
            else_actions,
            ..
        } => {
            then_actions.iter().any(action_contains_send)
                || else_actions.iter().any(action_contains_send)
        }
        RouteAction::For { body, .. } => body.iter().any(action_contains_send),
        _ => false,
    }
}

/// L5 / L6: walk a sequence of [`TestStep`]s in declaration order,
/// remembering the most recent `Call` so any subsequent
/// `expect throw` / `expect return` can be checked against the
/// targeted route's signature.
fn check_lean_expect_targets(
    program: &Program,
    entity_name: &str,
    steps: &[TestStep],
    where_: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let entity = match program.entities.iter().find(|e| e.name == entity_name) {
        Some(e) => e,
        None => return,
    };
    let mut current_route: Option<&Route> = None;
    for step in steps {
        match step {
            TestStep::Call { route, .. } => {
                current_route = entity.routes.iter().find(|r| &r.name == route);
            }
            TestStep::ExpectThrow { code } => {
                if let Some(r) = current_route {
                    if !lean_route_can_fail(program, entity, r) {
                        diags.push(Diagnostic::error(
                            "L5",
                            format!(
                                "{}: 'expect throw {}' targets route '{}' which cannot fail in Lean (no `where` / `throw` / unrescued extern CALL)",
                                where_, code, r.name,
                            ),
                        ));
                    }
                }
            }
            TestStep::ExpectReturn { .. }
            | TestStep::ExpectReturnTuple { .. }
            | TestStep::ExpectReturnLens { .. } => {
                if let Some(r) = current_route {
                    if r.return_type.is_none() {
                        diags.push(Diagnostic::error(
                            "L6",
                            format!(
                                "{}: 'expect return …' targets route '{}' which has no declared return type",
                                where_, r.name,
                            ),
                        ));
                    }
                }
            }
            _ => {}
        }
    }
}

/// Mirror of `codegen::lean::route::route_fail_mode` — true when the
/// route's Lean signature is wrapped in `Cambrian.RouteResult`. Recurses
/// into same-entity `call` targets so a wrapper that only delegates to a
/// failing helper is itself a fail surface (T-X-010 / T-VAL-003).
///
/// Includes the evm×lean PN-106 overlay: unrescued typed extern sends.
fn lean_route_can_fail(program: &Program, entity: &Entity, route: &Route) -> bool {
    let mut visiting = std::collections::HashSet::new();
    lean_route_can_fail_rec(program, entity, route, &mut visiting)
}

fn lean_route_can_fail_rec(
    program: &Program,
    entity: &Entity,
    route: &Route,
    visiting: &mut std::collections::HashSet<String>,
) -> bool {
    if !visiting.insert(route.name.clone()) {
        return false;
    }
    if crate::analysis::route_can_fail_evm_lean(entity, route) {
        return true;
    }
    if crate::analysis::route_has_unrescued_extern_send(program, entity, route) {
        return true;
    }
    let mut found = false;
    walk_lean_call_callees_for_fail(program, entity, route, visiting, &mut found);
    found
}

fn walk_lean_call_callees_for_fail(
    program: &Program,
    entity: &Entity,
    route: &Route,
    visiting: &mut std::collections::HashSet<String>,
    found: &mut bool,
) {
    fn walk(
        action: &RouteAction,
        program: &Program,
        entity: &Entity,
        visiting: &mut std::collections::HashSet<String>,
        found: &mut bool,
    ) {
        if *found {
            return;
        }
        match action {
            RouteAction::CallRoute { name, .. } => {
                if let Some(target) = entity.routes.iter().find(|r| r.name == *name) {
                    if lean_route_can_fail_rec(program, entity, target, visiting) {
                        *found = true;
                    }
                }
            }
            RouteAction::Conditional {
                then_actions,
                else_actions,
                ..
            } => {
                for a in then_actions {
                    walk(a, program, entity, visiting, found);
                }
                for a in else_actions {
                    walk(a, program, entity, visiting, found);
                }
            }
            RouteAction::For { body, .. } => {
                for a in body {
                    walk(a, program, entity, visiting, found);
                }
            }
            RouteAction::Rescue { .. } => {}
            _ => {}
        }
    }
    match &route.body {
        RouteBody::Unphased(actions) => {
            for a in actions {
                walk(a, program, entity, visiting, found);
            }
        }
        RouteBody::Phased(phases) => {
            for p in phases {
                for a in &p.actions {
                    walk(a, program, entity, visiting, found);
                }
            }
        }
        RouteBody::Mixed(phases, trailing) => {
            for p in phases {
                for a in &p.actions {
                    walk(a, program, entity, visiting, found);
                }
            }
            for a in trailing {
                walk(a, program, entity, visiting, found);
            }
        }
    }
}

/// Walk a [`Type`] and emit `L1` (unsupported generics) / `L14`
/// (unknown simple types) diagnostics for the Lean language core.
fn check_lean_type(
    ty: &Type,
    where_: &str,
    program: &Program,
    entity: Option<&Entity>,
    diags: &mut Vec<Diagnostic>,
) {
    check_lean_no_unsupported_generic(ty, where_, diags);
    check_lean_unknown_simple(ty, where_, program, entity, diags);
}

/// Walk a [`Type`] and emit `L1` diagnostics for any generic that
/// isn't handled in P1 (currently: every non-empty generic).
fn check_lean_no_unsupported_generic(ty: &Type, where_: &str, diags: &mut Vec<Diagnostic>) {
    match ty {
        Type::Generic(name, params) => match name.as_str() {
            "Vec" | "HashMap" | "Option" => {
                for p in params {
                    check_lean_no_unsupported_generic(p, where_, diags);
                }
            }
            _ => {
                diags.push(Diagnostic::error(
                    "L1",
                    format!(
                        "{}: generic type '{}<…>' has no Lean lowering on the Lean target",
                        where_, name,
                    ),
                ));
                for p in params {
                    check_lean_no_unsupported_generic(p, where_, diags);
                }
            }
        },
        Type::Tuple(items) => {
            for t in items {
                check_lean_no_unsupported_generic(t, where_, diags);
            }
        }
        Type::Simple(_) | Type::TypedAddress(_) => {}
    }
}

/// L14 — bare type identifier that resolves to no primitive / record /
/// enum / alias visible on the Lean core. Without this check Lean emits
/// the identifier opaquely and only `lake build` fails.
fn check_lean_unknown_simple(
    ty: &Type,
    where_: &str,
    program: &Program,
    entity: Option<&Entity>,
    diags: &mut Vec<Diagnostic>,
) {
    match ty {
        Type::Simple(name) => {
            if !is_known_lean_type(name, program, entity) {
                diags.push(Diagnostic::error(
                    "L14",
                    format!(
                        "{}: type `{}` does not resolve to a primitive, record, enum, or type alias visible from the Lean target (would emit an opaque identifier and fail at `lake build`).",
                        where_, name,
                    ),
                ));
            }
        }
        Type::Generic(_, params) | Type::Tuple(params) => {
            for p in params {
                check_lean_unknown_simple(p, where_, program, entity, diags);
            }
        }
        Type::TypedAddress(_) => {}
    }
}

fn is_known_lean_type(name: &str, program: &Program, entity: Option<&Entity>) -> bool {
    // Lean accepts the same primitives as EVM plus the lowercase `string`
    // spelling that `lean_types::lower_simple` maps to `String`.
    if name == "string" {
        return true;
    }
    is_known_user_type(name, program, entity)
}

/// Recurse into an action looking for `for`-loops and similar
/// iterator constructs. The shape mirrors [`check_no_evm_field_in_action`]
/// — keep them in sync when the action enum grows.
fn check_lean_no_loops_in_action(action: &RouteAction, where_: &str, diags: &mut Vec<Diagnostic>) {
    match action {
        RouteAction::For { .. } => {
            // P4b: `for` over `Vec` / `Range` lowers in route bodies.
        }
        RouteAction::Conditional {
            condition,
            then_actions,
            else_actions,
        } => {
            check_lean_no_loops_in_expr(condition, where_, diags);
            for a in then_actions {
                check_lean_no_loops_in_action(a, where_, diags);
            }
            for a in else_actions {
                check_lean_no_loops_in_action(a, where_, diags);
            }
        }
        RouteAction::Let { value, .. } => {
            check_lean_no_loops_in_expr(value, where_, diags);
        }
        RouteAction::Return { values } => {
            for v in values {
                check_lean_no_loops_in_expr(v, where_, diags);
            }
        }
        RouteAction::Send {
            args,
            dest,
            send_options,
            ..
        }
        | RouteAction::VarCall {
            args,
            dest,
            send_options,
            ..
        } => {
            for a in args {
                check_lean_no_loops_in_expr(a, where_, diags);
            }
            check_lean_no_loops_in_expr(dest, where_, diags);
            if let Some(opts) = send_options {
                check_lean_no_loops_in_expr(opts, where_, diags);
            }
        }
        RouteAction::Deploy {
            send_options,
            constructor_args,
            ..
        } => {
            for a in constructor_args {
                check_lean_no_loops_in_expr(a, where_, diags);
            }
            if let Some(opts) = send_options {
                check_lean_no_loops_in_expr(opts, where_, diags);
            }
        }
        RouteAction::Rescue { action, .. } => {
            check_lean_no_loops_in_action(action, where_, diags);
        }
        RouteAction::CallRoute { args, .. }
        | RouteAction::Effect { args, .. }
        | RouteAction::Emit { args, .. }
        | RouteAction::ThrowCustom { args, .. } => {
            for a in args {
                check_lean_no_loops_in_expr(a, where_, diags);
            }
        }
        RouteAction::UpdateCode {
            update_args,
            callback_args,
            ..
        } => {
            for a in update_args {
                check_lean_no_loops_in_expr(a, where_, diags);
            }
            for a in callback_args {
                check_lean_no_loops_in_expr(a, where_, diags);
            }
        }
        RouteAction::Throw { .. } => {}
    }
}

/// Recurse into an expression looking for closures, ranges, and `for`
/// expressions — the surface forms that drive Cambrian's iteration
/// constructs and have no Lean P1 equivalent.
fn check_lean_no_loops_in_expr(expr: &Expr, where_: &str, diags: &mut Vec<Diagnostic>) {
    check_lean_no_loops_in_expr_inner(expr, where_, diags, false);
}

fn check_lean_no_loops_in_expr_inner(
    expr: &Expr,
    where_: &str,
    diags: &mut Vec<Diagnostic>,
    allow_closure: bool,
) {
    match expr {
        Expr::For(_, _, _) => {}
        Expr::Closure(_, body) => {
            if allow_closure {
                check_lean_no_loops_in_expr_inner(body, where_, diags, false);
            } else {
                diags.push(Diagnostic::error(
                    "L2",
                    format!(
                        "{}: closure expressions have no Lean lowering outside iterator chains",
                        where_
                    ),
                ));
            }
        }
        Expr::Range(lo, hi) => {
            // P4c lifted the restriction: `lo..hi` lowers to a Lean
            // `List Nat` via `lean_expr::gen_range_list` and composes
            // with `for` / `.fold` / `.map` / `.filter` chains.
            check_lean_no_loops_in_expr_inner(lo, where_, diags, allow_closure);
            check_lean_no_loops_in_expr_inner(hi, where_, diags, allow_closure);
        }
        Expr::MethodCall(receiver, name, args) => {
            let allow_closure_arg = matches!(name.as_str(), "filter" | "map" | "fold" | "for_each");
            check_lean_no_loops_in_expr_inner(receiver, where_, diags, false);
            for a in args {
                check_lean_no_loops_in_expr_inner(a, where_, diags, allow_closure_arg);
            }
        }
        Expr::BinOp(l, _, r) | Expr::Index(l, r) => {
            check_lean_no_loops_in_expr_inner(l, where_, diags, allow_closure);
            check_lean_no_loops_in_expr_inner(r, where_, diags, allow_closure);
        }
        Expr::UnaryOp(_, e) | Expr::FieldAccess(e, _) | Expr::Some(e) | Expr::Cast(e, _) => {
            check_lean_no_loops_in_expr_inner(e, where_, diags, allow_closure);
        }
        Expr::FnCall(_, args)
        | Expr::ArrayLit(args)
        | Expr::Tuple(args)
        | Expr::EnumVariantWithData(_, _, args)
        | Expr::MacroRef(_, args)
        | Expr::NamespacedCall { args, .. } => {
            for a in args {
                check_lean_no_loops_in_expr_inner(a, where_, diags, allow_closure);
            }
        }
        Expr::If(c, t, e) => {
            check_lean_no_loops_in_expr_inner(c, where_, diags, allow_closure);
            check_lean_no_loops_in_expr_inner(t, where_, diags, allow_closure);
            if let Some(e) = e {
                check_lean_no_loops_in_expr_inner(e, where_, diags, allow_closure);
            }
        }
        Expr::Let(_, v, b) => {
            check_lean_no_loops_in_expr_inner(v, where_, diags, allow_closure);
            check_lean_no_loops_in_expr_inner(b, where_, diags, allow_closure);
        }
        Expr::Block(items) => {
            for it in items {
                check_lean_no_loops_in_expr_inner(it, where_, diags, allow_closure);
            }
        }
        Expr::Match(s, arms) => {
            check_lean_no_loops_in_expr_inner(s, where_, diags, allow_closure);
            for arm in arms {
                check_lean_no_loops_in_expr_inner(&arm.body, where_, diags, allow_closure);
            }
        }
        Expr::RecordConstruct(_, fields) | Expr::RecordUpdate(_, fields) => {
            for (_, v) in fields {
                check_lean_no_loops_in_expr_inner(v, where_, diags, allow_closure);
            }
        }
        Expr::AddressOf {
            args, with_params, ..
        } => {
            for a in args {
                check_lean_no_loops_in_expr_inner(a, where_, diags, allow_closure);
            }
            for (_, v) in with_params {
                check_lean_no_loops_in_expr_inner(v, where_, diags, allow_closure);
            }
        }
        Expr::Encode { value, .. } => {
            check_lean_no_loops_in_expr_inner(value, where_, diags, allow_closure);
        }
        Expr::IntLiteral(_)
       
       
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::BoolLiteral(_)
        | Expr::EmptyCollection
        | Expr::Ident(_)
        | Expr::TemporalRef(_)
        | Expr::MsgField(_)
        | Expr::SysField(_)
        | Expr::TraceField(_)
        | Expr::TraceCall { .. }
        | Expr::EnumVariant(_, _)
        | Expr::None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Program {
        crate::ProgramParser::new().parse(src).unwrap()
    }

    // ===== V1: Route reference validation =====

    #[test]
    fn v1_valid_route_refs() {
        let prog = parse(
            r#"
            entity E {
                routes { increment() => [] }
                m_count: uint256 { in increment() => m_count + 1 }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V1").collect();
        assert!(errors.is_empty());
    }

    #[test]
    fn v1_invalid_route_ref() {
        let prog = parse(
            r#"
            entity E {
                routes { increment() => [] }
                m_count: uint256 { in decrement() => m_count - 1 }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V1").collect();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("decrement"));
    }

    // ===== V3: Temporal DAG =====

    #[test]
    fn v3_no_temporal_refs() {
        let prog = parse(
            r#"
            entity E {
                routes { setup() => [] }
                m_a: uint256 { in setup() => 1 }
                m_b: uint256 { in setup() => 2 }
            }
        "#,
        );
        let (orders, diags) = build_temporal_orders(&prog.entities[0]);
        assert!(diags.is_empty());
        let setup_order = orders.iter().find(|o| o.route_name == "setup").unwrap();
        assert_eq!(setup_order.order.len(), 2);
    }

    #[test]
    fn v3_valid_dag() {
        let prog = parse(
            r#"
            entity E {
                routes { setup() => [] }
                m_a: uint256 { in setup() => 1 }
                m_b: uint256 { in setup() => ^m_a + 1 }
            }
        "#,
        );
        let (orders, diags) = build_temporal_orders(&prog.entities[0]);
        assert!(diags.is_empty());
        let setup_order = orders.iter().find(|o| o.route_name == "setup").unwrap();
        let a_pos = setup_order.order.iter().position(|n| n == "m_a").unwrap();
        let b_pos = setup_order.order.iter().position(|n| n == "m_b").unwrap();
        assert!(a_pos < b_pos, "m_a should be computed before m_b");
    }

    // ===== V43: Temporal refs only in transform bodies =====

    #[test]
    fn v43_temporal_in_transform_ok() {
        let prog = parse(
            r#"
            entity E {
                routes { setup() => [] }
                m_a: uint256 { in setup() => 1 }
                m_b: uint256 { in setup() => ^m_a + 1 }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V43").collect();
        assert!(errors.is_empty());
    }

    #[test]
    fn v43_temporal_in_where_rejected() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    bump() where (^m_a > 0) : throw 1 => []
                }
                m_a: uint256 {
                    in bump() => m_a + 1
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V43").collect();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("^m_a"));
        assert!(errors[0].message.contains("where"));
    }

    #[test]
    fn v43_temporal_in_route_let_rejected() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    bump() => [
                        let x = ^m_a;
                    ]
                }
                m_a: uint256 {
                    in bump() => m_a + 1
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V43").collect();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("let"));
    }

    #[test]
    fn v43_temporal_in_from_rejected() {
        let prog = parse(
            r#"
            entity Peer {
                identity m_id: u64
                routes { ping() => [] }
            }
            entity E {
                routes {
                    bump() from Peer(^m_a) => []
                }
                m_a: u64 {
                    in bump() => m_a + 1
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V43").collect();
        assert!(!errors.is_empty());
        assert!(errors.iter().any(|e| e.message.contains("from")));
    }

    #[test]
    fn v3_cycle_detected() {
        let prog = parse(
            r#"
            entity E {
                routes { setup() => [] }
                m_a: uint256 { in setup() => ^m_b + 1 }
                m_b: uint256 { in setup() => ^m_a + 1 }
            }
        "#,
        );
        let (_, diags) = build_temporal_orders(&prog.entities[0]);
        assert!(!diags.is_empty());
        assert!(diags[0].message.contains("cycle"));
    }

    // ===== V4: Pure function purity =====

    #[test]
    fn v4_pure_fn_ok() {
        let prog = parse("pure fn add(a: uint256, b: uint256) -> uint256 { a + b }");
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V4").collect();
        assert!(errors.is_empty());
    }

    #[test]
    fn v4_pure_fn_uses_msg() {
        let prog = parse("pure fn bad() -> uint256 { msg::sender }");
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V4").collect();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("msg::sender"));
    }

    #[test]
    fn v4_pure_fn_uses_temporal() {
        let prog = parse("pure fn bad() -> uint256 { ^m_count }");
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V4").collect();
        assert_eq!(errors.len(), 1);
    }

    // ===== V7: Constant literal types =====

    #[test]
    fn v7_valid_constants() {
        let prog = parse(
            r#"
            entity E {
                const MAX: uint8 = 32
                const FLAG: bool = true
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V7").collect();
        assert!(errors.is_empty());
    }

    #[test]
    fn v7_non_literal_constant() {
        let prog = parse(
            r#"
            entity E {
                const BAD: uint8 = x + 1
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V7").collect();
        assert_eq!(errors.len(), 1);
    }

    // ===== V8: Undefined references =====

    #[test]
    fn v8_undefined_fn() {
        let prog = parse(
            r#"
            entity E {
                routes { go() => [] }
                m_x: uint256 { in go() => unknown_fn(1) }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V8").collect();
        assert!(!errors.is_empty());
        assert!(errors.iter().any(|e| e.message.contains("unknown_fn")));
    }

    // ===== V10: Pure route — no member transforms =====

    #[test]
    fn v10_pure_route_with_transforms() {
        let prog = parse(
            r#"
            entity E {
                routes { pure compute(x: u64) => [] }
                m_val: u64 { in compute(x) => x }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V10").collect();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("compute"));
    }

    #[test]
    fn v10_pure_route_no_transforms() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    pure add(a: u64, b: u64) -> u64 => [
                        return(a + b)
                    ]
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V10").collect();
        assert!(errors.is_empty());
    }

    // ===== V11: Pure route — no msg::, members, macros, temporals =====

    #[test]
    fn v11_pure_route_uses_msg() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    pure bad() -> address => [
                        return(msg::sender)
                    ]
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V11").collect();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("msg::sender"));
    }

    #[test]
    fn v11_pure_route_uses_member() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    pure bad() -> u64 => [
                        return(m_count)
                    ]
                }
                m_count: u64 {}
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V11").collect();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("m_count"));
    }

    #[test]
    fn v11_pure_route_uses_temporal() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    pure bad() -> u64 => [
                        return(^m_count)
                    ]
                }
                m_count: u64 {}
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V11").collect();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("^m_count"));
    }

    #[test]
    fn v11_pure_route_ok() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    pure add(a: u64, b: u64) -> u64 => [
                        return(a + b)
                    ]
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V11").collect();
        assert!(errors.is_empty());
    }

    #[test]
    fn v8_undefined_macro() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    go()
                    where @nonexistent() : throw 100 => []
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V8").collect();
        assert!(!errors.is_empty());
    }

    // ===== W1: Unused members =====

    #[test]
    fn w1_unused_member() {
        let prog = parse(
            r#"
            entity E {
                routes { go() => [] }
                m_unused: u64 {}
            }
        "#,
        );
        let diags = validate(&prog);
        let warnings: Vec<_> = diags.iter().filter(|d| d.code == "W1").collect();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("m_unused"));
    }

    #[test]
    fn w1_used_member_no_warning() {
        let prog = parse(
            r#"
            entity E {
                routes { go() => [] }
                m_val: u64 {
                    in go() => 1
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let warnings: Vec<_> = diags.iter().filter(|d| d.code == "W1").collect();
        assert!(warnings.is_empty());
    }

    // ===== W2: Unused pure functions =====

    #[test]
    fn w2_unused_pure_fn() {
        let prog = parse(
            r#"
            pure fn helper(x: u64) -> u64 { x }
            entity E {
                routes { go() => [] }
            }
        "#,
        );
        let diags = validate(&prog);
        let warnings: Vec<_> = diags.iter().filter(|d| d.code == "W2").collect();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("helper"));
    }

    #[test]
    fn w2_used_pure_fn_no_warning() {
        let prog = parse(
            r#"
            pure fn helper(x: u64) -> u64 { x }
            entity E {
                routes { go() => [] }
                m_val: u64 {
                    in go() => helper(42)
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let warnings: Vec<_> = diags.iter().filter(|d| d.code == "W2").collect();
        assert!(warnings.is_empty());
    }

    // ===== W3: Unused constants =====

    #[test]
    fn w3_unused_constant() {
        let prog = parse(
            r#"
            entity E {
                const UNUSED: u64 = 42
                routes { go() => [] }
            }
        "#,
        );
        let diags = validate(&prog);
        let warnings: Vec<_> = diags.iter().filter(|d| d.code == "W3").collect();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("UNUSED"));
    }

    #[test]
    fn w3_used_constant_no_warning() {
        let prog = parse(
            r#"
            entity E {
                const LIMIT: u64 = 100
                routes {
                    go() where m_val < LIMIT : throw 1 => []
                }
                m_val: u64 {}
            }
        "#,
        );
        let diags = validate(&prog);
        let warnings: Vec<_> = diags.iter().filter(|d| d.code == "W3").collect();
        assert!(warnings.is_empty());
    }

    // ===== V13: Duplicate phase names =====

    #[test]
    fn v13_duplicate_phase_name() {
        let prog = parse(
            r#"
            use gosh
            entity E {
                routes {
                    go() => [
                        save: [ gosh::commit() ]
                        save: [ gosh::exit(0) ]
                    ]
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V13").collect();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("save"));
    }

    #[test]
    fn v13_unique_phases_no_error() {
        let prog = parse(
            r#"
            use gosh
            entity E {
                routes {
                    go() => [
                        save: [ gosh::commit() ]
                        done: [ gosh::exit(0) ]
                    ]
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V13").collect();
        assert!(errors.is_empty());
    }

    // ===== V14: Phased/unphased consistency =====

    #[test]
    fn v14_unphased_transform_in_phased_route() {
        let prog = parse(
            r#"
            use gosh
            entity E {
                routes {
                    go(x: u64) => [
                        save: [ gosh::commit() ]
                    ]
                }
                m_val: u64 {
                    in go(x) => x
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V14").collect();
        assert_eq!(errors.len(), 1);
        assert!(errors[0]
            .message
            .contains("unphased transform for phased route"));
    }

    #[test]
    fn v14_phased_transform_in_unphased_route() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    go(x: u64) => []
                }
                m_val: u64 {
                    in go(x) => save: x
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V14").collect();
        assert_eq!(errors.len(), 1);
        assert!(errors[0]
            .message
            .contains("phased transform for unphased route"));
    }

    #[test]
    fn v14_correctly_phased_no_error() {
        let prog = parse(
            r#"
            use gosh
            entity E {
                routes {
                    go(x: u64) => [
                        save: [ gosh::commit() ]
                    ]
                }
                m_val: u64 {
                    in go(x) => save: x
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V14").collect();
        assert!(errors.is_empty());
    }

    // ===== V15: Unknown phase tag in member transform =====

    #[test]
    fn v15_unknown_phase_tag() {
        let prog = parse(
            r#"
            use gosh
            entity E {
                routes {
                    go(x: u64) => [
                        save: [ gosh::commit() ]
                    ]
                }
                m_val: u64 {
                    in go(x) => other: x
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V15").collect();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("other"));
    }

    #[test]
    fn v15_known_phase_tag_no_error() {
        let prog = parse(
            r#"
            use gosh
            entity E {
                routes {
                    go(x: u64) => [
                        save: [ gosh::commit() ]
                        done: []
                    ]
                }
                m_val: u64 {
                    in go(x) => save: x
                    in go(x) => done: x + 1
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V15").collect();
        assert!(errors.is_empty());
    }

    // ===== V13: duplicate phases — extended =====

    #[test]
    fn v13_multiple_duplicates() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    go() => [
                        a: []
                        b: []
                        a: []
                        b: []
                    ]
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V13").collect();
        assert_eq!(errors.len(), 2, "Two duplicate phase names: {:?}", errors);
    }

    #[test]
    fn v13_triple_duplicate() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    go() => [
                        x: []
                        x: []
                        x: []
                    ]
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V13").collect();
        assert_eq!(
            errors.len(),
            2,
            "Two errors for triple duplicate: {:?}",
            errors
        );
    }

    #[test]
    fn v13_many_unique_phases_no_error() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    go() => [
                        a: [] b: [] c: [] d: [] e: []
                    ]
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V13").collect();
        assert!(errors.is_empty());
    }

    // ===== V14: phased/unphased consistency — extended =====

    #[test]
    fn v14_multiple_members_unphased_in_phased_route() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    go(x: u64) => [ step: [] ]
                }
                m_a: u64 { in go(x) => x }
                m_b: u64 { in go(x) => x + 1 }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V14").collect();
        assert_eq!(errors.len(), 2, "Both members should error: {:?}", errors);
    }

    #[test]
    fn v14_one_member_phased_one_not() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    go(x: u64) => [ step: [] ]
                }
                m_a: u64 { in go(x) => step: x }
                m_b: u64 { in go(x) => x + 1 }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V14").collect();
        assert_eq!(errors.len(), 1, "Only m_b should error: {:?}", errors);
        assert!(errors[0].message.contains("m_b"));
    }

    #[test]
    fn v14_mixed_routes_one_phased_one_not() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    foo(x: u64) => [ step: [] ]
                    bar(y: u64) => []
                }
                m_val: u64 {
                    in foo(x) => step: x
                    in bar(y) => y
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V14").collect();
        assert!(
            errors.is_empty(),
            "Each route checked independently: {:?}",
            errors
        );
    }

    #[test]
    fn v14_phased_transform_for_unphased_multiple_members() {
        let prog = parse(
            r#"
            entity E {
                routes { go(x: u64) => [] }
                m_a: u64 { in go(x) => step: x }
                m_b: u64 { in go(x) => step: x + 1 }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V14").collect();
        assert_eq!(
            errors.len(),
            2,
            "Both members phased in unphased route: {:?}",
            errors
        );
    }

    #[test]
    fn v14_phased_route_no_transforms_ok() {
        let prog = parse(
            r#"
            use gosh
            entity E {
                routes {
                    go() => [
                        step: [ gosh::commit() ]
                    ]
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V14").collect();
        assert!(
            errors.is_empty(),
            "Route with phases but no member transforms is valid"
        );
    }

    #[test]
    fn v14_multiple_phased_routes_all_correct() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    foo(x: u64) => [ a: [] b: [] ]
                    bar(y: u64) => [ c: [] ]
                }
                m_val: u64 {
                    in foo(x) => a: x
                    in foo(x) => b: x + 1
                    in bar(y) => c: y
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags
            .iter()
            .filter(|d| d.code == "V14" || d.code == "V15")
            .collect();
        assert!(
            errors.is_empty(),
            "All transforms correctly phased: {:?}",
            errors
        );
    }

    // ===== V15: unknown phase tag — extended =====

    #[test]
    fn v15_multiple_unknown_tags() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    go(x: u64) => [ real: [] ]
                }
                m_a: u64 { in go(x) => fake1: x }
                m_b: u64 { in go(x) => fake2: x }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V15").collect();
        assert_eq!(errors.len(), 2, "Two unknown tags: {:?}", errors);
    }

    #[test]
    fn v15_one_known_one_unknown() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    go(x: u64) => [ real: [] ]
                }
                m_val: u64 {
                    in go(x) => real: x
                    in go(x) => fake: x + 1
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V15").collect();
        assert_eq!(errors.len(), 1, "Only 'fake' should error: {:?}", errors);
        assert!(errors[0].message.contains("fake"));
    }

    #[test]
    fn v15_phase_not_used_by_any_member_is_valid() {
        let prog = parse(
            r#"
            use gosh
            entity E {
                routes {
                    go(x: u64) => [
                        effects_only: [ gosh::commit() ]
                        transforms: []
                    ]
                }
                m_val: u64 {
                    in go(x) => transforms: x
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V15").collect();
        assert!(
            errors.is_empty(),
            "Phase with only effects (no transforms) is valid"
        );
    }

    #[test]
    fn v15_member_uses_all_phases() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    go(x: u64) => [ a: [] b: [] c: [] ]
                }
                m_val: u64 {
                    in go(x) => a: x
                    in go(x) => b: x + 1
                    in go(x) => c: x + 2
                }
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags.iter().filter(|d| d.code == "V15").collect();
        assert!(errors.is_empty());
    }

    // ===== Combined V13+V14+V15 =====

    #[test]
    fn combined_v13_v14_v15_all_errors() {
        let prog = parse(
            r#"
            entity E {
                routes {
                    go(x: u64) => [
                        step: []
                        step: []
                    ]
                }
                m_a: u64 { in go(x) => x }
                m_b: u64 { in go(x) => unknown: x }
            }
        "#,
        );
        let diags = validate(&prog);
        assert!(
            diags.iter().any(|d| d.code == "V13"),
            "Duplicate phase: {:?}",
            diags
        );
        assert!(
            diags.iter().any(|d| d.code == "V14"),
            "Unphased transform: {:?}",
            diags
        );
        assert!(
            diags.iter().any(|d| d.code == "V15"),
            "Unknown phase tag: {:?}",
            diags
        );
    }

    #[test]
    fn valid_complex_phased_entity() {
        let prog = parse(
            r#"
            use gosh

            entity Vault {
                routes {
                    deposit(amount: u128) => [
                        update: []
                        notify: [ gosh::commit() ]
                    ]
                    withdraw(amount: u128)
                        where m_balance >= amount : throw 100
                    => [
                        deduct: [ gosh::rawReserve(100, 0) ]
                        send: [ ~> m_owner ]
                    ]
                    view balance() -> u128 => [ return(m_balance) ]
                }
                m_balance: u128 {
                    in deposit(amount) => update: m_balance + amount
                    in withdraw(amount) => deduct: m_balance - amount
                }
                m_owner: address {}
            }
        "#,
        );
        let diags = validate(&prog);
        let errors: Vec<_> = diags
            .iter()
            .filter(|d| matches!(d.severity, Severity::Error))
            .collect();
        assert!(
            errors.is_empty(),
            "Complex valid phased entity should have no errors: {:?}",
            errors
        );
    }
}
