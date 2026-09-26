// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_std-diff-math_project.sol";

/// T-STD-DIFF-001: EVM ground truth for shared `std::math` differential gate.
contract StdDiffMathTest is Test {
    CambrianFactory internal factory;
    StdDiffMath internal math;

    function setUp() public {
        factory = new CambrianFactory();
        math = StdDiffMath(factory.deployStdDiffMath());
    }

    function test_std_diff_muldiv() public {
        assertEq(math.runMuldiv(), 400);
    }

    function test_std_diff_clamp() public {
        assertEq(math.runClamp(), 100);
    }
}
