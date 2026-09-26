# T-EVM-EX-FUZZ / EVM-EX-H2 repro (T-EVM-EX-007)

Kind: UnaryNot
Literal: 337

--- forge output ---
stdout:
Compiling 2 files with Solc 0.8.24
Solc 0.8.24 finished in 8.26ms

stderr:
Error: Compiler run failed:
Error (9574): Type bool is not implicitly convertible to expected type uint256.
  --> src/_not_337_project.sol:17:9:
   |
17 |         uint256 x = (!m_flag);
   |         ^^^^^^^^^^^^^^^^^^^^^

