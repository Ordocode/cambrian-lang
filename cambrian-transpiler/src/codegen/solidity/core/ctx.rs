// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM codegen — per-program context (replaces program-scoped thread-locals).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::ast::{Entity, EnumDecl, Expr, Program, PureFn, Record, Type};

use super::state::is_payload_enum;
use super::types::is_mapping_type;
use crate::codegen::solidity::evm::analysis::build_hashmap_exists_sidecar_keys;

thread_local! {
    /// Test-codegen bridge: `generate_evm_tests` sets this once per program
    /// so helpers can call `gen_expr` without threading `&EvmCtx` through
    /// every private helper. Main Solidity codegen threads `&EvmCtx` explicitly.
    static ACTIVE_CTX: RefCell<Option<EvmCtx>> = RefCell::new(None);
}

/// Entity/route/phase scope for one expression-emission path.
#[derive(Clone, Copy)]
pub struct EmitScope<'a> {
    pub entity: Option<&'a Entity>,
    pub route: Option<&'a str>,
    pub phase: Option<&'a str>,
    /// When set, value-producing subexpressions narrow to this Cambrian type.
    pub expected_ty: Option<&'a Type>,
}

impl<'a> EmitScope<'a> {
    pub fn none() -> Self {
        Self {
            entity: None,
            route: None,
            phase: None,
            expected_ty: None,
        }
    }

    pub fn for_entity(entity: &'a Entity) -> Self {
        Self {
            entity: Some(entity),
            route: None,
            phase: None,
            expected_ty: None,
        }
    }

    pub fn for_transform(entity: &'a Entity, route: &'a str, phase: Option<&'a str>) -> Self {
        Self {
            entity: Some(entity),
            route: Some(route),
            phase,
            expected_ty: None,
        }
    }

    pub fn with_expected_ty(&self, expected_ty: Option<&'a Type>) -> Self {
        Self {
            entity: self.entity,
            route: self.route,
            phase: self.phase,
            expected_ty,
        }
    }
}

/// Program-scoped data for one `gen_evm_solidity` invocation.
#[derive(Clone)]
pub struct EvmCtx {
    pub deterministic: bool,
    /// When false, constructors omit `payable` and factory CREATE2 uses `value: 0`.
    pub allow_constructor_payable: bool,
    pub pure_fn_returns: HashMap<String, Type>,
    pub pure_fn_params: HashMap<String, Vec<Type>>,
    pub pure_fn_exists_params: HashMap<String, Vec<(usize, String)>>,
    /// HashMap params whose body iterates via `.keys()` / `.fold()` / etc.
    pub pure_fn_keys_params: HashMap<String, Vec<(usize, String)>>,
    pub program_enums: HashSet<String>,
    /// Program-scope `record` names (PM-002): members typed as these must
    /// lower to the struct name, not silent `uint256` erasure.
    pub program_records: HashSet<String>,
    pub enum_decls: HashMap<String, EnumDecl>,
    /// Program + entity record decls for default zero-init / layout helpers.
    pub record_decls: HashMap<String, Record>,
    pub lib_fn_library: HashMap<String, String>,
    /// Library name → pure-fn names declared in that library.
    pub library_fns: HashMap<String, HashSet<String>>,
    pub solidity_import_entities: HashSet<String>,
    /// Entity name → mapping members that carry an `_exists` sidecar.
    /// Decided once here so the declaration, the insert/update lowering
    /// and the remove lowering cannot disagree: a sidecar that is
    /// declared and read but never written makes every `.exists(k)`
    /// answer false forever.
    pub members_with_exists: HashMap<String, HashSet<String>>,
    /// `"{entity}::{member}"` for HashMap members that need a `_keys` sidecar
    /// (includes transitive propagation through pure-fn callees).
    pub hashmap_iterated_members: HashSet<String>,
}

impl EvmCtx {
    pub fn empty() -> Self {
        Self {
            deterministic: false,
            allow_constructor_payable: true,
            pure_fn_returns: HashMap::new(),
            pure_fn_params: HashMap::new(),
            pure_fn_exists_params: HashMap::new(),
            pure_fn_keys_params: HashMap::new(),
            program_enums: HashSet::new(),
            program_records: HashSet::new(),
            enum_decls: HashMap::new(),
            record_decls: HashMap::new(),
            lib_fn_library: HashMap::new(),
            library_fns: HashMap::new(),
            solidity_import_entities: HashSet::new(),
            members_with_exists: HashMap::new(),
            hashmap_iterated_members: HashSet::new(),
        }
    }

