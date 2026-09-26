// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax05-02-evm_project.sol";

/// T-ARCH-019 / H-AX-05-02: `let sum = a + b` must revert on u64 overflow (EVM ground truth).
contract Ax05LetU64OverflowTest is Test {
    CambrianFactory internal factory;
    LetAddProbe internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = LetAddProbe(address(factory.deployLetAddProbe()));
    }

    function test_ARCH_AX05_02_letU64AddMaxPlusOneReverts() public {
        vm.expectRevert(stdError.arithmeticError);
        target.addViaLet(type(uint64).max, 1);
    }
}
