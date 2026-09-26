// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean codegen — `<E>Spec.lean` orchestrator (P2).
//!
//! For each entity that has at least one `TestDecl`, `PropertyDecl`, or
//! single-entity `InvariantDecl`, we emit `Cambrian/Generated/<E>Spec.lean`
//! containing three nested namespaces:
//!
//! ```text
//! namespace <E>.Spec.Tests       -- one theorem per TestDecl
//! namespace <E>.Spec.Properties  -- one theorem per PropertyDecl
//! namespace <E>.Spec.Invariants  -- one sub-namespace per InvariantDecl
//! ```
//!
//! **Sorry policy:** main theorems/lemmas are statements with `sorry`
//! bodies (this repo does not prove user properties). Avoid `sorry`
//! outside theorem/lemma bodies. Small helper lemmas introduced for
//! proof ergonomics may carry real (simple) proofs — see
//! `docs/PLAN_LEAN_TARGET.md` → "Sorry policy".
//!
//! Each sub-emitter pushes Lean code into a single `String`; the
//! orchestrator wraps it in the file header / imports and appends an
//! `end <E>.Spec` footer. Returns `None` when the entity has no spec
//! declarations attached, in which case the caller skips emission.

use std::collections::HashSet;

use crate::ast::{Entity, Program, TestStep};

use super::invariant::gen_invariants_block;
use super::property::gen_property_block;
use super::test::gen_tests_block;

/// A diagnostic surfaced during P2 codegen — currently used only for
/// non-error situations like "skip from not yet honoured" or
/// "expect effects skipped". The orchestrator collects these and the
/// caller can route them through the standard validator pipeline if
/// desired (P2 does not require any to be hard errors).
#[derive(Debug, Clone)]
pub(crate) struct SpecDiag {
    pub(crate) message: String,
}

impl SpecDiag {
    pub(crate) fn warn(message: String) -> Self {
        SpecDiag { message }
    }
}

/// One spec block's emitted output (`Tests` / `Properties` / `Invariants`).
///
/// Under the default profile `statements` is `None` and `proofs` is
/// byte-identical to the pre-B1 output. Under the predictable profile (B1) each
/// spec theorem's *type* is lifted into a reducible `abbrev … : Prop` that
/// lands in `statements` (destined for `<E>Statements.lean`), and `proofs`
/// carries the matching `theorem … : <name>.statement := by …` shell (its
/// proof side is unchanged). Both halves wrap the same namespace.
pub(crate) struct SpecBlock {
    /// Statements-file portion (`abbrev`s + relocated invariant model
    /// machinery), namespace-wrapped. `None` under the default profile.
    pub(crate) statements: Option<String>,
    /// Spec-file portion (theorems / proofs / scaffolding), namespace-wrapped.
    pub(crate) proofs: String,
}

/// Result of [`gen_spec_module`]: the `<E>Spec.lean` body plus, under the
/// predictable profile, the hermetic `<E>Statements.lean` body it imports.
pub struct SpecModule {
    /// `Cambrian/Generated/<E>Spec.lean` contents.
    pub spec: String,
    /// `Cambrian/Generated/<E>Statements.lean` contents (predictable profile only).
    pub statements: Option<String>,
}

/// Emit the import list every spec/statements module for `entity` shares:
/// the Prelude (route-result helpers, defaults) plus this entity's own
/// module / `World` / `Routes`, and any cross-entity `Routes` a multi-entity
/// invariant reaches. Pushed onto `out`.
///
/// (B1 split extracted this from the inline `gen_spec_module` header; upstream's
/// 028db5f header doc-text fix is applied to the callers' `/- … -/` blocks.)
fn peer_entities_from_steps(steps: &[TestStep]) -> HashSet<String> {
    steps
        .iter()
        .filter_map(|step| {
            if let TestStep::DeployPeer { entity, .. } = step {
                Some(entity.clone())
            } else {
                None
            }
        })
        .collect()
}

