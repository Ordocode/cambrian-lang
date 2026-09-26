# Cambrian Transpiler

Transpiler for the Cambrian language. Two generation targets in this tree:

- **`--target evm`** — Solidity (`^0.8.24`) plus a Foundry test harness.
  Gap inventory: [docs/EVM_GAPS.md](../docs/EVM_GAPS.md).
- **`--target lean`** — Lean 4 + Lake project for formal verification
  (theorem statements; main proofs are `sorry` by policy). Layout and
  policy: [docs/LANGUAGE.md](../docs/LANGUAGE.md) (Codegen Mapping
  Summary and Lean `sorry` policy sections).

Default `--target` is `evm`.

## Quick start

```bash
cargo build --release -p cambrian-transpiler

./target/release/cambrian-transpiler input.cam -o output/ --target evm

./target/release/cambrian-transpiler input.cam -o build/lean --target lean
./target/release/cambrian-transpiler input.cam -o build/lean --target lean --check-lean
cd build/lean && lake build

./target/release/cambrian-transpiler --project project.yaml --check --diagnostic-format json
./target/release/cambrian-transpiler --version --json
```

## CLI flags

| Flag | Meaning |
|------|---------|
| `--help` / `-h` | Print usage and exit 0. |
| `--version` `[--json]` | Print compiler identity (`cambrian.build-identity/v1` when `--json`). |
| `--check` | Parse, validate, and run target-compat; do **not** codegen or write files. Does not require `-o`. Distinct from `--check-lean` (post-emit `lake build`) and `--dump-ast` (exits before validation). |
| `--diagnostic-format human\|json` | `human` (default) prints `error [CODE]: …` on stderr. `json` prints a single `cambrian.diagnostics/v1` envelope on stdout (exit code unchanged). |
| `--max-errors N` | Cap recovered parse diagnostics (default 32). |
| `--dump-ast` | Pretty-print the AST and exit (no validate/codegen). |
| `--check-lean` | After emitting a Lean project, run `lake build`. |
| `--project <yaml>` | Multi-file project. `--check` and `--diagnostic-format` apply. |
| `-o <dir>` | Output directory (ignored by `--check`). |
| `--target evm\|lean` | Backend. Default `evm`. |
| `--source-map` | Emit `.cam.map` JSON alongside generated code. |

## Architecture

```
.cam source
    │
    ▼
 Parser (LALRPOP)  → AST
    │
    ▼
 AST validation    → Diagnostics (V / E / L / T / I / F / W)
    │
    ▼
 Code generator
    ├── EvmSolidityBackend  → Entity.sol + Foundry tests
    └── LeanBackend         → Lake project (Prelude + Generated + Spec)
```

## Project structure

```
cambrian-transpiler/
├── Cargo.toml
├── build.rs                        # LALRPOP build script
├── src/
│   ├── main.rs                     # CLI
│   ├── ast.rs
│   ├── cambrian.lalrpop
│   ├── validate/                   # V1–V64, E01–E26, L1–L16, T1–T23, I1–I18, F1–F6, W1–W15
│   ├── target.rs                   # Target { domain, language } + capabilities
│   ├── project.rs                  # Multi-file project loader
│   └── codegen/
│       ├── mod.rs                  # Orchestration (EVM + Lean)
│       ├── solidity/               # Solidity / EVM emitter
│       ├── evm_test_codegen.rs     # Foundry harness
│       └── lean/                   # Lean 4 + Lake emitter
└── tests/                          # Integration tests
```

`.cam` fixtures live in the repo-root `contracts/` directory.

## Validation checks

