// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-stdlib-divc-evm_project.sol";

/// PW3-G-009: `std::math::divc` with zero divisor must revert on EVM.
contract Pw3StdDivcZeroTest is Test {
    CambrianFactory internal factory;
    StdDivcZero internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = StdDivcZero(address(factory.deployStdDivcZero()));
    }

    function test_PW3_G009_stdDivcZeroReverts() public {
        vm.expectRevert(stdError.divisionError);
        target.divcZero();
    }
}
