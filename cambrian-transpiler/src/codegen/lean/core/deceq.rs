// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean codegen — explicit `DecidableEq` / `BEq` instances under the escrow
//! profile (`CAMBRIAN_R3_SPEC.md` §7, packages B3 + P3).
//!
//! Under the default profile `deriving DecidableEq` / `deriving BEq` are emitted
//! as before; under predictable profile those derives are dropped from the derive list (the
//! equation compiler's `.decEq` / `.beq` chain + its `.match_N` matcher are
//! non-predictable equation-compiler territory) and replaced by an explicit
//! **term-level** instance, printed right after the type inside the same
//! namespace so the kernel-qualified name comes out byte-identical to the
//! deriving-generated `instDecidableEq<ShortName>` / `instBEq<ShortName>`
//! (Pre-Decidable-instance resolution and every `==`/`DecidableEq`-driven lookup
//! are therefore unchanged).
//!
//! Shapes, all zero-tactic / zero-matcher (dump-verified against v4.29.1):
//!   * enum (nullary ctors): the n×n `casesOn` matrix — diagonal
//!     `Decidable.isTrue rfl`, off-diagonal `Decidable.isFalse (fun h =>
//!     <T>.noConfusion h)`;
//!   * enum (data-carrying ctors, P3): the same n×n `casesOn` matrix, but each
//!     diagonal data-ctor cell is a per-field `dite` chain (structural, like a
//!     record) closed with `<T>.<ctor>.inj`; every off-diagonal cell binds the
//!     other ctor's fields (so the `casesOn` arm is well-typed) and returns
//!     `Decidable.isFalse (fun h => <T>.noConfusion h)`;
//!   * structure (k fields): nested `casesOn` (over `a`, then `b`) + a per-field
//!     `dite` chain whose true tail is `Decidable.isTrue <congr-proof>` and whose
//!     per-field false branch is `Decidable.isFalse (fun heq => h_i (proj_i
//!     (<T>.mk.inj heq)))`;
//!   * structure `BEq` (P3): `BEq.mk (fun a b => a.f₁ == b.f₁ && … && a.f_k ==
//!     b.f_k)` — a `&&`-fold of per-field `==`, projection-based (no `casesOn`,
//!     no matcher). The field `BEq` is resolved structurally by the elaborator.
//!
//! The elaborator lifts the (proof-typed) true/false arguments of a structural
//! `dite` chain into `instDecidableEq<T>._proof_N` theorems (kind=Thm → thm-aux,
//! excluded from the digest comparison); the instance body — which IS hashed —
//! references them by their deterministic sequential names. The predictor
//! (`cambrian-predict`) reproduces the same body.

/// The derive list to hand to [`super::emitter::push_deriving`]: under
/// predictable, `DecidableEq` is dropped iff `template_deceq` (i.e. the site will
/// emit an explicit instance instead). Legacy / non-templatable → unchanged.
pub fn deceq_filtered<'a>(
    use_predictable: bool,
    template_deceq: bool,
    derives: &[&'a str],
) -> Vec<&'a str> {
    if use_predictable && template_deceq {
        derives
            .iter()
            .copied()
            .filter(|&d| d != "DecidableEq")
            .collect()
    } else {
        derives.to_vec()
    }
}

/// The derive list after dropping BOTH `DecidableEq` (iff `template_deceq`) and
/// `BEq` (iff `template_beq`) under predictable profile — the record site drops both and
/// emits explicit term-level instances for each. Legacy / non-templatable →
/// unchanged.
pub fn instances_filtered<'a>(
    use_predictable: bool,
    template_deceq: bool,
    template_beq: bool,
    derives: &[&'a str],
) -> Vec<&'a str> {
    if !use_predictable {
        return derives.to_vec();
    }
    derives
        .iter()
        .copied()
        .filter(|&d| !(template_deceq && d == "DecidableEq") && !(template_beq && d == "BEq"))
        .collect()
}

