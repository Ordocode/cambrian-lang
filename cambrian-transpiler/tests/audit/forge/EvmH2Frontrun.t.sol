// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_evm-h2-audit_project.sol";

/// T-EVM-002 / EVM-H2: honest deployer must control initial Vault state.
contract EvmH2FrontrunTest is Test {
    CambrianFactory internal factory;

    uint64 internal constant VAULT_ID = 42;
    uint64 internal constant HONEST_ID = 1;
    uint64 internal constant HONEST_BALANCE = 100;

    function setUp() public {
        factory = new CambrianFactory();
    }

    function test_EVM_H2_attackerCannotPoisonVaultInit() public {
        // Non-owner EOAs cannot occupy identity-only CREATE2 slots.
        address attacker = address(0xBEEF);
        vm.prank(attacker);
        vm.expectRevert();
        factory.deployVault(VAULT_ID, 0);

        // Factory owner may bootstrap entity instances.
        Honest honest = Honest(factory.deployHonest(HONEST_ID));

        // Cambrian-deployed contracts may spawn children with intended init.
        honest.createVault(VAULT_ID, HONEST_BALANCE);

        address vaultAddr = factory.predictVault(VAULT_ID);
        assertEq(
            Vault(vaultAddr).getBalance(),
            HONEST_BALANCE,
            "EVM-H2: vault must reflect honest initializer, not attacker"
        );
    }
}
