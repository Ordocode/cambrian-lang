// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-invariant-phased-value-evm_project.sol";

/// PW3-G-012 row 2: phased value transfer + trace-shaped state after fund().
contract Pw3InvariantPhasedValueTest is Test {
    CambrianFactory internal factory;
    ValuePhase internal payer;
    address internal recipient;

    function setUp() public {
        factory = new CambrianFactory();
        recipient = address(0xCAFE);
        payer = ValuePhase(factory.deployValuePhase(recipient));
        vm.store(address(payer), bytes32(uint256(0)), bytes32(uint256(uint160(recipient))));
    }

    function test_PW3_G012_phasedValueFundMarksAndPays() public {
        vm.deal(address(payer), 1 ether);
        uint256 before = recipient.balance;
        payer.fund(100);
        assertEq(payer.m_marked(), 1, "G012: mark phase must bump m_marked once per fund");
        assertEq(payer.m_paid(), 100, "G012: send phase must accumulate paid amount");
        assertEq(recipient.balance, before + 100, "G012: value must reach recipient");
    }
}
