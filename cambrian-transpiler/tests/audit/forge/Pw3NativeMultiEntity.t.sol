// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-native-multi-evm_project.sol";

/// PW3-G-015 row 4: forge behavioral oracle — independent entity state updates.
contract Pw3NativeMultiEntityTest is Test {
    CambrianFactory internal factory;
    Vault internal vault;
    Ledger internal ledger;

    function setUp() public {
        factory = new CambrianFactory();
        vault = Vault(factory.deployVault());
        ledger = Ledger(factory.deployLedger());
    }

    function test_PW3_G015_multiEntityStateUpdates() public {
        vault.credit(100);
        ledger.bump(3);
        assertEq(vault.m_total(), 100, "PW3-G-015 forge: vault credit");
        assertEq(ledger.m_count(), 3, "PW3-G-015 forge: ledger bump");
    }
}
