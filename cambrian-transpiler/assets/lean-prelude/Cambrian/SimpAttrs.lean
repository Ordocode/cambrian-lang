-- Copyright (C) 2025-2026 The Cambrian Authors
-- SPDX-License-Identifier: GPL-3.0-only

import Lean.Meta.Tactic.Simp

/-
Cambrian.SimpAttrs — hermetically vendored from `cambrian-transpiler`.

Registers the Cambrian-curated `simp` sets. Kept in a *separate* module
from `Cambrian.Core` because a `simp` attribute registered with
`register_simp_attr` cannot be *applied* in the same module that registers
it — the extension is only visible to importing modules. Core (and the
Prelude facade) imports this file so generated modules can both
register-tag and apply the sets.
-/

/-- Codegen-populated simp set: the `<E>.Routes.<route>` assemblers.
`simp [cambrian_route_simp]` unfolds route bodies so the generic
`Except.isOk` plumbing lemmas in the Prelude can reflect a route call into
the Boolean combination of its `_pre_<i>` guards (kept opaque). -/
register_simp_attr cambrian_route_simp

/-- Codegen-populated simp set: the `<route>_pre_<i>` guard predicates
(the Cambrian "error conditions"). Add it to a `simp` call to further
unfold the guards into their underlying arithmetic conditions. -/
register_simp_attr cambrian_pre_simp

/-- Codegen-populated simp set: the `<E>.Members.M_<member>.<route>`
per-member transform functions. `simp [cambrian_member_simp]` unfolds a
member's post-route value into its defining arithmetic so `omega` /
`decide` can finish a "step preserves invariant" goal. -/
register_simp_attr cambrian_member_simp

/-- Prelude-populated simp set: `BitVec` ↔ `Nat` canonicalization lemmas
(tagged in `Cambrian.Core`). Pushes `BitVec` arithmetic/relations down
to `Nat` so `omega` works uniformly regardless of bit width. -/
register_simp_attr cambrian_bitvec_simp

/-- Prelude-populated simp set: reduces the monadic `Except` operations a
route assembler leaves behind (`pure` / `throw` / `>>=` / `<$>`) into raw
`Except.ok` / `Except.error` constructors, so a `match route … with …`
(the shape `test` / `invariant` theorems use) iota-reduces. Kept *out* of
the global `simp` set so it never competes with the `isOk` reflection
lemmas (which need `throw e >>= f` to stay un-reduced). -/
register_simp_attr cambrian_except_simp
