// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_fuzz-f002-seq-counter-evm_project.sol";

/// T-F-002: Foundry fuzz over transpiled `SeqCounter` route sequences.
contract F002RouteSeqFuzz is Test {
    CambrianFactory internal factory;
    SeqCounter internal seqCounter;

    function setUp() public {
        factory = new CambrianFactory();
        seqCounter = SeqCounter(address(factory.deploySeqCounter()));
        assertEq(seqCounter.m_count(), 0, "create initializes m_count");
    }

    function testFuzz_incSequence(uint8 n) public {
        for (uint8 i = 0; i < n; i++) {
            seqCounter.inc();
            assertLt(seqCounter.m_count(), 10000, "invariant m_count < 10000 after inc");
        }
        assertEq(seqCounter.m_count(), uint256(n), "inc sequence must match call count");
    }

    function testFuzz_mixedIncReset(uint8 ops) public {
        uint256 expected = 0;
        for (uint8 bit = 0; bit < 8; bit++) {
            if (((ops >> bit) & 1) == 0) {
                seqCounter.inc();
                expected++;
            } else {
                seqCounter.reset();
                expected = 0;
            }
            assertLt(seqCounter.m_count(), 10000, "invariant m_count < 10000 mid-sequence");
            assertEq(seqCounter.m_count(), expected, "mixed inc/reset sequence drift");
        }
    }
}
