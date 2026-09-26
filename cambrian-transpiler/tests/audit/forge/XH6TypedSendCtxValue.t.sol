// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_x-h6-evm-audit_project.sol";

/// T-X-006 / LEAN-H4: typed send with `{ value }` must credit Payee `m_spent` via `msg.value`.
/// Exercises `send` (stored `m_payee` twin of `pay` + `Payee.address`).
contract XH6TypedSendCtxValueTest is Test {
    CambrianFactory internal factory;
    Payee internal payee;
    Payer internal payer;

    uint64 internal constant PAYEE_ID = 2;
    uint64 internal constant PAYER_ID = 1;
    uint128 internal constant SEND_AMOUNT = 1 ether;

    function setUp() public {
        factory = new CambrianFactory();
        payee = Payee(address(factory.deployPayee(PAYEE_ID)));
        payer = Payer(address(factory.deployPayer(PAYER_ID, address(payee))));
        vm.deal(address(payer), 10 ether);
    }

    function test_X006_typedSendValueCreditsPayeeSpent() public {
        assertEq(payee.getSpent(), 0, "baseline");

        payer.pay(SEND_AMOUNT);

        assertEq(
            payee.getSpent(),
            SEND_AMOUNT,
            "X006: callee must observe msg.value from typed send with value"
        );
    }
}
