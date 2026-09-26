// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax01-01-evm_project.sol";

/// T-ARCH-010 / H-AX-01-01: underfunded static typed `{ value }` send must revert on EVM.
contract Ax01UnderfundTypedCallTest is Test {
    CambrianFactory internal factory;
    UnderfundCreditor internal creditor;
    UnderfundPayer internal payer;

    uint128 internal constant SMALL_BALANCE = 100;
    uint128 internal constant OVERPAY = 1000;

    function setUp() public {
        factory = new CambrianFactory();
        creditor = UnderfundCreditor(address(factory.deployUnderfundCreditor()));
        payer = UnderfundPayer(address(factory.deployUnderfundPayer()));
        vm.deal(address(payer), SMALL_BALANCE);
    }

    function test_ARCH_AX01_01_underfundedTypedCallReverts() public {
        assertEq(creditor.getSpent(), 0, "baseline");
        assertEq(payer.getDone(), 0, "baseline");

        vm.expectRevert();
        payer.tryPay(OVERPAY);

        assertEq(creditor.getSpent(), 0, "callee must not observe value after revert");
        assertEq(payer.getDone(), 0, "caller route must not commit on revert");
    }
}
