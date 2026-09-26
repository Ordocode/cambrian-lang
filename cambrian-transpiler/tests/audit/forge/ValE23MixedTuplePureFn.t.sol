// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_val-e23-audit_project.sol";

/// T-VAL-002 / X-H2: tuple slots from `split` must keep distinct types/values.
contract ValE23MixedTuplePureFnTest is Test {
    CambrianFactory internal factory;
    TupleSplit internal demo;
    address internal caller = address(0xCAFE);

    function setUp() public {
        factory = new CambrianFactory();
        demo = TupleSplit(address(factory.deployTupleSplit()));
    }

    function test_VAL_E02_mixedTuplePureFnPreservesSlots() public {
        vm.prank(caller);
        (address owner, uint256 amt, bool flag) = demo.peek(caller, 42);

        assertEq(owner, caller, "address slot must stay address-typed");
        assertEq(amt, 42, "amount slot must carry seed unchanged");
        assertTrue(flag, "bool slot must be true for positive seed");
        assertTrue(owner != address(uint160(amt)), "address must not collapse into widened amount");
    }
}
