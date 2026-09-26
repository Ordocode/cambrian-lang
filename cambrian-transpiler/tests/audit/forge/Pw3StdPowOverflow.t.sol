// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-stdlib-pow-evm_project.sol";

/// PW3-G-009: `std::math::pow(2, 64)` on `u64` wraps to 0 on EVM (silent overflow).
contract Pw3StdPowOverflowTest is Test {
    CambrianFactory internal factory;
    StdPowOverflow internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = StdPowOverflow(address(factory.deployStdPowOverflow()));
    }

    function test_PW3_G009_stdPowOverflowWrapsToZero() public {
        assertEq(target.powOverflow(), 0);
    }
}
