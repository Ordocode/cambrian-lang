// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean codegen — Type → Lean type printer + default-value printer (P1.1).
//!
//! Mirrors `evm_types.rs` in spirit: a single entry point per concern,
//! routing primitives to BitVec and user-defined names to entity-scoped
//! identifiers (see [docs/PLAN_LEAN_TARGET.md](../../docs/PLAN_LEAN_TARGET.md) §P1.1).
//!
//! P1 collection types (`Vec<T>`, `HashMap<K,V>`, `Option<T>`) and any
//! other generics are *not* lowered here — they are intercepted upstream
//! by `validate::check_lean_target_compat` (rule `L1`). If one slips
//! through (e.g. inside a record we transitively pulled in), we fall
//! back to a `/- L1: <generic> -/ Cambrian.Unsupported` placeholder so
//! Lean refuses to build with a localized error rather than silently
//! coercing.

use super::super::LeanProfile;
use crate::ast::{EnumDecl, Program, Record, Type};

// Re-export kernel type-ordering (moved out of LeanCore in P2).
pub use crate::analysis::{order_type_items, TypeItem};

/// Fixed namespace under which program-scope (top-level, non-entity) user
/// types nest when the predictable emission profile is active (B6, spec
/// §15.11). The digest-zone definition (§2) already reserves the
/// `Cambrian` root, so `Cambrian.Generated.Types.*` falls inside the
/// pinned zone by construction (same as the `Cambrian.Generated.Dispatch`
/// opaques). Under legacy these types stay bare (byte-identical).
pub const PROGRAM_TYPES_NS: &str = "Cambrian.Generated.Types";

/// Namespace prefix (with trailing dot) to prepend to a bare program-scope
/// type / enum-variant reference under the active emission profile.
/// Escrow → `Cambrian.Generated.Types.`; legacy → empty string. Shared by
/// the type-position resolver here and the enum-literal emitter in
/// `lean_expr.rs` so both agree on the qualified spelling.
pub fn program_type_ns_prefix(use_predictable_profile: bool) -> String {
    if use_predictable_profile {
        format!("{}.", PROGRAM_TYPES_NS)
    } else {
        String::new()
    }
}

/// Type names the backend itself declares inside every entity namespace
/// (`structure State` / `structure Identity`) or spec namespace
/// (`inductive Action`). A bare user type with one of these names resolves
/// to the generated declaration instead.
const GENERATED_TYPE_NAMES: &[&str] = &["State", "Identity", "Action"];

/// Lean spelling of an entity-local record / enum name. A local type
/// cannot share a name with the entity's generated `State` / `Identity`
/// (same namespace), so those get a trailing underscore.
pub fn lean_local_type_name(name: &str) -> String {
    if name == "State" || name == "Identity" {
        format!("{}_", name)
    } else {
        name.to_string()
    }
}

/// Reference to a program-scope record / enum. Under the default profile
/// these live at the root namespace, so a name the backend also declares
/// per entity is written `_root_.Name`.
/// Whether a bare reference to `name` would be captured by a generated
/// declaration of the same name.
pub fn is_generated_type_name(name: &str) -> bool {
    GENERATED_TYPE_NAMES.contains(&name)
}

pub fn program_type_ref(name: &str, use_predictable_profile: bool) -> String {
    if !use_predictable_profile && GENERATED_TYPE_NAMES.contains(&name) {
        format!("_root_.{}", name)
    } else {
        format!("{}{}", program_type_ns_prefix(use_predictable_profile), name)
    }
}

