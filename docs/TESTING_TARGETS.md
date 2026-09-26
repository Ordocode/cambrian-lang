# Testing targets

This tree ships **Solidity@ETH** (Foundry) and **Lean**. New language and
stdlib features should land conformance on both where the surface is
accepted.

**Shared corpus:** primary `.cam` files under `contracts/` are selected by
[`cambrian-transpiler/tests/corpus/mod.rs`](../cambrian-transpiler/tests/corpus/mod.rs).

| ID | Target | CLI | Runtime oracle | Compile gate |
|----|--------|-----|----------------|--------------|
| **T1** | **Solidity@ETH** | `--target evm` | `forge test` / `forge build` | transpile + solc via forge |
| **T3** | **Lean** | `--target lean` | `lake build` on generated projects | Lean codegen string gates |

Never call `solc` directly; only `forge test` / `forge build`.

### T1 — Solidity@ETH

- Semantic gates: `tests/fixtures/*_matrix.cam` + `tests/fixtures/forge/*.t.sol`
- Harnesses: `test_std_evm.rs`, `test_std_str_matrix.rs`, `test_std_parse.rs`
- Rule: **never** call `solc` directly

### T3 — Lean

- Gates: `test_std_lean.rs`, `test_std_str_matrix.rs` (`std_str_matrix_lean_lake`),
  `CAMBRIAN_TEST_LEAN_BUILD=1` lake jobs
- Shared fixtures: same `.cam` as T1 where Lean accepts the surface

## Cross-target fixture pattern

```
tests/fixtures/<feature>_matrix.cam
tests/fixtures/<feature>_matrix.yaml       # target: evm
tests/fixtures/<feature>_matrix_lean.yaml
tests/fixtures/forge/<Feature>Matrix.t.sol
tests/test_<feature>_matrix.rs             # T1 forge + T3 lake (gated)
```

## Related docs

- [LANGUAGE.md](LANGUAGE.md) — language reference
- [STDLIB.md](STDLIB.md) — `std::` specification
- [EVM_GAPS.md](EVM_GAPS.md) — EVM gap inventory
