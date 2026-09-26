// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_evm-h7-audit_project.sol";

/// T-EVM-007 / EVM-H7: member transforms must observe a consistent pre-route snapshot.
/// Expected semantics: m_b reads pre-route m_a (0), so after bump: m_a=1, m_b=10.
/// Bug hypothesis: immediate write after `let` in m_a causes m_b to read m_a=1 → m_b=11.
contract EvmH7TransformLetTest is Test {
    CambrianFactory internal factory;
    Counter internal counter;

    function setUp() public {
        factory = new CambrianFactory();
        counter = Counter(factory.deployCounter());
    }

    function test_EVM_H7_bumpUsesPreRouteSnapshotForSecondMember() public {
        counter.bump();

        assertEq(counter.getA(), 1, "m_a should be 1 after bump");
        assertEq(
            counter.getB(),
            10,
            "EVM-H7: m_b should use pre-route m_a (0+10), not post-update m_a (1+10)"
        );
    }
}
