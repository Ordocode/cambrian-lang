// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-escrow-nested-hashmap-evm_project.sol";

/// PW3-G-006 WONT FIX: nested HashMap `[]` on a missing outer key returns 0
/// (Solidity mapping default). Use `exists` / `contains` for membership.
contract Pw3EscrowNestedHashmapTest is Test {
    CambrianFactory internal factory;
    NestedProbe internal probe;

    function setUp() public {
        factory = new CambrianFactory();
        probe = NestedProbe(factory.deployNestedProbe());
    }

    function test_pw3_g006_missing_outer_key_returns_zero() public {
        address unknown = address(0xBEEF);
        assertEq(probe.read(unknown, 1), 0);
    }
}
