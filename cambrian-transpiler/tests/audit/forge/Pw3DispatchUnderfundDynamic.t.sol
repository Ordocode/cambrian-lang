// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-dispatch-underfund-dynamic-evm_project.sol";

/// PW3-G-004 row 2: underfunded dynamic `{ value }` dispatch must revert on EVM.
contract Pw3DispatchUnderfundDynamicTest is Test {
    CambrianFactory internal factory;
    Payee internal payee;
    Payer internal payer;

    function setUp() public {
        factory = new CambrianFactory();
        payee = Payee(address(factory.deployPayee(2)));
        payer = Payer(address(factory.deployPayer(1, address(payee))));
    }

    function test_PW3_G004_underfundDynamicDispatchReverts() public {
        vm.expectRevert();
        payer.pay(1 ether);
    }
}
