# T-EVM-ST-001 / EVM-ST-H1 repro

Expected faithful `forge build`; got solc error.

--- forge output ---
stdout:
Compiling 2 files with Solc 0.8.24
Solc 0.8.24 finished in 34.61ms

stderr:
Error: Compiler run failed:
Error (7576): Undeclared identifier.
  --> src/_T-EVM-ST-001_project.sol:17:23:
   |
17 |         return uint64(y);
   |                       ^

