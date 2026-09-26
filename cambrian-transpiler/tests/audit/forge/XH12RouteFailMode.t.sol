// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_x-h12-evm-audit_project.sol";

/// T-X-010 / LEAN-H10 / X-H12: nested `failingHelper` where-guard propagates through `invoke`.
contract XH12RouteFailModeTest is Test {
    CambrianFactory internal factory;
    FailSurface internal failSurface;

    function setUp() public {
        factory = new CambrianFactory();
        failSurface = FailSurface(address(factory.deployFailSurface()));
    }

    function test_X010_nestedFailModeRevertAndPass() public {
        // `where amount < 500 : throw 2` — guard fails when amount >= 500.
        failSurface.invoke(400);

        vm.expectRevert(bytes("throw(2)"));
        failSurface.invoke(600);
    }
}
