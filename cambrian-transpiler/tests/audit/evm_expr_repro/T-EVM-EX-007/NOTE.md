# T-EVM-EX-007 / EVM-EX-H1 repro (mirror T-LEAN-EX-007)

Forge build failed on validator-clean expression cell.

--- forge output ---
stdout:
Compiling 2 files with Solc 0.8.24
Solc 0.8.24 finished in 4.35ms

stderr:
Error: Compiler run failed:
Error (9574): Type bool is not implicitly convertible to expected type uint256.
  --> src/_T-EVM-EX-007_project.sol:17:9:
   |
17 |         uint256 b = (!m_flag);
   |         ^^^^^^^^^^^^^^^^^^^^^

