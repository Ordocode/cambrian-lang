# Cambrian

**Cambrian** is an AI-first language for stateful, message-based smart
contracts with formal verification in mind. A single `.cam` source
transpiles to:

| Target | Flag | Output |
|--------|------|--------|
| **EVM** | `--target evm` | Flat Solidity (`^0.8.24`) + Foundry tests |
| **Lean 4** | `--target lean` | Lean 4 + Lake project (theorem *statements*) |

The language is member-centric: each state variable owns its transformation
logic, with temporal operators (`^x`), pure functions, message-based routing,
and a built-in testing framework (`test` / `fuzz` / `invariant` / `property`)
that lowers to Foundry tests on EVM and to Lean theorems on `--target lean`.

## Documentation

Language documentation: [https://cambrian-lang.dev/](https://cambrian-lang.dev/).
The CLI and `project.yaml` layout are described in
[cambrian-transpiler/README.md](cambrian-transpiler/README.md).

## Quick start

```bash
cargo build --release -p cambrian-transpiler

# EVM: flat Solidity + Foundry harness
./target/release/cambrian-transpiler contracts/counter.cam -o build/out-evm --target evm

# Lean 4: Lake project for formal verification
./target/release/cambrian-transpiler contracts/counter.cam -o build/out-lean --target lean
./target/release/cambrian-transpiler contracts/counter.cam -o build/out-lean --target lean --check-lean
cd build/out-lean && lake build

cargo test -p cambrian-transpiler
```

Multi-file programs (several entities, `test` / `fuzz` / `invariant`
companions) use a `project.yaml` — see the examples below and
[cambrian-transpiler/README.md](cambrian-transpiler/README.md#project-yaml-reference-excerpt).
Default `--target` in this tree is `evm`.

## Project structure

```
cambrian-lang/
├── Cargo.toml                    # Workspace: cambrian-transpiler + cambrian-core
├── cambrian-transpiler/          # Parser → AST → validation → EVM / Lean codegen
├── cambrian-core/                # Shared language types (U256, checked arithmetic)
├── contracts/                    # Example .cam contracts
├── examples/                     # governor, uniswap-v2 (EVM + optional Lean YAML)
├── stdlib/                       # Component library: ERC-20/EIP-2612, ERC-7943 (uRWA),
│                                 # ERC-4626 vaults, integer math — with test/fuzz/invariant
│                                 # suites and an ABI conformance gate (scripts/abi-check.py)
└── docs/
    ├── LANGUAGE.md
    ├── STDLIB.md
    ├── TESTING_TARGETS.md
    └── EVM_GAPS.md
```

## Architecture

### EVM — flat Solidity

Each Cambrian entity becomes a single self-contained Solidity contract
(`^0.8.24`). Tests in `.cam` lower to Foundry (`test/` + `foundry.toml`).
Deployment lowers to `CREATE2` and an auto-generated `CambrianFactory`.

### Lean 4 — formal verification

Produces a per-program Lake project that preserves Cambrian's per-member
transform decomposition as individually addressable `def`s. `test` / `fuzz` /
`invariant` declarations lower to Lean **theorem statements**; main theorems
ship with `sorry` proof bodies (this repo does not prove user properties).
The generated layout and the `sorry` policy are described in
[docs/LANGUAGE.md](docs/LANGUAGE.md) (Codegen Mapping Summary and
Lean `sorry` policy sections).

## Examples

| Example | Description |
|---------|-------------|
| [examples/governor](examples/governor) | Compound-style Governor + ERC20Votes + TimelockController (Foundry) |
| [examples/uniswap-v2](examples/uniswap-v2) | UniswapV2 Factory / Pair / ERC20 ports (Foundry) |

Both also ship a `project.lean.yaml` for a Lean verification build.

## Testing

Cambrian ships a **built-in testing framework**: `test`, `fuzz`, `invariant`,
and `property` blocks in `.cam` lower to Foundry on EVM and to theorems on
Lean. See [docs/LANGUAGE.md](docs/LANGUAGE.md).

```bash
cargo test -p cambrian-transpiler
cargo test test_codegen_evm
cargo test test_codegen_lean
cargo test test_std
cargo test test_validate
```

Optional Lean compile gate (needs `lake` / `elan` on `PATH`):

```bash
CAMBRIAN_TEST_LEAN_BUILD=1 cargo test -p cambrian-transpiler --test test_std_lean --test test_std_str_matrix
```

## Contributing

We are not accepting code contributions (pull requests or patches) at this
time.

**Bug reports and GitHub issues are very welcome** — crashes, unexpected
behavior, documentation gaps, and feature requests. Please describe what
you observed and how to reproduce it; you do not need to send a fix.

## License

This project is licensed under the GNU General Public License v3.0 only.
See [LICENSE](LICENSE) for the full terms.

**Generated output.** Solidity, Lean, and other files emitted by the
transpiler are not licensed as GPL by default. They are tagged
`UNLICENSED` (or equivalent) so you choose the license for your contracts.
That includes injected helpers such as `_cam_muldiv` and the copied Lean
support modules (`Cambrian/*.lean`). The in-repo sources
of those modules stay GPL; the copy written into a generated project does
not. They do not relicense the rest of your output.