/// Context passed to type lowering so that user-defined names resolve
/// inside the enclosing entity namespace when applicable. The enclosing
/// entity's locally-declared records / enums / aliases are name-shadowed
/// from program-level ones (matches the source language's lexical scope).
pub struct LeanTypeCtx<'a> {
    pub program: &'a Program,
    /// Whether proof-assistance codegen is enabled for this run.
    pub proof_helpers: bool,
    /// When true, unsigned primitives lower to `Nat` and signed to `Int`
    /// instead of `BitVec` (project `lean.numerics: nat`).
    pub use_nat_numerics: bool,
    /// When true, BitVec `+`/`-`/`*` lower via `Cambrian.checkedAdd` etc.
    /// (`lean.numerics: overflow-panic`).
    pub overflow_panic: bool,
    /// When true, the predictable emission profile is active (project
    /// `lean.emission_profile: predictable`). B2: gates dropping `Repr` from
    /// emitted deriving lists.
    pub use_predictable_profile: bool,
    pub deterministic_addresses: bool,
    /// Optional enclosing entity name. When `Some`, names that match a
    /// *local* record/enum/alias on that entity are resolved to
    /// `<Entity>.<Name>`; otherwise top-level names resolve bare.
    pub entity_name: Option<&'a str>,
    /// Local records of the enclosing entity (subset of `program.records`).
    pub local_records: &'a [Record],
    /// Local enums of the enclosing entity (subset of `program.enums`).
    pub local_enums: &'a [EnumDecl],
    /// Local type aliases of the enclosing entity.
    pub local_aliases: &'a [crate::ast::TypeAlias],
}

impl<'a> LeanTypeCtx<'a> {
    /// Build a context with no enclosing entity (used for top-level
    /// pure fns / records / enums).
    pub fn top_level(program: &'a Program, profile: LeanProfile) -> Self {
        LeanTypeCtx {
            program,
            proof_helpers: profile.proof_helpers,
            use_nat_numerics: profile.nat_numerics,
            overflow_panic: profile.overflow_panic,
            use_predictable_profile: profile.predictable,
            deterministic_addresses: profile.deterministic_addresses,
            entity_name: None,
            local_records: &[],
            local_enums: &[],
            local_aliases: &[],
        }
    }

    /// Build a context for the named entity. Caller passes the locally
    /// declared records/enums/aliases (we don't resolve them here so
    /// the caller can compose them with project-merging later).
    pub fn for_entity(
        program: &'a Program,
        entity_name: &'a str,
        local_records: &'a [Record],
        local_enums: &'a [EnumDecl],
        local_aliases: &'a [crate::ast::TypeAlias],
        profile: LeanProfile,
    ) -> Self {
        LeanTypeCtx {
            program,
            proof_helpers: profile.proof_helpers,
            use_nat_numerics: profile.nat_numerics,
            overflow_panic: profile.overflow_panic,
            use_predictable_profile: profile.predictable,
            deterministic_addresses: profile.deterministic_addresses,
            entity_name: Some(entity_name),
            local_records,
            local_enums,
            local_aliases,
        }
    }

    pub fn with_nat_numerics(mut self, on: bool) -> Self {
        self.use_nat_numerics = on;
        self
    }

    /// Recover the [`LeanProfile`] this context was seeded with.
    pub fn profile(&self) -> LeanProfile {
        LeanProfile {
            proof_helpers: self.proof_helpers,
            nat_numerics: self.use_nat_numerics,
            overflow_panic: self.overflow_panic,
            predictable: self.use_predictable_profile,
            deterministic_addresses: self.deterministic_addresses,
        }
    }
}

/// Lower a Cambrian `Type` to a Lean type expression string.
///
/// Returns the Lean source for the type. Always produces *something* —
/// unsupported shapes evaluate to a self-documenting placeholder so the
/// resulting Lean file still parses up to the point of use, where Lean
/// will fail with a clean error pointing at the bad type. Callers that
/// want hard upfront rejection should run the validator first.
pub fn lower_type(ty: &Type, ctx: &LeanTypeCtx<'_>) -> String {
    match ty {
        Type::Simple(name) => lower_simple(name, ctx),
        Type::Generic(name, params) => lower_generic(name, params, ctx),
        Type::Tuple(items) => lower_tuple(items, ctx),
        Type::TypedAddress(_) => "Cambrian.Address".to_string(),
    }
}