/// Emit `instance instDecidableEq<ty> : DecidableEq <ty> := <n×n casesOn matrix>`
/// for an enum with constructors `ctors` (short name + field arity, in ctor
/// order). Nullary-only enums take the byte-identical B3 nullary path; a
/// data-carrying enum (some ctor with fields) takes the P3 mixed-matrix path.
/// `ty` is the short type name; all names must resolve inside the namespace this
/// is printed in.
pub fn emit_enum_deceq(out: &mut String, ty: &str, ctors: &[(String, usize)]) {
    if ctors.iter().all(|(_, arity)| *arity == 0) {
        emit_enum_deceq_nullary(out, ty, ctors);
    } else {
        emit_enum_deceq_data(out, ty, ctors);
    }
}

/// B3 nullary path — kept byte-identical to the original template (governor /
/// uniswap / counter escrow fixtures depend on it staying bit-for-bit).
fn emit_enum_deceq_nullary(out: &mut String, ty: &str, ctors: &[(String, usize)]) {
    out.push_str(&format!(
        "instance instDecidableEq{ty} : DecidableEq {ty} := fun a b =>\n"
    ));
    out.push_str("  a.casesOn (motive := fun a => Decidable (a = b))\n");
    for (i, (ci, _)) in ctors.iter().enumerate() {
        out.push_str(&format!(
            "    (b.casesOn (motive := fun b => Decidable ({ty}.{ci} = b))\n"
        ));
        out.push_str("      ");
        let arms: Vec<String> = (0..ctors.len())
            .map(|j| {
                if i == j {
                    "(Decidable.isTrue rfl)".to_string()
                } else {
                    format!("(Decidable.isFalse (fun h => {ty}.noConfusion h))")
                }
            })
            .collect();
        out.push_str(&arms.join(" "));
        out.push_str(")\n");
    }
    out.push('\n');
}

/// P3 data-carrying path — the n×n `casesOn` matrix with structural diagonal
/// cells. Off-diagonal cells bind the other ctor's fields so each `casesOn` arm
/// has the arity `casesOn` expects.
fn emit_enum_deceq_data(out: &mut String, ty: &str, ctors: &[(String, usize)]) {
    out.push_str(&format!(
        "instance instDecidableEq{ty} : DecidableEq {ty} := fun a b =>\n"
    ));
    out.push_str("  a.casesOn (motive := fun a => Decidable (a = b))\n");
    for (i, (ci, ai)) in ctors.iter().enumerate() {
        // Outer arm binds a's fields (a1..a_{ai}); the `casesOn` motive fixes
        // the lhs at `<ty>.<ci> a1..a_{ai}`.
        let a_binders: Vec<String> = (1..=*ai).map(|n| format!("a{n}")).collect();
        let a_head = if *ai == 0 {
            format!("{ty}.{ci}")
        } else {
            format!("{ty}.{ci} {}", a_binders.join(" "))
        };
        let arm_open = if *ai == 0 {
            "    (".to_string()
        } else {
            format!("    (fun {} =>\n      ", a_binders.join(" "))
        };
        out.push_str(&arm_open);
        out.push_str(&format!(
            "b.casesOn (motive := fun b => Decidable ({a_head} = b))\n"
        ));
        for (j, (cj, aj)) in ctors.iter().enumerate() {
            let b_binders: Vec<String> = (1..=*aj).map(|n| format!("b{n}")).collect();
            out.push_str("        ");
            let cell = enum_deceq_cell(ty, ci, *ai, cj, *aj, i == j, &b_binders);
            out.push_str(&cell);
            out.push('\n');
        }
        // One closing paren for the arm group opened in `arm_open` (either `(`
        // wrapping `b.casesOn`, or `(fun a₁..a_m => …`).
        out.push_str("      )\n");
    }
    out.push('\n');
}

/// One inner-`casesOn` cell of the data-enum matrix (arm for ctor `cj`).
fn enum_deceq_cell(
    ty: &str,
    ci: &str,
    ai: usize,
    _cj: &str,
    aj: usize,
    diagonal: bool,
    b_binders: &[String],
) -> String {
    let wrap_b = |inner: String| -> String {
        if aj == 0 {
            format!("({inner})")
        } else {
            format!("(fun {} => {inner})", b_binders.join(" "))
        }
    };
    if !diagonal {
        return wrap_b(format!("Decidable.isFalse (fun h => {ty}.noConfusion h)"));
    }
    // Diagonal cell: `<ty>.<ci>` with fields on both sides → structural dite
    // chain over the ai fields, closed with `<ty>.<ci>.inj`.
    if ai == 0 {
        return "(Decidable.isTrue rfl)".to_string();
    }
    let ctor = format!("{ty}.{ci}");
    let inj = format!("{ty}.{ci}.inj");
    wrap_b(dite_chain(&ctor, &inj, ai, 1))
}

