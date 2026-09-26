// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_evm-h15-audit_project.sol";

/// T-EVM-015 / EVM-H15: reserved Solidity route name must compile in deterministic mode.
contract EvmH15ReservedRouteTest is Test {
    CambrianFactory internal factory;
    Opcode internal opcode;

    function setUp() public {
        factory = new CambrianFactory();
        opcode = Opcode(factory.deployOpcode());
    }

    function test_EVM_H15_reservedRouteNameIsCallable() public {
        // Deterministic codegen may emit `_assembly` if sanitized, or bare `assembly` if not.
        (bool okSanitized,) = address(opcode).call(
            abi.encodeWithSignature("_assembly(uint64)", uint64(4))
        );
        if (okSanitized) {
            assertTrue(okSanitized, "EVM-H15: sanitized assembly route should be callable");
            return;
        }
        (bool okRaw,) = address(opcode).call(abi.encodeWithSignature("assembly(uint64)", uint64(4)));
        assertTrue(okRaw, "EVM-H15: reserved route name must be emitted and callable");
    }
}
