# T-EVM-ST-FUZZ repro (T-EVM-ST-001)

Kind: LetEscapeIf
Literal: 764

--- forge output ---
stdout:
Compiling 2 files with Solc 0.8.24
Solc 0.8.24 finished in 8.28ms

stderr:
Error: Compiler run failed:
Error (7576): Undeclared identifier.
  --> src/_let_escape_if_764_project.sol:17:23:
   |
17 |         return uint64(y);
   |                       ^

