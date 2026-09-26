-- Copyright (C) 2025-2026 The Cambrian Authors
-- SPDX-License-Identifier: GPL-3.0-only

import Cambrian.SimpAttrs

/-
Cambrian.Core — domain-free foundations, hermetically vendored from
`cambrian-transpiler`.

Bumping this file is a transpiler PR: the transpiler copies it verbatim
into every generated Lean project under `Cambrian/Core.lean`. There is
no separate Lake package to track and no per-project Core divergence.

Declaration names stay under `namespace Cambrian` (e.g. `Cambrian.U256`,
`Cambrian.RouteResult`) — this module only reorganises files, not the
public name surface. Downstream modules keep `import Cambrian.Prelude`
(the thin facade that re-exports Core + domain).

The Cambrian `simp` sets are registered in `Cambrian.SimpAttrs` (imported
above) so they can be both populated here and applied by generated
modules; see that file for the rationale.
-/

/- Canonicalize `BitVec` operations/relations to `Nat` (mod `2^w`) so a
`simp [cambrian_bitvec_simp]; omega` closes width-generic arithmetic
goals. Lemma names pinned for the vendored Lean toolchain (v4.26). -/
attribute [cambrian_bitvec_simp]
  BitVec.toNat_eq
  BitVec.lt_def
  BitVec.le_def
  BitVec.toNat_add
  BitVec.toNat_sub
  BitVec.toNat_mul
  BitVec.toNat_ofNat
  BitVec.ofNat_eq_ofNat

namespace Cambrian

/-- Prelude version stamp. The transpiler embeds the version that
generated the project; downstream tools can `#guard` compatibility. -/
def Version : String := "0.0.8-proof"

/-- Placeholder type returned by Lean lowerings of features deferred
to later phases (`Vec<T>`, `HashMap<K,V>`, closures, iterator chains).
Programs that never reference such features never see this type. -/
opaque Unsupported : Type

namespace Unsupported
  /-- Default value for [`Unsupported`]. Never inhabited — using this
  value at runtime is a transpiler bug. -/
  axiom default : Unsupported
end Unsupported

/-! ## Numeric primitives (P1.1)

By default every Cambrian integer primitive lowers to `BitVec n` so
wraparound semantics line up with EVM. Projects that set
`lean.numerics: nat` in `project.yaml` instead lower integer scalars
(`u8`…`u128`, `i8`…`i128`, `U256`) to `Nat` for overflow-free "simple
numbers" reasoning and to feed plausible-pipeline's bounded samplers;
`address`/`pubkey` (and `hashOf` results) stay fixed-width `BitVec`
regardless, so the abbrevs below are always in play. -/

/-- Unsigned 256-bit integer (Cambrian `U256` / `uint256`). -/
abbrev U256 : Type := BitVec 256

/-- 160-bit Ethereum-style address (Cambrian `address`). -/
abbrev Address : Type := BitVec 160

/-- 256-bit pubkey. EVM has no first-class pubkey, but the source
language carries one and the Lean target preserves it for downstream
specs. -/
abbrev Pubkey : Type := BitVec 256

/-! ## Errors and route results (P1.4) -/

/-- Throw codes raised from Cambrian routes (`throw N`, `where … : throw N`).
P3+ may extend this with named error variants. -/
inductive ThrowCode where
  | ofNat (n : Nat)
  deriving Repr, DecidableEq

/-- Result type for routes that *can* fail. Per the P1.4 return-type
policy, routes (and per-phase functions) are wrapped in `RouteResult`
only when they have at least one `where` clause or `throw` action;
otherwise they return their underlying type directly. -/
abbrev RouteResult (α : Type) : Type := Except ThrowCode α

/-- Recover a total result from an `Except`; used when a non-failing
caller invokes a route that returns `RouteResult`. -/
def exceptGetD {ε α : Type} (e : Except ε α) (fallback : α) : α :=
  match e with
  | .ok a => a
  | .error _ => fallback

/-! ### Reflection plumbing for error conditions (design A)

Generated routes guard their effects with `if !(<route>_pre_<i> …) then
throw …`, so a fail-mode route assembler desugars to a nested
`Except` `do`-block. These generic `@[simp]` lemmas reduce
`Except.isOk` through that desugaring (`pure` / `throw` / `<$>` / `>>=`
/ `ite`), leaving the `_pre_<i>` predicates opaque. The net effect:
once `cambrian_route_simp` unfolds a route, `(…Routes.r …).isOk`
collapses to the Boolean combination of its guards. -/

