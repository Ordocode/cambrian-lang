# Governor — Cambrian translation plan

## Overview

This example re-expresses the OpenZeppelin governance suite — `Governor`,
`TimelockController`, `ERC20Votes` — in idiomatic Cambrian targeting EVM.
The reference Solidity (pinned to OZ `v5.0.2`) lives under `ref/` as the
behavioural spec; the Cambrian side is **not** a line-for-line port. It
re-uses these Cambrian features:

- **Deterministic CREATE2 addresses** via `Entity.address(args)` (or the
  equivalent `addressOf(Entity.state(args))`), with the auto-generated
  `CambrianFactory` handling deploy + factory-guarded `initialize()`.
- **Member-centric state** — transfer/delegate/vote/queue/execute logic
  lives in member transforms (`m_balances: HashMap<...> { in transfer(...) => ... }`),
  not in imperative function bodies.
- **`from <Entity>(...)` sender chains** for role-based authorisation.
- **`enum` + `match`** for proposal/operation state machines.
- **`hashOf(...)`** instead of inline `keccak256(abi.encode(...))`.
- **EVM `var` capture across phases** for synchronous return values
  (e.g. reading `getVotes(msg::sender)` from the token before tallying
  in `castVote`), with **per-phase `where` clauses** gating later phases
  on captured values.

## Entities (3 + 1 sink)

- **`ERC20Votes`** — singleton (no `identity`). Member-centric state:
  `m_balances`, `m_votes`, `m_delegates`, `m_total_supply`. Voting power
  is recomputed in `m_votes`'s `in transfer / in delegate / in mint / in burn`
  clauses — no `_beforeTokenTransfer` hook indirection.
- **`TimelockController`** — singleton. v1 simplifies OZ's role mapping
  to single role-holders: `m_proposer: address`, `m_executor: address`,
  `m_canceller: address`, gated via `from` clauses. Operation id via
  `hashOf(target, value, data, predecessor, salt)`. Operation lifecycle
  as `enum OpState { Unset, Pending, Ready, Done, Cancelled }` with
  `match` over `m_op_state[id]`. Multi-holder roles are **out of scope**
  for v1 (would require `from`-chain over `Vec<address>`).
- **`Governor`** — singleton. `m_token: Address<ERC20Votes>` and
  `m_timelock: Address<TimelockController>` initialised in member
  transforms via `Entity.address()` (both targets are singletons).
  `enum ProposalState { Pending, Active, Defeated, Succeeded, Queued, Executed, Cancelled }`
  driven by `match` in a `view state(id)` route. `proposalId` via
  `hashOf(target, value, data, descriptionHash)`.
  - **`castVote` is a 2-phase route**: phase `read:` captures
    `var weight = getVotes(msg::sender) ~> m_token`; phase
    `tally where weight > 0 : throw 100 : [...]` consumes `weight` in
    the `m_for`/`m_against` transforms tagged `tally:`.
  - All other routes (`queue`, `execute`, `state`) stay unphased; the
    EVM SSTORE-before-CALL guarantee preserves the
    checks-effects-interactions pattern.
- **`CounterTarget`** *(plain Solidity, in `ref/target/`)* — trivial sink
  contract that the executed proposal calls into. Mirrors the role of
  `UpdateZeroContract.sol` in the accumulator example. Copied into
  `build/src/` by `setup.sh` (TBD).

## Auto-generated factory

The EVM backend auto-emits `CambrianFactory.sol`, providing
`predictERC20Votes()/deployERC20Votes(...)`,
`predictTimelockController()/deployTimelockController(...)`,
`predictGovernor()/deployGovernor(...)`. There is no hand-written
`GovernorFactory.cam`.

## OZ ↔ Cambrian recipe summary

