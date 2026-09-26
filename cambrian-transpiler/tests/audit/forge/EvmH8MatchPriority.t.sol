// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_evm-h8-audit_project.sol";

/// T-EVM-008 / EVM-H8: first matching match arm must win (source order).
contract EvmH8MatchPriorityTest is Test {
    CambrianFactory internal factory;
    Matcher internal matcher;

    function setUp() public {
        factory = new CambrianFactory();
        matcher = Matcher(factory.deployMatcher());
    }

    function test_EVM_H8_matchUsesFirstMatchingArm() public {
        matcher.setCode(0);
        assertEq(matcher.classify(), 100, "EVM-H8: arm 0 => 100");

        matcher.setCode(1);
        assertEq(matcher.classify(), 200, "EVM-H8: arm 1 => 200");

        matcher.setCode(42);
        assertEq(matcher.classify(), 999, "EVM-H8: wildcard => 999");
    }
}