namespace RouteResult

@[simp] theorem isOk_pure {ε α} (a : α) :
    (pure a : Except ε α).isOk = true := rfl
@[simp] theorem isOk_throw {ε α} (e : ε) :
    (throw e : Except ε α).isOk = false := rfl
@[simp] theorem isOk_error {ε α} (e : ε) :
    (Except.error e : Except ε α).isOk = false := rfl
@[simp] theorem isOk_ok {ε α} (a : α) :
    (Except.ok a : Except ε α).isOk = true := rfl

/-- `isOk` ignores a successful `Functor.map` (`f <$> x`) — the route
entry-point's final `World.with<E> w inst <$> …` writeback. -/
@[simp] theorem isOk_map {ε α β} (f : α → β) (x : Except ε α) :
    (f <$> x).isOk = x.isOk := by cases x <;> rfl

/-- Push `isOk` through a guard `ite`. -/
@[simp] theorem isOk_ite {ε α} {c : Prop} [Decidable c] (a b : Except ε α) :
    (if c then a else b).isOk = (if c then a.isOk else b.isOk) := by
  by_cases h : c <;> simp [h]

/-- A `throw` short-circuits the rest of a `do`-block. -/
@[simp] theorem throw_bind {ε α β} (e : ε) (f : α → Except ε β) :
    (throw e >>= f) = throw e := rfl

/-- Push `isOk` through a `bind` (`s ← phaseₖ s; …`) — the shape a
*phased* route entry threads state through. Phased reflection is
inherently nested (a later phase's guard reads the state produced by
the earlier phase), so this exposes that structure rather than a flat
conjunction. -/
@[simp] theorem isOk_bind {ε α β} (x : Except ε α) (f : α → Except ε β) :
    (x >>= f).isOk = (match x with | .ok a => (f a).isOk | .error _ => false) := by
  cases x <;> rfl

/-- Normal form for the Bool guard exposed by [`isOk_ite`] after the
`!guard` branch is taken (`if guard = false then false else true`). -/
@[simp] theorem ite_false_true {c : Prop} [Decidable c] :
    (if c then false else true) = !decide c := by
  by_cases h : c <;> simp [h]

/-! ### `Except`-constructor reduction (`cambrian_except_simp`)

`test` / `invariant` theorems scrutinise a route call with
`match … with | .error _ => … | .ok _ => …` rather than `.isOk`. To let
that `match` iota-reduce after `cambrian_route_simp` unfolds the route,
these lemmas rewrite the monadic operations the assembler leaves behind
(`pure` / `throw` / `>>=` / `<$>`) into raw `Except.ok` / `Except.error`
constructors. The rewrite system is confluent — every concrete `Except`
`do`-block normalises to a single constructor regardless of rewrite
order — so it composes safely with the global `throw_bind`. Kept in its
own set (not `@[simp]`) so route-`isOk` reflection is unaffected. The
global `throw_bind` (above) is part of the default `simp` set and fires
alongside these in `test`/`invariant` proofs. -/

@[cambrian_except_simp] theorem pure_eq_ok {ε α} (a : α) :
    (pure a : Except ε α) = Except.ok a := rfl
@[cambrian_except_simp] theorem throw_eq_error {ε α} (e : ε) :
    (throw e : Except ε α) = Except.error e := rfl
@[cambrian_except_simp] theorem bind_ok {ε α β} (a : α) (f : α → Except ε β) :
    (Except.ok a >>= f) = f a := rfl
@[cambrian_except_simp] theorem bind_error {ε α β} (e : ε) (f : α → Except ε β) :
    (Except.error e >>= f : Except ε β) = Except.error e := rfl
@[cambrian_except_simp] theorem map_ok {ε α β} (f : α → β) (a : α) :
    (f <$> (Except.ok a : Except ε α)) = Except.ok (f a) := rfl
@[cambrian_except_simp] theorem map_error {ε α β} (f : α → β) (e : ε) :
    (f <$> (Except.error e : Except ε α)) = Except.error e := rfl

/-! ### Spec-statement combinators (`okAnd` / `okImplies` / `errCodeIs`)

Used by invariant / test / property theorem *statements* so the statement
carries no raw `match … | .error _ => True/False` (Phase N / PN-104).
Predictable emission historically spliced these into `Prelude`; they live in
`Core` so legacy theorems can call them too. -/