/// Peer entities referenced by this entity's tests / properties / fuzz bodies
/// (`deploy peer = Peer(...)`) and by multi-entity invariants it participates in.
fn spec_peer_route_entities(program: &Program, entity: &Entity) -> HashSet<String> {
    let mut peers = HashSet::new();
    for test in program.tests.iter().filter(|t| t.entity_name == entity.name) {
        peers.extend(peer_entities_from_steps(&test.body));
    }
    for fuzz in program.fuzz_tests.iter().filter(|f| f.entity_name == entity.name) {
        peers.extend(peer_entities_from_steps(&fuzz.body));
    }
    for prop in program.properties.iter().filter(|p| p.entity_name == entity.name) {
        peers.extend(peer_entities_from_steps(&prop.body));
    }
    for inv in program.invariants.iter() {
        if inv.is_single_entity() {
            continue;
        }
        if inv.instances.iter().any(|i| i.entity == entity.name) {
            for inst in &inv.instances {
                if inst.entity != entity.name {
                    peers.insert(inst.entity.clone());
                }
            }
        }
    }
    peers
}

fn push_spec_imports(out: &mut String, program: &Program, entity: &Entity) {
    out.push_str("import Cambrian.Prelude\n");
    out.push_str(&format!("import Cambrian.Generated.{}\n", entity.name));
    out.push_str("import Cambrian.Generated.World\n");
    if !entity.routes.is_empty() {
        out.push_str(&format!(
            "import Cambrian.Generated.{}Routes\n",
            entity.name
        ));
    }
    let peer_entities = spec_peer_route_entities(program, entity);
    for other in &program.entities {
        if other.name == entity.name {
            continue;
        }
        if !other.routes.is_empty() && peer_entities.contains(&other.name) {
            out.push_str(&format!("import Cambrian.Generated.{}Routes\n", other.name));
        }
    }
}

/// Emit `<E>Spec.lean` (and, under predictable, its `<E>Statements.lean` sidecar)
/// for `entity`. Returns `None` when the entity has no test / fuzz /
/// invariant declarations.
pub fn gen_spec_module(
    program: &Program,
    entity: &Entity,
    profile: super::super::LeanProfile,
) -> Option<SpecModule> {
    let mut diags: Vec<SpecDiag> = Vec::new();

    let tests = gen_tests_block(program, entity, &mut diags, profile);
    let properties = gen_property_block(program, entity, &mut diags, profile);
    let invariants = gen_invariants_block(program, entity, &mut diags, profile);

    if tests.is_none() && properties.is_none() && invariants.is_none() {
        return None;
    }

    let predictable = profile.predictable;

    // ---- Spec file (theorems / proofs) --------------------------------
    let mut out = String::new();
    out.push_str("/-\n");
    out.push_str("  Auto-generated by cambrian-transpiler — Lean target (P2).\n");
    out.push_str(&format!(
        "  Specs (tests / fuzz / invariants) for entity `{}`.\n",
        entity.name
    ));
    out.push_str("  Main theorems ship `sorry` proof bodies (statements only).\n");
    out.push_str("  Helper lemmas may carry small generated proofs; see PLAN_LEAN_TARGET.\n");
    out.push_str("-/\n\n");
    push_spec_imports(&mut out, program, entity);
    // Predictable profile (B1): the theorem *types* live in the hermetic
    // `<E>Statements.lean` sidecar as reducible `abbrev`s; import it so each
    // `theorem … : <name>.statement` resolves its single-`Const` type.
    if predictable {
        out.push_str(&format!(
            "import Cambrian.Generated.{}Statements\n",
            entity.name
        ));
    }
    out.push_str("\nset_option linter.unusedVariables false\n");
    // Raise the per-command heartbeat ceiling well above Lean's 200000
    // default. The auto-discharge ladders bound each `simp` rung to a far
    // smaller budget (so an intractable goal gives up and falls to `sorry`
    // quickly), but elaborating a large quantified statement *plus* those
    // bounded attempts can still exceed the default — raising the ceiling
    // keeps the *global* timeout from firing before a rung's own bound does.
    // Only relevant when the proof ladders are emitted.
    if profile.proof_helpers {
        out.push_str("set_option maxHeartbeats 1000000\n");
    }
    out.push('\n');

    // ---- Statements file (escrow only) --------------------------------
    // Header + imports mirror the Spec file (minus the Statements self-import).
    // Hermetic: nothing here depends on the Spec file, so it is emitted
    // *before* it in module order.
    let mut stmts = String::new();
    if predictable {
        stmts.push_str("/-\n");
        stmts.push_str(
            "  Auto-generated by cambrian-transpiler — Lean target (P2, predictable profile).\n",
        );
        stmts.push_str(&format!(
            "  Spec *statements* for entity `{}`.\n",
            entity.name
        ));
        stmts.push_str("\n");
        stmts.push_str("  Each spec theorem's type is lifted here as a reducible\n");
        stmts.push_str("  `abbrev <thm>.statement : Prop`, so the matching `theorem` in\n");
        stmts.push_str("  `<E>Spec.lean` has a single-`Const` type (its statement body — what a\n");
        stmts
            .push_str("  buyer reads / what enters the R3 digest — lives here, not in the proof\n");
        stmts.push_str("  file). Route-result `match`es are expressed through the\n");
        stmts
            .push_str("  `Cambrian.RouteResult.{errCodeIs,okAnd,okImplies}` Prelude helpers, so\n");
        stmts.push_str("  no statement carries a `match` on a `RouteResult`.\n");
        stmts.push_str("\n");
        stmts
            .push_str("  Naming: for a theorem whose fully-qualified name is `F`, its statement\n");
        stmts.push_str(
            "  abbrev is `F.statement` — collision-free (`statement` is a reserved leaf\n",
        );
        stmts.push_str(
            "  the emitter never gives a user declaration) and deterministic. Invariant\n",
        );
        stmts.push_str("  model machinery (`Action` / `step` / `runTrace` / `senders` / …) is\n");
        stmts.push_str("  relocated here too, since the statement quantifies over it.\n");
        stmts.push_str("-/\n\n");
        push_spec_imports(&mut stmts, program, entity);
        stmts.push_str("\nset_option linter.unusedVariables false\n");
        stmts.push_str("set_option maxHeartbeats 1000000\n");
        stmts.push('\n');
    }

    let mut any_statements = false;
    for block in [tests, properties, invariants].into_iter().flatten() {
        out.push_str(&block.proofs);
        if let Some(s) = block.statements {
            any_statements = true;
            stmts.push_str(&s);
        }
    }

    // Diagnostics are printed at the bottom of the file as a comment
    // block so anyone reading the generated source sees them. We
    // could route them through the validator instead, but that would
    // require a richer reporting pipeline than P2 ships.
    if !diags.is_empty() {
        out.push_str("\n/-\n");
        out.push_str("  Lean target diagnostics (informational):\n");
        for d in &diags {
            out.push_str(&format!("    - {}\n", d.message));
        }
        out.push_str("-/\n");
    }

    Some(SpecModule {
        spec: out,
        statements: if predictable && any_statements {
            Some(stmts)
        } else {
            None
        },
    })
}

