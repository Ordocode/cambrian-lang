// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-p7-range-evm_project.sol";

/// PW3-G-007 red gate: validator warning-only admission must not reach clean execution.
contract Pw3P7RangeExecTest is Test {
    CambrianFactory internal factory;
    RangeAdmit internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = RangeAdmit(address(factory.deployRangeAdmit()));
    }

    function test_PW3_G007_rangeRunShouldSucceed() public {
        assertEq(target.run(3), 3);
    }
}
