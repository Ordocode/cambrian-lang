# T-EVM-EX-006 / EVM-EX-H1 repro (mirror T-LEAN-EX-006)

Forge build failed on validator-clean expression cell.

--- forge output ---
stdout:
Compiling 2 files with Solc 0.8.24
Solc 0.8.24 finished in 5.00ms

stderr:
Error: Compiler run failed:
Error (9640): Explicit type conversion not allowed from "bool" to "uint64".
  --> src/_T-EVM-EX-006_project.sol:14:20:
   |
14 |         uint64 b = uint64((m_count == n));
   |                    ^^^^^^^^^^^^^^^^^^^^^^

