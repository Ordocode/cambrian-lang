# SMAFD PL-F-INV-04 fixture (Run #9 cp_004)

Minimal TokenCore overlay from `playground/phase4-erc20-live-20260918-run9` with explicit
`deploy { 0xd01 }` on the ctor-mint supplemental invariant.

| File | Role |
| --- | --- |
| `TokenCore.cam` | Entity (burn route) |
| `TokenCore.properties.cam` | Catalog `INV-TCORE-001`…`004` + properties |
| `TokenCore.invariant.cam` | CP-001…CP-003 four-actor overlays |
| `TokenCore.invariant_ctor_mint.cam` | CP-004 with `deploy { 0xd01 }` |

Gate: `cargo test -p cambrian-transpiler --test test_smafd_plf_inv04`

Docs: [`docs/plans/plf-inv04-ctor-mint-invariant.md`](../../../../docs/plans/plf-inv04-ctor-mint-invariant.md) · EVM vacuity class [`docs/audit/PL-F-INV-04-EVM.md`](../../../../docs/audit/PL-F-INV-04-EVM.md) · **W15** lint on ill-posed `totalSupply() == const` when `burn` is in the action pool.
