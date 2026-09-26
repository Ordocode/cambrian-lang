// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean codegen — low-level pretty-printer / indentation helpers.
//!
//! The Lean emitter is intentionally tiny in P0: every later phase
//! (P1+) goes through the same `LeanEmitter` so indentation stays
//! consistent across files.

/// Two-space indentation step. Lean 4 conventionally uses two spaces;
/// keep this constant in one place so reformatters stay deterministic.
pub const INDENT: &str = "  ";

/// Append `n` indentation steps to `out`.
pub fn push_indent(out: &mut String, n: usize) {
    for _ in 0..n {
        out.push_str(INDENT);
    }
}

/// Emit a single-line doc comment in Lean's `/-- … -/` style.
pub fn doc_comment(out: &mut String, indent: usize, text: &str) {
    push_indent(out, indent);
    out.push_str("/-- ");
    out.push_str(text);
    out.push_str(" -/\n");
}

/// Emit a single-line `--` comment.
pub fn line_comment(out: &mut String, indent: usize, text: &str) {
    push_indent(out, indent);
    out.push_str("-- ");
    out.push_str(text);
    out.push('\n');
}

/// Emit a `  deriving <names>\n\n` line for a `structure`/`inductive`,
/// filtering it against the predictable emission profile (project
/// `lean.emission_profile: predictable`; `CAMBRIAN_R3_SPEC.md` §7, package B2).
///
/// `derives` lists the legacy derive set, in emission order. Under the
/// predictable profile `Repr` is dropped from it (the only B2-gated form); every
/// other name passes through unchanged. `DecidableEq` (B3) is NOT filtered
/// here — the DecidableEq call sites drop it from the list they pass (and
/// emit an explicit [`super::deceq`] instance instead) only when the
/// shape is templatable (every structure; nullary enums), keeping `deriving
/// DecidableEq` as a fallback for data-carrying enum variants. When
/// `use_predictable_profile` is `false` (the default — absent or
/// `lean.emission_profile: default`), this always emits the full `derives`
/// list verbatim, so legacy output stays byte-identical.
///
/// If filtering empties the list (a lone `deriving Repr`, or Identity's
/// `Repr` after the site drops `DecidableEq`), the whole `deriving` clause is
/// omitted — Lean accepts a `structure … where` / `inductive … where` with no
/// trailing `deriving` clause — and a blank line is emitted in its place so
/// the surrounding spacing is unchanged.
pub fn push_deriving(out: &mut String, use_predictable_profile: bool, derives: &[&str]) {
    #[cfg(feature = "predictable-profile")]
    let kept: Vec<&str> = if use_predictable_profile {
        derives.iter().copied().filter(|&d| d != "Repr").collect()
    } else {
        derives.to_vec()
    };
    #[cfg(not(feature = "predictable-profile"))]
    let kept: Vec<&str> = {
        let _ = use_predictable_profile;
        derives.to_vec()
    };
    if kept.is_empty() {
        out.push('\n');
    } else {
        out.push_str(&format!("  deriving {}\n\n", kept.join(", ")));
    }
}
