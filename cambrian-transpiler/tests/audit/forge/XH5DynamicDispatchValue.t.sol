// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_x-h5-evm-audit_project.sol";

/// T-X-005 / LEAN-H7: dynamic `deposit() ~> m_dest with { value }` must debit Payer
/// and credit Payee on EVM (Lean omits `WorldState.transfer` — see T-LEAN-005).
contract XH5DynamicDispatchValueTest is Test {
    CambrianFactory internal factory;
    Payee internal payee;
    Payer internal payer;

    uint64 internal constant PAYEE_ID = 2;
    uint64 internal constant PAYER_ID = 1;
    uint128 internal constant PAY_AMOUNT = 1 ether;

    function setUp() public {
        factory = new CambrianFactory();
        payee = Payee(address(factory.deployPayee(PAYEE_ID)));
        payer = Payer(address(factory.deployPayer(PAYER_ID, address(payee))));
    }

    function test_X005_dynamicDispatchValueDebitsPayerAndCreditsPayee() public {
        vm.deal(address(payer), 10 ether);

        uint256 payerEthBefore = address(payer).balance;
        uint256 payeeEthBefore = address(payee).balance;
        uint128 payeeBalBefore = payee.getBalance();

        payer.pay(PAY_AMOUNT);

        assertEq(
            address(payer).balance,
            payerEthBefore - PAY_AMOUNT,
            "X005: Payer contract ETH must decrease by send amount"
        );
        assertEq(
            address(payee).balance,
            payeeEthBefore + PAY_AMOUNT,
            "X005: Payee contract ETH must increase by send amount"
        );
        assertEq(
            payee.getBalance(),
            payeeBalBefore + PAY_AMOUNT,
            "X005: Payee m_balance must record msg.value from deposit"
        );
    }
}
