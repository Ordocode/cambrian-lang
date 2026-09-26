-- Copyright (C) 2025-2026 The Cambrian Authors
-- SPDX-License-Identifier: GPL-3.0-only

-- Splice fragment (NOT a standalone module): the transpiler inlines this text
-- between the `-- CAMBRIAN:INTRINSICS:BEGIN` / `-- CAMBRIAN:INTRINSICS:END`
-- markers of `Cambrian/Evm.lean` when it emits the plausible-testing
-- prelude variant. It runs inside the Evm module's `namespace Cambrian`, so it
-- declares no namespace/imports of its own. Keep every name, arity, and result
-- type in lockstep with the opaque block it replaces.
/-! ## EVM intrinsics — executable reference models (plausible-testing variant)

The goal here is *executability*, not fidelity: every function is total,
deterministic, and computable, so property / invariant checks that must
evaluate a hash reduce to a concrete value instead of getting stuck on an
`opaque` symbol (which makes Plausible "give up").

These are deliberately NOT cryptographic and NOT EVM-accurate:

  * hashing is an FNV-1a-style mixing fold over 256-bit words — deterministic
    (equal inputs ⇒ equal outputs) and practically collision-free on the small
    domains a fuzzer explores, but trivially invertible and not Keccak;
  * `ecrecover` derives an address from the signature words (no secp256k1
    recovery), so a given `(digest, v, r, s)` always maps to the same signer;
  * `create2Address` mixes deployer / salt / initCodeHash into an address
    (no real CREATE2 / Keccak), distinct triples yielding distinct addresses in
    practice.

Every definition is `@[irreducible]` so `simp` / proof ladders in generated
`Spec` files do not unfold it (proofs stay stable), while compiled evaluation —
which is what Plausible actually runs — still executes it. The opaque variant's
`create2Address_injective` axiom is intentionally absent here: it would be
unsound over a concrete, non-injective function. -/

/-- One FNV-1a mixing step over a 256-bit accumulator. Wraps modulo 2^256. -/
def hashStep (h x : BitVec 256) : BitVec 256 :=
  (h ^^^ x) * 0x100000001b3#256

/-- Fold a list of 256-bit words into a single digest (FNV-1a 64-bit offset
basis, widened). Deterministic and total. -/
def hashWordsCore (xs : List (BitVec 256)) : BitVec 256 :=
  xs.foldl hashStep 0xcbf29ce484222325#256

/-- Types that can feed the deterministic hash. `toHashWords` flattens a value
into 256-bit words; the polymorphic `hashOf` / `keccak256Packed` families use
it so their call sites stay pre-encoding-free (matching the opaque models). -/
class CamHashable (α : Type) where
  toHashWords : α → List (BitVec 256)

/-- `Nat` (the `numerics: nat` scalar carrier) → one word. -/
instance : CamHashable Nat where
  toHashWords n := [BitVec.ofNat 256 n]

/-- Any fixed-width word (`Address = BitVec 160`, `U256 = BitVec 256`,
`pubkey`, `BitVec 8`, …) → one 256-bit word. -/
instance {n : Nat} : CamHashable (BitVec n) where
  toHashWords x := [BitVec.ofNat 256 x.toNat]