    pub fn build(program: &Program, deterministic: bool, allow_constructor_payable: bool) -> Self {
        let mut pure_fn_returns = HashMap::new();
        let mut pure_fn_params = HashMap::new();
        let mut lib_fn_owners: HashMap<String, Vec<String>> = HashMap::new();
        let mut library_fns: HashMap<String, HashSet<String>> = HashMap::new();

        for pf in &program.pure_fns {
            pure_fn_returns.insert(pf.name.clone(), pf.return_type.clone());
            pure_fn_params.insert(
                pf.name.clone(),
                pf.params.iter().map(|p| p.ty.clone()).collect(),
            );
        }
        for lib in &program.libraries {
            for pf in &lib.pure_fns {
                pure_fn_returns.insert(pf.name.clone(), pf.return_type.clone());
                pure_fn_params.insert(
                    pf.name.clone(),
                    pf.params.iter().map(|p| p.ty.clone()).collect(),
                );
                lib_fn_owners
                    .entry(pf.name.clone())
                    .or_default()
                    .push(lib.name.clone());
                library_fns
                    .entry(lib.name.clone())
                    .or_default()
                    .insert(pf.name.clone());
            }
        }
        let mut lib_fn_library = HashMap::new();
        for (fn_name, libs) in &lib_fn_owners {
            if libs.len() == 1 {
                lib_fn_library.insert(fn_name.clone(), libs[0].clone());
            }
        }

        let pure_fn_exists_params = build_pure_fn_exists_params(program);
        let pure_fn_keys_params = build_pure_fn_keys_params(program);

        let mut program_enums = HashSet::new();
        for e in &program.enums {
            program_enums.insert(e.name.clone());
        }

        let mut program_records = HashSet::new();
        for r in &program.records {
            program_records.insert(r.name.clone());
        }

        let mut enum_decls = HashMap::new();
        for e in &program.enums {
            enum_decls.insert(e.name.clone(), e.clone());
        }
        for entity in &program.entities {
            for e in &entity.enums {
                enum_decls.insert(e.name.clone(), e.clone());
            }
        }

        let mut record_decls = HashMap::new();
        for r in &program.records {
            record_decls.insert(r.name.clone(), r.clone());
        }
        for entity in &program.entities {
            for r in &entity.records {
                record_decls.insert(r.name.clone(), r.clone());
            }
        }

        let mut solidity_import_entities = HashSet::new();
        for ext in &program.extern_entities {
            if ext.solidity_import.is_some() {
                solidity_import_entities.insert(ext.name.clone());
            }
        }

        let mut members_with_exists: HashMap<String, HashSet<String>> = HashMap::new();
        for key in build_hashmap_exists_sidecar_keys(program, &pure_fn_keys_params) {
            if let Some((entity, member)) = key.split_once("::") {
                members_with_exists
                    .entry(entity.to_string())
                    .or_default()
                    .insert(member.to_string());
            }
        }

        let hashmap_iterated_members =
            crate::codegen::solidity::evm::analysis::build_hashmap_iterated_member_keys(
                program,
                &pure_fn_keys_params,
            );

        Self {
            deterministic,
            allow_constructor_payable,
            pure_fn_returns,
            pure_fn_params,
            pure_fn_exists_params,
            pure_fn_keys_params,
            program_enums,
            program_records,
            enum_decls,
            record_decls,
            lib_fn_library,
            library_fns,
            solidity_import_entities,
            members_with_exists,
            hashmap_iterated_members,
        }
    }

    /// Whether `member` of `entity` carries an `_exists` sidecar mapping.
    /// The single source of truth for the declaration and for the
    /// insert/update/remove lowering that has to keep the flag in sync.
    pub fn member_has_exists_sidecar(&self, entity: &str, member: &str) -> bool {
        self.members_with_exists
            .get(entity)
            .is_some_and(|s| s.contains(member))
    }

    pub fn hashmap_member_is_iterated(&self, entity: &Entity, member_name: &str) -> bool {
        self.hashmap_iterated_members
            .contains(&format!("{}::{}", entity.name, member_name))
    }

