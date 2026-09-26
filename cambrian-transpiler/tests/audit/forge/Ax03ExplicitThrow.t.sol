// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax03-05-evm_project.sol";

/// T-ARCH-023 / H-AX-03-05: explicit `throw 42` must revert on EVM (not skipped).
contract Ax03ExplicitThrowTest is Test {
    CambrianFactory internal factory;
    ThrowProbe internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = ThrowProbe(address(factory.deployThrowProbe()));
    }

    function test_ARCH_AX03_05_explicitThrowReverts() public {
        vm.expectRevert(bytes("throw(42)"));
        target.bail();
    }
}
