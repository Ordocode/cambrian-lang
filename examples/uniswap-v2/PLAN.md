# Uniswap V2 — Cambrian translation plan

## Overview

This example re-expresses Uniswap V2 Core — `UniswapV2Pair`,
`UniswapV2Factory`, and a separate test ERC20 wrapped by the pair —
in idiomatic Cambrian targeting EVM. The reference Solidity (pinned to
commit `6a9e7c97860676e0992f22a49665760444c1cdf5` on
[`Uniswap/v2-core`](https://github.com/Uniswap/v2-core)) lives under
`ref/core/` as the behavioural spec; the Cambrian side is **not** a
line-for-line port. It re-uses these Cambrian features:

- **2-`identity` deterministic CREATE2 addresses.** `UniswapV2Pair`
  declares `identity { token0: address, token1: address }`; every
  cross-contract reference (`Pair.address(t0, t1)`) resolves to the
  same predicted address that
  `CambrianFactory.deployUniswapV2Pair(t0, t1)` deploys to. This is
  the first fixture in the repo to exercise CREATE2 with N>1 identity
  fields — `examples/governor/PLAN.md` flagged it as untested.
- **Multi-instance test ERC20.** `ERC20` declares
  `identity { token_id: u8 }` so the Pair test harness can predict and
  deploy two distinct token contracts at known addresses without a
  hand-written factory.
- **Member-centric state.** Reserves, balances, allowances, pair
  registry, fee config — all updated through `in route(...) =>`
  transforms; no `_update` / `_beforeTokenTransfer` hook indirection.
- **`from <member>` sender chains** for the factory's `feeToSetter`
  guards (`from m_fee_to_setter : throw 10`). Lowers to
  `require(msg.sender == m_fee_to_setter, "throw(10)")`.
- **`hashOf(...)`** — used to key `m_pairs` by
  `hashOf(min(t0, t1), max(t0, t1))`. Lowers to
  `keccak256(abi.encode(...))` on EVM.
- **EVM `var` capture across phases** for synchronous return values.
  `mint`, `burn`, and `swap` all read
  `balanceOf(sys::address) ~> token0` and `~> token1` in their
  `read:` / `callback:` phases, then consume the captured values in
  later phases (the K-invariant lives on `swap`'s `check:` phase).
- **Per-phase `where` clauses.** The K-invariant
  `k_holds(bal0, bal1, m_reserve0, m_reserve1, amount0_out, amount1_out) : throw 203`
  lives on the `check:` phase of `swap` so the captured `bal0` / `bal1`
  are in scope when the precondition runs.
- **`.fold(...)` over a `Range`.** Babylonian sqrt is implemented as a
  256-step scalar fold over `0..256`, lowered by the EVM backend (Phase
  1 of this delivery) to an imperative for-loop with a single
  accumulator. See `contracts/fold_evm.cam` for the standalone
  fixture; `pure fn sqrt(y)` here is the user.
- **Flash-swap calldata threading.** `swap(...)`'s `callback:` phase
  emits `~> to with { value: 0, data: data }`; G-T3's calldata-pass-
  through fix (`solidity/evm/route.rs` — typed send calldata threading) ensures the original `data:` field
  reaches the flash callee verbatim.

## Entities (3 + 1 sink)

- **`ERC20`** — multi-instance (`identity { token_id: u8 }`) test
  token. Routes: `constructor(supply, holder)`, `transfer`,
  `transferFrom`, `approve`, `view balanceOf` / `allowance` /
  `totalSupply`. Member-centric `m_balances`, `m_allowances`,
  `m_total_supply`. Singleton variant lives in
  `examples/governor/ERC20Votes.cam`; here we deliberately exercise the
  multi-instance form so the Pair test harness can wrap two distinct
  ERC20 instances with `ERC20.address(0)` / `ERC20.address(1)`.
- **`UniswapV2Pair`** — `identity { token0: address, token1: address }`.
  - `mint(to)` is **2-phase**: `read:` captures `bal0` / `bal1` off
    both tokens; member transforms run in the unphased default phase
    after, updating reserves to `bal0` / `bal1` and minting LP shares
    to `to`. Per-phase `where` rejects non-deposits
    (`bal0 > m_reserve0 && bal1 > m_reserve1 : throw 100`).
  - `burn(to)` is **2-phase**: `read:` captures balances; `settle:`
    transfers proportional `(amount0_out, amount1_out)` back to `to`
    via `~> token0 with { value: 0, data: encodeTransfer(to, amount0_out) }`
    and the same for `token1`.
  - `swap(amount0_out, amount1_out, to, data)` is **3-phase**:
    `optimistic:` does the optimistic transfers; `callback:` invokes
    the flash callee with the user-supplied `data:` (no-op when empty)
    and re-reads balances; `check:` runs the K-invariant under a
    per-phase `where` and rejects non-repayments with `throw 203`.
  - `view getReserves()` returns `(m_reserve0, m_reserve1, m_block_timestamp_last)`.
  - `m_block_timestamp_last` is updated in every state-changing route
    via `in mint(_) => sys::timestamp` (etc.) — verifies G-T7.
- **`UniswapV2Factory`** — singleton.
  - `createPair(t0, t1)` enforces `t0 != t1` (throw 1),
    `t0 != 0 && t1 != 0` (throw 2), no-duplicate via
    `!m_pairs.exists(hashOf(min, max))` (throw 3), then
    `deploy UniswapV2Pair(min(t0, t1), max(t0, t1))`. Member transforms
    record the new pair into both `m_pairs` (hash-keyed lookup) and
    `m_all_pairs` (counter-keyed indexed view).
  - `setFeeTo(addr)` and `setFeeToSetter(addr)` are gated on
    `from m_fee_to_setter : throw 10/11`.
  - `view allPairs(idx)` reads off `m_all_pairs[idx]` — see G-U3 for
    why we use a `HashMap<u64, address>` instead of OZ's `address[]`.
- **`FlashCallback`** *(plain Solidity, in `ref/target/`)* — trivial
  flash callee that the test suite uses to verify the `swap(...)`
  callback round-trip. Mirrors the role of `CounterTarget.sol` in the
  governor example. Has a `setRepayBP(bp)` setter so a single test
  contract can drive both the "repays correctly" and "fails K-check"
  scenarios.

## Auto-generated factory

The EVM backend auto-emits `CambrianFactory.sol`, providing
`predictERC20(token_id) / deployERC20(token_id, supply, holder)`,
`predictUniswapV2Pair(t0, t1) / deployUniswapV2Pair(t0, t1)`, and
`predictUniswapV2Factory() / deployUniswapV2Factory(fee_to_setter)`.
The factory splits each constructor into a deploy-time stub plus a
factory-guarded `initialize(...)` exactly as governor does. There is
no hand-written `UniswapV2Factory.cam`-side factory — the
**Cambrian** `UniswapV2Factory` is the *Uniswap-business-logic*
factory (the registry + `createPair(...)` admin-gated entry point);
the auto-emitted `CambrianFactory.sol` is the *deployment-mechanics*
factory that handles CREATE2 keyed on identity members.

## Uniswap ↔ Cambrian recipe summary

| Uniswap V2 Solidity | Cambrian | Feature |
|---|---|---|
| `UniswapV2Factory.allPairs[i]` | `m_all_pairs: HashMap<u64, address>` + `m_all_pairs_length: u64` | G-U3 workaround |
| `getPair[t0][t1] = ... ; getPair[t1][t0] = ...` | `m_pairs[hashOf(min(t0,t1), max(t0,t1))]` | `hashOf` builtin |
| `IUniswapV2Pair(...).initialize(t0, t1)` | `identity { token0, token1 }` + auto-`CambrianFactory` | Identity CREATE2 |
| `_update(balance0, balance1, ...)` | `in mint(_) => bal0` / `in mint(_) => bal1` member transforms | Member-centric state |
| `bytes32 PAIR_INIT_CODE_HASH` | `Pair.address(min, max)` | Built-in CREATE2 prediction |
| `IUniswapV2Callee(to).uniswapV2Call(sender, a0, a1, data)` | `~> to with { value: 0, data: data }` (callback phase) | Named send + G-T3 calldata threading |
| `require(balance0 * balance1 >= reserve0 * reserve1 * (...) , "K")` | per-phase `where (k_holds(...)) : throw 203` | Per-phase `where` |
| `safeTransfer(token, to, value)` | `~> token with { data: encodeTransfer(to, value) }` | Generic external call (no SafeERC20 needed since EVM backend reverts on `success == false`) |
| `_mintFee(_reserve0, _reserve1)` | `m_balances` and `m_total_supply` member transforms gated on `m_fee_to_set` | Member-centric protocol fee |
| `Math.sqrt(...)` | `pure fn sqrt(y)` using `(0..256).fold(y, |r, _| ...)` | `.fold` on EVM |

## `G-Un` gap log

Status legend:

- **OPEN** — gap is real, not worked around in this example, blocks
  feature-parity with upstream V2 Core. **Requires future work.**
- **WORKAROUND** — gap is real and remains in the transpiler /
  test-codegen, but this example side-steps it cleanly inside the
  `.cam` sources. Future work would *let* the example express the
  upstream-faithful shape without changing behaviour.
- **RESOLVED** — risk noted before Phase 2 turned out to land cleanly
  in the existing codegen / orchestration; entry kept in the log as
  the canonical fixture in case of regression.
- **ENVIRONMENT** — not a Cambrian / transpiler bug; gated on the
  developer's local toolchain.

| Gap | Status | Notes |
|---|---|---|
| **G-U1**: `permit()` / EIP-2612 on the LP token | **RESOLVED** | `evm::ecrecover(hash, v, r, s)` and `evm::keccak256Packed(...)` are now first-class on the EVM target (`gen_evm_ns` in `solidity/core/expr.rs`); the EIP-712 digest is built directly in Cambrian and `permit()` lives on `ERC20.cam` with `m_nonces: HashMap<address, U256>`. Forge + revm unit tests cover the happy path, expired-deadline (`throw 4`), and bad-signature (`throw 5`) revert paths. |
| **G-U2**: cumulative-price oracle (`price0CumulativeLast`, `price1CumulativeLast`) | **RESOLVED** | `+%` / `-%` / `*%` lower through `_wadd` / `_wsub` / `_wmul` `internal pure unchecked` helpers, emitted only when the entity uses any wrapping op (`gen_wrapping_op_helpers`). `UniswapV2Pair.cam` carries `m_price0_cumulative_last` / `m_price1_cumulative_last`, a `uqdiv` helper, and per-route TWAP delta updates; tests cover the fresh-pair zero case and the wrap path. |
| **G-U3**: `address[] public allPairs` | **RESOLVED** | `Vec<T>.push(arg)` in member-transform position lowers to a statement-shaped `m_xyz.push({arg});` via `lower_member_push_body`; `default_value` skips the redundant `m_xyz = 0` for dynamic arrays. `UniswapV2Factory.cam`'s `m_all_pairs: Vec<address>` matches upstream `address[] public allPairs;`, and `allPairsLength()` is just `m_all_pairs.len()`. |
| **G-U4**: 2-identity `CambrianFactory.deployUniswapV2Pair(t0, t1)` end-to-end with `solc` | **RESOLVED** | First fixture in the repo to exercise CREATE2 with N=2 identity fields. `gen_create2_address_expr` (`solidity/core/create2.rs`) handled it without change; `Pair.address(t0, t1)` agrees with the deploy-time address from `CambrianFactory.deployUniswapV2Pair`. |
| **G-U5**: cross-contract `var` capture inside `mint`/`burn` (state-writing route reading off another contract synchronously, then sending back to the same contract in a later phase) | **RESOLVED** | Phase orchestrator threads `bal0`/`bal1` from `read:` through to the unphased default phase (for `mint`) and `settle:` (for `burn`) without change; the documented escape hatch (collapse to a 1-phase shape that wraps `balanceOf` in a pure helper) was not needed. Proportional-burn algebra exercises the captured vars on the RHS of every `m_*` transform; round-trip `mint → burn` fuzz tests pass. |
| **G-U6**: 3-phase `swap` (`optimistic: → callback: → check:`) | **RESOLVED** | Deeper than governor's 2-phase `castVote`. Worked first try; K-invariant on the `check:` phase rejects non-repaying flash swaps with `throw 203` and the proptest harness finds no counter-examples. The documented fallback (collapse to 2 phases by inlining `optimistic:` into the top of `callback:`) was not needed. |
| **G-U7**: address-shaped hex literals are inferred as `uint256` at the Solidity boundary | **RESOLVED** | `gen_expr` and `infer_expr_type` now recognise 40-digit hex literals (and 64-digit literals with high 24 bytes zero) and emit `address(uint160(uint256(0x{:0>64})))` so Solidity treats them as addresses without checksum noise. The `tokenA != 0x0…0` guards are back in `UniswapV2Factory.createPair`, the `feeTo()` zero-assertion is uncommented, and the test-codegen address wrappers were dropped. |
| **G-U8**: `let` bindings of `Entity.address(args)` collide / mistype inside member transforms | **RESOLVED** | `emit_transforms_sol` now wraps each transform body in its own `{ ... }` whenever the transform uses `let` or aliases (`transform_has_let` / `gen_transform_param_aliases`), and `infer_let_type_entity` recognises `Entity.address(...)` as `address`-typed. `UniswapV2Factory.cam` reuses `let pair = UniswapV2Pair.address(t0, t1)` across the `m_get_pair` and `m_all_pairs.push(...)` transforms without colliding. |
| **G-U9**: `.test.cam` / `.invariant.cam` scaffolding cannot seed identity members | **RESOLVED** | The single- and multi-instance invariant deploy paths in `evm_test_codegen.rs` and `evm_revm_test_codegen.rs` thread per-instance `init_state` through to the constructor: each identity member is seeded from the `with { ... }` (or `init { ... }`) clause and falls back to a per-type default only when the invariant didn't seed it. `ERC20.invariant.cam` and `UniswapV2Pair.invariant.cam` are restored and listed in `project.yaml`. |
| **G-U10**: cargo-fuzz coverage harness needs `rustc ≥ 1.90` | **RESOLVED** | `cargo_fuzz_codegen.rs` now emits `fuzz/rust-toolchain.toml` (channel `nightly` with `rust-src`) for both the revm and Acki Nacki backends, so `cargo` inside the fuzz crate auto-selects nightly via rustup proxies regardless of the host's `default` toolchain. The README caveat is removed. |

### Summary

- **0 still open**.
- **All ten G-Un items resolved.** Phase 2 closed **G-U4** /
  **G-U5** / **G-U6** by construction; the remaining seven (**G-U1**,
  **G-U2**, **G-U3**, **G-U7**, **G-U8**, **G-U9**, **G-U10**) are
  closed by the *resolve uniswap-v2 gap log* pass — see the matching
  rows above for the surface that lifted them.

## Per-entity / per-route status

| Entity | Routes | Transpile | `solc` | `forge test` | `revm-tests` |
|---|---|---|---|---|---|
| `ERC20` | constructor / transfer / transferFrom / approve / mint / burn / view balanceOf-allowance-totalSupply | ✅ | ✅ | ✅ | ✅ (12/12) |
| `UniswapV2Pair` | constructor / mint(2-phase) / burn(2-phase) / swap(3-phase) / sync / skim / view getReserves-balanceOf-totalSupply | ✅ | ✅ (`via_ir`) | ✅ | ✅ (5/5) |
| `UniswapV2Factory` | constructor / createPair / setFeeTo / setFeeToSetter / view allPairs(idx)-allPairsLength-getPair-feeTo-feeToSetter | ✅ | ✅ | ✅ | ✅ (6/6, incl. 1 invariant) |

`forge test` summary: **32 / 32 passing** across the three entities
(unit + fuzz + three invariants — `UniswapV2Factory.allPairs-length`,
`ERC20.totalSupply-conserved`, `UniswapV2Pair.identity-tokens`).
`revm-tests` (proptest harness) reports **31 / 31 passing**. The
cargo-fuzz coverage targets emitted under `revm-tests/fuzz/` build
cleanly with the auto-emitted `rust-toolchain.toml` (G-U10).

## Risks / things that might force a smaller v1 — *resolution log*

- **2-identity `CambrianFactory.deployUniswapV2Pair(t0, t1)`** —
  resolved. `gen_create2_address_expr` handles N=2 correctly; the
  emitted `CambrianFactory.sol` compiles and tests against
  `Pair.address(t0, t1)` agree with the deploy-time address.
- **Two-token deploy in tests** — resolved. `ERC20.address(0)` and
  `ERC20.address(1)` resolve to two distinct CREATE2 addresses and
  the Pair test harness deploys both via `CambrianFactory`.
- **`feeTo() ~> Factory` synchronous read inside `Pair.mint`/`burn`**
  — resolved. The two-phase shape (`read: → unphased default`) works
  on EVM; `bal0`/`bal1` are captured in `read:` and consumed in the
  default phase that also writes the route's own state.
- **3-phase `swap`** — resolved. `optimistic: → callback: → check:`
  works as specified; G-U6 is closed. The K-invariant on the `check:`
  phase rejects non-repaying flash swaps with `throw 203` and
  proptest-driven fuzzing finds no counter-examples.
- **`sqrt` via `.fold`** — resolved by Phase 1's `.fold` lowering on
  EVM (`solidity/core/iter.rs` — `gen_fold_loop` / scalar reduce). The route emits a flat
  imperative for-loop with one scalar accumulator, no temporaries
  hoisted.

The remaining open items are the transpiler / test-codegen gaps
**G-U7 / G-U8 / G-U9 / G-U10**, all of which either have a clean
worked-around path inside this example or are bounded by environment
(`rustc` toolchain), and none of which gate the core AMM flow.
