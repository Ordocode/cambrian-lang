// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_audit-fuzz-tf005_project.sol";

/// T-F-005: proptest-generated match arm priority check (first-match-wins).
contract F005MatchArmExecTest is Test {
    MatchDemo internal demo;

    function setUp() public {
        demo = new MatchDemo();
    }

    function test_F005_matchArms() public {
        demo.setCode(12);
assertEq(demo.classify(), 100, "arm lit=12 => 100");
        demo.setCode(14);
assertEq(demo.classify(), 115, "arm lit=14 => 115");
        demo.setCode(0);
assertEq(demo.classify(), 633, "wildcard for code=0");
    }
}
