// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax01-06-evm_project.sol";

/// T-ARCH-026 / H-AX-01-06: raw transfer must invoke target `receive()` on EVM.
contract Ax01RawTransferReceiveTest is Test {
    CambrianFactory internal factory;
    ValueVault internal vault;
    RawPayer internal payer;

    function setUp() public {
        factory = new CambrianFactory();
        vault = ValueVault(payable(factory.deployValueVault()));
        payer = RawPayer(payable(factory.deployRawPayer()));
        vm.deal(address(payer), 1 ether);
    }

    function test_ARCH_AX01_06_rawTransferCreditsReceiveTransform() public {
        assertEq(vault.getDeposits(), 0, "baseline");
        payer.pay(5);
        assertEq(vault.getDeposits(), 5, "EVM: receive transform must run on raw transfer");
    }
}
