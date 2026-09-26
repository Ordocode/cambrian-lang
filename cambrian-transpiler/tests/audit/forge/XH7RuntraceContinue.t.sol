// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_x-h7-evm-audit_project.sol";

/// T-X-007 / LEAN-H9: after a failing `fail` step, `inc` must still run when
/// `fail_on_revert: false` (Foundry `try/catch` schedule). Lean `runTrace` stops
/// at the first `.error`.
contract XH7RuntraceContinueTest is Test {
    CambrianFactory internal factory;
    TraceCont internal traceCont;

    function setUp() public {
        factory = new CambrianFactory();
        traceCont = TraceCont(address(factory.deployTraceCont()));
        vm.store(address(traceCont), bytes32(uint256(0)), bytes32(uint256(10)));
    }

    function test_X007_traceContinuesAfterFailRevertThenInc() public {
        uint64 before = traceCont.m_count();
        assertEq(before, 10, "invariant init seeds m_count");

        // `amount` in 100..200 violates `amount <= m_count` (10) → revert.
        try traceCont.fail(150) {} catch {}

        traceCont.inc();

        assertEq(
            traceCont.m_count(),
            before + 1,
            "X007: inc must run after failing fail when fail_on_revert=false"
        );
    }
}
