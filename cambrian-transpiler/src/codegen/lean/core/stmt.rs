// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean route-codegen statement IR (Layer 3).
//!
//! Historically the Lean route codegen interpreted each [`RouteAction`]
//! in three separate places — `emit_action` (state-only) and
//! `emit_world_threaded_action` (world-threaded), each implicitly gated by
//! a *context matrix* (state vs world carrier; pure `Id` vs failing
//! `Except`; nested in `if`/`for`), plus the raw-return value loop in
//! `build_route_body`. When one cell of that matrix was missed, codegen
//! *failed open* — it emitted a silent sentinel rather than an error, so
//! gaps surfaced one fixture at a time.
//!
//! The two **statement emitters** are now collapsed into a single
//! pipeline:
//!
//! * [`Carrier`] names the (exhaustive) set of statement-context cells:
//!   state-only (`s`) vs world-threaded (`w`), each with a failing
//!   (`Except` `do`) or pure (`Id`/`:=`) framing.
//! * `lower_action` (in [`super::super::route`]) is the **one** exhaustive
//!   `match` over `RouteAction` that produces a [`LeanStmt`] tree,
//!   branching internally on the `Carrier`. Adding a `RouteAction` variant
//!   fails to compile until handled; an unmodelled `(action, carrier)`
//!   cell becomes [`LeanStmt::Abort`], which **hard-errors** at render
//!   time instead of emitting a silent sentinel. (`emit_action` /
//!   `emit_world_threaded_action` are gone; `emit_actions` /
//!   `emit_world_threaded_actions` are thin `render_into ∘ lower_actions`
//!   wrappers kept for their existing call sites.)
//! * [`render_into`] is a dumb, carrier-agnostic pretty-printer that owns
//!   all indentation, so `lower_action` is indent-free.
//!
//! The raw tail-value lowering in `build_route_body` (a non-failing
//! route's `let`-chain that produces a *value*, where a
//! conditional-without-return collapses to a comment) is a distinct
//! *value* concern — shared with `lower_view_payload_term` — and is not a
//! statement emitter, so it stays separate. It takes a [`Carrier`] and
//! builds expr ctxs only via [`super::route_local::expr_ctx_for`], so it
//! cannot re-derive "has `w`?" from proxies like `phase.is_none()`.
//! Its `for`-body delegates to the unified [`Carrier::State`] emitter.

/// The carrier / monad context a *statement* sequence is lowered into.
/// This is the exhaustive enumeration of the statement-emitter context
/// matrix; `lower_action` matches on it so every `(action, carrier)` cell
/// is accounted for in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Carrier {
    /// State-only statement emitter over `s`. `fail` toggles the `Except`
    /// `do` framing (reachable at top level for failing routes) vs the
    /// pure framing (reachable only inside a raw route's `for`-body).
    State { fail: bool },
    /// World-threaded statement emitter: `w` carrier (sends mutate the
    /// world). `fail` toggles the `Except` `do` framing.
    World { fail: bool },
}

impl Carrier {
    pub(crate) fn is_fail(self) -> bool {
        matches!(
            self,
            Carrier::State { fail: true } | Carrier::World { fail: true }
        )
    }
    pub(crate) fn is_world(self) -> bool {
        matches!(self, Carrier::World { .. })
    }
}

/// Structural statement IR. The tree carries no indentation — [`render`]
/// applies it. `Block` increases the indent level by one for its
/// children; this is how nested `if` / `for` bodies are expressed.
pub(crate) enum LeanStmt {
    /// A single source line (no leading indent, no trailing newline).
    Line(String),
    /// A pre-formatted, possibly multi-line snippet produced by a helper
    /// (`lean_send`/`lean_deploy`/…). Rendered with `write_indented`
    /// semantics and a guaranteed trailing newline.
    Raw(String),
    /// Indent the nested statements one level deeper.
    Block(Vec<LeanStmt>),
    /// An unmodelled `(action, carrier)` cell. Rendering this is a
    /// transpiler bug — it panics with a structured message rather than
    /// emitting a silent sentinel (the Layer 3 "hard-error" guarantee).
    Abort(String),
}

impl LeanStmt {
    pub(crate) fn line(s: impl Into<String>) -> LeanStmt {
        LeanStmt::Line(s.into())
    }
}

/// Fail-closed guard for an unmodelled `(action, carrier)` statement cell.
///
/// The Lean route codegen used to "fail open" at these sites — emitting a
/// silent comment (`-- skipped` / `-- internal` / `-- bug`) so an
/// unmodelled construct produced *plausible-looking but wrong* output.
/// Calling this instead turns such a cell into a loud transpiler error, so
/// a never-before-exercised combination can never silently degrade
/// fidelity: it either has a faithful lowering or it stops the build.
///
/// `carrier` describes the emission context; `detail` describes the action.
pub(crate) fn unmodeled_cell(carrier: Carrier, detail: impl std::fmt::Display) -> ! {
    panic!(
        "Lean route codegen: unmodelled (action, carrier) cell reached — \
         this is a transpiler bug, not a silent sentinel. carrier={carrier:?}, {detail}"
    );
}

/// Render a statement sequence into `out` at `indent` (in units of
/// two-space levels). Owns all indentation so the lowering stays
/// indent-free.
pub(crate) fn render_into(out: &mut String, stmts: &[LeanStmt], indent: usize) {
    let prefix: String = "  ".repeat(indent);
    for stmt in stmts {
        match stmt {
            LeanStmt::Line(s) => {
                // Multi-line terms (e.g. `match … with\n  | … => …` from
                // `gen_match`) must indent every continuation line, not
                // only the first — otherwise arms fall out of the
                // enclosing `do` / `foldlM` block (C-2).
                for (i, line) in s.split_inclusive('\n').enumerate() {
                    if i == 0 || !line.trim().is_empty() {
                        out.push_str(&prefix);
                    }
                    out.push_str(line);
                }
                if !s.ends_with('\n') {
                    out.push('\n');
                }
            }
            LeanStmt::Raw(snippet) => {
                // Mirror `write_indented`: prefix line 0 always, and any
                // subsequent non-blank line. Guarantee a trailing newline.
                for (i, line) in snippet.split_inclusive('\n').enumerate() {
                    if i == 0 || !line.trim().is_empty() {
                        out.push_str(&prefix);
                    }
                    out.push_str(line);
                }
                if !snippet.ends_with('\n') {
                    out.push('\n');
                }
            }
            LeanStmt::Block(inner) => {
                render_into(out, inner, indent + 1);
            }
            LeanStmt::Abort(msg) => {
                panic!(
                    "Lean route codegen: unmodelled statement reached \
                     render() — this is a transpiler bug, not a silent \
                     sentinel: {msg}",
                );
            }
        }
    }
}
