// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-div-zero-evm_project.sol";

/// PW3-O-006: division by zero must revert on EVM.
contract Pw3DivByZeroTest is Test {
    CambrianFactory internal factory;
    DivByZero internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = DivByZero(address(factory.deployDivByZero()));
    }

    function test_PW3_O006_divByZeroReverts() public {
        vm.expectRevert(stdError.divisionError);
        target.divByZero();
    }
}
