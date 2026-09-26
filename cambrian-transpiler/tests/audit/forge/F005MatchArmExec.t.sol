// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_audit-fuzz-tf005_project.sol";

/// T-F-005 reference wrapper (evm_h8 shape).
/// Proptest harness generates per-case copies with baked literal arms + wildcard.
contract F005MatchArmExecTest is Test {
    MatchDemo internal demo;

    function setUp() public {
        demo = new MatchDemo();
    }

    function test_F005_matchArms() public {
        demo.setCode(0);
        assertEq(demo.classify(), 100, "arm lit=0 => 100");

        demo.setCode(1);
        assertEq(demo.classify(), 200, "arm lit=1 => 200");

        demo.setCode(42);
        assertEq(demo.classify(), 999, "wildcard for code=42");
    }
}
