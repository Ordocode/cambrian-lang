// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use super::entity;
use super::Diagnostic;
use crate::ast::*;
use std::collections::{HashMap, HashSet};

/// Bindings introduced by `deploy <name> = <Entity>(...)`, in declaration
/// order, so a later `call <name>.route(...)` can be resolved against the
/// right entity.
type PeerBindings<'a> = HashMap<String, &'a Entity>;

/// Walk a test/fuzz/property body once and check every `deploy` step and
/// every qualified `call <peer>.route(...)`, returning the bindings so the
/// caller can keep using them.
///
/// Split out because `validate_test`, `validate_fuzz` and `validate_property`
/// each own a near-identical step loop; peer checks are the same in all
/// three and duplicating them three ways is how the arity check on the
/// entity under test already drifted between them.
fn validate_peer_steps<'a>(
    what: &str,
    body: &[TestStep],
    entities: &'a [Entity],
    diags: &mut Vec<Diagnostic>,
) -> PeerBindings<'a> {
    let mut peers: PeerBindings<'a> = HashMap::new();

    for step in body {
        match step {
            TestStep::DeployPeer {
                binding,
                entity: peer_name,
                args,
                init_state,
            } => {
                // T24: the deployed entity has to exist.
                let peer = match entities.iter().find(|e| &e.name == peer_name) {
                    Some(e) => e,
                    None => {
                        diags.push(Diagnostic::error(
                            "T24",
                            format!(
                                "{}: deploy {} = {}(...): entity '{}' not found",
                                what, binding, peer_name, peer_name
                            ),
                        ));
                        continue;
                    }
                };

                // T25: one name, one contract. Rebinding would make every
                // later `call <binding>.…` ambiguous to a reader.
                if peers.contains_key(binding) {
                    diags.push(Diagnostic::error(
                        "T25",
                        format!(
                            "{}: '{}' is already bound to a deployed contract",
                            what, binding
                        ),
                    ));
                }

                // T26: arity against the constructor's declared parameters.
                // Identity members are not parameters — they arrive through
                // the `with { ... }` clause — so this compares against
                // `params` exactly as the entity-under-test check does.
                if let Some(ctor) = peer.routes.iter().find(|r| r.name == "constructor") {
                    if args.len() != ctor.params.len() {
                        diags.push(Diagnostic::error(
                            "T26",
                            format!(
                                "{}: deploy {} = {}(...) expects {} constructor arg(s), got {}",
                                what,
                                binding,
                                peer_name,
                                ctor.params.len(),
                                args.len()
                            ),
                        ));
                    }
                } else if !args.is_empty() {
                    diags.push(Diagnostic::error("T26",
                        format!("{}: deploy {} = {}(...): entity '{}' declares no constructor, got {} arg(s)",
                            what, binding, peer_name, peer_name, args.len())));
                }

                // T27: seeded fields have to be members of the peer.
                for (field, _) in init_state {
                    if !peer.members.iter().any(|m| &m.name == field) {
                        diags.push(Diagnostic::error(
                            "T27",
                            format!(
                                "{}: deploy {} = {}(...): '{}' is not a member of entity '{}'",
                                what, binding, peer_name, field, peer_name
                            ),
                        ));
                    }
                }

                peers.insert(binding.clone(), peer);
            }
            TestStep::Call {
                target: Some(binding),
                route,
                args,
            } => {
                // T28: the binding has to exist, and has to exist *already* —
                // the generated Solidity declares each peer as a local, so a
                // forward reference would not compile.
                let peer = match peers.get(binding) {
                    Some(p) => *p,
                    None => {
                        diags.push(Diagnostic::error(
                            "T28",
                            format!(
                                "{}: call {}.{}(...): '{}' is not a deployed contract; \
                                    add `deploy {} = <Entity>(...)` before this step",
                                what, binding, route, binding, binding
                            ),
                        ));
                        continue;
                    }
                };

                // T29: route existence and arity on the peer.
                match peer.routes.iter().find(|r| &r.name == route) {
                    Some(r) if args.len() != r.params.len() => {
                        diags.push(Diagnostic::error(
                            "T29",
                            format!(
                                "{}: call {}.{}() expects {} arg(s), got {}",
                                what,
                                binding,
                                route,
                                r.params.len(),
                                args.len()
                            ),
                        ));
                    }
                    Some(_) => {}
                    None => {
                        diags.push(Diagnostic::error(
                            "T29",
                            format!(
                                "{}: route '{}' not found in entity '{}' (bound as '{}')",
                                what, route, peer.name, binding
                            ),
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    peers
}

fn lookup_event_decl<'a>(
    name: &str,
    entity: &'a Entity,
    program: &'a crate::ast::Program,
) -> Option<&'a crate::ast::EventDecl> {
    entity
        .events
        .iter()
        .find(|e| e.name == name)
        .or_else(|| program.events.iter().find(|e| e.name == name))
}

fn validate_expect_emit(
    scope: &str,
    event_name: &str,
    args: &[crate::ast::Expr],
    entity: &Entity,
    program: &crate::ast::Program,
    diags: &mut Vec<Diagnostic>,
) {
    match lookup_event_decl(event_name, entity, program) {
        None => {
            diags.push(Diagnostic::error(
                "V34",
                format!(
                    "{}: expect emit references undeclared event '{}'",
                    scope, event_name
                ),
            ));
        }
        Some(decl) => {
            if decl.params.len() != args.len() {
                diags.push(Diagnostic::error(
                    "V35",
                    format!(
                        "{}: expect emit '{}' arity mismatch — declaration takes {} arg(s), got {}",
                        scope,
                        event_name,
                        decl.params.len(),
                        args.len()
                    ),
                ));
            }
        }
    }
}

pub(super) fn validate_test(test: &TestDecl, program: &crate::ast::Program, diags: &mut Vec<Diagnostic>) {
    let entities = &program.entities;
    // T1
    let entity = entities.iter().find(|e| e.name == test.entity_name);
    if entity.is_none() {
        diags.push(
            Diagnostic::error(
                "T1",
                format!(
                    "test \"{}\": entity '{}' not found",
                    test.name, test.entity_name
                ),
            )
            .with_span(test.span),
        );
        return;
    }
    let entity = entity.unwrap();
    let member_names: HashSet<&str> = entity.members.iter().map(|m| m.name.as_str()).collect();
    let route_map: HashMap<&str, &Route> =
        entity.routes.iter().map(|r| (r.name.as_str(), r)).collect();

    // T4
    for (field, _) in &test.init_state {
        if !member_names.contains(field.as_str()) {
            diags.push(Diagnostic::error(
                "T4",
                format!(
                    "test \"{}\": '{}' is not a member of entity '{}'",
                    test.name, field, test.entity_name
                ),
            ));
        }
    }

    validate_peer_steps(
        &format!("test \"{}\"", test.name),
        &test.body,
        entities,
        diags,
    );

    let mut has_call = false;

    for step in &test.body {
        match step {
            // A peer call is still a call: `expect` after it is well-formed,
            // and T5 must not fire.
            TestStep::Call {
                target: Some(_), ..
            } => {
                has_call = true;
            }
            // Already checked by validate_peer_steps above.
            TestStep::DeployPeer { .. } => {}
            TestStep::Call {
                target: None,
                route,
                args,
            } => {
                has_call = true;
                // T2
                if let Some(r) = route_map.get(route.as_str()) {
                    // T3
                    if args.len() != r.params.len() {
                        diags.push(Diagnostic::error(
                            "T3",
                            format!(
                                "test \"{}\": call {}() expects {} args, got {}",
                                test.name,
                                route,
                                r.params.len(),
                                args.len()
                            ),
                        ));
                    }
                } else {
                    diags.push(Diagnostic::error(
                        "T2",
                        format!(
                            "test \"{}\": route '{}' not found in entity '{}'",
                            test.name, route, test.entity_name
                        ),
                    ));
                }
            }
            TestStep::ExpectState { fields } => {
                // T5
                if !has_call {
                    diags.push(Diagnostic::error(
                        "T5",
                        format!("test \"{}\": 'expect state' before any 'call'", test.name),
                    ));
                }
                // T4
                for (path, _) in fields {
                    if let Some(PathSegment::Field(root)) = path.first() {
                        if !member_names.contains(root.as_str()) {
                            diags.push(Diagnostic::error(
                                "T4",
                                format!(
                                    "test \"{}\": '{}' is not a member of entity '{}'",
                                    test.name, root, test.entity_name
                                ),
                            ));
                        }
                    }
                }
            }
            TestStep::ExpectEmit { event_name, args } => {
                validate_expect_emit(
                    &format!("test \"{}\"", test.name),
                    event_name,
                    args,
                    entity,
                    program,
                    diags,
                );
            }
            TestStep::ExpectThrow { .. }
            | TestStep::ExpectReturn { .. }
            | TestStep::ExpectReturnTuple { .. }
            | TestStep::ExpectReturnLens { .. }
            | TestStep::ExpectEffects { .. } => {
                if !has_call {
                    diags.push(Diagnostic::error(
                        "T5",
                        format!("test \"{}\": 'expect' before any 'call'", test.name),
                    ));
                }
            }
            TestStep::ExpectPred { .. } => {
                // T5 / T38: validate_expect_preds
            }
            TestStep::SetContext { namespace, .. } => {
                let known = ["msg", "sys"];
                if !known.contains(&namespace.as_str()) {
                    diags.push(Diagnostic::warning(
                        "T8",
                        format!(
                            "test \"{}\": unknown context namespace '{}' (known: msg, sys)",
                            test.name, namespace
                        ),
                    ));
                }
            }
            TestStep::Let { .. } => {}
            TestStep::SetRegistry { entity_name, .. } => {
                if entities.iter().find(|e| e.name == *entity_name).is_none() {
                    diags.push(Diagnostic::warning(
                        "T9",
                        format!(
                            "test \"{}\": registry entity '{}' not found among declared entities",
                            test.name, entity_name
                        ),
                    ));
                }
            }
            TestStep::Assume { .. } => {
                diags.push(Diagnostic::error(
                    "T12",
                    format!(
                        "test \"{}\": 'assume' is only allowed inside 'fuzz' blocks",
                        test.name
                    ),
                ));
            }
            TestStep::Bound { .. } => {
                diags.push(Diagnostic::error(
                    "T11",
                    format!(
                        "test \"{}\": 'bound' is only allowed inside 'fuzz' blocks",
                        test.name
                    ),
                ));
            }
            TestStep::SkipIf { .. } => {
                diags.push(Diagnostic::error(
                    "T15",
                    format!(
                        "test \"{}\": 'skip if' is only allowed inside invariant action bodies",
                        test.name
                    ),
                ));
            }
            TestStep::AdvanceTime { .. } => {
                diags.push(Diagnostic::error("T16",
                    format!("test \"{}\": 'advanceTime(...)' is only allowed inside invariant action bodies (and requires `#[with_time]` on the invariant)",
                        test.name)));
            }
        }
    }

    // T6
    if !has_call {
        diags.push(Diagnostic::error(
            "T6",
            format!("test \"{}\": must contain at least one 'call'", test.name),
        ));
    }

    validate_typed_spec_lets(
        &format!("test \"{}\"", test.name),
        &test.body,
        &HashSet::new(),
        diags,
    );
    validate_expect_preds(
        &format!("test \"{}\"", test.name),
        &test.body,
        entity,
        entities,
        &HashSet::new(),
        diags,
    );
}

fn expr_is_non_boolean(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Tuple(_)
            | Expr::RecordConstruct(..)
            | Expr::RecordUpdate(..)
            | Expr::EmptyCollection
            | Expr::ArrayLit(_)
    )
}

/// T5 (predicate before any call) and T38 for `expect <bool>`.
///
/// `result` is the preceding call's return value. Mentioning it on a void
/// route, or when a parameter, `let`, or member of the same name is in
/// scope, is T38.
pub(super) fn validate_expect_preds(
    label: &str,
    body: &[TestStep],
    entity: &Entity,
    entities: &[Entity],
    param_names: &HashSet<&str>,
    diags: &mut Vec<Diagnostic>,
) {
    let mut peers: HashMap<&str, &Entity> = HashMap::new();
    let mut lets: HashSet<&str> = HashSet::new();
    let mut has_call = false;
    let mut last_returns: Option<bool> = None;
    let mut last_route = String::new();

    for step in body {
        match step {
            TestStep::DeployPeer {
                binding,
                entity: peer_name,
                ..
            } => {
                if let Some(peer) = entities.iter().find(|e| e.name == *peer_name) {
                    peers.insert(binding.as_str(), peer);
                }
            }
            TestStep::Let { name, .. } => {
                lets.insert(name.as_str());
            }
            TestStep::Call { target, route, .. } => {
                has_call = true;
                last_route = route.clone();
                let callee = match target {
                    None => Some(entity),
                    Some(binding) => peers.get(binding.as_str()).copied(),
                };
                last_returns = callee.and_then(|e| {
                    e.routes
                        .iter()
                        .find(|r| r.name == *route)
                        .map(|r| r.return_type.is_some())
                });
            }
            TestStep::ExpectPred { cond } => {
                if !has_call {
                    diags.push(Diagnostic::error(
                        "T5",
                        format!("{label}: 'expect' before any 'call'"),
                    ));
                }
                if expr_is_non_boolean(cond) {
                    diags.push(Diagnostic::error(
                        "T38",
                        format!(
                            "{label}: 'expect' expression must be a boolean, got non-scalar literal"
                        ),
                    ));
                }
                if expr_mentions_ident(cond, EXPECT_RESULT_NAME) {
                    if last_returns == Some(false) {
                        diags.push(Diagnostic::error(
                            "T38",
                            format!(
                                "{label}: 'expect' mentions 'result' but route '{last_route}' has no declared return type"
                            ),
                        ));
                    }
                    let member_clash = entity.members.iter().any(|m| m.name == EXPECT_RESULT_NAME);
                    if param_names.contains(EXPECT_RESULT_NAME)
                        || lets.contains(EXPECT_RESULT_NAME)
                        || member_clash
                    {
                        diags.push(Diagnostic::error(
                            "T38",
                            format!(
                                "{label}: 'result' in 'expect' is the call's return value, but that name is already a parameter, let, or member"
                            ),
                        ));
                    }
                }
            }
            _ => {}
        }
    }
}

/// Target-specific guidance used by W7 warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FuzzBackendKind {
    Foundry,
    AckiNacki,
}

/// Check that a Cambrian type can serve as a fuzz parameter for the given backend.
/// Returns a list of `(rule_code, message)` tuples — empty when the type is fully supported.
pub(crate) fn fuzz_param_support_warnings(
    test_name: &str,
    param_name: &str,
    ty: &Type,
    backend: FuzzBackendKind,
) -> Vec<(&'static str, String)> {
    let kind_label = match backend {
        FuzzBackendKind::Foundry => "Foundry",
        FuzzBackendKind::AckiNacki => "Acki Nacki",
    };
    let supported = match ty {
        Type::Simple(name) => matches!(
            name.as_str(),
            "u8" | "u16"
                | "u32"
                | "u64"
                | "u128"
                | "u256"
                | "U256"
                | "i8"
                | "i16"
                | "i32"
                | "i64"
                | "i128"
                | "bool"
                | "address"
                | "bytes"
                | "bytes32"
                | "bytes4"
                | "string"
        ),
        _ => false,
    };
    if supported {
        Vec::new()
    } else {
        vec![("W7", format!(
            "fuzz \"{}\": parameter '{}' of type {:?} may not be fully supported on {} target; the harness will be emitted but the test may degrade or be skipped",
            test_name, param_name, ty, kind_label))]
    }
}

/// Emit W7 warnings for the given backends only (P7 target-aware).
pub(crate) fn emit_w7_for_backends(
    program: &Program,
    backends: &[FuzzBackendKind],
    diags: &mut Vec<Diagnostic>,
) {
    for prop in &program.properties {
        for p in &prop.params {
            for backend in backends {
                for (code, msg) in fuzz_param_support_warnings(&prop.name, &p.name, &p.ty, *backend)
                {
                    diags.push(Diagnostic::warning(code, msg));
                }
            }
        }
    }
    for inv in &program.invariants {
        for action in &inv.actions {
            for p in &action.params {
                for backend in backends {
                    for (code, msg) in
                        fuzz_param_support_warnings(&inv.name, &p.name, &p.ty, *backend)
                    {
                        diags.push(Diagnostic::warning(code, msg));
                    }
                }
            }
        }
    }
}

/// Validate an abstract `property` declaration and its nested
/// `test` / `fuzz` instances. Rule codes:
/// - T1: `for Entity` must reference a known entity.
/// - T2/T3: `call route(...)` route existence / arity.
/// - T4: `with { ... }` and `expect state { ... }` fields must be members.
/// - T5/T6: `expect*` must follow a `call`; at least one `call` required.
/// - T8: unknown context namespace.
/// - T10: parameter shadows an entity member; `let` shadows a parameter.
/// - T11: a `bound` / range binding references a name that is not a
///   property parameter.
/// - T12: `assume` expression must be boolean-ish.
/// - T15/T16: `skip if` / `advanceTime(...)` are invariant-only — never
///   legal in a property body.
/// - T17: an instance binding uses the wrong form for its kind
///   (`test` requires `p: value`, `fuzz` requires `p in lo..hi`).
/// - T24: a `fuzz` / invariant `bound` range with literal endpoints is empty
///   (`lo > hi`, or exclusive `lo >= hi`).
/// - T18: a `test` instance must bind every property parameter to a
///   concrete value.
/// Blockchain/context fields that may be forall-ized via `msg::x: *` /
/// `sys::x: *`. Mirrors `remap_ctx_field` in `codegen/lean_property.rs`.
pub(super) fn known_ctx_field(namespace: &str, field: &str) -> bool {
    match namespace {
        "msg" => matches!(field, "sender" | "value"),
        "sys" => matches!(
            field,
            "now"
                | "timestamp"
                | "chainid"
                | "chain_id"
                | "block_number"
                | "blockNumber"
                | "balance"
        ),
        _ => false,
    }
}

/// Validate a `with { ... }` forall spec against an entity's members and the
/// concrete pins declared alongside it. Emits T19 (unknown state field),
/// T20 (unknown context param) and T21 (field both pinned and forall-ized).
pub(super) fn validate_forall_spec(
    ctx_label: &str,
    spec: &ForallSpec,
    pins: &[(String, Expr)],
    member_names: &HashSet<&str>,
    diags: &mut Vec<Diagnostic>,
) {
    let pinned: HashSet<&str> = pins.iter().map(|(n, _)| n.as_str()).collect();
    for t in &spec.targets {
        match t {
            ForallTarget::StateField(f) => {
                // T19
                if !member_names.contains(f.as_str()) {
                    diags.push(Diagnostic::error(
                        "T19",
                        format!(
                            "{}: forall field '{}: *' is not a member of the entity",
                            ctx_label, f
                        ),
                    ));
                }
                // T21: an explicit `f: *` cannot also be pinned.
                if pinned.contains(f.as_str()) {
                    diags.push(Diagnostic::error("T21",
                        format!("{}: field '{}' is both pinned and forall-ized ('{}: *') in the same 'with'",
                            ctx_label, f, f)));
                }
            }
            ForallTarget::Context { namespace, field } => {
                // T22: context params no longer live in `with { ... }` /
                // `init { ... }`; they belong in the dedicated `ctx { ... }`
                // block alongside entity state.
                diags.push(Diagnostic::error("T22",
                    format!("{}: context param '{}::{}' must be declared in a 'ctx {{ ... }}' block, not in 'with'/'init' (entity state and context are separate records)",
                        ctx_label, namespace, field)));
            }
        }
    }
}

/// Validate a dedicated `ctx { ... }` block: every entry must name a known
/// blockchain/context parameter (T20), and no field may be declared twice
/// (T23).
pub(super) fn validate_context_spec(
    ctx_label: &str,
    spec: &ContextSpec,
    diags: &mut Vec<Diagnostic>,
) {
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    for e in &spec.entries {
        if !known_ctx_field(&e.namespace, &e.field) {
            diags.push(Diagnostic::error("T20",
                format!("{}: context param '{}::{}' is unknown (allowed: msg::{{sender,value}}, sys::{{now,timestamp,chainid,block_number,balance}})",
                    ctx_label, e.namespace, e.field)));
        }
        if !seen.insert((e.namespace.as_str(), e.field.as_str())) {
            diags.push(Diagnostic::error(
                "T23",
                format!(
                    "{}: context param '{}::{}' is declared more than once in the same 'ctx' block",
                    ctx_label, e.namespace, e.field
                ),
            ));
        }
    }
}

pub(super) fn validate_property(
    prop: &PropertyDecl,
    entities: &[Entity],
    diags: &mut Vec<Diagnostic>,
) {
    // T1
    let entity = entities.iter().find(|e| e.name == prop.entity_name);
    if entity.is_none() {
        diags.push(
            Diagnostic::error(
                "T1",
                format!(
                    "property \"{}\": entity '{}' not found",
                    prop.name, prop.entity_name
                ),
            )
            .with_span(prop.span),
        );
        return;
    }
    let entity = entity.unwrap();
    let member_names: HashSet<&str> = entity.members.iter().map(|m| m.name.as_str()).collect();
    let route_map: HashMap<&str, &Route> =
        entity.routes.iter().map(|r| (r.name.as_str(), r)).collect();
    let param_names: HashSet<&str> = prop.params.iter().map(|p| p.name.as_str()).collect();

    // T10: parameter names must not collide with entity members.
    for p in &prop.params {
        if member_names.contains(p.name.as_str()) {
            diags.push(Diagnostic::error(
                "T10",
                format!(
                    "property \"{}\": parameter '{}' shadows entity member '{}'",
                    prop.name, p.name, p.name
                ),
            ));
        }
    }

    // T13/W7: deferred to target-aware `rules/w7.rs` (reads ValidateCtx::target).

    // T4: default init_state fields must be valid members.
    for (field, _) in &prop.init_state {
        if !member_names.contains(field.as_str()) {
            diags.push(Diagnostic::error(
                "T4",
                format!(
                    "property \"{}\": '{}' is not a member of entity '{}'",
                    prop.name, field, prop.entity_name
                ),
            ));
        }
    }

    // T19/T21/T22: property-level forall spec.
    validate_forall_spec(
        &format!("property \"{}\"", prop.name),
        &prop.forall_state,
        &prop.init_state,
        &member_names,
        diags,
    );
    // T20/T23: property-level ctx block.
    validate_context_spec(&format!("property \"{}\"", prop.name), &prop.context, diags);

    validate_peer_steps(
        &format!("property \"{}\"", prop.name),
        &prop.body,
        entities,
        diags,
    );

    let mut has_call = false;

    for step in &prop.body {
        match step {
            TestStep::Call {
                target: Some(_), ..
            } => {
                has_call = true;
            }
            // Already checked by validate_peer_steps above.
            TestStep::DeployPeer { .. } => {}
            TestStep::Call {
                target: None,
                route,
                args,
            } => {
                has_call = true;
                if let Some(r) = route_map.get(route.as_str()) {
                    if args.len() != r.params.len() {
                        diags.push(Diagnostic::error(
                            "T3",
                            format!(
                                "property \"{}\": call {}() expects {} args, got {}",
                                prop.name,
                                route,
                                r.params.len(),
                                args.len()
                            ),
                        ));
                    }
                } else {
                    diags.push(Diagnostic::error(
                        "T2",
                        format!(
                            "property \"{}\": route '{}' not found in entity '{}'",
                            prop.name, route, prop.entity_name
                        ),
                    ));
                }
            }
            TestStep::ExpectState { fields } => {
                if !has_call {
                    diags.push(Diagnostic::error(
                        "T5",
                        format!(
                            "property \"{}\": 'expect state' before any 'call'",
                            prop.name
                        ),
                    ));
                }
                for (path, _) in fields {
                    if let Some(PathSegment::Field(root)) = path.first() {
                        if !member_names.contains(root.as_str()) {
                            diags.push(Diagnostic::error(
                                "T4",
                                format!(
                                    "property \"{}\": '{}' is not a member of entity '{}'",
                                    prop.name, root, prop.entity_name
                                ),
                            ));
                        }
                    }
                }
            }
            TestStep::ExpectEmit { .. } => {
                diags.push(Diagnostic::error(
                    "T36",
                    format!(
                        "property \"{}\": expect emit is only allowed in test blocks",
                        prop.name
                    ),
                ));
            }
            TestStep::ExpectThrow { .. }
            | TestStep::ExpectReturn { .. }
            | TestStep::ExpectReturnTuple { .. }
            | TestStep::ExpectReturnLens { .. }
            | TestStep::ExpectEffects { .. } => {
                if !has_call {
                    diags.push(Diagnostic::error(
                        "T5",
                        format!("property \"{}\": 'expect' before any 'call'", prop.name),
                    ));
                }
            }
            TestStep::ExpectPred { .. } => {
                // T5 / T38: validate_expect_preds
            }
            TestStep::SetContext { namespace, .. } => {
                let known = ["msg", "sys"];
                if !known.contains(&namespace.as_str()) {
                    diags.push(Diagnostic::warning(
                        "T8",
                        format!(
                            "property \"{}\": unknown context namespace '{}' (known: msg, sys)",
                            prop.name, namespace
                        ),
                    ));
                }
            }
            TestStep::Let { name, .. } => {
                if param_names.contains(name.as_str()) {
                    diags.push(Diagnostic::error(
                        "T10",
                        format!(
                            "property \"{}\": let binding '{}' shadows parameter '{}'",
                            prop.name, name, name
                        ),
                    ));
                }
            }
            TestStep::SetRegistry { entity_name, .. } => {
                if entities.iter().find(|e| e.name == *entity_name).is_none() {
                    diags.push(Diagnostic::warning("T9",
                        format!("property \"{}\": registry entity '{}' not found among declared entities",
                            prop.name, entity_name)));
                }
            }
            TestStep::Assume { cond } => {
                // T12: lightweight boolean-ish check.
                match cond {
                    Expr::Tuple(_)
                    | Expr::RecordConstruct(..)
                    | Expr::RecordUpdate(..)
                    | Expr::EmptyCollection
                    | Expr::ArrayLit(_) => {
                        diags.push(Diagnostic::error("T12",
                            format!("property \"{}\": 'assume' expression must be a boolean, got non-scalar literal",
                                prop.name)));
                    }
                    _ => {}
                }
            }
            TestStep::Bound { .. } => {
                // Sampling bounds belong in `fuzz` instances, not the
                // abstract property body (they would otherwise pollute the
                // Lean theorem). T11 with property-specific guidance.
                diags.push(Diagnostic::error("T11",
                    format!("property \"{}\": 'bound' is not allowed in the property body; move it into a 'fuzz {{ p in lo..hi }}' instance",
                        prop.name)));
            }
            TestStep::SkipIf { .. } => {
                diags.push(Diagnostic::error(
                    "T15",
                    format!(
                        "property \"{}\": 'skip if' is only allowed inside invariant action bodies",
                        prop.name
                    ),
                ));
            }
            TestStep::AdvanceTime { .. } => {
                diags.push(Diagnostic::error("T16",
                    format!("property \"{}\": 'advanceTime(...)' is only allowed inside invariant action bodies",
                        prop.name)));
            }
        }
    }

    // T6
    if !has_call {
        diags.push(Diagnostic::error(
            "T6",
            format!(
                "property \"{}\": must contain at least one 'call'",
                prop.name
            ),
        ));
    }

    validate_typed_spec_lets(
        &format!("property \"{}\"", prop.name),
        &prop.body,
        &param_names,
        diags,
    );
    validate_expect_preds(
        &format!("property \"{}\"", prop.name),
        &prop.body,
        entity,
        entities,
        &param_names,
        diags,
    );

    let declared_params: Vec<&str> = prop.params.iter().map(|p| p.name.as_str()).collect();
    for inst in &prop.instances {
        let kind_label = match inst.kind {
            PropertyInstanceKind::Test => "test",
            PropertyInstanceKind::Fuzz => "fuzz",
        };
        let inst_label = inst.name.clone().unwrap_or_else(|| "<unnamed>".to_string());

        let mut bound_here: HashSet<&str> = HashSet::new();
        for (name, arg) in &inst.bindings {
            // T11: the binding must reference a declared parameter.
            if !param_names.contains(name.as_str()) {
                diags.push(Diagnostic::error("T11",
                    format!("property \"{}\": {} instance '{}' binds '{}', which is not a property parameter (declared: {})",
                        prop.name, kind_label, inst_label, name, declared_params.join(", "))));
            }
            if !bound_here.insert(name.as_str()) {
                diags.push(Diagnostic::error(
                    "T11",
                    format!(
                        "property \"{}\": {} instance '{}' binds parameter '{}' more than once",
                        prop.name, kind_label, inst_label, name
                    ),
                ));
            }
            // T17: binding form must match the instance kind.
            match (inst.kind, arg) {
                (PropertyInstanceKind::Test, InstanceArg::Range { .. }) => {
                    diags.push(Diagnostic::error("T17",
                        format!("property \"{}\": test instance '{}' must bind '{}' to a concrete value ('{}: value'), not a range",
                            prop.name, inst_label, name, name)));
                }
                (PropertyInstanceKind::Fuzz, InstanceArg::Concrete(_)) => {
                    diags.push(Diagnostic::error("T17",
                        format!("property \"{}\": fuzz instance '{}' must bind '{}' to a range ('{} in lo..hi'), not a concrete value",
                            prop.name, inst_label, name, name)));
                }
                _ => {}
            }
            if inst.kind == PropertyInstanceKind::Fuzz {
                if let InstanceArg::Range { lo, hi, inclusive } = arg {
                    check_literal_range_endpoints(
                        &format!(
                            "property \"{}\": fuzz instance '{}'",
                            prop.name, inst_label
                        ),
                        name,
                        lo,
                        hi,
                        *inclusive,
                        diags,
                    );
                }
            }
        }

        // T18: a test instance must provide a concrete value for every parameter.
        // Linked `#[instantiates]` tests reuse the catalog property surface (T37 pins only).
        if inst.kind == PropertyInstanceKind::Test {
            for p in &prop.params {
                if !bound_here.contains(p.name.as_str()) {
                    diags.push(Diagnostic::error("T18",
                        format!("property \"{}\": test instance '{}' does not bind parameter '{}' (concrete tests must fix every parameter)",
                            prop.name, inst_label, p.name)));
                }
            }
        }

        // T4: per-instance init fields must be members.
        for (field, _) in &inst.init_state {
            if !member_names.contains(field.as_str()) {
                diags.push(Diagnostic::error(
                    "T4",
                    format!(
                        "property \"{}\": {} instance '{}': '{}' is not a member of entity '{}'",
                        prop.name, kind_label, inst_label, field, prop.entity_name
                    ),
                ));
            }
        }

        // T19/T21/T22: per-instance forall spec.
        validate_forall_spec(
            &format!(
                "property \"{}\": {} instance '{}'",
                prop.name, kind_label, inst_label
            ),
            &inst.forall_state,
            &inst.init_state,
            &member_names,
            diags,
        );
        // T20/T23: per-instance ctx block.
        validate_context_spec(
            &format!(
                "property \"{}\": {} instance '{}'",
                prop.name, kind_label, inst_label
            ),
            &inst.context,
            diags,
        );

        // W8: a concrete `test` instance cannot honour a property-level forall
        // field; it falls back to the field's default unless the instance pins
        // it explicitly. Warn so the loss of quantification is visible.
        if inst.kind == PropertyInstanceKind::Test {
            let inst_pinned: HashSet<&str> =
                inst.init_state.iter().map(|(n, _)| n.as_str()).collect();
            for t in &prop.forall_state.targets {
                if let ForallTarget::StateField(f) = t {
                    if !inst_pinned.contains(f.as_str()) {
                        diags.push(Diagnostic::warning("W8",
                            format!("property \"{}\": test instance '{}' leaves forall field '{}' unpinned; it falls back to its default (concrete tests cannot quantify)",
                                prop.name, inst_label, f)));
                    }
                }
            }
        }
    }
}

/// Validate a stateful `invariant` declaration. Rule codes:
/// - I1: action route must exist on the entity.
/// - I2: action parameter list must match the route signature.
/// - I3: each `check` expression must look like a boolean.
/// - I4: action body may only contain `bound`/`assume`.
/// - I5: invariant must declare at least one action and at least one check.
/// - I6: `senders { ... }` entries must be address/pubkey literal-ish.
/// Reuses T4 (init member existence), T13/W7 (param type support).
pub(super) fn validate_invariant(
    inv: &InvariantDecl,
    entities: &[Entity],
    diags: &mut Vec<Diagnostic>,
) {
    // I7 (multi-instance invariants unsupported on the TVM backend) lives in
    // rules/multi_entity_invariants.rs with a Domains([Tvm]) binding.

    // I8: instance names must be unique within an invariant.
    let mut seen_instances: HashSet<&str> = HashSet::new();
    for inst in &inv.instances {
        if !seen_instances.insert(inst.name.as_str()) {
            diags.push(Diagnostic::error(
                "I8",
                format!(
                    "invariant \"{}\": instance name '{}' is declared more than once",
                    inv.name, inst.name
                ),
            ));
        }
    }

    // I9: each instance's entity must exist in the program.
    let mut entity_by_inst: HashMap<&str, &Entity> = HashMap::new();
    let mut unbound_instances: HashSet<&str> = HashSet::new();
    for inst in &inv.instances {
        match entities.iter().find(|e| e.name == inst.entity) {
            Some(e) => {
                entity_by_inst.insert(inst.name.as_str(), e);
            }
            None => {
                unbound_instances.insert(inst.name.as_str());
                if inv.is_single_entity() {
                    diags.push(
                        Diagnostic::error(
                            "T1",
                            format!(
                                "invariant \"{}\": entity '{}' not found",
                                inv.name, inst.entity
                            ),
                        )
                        .with_span(inv.span),
                    );
                } else {
                    diags.push(
                        Diagnostic::error(
                            "I9",
                            format!(
                                "invariant \"{}\": instance '{}' references unknown entity '{}'",
                                inv.name, inst.name, inst.entity
                            ),
                        )
                        .with_span(inst.span),
                    );
                }
            }
        }
    }

    // I10: per-instance init keys must reference declared instance members
    // (only meaningful for the system form; for the single-entity form we keep
    // the legacy T4 code).
    for inst in &inv.instances {
        let entity = match entity_by_inst.get(inst.name.as_str()) {
            Some(e) => *e,
            None => continue,
        };
        let member_names: HashSet<&str> = entity.members.iter().map(|m| m.name.as_str()).collect();
        for (field, _) in &inst.init {
            if !member_names.contains(field.as_str()) {
                let code = if inv.is_single_entity() { "T4" } else { "I10" };
                diags.push(Diagnostic::error(
                    code,
                    format!(
                        "invariant \"{}\": '{}' is not a member of entity '{}' (instance '{}')",
                        inv.name, field, entity.name, inst.name
                    ),
                ));
            }
        }

        // T19/T20/T21: per-instance forall spec on the invariant's init/with.
        let inst_label = if inv.is_single_entity() {
            format!("invariant \"{}\"", inv.name)
        } else {
            format!("invariant \"{}\" (instance '{}')", inv.name, inst.name)
        };
        validate_forall_spec(
            &inst_label,
            &inst.forall_state,
            &inst.init,
            &member_names,
            diags,
        );
    }

    // T20/T23: invariant-level ctx block.
    validate_context_spec(&format!("invariant \"{}\"", inv.name), &inv.context, diags);

    // I5: at least one action.
    if inv.actions.is_empty() {
        diags.push(Diagnostic::error(
            "I5",
            format!(
                "invariant \"{}\": must declare at least one 'action' clause",
                inv.name
            ),
        ));
    }
    // I5: at least one check.
    if inv.checks.is_empty() {
        diags.push(Diagnostic::error(
            "I5",
            format!(
                "invariant \"{}\": must declare at least one 'check' clause",
                inv.name
            ),
        ));
    }

    // V58: at most one deploy address (V1).
    if inv.deploy.len() > 1 {
        diags.push(Diagnostic::error(
            "V58",
            format!(
                "invariant \"{}\": deploy {{ ... }} must contain exactly one address (found {})",
                inv.name,
                inv.deploy.len()
            ),
        ));
    }

    // V59: multi-entity invariants cannot use deploy { } until bootstrap exists.
    if !inv.deploy.is_empty() && !inv.is_single_entity() {
        diags.push(Diagnostic::error(
            "V59",
            format!(
                "invariant \"{}\": deploy {{ ... }} is not supported on multi-entity (system) invariants",
                inv.name
            ),
        ));
    }

    // I6: senders / deploy entries must be literal-ish (address/pubkey/hex/int) or bare identifier
    // referring to a constant.
    for (idx, s) in inv.deploy.iter().enumerate() {
        if !is_sender_literal(s) {
            diags.push(Diagnostic::error(
                "I6",
                format!(
                    "invariant \"{}\": deploy entry #{} must be an address/pubkey literal or a constant identifier",
                    inv.name,
                    idx + 1
                ),
            ));
        }
    }
    for (idx, s) in inv.senders.iter().enumerate() {
        if !is_sender_literal(s) {
            diags.push(Diagnostic::error("I6",
                format!("invariant \"{}\": senders entry #{} must be an address/pubkey literal or a constant identifier",
                    inv.name, idx + 1)));
        }
    }

    // W13: ctor-bootstrap invariants without explicit deploy use legacy harness rules.
    if inv.deploy.is_empty() && inv.is_single_entity() {
        if let Some(entity) = entity_by_inst.get("_self") {
            let init_pins: std::collections::HashSet<String> =
                inv.instances[0].init.iter().map(|(n, _)| n.clone()).collect();
            if invariant_needs_constructor_bootstrap(entity, &init_pins) {
                diags.push(Diagnostic::warning(
                    "W13",
                    format!(
                        "invariant \"{}\": constructor bootstrap without explicit deploy {{ ... }} uses legacy harness semantics (Lean: senders[0]; EVM: address(this))",
                        inv.name
                    ),
                ));
            }
        }
    }

    // W14: deploy address also listed under senders (informational).
    if inv.deploy.len() == 1 && !inv.senders.is_empty() {
        let deploy_expr = &inv.deploy[0];
        for (idx, s) in inv.senders.iter().enumerate() {
            if sender_exprs_equal(deploy_expr, s) {
                diags.push(Diagnostic::warning(
                    "W14",
                    format!(
                        "invariant \"{}\": deploy address is also senders entry #{} — deploy is setup-only; list it under senders only if it should appear in the trace pool",
                        inv.name,
                        idx + 1
                    ),
                ));
            }
        }
    }

    if inv.is_single_entity() {
        if let Some(entity) = entity_by_inst.get("_self") {
            check_invariant_illposed_eq_const(inv, entity, diags);
        }
    }

    // Declared action routes — used by I14 (exclude selectors) and I15
    // (trace::count/lastWas route references).
    let action_routes: HashSet<&str> = inv.actions.iter().map(|a| a.route.as_str()).collect();

    for action in &inv.actions {
        // I9 (action side): the instance referenced by the action must exist.
        let entity = match entity_by_inst.get(action.instance.as_str()) {
            Some(e) => *e,
            None => {
                if unbound_instances.contains(action.instance.as_str()) {
                    let primary = if inv.is_single_entity() { "T1" } else { "I9" };
                    let mut d = Diagnostic::error(
                        "I9",
                        format!(
                            "invariant \"{}\": action references unknown instance '{}'",
                            inv.name, action.instance
                        ),
                    )
                    .with_span(action.span);
                    d.suppressed_by = Some(primary.to_string());
                    diags.push(d);
                    continue;
                }
                diags.push(
                    Diagnostic::error(
                        "I9",
                        format!(
                            "invariant \"{}\": action references unknown instance '{}'",
                            inv.name, action.instance
                        ),
                    )
                    .with_span(action.span),
                );
                continue;
            }
        };
        let route_map: HashMap<&str, &Route> =
            entity.routes.iter().map(|r| (r.name.as_str(), r)).collect();

        // I1: route must exist on the entity bound to the instance.
        let route = match route_map.get(action.route.as_str()) {
            Some(r) => *r,
            None => {
                diags.push(Diagnostic::error(
                    "I1",
                    format!(
                        "invariant \"{}\": action '{}.{}' references unknown route on entity '{}'",
                        inv.name, action.instance, action.route, entity.name
                    ),
                ));
                continue;
            }
        };

        // I2: parameter list must match route signature in arity, and best-effort match by type.
        if action.params.len() != route.params.len() {
            diags.push(Diagnostic::error("I2",
                format!("invariant \"{}\": action '{}.{}' has {} parameter(s) but route signature expects {}",
                    inv.name, action.instance, action.route, action.params.len(), route.params.len())));
        } else {
            for (ap, rp) in action.params.iter().zip(route.params.iter()) {
                if ap.ty != rp.ty {
                    diags.push(Diagnostic::warning("I2",
                        format!("invariant \"{}\": action '{}.{}' parameter '{}' has type {:?}, route expects {:?}",
                            inv.name, action.instance, action.route, ap.name, ap.ty, rp.ty)));
                }
            }
        }

        // T13/W7: deferred to target-aware `rules/w7.rs`.

        // I4: action body may only contain `bound` / `assume` /
        // `skip if` / `advanceTime` steps. (Plain `let` was historically
        // rejected; we keep that behaviour for now -- bindings can move
        // into a `track { let ... }` snapshot block instead.)
        let action_param_names: HashSet<&str> =
            action.params.iter().map(|p| p.name.as_str()).collect();
        for step in &action.body {
            match step {
                TestStep::Assume { cond } => {
                    match cond {
                        Expr::Tuple(_)
                        | Expr::RecordConstruct(..)
                        | Expr::RecordUpdate(..)
                        | Expr::EmptyCollection
                        | Expr::ArrayLit(_) => {
                            diags.push(Diagnostic::error("T12",
                                format!("invariant \"{}\": action '{}.{}': 'assume' expression must be a boolean",
                                    inv.name, action.instance, action.route)));
                        }
                        _ => {}
                    }
                    // I15: `trace::` accessors are allowed in `assume`.
                    check_trace_accessor_shapes(&inv.name, cond, &action_routes, diags);
                }
                TestStep::Bound {
                    var,
                    lo,
                    hi,
                    inclusive,
                    ..
                } => {
                    // I16: `trace::` accessors are not allowed in bound endpoints.
                    forbid_trace_refs(&inv.name, lo, "'bound' endpoints", diags);
                    forbid_trace_refs(&inv.name, hi, "'bound' endpoints", diags);
                    check_literal_range_endpoints(
                        &format!(
                            "invariant \"{}\": action '{}.{}'",
                            inv.name, action.instance, action.route
                        ),
                        var,
                        lo,
                        hi,
                        *inclusive,
                        diags,
                    );
                    if !action_param_names.contains(var.as_str()) {
                        // Computed-bounds relaxation: T11 is now scoped to
                        // "the bound variable must be a parameter of this
                        // action". The lo/hi expressions are free to reference
                        // entity members, snapshot bindings, and earlier
                        // `let`s; the codegen threads them through
                        // `substitute_member_accessors`.
                        diags.push(Diagnostic::error("T11",
                            format!("invariant \"{}\": action '{}.{}': 'bound {}' does not refer to an action parameter (declared: {})",
                                inv.name, action.instance, action.route, var,
                                action.params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", "))));
                    }
                }
                TestStep::SkipIf { cond } => {
                    // Allowed; codegen lowers to `if (cond) return;`.
                    // I16: `trace::` accessors are not allowed in `skip if`.
                    forbid_trace_refs(&inv.name, cond, "'skip if' conditions", diags);
                }
                TestStep::AdvanceTime { secs } => {
                    // I16: `trace::` accessors are not allowed in `advanceTime`.
                    forbid_trace_refs(&inv.name, secs, "'advanceTime' arguments", diags);
                    // Allowed; codegen emits `vm.warp + vm.roll`. The
                    // builtin is callable from any action body but only
                    // does anything useful when the invariant is
                    // declared `#[with_time]` (so the runner's
                    // targetSelector includes the synthetic
                    // `advanceTime(uint256)` action).
                }
                _ => {
                    diags.push(Diagnostic::error("I4",
                        format!("invariant \"{}\": action '{}.{}' body may only contain 'bound', 'assume', 'skip if', or 'advanceTime(...)' steps",
                            inv.name, action.instance, action.route)));
                }
            }
        }
    }

    // I3: each check expression must look like a boolean.
    for c in &inv.checks {
        match c {
            Expr::Tuple(_)
            | Expr::RecordConstruct(..)
            | Expr::RecordUpdate(..)
            | Expr::EmptyCollection
            | Expr::ArrayLit(_) => {
                diags.push(Diagnostic::error("I3",
                    format!("invariant \"{}\": 'check' expression must be a boolean, got non-scalar literal",
                        inv.name)));
            }
            _ => {}
        }
        // I15: `trace::` accessors are allowed in `check`.
        check_trace_accessor_shapes(&inv.name, c, &action_routes, diags);
    }

    // I16: `trace::` accessors are forbidden in every other invariant
    // position (only `assume` / `check` may reference them).
    for binding in &inv.track {
        forbid_trace_refs(&inv.name, &binding.value, "'track' bindings", diags);
    }
    for q in &inv.derived {
        for step in &q.body {
            if let TestStep::Let { value, .. } = step {
                forbid_trace_refs(&inv.name, value, "'derived' bodies", diags);
            }
        }
        forbid_trace_refs(&inv.name, &q.return_value, "'derived' bodies", diags);
    }
    for s in &inv.deploy {
        forbid_trace_refs(&inv.name, s, "'deploy' lists", diags);
    }
    for s in &inv.senders {
        forbid_trace_refs(&inv.name, s, "'senders' lists", diags);
    }
    for s in &inv.exclude_senders {
        forbid_trace_refs(&inv.name, s, "'exclude senders' lists", diags);
    }
    for inst in &inv.instances {
        for (_, v) in &inst.init {
            forbid_trace_refs(&inv.name, v, "'init' values", diags);
        }
    }
    for entry in &inv.context.entries {
        if let Some(v) = &entry.value {
            forbid_trace_refs(&inv.name, v, "'ctx' values", diags);
        }
    }

    // I12: `derived` queries are pure view helpers; their bodies may
    // only contain `let` steps (no `call`/`assume`/`expect_*` etc.).
    for q in &inv.derived {
        for step in &q.body {
            if !matches!(step, TestStep::Let { .. }) {
                diags.push(Diagnostic::error("I12",
                    format!("invariant \"{}\": derived '{}' body may only contain `let` bindings; queries are pure",
                        inv.name, q.name)));
                break;
            }
        }
        validate_typed_spec_lets(
            &format!("invariant \"{}\": derived '{}'", inv.name, q.name),
            &q.body,
            &HashSet::new(),
            diags,
        );
    }

    // I13: `track { let name = expr; ... }` may only contain `let`
    // bindings. (At parse time the grammar enforces this; the
    // validator double-checks for forward-compatibility with grammar
    // additions.) Names must be unique within the block.
    let mut seen_track_names: HashSet<&str> = HashSet::new();
    for binding in &inv.track {
        if !seen_track_names.insert(binding.name.as_str()) {
            diags.push(Diagnostic::error(
                "I13",
                format!(
                    "invariant \"{}\": duplicate track binding '{}'",
                    inv.name, binding.name
                ),
            ));
        }
    }

    // I14: every name listed in `exclude selectors { ... }` must match
    // a declared action route on the handler.
    for sel in &inv.exclude_selectors {
        if !action_routes.contains(sel.as_str()) && sel != "advanceTime" {
            diags.push(Diagnostic::error("I14",
                format!("invariant \"{}\": 'exclude selectors' references '{}', which is not a declared action",
                    inv.name, sel)));
        }
    }

    // I11: in a multi-instance invariant, member references in checks and per-action
    // bound/assume must be qualified with `<inst>.<member>`. Bare `Ident(name)` that
    // matches any member of any listed entity is ambiguous.
    if !inv.is_single_entity() {
        let mut entity_member_names: HashSet<&str> = HashSet::new();
        for inst in &inv.instances {
            if let Some(entity) = entity_by_inst.get(inst.name.as_str()) {
                for m in &entity.members {
                    entity_member_names.insert(m.name.as_str());
                }
            }
        }

        for c in &inv.checks {
            check_qualified_members(inv, c, &entity_member_names, "check", "", "", diags);
        }
        for action in &inv.actions {
            let action_param_names: HashSet<&str> =
                action.params.iter().map(|p| p.name.as_str()).collect();
            for step in &action.body {
                match step {
                    TestStep::Assume { cond } => {
                        check_qualified_members_with_locals(
                            inv,
                            cond,
                            &entity_member_names,
                            &action_param_names,
                            "assume",
                            &action.instance,
                            &action.route,
                            diags,
                        );
                    }
                    TestStep::Bound { lo, hi, .. } => {
                        check_qualified_members_with_locals(
                            inv,
                            lo,
                            &entity_member_names,
                            &action_param_names,
                            "bound",
                            &action.instance,
                            &action.route,
                            diags,
                        );
                        check_qualified_members_with_locals(
                            inv,
                            hi,
                            &entity_member_names,
                            &action_param_names,
                            "bound",
                            &action.instance,
                            &action.route,
                            diags,
                        );
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Walk an expression looking for bare `Expr::Ident(name)` references whose
/// `name` matches any entity member -- those must be qualified with
/// `<inst>.<member>` in the multi-instance form (rule I11).
fn check_qualified_members(
    inv: &InvariantDecl,
    expr: &Expr,
    entity_member_names: &HashSet<&str>,
    ctx: &str,
    inst: &str,
    route: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let empty: HashSet<&str> = HashSet::new();
    check_qualified_members_with_locals(
        inv,
        expr,
        entity_member_names,
        &empty,
        ctx,
        inst,
        route,
        diags,
    );
}

fn check_qualified_members_with_locals(
    inv: &InvariantDecl,
    expr: &Expr,
    entity_member_names: &HashSet<&str>,
    locals: &HashSet<&str>,
    ctx: &str,
    inst: &str,
    route: &str,
    diags: &mut Vec<Diagnostic>,
) {
    walk_expr_for_qualified(expr, &mut |e| {
        if let Expr::Ident(name) = e {
            if entity_member_names.contains(name.as_str()) && !locals.contains(name.as_str()) {
                let where_ = if inst.is_empty() {
                    format!("{}", ctx)
                } else {
                    format!("action '{}.{}' {}", inst, route, ctx)
                };
                diags.push(Diagnostic::error("I11",
                    format!("invariant \"{}\": {}: bare member reference '{}' must be qualified with '<instance>.{}'",
                        inv.name, where_, name, name)));
            }
        }
    });
}

/// Visit every sub-expression once. Skips the right-hand side of a
/// `FieldAccess` so that `v.m_balance` does not trip the I11 check on
/// `m_balance` itself.
fn walk_expr_for_qualified(expr: &Expr, f: &mut impl FnMut(&Expr)) {
    f(expr);
    match expr {
        Expr::BinOp(l, _, r) => {
            walk_expr_for_qualified(l, f);
            walk_expr_for_qualified(r, f);
        }
        Expr::UnaryOp(_, inner) => walk_expr_for_qualified(inner, f),
        Expr::FieldAccess(inner, _) => {
            // `<inner>.<field>` -- only the LHS is a normal expression. The
            // field name is part of the access path and must not be analysed
            // as a bare identifier.
            walk_expr_for_qualified(inner, f);
        }
        Expr::Index(arr, idx) => {
            walk_expr_for_qualified(arr, f);
            walk_expr_for_qualified(idx, f);
        }
        Expr::FnCall(_, args) => {
            for a in args {
                walk_expr_for_qualified(a, f);
            }
        }
        Expr::MethodCall(recv, _, args) => {
            walk_expr_for_qualified(recv, f);
            for a in args {
                walk_expr_for_qualified(a, f);
            }
        }
        Expr::If(c, t, e) => {
            walk_expr_for_qualified(c, f);
            walk_expr_for_qualified(t, f);
            if let Some(e) = e {
                walk_expr_for_qualified(e, f);
            }
        }
        Expr::Let(_, value, body) => {
            walk_expr_for_qualified(value, f);
            walk_expr_for_qualified(body, f);
        }
        Expr::Range(a, b) => {
            walk_expr_for_qualified(a, f);
            walk_expr_for_qualified(b, f);
        }
        Expr::Cast(inner, _) => walk_expr_for_qualified(inner, f),
        Expr::Tuple(items) | Expr::ArrayLit(items) => {
            for it in items {
                walk_expr_for_qualified(it, f);
            }
        }
        Expr::RecordConstruct(_, fields) => {
            for (_, v) in fields {
                walk_expr_for_qualified(v, f);
            }
        }
        Expr::RecordUpdate(base, fields) => {
            walk_expr_for_qualified(base, f);
            for (_, v) in fields {
                walk_expr_for_qualified(v, f);
            }
        }
        Expr::Block(stmts) => {
            for s in stmts {
                walk_expr_for_qualified(s, f);
            }
        }
        Expr::Match(subject, arms) => {
            walk_expr_for_qualified(subject, f);
            for arm in arms {
                walk_expr_for_qualified(&arm.body, f);
            }
        }
        Expr::Closure(_, body) => walk_expr_for_qualified(body, f),
        Expr::For(_, iter, body) => {
            walk_expr_for_qualified(iter, f);
            walk_expr_for_qualified(body, f);
        }
        Expr::Some(inner) => walk_expr_for_qualified(inner, f),
        Expr::EnumVariantWithData(_, _, args) | Expr::MacroRef(_, args) => {
            for a in args {
                walk_expr_for_qualified(a, f);
            }
        }
        Expr::NamespacedCall { args, .. } => {
            for a in args {
                walk_expr_for_qualified(a, f);
            }
        }
        Expr::AddressOf {
            args, with_params, ..
        } => {
            for a in args {
                walk_expr_for_qualified(a, f);
            }
            for (_, v) in with_params {
                walk_expr_for_qualified(v, f);
            }
        }
        Expr::Encode { value, .. } => walk_expr_for_qualified(value, f),
        _ => {}
    }
}

/// True when `expr` contains any `trace::` accessor.
pub(super) fn expr_has_trace_ref(expr: &Expr) -> bool {
    let mut found = false;
    walk_expr_for_qualified(expr, &mut |e| {
        if matches!(e, Expr::TraceField(_) | Expr::TraceCall { .. }) {
            found = true;
        }
    });
    found
}

/// I15: validate the shape of each `trace::` accessor reachable from
/// `expr` — the accessor name must be recognised and any referenced
/// route must be a declared action of the invariant.
fn check_trace_accessor_shapes(
    inv_name: &str,
    expr: &Expr,
    action_routes: &HashSet<&str>,
    diags: &mut Vec<Diagnostic>,
) {
    walk_expr_for_qualified(expr, &mut |e| match e {
        Expr::TraceField(field) => {
            if field != "length" {
                diags.push(Diagnostic::error("I15",
                    format!("invariant \"{}\": unknown trace accessor 'trace::{}' (expected 'trace::length')",
                        inv_name, field)));
            }
        }
        Expr::TraceCall { name, route } => {
            if name != "count" && name != "lastWas" {
                diags.push(Diagnostic::error("I15",
                    format!("invariant \"{}\": unknown trace accessor 'trace::{}(...)' (expected 'count' or 'lastWas')",
                        inv_name, name)));
            } else if !action_routes.contains(route.as_str()) {
                diags.push(Diagnostic::error("I15",
                    format!("invariant \"{}\": 'trace::{}({})' references '{}', which is not a declared action",
                        inv_name, name, route, route)));
            }
        }
        _ => {}
    });
}

/// I16: `trace::` accessors are only legal inside an invariant's
/// `assume` and `check` expressions. Emit an error if any appear in
/// `expr` (used for every other position).
fn forbid_trace_refs(inv_name: &str, expr: &Expr, position: &str, diags: &mut Vec<Diagnostic>) {
    if expr_has_trace_ref(expr) {
        diags.push(Diagnostic::error("I16",
            format!("invariant \"{}\": trace:: accessors may only appear in 'assume' and 'check' expressions, not in {}",
                inv_name, position)));
    }
}

fn is_sender_literal(e: &Expr) -> bool {
    matches!(
        e,
        Expr::IntLiteral(_)
            | Expr::StringLiteral(_)
            | Expr::BytesLiteral(_)
            | Expr::Ident(_)
    )
}

fn init_route(entity: &Entity) -> Option<&Route> {
    entity
        .routes
        .iter()
        .find(|r| r.is_init)
        .or_else(|| entity.routes.iter().find(|r| r.name == "constructor"))
}

/// True when `init { … }` leaves members that the constructor route still initializes.
fn invariant_needs_constructor_bootstrap(
    entity: &Entity,
    init_pins: &HashSet<String>,
) -> bool {
    let route = match init_route(entity) {
        Some(r) => r,
        None => return false,
    };
    entity.members.iter().any(|m| {
        !m.is_identity
            && !init_pins.contains(&m.name)
            && m.transforms
                .iter()
                .any(|t| t.route_name == route.name)
    })
}

fn sender_exprs_equal(a: &Expr, b: &Expr) -> bool {
    a == b
}

/// W15: `check <scalar> == <const>` when a non-excluded action can decrease `<scalar>`.
fn check_invariant_illposed_eq_const(
    inv: &InvariantDecl,
    entity: &Entity,
    diags: &mut Vec<Diagnostic>,
) {
    let excluded: HashSet<&str> = inv
        .exclude_selectors
        .iter()
        .map(|s| s.as_str())
        .collect();
    for check in &inv.checks {
        let (lhs, rhs) = match check {
            Expr::BinOp(lhs, BinOp::Eq, rhs) => (lhs.as_ref(), rhs.as_ref()),
            _ => continue,
        };
        if !is_invariant_const_operand(rhs, entity) {
            continue;
        }
        let member = match invariant_check_scalar_member(lhs, entity) {
            Some(m) => m,
            None => continue,
        };
        let decreasing = inv
            .actions
            .iter()
            .filter(|a| a.instance == "_self" && !excluded.contains(a.route.as_str()))
            .any(|a| route_decreases_member(entity, &a.route, &member));
        if !decreasing {
            continue;
        }
        diags.push(Diagnostic::warning(
            "W15",
            format!(
                "invariant \"{}\": check `{member} == …` is ill-posed — action(s) can decrease `{member}` while the check pins an exact constant (invariant may be vacuous or tautological under fuzz)",
                inv.name,
                member = member
            ),
        ));
    }
}

fn is_invariant_const_operand(expr: &Expr, entity: &Entity) -> bool {
    match expr {
        Expr::IntLiteral(_)
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::BoolLiteral(_) => true,
        Expr::Ident(name) => !entity.members.iter().any(|m| m.name == *name),
        _ => false,
    }
}

fn invariant_check_scalar_member(lhs: &Expr, entity: &Entity) -> Option<String> {
    if let Expr::Ident(name) = lhs {
        if entity.members.iter().any(|m| m.name == *name) {
            return Some(name.clone());
        }
        return None;
    }
    if let Expr::FieldAccess(base, field) = lhs {
        if matches!(base.as_ref(), Expr::Ident(_))
            && entity.members.iter().any(|m| m.name == *field)
        {
            return Some(field.clone());
        }
        return None;
    }
    let (route_name, args) = match lhs {
        Expr::FnCall(name, args) => (name.as_str(), args.as_slice()),
        _ => return None,
    };
    if !args.is_empty() {
        return None;
    }
    let route = entity
        .routes
        .iter()
        .find(|r| r.name == route_name && r.is_view)?;
    route_returns_single_member(route)
}

fn route_returns_single_member(route: &Route) -> Option<String> {
    for action in route.body.actions() {
        if let RouteAction::Return { values } = action {
            if values.len() == 1 {
                if let Expr::Ident(name) = &values[0] {
                    return Some(name.clone());
                }
            }
        }
    }
    None
}

fn route_decreases_member(entity: &Entity, route: &str, member: &str) -> bool {
    entity.members.iter().any(|m| {
        m.name == member
            && m.transforms.iter().any(|t| {
                t.route_name == route && transform_expr_decreases_member(&t.body, member)
            })
    })
}

fn transform_expr_decreases_member(expr: &Expr, member: &str) -> bool {
    match expr {
        Expr::BinOp(lhs, BinOp::Sub, _) if member_ref(lhs, member) => true,
        Expr::If(_, then_e, else_e) => {
            transform_expr_decreases_member(then_e, member)
                || else_e
                    .as_ref()
                    .map_or(false, |e| transform_expr_decreases_member(e, member))
        }
        Expr::Block(items) => items
            .iter()
            .any(|e| transform_expr_decreases_member(e, member)),
        Expr::Let(_, value, body) => {
            transform_expr_decreases_member(value, member)
                || transform_expr_decreases_member(body, member)
        }
        Expr::Cast(inner, _) | Expr::UnaryOp(_, inner) => {
            transform_expr_decreases_member(inner, member)
        }
        Expr::BinOp(lhs, _, rhs) => {
            transform_expr_decreases_member(lhs, member)
                || transform_expr_decreases_member(rhs, member)
        }
        Expr::Tuple(items) | Expr::ArrayLit(items) => items
            .iter()
            .any(|e| transform_expr_decreases_member(e, member)),
        Expr::MethodCall(base, _, args) => {
            transform_expr_decreases_member(base, member)
                || args.iter().any(|a| transform_expr_decreases_member(a, member))
        }
        Expr::FieldAccess(base, _) | Expr::Index(base, _) => {
            transform_expr_decreases_member(base, member)
        }
        Expr::Match(subject, arms) => {
            transform_expr_decreases_member(subject, member)
                || arms
                    .iter()
                    .any(|a| transform_expr_decreases_member(&a.body, member))
        }
        _ => false,
    }
}

fn member_ref(expr: &Expr, member: &str) -> bool {
    matches!(expr, Expr::Ident(n) if n == member)
        || matches!(expr, Expr::TemporalRef(n) if n == member)
}

/// T31: reject empty literal fuzz / invariant ranges when both endpoints fold.
fn check_literal_range_endpoints(
    context: &str,
    param: &str,
    lo: &Expr,
    hi: &Expr,
    inclusive: bool,
    diags: &mut Vec<Diagnostic>,
) {
    let (Some(lo_v), Some(hi_v)) = (
        entity::const_eval_u256(lo),
        entity::const_eval_u256(hi),
    ) else {
        return;
    };
    let empty = if inclusive { lo_v > hi_v } else { lo_v >= hi_v };
    if !empty {
        return;
    }
    let hint = if inclusive {
        "check that lo <= hi"
    } else {
        "exclusive '..' ranges require lo < hi; use '..=' for an inclusive top"
    };
    diags.push(Diagnostic::error(
        "T31",
        format!(
            "{}: range for parameter '{}' is empty ({})",
            context, param, hint
        ),
    ));
}

/// T30: typed `let name: Type = value` in spec bodies — duplicate names,
/// parameter shadowing, and assignability of the initializer to the annotation.
pub(super) fn validate_typed_spec_lets(
    owner: &str,
    body: &[TestStep],
    param_names: &HashSet<&str>,
    diags: &mut Vec<Diagnostic>,
) {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut env: HashMap<String, Type> = HashMap::new();
    for step in body {
        if let TestStep::Let {
            name,
            ty: decl_ty,
            value,
        } = step
        {
            if !seen.insert(name.as_str()) {
                diags.push(Diagnostic::error(
                    "T30",
                    format!("{owner}: duplicate `let` binding '{name}'"),
                ));
            }
            if param_names.contains(name.as_str()) {
                diags.push(Diagnostic::error(
                    "T30",
                    format!("{owner}: `let {name}` shadows a parameter"),
                ));
            }
            if let Some(decl) = decl_ty {
                if let Some(rhs_ty) = infer_spec_let_rhs_type(value, &env) {
                    if !spec_let_types_compatible(decl, &rhs_ty) {
                        diags.push(Diagnostic::error(
                            "T30",
                            format!(
                                "{owner}: `let {name}: {decl}`: initializer is not assignable (got {rhs_ty})",
                                decl = fmt_type(decl),
                                rhs_ty = fmt_type(&rhs_ty),
                            ),
                        ));
                    }
                }
                env.insert(name.clone(), decl.clone());
            } else if let Some(inf) = infer_spec_let_rhs_type(value, &env) {
                env.insert(name.clone(), inf);
            }
        }
    }
}

fn fmt_type(ty: &Type) -> String {
    match ty {
        Type::Simple(s) => s.clone(),
        Type::TypedAddress(e) => format!("Address<{}>", e),
        Type::Generic(g, ps) => {
            let args = ps.iter().map(fmt_type).collect::<Vec<_>>().join(", ");
            format!("{}<{}>", g, args)
        }
        Type::Tuple(items) => {
            let inner = items.iter().map(fmt_type).collect::<Vec<_>>().join(", ");
            format!("({})", inner)
        }
    }
}

fn infer_spec_let_rhs_type(expr: &Expr, prior: &HashMap<String, Type>) -> Option<Type> {
    match expr {
        Expr::IntLiteral(_) => Some(Type::Simple("u64".to_string())),
        Expr::BoolLiteral(_) => Some(Type::Simple("bool".to_string())),
        Expr::StringLiteral(_) => Some(Type::Simple("string".to_string())),
        Expr::BytesLiteral(_) => Some(Type::Simple("bytes".to_string())),
        Expr::Cast(_, ty) => Some(ty.clone()),
        Expr::Ident(name) => prior.get(name).cloned(),
        _ => None,
    }
}

fn spec_let_types_compatible(decl: &Type, rhs: &Type) -> bool {
    if decl == rhs {
        return true;
    }
    use crate::codegen::stdlib::{is_numeric_type, is_string_type};
    if matches!(decl, Type::Simple(s) if s == "address") && is_numeric_type(rhs) {
        return true;
    }
    if is_numeric_type(decl) && is_numeric_type(rhs) {
        return true;
    }
    if is_string_type(decl) && is_string_type(rhs) {
        return true;
    }
    false
}

/// T39 (Solidity): `expect state` on a member position that has no public
/// getter. The EVM backend stores `Vec` members (and records holding a
/// `Vec` / `HashMap`) without `public`, and a mapping-to-array getter
/// takes an index the harness cannot supply.
pub fn check_expect_state_getters_solidity(program: &Program, diags: &mut Vec<Diagnostic>) {
    fn dynamic(entity: &Entity, program: &Program, ty: &Type, depth: usize) -> bool {
        if depth > 32 {
            return false;
        }
        let ty = entity::unfold_alias(ty, &entity.type_aliases, &program.type_aliases, 0);
        match &ty {
            Type::Simple(n) => entity
                .records
                .iter()
                .chain(program.records.iter())
                .find(|r| &r.name == n)
                .is_some_and(|r| r.fields.iter().any(|f| dynamic(entity, program, &f.ty, depth + 1))),
            Type::Generic(n, ps) => {
                n == "Vec" || n == "HashMap" || ps.iter().any(|p| dynamic(entity, program, p, depth + 1))
            }
            Type::Tuple(items) => items.iter().any(|p| dynamic(entity, program, p, depth + 1)),
            _ => false,
        }
    }
    let bodies = program
        .tests
        .iter()
        .map(|t| (&t.name, &t.entity_name, &t.body))
        .chain(program.fuzz_tests.iter().map(|f| (&f.name, &f.entity_name, &f.body)));
    for (name, entity_name, body) in bodies {
        let Some(entity) = program.entities.iter().find(|e| &e.name == entity_name) else {
            continue;
        };
        for step in body {
            let TestStep::ExpectState { fields } = step else { continue };
            for (path, _) in fields {
                let Some(PathSegment::Field(m)) = path.first() else { continue };
                let Some(member) = entity.members.iter().find(|x| &x.name == m) else {
                    continue;
                };
                let ty = entity::unfold_alias(&member.ty, &entity.type_aliases, &program.type_aliases, 0);
                let unreadable = match &ty {
                    Type::Generic(n, ps) if n == "HashMap" && ps.len() == 2 => {
                        let mut v = ps[1].clone();
                        loop {
                            let u = entity::unfold_alias(&v, &entity.type_aliases, &program.type_aliases, 0);
                            match u {
                                Type::Generic(n, ps) if n == "HashMap" && ps.len() == 2 => v = ps[1].clone(),
                                Type::Generic(n, _) if n == "Vec" => break true,
                                _ => break false,
                            }
                        }
                    }
                    other => dynamic(entity, program, other, 0),
                };
                if unreadable {
                    diags.push(Diagnostic::error(
                        "T39",
                        format!(
                            "test \"{}\": 'expect state' reads member '{}' of type `{}`, which has no public Solidity getter on the EVM target; expose it through a `view` route and assert with `expect return`",
                            name,
                            m,
                            crate::pretty::fmt_type(&member.ty)
                        ),
                    ));
                }
            }
        }
    }
}