/// True when `entity` has at least one test/fuzz/invariant declaration
/// in `program`. Used by the root-file generator to know whether to
/// `import Cambrian.Generated.<E>Spec`.
pub fn entity_has_specs(program: &Program, entity: &Entity) -> bool {
    program.tests.iter().any(|t| t.entity_name == entity.name)
        || program
            .properties
            .iter()
            .any(|p| p.entity_name == entity.name)
        || program
            .invariants
            .iter()
            .any(|inv| {
                inv.emit_policy != crate::ast::InvariantEmitPolicy::Superseded
                    && inv.is_single_entity()
                    && inv.entity_name() == entity.name
            })
        || program.invariants.iter().any(|inv| {
            inv.emit_policy != crate::ast::InvariantEmitPolicy::Superseded
                && !inv.is_single_entity()
                && inv.instances.iter().any(|i| i.entity == entity.name)
        })
}

/// The reflection-layer `simp` lemma list (entity-specific) shared by the
/// `test` / `property` / `invariant` proof tactics.
///
/// It combines the curated, codegen-populated `simp` sets
/// (`cambrian_route_simp` / `cambrian_member_simp` / `cambrian_pre_simp` /
/// `cambrian_except_simp`) with *this* entity's `World` accessor/setter and
/// the structural `default`s, so a single `simp` / `simp_all` call can unfold
/// a route call all the way down to its arithmetic and read the post-state
/// back out of the world.
///
/// `cambrian_bitvec_simp` is intentionally **excluded** here: callers add it
/// only on the `omega`-backed rung of their ladder, so the cheap
/// exact-equality rung is not perturbed by `BitVec` ↔ `Nat` canonicalization.
pub(crate) fn reflection_simp_base(entity: &Entity) -> String {
    let field = super::world::entity_field_name(&entity.name);
    format!(
        "cambrian_route_simp, cambrian_member_simp, cambrian_pre_simp, cambrian_except_simp, \
Cambrian.Generated.World.{field}, Cambrian.Generated.World.with{ent}, \
Cambrian.Generated.World.default, Cambrian.Generated.Storage.default, \
Cambrian.MsgCtx.default, Cambrian.BlockEnv.default, {ent}.State.default",
        ent = entity.name,
    )
}