/// True when `name` is a Cambrian signed integer primitive (`i8`…`i128`).
pub fn is_signed_primitive(name: &str) -> bool {
    matches!(name, "i8" | "i16" | "i32" | "i64" | "i128" | "isize")
}

/// True when `ty` resolves to a signed integer primitive (after alias inline).
pub fn type_is_signed(ty: &Type, ctx: &LeanTypeCtx<'_>) -> bool {
    match ty {
        Type::Simple(name) => {
            if is_signed_primitive(name) {
                return true;
            }
            if let Some(alias) = ctx
                .local_aliases
                .iter()
                .find(|a| a.name == *name)
                .or_else(|| ctx.program.type_aliases.iter().find(|a| a.name == *name))
            {
                return type_is_signed(&alias.ty, ctx);
            }
            false
        }
        _ => false,
    }
}

fn lower_simple(name: &str, ctx: &LeanTypeCtx<'_>) -> String {
    if ctx.use_nat_numerics {
        return match name {
            "bool" => "Bool".to_string(),
            "i8" | "i16" | "i32" | "i64" | "i128" | "isize" => "Int".to_string(),
            // `bytes32` is a full machine word, so the Lean model gives it
            // the same carrier as `U256`. `bytes4` cannot: a selector is a
            // four-byte tag compared for equality, and widening it would make
            // two different selectors equal after truncation.
            "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "U256" | "uint256" | "bytes32" => {
                "Nat".to_string()
            }
            "address" => "Cambrian.Address".to_string(),
            "pubkey" => "Cambrian.Pubkey".to_string(),
            // A selector is four bytes wide by definition — it is a tag, not a
            // quantity — so it stays fixed-width under `numerics: nat` for the
            // same reason `address` does.
            "bytes4" => "BitVec 32".to_string(),
            "String" | "string" => "String".to_string(),
            "bytes" | "CamData" => "Cambrian.Bytes".to_string(),
            other => resolve_user_type(other, ctx),
        };
    }
    match name {
        "bool" => "Bool".to_string(),
        "u8" => "BitVec 8".to_string(),
        "u16" => "BitVec 16".to_string(),
        "u32" => "BitVec 32".to_string(),
        "u64" => "BitVec 64".to_string(),
        "u128" => "BitVec 128".to_string(),
        "i8" => "BitVec 8".to_string(),
        "i16" => "BitVec 16".to_string(),
        "i32" => "BitVec 32".to_string(),
        "i64" => "BitVec 64".to_string(),
        "i128" => "BitVec 128".to_string(),
        "usize" => "Nat".to_string(),
        "U256" | "uint256" | "bytes32" => "Cambrian.U256".to_string(),
        "address" => "Cambrian.Address".to_string(),
        "pubkey" => "Cambrian.Pubkey".to_string(),
        "bytes4" => "BitVec 32".to_string(),
        "String" | "string" => "String".to_string(),
        "bytes" | "CamData" => "Cambrian.Bytes".to_string(),
        other => resolve_user_type(other, ctx),
    }
}

/// Resolve a user-defined type name against the local entity scope and
/// then the program scope. Type aliases are inlined (lowered to the
/// alias target) so that downstream code never sees an alias name.
fn resolve_user_type(name: &str, ctx: &LeanTypeCtx<'_>) -> String {
    if let Some(alias) = ctx.local_aliases.iter().find(|a| a.name == name) {
        return lower_type(&alias.ty, ctx);
    }
    if let Some(alias) = ctx.program.type_aliases.iter().find(|a| a.name == name) {
        return lower_type(&alias.ty, ctx);
    }

    if ctx.local_records.iter().any(|r| r.name == name)
        || ctx.local_enums.iter().any(|e| e.name == name)
    {
        match ctx.entity_name {
            Some(entity) => format!("{}.{}", entity, lean_local_type_name(name)),
            None => lean_local_type_name(name),
        }
    } else if ctx.program.records.iter().any(|r| r.name == name)
        || ctx.program.enums.iter().any(|e| e.name == name)
    {
        // Program-scope (top-level, non-entity) user type. Under predictable profile
        // (B6) it nests under `Cambrian.Generated.Types`; under legacy it
        // stays bare (byte-identical).
        program_type_ref(name, ctx.use_predictable_profile)
    } else {
        // Unknown identifier: emit it bare and let Lean report the
        // missing definition. Validator should catch most of these
        // (the bare name escaped without resolving).
        name.to_string()
    }
}

