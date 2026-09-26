// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! B7 — escrow name validator (Phase B, `onchain/CAMBRIAN_R3_SPEC.md` §7
//! item B7, §15.9, §15.11).
//!
//! The escrow digest (R3) partitions the exported NDJSON into a **zone** whose
//! roots are the entity names plus the `Cambrian` framework namespace (spec
//! §2). Soundness of that partition relies on three naming invariants that the
//! honest emitter upholds by construction but an adversarial `.cam` author
//! could violate:
//!
//!   * no zone root shadows the `Cambrian` framework namespace or a Lean core
//!     namespace the generated model references by name (`List`, `Nat`, …);
//!   * no zone root is a **dot-component prefix** of another — the classifier
//!     matches roots component-wise (`Foo` is *not* a prefix of `FooBar`, but
//!     `Foo` *is* a prefix of `Foo.Bar`), so nested roots would mis-partition
//!     the zone (§2, §15.9 double-slug naming);
//!   * no user identifier collides with a declaration the emitter generates
//!     into the zone (the per-entity `State` / `Identity` / `address` / … set,
//!     the B1 `.statement` abbrev leaf, the B4 `__cbr_` fresh binders).
//!
//! This module is the fail-fast, human-diagnosable counterpart of the
//! fail-closed digest mismatch: an honest prover whose `.cam` trips one of
//! these would otherwise only discover it as an inscrutable `defs_digest`
//! disagreement (or broken `lake build`) rather than a named error.
//!
//! ## Gating
//!
//! Every rule except the `__cbr_` prefix reservation (`N4`) is gated on the
//! `escrow` emission profile: the zone/collision hazards only exist for the
//! escrow Lean emission, and reserving ordinary words (`List`, `State`, …)
//! unconditionally would reject legitimate legacy projects (including non-Lean
//! targets that never emit these declarations). `N4` is unconditional: no
//! human writes a `__cbr_`-prefixed identifier, so reserving it can never
//! reject a real program and it is forward-safe for the B4 binder scheme.

use super::Diagnostic;
use crate::ast::*;

// ---------------------------------------------------------------------------
// Reserved-name inventories (documented consts)
// ---------------------------------------------------------------------------

/// The Cambrian framework namespace root. Every generated declaration that is
/// not under an entity root lives under `Cambrian.*` (Prelude, `Generated.*`,
/// `Address`, `U256`, …), so an entity/top-level type named `Cambrian` would
/// mint a second, colliding zone root (spec §2; attack table §10 "Entity с
/// именем `List`/`Cambrian`").
pub const CAMBRIAN_ROOT: &str = "Cambrian";

/// Root (first dot-component) identifiers of Lean **core** namespaces that the
/// escrow model layer references by fully-qualified name. An `entity` or
/// top-level type whose name equals one of these would emit `<Name>.State`
/// (entity) or `Cambrian.Generated.Types.<Name>` (top-level) declarations that
/// shadow — and, in file scope, collide with — the core namespace the emission
/// depends on.
///
/// Provenance: every root here is emitted by the Lean backend (grepped from
/// the emitters, not assumed):
///   * `lean_types.rs::lower_type` primitive map — `Bool`, `Nat`, `BitVec`,
///     `String`, `Unit`, `List`, `Option`, and `Prod` (the `×` product used
///     for tuples / `World × T` view returns);
///   * route / invariant / world machinery — `Except` (`RouteResult α :=
///     Except ThrowCode α`), `Id`, `Fin` (invariant `sender_idx`), `Eq` / `Iff`
///     / `And` / `Or` / `Not` / `True` / `False` (statement bodies and
///     invariant match arms), and `Decidable` (`ite` / derived `DecidableEq`).
///
/// Deliberately excludes typeclass operator roots (`HAdd`, `GE`, `OfNat`, …):
/// those are elaborator-synthesised, never written as an entity name, and
/// reserving them would be noise. `Cambrian`-tier roots are covered by
/// [`CAMBRIAN_ROOT`].
pub const CORE_NAMESPACE_ROOTS: &[&str] = &[
    // lean_types.rs::lower_type primitive map
    "Bool",
    "Nat",
    "BitVec",
    "String",
    "Unit",
    "List",
    "Option",
    "Prod",
    // route / invariant / world machinery
    "Except",
    "Id",
    "Fin",
    "Eq",
    "Iff",
    "And",
    "Or",
    "Not",
    "True",
    "False",
    "Decidable",
];

