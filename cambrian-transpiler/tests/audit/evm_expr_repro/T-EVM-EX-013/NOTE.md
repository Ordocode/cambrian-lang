# T-EVM-EX-013 / EVM-EX-H1 repro (mirror T-LEAN-EX-013)

Forge build failed on validator-clean expression cell.

--- forge output ---
stdout:
Compiling 2 files with Solc 0.8.24
Solc 0.8.24 finished in 8.39ms

stderr:
Error: Compiler run failed:
Error (7576): Undeclared identifier. Did you mean "next_m_b"?
  --> src/_T-EVM-EX-013_project.sol:22:38:
   |
22 |             uint64 next_m_b = uint64(next_m_a);
   |                                      ^^^^^^^^

