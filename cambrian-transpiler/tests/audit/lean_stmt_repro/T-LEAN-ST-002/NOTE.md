# T-LEAN-ST-002 / LEAN-ST-H2 repro

`var` capture inside world-threaded `if`: binder does not escape branch.

Expected: faithful `lake build`. Got `Unknown identifier 'alive'`.

Re-verify:

    CAMBRIAN_TEST_LEAN_BUILD=1 cargo test -p cambrian-transpiler --test test_audit_lean_stmt audit_lean_stmt_matrix_lake_build -- --nocapture
