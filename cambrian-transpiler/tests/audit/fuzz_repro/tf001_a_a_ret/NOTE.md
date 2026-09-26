T-F-001 finding — FIXED (2026-07-22) via validation rule **V42**.

Historical: forge build failed on validator-clean mini-program with
`return(m_x)` on a route that declared no `-> T` (solc 8863 arity).

Entity: A route: a member: m_a param: u8 return-route: true

Current: such programs are rejected at validation (`V42`); proptest
`audit_fuzz_parser_evm_forge_build` no longer hits solc 8863 on this shape.

--- historical forge output ---
stdout:
Compiling 2 files with Solc 0.8.24
Solc 0.8.24 finished in 6.58ms

stderr:
    10|Error: Compiler run failed:
Error (8863): Different number of arguments in return statement than in returns declaration.
  --> src/_audit-fuzz-tf001_project.sol:14:9:
   |
14 |         return m_a;
   |         ^^^^^^^^^^