/// Emit `instance instDecidableEq<ty> : DecidableEq <ty> := <casesOn + dite
/// chain>` for a structure with fields `fields` (short field names, in
/// declaration order). Works for any arity; the k=0/1/2 shapes are the
/// dump-calibrated ones exercised by the standard corpora.
pub fn emit_struct_deceq(out: &mut String, ty: &str, fields: &[String]) {
    let k = fields.len();
    out.push_str(&format!(
        "instance instDecidableEq{ty} : DecidableEq {ty} := fun a b =>\n"
    ));
    if k == 0 {
        out.push_str("  a.casesOn (motive := fun a => Decidable (a = b))\n");
        out.push_str(&format!(
            "    (b.casesOn (motive := fun b => Decidable ({ty}.mk = b))\n"
        ));
        out.push_str("      (Decidable.isTrue rfl))\n\n");
        return;
    }
    let a_binders: Vec<String> = (1..=k).map(|i| format!("a{i}")).collect();
    let b_binders: Vec<String> = (1..=k).map(|i| format!("b{i}")).collect();
    out.push_str(&format!(
        "  a.casesOn (motive := fun a => Decidable (a = b)) (fun {} =>\n",
        a_binders.join(" ")
    ));
    out.push_str(&format!(
        "    b.casesOn (motive := fun b => Decidable ({ty}.mk {} = b)) (fun {} =>\n",
        a_binders.join(" "),
        b_binders.join(" ")
    ));
    out.push_str("      ");
    out.push_str(&dite_chain(
        &format!("{ty}.mk"),
        &format!("{ty}.mk.inj"),
        k,
        1,
    ));
    // close the `(fun b1… =>` and `(fun a1… =>` lambdas.
    out.push_str("))\n\n");
}

/// Emit `instance instBEq<ty> : BEq <ty> := BEq.mk (fun a b => <n×n casesOn
/// matrix>)` for a DATA-carrying enum (P4). This is the Bool mirror of
/// [`emit_enum_deceq_data`]: the same `a.casesOn`/`b.casesOn` matrix over the
/// constructors, but each cell is a `Bool` (no proof) — the diagonal data-ctor
/// cell is the `&&`-fold of the per-field `a_i == b_i`, the nullary diagonal is
/// `true`, and every off-diagonal cell is `false` (binding the other ctor's
/// fields so the `casesOn` arm has the arity `casesOn` expects). Matcher-free
/// (direct `casesOn`, non-dependent `motive := fun _ => Bool`), so it replaces
/// the `deriving BEq` equation-compiler matcher (`instBEq<T>.beq.match_1`) the
/// predictable profile must not mint. A field `==` is resolved structurally by the
/// elaborator (BitVec / String / user enum-or-record `BEq`), exactly as in
/// [`emit_struct_beq`]. Called ONLY for data-carrying enums; nullary enums keep
/// `deriving BEq` (which reduces to a `ctorIdx` comparison — predictable, not a
/// matcher).
pub fn emit_enum_beq(out: &mut String, ty: &str, ctors: &[(String, usize)]) {
    out.push_str(&format!(
        "instance instBEq{ty} : BEq {ty} := BEq.mk (fun a b =>\n"
    ));
    out.push_str("  a.casesOn (motive := fun _ => Bool)\n");
    for (i, (_ci, ai)) in ctors.iter().enumerate() {
        let a_binders: Vec<String> = (1..=*ai).map(|n| format!("a{n}")).collect();
        if *ai == 0 {
            out.push_str("    (b.casesOn (motive := fun _ => Bool)\n");
        } else {
            out.push_str(&format!(
                "    (fun {} =>\n      b.casesOn (motive := fun _ => Bool)\n",
                a_binders.join(" ")
            ));
        }
        for (j, (_cj, aj)) in ctors.iter().enumerate() {
            let b_binders: Vec<String> = (1..=*aj).map(|n| format!("b{n}")).collect();
            let bool_term = if i == j {
                if *ai == 0 {
                    "true".to_string()
                } else {
                    (1..=*ai)
                        .map(|n| format!("a{n} == b{n}"))
                        .collect::<Vec<_>>()
                        .join(" && ")
                }
            } else {
                "false".to_string()
            };
            let cell = if *aj == 0 {
                format!("({bool_term})")
            } else {
                format!("(fun {} => {bool_term})", b_binders.join(" "))
            };
            out.push_str(&format!("        {cell}\n"));
        }
        out.push_str("      )\n");
    }
    out.push_str("  )\n\n");
}

