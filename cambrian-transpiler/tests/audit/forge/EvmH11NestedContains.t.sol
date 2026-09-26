// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_evm-h11-audit_project.sol";

/// T-EVM-011 / EVM-H11: nested contains must compile (inner_exists sidecar).
contract EvmH11NestedContainsTest is Test {
    CambrianFactory internal factory;
    Allowance internal allowance;

    function setUp() public {
        factory = new CambrianFactory();
        allowance = Allowance(factory.deployAllowance());
    }

    function test_EVM_H11_nestedContainsCompilesAndReturnsFalseOnEmpty() public {
        assertFalse(
            allowance.hasPair(address(1), address(2)),
            "EVM-H11: empty nested map should report no allowance"
        );
    }
}
