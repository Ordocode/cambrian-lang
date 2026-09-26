-- Copyright (C) 2025-2026 The Cambrian Authors
-- SPDX-License-Identifier: GPL-3.0-only

import Cambrian.SimpAttrs
import Cambrian.Core
import Cambrian.Evm

/-
Cambrian.Prelude — thin facade over `Cambrian.Core` + `Cambrian.Evm`.

Hermetically vendored from `cambrian-transpiler`. The transpiler emits
this file as `Cambrian/Prelude.lean` in every generated Lean project so
existing `import Cambrian.Prelude` sites keep working. Declaration names
remain `Cambrian.*` (defined in Core / Evm); this module only re-exports
them via imports.

Some project settings append further helper definitions after these
imports; they live in the same `Cambrian.Prelude` module and stay visible
to `import Cambrian.Prelude`.

`std::math` / `std::str` Lean helpers live in `Cambrian.Core` (see the
Built-in helpers section there).
-/
