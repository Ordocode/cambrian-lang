// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_x-h3-evm-audit_project.sol";

/// T-X-003 / X-H5: deploy init args must seed non-identity members on EVM.
contract XH3DeployInitParamsTest is Test {
    CambrianFactory internal factory;
    Deployer internal deployer;

    uint64 internal constant DEPLOYER_ID = 1;
    uint64 internal constant VAULT_ID = 7;
    uint64 internal constant INITIAL = 42_000;

    function setUp() public {
        factory = new CambrianFactory();
        deployer = Deployer(factory.deployDeployer(DEPLOYER_ID));
    }

    function test_X003_deployInitParamsSetsMemberBalance() public {
        address predicted = factory.predictVault(VAULT_ID);

        deployer.spawn(VAULT_ID, INITIAL);

        assertEq(
            Vault(predicted).getBalance(),
            INITIAL,
            "deploy init arg must initialize m_balance via constructor/initialize"
        );
        assertEq(deployer.m_spawns(), 1, "spawn increments deployer counter");
    }
}
