# T-EVM-EX-FUZZ / EVM-EX-H2 repro (T-EVM-EX-013)

Kind: PhasedRef
Literal: 8

--- forge output ---
stdout:
Compiling 2 files with Solc 0.8.24
Solc 0.8.24 finished in 8.17ms

stderr:
Error: Compiler run failed:
Error (7576): Undeclared identifier. Did you mean "next_m_b"?
  --> src/_phased_8_project.sol:22:38:
   |
22 |             uint64 next_m_b = uint64(next_m_a);
   |                                      ^^^^^^^^

