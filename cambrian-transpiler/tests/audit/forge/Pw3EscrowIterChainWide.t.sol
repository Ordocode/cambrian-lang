// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-escrow-iter-chain-wide-evm_project.sol";

/// PW3-G-014: map.filter.map.fold chain — expected sum 24 for [1,2,3,4].
contract Pw3EscrowIterChainWideTest is Test {
    CambrianFactory internal factory;
    ChainWide internal wide;

    function setUp() public {
        factory = new CambrianFactory();
        wide = ChainWide(factory.deployChainWide());
    }

    function test_pw3_g014_wide_sum_matches_expected() public {
        uint64[] memory items = new uint64[](4);
        items[0] = 1;
        items[1] = 2;
        items[2] = 3;
        items[3] = 4;
        assertEq(wide.run(items), 24, "map.filter.map.fold sum");
    }
}
