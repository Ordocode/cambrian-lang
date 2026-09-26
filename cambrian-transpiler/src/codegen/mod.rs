// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

#[cfg(feature = "rust-targets")]
mod expr;
mod adapter;
#[cfg(feature = "rust-targets")]
mod adapter_rust;
mod types;
#[cfg(feature = "rust-targets")]
mod entity;
#[cfg(feature = "rust-targets")]
mod route;
pub(crate) mod stdlib;
pub(crate) mod std_str;
#[cfg(feature = "rust-targets")]
pub mod ackinacki;
pub mod solidity;
/// Compatibility re-export cluster (legacy `codegen::evm` paths).
pub mod evm {
    pub use super::solidity::evm::{gen_evm_solidity, gen_evm_solidity_opts};
    pub use super::solidity::evm::EvmActionEmitter;
    
    
}
pub(crate) mod evm_harness_factory;
pub mod evm_test_codegen;
#[cfg(feature = "revm")]
pub mod evm_revm_test_codegen;
#[cfg(feature = "revm")]
pub mod cargo_fuzz_codegen;
pub mod storage_layout;
pub mod test_backend;
pub(crate) mod invariant_harness;
#[cfg(feature = "rust-targets")]
pub mod test_codegen;
pub mod lean;
pub(crate) mod invariant_predicate_types;
pub(crate) mod predicate_expr;
#[cfg(feature = "revm")]
pub(crate) mod predicate_expr_revm;
#[cfg(feature = "rust-targets")]
mod rust_backends;

pub use adapter::{OutputBackend, ActionEmitter, EffectOrdering};
#[cfg(feature = "rust-targets")]
pub use adapter_rust::{SdkAdapter, ContainerAdapter, HostProfile, NativeHost, WasmHost};
#[cfg(feature = "rust-targets")]
pub use ackinacki::{AckiNackiAdapter, AckiNackiWasmHost, gen_solidity, gen_solidity_with_aliases, ackinacki_default_ser_fallback, cambrian_function_id_sig};
#[cfg(feature = "rust-targets")]
pub use rust_backends::{
    AckiNackiBackend, RustBackend, generate, generate_mapped, generate_mapped_primary,
    generate_per_entity, generate_per_entity_mapped,
};
pub use solidity::{gen_evm_solidity, gen_evm_solidity_opts};
pub use solidity::{find_unlowered_values, unlowered_value_report, UnloweredValue};
pub use lean::LeanBackend;
#[cfg(feature = "rust-targets")]
pub use expr::gen_expr;
pub use types::{
    gen_type, infer_expr_type, resolve_target_entity, extract_send_options,
    default_ser_be, is_complex_type, compute_default_state_hex,
    is_complex_rust_type, cambrian_function_id,
};

use crate::ast::*;
use crate::project::Project;
#[cfg(feature = "rust-targets")]
use crate::sourcemap::SourceMapper;
use std::collections::HashMap;


// ---------------------------------------------------------------------------
// Codegen context (replaces former thread-local state)
// ---------------------------------------------------------------------------

pub struct CodegenCtx {
    pub alias_map: HashMap<String, Type>,
    pub entity_routes: HashMap<String, HashMap<String, Vec<Type>>>,
    pub entity_init_routes: HashMap<String, String>,
}

impl CodegenCtx {
    pub fn new(program: &Program) -> Self {
        let mut entity_routes = HashMap::new();
        let mut entity_init_routes = HashMap::new();
        for entity in &program.entities {
            let mut routes: HashMap<String, Vec<Type>> = HashMap::new();
            for route in &entity.routes {
                let param_types: Vec<Type> = route.params.iter().map(|p| p.ty.clone()).collect();
                if route.is_init || route.name == "constructor" {
                    entity_init_routes.insert(entity.name.clone(), route.name.clone());
                }
                if route.name == "constructor" {
                    routes.insert("constructor".to_string(), param_types);
                } else {
                    routes.insert(route.name.clone(), param_types);
                }
            }
            entity_routes.insert(entity.name.clone(), routes);
        }
        CodegenCtx {
            alias_map: HashMap::new(),
            entity_routes,
            entity_init_routes,
        }
    }

    pub fn with_aliases(program: &Program, alias_map: HashMap<String, Type>) -> Self {
        let mut ctx = Self::new(program);
        ctx.alias_map = alias_map;
        ctx
    }

    pub fn resolve_type(&self, ty: &Type) -> Type {
        types::resolve_alias(ty, &self.alias_map)
    }

    pub fn lookup_route_types(&self, entity_name: &str, route_name: &str) -> Option<Vec<Type>> {
        self.entity_routes.get(entity_name)
            .and_then(|routes| routes.get(route_name))
            .cloned()
    }

