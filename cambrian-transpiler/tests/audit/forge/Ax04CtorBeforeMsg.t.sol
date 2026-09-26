// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax04-01-evm_project.sol";

/// T-ARCH-012 / H-AX-04-01: W11 init-route/msg ordering — EVM ground truth.
contract Ax04CtorBeforeMsgTest is Test {
    CambrianFactory internal factory;
    address internal constant ZERO = address(0);

    function setUp() public {
        factory = new CambrianFactory();
    }

    function test_ARCH_AX04_01_msgBeforeGateSucceeds() public {
        SenderGate gate = SenderGate(address(factory.deploySenderGate()));
        assertTrue(gate.readOk(), "factory deploy: init guard passes with non-zero factory sender");
    }

    function test_ARCH_AX04_01_gateBeforeMsgGuardReverts() public {
        vm.prank(ZERO);
        vm.expectRevert();
        factory.deploySenderGate();
    }
}
