// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_evm-h4-audit_project.sol";

/// T-EVM-004 / EVM-H4: typed send with value must credit Payee when fund() reads msg::value.
contract EvmH4InterfacePayableTest is Test {
    CambrianFactory internal factory;
    Payer internal payer;
    Payee internal payee;

    uint64 internal constant PAYER_ID = 1;
    uint64 internal constant PAYEE_ID = 2;
    uint256 internal constant SEND_VALUE = 1 ether;

    function setUp() public {
        factory = new CambrianFactory();
        payee = Payee(factory.deployPayee(PAYEE_ID));
        payer = Payer(factory.deployPayer(PAYER_ID, address(payee)));
    }

    function test_EVM_H4_typedSendWithValueCreditsPayee() public {
        // Payer must hold the ETH it forwards via typed send `{ value }`.
        vm.deal(address(payer), SEND_VALUE);
        payer.sendFund();

        assertEq(
            payee.getTotal(),
            SEND_VALUE,
            "EVM-H4: fund() must observe msg.value from typed send"
        );
    }
}