    pub fn lookup_entity_for_route(&self, route_name: &str, exclude_entity: &str) -> Option<String> {
        let mut found: Option<String> = None;
        for (entity_name, routes) in &self.entity_routes {
            if entity_name == exclude_entity { continue; }
            if routes.contains_key(route_name) {
                if found.is_some() { return None; }
                found = Some(entity_name.clone());
            }
        }
        found
    }

    pub fn lookup_init_route(&self, entity_name: &str) -> &str {
        self.entity_init_routes.get(entity_name)
            .map(String::as_str)
            .unwrap_or("constructor")
    }
}

// ---------------------------------------------------------------------------
// Source-map helpers (shared with entity submodule)
// ---------------------------------------------------------------------------

// ===========================================================================
// EvmSolidityBackend — wraps gen_evm_solidity into OutputBackend
// ===========================================================================

pub struct EvmSolidityBackend {
    pub deterministic_addresses: bool,
}

impl OutputBackend for EvmSolidityBackend {
    fn gen_program(&self, program: &Program) -> String {
        evm::gen_evm_solidity(program, self.deterministic_addresses)
    }

    fn file_extension(&self) -> &str {
        "sol"
    }

    fn file_name_for_entity(&self, entity_name: &str) -> String {
        format!("src/{}.sol", entity_name)
    }

    fn gen_test_files(&self, program: &Program, _entity_name: &str) -> Vec<(String, String)> {
        let inv_cfg = crate::project::InvariantConfig::default();
        let mut files = evm_test_codegen::generate_evm_tests(program, self.deterministic_addresses, &inv_cfg);
        if !files.is_empty()
            || !program.tests.is_empty()
            || !program.fuzz_tests.is_empty()
            || !program.invariants.is_empty()
        {
            files.push((
                "foundry.toml".to_string(),
                evm_test_codegen::generate_foundry_toml(None, None, None),
            ));
            files.push(("setup.sh".to_string(), evm_test_codegen::generate_setup_sh()));
        }
        files
    }

    fn target_description(&self) -> &str {
        "EVM Solidity"
    }

    fn gen_project(&self, proj: &Project) -> Vec<(String, String)> {
        let det = proj.config.resolved_deterministic_addresses();
        let fuzz_cfg = proj.config.resolved_fuzz();
        let inv_cfg = proj.config.resolved_invariant();
        let mut all_files = Vec::new();

        let allow_ctor_payable = proj.config.resolved_evm().allow_constructor_payable();
        let code = evm::gen_evm_solidity_opts(&proj.merged, det, allow_ctor_payable);
        // Use a `_project.sol` suffix rather than the bare project name
        // to avoid case-insensitive collisions on macOS/Windows when an
        // entity is named the same as the project (e.g. project `governor`
        // and entity `Governor` would otherwise both want `src/governor.sol`).
        let project_file = format!("_{}_project.sol", proj.name());
        all_files.push((format!("src/{}", project_file), code));

        // Test files import per-entity (`../src/<Entity>.sol`), so emit
        // a thin stub for each entity that re-exports the project file
        // via `import` (Solidity propagates imported symbols transitively).
        for entity in &proj.merged.entities {
            let stub = format!(
                "// SPDX-License-Identifier: UNLICENSED\npragma solidity ^0.8.24;\n// Auto-generated stub: re-exports {} from the combined project file.\nimport \"./{}\";\n",
                entity.name, project_file,
            );
            all_files.push((format!("src/{}.sol", entity.name), stub));
        }

        let test_files = evm_test_codegen::generate_evm_tests_for_project(
            &proj.merged,
            det,
            &inv_cfg,
            Some(proj.name().as_str()),
            false,
        );
        all_files.extend(test_files);

        if !proj.merged.tests.is_empty()
            || !proj.merged.fuzz_tests.is_empty()
            || !proj.merged.invariants.is_empty()
        {
            let foundry_config = proj.config.foundry.as_ref();
            // Align `[profile.default.invariant].fail_on_revert` with
            // the per-decl flag: if *any* invariant in the program is
            // declared `#[fail_on_revert]`, the runner-level setting
            // is set to true so reverts surface in the test report.
            let any_fail_on_revert = proj.merged.invariants.iter().any(|i| i.fail_on_revert);
            let mut effective_inv = inv_cfg.clone();
            if any_fail_on_revert {
                effective_inv.fail_on_revert = true;
            }
            all_files.push((
                "foundry.toml".to_string(),
                evm_test_codegen::generate_foundry_toml(
                    foundry_config,
                    Some(&fuzz_cfg),
                    Some(&effective_inv),
                ),
            ));
            all_files.push(("setup.sh".to_string(), evm_test_codegen::generate_setup_sh()));
        }

        #[cfg(feature = "revm")]
        {
            all_files.extend(evm_revm_test_codegen::maybe_generate_revm_tests(
                proj, det, &fuzz_cfg, &inv_cfg,
            ));
        }

        all_files
    }
}

