# SMAFD canonical Cambrian project (RehearsalToken)

**Migration guide:** [`docs/audit/SMAFD_INSTANTIATES_MIGRATION.md`](../../../../docs/audit/SMAFD_INSTANTIATES_MIGRATION.md) — copy parametric fuzz / integration patterns from here, not empty `property () for` shells.

**Latest handoff (T37):** [`docs/audit/HANDOFF_CAMBRIAN_T37_HARNESS_GREEN_RESPONSE.md`](../../../../docs/audit/HANDOFF_CAMBRIAN_T37_HARNESS_GREEN_RESPONSE.md)

Reference layout for the SMAFD dual-target workflow: one entity implementation,
a **catalog** of `property` / `invariant` specs, and supplemental files — all
listed in `project.yaml` sources (no parity clones, no duplicate Foundry-only
`.cam` files). **Fuzz / invariant overlays** use **`#[instantiates("STEM")]`**
with delta-only bodies; **integration tests** (T37) use standalone `test` +
`// CTI: PROP-…` (not full bodies under `#[instantiates]`).

## Directory layout

```
smafd_canonical/
  cambrian-contracts/
    RehearsalToken.cam              # entity implementation
    RehearsalToken.test.cam         # unit tests
    RehearsalToken.integration.test.cam   # standalone integration tests (CTI comment)
    RehearsalToken.fuzz.cam         # supplemental property delta (CP-001)
    RehearsalToken.invariant.cam    # #[instantiates] INV-RHT-001 + init delta
    RehearsalToken.invariant_ctor_mint.cam
  docs/cambrian-spec/
    RehearsalToken.properties.cam   # catalog: properties + INV-RHT-001
  project.evm.yaml                  # Foundry leg (deterministic_addresses: true)
  project.lean.yaml                 # Lean leg
```

## Required SMAFD elements (this fixture)

| Element | File | Notes |
| --- | --- | --- |
| Entity impl | `cambrian-contracts/RehearsalToken.cam` | Single source of truth |
| Property catalog | `docs/cambrian-spec/RehearsalToken.properties.cam` | `property` + `invariant`; included in `sources` |
| Unit tests | `RehearsalToken.test.cam` | Plain `test` blocks |
| Integration tests | `RehearsalToken.integration.test.cam` | **Standalone** `test` + `// CTI: PROP-…` (T37 — not `#[instantiates]` with full bodies) |
| Supplemental fuzz | `RehearsalToken.fuzz.cam` | `#[instantiates]` + `assume` delta only |
| Supplemental invariant | `RehearsalToken.invariant*.cam` | `invariant "CP-…" #[instantiates("INV-…")] for …` |
| EVM project | `project.evm.yaml` | All seven `.cam` files in `sources` |
| Lean project | `project.lean.yaml` | Same `sources` list |

## Canonical spec step order

Per `docs/LANGUAGE.md`, set the message context **before** `call constructor`:

```cam
msg { sender: deployer }
call constructor(deployer)
```

Do **not** rely on `call constructor` before `msg { sender }` (W11 anti-pattern).

## Reproduce (EVM / Foundry)

From this directory:

```bash
# release binary recommended
../../../../target/release/cambrian_transpiler --project project.evm.yaml
cd build
bash setup.sh    # installs forge-std if missing
forge test
```

Regression gate (no manual `forge test` required):

```bash
cargo test --release -p cambrian-transpiler --test test_smafd_canonical_fixture
# 5/5 PASS
```

`setUp` deploys via **`CambrianFactory.deploy*`** (U4-4; EVM det mandatory per U4-6).
Invariants in other corpora use handler-first harness (U4-4c: `Handler` + `cam_wire`).
Constructor args pinned in `setUp` are hoisted; per-test `call constructor()`
is a noop in generated Solidity.

Catalog `property` blocks lower to Foundry `test` / `testFuzz` in
`test/RehearsalToken.t.sol` when `RehearsalToken.properties.cam` is in
`project.yaml` `sources`.

Catalog `INV-RHT-001` is **superseded** by merged CP-* overlays (no third
invariant harness).

## Reproduce (Lean)

```bash
../../../../target/release/cambrian_transpiler --project project.lean.yaml
cd build-lean && lake build
```

Theorem slug snapshot: `cargo test -p cambrian-transpiler --test test_smafd_lean_canonical smafd_lean_canonical_theorem_slug_snapshot`

**37** theorem slugs in `RehearsalTokenSpec.lean` (includes **9** `integration_*`
from standalone integration tests).

## What we intentionally omit

- `*.parity.fuzz.cam` / duplicate catalog clones for gates only
- `// @covers` comments — replaced by `#[instantiates]` (CAM-H-02)
- Full `call` / `expect` bodies under `#[instantiates]` on `test` (T37 / W2-BC-10)

Catalog `property` / `invariant` bodies may reference entity `const` names
(`INITIAL_SUPPLY`, `ZERO`); the Foundry harness mirrors them as
`private constant` declarations at the top of each generated test contract.

## Further reading

- [`docs/audit/HANDOFF_CAMBRIAN_T37_HARNESS_GREEN_RESPONSE.md`](../../../../docs/audit/HANDOFF_CAMBRIAN_T37_HARNESS_GREEN_RESPONSE.md) — T37 closeout
- [`docs/audit/HANDOFF_CAMBRIAN_CAM-H02_RESPONSE.md`](../../../../docs/audit/HANDOFF_CAMBRIAN_CAM-H02_RESPONSE.md) — CAM-H-02 migration (historical counts superseded by T37)
- [`docs/LANGUAGE.md`](../../../../docs/LANGUAGE.md) — § Catalog links
