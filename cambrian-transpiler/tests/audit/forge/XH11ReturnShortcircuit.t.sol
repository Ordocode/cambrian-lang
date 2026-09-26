// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_x-h11-evm-audit_project.sol";

/// T-X-009 / LEAN-H1 / X-H11: `throw 5` short-circuits; only `Logged(1)` before revert.
contract XH11ReturnShortcircuitTest is Test {
    CambrianFactory internal factory;
    ShortCircuit internal shortCircuit;

    bytes32 internal constant LOGGED_TOPIC = keccak256("Logged(uint64)");

    function setUp() public {
        factory = new CambrianFactory();
        shortCircuit = ShortCircuit(address(factory.deployShortCircuit()));
    }

    function test_X009_throwShortCircuitsAfterFirstEmit() public {
        // `where n < 1000` — use 42 to reach body `throw 5` (not the guard `throw 1`).
        vm.recordLogs();
        vm.expectRevert(bytes("throw(5)"));
        shortCircuit.go(42);

        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(logs.length, 1, "X009: exactly one Logged before revert");
        assertEq(logs[0].topics[0], LOGGED_TOPIC, "X009: event is Logged");
        assertEq(abi.decode(logs[0].data, (uint64)), 1, "X009: only Logged(1), not Logged(2)");
    }
}
