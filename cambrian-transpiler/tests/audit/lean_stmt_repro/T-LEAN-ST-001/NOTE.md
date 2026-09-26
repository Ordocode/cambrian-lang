# T-LEAN-ST-001 / LEAN-ST-H1 repro

Bare top-level `for` in state-only route: route tail `s` appended outside `do` block.

Expected: faithful `lake build`. Got ill-typed Lean (`Function expected at` / route tail).

Re-verify:

    CAMBRIAN_TEST_LEAN_BUILD=1 cargo test -p cambrian-transpiler --test test_audit_lean_stmt audit_lean_stmt_matrix_lake_build -- --nocapture
