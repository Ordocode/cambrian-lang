// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax03-01-evm_project.sol";

/// T-ARCH-001 / H-AX-03-01: `let q = a/b; return(q)` must revert on div0 (EVM ground truth).
contract Ax03LetDivZeroTest is Test {
    CambrianFactory internal factory;
    LetDivZero internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = LetDivZero(address(factory.deployLetDivZero()));
    }

    function test_ARCH_AX03_01_letDivZeroReverts() public {
        vm.expectRevert(stdError.divisionError);
        target.split();
    }
}
