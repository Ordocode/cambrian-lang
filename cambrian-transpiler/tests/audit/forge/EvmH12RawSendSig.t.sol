// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_evm-h12-audit_project.sol";

/// T-EVM-012 / EVM-H12: raw named send must hit setOwner(address) on plain address dest.
contract EvmH12RawSendSigTest is Test {
    CambrianFactory internal factory;
    Target internal target;
    Caller internal caller;

    uint64 internal constant TARGET_ID = 1;
    uint64 internal constant CALLER_ID = 2;
    address internal constant ALICE = address(0xA11CE);

    function setUp() public {
        factory = new CambrianFactory();
        target = Target(factory.deployTarget(TARGET_ID));
        caller = Caller(factory.deployCaller(CALLER_ID));
    }

    function test_EVM_H12_rawNamedSendSetsOwnerOnPlainAddress() public {
        caller.poke(address(target), ALICE);

        assertEq(
            target.getOwner(),
            ALICE,
            "EVM-H12: encodeWithSignature selector must match setOwner(address)"
        );
    }
}