/// Per-entity declarations the Lean backend emits **directly** under the
/// entity namespace `<E>.<name>` (or opens as a sub-namespace `<E>.<name>.*`).
/// A user member / entity-local type / constant of the same name collides with
/// the generated declaration. Each tuple is `(reserved_name, what_it_collides
/// _with)` for the error message + audit trail.
///
/// Inventory rationale (why exactly these six — the task's candidate list also
/// named `Action` / `step` / `runTrace` / `senders`, which are **excluded**):
///   * `State`    — `structure <E>.State`            (lean_entity.rs::emit_state_struct)
///   * `Identity` — `structure <E>.Identity`         (lean_entity.rs::emit_identity_struct)
///   * `address`  — `def <E>.address`                (lean_address.rs::emit_address)
///   * `Routes`   — `namespace <E>.Routes`           (lean_route.rs)
///   * `Members`  — `namespace <E>.Members`          (lean_member.rs)
///   * `Spec`     — `namespace <E>.Spec`             (lean_spec/lean_invariant.rs)
///
/// `Action` / `step` / `runTrace` / `senders` are emitted **one level deeper**,
/// under `<E>.Spec.Invariants.<slug>.*` (spec §15.9: they are *siblings* of the
/// invariant theorem inside its `namespace <slug>`), NOT under `<E>.<name>` —
/// so an entity member/type of those names does not collide with them.
/// Reserving them would be over-blocking, so they are omitted (see report).
pub const RESERVED_GENERATED_NAMES: &[(&str, &str)] = &[
    (
        "State",
        "the generated `<E>.State` state structure (lean_entity.rs::emit_state_struct)",
    ),
    (
        "Identity",
        "the generated `<E>.Identity` identity structure (lean_entity.rs::emit_identity_struct)",
    ),
    (
        "address",
        "the generated `<E>.address` CREATE2-address def (lean_address.rs::emit_address)",
    ),
    (
        "Routes",
        "the generated `<E>.Routes` route namespace (lean_route.rs)",
    ),
    (
        "Members",
        "the generated `<E>.Members` member-transform namespace (lean_member.rs)",
    ),
    (
        "Spec",
        "the generated `<E>.Spec` test/property/invariant namespace (lean_spec/lean_invariant.rs)",
    ),
];

/// B1 (§7 item B1, local_changes §8): statement abbrevs are named
/// `<theorem>.statement`, relying on `statement` never being the leaf of a
/// user declaration. Reserve the leaf so the scheme stays collision-free.
const RESERVED_LEAF_STATEMENT: &str = "statement";

/// B4 (§7 item B4, local_changes §7): the route-body rewriter mints fresh
/// tuple-destructuring binders `__cbr_pN`. Reserve the prefix so a user
/// identifier can never be shadowed by (or shadow) one.
const RESERVED_BINDER_PREFIX: &str = "__cbr_";

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the B7 name validator. `escrow` gates the zone/collision rules (`N1`,
/// `N2`, `N3`, `N5`, `N6`); the `__cbr_` binder-prefix reservation (`N4`) is
/// unconditional. Returns hard-error diagnostics only.
pub fn check_reserved_names(program: &Program, predictable: bool) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    // N4 — unconditional (upstream-safe): reserved `__cbr_` binder prefix.
    check_binder_prefix(program, &mut diags);

    if !predictable {
        return diags;
    }

    // N1 — entity name ∉ {Cambrian} ∪ core roots.
    check_entity_names(program, &mut diags);
    // N6 — top-level type name ∉ {Cambrian} ∪ core roots ∪ generated names.
    check_top_level_type_names(program, &mut diags);
    // N2 — zone-root (entity ∪ top-level type) component-wise prefix freedom.
    check_prefix_freedom(program, &mut diags);
    // N3 — reserved `statement` leaf.
    check_statement_leaf(program, &mut diags);
    // N5 — reserved generated declaration names as entity member/type/const.
    check_generated_name_collisions(program, &mut diags);

    diags
}