// ===========================================================================
// Shared route/transform walker
// ===========================================================================

#[cfg(feature = "rust-targets")]
pub(crate) fn cam_line_comment(sm: Option<&SourceMapper>, span: &Span) -> String {
    match sm {
        Some(mapper) if span.start != 0 || span.end != 0 => {
            format!("// @cam:{}\n", mapper.line_of(span.start))
        }
        _ => String::new(),
    }
}

#[cfg(feature = "rust-targets")]
pub(crate) fn count_lines(s: &str) -> usize {
    s.chars().filter(|&c| c == '\n').count()
}

/// A resolved member transform: the member being transformed, the transform
/// definition, and optionally which phase it belongs to.
pub struct ResolvedTransform<'a> {
    pub member: &'a Member,
    pub transform: &'a MemberTransform,
    pub phase: Option<&'a str>,
}

/// Collect all member transforms for a given route, optionally filtered to a
/// specific phase. If `phase_filter` is `None`, returns transforms that have
/// **no** phase annotation (unphased transforms). If `phase_filter` is
/// `Some(name)`, returns transforms annotated with that phase.
///
/// Returns the results in entity member declaration order (stable for
/// deterministic codegen).
pub fn collect_transforms<'a>(
    entity: &'a Entity,
    route_name: &str,
    phase_filter: Option<&str>,
) -> Vec<ResolvedTransform<'a>> {
    entity.members.iter()
        .filter(|m| !m.is_identity)
        .flat_map(|m| m.transforms.iter()
            .filter(move |t| t.route_name == route_name)
            .filter(move |t| match phase_filter {
                None => t.phase.is_none(),
                Some(name) => t.phase.as_deref() == Some(name),
            })
            .map(move |t| ResolvedTransform {
                member: m,
                transform: t,
                phase: t.phase.as_deref(),
            })
        )
        .collect()
}

/// Collect **all** transforms for a route (phased and unphased) in entity
/// member declaration order.
pub fn collect_all_transforms<'a>(
    entity: &'a Entity,
    route_name: &str,
) -> Vec<ResolvedTransform<'a>> {
    entity.members.iter()
        .filter(|m| !m.is_identity)
        .flat_map(|m| m.transforms.iter()
            .filter(move |t| t.route_name == route_name)
            .map(move |t| ResolvedTransform {
                member: m,
                transform: t,
                phase: t.phase.as_deref(),
            })
        )
        .collect()
}

/// A member read an outbound effect performs against state the enclosing
/// phase is about to commit over, captured into a local ahead of the
/// `{ s with … }` / storage commit.
///
/// A phase is atomic: its member transforms and its effects all observe the
/// state as it stood when the phase began. The transforms already do (each is
/// a function of the pre-phase state); effects are emitted *after* the commit
/// (checks-effects-interactions), so every read of a same-phase-committed
/// member must be snapshotted first. Shared between the Lean emitter and
/// `cambrian-predict` so the two cannot drift; the EVM backend applies the
/// same semantics through its IR path (`solidity/evm/route.rs`,
/// `gen_updates_with_snapshots_from_phase_ir`).
pub struct PrecommitSnapshot {
    /// The read as written in the source (`m_total`, `m_shares[msg::sender]`).
    pub read: Expr,
    /// The `_pre_<member>_<i>` local the read is captured into.
    pub local: String,
}

/// The pre-commit snapshots of `actions`: reads of `committed` members inside
/// *outbound* actions only (send / var-call / deploy / effect / call /
/// updateCode). A scalar member is captured whole; a mapping only where it is
/// indexed. Discovery order (first occurrence) names the locals.
pub fn precommit_snapshots(
    entity: &Entity,
    committed: &std::collections::HashSet<String>,
    actions: &[RouteAction],
) -> Vec<PrecommitSnapshot> {
    use solidity::evm::route::{action_leaves_contract, committed_reads_in, map_action_exprs};
    let outbound: std::cell::RefCell<Vec<Expr>> = std::cell::RefCell::new(Vec::new());
    for action in actions {
        map_action_exprs(action, &|owner, e| {
            if action_leaves_contract(owner) {
                outbound.borrow_mut().push(e.clone());
            }
            e.clone()
        });
    }
    committed_reads_in(entity, committed, &outbound.into_inner())
        .into_iter()
        .enumerate()
        .map(|(i, read)| {
            let member = match &read {
                Expr::Index(base, _) => match base.as_ref() {
                    Expr::Ident(n) => n.clone(),
                    _ => unreachable!("only ident-based indexes are recorded"),
                },
                Expr::Ident(n) => n.clone(),
                _ => unreachable!("only idents and indexes are recorded"),
            };
            PrecommitSnapshot {
                read,
                local: format!("_pre_{}_{}", member, i),
            }
        })
        .collect()
}

