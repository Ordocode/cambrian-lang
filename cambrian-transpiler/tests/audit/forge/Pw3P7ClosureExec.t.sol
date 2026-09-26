// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-p7-closure-evm_project.sol";

/// PW3-G-007 red gate: P7 warning-only admission must not reach clean execution.
contract Pw3P7ClosureExecTest is Test {
    CambrianFactory internal factory;
    ClosureAdmit internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = ClosureAdmit(address(factory.deployClosureAdmit()));
    }

    function test_PW3_G007_closureRunShouldSucceed() public {
        assertEq(target.run(), 1);
    }
}