/-- The route call `r` failed with exactly throw-code `n`. -/
def errCodeIs {α : Type} (r : Cambrian.RouteResult α) (n : Nat) : Prop :=
  match r with
  | .error (Cambrian.ThrowCode.ofNat m) => m = n
  | .ok _ => False

@[simp] theorem errCodeIs_ok {α : Type} (a : α) (n : Nat) :
    errCodeIs (Except.ok a) n = False := rfl

@[simp] theorem errCodeIs_error {α : Type} (m n : Nat) :
    errCodeIs (α := α) (Except.error (Cambrian.ThrowCode.ofNat m)) n = (m = n) := rfl

/-- Named `Decidable` so Plausible can synthesize `Testable` on property
statements that use `errCodeIs`. An anonymous `match` on `Except` is
keyed per-occurrence and never matches a generic instance. -/
instance instDecidableErrCodeIs {α : Type} (r : Cambrian.RouteResult α) (n : Nat) :
    Decidable (errCodeIs r n) :=
  match r with
  | .error (Cambrian.ThrowCode.ofNat m) => (inferInstance : Decidable (m = n))
  | .ok _ => (inferInstance : Decidable False)

/-- The route call `r` succeeded and its payload satisfies `P`.
Replacement for `match r with | .ok a => P a | .error _ => False`
(`#[fail_on_revert]` / ok-asserting tests). -/
def okAnd {α : Type} (r : Cambrian.RouteResult α) (P : α → Prop) : Prop :=
  match r with
  | .ok a => P a
  | .error _ => False

@[simp] theorem okAnd_ok {α : Type} (a : α) (P : α → Prop) :
    okAnd (Except.ok a) P = P a := rfl

@[simp] theorem okAnd_error {α : Type} (e : Cambrian.ThrowCode) (P : α → Prop) :
    okAnd (Except.error e) P = False := rfl

/-- Named `Decidable` so Plausible can synthesize `Testable` on
`#[fail_on_revert]` invariant statements (`okAnd (runTrace …) P`). -/
instance instDecidableOkAnd {α : Type} (r : Cambrian.RouteResult α) (P : α → Prop)
    [∀ a, Decidable (P a)] : Decidable (okAnd r P) :=
  match r with
  | .ok a => (inferInstance : Decidable (P a))
  | .error _ => (inferInstance : Decidable False)

/-- If `r` succeeded its payload satisfies `P`; a failure is vacuously
accepted. Replacement for `match r with | .error _ => True | .ok w => P w`
(default invariant policy when `#[fail_on_revert]` is off). -/
def okImplies {α : Type} (r : Cambrian.RouteResult α) (P : α → Prop) : Prop :=
  match r with
  | .ok a => P a
  | .error _ => True

@[simp] theorem okImplies_ok {α : Type} (a : α) (P : α → Prop) :
    okImplies (Except.ok a) P = P a := rfl

@[simp] theorem okImplies_error {α : Type} (e : Cambrian.ThrowCode) (P : α → Prop) :
    okImplies (Except.error e) P = True := rfl

/-- Named `Decidable` so Plausible can synthesize `Testable` on the
PN-104 default invariant shape (`okImplies (runTrace …) P`). This is
the same discrimination-tree reason as overlay `CamCheck`: the
combinator must be a named constant, not an anonymous matcher. -/
instance instDecidableOkImplies {α : Type} (r : Cambrian.RouteResult α) (P : α → Prop)
    [∀ a, Decidable (P a)] : Decidable (okImplies r P) :=
  match r with
  | .ok a => (inferInstance : Decidable (P a))
  | .error _ => (inferInstance : Decidable True)

end RouteResult

/-! ## Checked arithmetic (BitVec)

Used when `lean.numerics: overflow-panic`. Solidity 0.8 panic code `0x11`
(arithmetic overflow/underflow). Wrapping ops (`+%` / `-%` / `*%`) stay
plain BitVec `+` / `-` / `*`. -/

/-- Checked addition: overflow → `ThrowCode.ofNat 0x11`. -/
def checkedAdd {n : Nat} (a b : BitVec n) : RouteResult (BitVec n) :=
  let sum := a.toNat + b.toNat
  if sum ≥ 2 ^ n then
    Except.error (ThrowCode.ofNat 0x11)
  else
    Except.ok (BitVec.ofNat n sum)