| OZ Solidity | Cambrian | Feature |
|---|---|---|
| `bytes32 PROPOSER_ROLE; _grantRole(...)` | `m_proposer: address` + `from m_proposer` | `from` clause |
| `constructor(IVotes _token, TimelockController _timelock)` | `m_token: Address<ERC20Votes> { in constructor() => ERC20Votes.address() }` | CREATE2 + member-init transform |
| Forge `script/` wires three contracts | Auto `CambrianFactory.deployX()` | Auto-factory |
| `keccak256(abi.encode(...))` for `proposalId` | `hashOf(target, value, data, descriptionHash)` | `hashOf` builtin |
| `function state(id)` with `if/else if` | `enum ProposalState` + `match` | EVM enum+match |
| `_beforeTokenTransfer` hook + checkpoint update | `m_votes: HashMap<...> { in transfer(...) => ...; in delegate(...) => ...}` | Member-centric state |
| `try targetContract.call{value:v}(data)` | `~> m_timelock with { value: v, data: ... }` | EVM named send |
| `emit ProposalCreated(...)` | If unsupported, scope-cut | Out-of-scope candidate |

## Out of scope (v1)

- ERC20Permit / EIP-2612 (separate gap, may be picked up if `ecrecover`
  lands).
- Multi-target proposals with mixed value+calldata arrays — restricted
  to single-target until the dynamic-array gap is resolved.
- Quorum fraction in basis points dynamic over time — fixed quorum for
  v1.
- On-chain Governor parameter updates via self-call — kept static for
  v1.
- Cross-chain or upgradeable variants.
- Multi-holder roles in TimelockController.
- `castVoteBySig` (EIP-712).
- `block.number` access — checkpoints use `sys::timestamp` (seconds)
  instead of `block.number`.

Each scope cut is recorded here as it is taken; the goal of v1 is a
clean idiomatic re-implementation of the **core** governance flow
(propose → vote → queue → execute), not feature-parity.

## Gaps

Phase 0 closed the known gap set (Gaps 1, 5, 6, 7, 8 hardening; phased-
route Q1 and Q2A). New gaps surfacing during the per-entity translation
phases (2-4) are recorded below.

### Status snapshot

| Gap | Status | Regression test |
|---|---|---|
| G-T1 (`from <member>` + `: throw N`) | RESOLVED | `cambrian-transpiler/tests/test_codegen_evm.rs::evm_gt1_*` |
| G-T2 (`sys::timestamp + N` in transforms) | RESOLVED (subsumed by G-T7) | `evm_gt2_sys_timestamp_in_transform_lowers_to_block_timestamp` |
| G-T3 (`data:` field of named send) | RESOLVED | `evm_gt3_named_send_data_field_threaded_into_call` |
| G-T4 (duplicate `function constructor`) | RESOLVED | `evm_gt4_no_duplicate_function_constructor_emitted` |
| G-T5 (`HashMap` as `pure fn` param) | RESOLVED (lowered to `storage`, demoted to `view`) | `evm_gt5_pure_fn_hashmap_param_uses_storage_view_no_visibility` |
| G-T6 (`if/else` in transform body) | RESOLVED | `evm_gt6_if_else_in_transform_emits_conditional_writes` |
| G-T7 (`sys::timestamp` in transforms / `where`) | RESOLVED | `evm_gt7_sys_timestamp_in_where_clause_compares_block_timestamp` + Governor.test.cam suite |
| G-T8 (single-file foreign-entity stubs) | KNOWN — project mode is the supported workflow | `evm_gt8_cross_entity_interface_emission_pins_both_modes` |

### G-T1 — Address-member `from` clauses *(RESOLVED)*

**Status.** Grammar accepts a bare `from <address-member>` (no parens),
optionally suffixed by `: throw N`; EVM codegen lowers it to
`require(msg.sender == m_member, "throw(N)")`. Tracked by
`evm_gt1_from_address_member_lowers_to_msg_sender_eq` and
`evm_gt1_from_member_with_throw_code_emits_throw_n`. Used end-to-end by
`Governor.cancel`, all three role-gated `TimelockController` routes, and
verified by the corresponding Foundry / revm tests.



- **OZ pattern.** `bytes32 PROPOSER_ROLE; _grantRole(PROPOSER_ROLE, p);
  _checkRole(PROPOSER_ROLE)` — i.e. "the sender's address must equal one
  of a known set of role-holder addresses".
