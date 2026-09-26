// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-iter-tf004-anchor-evm_project.sol";

/// PW3-S-015 row 4: T-F-004 anchor — filter().map().fold() green baseline.
contract Pw3IterTf004AnchorTest is Test {
    CambrianFactory internal factory;
    Tf004Anchor internal demo;

    function setUp() public {
        factory = new CambrianFactory();
        demo = Tf004Anchor(factory.deployTf004Anchor());
    }

    function test_PW3_S015_tf004AnchorSum() public {
        uint256[] memory xs = new uint256[](4);
        xs[0] = 1;
        xs[1] = 2;
        xs[2] = 3;
        xs[3] = 0;
        demo.compute(xs);
        assertEq(demo.getSum(), 12, "T-F-004 anchor sum");
    }
}
