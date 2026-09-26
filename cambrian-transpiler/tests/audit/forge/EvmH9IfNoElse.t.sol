// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_evm-h9-audit_project.sol";

/// T-EVM-009 / EVM-H9: `if` without `else` on bool must yield false when condition is false.
contract EvmH9IfNoElseTest is Test {
    CambrianFactory internal factory;
    BoolPick internal picker;

    function setUp() public {
        factory = new CambrianFactory();
        picker = BoolPick(factory.deployBoolPick());
    }

    function test_EVM_H9_ifWithoutElseDefaultsBoolToFalse() public {
        picker.pick(true);
        assertTrue(picker.getFlag(), "EVM-H9: pick(true) should set flag");

        picker.pick(false);
        assertFalse(picker.getFlag(), "EVM-H9: pick(false) must clear flag (not leave true / invalid 0)");
    }
}