- **Attempted Cambrian construct.** `from m_proposer` (no parens; treat
  the address-typed member as the sender to compare against).
- **Observed.** The grammar only supports `from Entity(args)`. A bare
  `from m_proposer()` is parsed as "Entity named `m_proposer`" and lowers
  to `require(false, "from clause failed")` because no such entity
  exists.
- **v1 workaround.** Replace each `from m_proposer` with
  `where msg::sender == m_proposer : throw N`. Less ergonomic but
  semantically equivalent for single-holder roles.
- **Design options.**
  1. Add `from <member>` syntax (member must be `address` or
     `Vec<address>`); lowers to `require(msg.sender == m_x, ...)` or
     OR-chain over the vector.
  2. Add `from <expr>` for arbitrary boolean expressions (subsumes #1
     but loses the syntactic guarantee that the check is over
     `msg.sender`).
  3. Keep `where` and document it as the canonical pattern; deprecate
     entity-only `from`.

### G-T2 — `sys::timestamp + N` in member transforms *(RESOLVED)*

**Status.** Subsumed by the broader G-T7 fix: `sys::timestamp` lowers to
`block.timestamp` in every position, including transform-RHS arithmetic
(`m_op_timestamp.update(id, sys::timestamp + delay)`). Pinned by
`evm_gt2_sys_timestamp_in_transform_lowers_to_block_timestamp` and end-
to-end by `TimelockController.test.cam::"schedule records ready-at as
now + delay"`.



- **OZ pattern.** `_timestamps[id] = block.timestamp + delay;` in
  `TimelockController.schedule(...)`.
- **Attempted Cambrian construct.**
  `m_op_timestamp.update(id, sys::timestamp + delay)` inside
  `in schedule(target, value, data, predecessor, salt, delay) => ...`.
- **Observed.** The EVM codegen emits
  `m_op_timestamp[id] = /* unsupported expr */ 0;`. The `sys::timestamp`
  read or the `+` arithmetic on it (or both) is unsupported in
  member-transform context. The same expression in a `where` clause
  works.
- **v1 workaround.** None used yet — readiness is checked off-chain
  and the timestamp slot is left at zero. A real implementation must
  fix this for `execute(...)` to gate on the elapsed delay.
- **Design options.**
  1. Lower `sys::timestamp` in transform context to `block.timestamp`
     directly.
  2. Hoist the read into the route prelude (similar to `var` capture)
     and reference it from transforms.

### G-T3 — `data:` field of named send not threaded into `.call(...)` *(RESOLVED)*

**Status.** EVM codegen for `~> address with { value: v, data: d }`
threads `d` into the `.call(...)` payload (the previous lowering hard-
coded `bytes("")`). `TimelockController.execute(...)` now forwards
arbitrary calldata to its target. Tracked by
`evm_gt3_named_send_data_field_threaded_into_call`.



- **OZ pattern.** `target.call{value: v}(data)` — execute arbitrary
  calldata against an address.
- **Attempted Cambrian construct.**
  `~> target with { value: value, data: data }`.
- **Observed.** The EVM codegen drops `data:` and emits
  `payable(target).call{value: value}(bytes(""))` — the calldata is
  always empty, so the executed call always hits `target`'s receive /
  fallback function rather than the intended selector.
- **v1 workaround.** Restrict `TimelockController.execute(...)` to
  receive-style targets (e.g. `CounterTarget.bump()` is the only
  canonical sink in this example) until the gap is closed.
- **Design options.**
  1. Extend the EVM codegen for plain `~> address with { ... }` sends to
     read `data:` (and any future `selector:`) and pass them as the
     calldata arg of `.call(...)`.
  2. Restrict raw-`address` sends to value-only and require typed
     `Address<E>` for arbitrary calldata.

### G-T4 — Duplicate `function constructor(...)` emission *(RESOLVED)*