fn lower_generic(name: &str, params: &[Type], ctx: &LeanTypeCtx<'_>) -> String {
    match name {
        "Vec" if params.len() == 1 => {
            format!("List ({})", lower_type(&params[0], ctx))
        }
        "HashMap" if params.len() == 2 => {
            format!(
                "Cambrian.AddressMap ({}) ({})",
                lower_type(&params[0], ctx),
                lower_type(&params[1], ctx),
            )
        }
        "Option" if params.len() == 1 => {
            format!("Option ({})", lower_type(&params[0], ctx))
        }
        _ => {
            let inner: Vec<String> = params.iter().map(|t| lower_type(t, ctx)).collect();
            format!(
                "/- unsupported generic '{}<{}>' -/ Cambrian.Unsupported",
                name,
                inner.join(", "),
            )
        }
    }
}

fn lower_tuple(items: &[Type], ctx: &LeanTypeCtx<'_>) -> String {
    if items.is_empty() {
        // Empty tuple = unit-equivalent. Lean has Unit.
        return "Unit".to_string();
    }
    if items.len() == 1 {
        // 1-tuple is just the inner type (matches Cambrian semantics).
        return lower_type(&items[0], ctx);
    }
    let parts: Vec<String> = items.iter().map(|t| lower_type(t, ctx)).collect();
    format!("({})", parts.join(" × "))
}

/// Build a Lean *value* expression that produces the canonical default
/// for `ty`. Matches the implicit defaults Cambrian assigns to
/// non-identity members declared without a `=` clause. Used by
/// `lean_entity::gen_state_default` to populate `State.default`.
///
/// Unknown shapes fall back to a self-documenting placeholder so the
/// generated file still pretty-prints — Lean will fail at use-site if
/// the placeholder is ever evaluated.
pub fn default_for_type(ty: &Type, ctx: &LeanTypeCtx<'_>) -> String {
    match ty {
        Type::Simple(name) => default_for_simple(name, ctx),
        Type::Generic(name, params) => match name.as_str() {
            "Vec" if params.len() == 1 => "[]".to_string(),
            "HashMap" if params.len() == 2 => "Cambrian.AddressMap.empty".to_string(),
            "Option" if params.len() == 1 => "Option.none".to_string(),
            _ => "Cambrian.Unsupported.default".to_string(),
        },
        Type::Tuple(items) => {
            if items.is_empty() {
                return "()".to_string();
            }
            if items.len() == 1 {
                return default_for_type(&items[0], ctx);
            }
            let parts: Vec<String> = items.iter().map(|t| default_for_type(t, ctx)).collect();
            format!("({})", parts.join(", "))
        }
        Type::TypedAddress(_) => "0#160".to_string(),
    }
}

fn default_for_simple(name: &str, ctx: &LeanTypeCtx<'_>) -> String {
    if ctx.use_nat_numerics {
        match name {
            "bool" => return "false".to_string(),
            "i8" | "i16" | "i32" | "i64" | "i128" | "isize" => return "(0 : Int)".to_string(),
            "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "U256" | "uint256" | "bytes32" => {
                return "0".to_string()
            }
            "address" => return "0#160".to_string(),
            "pubkey" => return "0#256".to_string(),
            "bytes4" => return "0#32".to_string(),
            "String" | "string" => return "\"\"".to_string(),
            "bytes" | "CamData" => return "Cambrian.Bytes.empty".to_string(),
            _ => {}
        }
    }
    match name {
        "bool" => "false".to_string(),
        "u8" | "i8" => "0#8".to_string(),
        "u16" | "i16" => "0#16".to_string(),
        "u32" | "i32" => "0#32".to_string(),
        "u64" | "i64" => "0#64".to_string(),
        "u128" | "i128" => "0#128".to_string(),
        "usize" => "0".to_string(),
        "U256" | "uint256" | "bytes32" => "0#256".to_string(),
        "address" => "0#160".to_string(),
        "pubkey" => "0#256".to_string(),
        "bytes4" => "0#32".to_string(),
        "String" | "string" => "\"\"".to_string(),
        "bytes" | "CamData" => "Cambrian.Bytes.empty".to_string(),
        other => default_for_user_type(other, ctx),
    }
}

