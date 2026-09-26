# T-STD-DIFF-001 / STD-H-DIFF-1 repro

Divergence: YES — EVM PASS, Lean not aligned

Fixture: `tests/audit/fixtures/std_diff_math.cam`

Re-run:
`cargo test -p cambrian-transpiler --test test_audit_std_diff audit_std_diff_math -- --nocapture`

--- forge log ---
stdout:
Compiling 21 files with Solc 0.8.24
Solc 0.8.24 finished in 692.75ms
Compiler run successful with warnings:
Warning (2018): Function state mutability can be restricted to pure
  --> src/_std-diff-math_project.sol:69:5:
   |
69 |     function runMuldiv() external returns (uint64) {
   |     ^ (Relevant source part starts here and spans across multiple lines).

Warning (2018): Function state mutability can be restricted to pure
  --> src/_std-diff-math_project.sol:73:5:
   |
73 |     function runClamp() external returns (uint64) {
   |     ^ (Relevant source part starts here and spans across multiple lines).


Ran 2 tests for test/StdDiffMath.t.sol:StdDiffMathTest
[PASS] test_std_diff_clamp() (gas: 5834)
[PASS] test_std_diff_muldiv() (gas: 5979)
Suite result: ok. 2 passed; 0 failed; 0 skipped; finished in 691.25µs (170.54µs CPU time)

Ran 1 test suite in 156.77ms (691.25µs CPU time): 2 tests passed, 0 failed, 0 skipped (2 total tests)

stderr:


--- lean inspect ---
route `runClamp` missing `Cambrian.clamp 150 0 100`
def runClamp (w : Cambrian.Generated.World) (inst : StdDiffMath.Identity) (ctx : Cambrian.MsgCtx) : Cambrian.Generated.World × BitVec 64 :=
  let s := w.storage.stdDiffMath inst
  let (s, payload) :=
    (s, (Cambrian.clamp (150 : BitVec 64) (0 : BitVec 64) (100 : BitVec 64)))
  (Cambrian.Generated.World.withStdDiffMath w inst s, payload)