/// Emit `instance instBEq<ty> : BEq <ty> := BEq.mk (fun a b => a.f₁ == b.f₁ && …)`
/// for a structure with fields `fields` (short field names, in declaration
/// order). `&&` right-associates (Lean's `Bool.and`); a 0-field structure
/// degenerates to a constant `true`. Matcher-free — projection-based, so the
/// field `BEq` is resolved structurally by the elaborator.
pub fn emit_struct_beq(out: &mut String, ty: &str, fields: &[String]) {
    out.push_str(&format!(
        "instance instBEq{ty} : BEq {ty} := BEq.mk (fun a b => "
    ));
    if fields.is_empty() {
        out.push_str("true)\n\n");
        return;
    }
    let cmp: Vec<String> = fields.iter().map(|f| format!("a.{f} == b.{f}")).collect();
    out.push_str(&cmp.join(" && "));
    out.push_str(")\n\n");
}

/// The nested `dite` chain over fields `i..=k` for constructor `ctor` (full
/// dotted head, e.g. `Point.mk` or `Action.deposit`) with injection lemma `inj`
/// (`Point.mk.inj` / `Action.deposit.inj`). At `i > k` the tail is the `isTrue`
/// branch.
fn dite_chain(ctor: &str, inj: &str, k: usize, i: usize) -> String {
    if i > k {
        return format!("(Decidable.isTrue {})", true_proof(ctor, k));
    }
    format!(
        "(dite (a{i} = b{i}) (fun h{i} => {}) (fun h{i} => Decidable.isFalse {}))",
        dite_chain(ctor, inj, k, i + 1),
        false_proof(inj, k, i),
    )
}

/// `isTrue` congruence proof of `<ctor> a₁…a_k = <ctor> b₁…b_k` from
/// `h₁ : a₁=b₁, …, h_k : a_k=b_k`.
fn true_proof(ctor: &str, k: usize) -> String {
    if k == 1 {
        return format!("(congrArg {ctor} h1)");
    }
    // steps, field k down to field 1; each rewrites one field. Fields < i stay
    // at their `a`-value, fields > i are already at their `b`-value.
    let step = |i: usize| -> String {
        // arg fixed prefix a₁..a_{i-1}, changing x, fixed suffix b_{i+1}..b_k
        if i == k {
            let prefix: Vec<String> = (1..i).map(|j| format!("a{j}")).collect();
            format!("(congrArg ({ctor} {}) h{i})", prefix.join(" "))
        } else {
            let mut parts: Vec<String> = (1..i).map(|j| format!("a{j}")).collect();
            parts.push("x".to_string());
            for j in (i + 1)..=k {
                parts.push(format!("b{j}"));
            }
            format!("(congrArg (fun x => {ctor} {}) h{i})", parts.join(" "))
        }
    };
    // Eq.trans step_k (Eq.trans step_{k-1} (... step_1))
    let mut acc = step(1);
    for i in 2..=k {
        acc = format!("(Eq.trans {} {})", step(i), acc);
    }
    acc
}

/// `isFalse` proof body `fun heq => h_i (proj_i (<inj> heq))` : the `fun heq =>`
/// is left to the caller (`Decidable.isFalse <this>`), so this returns the whole
/// `(fun heq => …)` lambda.
fn false_proof(inj: &str, k: usize, i: usize) -> String {
    if k == 1 {
        return format!("(fun heq => h{i} ({inj} heq))");
    }
    // `<inj> heq : a₁=b₁ ∧ (a₂=b₂ ∧ (… ∧ a_k=b_k))`. Projection for field i:
    // `.2` (i-1) times, then `.1` unless i==k.
    let mut proj = String::new();
    for _ in 1..i {
        proj.push_str(".2");
    }
    if i < k {
        proj.push_str(".1");
    }
    format!("(fun heq => h{i} (({inj} heq){proj}))")
}
