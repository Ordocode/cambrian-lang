T-F-005 finding: validator-clean match arms failed forge build/exec or wrong classify.
arms: [(0, 100), (1, 200)] default: 999
expected classify(2) = 999

--- forge output ---
stdout:
Compiler run failed:
Error (6160): Wrong argument count for function call: 0 arguments given but expected 1.
  --> test/F005MatchArmExec.t.sol:12:16:
   |
12 |         demo = new MatchDemo();
   |                ^^^^^^^^^^^^^^^


stderr:
Error: Compilation failed

