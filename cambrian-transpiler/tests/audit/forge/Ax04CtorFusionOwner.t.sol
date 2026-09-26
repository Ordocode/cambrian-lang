// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax04-07-evm_project.sol";

/// T-ARCH-029 / H-AX-04-07: det pair-fusion — `initialize` runs before `startPrank`,
/// so `msg::sender` in init transforms is the test contract, not ALICE.
contract Ax04CtorFusionOwnerTest is Test {
    address internal constant ALICE =
        address(uint160(uint256(0x0000000000000000000000000000000000000000000000000000000000000002)));

    function test_ARCH_AX04_07_fusionInitializeSetsOwnerToTestContract() public {
        OwnerProbe probe = new OwnerProbe(address(this));
        probe.initialize(ALICE);
        assertEq(probe.getOwner(), address(this), "det fusion: owner must be test contract");
        assertTrue(probe.getOwner() != ALICE, "det fusion: must not honor msg pin during initialize");
    }
}
