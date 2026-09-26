// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax03-02-evm_project.sol";

/// T-ARCH-007 / H-AX-03-02: div-by-zero inside `for` body must revert on EVM.
contract Ax03ForDivZeroTest is Test {
    CambrianFactory internal factory;
    ForDivZero internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = ForDivZero(address(factory.deployForDivZero()));
    }

    function test_ARCH_AX03_02_forDivZeroReverts() public {
        vm.expectRevert(stdError.divisionError);
        target.foldDiv();
    }
}