fn default_for_user_type(name: &str, ctx: &LeanTypeCtx<'_>) -> String {
    if let Some(alias) = ctx
        .local_aliases
        .iter()
        .find(|a| a.name == name)
        .or_else(|| ctx.program.type_aliases.iter().find(|a| a.name == name))
    {
        return default_for_type(&alias.ty, ctx);
    }

    if let Some(rec) = ctx
        .local_records
        .iter()
        .find(|r| r.name == name)
        .or_else(|| ctx.program.records.iter().find(|r| r.name == name))
    {
        let qualified = match ctx.entity_name {
            Some(entity) if ctx.local_records.iter().any(|r| r.name == name) => {
                format!("{}.{}", entity, lean_local_type_name(name))
            }
            // Program-scope record: escrow (B6) nests under
            // `Cambrian.Generated.Types`; legacy stays bare.
            _ => program_type_ref(name, ctx.use_predictable_profile),
        };
        if rec.fields.is_empty() {
            return format!("({} :=)", qualified); // ill-shaped but never reached for empty records
        }
        let field_inits: Vec<String> = rec
            .fields
            .iter()
            .map(|f| format!("{} := {}", lean_safe_ident(&f.name), default_for_type(&f.ty, ctx)))
            .collect();
        return format!("({{ {} }} : {})", field_inits.join(", "), qualified);
    }

    if let Some(en) = ctx
        .local_enums
        .iter()
        .find(|e| e.name == name)
        .or_else(|| ctx.program.enums.iter().find(|e| e.name == name))
    {
        let qualified = match ctx.entity_name {
            Some(entity) if ctx.local_enums.iter().any(|e| e.name == name) => {
                format!("{}.{}", entity, lean_local_type_name(name))
            }
            // Program-scope enum: escrow (B6) nests under
            // `Cambrian.Generated.Types`; legacy stays bare.
            _ => program_type_ref(name, ctx.use_predictable_profile),
        };
        let Some(first) = en.variants.first() else {
            // Empty enum: not constructable. Emit a self-documenting
            // placeholder; Lean will reject when it's used.
            return format!("(panic! \"empty enum {}\")", qualified);
        };
        if first.fields.is_empty() {
            return format!("{}.{}", qualified, sanitize_variant(&first.name));
        }
        let parts: Vec<String> = first
            .fields
            .iter()
            .map(|t| default_for_type(t, ctx))
            .collect();
        return format!(
            "{}.{} {}",
            qualified,
            sanitize_variant(&first.name),
            parts.join(" "),
        );
    }

    // Unknown — emit a placeholder that is obviously broken. Validator
    // should have caught this upstream.
    format!("(/- unknown type '{}' in default -/ default)", name)
}

/// Lower an enum variant constructor name. Lean enforces that
/// constructor names start with a lower-case letter when written in
/// dot-notation (e.g. `MyEnum.foo`). Source variants conventionally
/// use `PascalCase`, so we lower-case the leading character.
pub fn sanitize_variant(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => {
            let mut out = String::with_capacity(name.len());
            for c in first.to_lowercase() {
                out.push(c);
            }
            out.push_str(chars.as_str());
            out
        }
        None => name.to_string(),
    }
}

