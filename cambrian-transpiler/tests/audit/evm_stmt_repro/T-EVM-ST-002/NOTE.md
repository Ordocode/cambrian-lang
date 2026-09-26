# T-EVM-ST-002 / EVM-ST-H1 repro

Expected faithful `forge build`; got solc error.

--- forge output ---
stdout:
Compiling 2 files with Solc 0.8.24
Solc 0.8.24 finished in 13.28ms

stderr:
Error: Compiler run failed:
Error (7576): Undeclared identifier.
  --> src/_T-EVM-ST-002_project.sol:25:20:
   |
25 |             return alive;
   |                    ^^^^^

