# lean_sut_deploy_self_call — INT-004 regression fixture

Minimal two-entity project exercising:

1. Self-call on prefix `inst` before any SUT deploy.
2. Self-call on `pair_inst` after `deploy pair = Pair(...)`.
3. Latest deploy wins on redeploy (`pair2_inst`).
4. Property body with the same pattern.

Gate: `cargo test --release -p cambrian-transpiler --test test_lean_sut_deploy_self_call`

Plan: [`docs/plans/int004-sut-deploy-self-call.md`](../../../docs/plans/int004-sut-deploy-self-call.md)