/// Lean 4 keywords that cannot appear as bare structure-field or binder
/// names in generated code (e.g. `Permission.end` breaks parsing).
fn is_lean_reserved_keyword(name: &str) -> bool {
    matches!(
        name,
        "end" | "namespace"
            | "section"
            | "import"
            | "open"
            | "export"
            | "public"
            | "private"
            | "protected"
            | "variable"
            | "def"
            | "theorem"
            | "example"
            | "axiom"
            | "inductive"
            | "structure"
            | "class"
            | "instance"
            | "abbrev"
            | "opaque"
            | "mutual"
            | "where"
            | "let"
            | "in"
            | "if"
            | "then"
            | "else"
            | "match"
            | "with"
            | "fun"
            | "do"
            | "return"
            | "for"
            | "by"
            | "have"
            | "show"
            | "sorry"
            | "admit"
            | "partial"
            | "unsafe"
            | "meta"
            | "noncomputable"
            | "infix"
            | "postfix"
            | "prefix"
            | "notation"
            | "macro"
            | "elab"
            | "syntax"
            | "derive"
            | "aux"
            | "builtin"
    )
}

/// Map a Cambrian route-param / let / capture identifier to a Lean
/// binder that doesn't collide with the codegen's reserved names
/// (`s` = state, `w` = world, `inst` = identity, `ctx` = message
/// context) or Lean keywords (`end`, `structure`, …).
///
/// Codegen binders append `_p` (for "param"); Lean keywords append `_`.
pub fn lean_safe_ident(name: &str) -> String {
    match name {
        "s" | "w" | "inst" | "ctx" => format!("{}_p", name),
        _ if is_lean_reserved_keyword(name) => format!("{}_", name),
        _ => name.to_string(),
    }
}

/// Like [`lean_safe_ident`], but leaves the fire-and-forget binder `_` alone.
pub fn lean_safe_bind(name: &str) -> String {
    if name == "_" {
        "_".to_string()
    } else {
        lean_safe_ident(name)
    }
}

