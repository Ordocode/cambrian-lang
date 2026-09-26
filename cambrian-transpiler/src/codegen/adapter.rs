// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use crate::ast::{Entity, Expr, Pattern, Program, Route, RouteAction};
use crate::project::Project;

// ===========================================================================
// OutputBackend — language-agnostic codegen entry point
// ===========================================================================

/// High-level code generation backend.
/// Unifies the Rust and Solidity pipelines behind a common interface.
pub trait OutputBackend {
    /// Generate the full output source code for the given program.
    fn gen_program(&self, program: &Program) -> String;

    /// File extension for the generated output (e.g. "rs", "sol").
    fn file_extension(&self) -> &str;

    /// Output file name for a given entity (e.g. "lib.rs", "Counter.sol").
    fn file_name_for_entity(&self, entity_name: &str) -> String;

    /// Optional extra files to generate (e.g. Cargo.toml for Rust targets).
    /// Returns a list of (relative_path, contents) pairs.
    fn extra_files(&self, _program: &Program, _entity_name: &str) -> Vec<(String, String)> {
        vec![]
    }

    /// Human-readable target description for CLI output.
    fn target_description(&self) -> &str;

    /// Generate test files for a given entity.
    /// Returns a list of (relative_path, contents) pairs.
    /// Default: no test files (backends that support tests override this).
    fn gen_test_files(&self, _program: &Program, _entity_name: &str) -> Vec<(String, String)> {
        vec![]
    }

    /// Generate all output files for a multi-file project.
    /// Returns Vec<(relative_path, contents)>.
    /// Default: treats it like a single-program compilation of the merged program.
    fn gen_project(&self, project: &Project) -> Vec<(String, String)> {
        let code = self.gen_program(&project.merged);
        let entity_name = project.merged.entities.first()
            .map(|e| e.name.as_str())
            .unwrap_or("output");
        let main_file = self.file_name_for_entity(entity_name);
        let mut files = vec![(main_file, code)];
        files.extend(self.extra_files(&project.merged, entity_name));
        files
    }
}

// ===========================================================================
// ActionEmitter — shared route-action dispatch trait
// ===========================================================================

/// Per-variant action code emitter.
///
/// Rust and EVM/Solidity backends implement this trait. The shared
/// [`dispatch_action`](super::dispatch_action) function does the match on
/// `RouteAction` and delegates to the appropriate method, handling recursive
/// variants (`Conditional`, `Rescue`) uniformly.
pub trait ActionEmitter {
    fn emit_let(&self, pattern: &Pattern, value: &Expr, entity: &Entity, route: &Route, program: &Program, indent: &str) -> String;
    fn emit_return(&self, values: &[Expr], entity: &Entity, route: &Route, program: &Program, indent: &str) -> String;
    fn emit_throw(&self, error_code: u32, indent: &str) -> String;
    fn emit_send(
        &self, message: &Option<String>, args: &[Expr], dest: &Expr,
        send_options: Option<&Expr>, entity: &Entity, route: &Route, program: &Program, indent: &str,
    ) -> String;
    fn emit_deploy(
        &self, target_entity: &str, send_options: Option<&Expr>,
        constructor_args: &[Expr], entity: &Entity, route: &Route, program: &Program, indent: &str,
    ) -> String;
    fn emit_call_route(&self, name: &str, args: &[Expr], entity: &Entity, route: &Route, indent: &str) -> String;
    fn emit_effect(&self, namespace: &str, name: &str, args: &[Expr], entity: &Entity, route: &Route, indent: &str) -> String;

    /// Emit a `var name = msg(args) ~> dest [with opts];` cross-contract call.
    fn emit_var_call(
        &self, name: &str, message: &str, args: &[Expr], dest: &Expr,
        send_options: Option<&Expr>, entity: &Entity, route: &Route, program: &Program, indent: &str,
    ) -> String;

    /// Format a conditional block. `then_code` and `else_code` contain the
    /// already-emitted inner actions (at deeper indentation).
    fn format_conditional(
        &self, condition: &Expr, then_code: &str, else_code: &str,
        entity: &Entity, route: &Route, indent: &str,
    ) -> String;

    /// Format a `rescue` node. On EVM this is a comment only (E26 rejects
    /// bounce recovery); Acki Nacki uses the SdkAdapter bounce_tag path
    /// rather than this hook.
    fn format_rescue(
        &self, tag: &str, inner_code: &str, inner_action: &RouteAction,
        entity: &Entity, route: &Route, program: &Program, indent: &str,
    ) -> String;

    /// Emit a `gosh::updateCode(args) with callback(args)` action.
    fn emit_update_code(
        &self, _update_args: &[Expr], _callback_route: &str, _callback_args: &[Expr],
        _entity: &Entity, _route: &Route, _program: &Program, indent: &str,
    ) -> String {
        format!("{}// updateCode not supported on this target\n", indent)
    }

    /// Emit an action-level `for <pat> in <iter> => [ <body> ]` loop.
    /// `body_code` contains the already-emitted inner actions
    /// (at deeper indentation).
    fn format_for(
        &self, _pattern: &Pattern, _iter: &Expr, _body_code: &str,
        _entity: &Entity, _route: &Route, indent: &str,
    ) -> String {
        format!("{}// action-level `for` not supported on this target\n", indent)
    }

    /// Phase EVM-P0-C: emit an `emit EventName(args);` action. Default
    /// implementation drops the action with an explanatory comment for
    /// targets that lack a logged-event analogue (Acki Nacki).
    fn emit_emit(
        &self, event_name: &str, _args: &[Expr],
        _entity: &Entity, _route: &Route, _program: &Program, indent: &str,
    ) -> String {
        format!("{}// emit {}(...) — events not supported on this target\n", indent, event_name)
    }

    /// Phase EVM-P0-D: emit a `throw CustomError(args);` action. Default
    /// implementation lowers to a deterministic numeric `throw(N)`
    /// (FNV-1a 16-bit hash of the error name) so non-EVM targets keep
    /// reverting cleanly.
    fn emit_throw_custom(
        &self, name: &str, _args: &[Expr],
        _entity: &Entity, _route: &Route, _program: &Program, indent: &str,
    ) -> String {
        let code = (super::types::cambrian_function_id(name) & 0xFFFF) as u32;
        format!("{}revert(\"throw({})\"); // {}\n", indent, code, name)
    }
}

// ===========================================================================
// Effect ordering policy (adapter-level)
// ===========================================================================

/// Emission order for route body effects relative to state writes.
///
/// Both current Rust adapters use [`StateThenEffects`] (matches today's bytecode).
/// TVM (Acki Nacki) applies a queued effect list after the route body returns;
/// phased routes snapshot state via [`SdkAdapter::gen_phase_state_snapshot`]
/// between writes and phase actions. EVM CEI (state before external calls) is
/// enforced by the Solidity adapter, not this Rust core path.
///
/// Future alternate policies should consult kernel [`crate::analysis::RouteFacts`]
/// (send/call graphs, phase boundaries) rather than re-deriving order in
/// `route.rs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectOrdering {
    /// CEI-like / current shared core: member transforms → state writes →
    /// phase snapshot (if any) → actions/effects.
    StateThenEffects,
    // Future: EffectsThenState for alternate domains — not implemented yet.
}
