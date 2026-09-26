// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_evm-h3-audit_project.sol";

/// T-EVM-003 / EVM-H3: `rescue` around internal `call` must invoke recover on revert.
contract EvmH3RescueCallRouteTest is Test {
    CambrianFactory internal factory;
    Guard internal guard;

    function setUp() public {
        factory = new CambrianFactory();
        guard = Guard(factory.deployGuard());
    }

    function test_EVM_H3_rescueCallRouteRunsRecoverOnCalleeRevert() public {
        // failingHelper throws when amount >= 500 (where guard fails).
        guard.invokeWithRescue(500);

        assertEq(
            guard.getRecoverCount(),
            1,
            "EVM-H3: recover handler must run when rescued call reverts"
        );
    }
}
