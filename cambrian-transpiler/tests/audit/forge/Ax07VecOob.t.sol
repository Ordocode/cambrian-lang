// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax07-01-evm_project.sol";

/// T-ARCH-006 / H-AX-07-01: out-of-bounds Vec read must revert on EVM (Panic 0x32).
contract Ax07VecOobTest is Test {
    CambrianFactory internal factory;
    VecOob internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = VecOob(address(factory.deployVecOob()));
    }

    function test_ARCH_AX07_01_vecIndexOobReverts() public {
        vm.expectRevert(stdError.indexOOBError);
        target.readAt(5);
    }
}
