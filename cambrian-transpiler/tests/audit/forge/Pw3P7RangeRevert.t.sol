// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-p7-range-evm_project.sol";

/// PW3-G-007 baseline: bare Range admitted by E07 reverts at runtime.
contract Pw3P7RangeRevertTest is Test {
    CambrianFactory internal factory;
    RangeAdmit internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = RangeAdmit(address(factory.deployRangeAdmit()));
    }

    function test_PW3_G007_rangeRuntimeRevert() public {
        vm.expectRevert(bytes("EVM: range expression only valid as `for` iterator on EVM"));
        target.run(3);
    }
}
