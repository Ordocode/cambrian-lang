// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax08-03-evm_project.sol";

/// T-ARCH-025 / H-AX-08-03: distinct from-clause throw codes collapse to generic EVM revert.
contract Ax08MultiFromThrowTest is Test {
    DualFromProbe internal target;

    function setUp() public {
        target = new DualFromProbe(
            address(uint160(0xAA)),
            address(uint160(0xBB))
        );
    }

    function test_ARCH_AX08_03_multiFromUsesGenericRevertMessage() public {
        vm.expectRevert(bytes("from clause failed"));
        target.gate();
    }
}
