// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax01-03-evm_project.sol";

/// T-ARCH-011 / H-AX-01-03: underfunded deploy `{ value }` must revert on EVM.
contract Ax01DeployValueUnderfundTest is Test {
    CambrianFactory internal factory;
    ValueSpawner internal spawner;

    uint64 internal constant SPAWNER_ID = 1;
    uint64 internal constant VAULT_ID = 42;
    uint128 internal constant SMALL_BALANCE = 100;
    uint128 internal constant OVERFUND = 1000;

    function setUp() public {
        factory = new CambrianFactory();
        spawner = ValueSpawner(factory.deployValueSpawner(SPAWNER_ID));
        vm.deal(address(spawner), SMALL_BALANCE);
    }

    function test_ARCH_AX01_03_underfundedDeployValueReverts() public {
        address predicted = factory.predictValueVault(VAULT_ID);

        vm.expectRevert();
        spawner.spawn(VAULT_ID, OVERFUND);

        assertEq(spawner.getSpawns(), 0, "failed deploy must not commit spawns");
        assertEq(predicted.balance, 0, "vault must not receive ETH after revert");
        assertEq(predicted.code.length, 0, "vault must not be deployed after revert");
    }
}