/-- Checked subtraction: underflow → `ThrowCode.ofNat 0x11`. -/
def checkedSub {n : Nat} (a b : BitVec n) : RouteResult (BitVec n) :=
  if a.toNat < b.toNat then
    Except.error (ThrowCode.ofNat 0x11)
  else
    Except.ok (BitVec.ofNat n (a.toNat - b.toNat))

/-- Checked multiplication: overflow → `ThrowCode.ofNat 0x11`. -/
def checkedMul {n : Nat} (a b : BitVec n) : RouteResult (BitVec n) :=
  let prod := a.toNat * b.toNat
  if prod ≥ 2 ^ n then
    Except.error (ThrowCode.ofNat 0x11)
  else
    Except.ok (BitVec.ofNat n prod)

/-- Signed two's-complement min / max for width `n` (as `Int`). -/
def minInt (n : Nat) : Int :=
  -(Int.ofNat (2 ^ (n - 1)))

def maxInt (n : Nat) : Int :=
  Int.ofNat (2 ^ (n - 1) - 1)

/-- Checked signed addition: two's-complement overflow → `0x11`. -/
def checkedSAdd {n : Nat} (a b : BitVec n) : RouteResult (BitVec n) :=
  let s := a.toInt + b.toInt
  if s < minInt n ∨ s > maxInt n then
    Except.error (ThrowCode.ofNat 0x11)
  else
    Except.ok (BitVec.ofInt n s)

/-- Checked signed subtraction: two's-complement overflow → `0x11`. -/
def checkedSSub {n : Nat} (a b : BitVec n) : RouteResult (BitVec n) :=
  let s := a.toInt - b.toInt
  if s < minInt n ∨ s > maxInt n then
    Except.error (ThrowCode.ofNat 0x11)
  else
    Except.ok (BitVec.ofInt n s)

/-- Checked signed multiplication: two's-complement overflow → `0x11`. -/
def checkedSMul {n : Nat} (a b : BitVec n) : RouteResult (BitVec n) :=
  let s := a.toInt * b.toInt
  if s < minInt n ∨ s > maxInt n then
    Except.error (ThrowCode.ofNat 0x11)
  else
    Except.ok (BitVec.ofInt n s)

/-- Division by zero → Panic `0x12` (Solidity 0.8). Independent of `lean.numerics`. -/
def checkedDiv {n : Nat} (a b : BitVec n) : RouteResult (BitVec n) :=
  if b = 0#n then Except.error (ThrowCode.ofNat 0x12)
  else Except.ok (a / b)

/-- Modulo by zero → Panic `0x12`. -/
def checkedMod {n : Nat} (a b : BitVec n) : RouteResult (BitVec n) :=
  if b = 0#n then Except.error (ThrowCode.ofNat 0x12)
  else Except.ok (a % b)

/-- Signed division by zero → Panic `0x12`. -/
def checkedSDiv {n : Nat} (a b : BitVec n) : RouteResult (BitVec n) :=
  if b = 0#n then Except.error (ThrowCode.ofNat 0x12)
  else Except.ok (a.sdiv b)

/-- Signed remainder by zero → Panic `0x12`. -/
def checkedSRem {n : Nat} (a b : BitVec n) : RouteResult (BitVec n) :=
  if b = 0#n then Except.error (ThrowCode.ofNat 0x12)
  else Except.ok (a.srem b)

def checkedDivNat (a b : Nat) : RouteResult Nat :=
  if b = 0 then Except.error (ThrowCode.ofNat 0x12)
  else Except.ok (a / b)

def checkedModNat (a b : Nat) : RouteResult Nat :=
  if b = 0 then Except.error (ThrowCode.ofNat 0x12)
  else Except.ok (a % b)

/-- Out-of-bounds list index → Panic `0x32` (EVM array OOB). -/
def checkedListGet {α : Type} (xs : List α) (i : Nat) : RouteResult α :=
  if h : i < xs.length then
    Except.ok (xs.get ⟨i, h⟩)
  else
    Except.error (ThrowCode.ofNat 0x32)

