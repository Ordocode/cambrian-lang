# T-EVM-EX-FUZZ / EVM-EX-H2 repro (T-EVM-EX-006)

Kind: CompareEq
Literal: 2

--- forge output ---
stdout:
Compiling 2 files with Solc 0.8.24
Solc 0.8.24 finished in 8.42ms

stderr:
Error: Compiler run failed:
Error (9640): Explicit type conversion not allowed from "bool" to "uint64".
  --> src/_eq_2_project.sol:14:20:
   |
14 |         uint64 x = uint64((m_count == 2));
   |                    ^^^^^^^^^^^^^^^^^^^^^^

