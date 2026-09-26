// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_x-h4-member-tuple-let-evm-audit_project.sol";

/// T-X-004 / X-H6: route-level `let (owner,amt,flag)=split(...)` must compile and
/// preserve slots; member-transform `let (o,a,f)=split(...)` must not lower to
/// `split(...)[i]` uint256 slots (solc error today).
contract XH4MemberTupleLetTest is Test {
    CambrianFactory internal factory;
    MemberTupleLet internal demo;
    address internal caller = address(0xBEEF);

    function setUp() public {
        factory = new CambrianFactory();
        demo = MemberTupleLet(address(factory.deployMemberTupleLet()));
    }

    function test_X004_controlRoutePeekPreservesTupleSlots() public {
        (address owner, uint256 amt, bool flag) = demo.peek(caller, 42);

        assertEq(owner, caller, "address slot must stay address-typed");
        assertEq(amt, 42, "amount slot must carry seed unchanged");
        assertTrue(flag, "bool slot must be true for positive seed");
    }

    function test_X004_memberRunStoresAmountFromSplit() public {
        demo.run(caller, 99);
        assertEq(demo.getSlot(), 99, "member transform must store `a` from split");
    }
}
