// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax07-03-evm_project.sol";

/// T-ARCH-009 / H-AX-07-03: explicit if/else bool pick — EVM aligned semantics.
contract Ax07IfNoElseTest is Test {
    CambrianFactory internal factory;
    IfBoolAlign internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = IfBoolAlign(address(factory.deployIfBoolAlign()));
    }

    function test_ARCH_AX07_03_boolIfElsePickAligned() public {
        target.pick(true);
        assertTrue(target.getFlag(), "pick(true) must set flag");

        target.pick(false);
        assertFalse(target.getFlag(), "pick(false) must clear flag");
    }
}
