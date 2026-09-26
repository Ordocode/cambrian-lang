// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-p7-closure-evm_project.sol";

/// PW3-G-007 baseline: admitted E07 surface reverts at runtime on EVM.
contract Pw3P7ClosureRevertTest is Test {
    CambrianFactory internal factory;
    ClosureAdmit internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = ClosureAdmit(address(factory.deployClosureAdmit()));
    }

    function test_PW3_G007_closureRuntimeRevert() public {
        vm.expectRevert(bytes("EVM: closure-as-value not supported (use `for x in iter { ... }` directly)"));
        target.run();
    }
}
