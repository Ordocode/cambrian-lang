// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-dispatch-fail-callee-evm_project.sol";

/// PW3-G-004 row 3: dynamic dispatch to always-failing callee must revert on EVM.
contract Pw3DispatchFailCalleeTest is Test {
    CambrianFactory internal factory;
    Payee internal payee;
    Payer internal payer;

    function setUp() public {
        factory = new CambrianFactory();
        payee = Payee(address(factory.deployPayee(2)));
        payer = Payer(address(factory.deployPayer(1, address(payee))));
    }

    function test_PW3_G004_dynamicDispatchFailCalleeReverts() public {
        vm.expectRevert();
        payer.callFail();
    }
}
