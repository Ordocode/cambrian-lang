// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_x-h2-evm-audit_project.sol";

/// T-X-002 / LEAN-H3: underfunded raw `~>` transfer must revert on EVM.
contract XH2UnderfundedTransferTest is Test {
    CambrianFactory internal factory;
    Wallet internal wallet;
    address internal recipient = address(0xBEEF);

    function setUp() public {
        factory = new CambrianFactory();
        wallet = Wallet(factory.deployWallet());
    }

    function test_X002_underfundedRawTransferReverts() public {
        assertEq(wallet.getBalance(), 100, "fixture seeds m_balance only; contract holds 0 ETH");

        vm.expectRevert(bytes("transfer failed"));
        wallet.sendTooMuch(recipient);
    }
}
