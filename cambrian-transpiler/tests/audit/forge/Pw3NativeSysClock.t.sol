// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-native-sys-evm_project.sol";

/// PW3-G-015 row 2: forge behavioral oracle — sys::timestamp tracks block.timestamp.
contract Pw3NativeSysClockTest is Test {
    CambrianFactory internal factory;
    Clock internal clock;

    function setUp() public {
        factory = new CambrianFactory();
        clock = Clock(factory.deployClock());
    }

    function test_PW3_G015_sysTimestampMatchesBlock() public {
        vm.warp(1_700_000_000);
        uint64 ts = clock.readTs();
        assertEq(uint256(ts), block.timestamp, "PW3-G-015 forge: readTs must match block.timestamp");
        assertEq(clock.m_last(), ts, "PW3-G-015 forge: member transform must record sys::timestamp");
    }
}