    pub fn is_deterministic_mode(&self) -> bool {
        self.deterministic
    }

    pub fn lookup_pure_fn_return(&self, name: &str) -> Option<Type> {
        self.pure_fn_returns.get(name).cloned()
    }

    pub fn lookup_pure_fn_params(&self, name: &str) -> Option<Vec<Type>> {
        self.pure_fn_params.get(name).cloned()
    }

    pub fn lookup_pure_fn_exists_params(
        &self,
        name: &str,
        lib: Option<&str>,
    ) -> Option<Vec<(usize, String)>> {
        self.pure_fn_exists_params
            .get(&pure_fn_sidecar_key(lib, name))
            .cloned()
    }

    pub fn lookup_pure_fn_keys_params(
        &self,
        name: &str,
        lib: Option<&str>,
    ) -> Option<Vec<(usize, String)>> {
        self.pure_fn_keys_params
            .get(&pure_fn_sidecar_key(lib, name))
            .cloned()
    }

    pub fn is_program_enum(&self, name: &str) -> bool {
        self.program_enums.contains(name)
    }

    pub fn is_program_record(&self, name: &str) -> bool {
        self.program_records.contains(name)
    }

    pub fn lookup_enum(&self, name: &str) -> Option<EnumDecl> {
        self.enum_decls.get(name).cloned()
    }

    pub fn lookup_record(&self, name: &str) -> Option<&Record> {
        self.record_decls.get(name)
    }

    pub fn lookup_library_for_fn(&self, fn_name: &str) -> Option<String> {
        self.lib_fn_library.get(fn_name).cloned()
    }

    pub fn library_has_fn(&self, lib: &str, fn_name: &str) -> bool {
        self.library_fns
            .get(lib)
            .map(|s| s.contains(fn_name))
            .unwrap_or(false)
    }

    pub fn interface_cast_name(&self, entity_name: &str) -> String {
        if self.solidity_import_entities.contains(entity_name) {
            entity_name.to_string()
        } else {
            format!("I{}", entity_name)
        }
    }

    pub fn is_payload_enum_named(&self, name: &str) -> bool {
        self.lookup_enum(name)
            .map(|d| is_payload_enum(&d))
            .unwrap_or(false)
    }
}

pub(crate) fn set_active_ctx(ctx: EvmCtx) {
    ACTIVE_CTX.with(|c| *c.borrow_mut() = Some(ctx));
}

pub(crate) fn reset_active_ctx() {
    ACTIVE_CTX.with(|c| *c.borrow_mut() = None);
}

pub(crate) fn active_ctx() -> EvmCtx {
    ACTIVE_CTX.with(|c| {
        c.borrow()
            .clone()
            .expect("EvmCtx not set — call set_active_ctx before test codegen")
    })
}

/// Test-codegen convenience for expressions without route context.
pub(crate) fn gen_expr_test(expr: &Expr, entity: &Entity) -> Option<String> {
    use super::expr::gen_expr;
    let ctx = active_ctx();
    // Entity-local enums are declared inside the entity contract; the test
    // contract needs the `Entity.Enum.Variant` spelling.
    if let Expr::EnumVariant(enum_name, variant) = expr {
        if let Some(decl) = entity.enums.iter().find(|e| &e.name == enum_name) {
            if !is_payload_enum(decl) {
                return Some(format!("{}.{}.{}", entity.name, enum_name, variant));
            }
        }
    }
    let scope = EmitScope::for_entity(entity);
    let ast_expr = expr;
    super::scratch::with_active_scratch(|scratch| gen_expr(ast_expr, &ctx, &scope, scratch))
}

