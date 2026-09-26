// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax01-04-evm_project.sol";

/// T-ARCH-015 / H-AX-01-04: deployed Child init transform sees factory `msg.sender`.
contract Ax01DeploySenderTest is Test {
    CambrianFactory internal factory;
    ParentSpawner internal parent;

    uint64 internal constant PARENT_ID = 1;

    function setUp() public {
        factory = new CambrianFactory();
        parent = ParentSpawner(factory.deployParentSpawner(PARENT_ID));
    }

    function test_ARCH_AX01_04_deployedChildOwnerIsFactoryNotParent() public {
        parent.spawn();
        ChildOwner child = ChildOwner(factory.predictChildOwner());
        assertEq(child.getOwner(), address(factory), "det deploy: child m_owner must be factory");
        assertTrue(child.getOwner() != address(parent), "child m_owner must not copy parent address");
    }
}