**Status.** `is_emittable_route` now treats any route literally named
`constructor` as an init route (regardless of the `init` keyword). The
body is folded into the synthesised Solidity constructor (or
`initialize()` in deterministic mode); no public
`function constructor(...)` is emitted. Tracked by
`evm_gt4_no_duplicate_function_constructor_emitted`.



- **Observed.** Every entity with a `constructor(...)` route emits both
  the actual Solidity `constructor() { ... }` (parameterless, defaults
  only) and a separate public `function constructor(...) external {
  ... }` that runs the route body. The latter is invalid Solidity
  (`constructor` is a reserved keyword for the actual constructor) and
  is rejected by `solc`.
- **Pre-existing.** Surfaced by every det-mode fixture too. Tracked as
  a transpiler bug; out of scope for this example until fixed in
  `cambrian-transpiler/src/codegen/solidity/evm/route.rs::gen_constructor_impl`.

### G-T6 — `if/else` whose branches are `m_xs.update(...)` is dropped to `// no-op` *(RESOLVED)*

**Status.** Fixed in `cambrian-transpiler/src/codegen/solidity/evm/transform.rs` — the
codegen now lowers `if cond { m.update(k, v1) } else { m.update(k, v2) }`
into proper conditional `mapping[...] = ...;` writes. `Governor.castVote`
correctly accumulates `m_for[id] += weight` / `m_against[id] += weight`
based on the `support` value, and `Governor.settle` writes `Succeeded`
or `Defeated` per the quorum branch. Regression tests live in
`Governor.test.cam` ("settle after deadline ..." family).



- **OZ pattern.** Tally branch:
  `if (support == VoteType.For)  _forVotes[id]    += weight;`
  `if (support == VoteType.Against) _againstVotes[id] += weight;`
- **Attempted Cambrian construct.**
  ```cambrian
  m_for: HashMap<U256, U256> {
      in castVote(proposal_id, support) => tally: {
          if support == 1 {
              m_for.update(proposal_id, m_for[proposal_id] + weight)
          } else {
              m_for
          }
      }
  }
  ```
- **Observed.** EVM codegen replaces the entire transform body with the
  comment `// mapping m_for transform: no-op`. The branch that *would*
  perform the update is silently discarded — the storage slot never
  changes.
- **Same failure mode** for `m_proposal_state` in `Governor.settle(...)`:
  the `if for > against && for >= quorum { state.update(..., Succeeded) }
  else { state.update(..., Defeated) }` shape is dropped, so the
  proposal stays `Active` forever.
- **Suspected cause.** The codegen recognises `m_xs.update(...)` only
  when it is the syntactic root of the transform body, not when it is
  nested inside an `if/else` that the codegen can flatten to a
  conditional `mapping[...] = ...` write.
- **v1 workaround.** None usable for the tally case — `m_for` and
  `m_against` are written *unconditionally*, so a single `castVote`
  would accumulate weight in *both* maps. Documented as the reason
  `Governor.castVote(...)` is currently observable only via its
  cross-contract `var` capture, not via the tallies.
- **Design options.**
  1. Lower `if cond { m.update(k, v1) } else { m.update(k, v2) }` to
     `mapping[k] = cond ? v1 : v2;` (only when both branches share the
     same key).
  2. Lower the general form to `if (cond) { mapping[k1] = v1; } else
     { mapping[k2] = v2; }` (no key-equality requirement).
  3. Require the user to split into two member transforms with mutually
     exclusive `where` clauses on the route — but route-level `where`
     binds the whole route, not specific transforms, so this needs a
     "transform-level guard" feature (not in the language today).

### G-T7 — `sys::timestamp` in transforms or route-level `where` *(RESOLVED)*

**Status.** Fixed by the `sys::*` portable cross-target reads. EVM codegen
now lowers `sys::timestamp` to `block.timestamp` in every position —
transform RHS (`m_op_timestamp.update(id, sys::timestamp + delay)`),
route-level `where sys::timestamp >= m_proposal_deadline[id] : throw 501`,
and view returns. Regression tests:
`Governor.test.cam::"propose records deadline as now + voting_period"`,
`Governor.test.cam::"settle before deadline throws 501"`,
`TimelockController.test.cam::"schedule records ready-at as now + delay"`.