/// Test-codegen expression path when the owning route is known.
pub(crate) fn gen_expr_test_ir(
    expr: &Expr,
    program: &Program,
    entity: &Entity,
    route: &crate::ast::Route,
) -> Option<String> {
    use super::expr::gen_expr;
    use super::types::{coerce_return_value, sol_type_entity};
    use crate::codegen::solidity::evm::route::materialize_typed_expr;
    use crate::ir::{lower_expr, LowerCtx};

    let lower_ctx = LowerCtx::new(program, entity, route);
    let typed = lower_expr(expr, &lower_ctx);
    let materialized = materialize_typed_expr(&typed);
    let ctx = active_ctx();
    let scope = match route.return_type.as_ref() {
        Some(ty) => EmitScope::for_entity(entity).with_expected_ty(Some(ty)),
        None => EmitScope::for_entity(entity),
    };
    super::scratch::with_active_scratch(|scratch| {
        let rendered = gen_expr(&materialized, &ctx, &scope, scratch)?;
        let return_ty = route.return_type.as_ref()?;
        let target = sol_type_entity(entity, return_ty, true, &ctx);
        Some(coerce_return_value(
            &target,
            rendered,
            &materialized,
            entity,
            &ctx,
            scratch,
        ))
    })
}

fn param_uses_exists(body: &Expr, name: &str) -> bool {
    match body {
        Expr::MethodCall(base, method, args) if method == "exists" => {
            if matches!(base.as_ref(), Expr::Ident(n) if n == name) {
                return true;
            }
            param_uses_exists(base, name) || args.iter().any(|a| param_uses_exists(a, name))
        }
        Expr::MethodCall(base, _, args) => {
            param_uses_exists(base, name) || args.iter().any(|a| param_uses_exists(a, name))
        }
        Expr::BinOp(l, _, r) => param_uses_exists(l, name) || param_uses_exists(r, name),
        Expr::UnaryOp(_, x) => param_uses_exists(x, name),
        Expr::FieldAccess(b, _) => param_uses_exists(b, name),
        Expr::Index(b, k) => param_uses_exists(b, name) || param_uses_exists(k, name),
        Expr::If(c, t, e) => {
            param_uses_exists(c, name)
                || param_uses_exists(t, name)
                || e.as_ref().map_or(false, |x| param_uses_exists(x, name))
        }
        Expr::Let(_, v, b) => param_uses_exists(v, name) || param_uses_exists(b, name),
        Expr::Block(items) => items.iter().any(|e| param_uses_exists(e, name)),
        Expr::FnCall(_, args) | Expr::MacroRef(_, args) => {
            args.iter().any(|a| param_uses_exists(a, name))
        }
        Expr::Cast(e, _) => param_uses_exists(e, name),
        Expr::Tuple(es) => es.iter().any(|e| param_uses_exists(e, name)),
        Expr::Match(s, arms) => {
            param_uses_exists(s, name) || arms.iter().any(|a| param_uses_exists(&a.body, name))
        }
        Expr::Some(e) => param_uses_exists(e, name),
        Expr::Range(s, e) => param_uses_exists(s, name) || param_uses_exists(e, name),
        _ => false,
    }
}

fn collect_pure_fn_exists_params(pf: &PureFn) -> Vec<(usize, String)> {
    let mut hits = Vec::new();
    for (i, p) in pf.params.iter().enumerate() {
        if is_mapping_type(&p.ty) && param_uses_exists(&pf.body, &p.name) {
            hits.push((i, p.name.clone()));
        }
    }
    hits
}

fn build_pure_fn_exists_params(program: &Program) -> HashMap<String, Vec<(usize, String)>> {
    let mut m = HashMap::new();
    for pf in &program.pure_fns {
        let hits = collect_pure_fn_exists_params(pf);
        if !hits.is_empty() {
            m.insert(pf.name.clone(), hits);
        }
    }
    for lib in &program.libraries {
        for pf in &lib.pure_fns {
            let hits = collect_pure_fn_exists_params(pf);
            if !hits.is_empty() {
                m.insert(pure_fn_sidecar_key(Some(&lib.name), &pf.name), hits);
            }
        }
    }
    m
}

