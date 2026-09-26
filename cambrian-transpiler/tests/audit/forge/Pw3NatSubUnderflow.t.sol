// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-nat-sub-evm_project.sol";

/// PW3-O-002: `u64` underflow must revert on EVM (Panic 0x11).
contract Pw3NatSubUnderflowTest is Test {
    CambrianFactory internal factory;
    NatSubUnderflow internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = NatSubUnderflow(address(factory.deployNatSubUnderflow()));
    }

    function test_PW3_O002_probeUnderflowReverts() public {
        vm.expectRevert(stdError.arithmeticError);
        target.probe();
    }
}
