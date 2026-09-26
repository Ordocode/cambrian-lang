// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-invariant-caret-phased-evm_project.sol";

/// PW3-G-012 row 3: phased bump — snap phase must mirror post-inc m_a (^ semantics).
contract Pw3InvariantCaretPhasedTest is Test {
    CambrianFactory internal factory;
    CaretPhase internal caret;

    function setUp() public {
        factory = new CambrianFactory();
        caret = CaretPhase(factory.deployCaretPhase());
    }

    function test_PW3_G012_caretPhasedBumpMirrorsPostIncA() public {
        assertEq(caret.m_a(), 5, "constructor seeds m_a");
        caret.bump(3);
        assertEq(caret.m_a(), 8, "inc phase adds n");
        assertEq(caret.m_b(), 8, "G012: snap must copy post-inc m_a (^m_a)");
        assertEq(caret.m_steps(), 1, "snap phase increments step counter");
    }
}