/-- Ceiling division by zero → Panic `0x12`. -/
def checkedDivc {n : Nat} (x y : BitVec n) : RouteResult (BitVec n) :=
  if y = 0#n then Except.error (ThrowCode.ofNat 0x12)
  else Except.ok ((x + y - 1#n) / y)

def checkedDivr {n : Nat} (x y : BitVec n) : RouteResult (BitVec n) :=
  if y = 0#n then Except.error (ThrowCode.ofNat 0x12)
  else Except.ok ((x + y / 2#n) / y)

def checkedDivcNat (x y : Nat) : RouteResult Nat :=
  if y = 0 then Except.error (ThrowCode.ofNat 0x12)
  else Except.ok ((x + y - 1) / y)

def checkedDivrNat (x y : Nat) : RouteResult Nat :=
  if y = 0 then Except.error (ThrowCode.ofNat 0x12)
  else Except.ok ((x + y / 2) / y)

def checkedDivmod {n : Nat} (a b : BitVec n) : RouteResult (BitVec n × BitVec n) :=
  if b = 0#n then Except.error (ThrowCode.ofNat 0x12)
  else Except.ok (a / b, a % b)

def checkedDivmodNat (a b : Nat) : RouteResult (Nat × Nat) :=
  if b = 0 then Except.error (ThrowCode.ofNat 0x12)
  else Except.ok (a / b, a % b)

def checkedMuldiv {n : Nat} (x y z : BitVec n) : RouteResult (BitVec n) :=
  if z = 0#n then Except.error (ThrowCode.ofNat 0x12)
  else Except.ok ((x * y) / z)

def checkedMuldivNat (x y z : Nat) : RouteResult Nat :=
  if z = 0 then Except.error (ThrowCode.ofNat 0x12)
  else Except.ok ((x * y) / z)

/-- Narrowing bit-width cast: out of range → Panic `0x11`. Widening stays `castWidth`. -/
def checkedCastWidth (n : Nat) {m : Nat} (x : BitVec m) : RouteResult (BitVec n) :=
  if n ≥ m then
    Except.ok (BitVec.ofNat n x.toNat)
  else if x.toNat ≥ 2 ^ n then
    Except.error (ThrowCode.ofNat 0x11)
  else
    Except.ok (BitVec.ofNat n x.toNat)

/-! ## Built-in helpers (P1 + `std::`)

`divmod` and the phase-1 `std::math` / `std::str` helpers. Keep in
lockstep with Lean expr lowering (`codegen/lean/expr.rs` —
`prelude_helper` / `gen_namespaced_call`).
-/

def parseUintDigits (s : String) : Nat :=
  s.foldl (fun acc c =>
    if '0' ≤ c ∧ c ≤ '9' then acc * 10 + (c.toNat - 48) else acc) 0

def charRadixDigit (c : Char) (base : Nat) : Option Nat :=
  if '0' ≤ c ∧ c ≤ '9' then
    let d := c.toNat - 48
    if d < base then some d else none
  else if base > 10 ∧ 'a' ≤ c ∧ c ≤ 'z' then
    let d := c.toNat - 87
    if d < base then some d else none
  else if base > 10 ∧ 'A' ≤ c ∧ c ≤ 'Z' then
    let d := c.toNat - 55
    if d < base then some d else none
  else none

def parseRadixNatCore? (chars : List Char) (base : Nat) : Option Nat :=
  match chars with
  | [] => some 0
  | c :: rest =>
    match charRadixDigit c base with
    | none => none
    | some d =>
      match parseRadixNatCore? rest base with
      | none => none
      | some n => some (n * base + d)

def parseRadixNat? (s : String) (radix maxBits : Nat) : Option Nat :=
  let cs := s.toList
  let (rest, base) := match cs with
    | '0' :: 'x' :: r | '0' :: 'X' :: r =>
        if radix = 0 ∨ radix = 16 then (r, 16) else (cs, if radix = 0 then 10 else radix)
    | _ => (cs, if radix = 0 then 10 else radix)
  if base < 2 ∨ base > 36 then none
  else if rest.isEmpty then none
  else
    match parseRadixNatCore? rest base with
    | none => none
    | some n =>
      let maxV := if maxBits ≥ 256 then (Nat.pow 2 256 - 1) else (Nat.pow 2 maxBits - 1)
      if n ≤ maxV then some n else none

def parseRadixSignedNat? (s : String) (radix maxBits : Nat) : Option Int :=
  let cs := s.toList
  let (neg, rest) := match cs with
    | '-' :: r => (true, r)
    | _ => (false, cs)
  match parseRadixNat? (String.ofList rest) radix maxBits with
  | none => none
  | some n =>
    let v : Int := if neg then -Int.ofNat n else Int.ofNat n
    let maxPos : Nat :=
      if maxBits ≥ 256 then (Nat.pow 2 255 - 1) else (Nat.pow 2 (maxBits - 1) - 1)
    let minNeg : Int := -(Int.ofNat maxPos + 1)
    let maxPosI : Int := Int.ofNat maxPos
    if minNeg ≤ v ∧ v ≤ maxPosI then some v else none

def parseUintNat? (s : String) (base : Nat) : Option Nat :=
  parseRadixNat? s base 64

def parseUint {n : Nat} (s : String) (base : BitVec n) : BitVec n :=
  match parseUintNat? s base.toNat with
  | some v => BitVec.ofNat n v
  | none => 0#n

def signedNatToBitVec (width : Nat) (i : Int) : BitVec width :=
  BitVec.ofNat width (Int.toNat (i % (Nat.pow 2 width : Int)))

def formatPadNat (width n : Nat) : String :=
  let s := toString n
  if s.length ≥ width then s
  else String.ofList (List.replicate (width - s.length) '0') ++ s

def formatNat (fmt : String) (a : Nat) : String :=
  if fmt = "{}" then toString a
  else if fmt = "{:06}" then formatPadNat 6 a
  else fmt ++ toString a

def format {n : Nat} (fmt : String) (a : BitVec n) : String :=
  formatNat fmt a.toNat

def powBits {n : Nat} : BitVec n → Nat → BitVec n
  | base, 0 => 1#n
  | base, exp + 1 => base * powBits base exp

def powNat (base exp : Nat) : Nat :=
  Nat.pow base exp

def pow {n : Nat} (base : BitVec n) (exp : BitVec n) : BitVec n :=
  powBits base exp.toNat

def absNat (x : Nat) : Nat := x

/-- Absolute value on `Int` (`lean.numerics: nat` signed carrier). -/
def absInt (x : Int) : Int :=
  if x < 0 then -x else x

/-- Signed absolute value on `BitVec` (two's complement via `slt`). -/
def abs {n : Nat} (x : BitVec n) : BitVec n :=
  if x.slt 0#n then (-x) else x

/-- Unsigned absolute-value helper (no-op; kept for unsigned `std::math::abs`). -/
def absU {n : Nat} (x : BitVec n) : BitVec n := x

def clampNat (x lo hi : Nat) : Nat :=
  if x < lo then lo else if x > hi then hi else x

def clampInt (x lo hi : Int) : Int :=
  if x < lo then lo else if x > hi then hi else x

def clamp {n : Nat} (x lo hi : BitVec n) : BitVec n :=
  if x < lo then lo else if x > hi then hi else x

/-- Signed clamp using `slt` / `sgt`. -/
def clampS {n : Nat} (x lo hi : BitVec n) : BitVec n :=
  if x.slt lo then lo else if hi.slt x then hi else x

def muldivNat (x y z : Nat) : Nat :=
  (x * y) / z

def muldiv {n : Nat} (x y z : BitVec n) : BitVec n :=
  (x * y) / z

def signNat (x : Nat) : Nat :=
  if x = 0 then 0 else 1

def signInt (x : Int) : Int :=
  if x = 0 then 0 else if x < 0 then -1 else 1

/-- Signed signum on `BitVec` (two's complement via `slt`). -/
def sign {n : Nat} (x : BitVec n) : BitVec n :=
  if x = 0#n then 0#n else if x.slt 0#n then (-1#n) else 1#n

/-- Unsigned signum (never negative). -/
def signU {n : Nat} (x : BitVec n) : BitVec n :=
  if x = 0#n then 0#n else 1#n

def divmodNat (a b : Nat) : Nat × Nat :=
  (a / b, a % b)

/-- `divmod a b = (a / b, a % b)`. Mirrors Cambrian's `divmod` builtin. -/
def divmod {n : Nat} (a b : BitVec n) : BitVec n × BitVec n :=
  (a / b, a % b)

def divcNat (x y : Nat) : Nat :=
  (x + y - 1) / y

def divc {n : Nat} (x y : BitVec n) : BitVec n :=
  (x + y - 1#n) / y

def divrNat (x y : Nat) : Nat :=
  (x + y / 2) / y

def divr {n : Nat} (x y : BitVec n) : BitVec n :=
  (x + y / 2#n) / y

def minmaxNat (a b : Nat) : Nat × Nat :=
  if a < b then (a, b) else (b, a)

def minmax {n : Nat} (a b : BitVec n) : BitVec n × BitVec n :=
  if a < b then (a, b) else (b, a)

def modpow2Nat (x k : Nat) : Nat :=
  if k = 0 then 0 else x % Nat.pow 2 k

def modpow2 {n : Nat} (x k : BitVec n) : BitVec n :=
  let m := k.toNat
  if m = 0 then 0#n
  else if m ≥ n then x
  else x &&& ((1#n <<< m) - 1#n)

/-! ## `Bytes` carrier for Cambrian `CamData` / `bytes`

We lower `CamData` / `bytes` to `List (BitVec 8)` rather than the stdlib
`ByteArray` because `ByteArray` lacks `Repr`, `BEq`, `DecidableEq`, and
`Inhabited` instances in Lean `v4.29.1` (the toolchain the transpiler
pins). `List (BitVec 8)` gets all four via `deriving`. -/

abbrev Bytes : Type := List (BitVec 8)

namespace Bytes
  def empty : Bytes := []
end Bytes

/-- Bit-width cast helper. Unsigned reinterpretation: equivalent to
`BitVec.zeroExtend n x` when widening and `BitVec.truncate n x` when
narrowing. The Lean 4 stdlib doesn't expose a single direction-
agnostic helper across versions, so we use the round-trip through
`Nat` to stay portable. -/
def castWidth (n : Nat) {m : Nat} (x : BitVec m) : BitVec n :=
  BitVec.ofNat n x.toNat

/-! ## Collection carriers (P4b)

List-backed maps avoid a heavy `Finmap` / Mathlib dependency in the
vendored prelude while keeping decidable lookup via linear search. -/

abbrev AddressMap (K V : Type) [DecidableEq K] := List (K × V)

namespace AddressMap
  def empty {K V : Type} [DecidableEq K] : AddressMap K V := []

  /-- EVM mapping read: a key that was never written reads as the value
  type's zero (`Inhabited.default`), exactly like a Solidity `mapping`.
  Returns `Option` (always `some`) so the generated `lookup … |>.get!`
  access is *total* — under concrete execution an absent key yields the
  zero value instead of an `Option.get!` panic, and in proofs `… |>.get!`
  still reduces to that same default, so nothing downstream changes. Use
  `contains` / `keyExists` for a truthful presence test. -/
  def lookup {K V : Type} [DecidableEq K] [Inhabited V] (m : AddressMap K V) (k : K) : Option V :=
    some (((m.find? fun p => p.1 = k).map Prod.snd).getD default)

  def insert {K V : Type} [DecidableEq K] (m : AddressMap K V) (k : K) (v : V) : AddressMap K V :=
    (k, v) :: m.filter fun p => p.1 ≠ k

  def update {K V : Type} [DecidableEq K] (m : AddressMap K V) (k : K) (v : V) : AddressMap K V :=
    insert m k v

  def remove {K V : Type} [DecidableEq K] (m : AddressMap K V) (k : K) : AddressMap K V :=
    m.filter fun p => p.1 ≠ k

  /-- Truthful key-presence probe (independent of `lookup`, which now always
  returns `some` under EVM-zero semantics). -/
  def contains {K V : Type} [DecidableEq K] (m : AddressMap K V) (k : K) : Bool :=
    (m.find? fun p => p.1 = k).isSome

  /-- Alias for Cambrian `m.exists(k)` (Lean reserves `exists`). -/
  def keyExists {K V : Type} [DecidableEq K] (m : AddressMap K V) (k : K) : Bool :=
    contains m k

  def length {K V : Type} [DecidableEq K] (m : AddressMap K V) : Nat :=
    List.length m

  def isEmpty {K V : Type} [DecidableEq K] (m : AddressMap K V) : Bool :=
    List.isEmpty m

  /-- Project keys from pair-list (may contain duplicates). -/
  def keys {K V : Type} [DecidableEq K] (m : AddressMap K V) : List K :=
    m.map Prod.fst

  def values {K V : Type} [DecidableEq K] (m : AddressMap K V) : List V :=
    m.map Prod.snd

  /-- Append `k` to a ghost key index when absent. -/
  def pushKeyIfNew {K : Type} [DecidableEq K] (ks : List K) (k : K) : List K :=
    if ks.contains k then ks else ks ++ [k]

  /-- Remove one occurrence of `k` from the ghost key index. -/
  def removeKey {K : Type} [DecidableEq K] (ks : List K) (k : K) : List K :=
    ks.filter fun x => x ≠ k
end AddressMap

end Cambrian
