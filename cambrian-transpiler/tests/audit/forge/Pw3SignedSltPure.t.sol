// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-signed-slt-evm_project.sol";

/// PW3-G-005 / B-34: signed `i8` `-1 < 0` must be true on EVM.
contract Pw3SignedSltPureTest is Test {
    CambrianFactory internal factory;
    SignedSltPure internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = SignedSltPure(address(factory.deploySignedSltPure()));
    }

    function test_PW3_G005_signedMinusOneLessThanZero() public {
        assertTrue(target.probe(-1, 0), "i8 -1 < 0 (signed compare)");
    }
}
