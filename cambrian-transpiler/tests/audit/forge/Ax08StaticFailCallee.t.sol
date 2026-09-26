// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax08-01-evm_project.sol";

/// T-ARCH-002 / H-AX-08-01: static typed send to always-failing callee must revert on EVM.
contract Ax08StaticFailCalleeTest is Test {
    CambrianFactory internal factory;
    FailCallee internal callee;
    StaticRelay internal relay;

    function setUp() public {
        factory = new CambrianFactory();
        callee = FailCallee(address(factory.deployFailCallee(2)));
        relay = StaticRelay(address(factory.deployStaticRelay(1)));
    }

    function test_ARCH_AX08_01_staticFailCalleeReverts() public {
        vm.expectRevert();
        relay.relay(2, 0);
    }
}