Full per-code descriptions: [docs/LANGUAGE.md → Validation Rule Reference](../docs/LANGUAGE.md#validation-rule-reference).

| Family | Range | Scope |
|--------|-------|--------|
| **V** | V1 – V57 | Entity / route / member / phase / `extern entity` / library / `std::` |
| **E** | E01 – E26 | Cross-domain compatibility (TVM constructs rejected or warned on EVM) |
| **L** | L1 – L16 | Lean-target compatibility |
| **T** | T1 – T23 | `test` / `fuzz` / `property` |
| **I** | I1 – I18 | `invariant` (I17: EVM cannot lower this `check`; I18: multi-instance identity) |
| **F** | F1, F3–F6 | `project.yaml` packaging (**F6**: EVM det policy) |
| **W** | W1 – W15 | Lints |

Frequently hit codes: `V27` / `V28` (a `where` clause references a `var`
captured in the same or a later phase — move the check to a per-phase `where`
on a strictly later phase), `V54` (a route body mixes phase blocks and bare
actions; use fully phased or fully unphased form).

TVM-only constructs (`gosh::*`, `rescue`/`recover`, …) still have rule codes:
on `--target evm` / `--target lean` they warn or error (E01–E06, E26, …). They
are not emitters in this tree.

## EVM target

State-machine codegen, payload-bearing enums, tagged-union `match`,
deterministic addresses + `CREATE2`, `extern entity`, auto-`payable`,
`for` / `.fold`, EIP-712 / EIP-2612 intrinsics.

- **Gaps:** [docs/EVM_GAPS.md](../docs/EVM_GAPS.md)
- **Authoring:** EVM subsections of [docs/LANGUAGE.md](../docs/LANGUAGE.md)
- **Examples:** `examples/governor`, `examples/uniswap-v2` (EVM CREATE2 factory deploy)

## Lean target

- **Status:** single-entity translation, property theorems, abstract EVM
  world model, cross-entity sends, deploy, collections, `extern entity`,
  multi-entity invariants. Main theorems use `sorry` bodies.
- **Still rejected (`L5`–`L11`, plus E26):** `expect throw` against total
  routes (`L5`), `expect return` without `return_type` (`L6`), `skip from`
  not honoured (`L7` warning), failing self-call outside a fail surface
  (`L9`/`L11`), send-bearing invariant actions (`L10` warning).
  `rescue`/`recover` is not modelled (**E26**).
- **Layout** (under `<output_dir>/`): `lakefile.toml`, `lean-toolchain`,
  `Cambrian/Prelude.lean`, `Cambrian/Generated/<Entity>.lean`,
  `<Entity>Routes.lean`, `World.lean`, `<Entity>Spec.lean`, optional `Pure.lean`.

Route entry-points take `(w : World) (inst : <E>.Identity) (ctx : MsgCtx) (args…)`
and return `World × T` (or `RouteResult` when the route can fail).

## Project YAML reference (excerpt)

| Block / field | Purpose |
|---------------|---------|
| `target: "evm" \| "lean"` | Backend selection. |
| `sources: [<path>, ...]` | `.cam` files merged into one program. |
| `library_paths: [<path>, ...]` | Optional search roots for library-tier files (`${VAR}` expanded). No default root. |
| `imports: [<path>, ...]` | Optional library-tier `.cam` files (F4). Yaml dir first, then `library_paths` (F5 on miss). Does not namespace. |
| `output_dir: <path>` | Generated artifacts (default `build/`). |
| `evm.allow_constructor_payable: bool` | EVM ctor `payable` + factory CREATE2 `value` (default `true`). |
| `fuzz.{runs, seed, shrink, max_local_rejects}` | Foundry fuzz defaults. |
| `invariant.{runs, depth, fail_on_revert, seed}` | Foundry invariant defaults. |
| `lean.{numerics, proof_helpers}` | Lean numeric mode / proof-helper kill switch. |

See [docs/LANGUAGE.md](../docs/LANGUAGE.md) for `test` / `fuzz` / `invariant`
syntax. Tests in `.cam` lower to Foundry (`forge test`) on EVM.

## Tests

```bash
cargo test -p cambrian-transpiler
cargo test test_std
cargo test test_std_str_matrix
cargo test test_std_lean
cargo test test_codegen_evm
cargo test test_evm_forge_harness
cargo test test_codegen_lean
cargo test test_validate

CAMBRIAN_TEST_LEAN_BUILD=1 cargo test -p cambrian-transpiler --test test_std_lean --test test_std_str_matrix
```

Policy: [docs/TESTING_TARGETS.md](../docs/TESTING_TARGETS.md).
`std::` spec: [docs/STDLIB.md](../docs/STDLIB.md).

`test_evm_solc` and optional `lake build` smokes skip when `forge` / `lake`
are not on `PATH`.

## Dependencies

- **[LALRPOP](https://github.com/lalrpop/lalrpop)** — parser generator (build-time)
- **[sha2](https://crates.io/crates/sha2)** — hashing used by address / digest lowering