/// Shared auto-discharge proof tail for `test` / `property` spec theorems.
///
/// Proofs attach at theorem level (`\n:= by …`) so nested `match` goals are
/// not glued into an empty `by`-block (SMAFD LG-007b class B).  When
/// [`SpecStatementShape::needs_sorry_stub`] holds, ship `sorry` only (class A).
pub(crate) fn emit_proof_tail(
    shape: &super::spec_steps::SpecStatementShape,
    entity: &Entity,
    predictable: bool,
    proof_helpers: bool,
    slug: &str,
) -> String {
    if !proof_helpers {
        return ":= by sorry\n".to_string();
    }
    if shape.needs_sorry_stub() {
        return ":= by sorry\n".to_string();
    }
    let base = reflection_simp_base(entity);
    let unfold = if predictable {
        format!("{}.statement, ", slug)
    } else {
        String::new()
    };
    format!(
        ":= by\n  first\n    | (set_option maxHeartbeats 40000 in (simp (config := {{ contextual := true }}) [{unfold}{base}]; done))\n    | (set_option maxHeartbeats 80000 in (simp (config := {{ contextual := true }}) [{unfold}{base}, cambrian_bitvec_simp] <;> omega))\n    | sorry\n"
    )
}

/// Convert a free-form `test "name"` / `invariant "name"` string into a
/// Lean-friendly identifier.
///
/// The slug is intentionally **non-fancy**: lowercase ASCII letters and
/// digits, words separated by `_`. Non-ASCII characters are dropped.
/// Empty input becomes `_unnamed`. A leading digit is prefixed with
/// `_` so the result is always a valid Lean identifier.
pub(crate) fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut prev_underscore = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        return "_unnamed".to_string();
    }
    if out
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        out.insert(0, '_');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Entity, Span};
    use crate::codegen::lean::evm::spec_steps::SpecStatementShape;

    fn test_entity(name: &str) -> Entity {
        Entity {
            name: name.to_string(),
            records: vec![],
            enums: vec![],
            type_aliases: vec![],
            constants: vec![],
            macros: vec![],
            routes: vec![],
            members: vec![],
            events: vec![],
            errors: vec![],
            span: Span {
                start: 0,
                end: 0,
                file_id: 0,
            },
        }
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(
            slugify("increment adds to count"),
            "increment_adds_to_count"
        );
        assert_eq!(slugify("Reset Zeroes Count"), "reset_zeroes_count");
        assert_eq!(slugify("multi-step increment"), "multi_step_increment");
        assert_eq!(slugify("123 numbers first"), "_123_numbers_first");
        assert_eq!(slugify(""), "_unnamed");
        assert_eq!(slugify("__"), "_unnamed");
    }

    #[test]
    fn emit_proof_tail_shallow_shape_uses_ladder() {
        let shape = SpecStatementShape {
            max_route_depth: 0,
            has_throw_expect: false,
            has_unmodelable_effects: false,
            has_branching_map_rewrite: false,
        };
        let entity = test_entity("Counter");
        let tail = emit_proof_tail(&shape, &entity, false, true, "slug");
        assert!(tail.contains("first"));
        assert!(tail.contains("maxHeartbeats"));
    }

    #[test]
    fn emit_proof_tail_heavy_shape_sorry_only() {
        let shape = SpecStatementShape {
            max_route_depth: 2,
            has_throw_expect: false,
            has_unmodelable_effects: false,
            has_branching_map_rewrite: false,
        };
        let entity = test_entity("Counter");
        assert_eq!(
            emit_proof_tail(&shape, &entity, false, true, "slug"),
            ":= by sorry\n",
        );
    }
}
