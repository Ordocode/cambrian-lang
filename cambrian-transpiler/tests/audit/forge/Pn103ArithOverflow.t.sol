// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pn-arith-max-evm_project.sol";

/// T-PN-ARITH-001 / PN-103: EVM reference — checked add reverts at MAX.
contract Pn103ArithOverflowTest is Test {
    CambrianFactory internal factory;

    function setUp() public {
        factory = new CambrianFactory();
    }

    function test_PN103_maxPlusOneReverts() public {
        Adder a = Adder(factory.deployAdder());
        a.bump(type(uint256).max);
        vm.expectRevert();
        a.bump(1);
    }
}
