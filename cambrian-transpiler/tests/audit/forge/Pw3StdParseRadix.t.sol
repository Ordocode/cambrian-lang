// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-stdlib-parse-evm_project.sol";

/// PW3-G-010: `parse_uint` must discriminate radix on EVM.
contract Pw3StdParseRadixTest is Test {
    CambrianFactory internal factory;
    StdParseRadix internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = StdParseRadix(address(factory.deployStdParseRadix()));
    }

    function test_PW3_G010_parseRadixDecFailsHexSucceeds() public {
        assertEq(target.parseDecFf(), 999);
        assertEq(target.parseHexFf(), 255);
    }
}
