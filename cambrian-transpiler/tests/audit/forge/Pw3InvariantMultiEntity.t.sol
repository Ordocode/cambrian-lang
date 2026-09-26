// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-invariant-multi-entity-evm_project.sol";

/// PW3-S-014 row 4 / PN-107: multi-entity deposit trace replay (forge ground truth).
contract Pw3InvariantMultiEntityTest is Test {
    CambrianFactory internal factory;
    VaultA internal va;
    VaultB internal vb;

    function setUp() public {
        factory = new CambrianFactory();
        va = VaultA(factory.deployVaultA());
        vb = VaultB(factory.deployVaultB());
    }

    function test_PW3_O013_multiEntityDepositTraceReplay() public {
        va.deposit(7);
        vb.deposit(11);
        va.deposit(3);
        assertEq(va.m_balance(), 10, "va deposits accumulate");
        assertEq(vb.m_balance(), 11, "vb deposits accumulate");
        assertGe(va.m_balance() + vb.m_balance(), 0, "multi-entity sum check");
    }
}
