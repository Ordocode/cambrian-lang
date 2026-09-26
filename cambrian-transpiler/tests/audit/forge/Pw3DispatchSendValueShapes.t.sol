// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-dispatch-send-value-shapes-evm_project.sol";

/// PW3-G-004 row 5: escrow `send_value_shapes` funded typed send baseline (forge).
contract Pw3DispatchSendValueShapesTest is Test {
    CambrianFactory internal factory;
    Oracle internal oracle;
    Payer internal payer;

    function setUp() public {
        factory = new CambrianFactory();
        oracle = Oracle(factory.deployOracle());
        payer = Payer(factory.deployPayer());
    }

    function test_PW3_G004_sendValueShapesFundedPaySucceeds() public {
        vm.deal(address(payer), 1 ether);
        payer.pay(5, 1000);
        assertEq(payer.m_hits(), 1);
    }
}
