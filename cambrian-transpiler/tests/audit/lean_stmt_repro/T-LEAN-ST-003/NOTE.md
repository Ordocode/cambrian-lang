# T-LEAN-ST-003 / LEAN-ST-H1 repro

Phased `for` in state-only route: invalid `do` notation / ill-typed route tail.

Expected: faithful `lake build`. Got Lean type error on generated route body.

Re-verify:

    CAMBRIAN_TEST_LEAN_BUILD=1 cargo test -p cambrian-transpiler --test test_audit_lean_stmt audit_lean_stmt_matrix_lake_build -- --nocapture
