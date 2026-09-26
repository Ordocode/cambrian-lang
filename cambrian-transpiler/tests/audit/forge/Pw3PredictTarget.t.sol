// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-predict-evm_project.sol";

/// PW3-G-003: EVM CREATE2 `predict*` oracle for cross-target comparison.
contract Pw3PredictTargetTest is Test {
    CambrianFactory internal factory;

    function setUp() public {
        factory = new CambrianFactory();
    }

    function test_PW3_G003_predictTargetId42() public view {
        address predicted = factory.predictPredictTarget(42);
        assertTrue(predicted != address(0), "CREATE2 predict must be non-zero");
    }
}
