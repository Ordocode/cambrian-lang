// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_audit-fuzz-tf004_project.sol";

/// T-F-004 reference wrapper (val_e07 shape).
/// Proptest harness generates per-case copies with baked threshold/multiplier/expected sum.
contract F004FoldChainExecTest is Test {
    FoldDemo internal demo;

    function setUp() public {
        demo = new FoldDemo();
    }

    function test_F004_foldChainSum() public {
        uint256[] memory xs = new uint256[](4);
        xs[0] = 1;
        xs[1] = 2;
        xs[2] = 3;
        xs[3] = 0;

        demo.compute(xs);

        // filter (>0) → map (*2) → fold; expected 12 (T-VAL-001 / val_e07)
        assertEq(demo.getSum(), 12, "filter.map.fold sum mismatch");
    }
}