// ---------------------------------------------------------------------------
// Reserved-root helpers (shared by N1 / N6)
// ---------------------------------------------------------------------------

/// If `name` is a reserved zone root, return a human-readable reason.
fn reserved_root_reason(name: &str) -> Option<&'static str> {
    if name == CAMBRIAN_ROOT {
        Some("the Cambrian framework namespace")
    } else if CORE_NAMESPACE_ROOTS.contains(&name) {
        Some("a Lean core namespace the generated model references by name")
    } else {
        None
    }
}

/// If `name` collides with a per-entity generated declaration, return what.
fn reserved_generated_reason(name: &str) -> Option<&'static str> {
    RESERVED_GENERATED_NAMES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, what)| *what)
}

// ---------------------------------------------------------------------------
// N1 — entity names
// ---------------------------------------------------------------------------

fn check_entity_names(program: &Program, diags: &mut Vec<Diagnostic>) {
    for e in &program.entities {
        if let Some(reason) = reserved_root_reason(&e.name) {
            diags.push(Diagnostic::error(
                "N1",
                format!(
                    "entity name '{}' is reserved: it collides with {}. Under the escrow \
                     profile the zone root '{}' would shadow it (spec §2/§10). Rename the entity.",
                    e.name, reason, e.name,
                ),
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// N6 — top-level (program-scope) type names
// ---------------------------------------------------------------------------

/// Program-scope (top-level) type declarations: `(name, kind-label)`.
fn top_level_type_names(program: &Program) -> Vec<(&str, &'static str)> {
    let mut out: Vec<(&str, &'static str)> = Vec::new();
    for r in &program.records {
        out.push((r.name.as_str(), "record"));
    }
    for en in &program.enums {
        out.push((en.name.as_str(), "enum"));
    }
    for ta in &program.type_aliases {
        out.push((ta.name.as_str(), "type alias"));
    }
    out
}

fn check_top_level_type_names(program: &Program, diags: &mut Vec<Diagnostic>) {
    for (name, kind) in top_level_type_names(program) {
        if let Some(reason) = reserved_root_reason(name) {
            diags.push(Diagnostic::error(
                "N6",
                format!(
                    "top-level {kind} '{name}' is reserved: it collides with {reason}. Its escrow \
                     emission `Cambrian.Generated.Types.{name}` (spec §15.11) would shadow the core \
                     namespace. Rename the type.",
                ),
            ));
        } else if reserved_generated_reason(name).is_some() {
            diags.push(Diagnostic::error(
                "N6",
                format!(
                    "top-level {kind} '{name}' is reserved: '{name}' is a per-entity framework \
                     structural name generated inside every entity namespace; using it as a \
                     top-level type name is disallowed to keep the zone vocabulary unambiguous. \
                     Rename the type.",
                ),
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// N2 — component-wise prefix freedom of zone roots
// ---------------------------------------------------------------------------

fn components(name: &str) -> Vec<&str> {
    name.split('.').collect()
}

/// True when `short` is a (possibly-equal) dot-component prefix of `long`.
fn is_component_prefix(short: &[&str], long: &[&str]) -> bool {
    short.len() <= long.len() && short.iter().zip(long.iter()).all(|(a, b)| a == b)
}

fn check_prefix_freedom(program: &Program, diags: &mut Vec<Diagnostic>) {
    // Zone roots = entity names ∪ top-level type names. Entity/type names are
    // single-component today, so the practical bite is duplicate / shared
    // names; the general component-wise check is future-proof for any
    // multi-component name.
    let mut roots: Vec<(String, &'static str)> = Vec::new();
    for e in &program.entities {
        roots.push((e.name.clone(), "entity"));
    }
    for (name, kind) in top_level_type_names(program) {
        roots.push((
            name.to_string(),
            match kind {
                "record" => "top-level record",
                "enum" => "top-level enum",
                _ => "top-level type alias",
            },
        ));
    }

    for i in 0..roots.len() {
        for j in (i + 1)..roots.len() {
            let (a, ak) = &roots[i];
            let (b, bk) = &roots[j];
            if a == b {
                diags.push(Diagnostic::error(
                    "N2",
                    format!(
                        "{ak} '{a}' and {bk} '{b}' share the same zone-root name; entity and \
                         top-level type names must be pairwise distinct (spec §2 zone partition).",
                    ),
                ));
                continue;
            }
            let ca = components(a);
            let cb = components(b);
            if is_component_prefix(&ca, &cb) {
                diags.push(Diagnostic::error(
                    "N2",
                    format!(
                        "{ak} '{a}' is a dot-component prefix of {bk} '{b}': the predictable zone \
                         classifier matches roots component-wise (§2/§15.9) and would mis-nest \
                         '{b}' under '{a}'. Rename one of them.",
                    ),
                ));
            } else if is_component_prefix(&cb, &ca) {
                diags.push(Diagnostic::error(
                    "N2",
                    format!(
                        "{bk} '{b}' is a dot-component prefix of {ak} '{a}': the predictable zone \
                         classifier matches roots component-wise (§2/§15.9) and would mis-nest \
                         '{a}' under '{b}'. Rename one of them.",
                    ),
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// N3 / N4 — user-identifier leaf reservations
// ---------------------------------------------------------------------------

/// Collect every user-authored leaf identifier of the kinds B7 rule 3 names —
/// route / member / param / field / type / variant — each paired with a
/// human-readable context for the diagnostic.
fn collect_user_identifiers(program: &Program) -> Vec<(String, String)> {
    let mut ids: Vec<(String, String)> = Vec::new();

    let push = |name: &str, ctx: String, ids: &mut Vec<(String, String)>| {
        ids.push((name.to_string(), ctx));
    };

    // Program-scope types (record/enum/type-alias) + their fields/variants.
    for r in &program.records {
        push(&r.name, format!("top-level record '{}'", r.name), &mut ids);
        for f in &r.fields {
            push(
                &f.name,
                format!("field '{}' of top-level record '{}'", f.name, r.name),
                &mut ids,
            );
        }
    }
    for en in &program.enums {
        push(&en.name, format!("top-level enum '{}'", en.name), &mut ids);
        for v in &en.variants {
            push(
                &v.name,
                format!("variant '{}' of top-level enum '{}'", v.name, en.name),
                &mut ids,
            );
        }
    }
    for ta in &program.type_aliases {
        push(
            &ta.name,
            format!("top-level type alias '{}'", ta.name),
            &mut ids,
        );
    }

    // Program-scope pure functions (name + params).
    for pf in &program.pure_fns {
        push(&pf.name, format!("pure fn '{}'", pf.name), &mut ids);
        for p in &pf.params {
            push(
                &p.name,
                format!("parameter '{}' of pure fn '{}'", p.name, pf.name),
                &mut ids,
            );
        }
    }

    for e in &program.entities {
        let en = &e.name;
        // Entity-local types.
        for r in &e.records {
            push(
                &r.name,
                format!("record '{}' in entity '{}'", r.name, en),
                &mut ids,
            );
            for f in &r.fields {
                push(
                    &f.name,
                    format!(
                        "field '{}' of record '{}' in entity '{}'",
                        f.name, r.name, en
                    ),
                    &mut ids,
                );
            }
        }
        for enm in &e.enums {
            push(
                &enm.name,
                format!("enum '{}' in entity '{}'", enm.name, en),
                &mut ids,
            );
            for v in &enm.variants {
                push(
                    &v.name,
                    format!(
                        "variant '{}' of enum '{}' in entity '{}'",
                        v.name, enm.name, en
                    ),
                    &mut ids,
                );
            }
        }
        for ta in &e.type_aliases {
            push(
                &ta.name,
                format!("type alias '{}' in entity '{}'", ta.name, en),
                &mut ids,
            );
        }
        // Constants.
        for c in &e.constants {
            push(
                &c.name,
                format!("constant '{}' in entity '{}'", c.name, en),
                &mut ids,
            );
        }
        // Macros (name + params).
        for m in &e.macros {
            push(
                &m.name,
                format!("macro '{}' in entity '{}'", m.name, en),
                &mut ids,
            );
            for p in &m.params {
                push(
                    &p.name,
                    format!(
                        "parameter '{}' of macro '{}' in entity '{}'",
                        p.name, m.name, en
                    ),
                    &mut ids,
                );
            }
        }
        // Members.
        for mem in &e.members {
            push(
                &mem.name,
                format!("member '{}' in entity '{}'", mem.name, en),
                &mut ids,
            );
        }
        // Routes (name + params).
        for route in &e.routes {
            push(
                &route.name,
                format!("route '{}' in entity '{}'", route.name, en),
                &mut ids,
            );
            for p in &route.params {
                push(
                    &p.name,
                    format!(
                        "parameter '{}' of route '{}' in entity '{}'",
                        p.name, route.name, en
                    ),
                    &mut ids,
                );
            }
        }
    }

    ids
}

fn check_statement_leaf(program: &Program, diags: &mut Vec<Diagnostic>) {
    for (name, ctx) in collect_user_identifiers(program) {
        if name == RESERVED_LEAF_STATEMENT {
            diags.push(Diagnostic::error(
                "N3",
                format!(
                    "{ctx} uses the reserved identifier '{RESERVED_LEAF_STATEMENT}': the escrow \
                     profile names statement abbrevs `<theorem>.statement` (spec §7 B1) and relies \
                     on '{RESERVED_LEAF_STATEMENT}' never being a user declaration leaf. Rename it.",
                ),
            ));
        }
    }
}

fn check_binder_prefix(program: &Program, diags: &mut Vec<Diagnostic>) {
    for (name, ctx) in collect_user_identifiers(program) {
        if name.starts_with(RESERVED_BINDER_PREFIX) {
            diags.push(Diagnostic::error(
                "N4",
                format!(
                    "{ctx} uses the reserved prefix '{RESERVED_BINDER_PREFIX}': it is reserved for \
                     compiler-generated fresh binders (predictable route rewriter, spec §7 B4). Rename it.",
                ),
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// N5 — reserved generated declaration names (entity member / type / constant)
// ---------------------------------------------------------------------------

fn check_generated_name_collisions(program: &Program, diags: &mut Vec<Diagnostic>) {
    for e in &program.entities {
        let en = &e.name;

        let check = |name: &str, kind: &str, diags: &mut Vec<Diagnostic>| {
            if let Some(what) = reserved_generated_reason(name) {
                diags.push(Diagnostic::error(
                    "N5",
                    format!("{kind} '{name}' in entity '{en}' collides with {what}. Rename it.",),
                ));
            }
        };

        for mem in &e.members {
            check(&mem.name, "member", diags);
        }
        for r in &e.records {
            check(&r.name, "record", diags);
        }
        for enm in &e.enums {
            check(&enm.name, "enum", diags);
        }
        for ta in &e.type_aliases {
            check(&ta.name, "type alias", diags);
        }
        for c in &e.constants {
            check(&c.name, "constant", diags);
        }
    }
}
