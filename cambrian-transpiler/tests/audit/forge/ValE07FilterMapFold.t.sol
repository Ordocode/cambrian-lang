// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_val-e07-audit_project.sol";

/// T-VAL-001 / X-H1: EVM-12 filter→map→fold chain must compute correct sum.
contract ValE07FilterMapFoldTest is Test {
    CambrianFactory internal factory;
    ChainFold internal demo;

    function setUp() public {
        factory = new CambrianFactory();
        demo = ChainFold(address(factory.deployChainFold()));
    }

    function test_VAL_E07_filterMapFoldSumIsTwelve() public {
        uint256[] memory xs = new uint256[](4);
        xs[0] = 1;
        xs[1] = 2;
        xs[2] = 3;
        xs[3] = 0;

        demo.compute(xs);

        // filter (>0): 1,2,3 → map (*2): 2,4,6 → fold sum = 12
        assertEq(demo.getSum(), 12, "filter.map.fold must lower to working Solidity");
    }
}
