// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-semantic-deploy-evm_project.sol";

/// PW3-G-002: semantically equal deploy args must still collide on EVM CREATE2.
contract Pw3SemanticDoubleDeployTest is Test {
    CambrianFactory internal factory;
    Deployer internal deployer;

    function setUp() public {
        factory = new CambrianFactory();
        deployer = Deployer(factory.deployDeployer(1));
    }

    function test_PW3_G002_semanticDuplicateDeployReverts() public {
        address predicted = factory.predictVault(0);
        assertTrue(predicted != address(0), "predictVault(0) must be non-zero");

        vm.expectRevert();
        deployer.respawn();

        assertEq(deployer.m_spawns(), 0, "reverted respawn must not bump counter");
    }
}
