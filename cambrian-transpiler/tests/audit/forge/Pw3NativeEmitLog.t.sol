// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-native-emit-evm_project.sol";

/// PW3-G-015 row 3: forge behavioral oracle — emit produces EVM log.
contract Pw3NativeEmitLogTest is Test {
    CambrianFactory internal factory;
    Bank internal bank;

    function setUp() public {
        factory = new CambrianFactory();
        bank = Bank(factory.deployBank());
    }

    function test_PW3_G015_emitDepositedEvent() public {
        vm.recordLogs();
        bank.deposit(42);
        Vm.Log[] memory entries = vm.getRecordedLogs();
        assertEq(entries.length, 1, "PW3-G-015 forge: deposit must emit one event");
        assertEq(entries[0].topics[0], keccak256("Deposited(uint128)"));
    }
}
