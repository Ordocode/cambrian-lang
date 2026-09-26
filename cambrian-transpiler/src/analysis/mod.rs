// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Kernel analyses: graphs and shared AST facts computed once per program.
//!
//! See [`docs/plans/p2-kernel-analyses.md`]. Language cores and adapters
//! consume these; they do not re-derive declaration / call / send order.

pub mod call_graph;
pub mod graphs;
pub mod pure_order;
pub mod route_facts;
pub mod send_target;
pub mod temporal;
pub mod types;
pub mod walk;

pub use call_graph::{cross_entity_route_dependencies, same_entity_callees};
pub use graphs::ProgramGraphs;
pub use route_facts::{
    compute_route_facts, entity_evm_lean_can_fail, evm_lean_can_fail, fail_closure,
    route_can_fail_evm_lean, route_has_unphased_sends, route_infer_evm_view, route_is_view,
    route_phased_needs_world_thread, route_uses_msg_value, sys_field_needs_world, FailPropagation,
    RouteFacts,
};
pub use send_target::{
    classify_dest, classify_message_dest, route_has_dynamic_dispatch,
    route_has_unrescued_extern_send, route_has_unrescued_failing_cross_send,
    route_has_value_bearing_transfer, SendTarget,
};
pub use temporal::{build_temporal_orders, TemporalError, TemporalOrder};
pub use types::{order_type_items, TypeItem};
pub use walk::{action_walks_any, for_each_action, for_each_route_action};