/-- `bool` → a single 0/1 word. -/
instance : CamHashable Bool where
  toHashWords b := [if b then 1#256 else 0#256]

/-- `Bytes = List (BitVec 8)` → one word per byte. -/
instance : CamHashable Bytes where
  toHashWords bs := bs.map (fun b => BitVec.ofNat 256 b.toNat)

/-- `string` → one word per UTF-8 byte, which is what `abi.encodePacked`
does with a Solidity `string` before hashing. EIP-712 domain separators
hash the token name and version this way, so a `string` reaching a hash
is the common case rather than an exotic one. -/
instance : CamHashable String where
  toHashWords s := s.toUTF8.toList.map (fun b => BitVec.ofNat 256 b.toNat)

/-- `evm::keccak256(bytes)`. Deterministic 256-bit digest of the byte list. -/
@[irreducible] def keccak256 (bs : ByteArray) : U256 :=
  hashWordsCore (bs.toList.map (fun b => BitVec.ofNat 256 b.toNat))

/-- `evm::keccak256Packed` family. Concatenates the per-argument word encodings
before hashing (mirrors `keccak256(abi.encodePacked(...))`). -/
@[irreducible] def keccak256Packed1 {α : Type} [CamHashable α]
    (a : α) : U256 :=
  hashWordsCore (CamHashable.toHashWords a)
@[irreducible] def keccak256Packed2 {α β : Type} [CamHashable α] [CamHashable β]
    (a : α) (b : β) : U256 :=
  hashWordsCore (CamHashable.toHashWords a ++ CamHashable.toHashWords b)
@[irreducible] def keccak256Packed3 {α β γ : Type}
    [CamHashable α] [CamHashable β] [CamHashable γ]
    (a : α) (b : β) (c : γ) : U256 :=
  hashWordsCore (CamHashable.toHashWords a ++ CamHashable.toHashWords b
    ++ CamHashable.toHashWords c)
@[irreducible] def keccak256Packed4 {α β γ δ : Type}
    [CamHashable α] [CamHashable β] [CamHashable γ] [CamHashable δ]
    (a : α) (b : β) (c : γ) (d : δ) : U256 :=
  hashWordsCore (CamHashable.toHashWords a ++ CamHashable.toHashWords b
    ++ CamHashable.toHashWords c ++ CamHashable.toHashWords d)

/-- `evm::ecrecover(digest, v, r, s)`. Deterministic (non-secp256k1) signer:
derives a 160-bit address from the digest and signature words. -/
@[irreducible] def ecrecover (digest : U256) (v : BitVec 8) (r s : U256) : Address :=
  BitVec.ofNat 160 (hashWordsCore [digest, BitVec.ofNat 256 v.toNat, r, s]).toNat

/-- Polymorphic `hashOf` family. Cambrian's `hashOf(a₁, …, aₙ)` lowers to
`keccak256(abi.encode(a₁, …, aₙ))` on EVM; here it is the deterministic word
hash of the concatenated argument encodings. -/
@[irreducible] def hashOf2 {α β : Type} [CamHashable α] [CamHashable β]
    (a : α) (b : β) : U256 :=
  hashWordsCore (CamHashable.toHashWords a ++ CamHashable.toHashWords b)
@[irreducible] def hashOf3 {α β γ : Type}
    [CamHashable α] [CamHashable β] [CamHashable γ]
    (a : α) (b : β) (c : γ) : U256 :=
  hashWordsCore (CamHashable.toHashWords a ++ CamHashable.toHashWords b
    ++ CamHashable.toHashWords c)
@[irreducible] def hashOf4 {α β γ δ : Type}
    [CamHashable α] [CamHashable β] [CamHashable γ] [CamHashable δ]
    (a : α) (b : β) (c : γ) (d : δ) : U256 :=
  hashWordsCore (CamHashable.toHashWords a ++ CamHashable.toHashWords b
    ++ CamHashable.toHashWords c ++ CamHashable.toHashWords d)
@[irreducible] def hashOf5 {α β γ δ ε : Type}
    [CamHashable α] [CamHashable β] [CamHashable γ] [CamHashable δ] [CamHashable ε]
    (a : α) (b : β) (c : γ) (d : δ) (e : ε) : U256 :=
  hashWordsCore (CamHashable.toHashWords a ++ CamHashable.toHashWords b
    ++ CamHashable.toHashWords c ++ CamHashable.toHashWords d
    ++ CamHashable.toHashWords e)
@[irreducible] def hashOf6 {α β γ δ ε ζ : Type}
    [CamHashable α] [CamHashable β] [CamHashable γ] [CamHashable δ]
    [CamHashable ε] [CamHashable ζ]
    (a : α) (b : β) (c : γ) (d : δ) (e : ε) (f : ζ) : U256 :=
  hashWordsCore (CamHashable.toHashWords a ++ CamHashable.toHashWords b
    ++ CamHashable.toHashWords c ++ CamHashable.toHashWords d
    ++ CamHashable.toHashWords e ++ CamHashable.toHashWords f)

/-- Deterministic `CREATE2` address. Mixes deployer / salt / initCodeHash into a
160-bit address; NOT a real Keccak-based CREATE2, but distinct triples yield
distinct addresses in practice. No injectivity axiom (see header). -/
@[irreducible] def create2Address (deployer : Address) (salt : BitVec 256)
    (initCodeHash : BitVec 256) : Address :=
  BitVec.ofNat 160
    (hashWordsCore [BitVec.ofNat 256 deployer.toNat, salt, initCodeHash]).toNat
