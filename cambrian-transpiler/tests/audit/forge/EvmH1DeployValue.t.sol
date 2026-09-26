// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_evm-h1-audit_project.sol";

/// T-EVM-001 / EVM-H1: deploy with value must reach the CREATE2 instance.
contract EvmH1DeployValueTest is Test {
    CambrianFactory internal factory;
    Deployer internal deployer;

    uint64 internal constant DEPLOYER_ID = 1;
    uint64 internal constant VAULT_ID = 42;
    uint256 internal constant SEND_VALUE = 1 ether;

    function setUp() public {
        factory = new CambrianFactory();
        deployer = Deployer(factory.deployDeployer(DEPLOYER_ID));
    }

    function test_EVM_H1_deployForwardsValueToInstance() public {
        vm.deal(address(deployer), SEND_VALUE);
        address predicted = factory.predictVault(VAULT_ID);

        deployer.spawnFundedVault(VAULT_ID);

        assertEq(
            predicted.balance,
            SEND_VALUE,
            "EVM-H1: deployed instance should receive the deploy value"
        );
        assertEq(address(factory).balance, 0, "EVM-H1: factory must not retain deploy value");
    }
}
