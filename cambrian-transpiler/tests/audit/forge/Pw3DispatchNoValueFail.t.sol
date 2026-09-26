// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-dispatch-no-value-fail-evm_project.sol";

/// PW3-G-004 row 4: dynamic `{ value: 0 }` to failing callee must revert on EVM.
contract Pw3DispatchNoValueFailTest is Test {
    CambrianFactory internal factory;
    Payee internal payee;
    Payer internal payer;

    function setUp() public {
        factory = new CambrianFactory();
        payee = Payee(address(factory.deployPayee(2)));
        payer = Payer(address(factory.deployPayer(1, address(payee))));
    }

    function test_PW3_G004_dynamicDispatchZeroValueFailCalleeReverts() public {
        vm.expectRevert();
        payer.payZero();
    }
}
