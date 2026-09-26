// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax07-02-evm_project.sol";

/// T-ARCH-008 / H-AX-07-02: missing HashMap key → silent `0`, then fallible callee probe.
contract Ax07SilentZeroTest is Test {
    CambrianFactory internal factory;
    ZeroGate internal gate;
    SilentZeroProbe internal probe;

    function setUp() public {
        factory = new CambrianFactory();
        gate = ZeroGate(address(factory.deployZeroGate(2)));
        probe = SilentZeroProbe(address(factory.deploySilentZeroProbe(1)));
    }

    function test_ARCH_AX07_02_missingSlotSilentZeroThenRelayReverts() public {
        assertEq(probe.readSlot(999), 0, "missing map key must silently default to 0");
        vm.expectRevert();
        probe.relay(2, 0);
        assertEq(probe.m_steps(), 0, "failed relay must not commit m_steps on EVM");
    }
}