fn param_uses_iteration(body: &Expr, name: &str) -> bool {
    fn is_iter_method(method: &str) -> bool {
        matches!(method, "keys" | "values" | "iter")
    }
    fn is_chain_method(method: &str) -> bool {
        matches!(
            method,
            "fold" | "collect" | "filter" | "map" | "take" | "enumerate"
        )
    }
    fn walk(expr: &Expr, param_name: &str) -> bool {
        match expr {
            Expr::MethodCall(base, method, args) => {
                if method == "is_empty" && args.is_empty() {
                    if let Expr::Ident(n) = base.as_ref() {
                        if n == param_name {
                            return true;
                        }
                    }
                }
                if (is_iter_method(method) && args.is_empty()) || is_chain_method(method) {
                    if let Expr::Ident(n) = base.as_ref() {
                        if n == param_name {
                            return true;
                        }
                    }
                }
                walk(base, param_name) || args.iter().any(|a| walk(a, param_name))
            }
            Expr::For(_, it, body) => {
                matches!(it.as_ref(), Expr::Ident(n) if n == param_name)
                    || walk(it, param_name)
                    || walk(body, param_name)
            }
            Expr::BinOp(l, _, r) => walk(l, param_name) || walk(r, param_name),
            Expr::UnaryOp(_, e) => walk(e, param_name),
            Expr::FieldAccess(b, _) => walk(b, param_name),
            Expr::Index(b, k) => walk(b, param_name) || walk(k, param_name),
            Expr::FnCall(_, args) | Expr::MacroRef(_, args) => {
                args.iter().any(|a| walk(a, param_name))
            }
            Expr::If(c, t, e) => {
                walk(c, param_name)
                    || walk(t, param_name)
                    || e.as_ref().map_or(false, |x| walk(x, param_name))
            }
            Expr::Let(_, v, b) => walk(v, param_name) || walk(b, param_name),
            Expr::Block(items) => items.iter().any(|e| walk(e, param_name)),
            Expr::RecordConstruct(_, fields) => {
                fields.iter().any(|(_, e)| walk(e, param_name))
            }
            Expr::RecordUpdate(b, fields) => {
                walk(b, param_name) || fields.iter().any(|(_, e)| walk(e, param_name))
            }
            Expr::Closure(_, b) => walk(b, param_name),
            Expr::Cast(e, _) => walk(e, param_name),
            Expr::Tuple(es) => es.iter().any(|e| walk(e, param_name)),
            Expr::Match(s, arms) => {
                walk(s, param_name) || arms.iter().any(|a| walk(&a.body, param_name))
            }
            Expr::EnumVariantWithData(_, _, args) => args.iter().any(|a| walk(a, param_name)),
            Expr::Some(e) => walk(e, param_name),
            Expr::Range(s, e) => walk(s, param_name) || walk(e, param_name),
            Expr::NamespacedCall { args, .. } => args.iter().any(|a| walk(a, param_name)),
            Expr::AddressOf {
                args, with_params, ..
            } => {
                args.iter().any(|a| walk(a, param_name))
                    || with_params.iter().any(|(_, e)| walk(e, param_name))
            }
            Expr::Encode { value, .. } => walk(value, param_name),
            _ => false,
        }
    }
    walk(body, name)
}

fn collect_pure_fn_keys_params_direct(pf: &PureFn) -> Vec<(usize, String)> {
    let mut hits = Vec::new();
    for (i, p) in pf.params.iter().enumerate() {
        if is_mapping_type(&p.ty) && param_uses_iteration(&pf.body, &p.name) {
            hits.push((i, p.name.clone()));
        }
    }
    hits
}

