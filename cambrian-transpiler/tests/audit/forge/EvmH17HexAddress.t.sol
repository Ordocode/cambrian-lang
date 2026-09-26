// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_evm-h17-audit_project.sol";

/// T-EVM-016 / EVM-H17: 40-nibble hex in a `bytes32` let must not be wrapped as `address`.
contract EvmH17HexAddressTest is Test {
    CambrianFactory internal factory;
    HexProbe internal probe;

    bytes32 internal constant EXPECTED =
        bytes32(uint256(0x000000000000000000000000A11CE00000000000000000000000000000000000));

    function setUp() public {
        factory = new CambrianFactory();
        probe = HexProbe(factory.deployHexProbe());
    }

    function test_EVM_H17_addressShapedHexBindsAsBytes32() public {
        assertEq(probe.probe(), EXPECTED, "EVM-H17: probe must return the bytes32 literal unchanged");
    }
}