- **OZ patterns.**
  - `_timestamps[id] = block.timestamp + delay;` (transform position)
  - `require(block.timestamp >= _timestamps[id], "not ready");`
    (route-level `where` position)
- **Attempted Cambrian constructs.**
  - `m_op_timestamp.update(id, sys::timestamp + delay)`
  - `where sys::timestamp >= m_proposal_deadline[id] : throw 501`
- **Observed.**
  - In a transform body: emits `mapping[k] = /* unsupported expr */ 0;`
  - In a route-level `where`: emits `require(false, "where throw N");`
- **v1 workaround.** None — readiness checks are simply omitted from
  `Governor.settle(...)` and `TimelockController.execute(...)`. The
  structural intent is captured in the `.cam` source for later, but
  the generated Solidity does not enforce the time gate.
- **Design options.**
  1. Lower `sys::timestamp` in EVM codegen to `block.timestamp` in
     every position (transform RHS, where clause, var capture, view
     return). The expression-level lowering is trivial; the bug is
     that the recogniser short-circuits before the lowering runs.
  2. Add a dedicated `now()` builtin that the codegen pattern-matches
     more reliably, and deprecate `sys::timestamp` in EVM context.

### G-T8 — Empty interface stubs in single-file `--target evm` mode *(KNOWN, by design)*

**Status.** Intentional behaviour, not a bug to fix in v1: when an entity
references a typed address whose target is **not** in the same Program,
the codegen emits a stub `interface IFoo { /* external entity — add
route signatures as needed */ }` rather than guessing. The supported
workflow is `cambrian-transpiler --project examples/governor/project.yaml`,
which merges all `.cam` sources into a single Program before lowering;
in that mode the cross-entity interfaces are populated from the actual
route signatures. Both branches are pinned by
`evm_gt8_cross_entity_interface_emission_pins_both_modes`.



- **Observed.** Transpiling `Governor.cam` standalone (no
  `--project`) emits `interface IERC20Votes { /* add route signatures
  as needed */ }` with no methods, which then breaks the cross-call
  `IERC20Votes(m_token).getVotes(msg.sender)`.
- **Project-mode workaround.** `cambrian-transpiler --project
  examples/governor/project.yaml` correctly populates the interfaces
  from the other `.cam` sources. This is the supported flow, but the
  single-file UX is a sharp edge for anyone copy-pasting a single
  contract into `cambrian-transpiler --target evm`.

### G-T5 — `mapping(...)` as `pure fn` parameter *(RESOLVED)*

**Status.** `gen_pure_fn` now lowers any `HashMap<K,V>` parameter as a
`mapping(K => V) storage` reference (the only legal location for a
mapping parameter in Solidity) and demotes the function from `pure` to
`view` — storage reads are not pure in Solidity's sense, even though
the Cambrian-level purity contract still holds. Free functions also
correctly omit visibility modifiers (forbidden by Solidity at file
scope). Tracked by
`evm_gt5_pure_fn_hashmap_param_uses_storage_view_no_visibility`.



- **OZ pattern.** `function _balance(address a) internal view returns
  (uint256) { return _balances[a]; }` — internal helper that reads from
  a mapping.
- **Attempted Cambrian construct.**
  `pure fn balance_of(balances: HashMap<address, U256>, owner: address)
  -> U256 { ... }`.
- **Observed.** Lowers to `function balance_of(mapping(address =>
  uint256) balances, address owner) pure internal returns (uint256)
  { ... }`. Solidity does not allow mappings as memory parameters of
  internal/pure functions.
- **Pre-existing.** Already present in `contracts/token.cam` and
  several other reference fixtures; documented as a known transpiler
  limitation. The Cambrian source intentionally uses the idiom because
  it is the natural way to express a pure helper, and we expect the
  EVM backend to grow special-case lowering for `HashMap` params (e.g.
  emit them as `storage` references).
