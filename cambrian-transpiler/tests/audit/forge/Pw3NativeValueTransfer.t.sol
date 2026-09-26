// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-native-value-evm_project.sol";

/// PW3-G-015 row 1: forge behavioral oracle — raw transfer credits payee ETH.
contract Pw3NativeValueTransferTest is Test {
    CambrianFactory internal factory;
    Payer internal payer;
    address internal payee;

    function setUp() public {
        factory = new CambrianFactory();
        payee = address(0xBEEF);
        payer = Payer(factory.deployPayer(1, payee));
    }

    function test_PW3_G015_valueTransferCreditsPayee() public {
        vm.deal(address(payer), 1 ether);
        uint256 before = payee.balance;
        payer.pay(0.25 ether);
        assertEq(
            payee.balance,
            before + 0.25 ether,
            "PW3-G-015 forge: payee must receive forwarded value"
        );
    }
}