fn param_passes_to_keys_callee(
    body: &Expr,
    param_name: &str,
    keys_map: &HashMap<String, Vec<(usize, String)>>,
    lib: Option<&str>,
) -> bool {
    fn walk(
        expr: &Expr,
        param_name: &str,
        keys_map: &HashMap<String, Vec<(usize, String)>>,
        lib: Option<&str>,
    ) -> bool {
        match expr {
            Expr::FnCall(name, args) => {
                for (i, arg) in args.iter().enumerate() {
                    if matches!(arg, Expr::Ident(n) if n == param_name) {
                        let qualified = pure_fn_sidecar_key(lib, name);
                        if keys_map
                            .get(&qualified)
                            .map_or(false, |hits| hits.iter().any(|(idx, _)| *idx == i))
                        {
                            return true;
                        }
                        if lib.is_some()
                            && keys_map
                                .get(name)
                                .map_or(false, |hits| hits.iter().any(|(idx, _)| *idx == i))
                        {
                            return true;
                        }
                    }
                }
                args.iter().any(|a| walk(a, param_name, keys_map, lib))
            }
            Expr::BinOp(l, _, r) => {
                walk(l, param_name, keys_map, lib) || walk(r, param_name, keys_map, lib)
            }
            Expr::UnaryOp(_, e) => walk(e, param_name, keys_map, lib),
            Expr::FieldAccess(b, _) => walk(b, param_name, keys_map, lib),
            Expr::Index(b, k) => walk(b, param_name, keys_map, lib) || walk(k, param_name, keys_map, lib),
            Expr::If(c, t, e) => {
                walk(c, param_name, keys_map, lib)
                    || walk(t, param_name, keys_map, lib)
                    || e.as_ref().map_or(false, |x| walk(x, param_name, keys_map, lib))
            }
            Expr::Let(_, v, b) => walk(v, param_name, keys_map, lib) || walk(b, param_name, keys_map, lib),
            Expr::Block(items) => items.iter().any(|e| walk(e, param_name, keys_map, lib)),
            Expr::MethodCall(b, _, args) => {
                walk(b, param_name, keys_map, lib)
                    || args.iter().any(|a| walk(a, param_name, keys_map, lib))
            }
            Expr::MacroRef(_, args) | Expr::Tuple(args) => {
                args.iter().any(|a| walk(a, param_name, keys_map, lib))
            }
            Expr::RecordConstruct(_, fields) | Expr::RecordUpdate(_, fields) => fields
                .iter()
                .any(|(_, e)| walk(e, param_name, keys_map, lib)),
            Expr::Match(s, arms) => {
                walk(s, param_name, keys_map, lib)
                    || arms.iter().any(|a| walk(&a.body, param_name, keys_map, lib))
            }
            Expr::Closure(_, b) => walk(b, param_name, keys_map, lib),
            Expr::Cast(e, _) | Expr::Some(e) => walk(e, param_name, keys_map, lib),
            Expr::For(_, it, b) => {
                walk(it, param_name, keys_map, lib) || walk(b, param_name, keys_map, lib)
            }
            Expr::EnumVariantWithData(_, _, args) => {
                args.iter().any(|a| walk(a, param_name, keys_map, lib))
            }
            Expr::Range(s, e) => walk(s, param_name, keys_map, lib) || walk(e, param_name, keys_map, lib),
            Expr::NamespacedCall { args, .. } => args.iter().any(|a| walk(a, param_name, keys_map, lib)),
            Expr::AddressOf {
                args, with_params, ..
            } => {
                args.iter().any(|a| walk(a, param_name, keys_map, lib))
                    || with_params
                        .iter()
                        .any(|(_, e)| walk(e, param_name, keys_map, lib))
            }
            Expr::Encode { value, .. } => walk(value, param_name, keys_map, lib),
            _ => false,
        }
    }
    walk(body, param_name, keys_map, lib)
}

fn build_pure_fn_keys_params(program: &Program) -> HashMap<String, Vec<(usize, String)>> {
    let entries: Vec<(Option<String>, &PureFn)> = program
        .pure_fns
        .iter()
        .map(|pf| (None, pf))
        .chain(
            program
                .libraries
                .iter()
                .flat_map(|lib| lib.pure_fns.iter().map(|pf| (Some(lib.name.clone()), pf))),
        )
        .collect();

    let mut m = HashMap::new();
    for (lib, pf) in &entries {
        let hits = collect_pure_fn_keys_params_direct(pf);
        if !hits.is_empty() {
            m.insert(pure_fn_sidecar_key(lib.as_deref(), &pf.name), hits);
        }
    }

    let mut changed = true;
    while changed {
        changed = false;
        for (lib, pf) in &entries {
            let key = pure_fn_sidecar_key(lib.as_deref(), &pf.name);
            let mut hits = m.get(&key).cloned().unwrap_or_default();
            for (i, p) in pf.params.iter().enumerate() {
                if !is_mapping_type(&p.ty) || hits.iter().any(|(idx, _)| *idx == i) {
                    continue;
                }
                if param_uses_iteration(&pf.body, &p.name)
                    || param_passes_to_keys_callee(&pf.body, &p.name, &m, lib.as_deref())
                {
                    hits.push((i, p.name.clone()));
                    changed = true;
                }
            }
            if !hits.is_empty() {
                m.insert(key, hits);
            }
        }
    }
    m
}

fn pure_fn_sidecar_key(lib: Option<&str>, fn_name: &str) -> String {
    match lib {
        Some(l) => format!("{}::{}", l, fn_name),
        None => fn_name.to_string(),
    }
}
