// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-dispatch-underfund-raw-evm_project.sol";

/// PW3-G-004 row 1 baseline: raw underfund reverts on EVM (T-X-002 aligned).
contract Pw3DispatchUnderfundRawTest is Test {
    CambrianFactory internal factory;
    Wallet internal wallet;
    address internal recipient = address(0xBEEF);

    function setUp() public {
        factory = new CambrianFactory();
        wallet = Wallet(factory.deployWallet());
    }

    function test_PW3_G004_underfundRawTransferReverts() public {
        assertEq(wallet.getBalance(), 100, "fixture seeds m_balance only; contract holds 0 ETH");
        vm.expectRevert(bytes("transfer failed"));
        wallet.sendTooMuch(recipient);
    }
}
