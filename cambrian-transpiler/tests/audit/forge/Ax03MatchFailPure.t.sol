// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax03-04-evm_project.sol";

/// T-ARCH-021 / H-AX-03-04: fail-pure div in a `match` arm must revert on EVM.
contract Ax03MatchFailPureTest is Test {
    CambrianFactory internal factory;
    MatchFailPure internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = MatchFailPure(address(factory.deployMatchFailPure()));
    }

    function test_ARCH_AX03_04_matchFailPureReverts() public {
        vm.expectRevert(stdError.divisionError);
        target.pick(Mode.Risk, 0);
    }
}
