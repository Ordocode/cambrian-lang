// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_x-h1-evm-audit_project.sol";

/// T-X-001 / EVM-H16: duplicate CREATE2 deploy at the same salt must revert.
contract XH1DoubleDeployTest is Test {
    CambrianFactory internal factory;
    Deployer internal deployer;

    uint64 internal constant VAULT_ID = 7;

    function setUp() public {
        factory = new CambrianFactory();
        deployer = Deployer(factory.deployDeployer(1));
    }

    function test_X001_secondCreate2DeployAtSameSaltReverts() public {
        address predicted = factory.predictVault(VAULT_ID);

        deployer.spawn(VAULT_ID);
        assertEq(Vault(predicted).getMarker(), 1, "first deploy initializes vault");
        assertEq(deployer.m_spawns(), 1, "spawn increments counter");

        vm.expectRevert();
        deployer.respawn(VAULT_ID);

        assertEq(deployer.m_spawns(), 1, "reverted respawn must not bump counter");
        assertEq(Vault(predicted).getMarker(), 1, "vault state unchanged after revert");
    }
}
