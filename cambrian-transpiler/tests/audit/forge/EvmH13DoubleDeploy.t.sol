// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_evm-h13-audit_project.sol";

/// T-EVM-013 / EVM-H13: two Vault deploys in one route must compile and run.
contract EvmH13DoubleDeployTest is Test {
    CambrianFactory internal factory;
    Deployer internal deployer;

    uint64 internal constant DEPLOYER_ID = 1;

    function setUp() public {
        factory = new CambrianFactory();
        deployer = Deployer(factory.deployDeployer(DEPLOYER_ID));
    }

    function test_EVM_H13_twoDeploysSameEntityCompileAndRun() public {
        deployer.spawnPair(10, 20);
        assertEq(deployer.m_spawns(), 2, "EVM-H13: route should complete both deploys");
    }
}
