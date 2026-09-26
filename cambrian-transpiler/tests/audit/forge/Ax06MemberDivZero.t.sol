// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax06-03-evm_project.sol";

/// T-ARCH-020 / H-AX-06-03: member transform div0 must revert on EVM (ground truth).
contract Ax06MemberDivZeroTest is Test {
    CambrianFactory internal factory;
    DivMemberProbe internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = DivMemberProbe(address(factory.deployDivMemberProbe()));
    }

    function test_ARCH_AX06_03_memberDivZeroReverts() public {
        vm.expectRevert(stdError.divisionError);
        target.split(0);
    }
}
