# T-STD-EVM-003 / STD-H-EVM-3 repro

Forge test failed for `StdCryptoMatrixTest`.

Fixture: `tests/fixtures/std_crypto_matrix.cam`
Routes: runStdSha256, runEvmSha256

Re-run:
`cargo test -p cambrian-transpiler --test test_audit_std_evm audit_std_evm_crypto_forge -- --nocapture`

--- forge log ---
stdout:
Compiler run failed:
Error (6359): Return argument type bytes32 is not implicitly convertible to expected type (type of first return variable) bytes memory.
  --> src/_std-crypto-matrix_project.sol:21:16:
   |
21 |         return sha256(bytes("abc"));
   |                ^^^^^^^^^^^^^^^^^^^^


stderr:
Error: Compilation failed