/// Quote a Lean string literal, escaping `\` and `"` only. P1's
/// `String` literals are user-supplied and never embedded into types,
/// so we keep the escape table minimal.
pub fn lean_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Program, Span, Type, TypeAlias};

    fn empty_program() -> Program {
        Program {
            imports: vec![],
            file_imports: vec![],
            pure_fns: vec![],
            type_aliases: vec![],
            records: vec![],
            enums: vec![],
            entities: vec![],
            extern_entities: vec![],
            tests: vec![],
            properties: vec![],
            fuzz_tests: vec![],
            invariants: vec![],
            events: vec![],
            errors: vec![],
            libraries: vec![],
            using_decls: vec![],
        }
    }

    #[test]
    fn primitives_lower_to_bitvec() {
        let p = empty_program();
        let ctx = LeanTypeCtx::top_level(&p, LeanProfile::DEFAULT);
        assert_eq!(lower_type(&Type::Simple("u64".into()), &ctx), "BitVec 64");
        assert_eq!(lower_type(&Type::Simple("u128".into()), &ctx), "BitVec 128");
        assert_eq!(lower_type(&Type::Simple("i32".into()), &ctx), "BitVec 32");
        assert_eq!(
            lower_type(&Type::Simple("U256".into()), &ctx),
            "Cambrian.U256"
        );
        assert_eq!(
            lower_type(&Type::Simple("uint256".into()), &ctx),
            "Cambrian.U256"
        );
        assert_eq!(
            lower_type(&Type::Simple("address".into()), &ctx),
            "Cambrian.Address"
        );
        assert_eq!(
            lower_type(&Type::Simple("pubkey".into()), &ctx),
            "Cambrian.Pubkey"
        );
        assert_eq!(lower_type(&Type::Simple("bool".into()), &ctx), "Bool");
        assert_eq!(lower_type(&Type::Simple("String".into()), &ctx), "String");
        assert_eq!(
            lower_type(&Type::Simple("bytes".into()), &ctx),
            "Cambrian.Bytes"
        );
    }

    #[test]
    fn nat_mode_signed_lowers_to_int() {
        let p = empty_program();
        let ctx = LeanTypeCtx::top_level(&p, LeanProfile::DEFAULT).with_nat_numerics(true);
        assert_eq!(lower_type(&Type::Simple("u64".into()), &ctx), "Nat");
        assert_eq!(lower_type(&Type::Simple("U256".into()), &ctx), "Nat");
        assert_eq!(lower_type(&Type::Simple("i32".into()), &ctx), "Int");
        assert_eq!(lower_type(&Type::Simple("i64".into()), &ctx), "Int");
        assert_eq!(default_for_type(&Type::Simple("i8".into()), &ctx), "(0 : Int)");
        assert_eq!(default_for_type(&Type::Simple("u8".into()), &ctx), "0");
        assert!(type_is_signed(&Type::Simple("i16".into()), &ctx));
        assert!(!type_is_signed(&Type::Simple("u16".into()), &ctx));
    }

    #[test]
    fn typed_address_lowers_to_address() {
        let p = empty_program();
        let ctx = LeanTypeCtx::top_level(&p, LeanProfile::DEFAULT);
        assert_eq!(
            lower_type(&Type::TypedAddress("Vault".into()), &ctx),
            "Cambrian.Address"
        );
    }

    #[test]
    fn tuple_lowers_to_product() {
        let p = empty_program();
        let ctx = LeanTypeCtx::top_level(&p, LeanProfile::DEFAULT);
        let ty = Type::Tuple(vec![
            Type::Simple("u8".into()),
            Type::Simple("U256".into()),
            Type::Simple("bool".into()),
        ]);
        assert_eq!(lower_type(&ty, &ctx), "(BitVec 8 × Cambrian.U256 × Bool)");
    }

    #[test]
    fn type_alias_inlines_target() {
        let mut p = empty_program();
        p.type_aliases.push(TypeAlias {
            name: "TokenId".into(),
            ty: Type::Simple("u64".into()),
            span: Span::none(),
        });
        let ctx = LeanTypeCtx::top_level(&p, LeanProfile::DEFAULT);
        assert_eq!(
            lower_type(&Type::Simple("TokenId".into()), &ctx),
            "BitVec 64"
        );
    }

    #[test]
    fn unknown_generic_emits_l1_placeholder() {
        let p = empty_program();
        let ctx = LeanTypeCtx::top_level(&p, LeanProfile::DEFAULT);
        // `Vec` / `HashMap` / `Option` are lifted (P4); any *other*
        // generic name should still hit the L1 placeholder.
        let ty = Type::Generic("MyOddGeneric".into(), vec![Type::Simple("u8".into())]);
        let s = lower_type(&ty, &ctx);
        assert!(
            s.contains("Cambrian.Unsupported"),
            "expected Unsupported marker in {}",
            s
        );
        assert!(s.contains("MyOddGeneric"), "expected generic name in {}", s);
    }

    #[test]
    fn defaults_match_primitives() {
        let p = empty_program();
        let ctx = LeanTypeCtx::top_level(&p, LeanProfile::DEFAULT);
        assert_eq!(default_for_type(&Type::Simple("u64".into()), &ctx), "0#64");
        assert_eq!(
            default_for_type(&Type::Simple("address".into()), &ctx),
            "0#160"
        );
        assert_eq!(
            default_for_type(&Type::Simple("U256".into()), &ctx),
            "0#256"
        );
        assert_eq!(
            default_for_type(&Type::Simple("bool".into()), &ctx),
            "false"
        );
    }

    #[test]
    fn sanitize_variant_lowercases_first() {
        assert_eq!(sanitize_variant("Created"), "created");
        assert_eq!(sanitize_variant("STATE_FOO"), "sTATE_FOO");
        assert_eq!(sanitize_variant(""), "");
    }

    #[test]
    fn lean_safe_ident_escapes_codegen_and_lean_reserved_names() {
        assert_eq!(lean_safe_ident("s"), "s_p");
        assert_eq!(lean_safe_ident("end"), "end_");
        assert_eq!(lean_safe_ident("start"), "start");
        assert_eq!(lean_safe_ident("structure"), "structure_");
    }
}