/// `actions` with every snapshotted read inside an outbound action replaced
/// by its `_pre_*` local. Non-outbound positions (returns, `if` conditions,
/// `let` values) are left reading the committed state, mirroring the EVM
/// backend.
pub fn rewrite_actions_precommit(
    actions: &[RouteAction],
    snapshots: &[PrecommitSnapshot],
) -> Vec<RouteAction> {
    use solidity::core::types::subst_expr_where;
    use solidity::evm::route::{action_leaves_contract, map_action_exprs};
    if snapshots.is_empty() {
        return actions.to_vec();
    }
    actions
        .iter()
        .map(|a| {
            map_action_exprs(a, &|owner, e| {
                if !action_leaves_contract(owner) {
                    return e.clone();
                }
                subst_expr_where(e, &|inner| {
                    snapshots
                        .iter()
                        .find(|s| &s.read == inner)
                        .map(|s| Expr::Ident(s.local.clone()))
                })
            })
        })
        .collect()
}

/// Reorder a list of resolved transforms according to temporal dependency
/// ordering. Members not present in the temporal order are placed at the end.
pub fn order_transforms_temporally<'a>(
    mut transforms: Vec<ResolvedTransform<'a>>,
    temporal_orders: &[crate::validate::TemporalOrder],
    route_name: &str,
) -> Vec<ResolvedTransform<'a>> {
    if let Some(ord) = temporal_orders.iter().find(|o| o.route_name == route_name) {
        transforms.sort_by_key(|rt| {
            ord.order.iter().position(|n| n == &rt.member.name).unwrap_or(usize::MAX)
        });
    }
    transforms
}

pub fn dispatch_action(
    emitter: &dyn ActionEmitter,
    action: &RouteAction,
    entity: &Entity,
    route: &Route,
    program: &Program,
    indent: &str,
) -> String {
    match action {
        RouteAction::Let { pattern, value } => {
            emitter.emit_let(pattern, value, entity, route, program, indent)
        }
        RouteAction::Return { values } => {
            emitter.emit_return(values, entity, route, program, indent)
        }
        RouteAction::Throw { error_code } => {
            emitter.emit_throw(*error_code, indent)
        }
        RouteAction::ThrowCustom { name, args } => {
            emitter.emit_throw_custom(name, args, entity, route, program, indent)
        }
        RouteAction::Send { message, args, dest, send_options } => {
            emitter.emit_send(message, args, dest, send_options.as_ref(), entity, route, program, indent)
        }
        RouteAction::Deploy { entity: target_entity, send_options, constructor_args } => {
            emitter.emit_deploy(target_entity, send_options.as_ref(), constructor_args, entity, route, program, indent)
        }
        RouteAction::Conditional { condition, then_actions, else_actions } => {
            let inner_indent = format!("{}    ", indent);
            let then_code: String = then_actions.iter()
                .map(|a| dispatch_action(emitter, a, entity, route, program, &inner_indent))
                .collect();
            let else_code: String = else_actions.iter()
                .map(|a| dispatch_action(emitter, a, entity, route, program, &inner_indent))
                .collect();
            emitter.format_conditional(condition, &then_code, &else_code, entity, route, indent)
        }
        RouteAction::Rescue { tag, action: inner } => {
            let inner_indent = format!("{}    ", indent);
            let inner_code = dispatch_action(emitter, inner, entity, route, program, &inner_indent);
            emitter.format_rescue(tag, &inner_code, inner, entity, route, program, indent)
        }
        RouteAction::CallRoute { name, args } => {
            emitter.emit_call_route(name, args, entity, route, indent)
        }
        RouteAction::Effect { namespace, name, args } => {
            emitter.emit_effect(namespace, name, args, entity, route, indent)
        }
        RouteAction::VarCall { name, message, args, dest, send_options } => {
            emitter.emit_var_call(name, message, args, dest, send_options.as_ref(), entity, route, program, indent)
        }
        RouteAction::UpdateCode { update_args, callback_route, callback_args } => {
            emitter.emit_update_code(update_args, callback_route, callback_args, entity, route, program, indent)
        }
        RouteAction::For { pattern, iter, body } => {
            let inner_indent = format!("{}    ", indent);
            let body_code: String = body.iter()
                .map(|a| dispatch_action(emitter, a, entity, route, program, &inner_indent))
                .collect();
            emitter.format_for(pattern, iter, &body_code, entity, route, indent)
        }
        RouteAction::Emit { event_name, args } => {
            emitter.emit_emit(event_name, args, entity, route, program, indent)
        }
    }
}
