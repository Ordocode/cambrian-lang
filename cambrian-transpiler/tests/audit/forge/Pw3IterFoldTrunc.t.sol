// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-iter-fold-trunc-evm_project.sol";

/// PW3-O-007: per-step `(acc + x) as u64` fold — must revert when out of range.
contract Pw3IterFoldTruncTest is Test {
    CambrianFactory internal factory;
    FoldTrunc internal trunc;

    function setUp() public {
        factory = new CambrianFactory();
        trunc = FoldTrunc(factory.deployFoldTrunc());
    }

    function test_PW3_O007_truncFoldLargeElements() public {
        uint256[] memory xs = new uint256[](3);
        xs[0] = type(uint256).max;
        xs[1] = 2;
        xs[2] = 1;
        vm.expectRevert();
        trunc.run(xs);
    }
}
