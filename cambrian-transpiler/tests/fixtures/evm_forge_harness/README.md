# EVM Forge lowered-test harness regression corpus

Regression fixtures for Cambrian `*.test.cam` / `*.invariant.cam` lowering to
Foundry Solidity (`evm_test_codegen.rs`).

**Task / plan:** [`docs/TASK_EVM_FORGE_LOWERED_HARNESS.md`](../../../../docs/TASK_EVM_FORGE_LOWERED_HARNESS.md) · [`docs/plans/evm-forge-lowered-harness.md`](../../../../docs/plans/evm-forge-lowered-harness.md)

## Layout

```
evm_forge_harness/
  RehearsalToken/          # ERC20-like entity + unit/integration/invariant tests
    RehearsalToken.cam
    RehearsalToken.{test,integration.test,invariant}.cam
    project.deterministic.yaml
    project.non_deterministic.yaml
    build/                 # gitignored — transpiler output + forge-std (local)
```

## Rust regression gate

    cargo test -p cambrian-transpiler --test test_evm_forge_harness

Asserts: no `skipped: call constructor`; unit/fuzz `setUp` deploys via
`CambrianFactory.deploy*` (U4-4); invariants use handler-first harness
(U4-4c): `new Handler()` → `factory.deploy*(address(_handler), …)` →
`cam_wire` → `targetContract` — no `new Entity(…)` / `.initialize(…)` on
the SUT. See [`docs/plans/u4-4c-handler-first-invariant-harness.md`](../../../../docs/plans/u4-4c-handler-first-invariant-harness.md).

## Manual forge (optional)

    cd cambrian-transpiler/tests/fixtures/evm_forge_harness/RehearsalToken
    cargo build --release -p cambrian-transpiler
    rm -rf build
    ../../../../target/release/cambrian-transpiler --project project.deterministic.yaml
    cd build && forge install foundry-rs/forge-std --no-git && forge test

`project.non_deterministic.yaml` is retained for historical regression pins only;
EVM projects reject `deterministic_addresses: false` (**F6**, U4-6).
