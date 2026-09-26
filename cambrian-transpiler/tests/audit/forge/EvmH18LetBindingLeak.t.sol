// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_evm-h18-audit_project.sol";

/// T-EVM-017 / EVM-H18: routeB bool let must not inherit routeA tuple binding types.
contract EvmH18LetBindingLeakTest is Test {
    CambrianFactory internal factory;
    BindingLeak internal leak;

    function setUp() public {
        factory = new CambrianFactory();
        leak = BindingLeak(factory.deployBindingLeak());
    }

    function test_EVM_H18_routeBStoresBoolAfterRouteAQuotientLet() public {
        leak.routeA(10, 3);
        leak.routeB();

        assertTrue(leak.getFlag(), "EVM-H18: routeB should complete and set m_flag true");
    }
}
