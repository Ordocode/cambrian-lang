-- Copyright (C) 2025-2026 The Cambrian Authors
-- SPDX-License-Identifier: GPL-3.0-only

/-
  Regression proofs for the Lean "error-condition reflection" layer.

  Copied into the project generated from `tests/fixtures/lean_reflection.cam`
  and checked by `lean_reflection_proofs_build` (gated on
  `CAMBRIAN_TEST_LEAN_BUILD=1`).

  These pin the behaviour of the two codegen-populated simp sets:

    * `cambrian_route_simp` — the route assemblers (World wrappers and
      Local transitions / per-phase helpers);
    * `cambrian_pre_simp`   — the `_pre_<i>` guard predicates lowered
      from `where` / `from` / `throw` (emitted under `<E>.Local`).

  plus the generic `Except.isOk` plumbing lemmas in the Prelude
  (`Cambrian.RouteResult`). Together they reflect `(route …).isOk`
  into the Boolean combination of the route's guards.
-/

import Cambrian.Prelude
import Cambrian.Generated.Bank
import Cambrian.Generated.Gate
import Cambrian.Generated.Vault
import Cambrian.Generated.World
import Cambrian.Generated.BankRoutes
import Cambrian.Generated.GateRoutes
import Cambrian.Generated.VaultRoutes

open Cambrian

-- Unphased, single guard: success reflects to the lone `_pre_0`.
example (w : Generated.World) (inst : Bank.Identity) (ctx : MsgCtx) (amount : BitVec 128) :
    (Bank.Routes.deposit w inst ctx amount).isOk
      = Bank.Local.deposit_pre_0 (w.storage.bank inst) ctx inst amount := by
  simp [cambrian_route_simp]

-- Unphased, chained guards: success reflects to their conjunction.
example (w : Generated.World) (inst : Bank.Identity) (ctx : MsgCtx) (amount : BitVec 128) :
    (Bank.Routes.withdraw w inst ctx amount).isOk
      = (Bank.Local.withdraw_pre_0 (w.storage.bank inst) ctx inst amount
          && Bank.Local.withdraw_pre_1 (w.storage.bank inst) ctx inst amount) := by
  simp [cambrian_route_simp]

-- Unphased concrete revert, fully decided down to the arithmetic guard.
example (w : Generated.World) (inst : Bank.Identity) (ctx : MsgCtx) :
    (Bank.Routes.deposit w inst ctx 0).isOk = false := by
  simp [cambrian_route_simp, cambrian_pre_simp]

-- Phased: `simp [cambrian_route_simp]` unfolds the entry AND the per-phase
-- helpers; the flat conjunction closes with one case-split (the `check`
-- phase preserves the state the `apply` guard reads).
example (w : Generated.World) (inst : Vault.Identity) (ctx : MsgCtx) (amount : BitVec 128) :
    (Vault.Routes.process w inst ctx amount).isOk
      = (Vault.Local.process_check_pre_0 (w.storage.vault inst) ctx inst amount
          && Vault.Local.process_apply_pre_0 (w.storage.vault inst) ctx inst amount) := by
  simp [cambrian_route_simp]
  cases Vault.Local.process_check_pre_0 (w.storage.vault inst) ctx inst amount <;> rfl

-- Phased concrete revert across phases, fully decided.
example (w : Generated.World) (inst : Vault.Identity) (ctx : MsgCtx) :
    (Vault.Routes.process w inst ctx 0).isOk = false := by
  simp [cambrian_route_simp, cambrian_pre_simp]

/-
  Design B — per-route `Pre` + `<route>_isOk_iff`.

  Flat routes (`Bank`, `Gate`) get a guard-level decidable `Pre`; the
  phased `Vault` gets the abstract `Pre := isOk = true` fallback. The
  generated `<route>_isOk_iff` theorems are codegen-proved; here we pin
  that they are usable downstream.
-/

-- `Pre` characterizes success: `isOk_iff` rewrites `isOk` to `Pre`.
example (w : Generated.World) (inst : Bank.Identity) (ctx : MsgCtx) (amount : BitVec 128)
    (h : Bank.Routes.deposit.Pre w inst ctx amount) :
    (Bank.Routes.deposit w inst ctx amount).isOk = true :=
  (Bank.Routes.deposit_isOk_iff w inst ctx amount).mpr h

-- The from-check + where guard route exposes both conjuncts of `Pre`.
example (w : Generated.World) (inst : Gate.Identity) (ctx : MsgCtx) (amount : BitVec 128)
    (hsender : ctx.sender = (w.storage.gate inst).m_owner)
    (hamt : Gate.Local.bump_pre_0 (w.storage.gate inst) ctx inst amount = true) :
    (Gate.Routes.bump w inst ctx amount).isOk = true :=
  (Gate.Routes.bump_isOk_iff w inst ctx amount).mpr ⟨hsender, hamt⟩

-- `Pre` is decidable: a concrete instance evaluates (deposit's guard is
-- `amount > 0`, independent of the world state).
example (w : Generated.World) (inst : Bank.Identity) (ctx : MsgCtx) :
    Bank.Routes.deposit.Pre w inst ctx 5 := by
  simp [Bank.Routes.deposit.Pre, Bank.Local.deposit_pre_0]

-- Abstract `Pre` (phased): `isOk_iff` is definitional, and the
-- `cambrian_route_simp` set still expands it into the phase guards.
example (w : Generated.World) (inst : Vault.Identity) (ctx : MsgCtx) :
    ¬ Vault.Routes.process.Pre w inst ctx 0 := by
  simp [Vault.Routes.process.Pre, cambrian_route_simp, cambrian_pre_simp]
