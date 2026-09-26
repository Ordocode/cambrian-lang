# T-EVM-ST-FUZZ repro (T-EVM-ST-001)

Kind: LetEscapeIf
Literal: 10

--- forge output ---
stdout:
Compiling 2 files with Solc 0.8.24
Solc 0.8.24 finished in 7.98ms

stderr:
Error: Compiler run failed:
Error (7576): Undeclared identifier.
  --> src/_let_escape_if_10_project.sol:17:23:
   |
17 |         return uint64(y);
   |                       ^

