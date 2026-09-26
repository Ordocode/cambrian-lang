-- SPDX-License-Identifier: UNLICENSED
import Cambrian.Core

/-
Cambrian.Evm — EVM-domain prelude (MsgCtx / SysCtx / WorldState / crypto),
hermetically vendored from `cambrian-transpiler`.

Bumping this file is a transpiler PR: the transpiler copies it (or an
executable-intrinsic splice of it) into every generated Lean project
under `Cambrian/Evm.lean`. Declaration names stay under
`namespace Cambrian` (`Cambrian.MsgCtx`, `Cambrian.WorldState`, …) —
digest-neutral for PoP; only the file layout changes.

Generated modules keep `import Cambrian.Prelude`; the facade re-exports
this module alongside `Cambrian.Core`.
-/

namespace Cambrian

/-! ## Call-frame stub (P1.6 → P3)

The stub fields are stable so P1 routes can be written once and not
re-signed when P3 swaps in the real `WorldState` plumbing. -/

/-- Per-call message context. P0 carries only what `msg::*` references in
P1-scope code can read; P3 enriches this with the active call frame. -/
structure MsgCtx where
  sender    : Address
  value     : U256
  timestamp : BitVec 64
  chainId   : U256
  deriving Repr

namespace MsgCtx
  /-- Neutral default useful in tests/specs. Not a meaningful value. -/
  def default : MsgCtx :=
    { sender    := 0#160
      value     := 0#256
      timestamp := 0#64
      chainId   := 1#256 }

  /-- Override the message sender. P2 spec emitter uses the open form
  `{ ctx with sender := … }` for goal-readability; this helper exists
  so user proofs can `simp` over a single named def instead of a
  record-update term. -/
  def withSender (ctx : MsgCtx) (a : Address) : MsgCtx :=
    { ctx with sender := a }

  /-- Override the message value (native asset attached to the call). -/
  def withValue (ctx : MsgCtx) (v : U256) : MsgCtx :=
    { ctx with value := v }

  /-- Override the block timestamp. -/
  def withTimestamp (ctx : MsgCtx) (t : BitVec 64) : MsgCtx :=
    { ctx with timestamp := t }

  /-- Advance the block timestamp by a delta. Mirrors the
  `advanceTime` action in `#[with_time]` invariants. -/
  def advanceTime (ctx : MsgCtx) (d : BitVec 64) : MsgCtx :=
    { ctx with timestamp := ctx.timestamp + d }
end MsgCtx

/-- Per-block / VM system context. P0 carries only what `sys::*` in
P1-scope code can read; the bigprojects bump extends it with chain
metadata (`chainid`, `blockNumber`) and wall-clock `timestamp` that
EIP-712 / governance code references in route bodies. `address` is
*not* stored here — it is derived per-entity from `(E.address inst)`
in World mode. -/
structure SysCtx where
  balance     : U256
  chainId     : U256
  blockNumber : BitVec 64
  timestamp   : BitVec 64
  deriving Repr

namespace SysCtx
  /-- Neutral default. `chainId := 1` mirrors `BlockEnv.default` so
  cross-contract checks line up out of the box. -/
  def default : SysCtx :=
    { balance     := 0#256
      chainId     := 1#256
      blockNumber := 0#64
      timestamp   := 0#64 }
end SysCtx

-- CAMBRIAN:INTRINSICS:BEGIN
-- Everything between this marker and CAMBRIAN:INTRINSICS:END is the
-- *opaque* model of the EVM crypto intrinsics. The transpiler swaps this
-- whole region for `Cambrian/IntrinsicsExec.lean` (computable, deterministic
-- reference models) when emitting the plausible-testing prelude variant; the
-- FV prelude keeps the opaque block below. Keep the two variants in sync:
-- same names, arities, and result types.
/-! ## EVM intrinsics (axiomatised — opaque models, no executable body)

The Cambrian source surfaces a handful of EVM-native primitives —
`evm::keccak256`, `evm::keccak256Packed`, `evm::ecrecover`, and the
target-agnostic `hashOf(...)` (lowers to `keccak256(abi.encode(...))`
on EVM). Implementing them faithfully in Lean would pull in a full
Keccak / secp256k1 model, which is out of scope.

We instead expose them as **opaque** functions with the right type
signature. Downstream proofs can stipulate properties (`hashOf`
injective on equal inputs, `keccak256` collision-resistant, etc.)
via `axiom` declarations layered on top of these. None are asserted
here so the prelude stays minimal. -/

/-- `evm::keccak256(bytes)`. Opaque 256-bit hash. -/
opaque keccak256 (bs : ByteArray) : U256

/-- `evm::keccak256Packed` family. Solidity's
`keccak256(abi.encodePacked(a, b, c, …))` — concatenates the
abi-encoded chunks before hashing. Modelled as one opaque function
per call-site arity, polymorphic in the chunk types so the caller
can pass bytes, addresses, or `U256`s without pre-encoding. -/
opaque keccak256Packed1 {α       : Type} (a : α)                               : U256
opaque keccak256Packed2 {α β     : Type} (a : α) (b : β)                       : U256
opaque keccak256Packed3 {α β γ   : Type} (a : α) (b : β) (c : γ)               : U256
opaque keccak256Packed4 {α β γ δ : Type} (a : α) (b : β) (c : γ) (d : δ)       : U256

/-- `evm::ecrecover(digest, v, r, s)`. Recovers a 160-bit signer
address from an EIP-191 / EIP-712 digest and a secp256k1 signature
`(v, r, s)`. Opaque — the source contract calls this and compares the
result against a stored address; Lean callers do the same. -/
opaque ecrecover (digest : U256) (v : BitVec 8) (r s : U256) : Address

/-- Polymorphic `hashOf` family. Cambrian's `hashOf(a₁, …, aₙ)` lowers
to keccak256(abi.encode(a₁, …, aₙ)) on EVM; we model each arity as
its own opaque function, polymorphic in the argument types so the
call site doesn't need pre-widening. -/
opaque hashOf2 {α β       : Type} (a : α) (b : β)                                   : U256
opaque hashOf3 {α β γ     : Type} (a : α) (b : β) (c : γ)                           : U256
opaque hashOf4 {α β γ δ   : Type} (a : α) (b : β) (c : γ) (d : δ)                   : U256
opaque hashOf5 {α β γ δ ε : Type} (a : α) (b : β) (c : γ) (d : δ) (e : ε)           : U256
opaque hashOf6 {α β γ δ ε ζ : Type} (a : α) (b : β) (c : γ) (d : δ) (e : ε) (f : ζ) : U256

/-- Deterministic-address derivation (`CREATE2`). Opaque because a
fully-faithful Keccak implementation in Lean is out of scope for P3;
we model the function as injective via the axioms below, which is
sufficient for proofs over Cambrian routes that only need
`addressOf` agreement and disjointness, not raw byte-level equality.

The transpiler generates a per-entity `<Entity>.address (id : Identity)`
wrapper that picks the appropriate deployer / salt / initCodeHash for
that entity. -/
opaque create2Address (deployer : Address) (salt : BitVec 256)
    (initCodeHash : BitVec 256) : Address

/-- `create2Address` is injective in its argument triple. Captures
"two distinct deployer/salt/initCodeHash combinations yield distinct
addresses," which is the only thing P3 proofs ever need from CREATE2. -/
axiom create2Address_injective :
    ∀ d₁ s₁ h₁ d₂ s₂ h₂,
      create2Address d₁ s₁ h₁ = create2Address d₂ s₂ h₂ →
      d₁ = d₂ ∧ s₁ = s₂ ∧ h₁ = h₂
-- CAMBRIAN:INTRINSICS:END

/-! ## EVM blockchain model (P3)

Abstract semantic model of the EVM execution environment. Hand-rolled
(no `EVMYul` / `EvmYul` dependency); no gas, no opcodes, no bytecode.
Atomic `WorldState → WorldState` transitions: every `~>` collapses into
one step. Cambrian's phase boundaries already give us all the
intermediate-state observability we need; "no reentrancy" properties
are stated as *phase-level invariants* instead of cross-call traces.

The carrier is **polymorphic in the program's storage shape** and
**event-log element type** — the transpiler generates a
`Cambrian.Generated.Storage` record with one field per declared
entity, a `Cambrian.Generated.Event` inductive with one constructor
per declared `event`, and the generated `World` is
`Cambrian.WorldState Cambrian.Generated.Storage Cambrian.Generated.Event`. -/

/-- Per-block environment. Promoted from `MsgCtx` because it must be
mutable across calls — `advanceTime(...)` writes through to
`World.block.timestamp`, where multiple route invocations can witness
the updated value. -/
structure BlockEnv where
  timestamp : BitVec 64
  number    : BitVec 64
  chainId   : BitVec 64
  deriving Repr

namespace BlockEnv
  /-- Neutral default. `chainId` defaults to `1` (mainnet) rather than
  `0` because `chainId = 0` is reserved / invalid on most EVM chains
  and downstream `expect` assertions tend to compare it. -/
  def default : BlockEnv :=
    { timestamp := 0#64
      number    := 0#64
      chainId   := 1#64 }

  /-- Advance the block timestamp by a delta. Used by `World.advanceTime`. -/
  def advanceTime (b : BlockEnv) (d : BitVec 64) : BlockEnv :=
    { b with timestamp := b.timestamp + d }
end BlockEnv

/-- World state, parameterized by the per-program storage carrier `S`
and event-log element type `Ev`. The generated
`Cambrian.Generated.World` is
`Cambrian.WorldState Cambrian.Generated.Storage Cambrian.Generated.Event`.

`balances` is a total function `Address → U256` (no `Repr` instance —
proofs `case` on the underlying function pointwise).

`events` is the **ghost event log**: an append-ordered list of the
events emitted by `emit` actions across the call. It has no
operational effect (it never feeds back into route logic); it exists
so specs can assert *which* events a route emits and with what
payloads. -/
structure WorldState (S : Type) (Ev : Type) where
  storage  : S
  balances : Address → U256
  block    : BlockEnv
  events   : List Ev

/-- Failure modes for `WorldState.transfer`. -/
inductive TransferError where
  | insufficientBalance
  | selfTransfer
  deriving Repr, DecidableEq

namespace WorldState
  /-- Pure ledger update: move `v` from `src` to `dst`. Atomic — no
  reentrancy semantics. Fails on insufficient balance. Self-transfer is
  a no-op on the ledger (EVM permits it; `receive` / `fallback` run via
  the `WorldState.call` path — W2-BC-02). -/
  def transfer {S Ev : Type} (w : WorldState S Ev) (src dst : Address) (v : U256)
      : Except TransferError (WorldState S Ev) :=
    if src = dst then
      .ok w
    else if w.balances src < v then
      .error TransferError.insufficientBalance
    else
      .ok { w with
        balances := fun a =>
          if a = src then w.balances a - v
          else if a = dst then w.balances a + v
          else w.balances a }

  /-- Read a balance. Convenience wrapper so generated code can stay
  high-level. -/
  def balanceOf {S Ev : Type} (w : WorldState S Ev) (a : Address) : U256 :=
    w.balances a

  /-- Advance the world's block timestamp. Wraps `BlockEnv.advanceTime`
  so generated invariant `step` arms can write through to the world. -/
  def advanceTime {S Ev : Type} (w : WorldState S Ev) (d : BitVec 64) : WorldState S Ev :=
    { w with block := w.block.advanceTime d }

  /-- Append `e` to the ghost event log. `emit Name(args)` lowers to a
  call to this helper; the log preserves emission order (oldest first). -/
  def emit {S Ev : Type} (w : WorldState S Ev) (e : Ev) : WorldState S Ev :=
    { w with events := w.events ++ [e] }

  /-- Parameters for a typed cross-contract call (value transfer before
  the callee route runs). -/
  structure CallParams where
    to    : Address
    /-- Source address (`from` is reserved in Lean 4). -/
    src   : Address
    value : U256

  /-- Map ledger transfer failures to route throw codes (W2-BC-01). -/
  def transferThrowCode (e : TransferError) : ThrowCode :=
    match e with
    | .insufficientBalance => ThrowCode.ofNat 0x01
    | .selfTransfer        => ThrowCode.ofNat 0x02

  /-- Run `body` after optionally moving `params.value` from `params.src`
  to `params.to`. When `value` is zero, skips the transfer. Transfer
  failures abort before `body` (1:1 EVM — no best-effort credit). -/
  def call {S Ev : Type} {α : Type}
      (w : WorldState S Ev) (params : CallParams)
      (body : WorldState S Ev → Except ThrowCode (WorldState S Ev × α))
      : Except ThrowCode (WorldState S Ev × α) :=
    if params.value = 0#256 then
      body w
    else match transfer w params.src params.to params.value with
      | .ok w' => body w'
      | .error e => .error (transferThrowCode e)
end WorldState

end Cambrian
