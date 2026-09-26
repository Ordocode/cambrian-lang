// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-invariant-trace-multiaction-evm_project.sol";

/// PW3-O-013 row 1: forge trace oracle — fail/inc/tip sequence with fail_on_revert=false.
contract Pw3InvariantTraceMultiTest is Test {
    CambrianFactory internal factory;
    TraceSweep internal sweep;
    address internal sink;

    function setUp() public {
        factory = new CambrianFactory();
        sink = address(0xBEEF);
        sweep = TraceSweep(factory.deployTraceSweep(sink));
        vm.store(address(sweep), bytes32(uint256(0)), bytes32(uint256(uint160(sink))));
    }

    function test_PW3_O013_multiActionTraceSequence() public {
        for (uint256 i = 0; i < 10; i++) {
            sweep.inc();
        }
        assertEq(sweep.m_count(), 10, "seed count");

        try sweep.fail(150) {} catch {}

        sweep.inc();
        vm.deal(address(sweep), 1 ether);
        sweep.tip(5);

        assertEq(sweep.m_count(), 11, "O013: inc after failing fail must apply");
        assertEq(sweep.m_tips(), 1, "O013: tip action must increment m_tips");
    }
}
