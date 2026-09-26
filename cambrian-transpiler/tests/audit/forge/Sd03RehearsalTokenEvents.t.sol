// SPDX-License-Identifier: UNLICENSED
// SD-03 / SMAFD BLS §9 — forge log oracle for rehearsal_token_events_evm.cam
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/RehearsalTokenEvents.sol";

contract Sd03RehearsalTokenEventsTest is Test {
    address internal constant HOLDER = address(0xD01);
    address internal constant USER = address(0xBEEF);
    uint256 internal constant SUPPLY = 1_000_000;

    RehearsalTokenEvents internal token;
    CambrianFactory internal factory;

    function setUp() public {
        factory = new CambrianFactory();
        token = RehearsalTokenEvents(
            address(factory.deployRehearsalTokenEvents(HOLDER, SUPPLY))
        );
    }

    function test_sd03_constructor_mint_transfer_log() public {
        vm.recordLogs();
        CambrianFactory f = new CambrianFactory();
        RehearsalTokenEvents(
            address(f.deployRehearsalTokenEvents(HOLDER, SUPPLY))
        );
        Vm.Log[] memory logs = vm.getRecordedLogs();
        bytes32 transferTopic = keccak256("Transfer(address,address,uint256)");
        bool found;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics[0] != transferTopic) {
                continue;
            }
            assertEq(address(uint160(uint256(logs[i].topics[1]))), address(0));
            assertEq(address(uint160(uint256(logs[i].topics[2]))), HOLDER);
            assertEq(abi.decode(logs[i].data, (uint256)), SUPPLY);
            found = true;
        }
        assertTrue(found, "constructor mint Transfer log missing");
    }

    function test_sd03_transfer_emits_including_zero_value_self() public {
        vm.prank(HOLDER);
        vm.recordLogs();
        token.transfer(HOLDER, 0);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(logs.length, 1);
        assertEq(logs[0].topics[0], keccak256("Transfer(address,address,uint256)"));
        assertEq(address(uint160(uint256(logs[0].topics[1]))), HOLDER);
        assertEq(address(uint160(uint256(logs[0].topics[2]))), HOLDER);
        assertEq(abi.decode(logs[0].data, (uint256)), 0);
    }

    function test_sd03_approve_whitelist_claim_burn_logs() public {
        vm.prank(HOLDER);
        vm.recordLogs();
        token.approve(USER, 42);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(logs.length, 1);
        assertEq(logs[0].topics[0], keccak256("Approval(address,address,uint256)"));

        vm.prank(HOLDER);
        vm.recordLogs();
        token.setWhitelisted(USER, true);
        logs = vm.getRecordedLogs();
        assertEq(logs.length, 1);
        assertEq(logs[0].topics[0], keccak256("Whitelisted(address,bool)"));

        vm.prank(USER);
        vm.recordLogs();
        token.claim();
        logs = vm.getRecordedLogs();
        assertEq(logs.length, 2);
        assertEq(logs[0].topics[0], keccak256("Claimed(address,uint256)"));
        assertEq(logs[1].topics[0], keccak256("Transfer(address,address,uint256)"));

        vm.prank(HOLDER);
        vm.recordLogs();
        token.burn(10);
        logs = vm.getRecordedLogs();
        assertEq(logs.length, 1);
        assertEq(logs[0].topics[0], keccak256("Transfer(address,address,uint256)"));
        assertEq(address(uint160(uint256(logs[0].topics[2]))), address(0));
        assertEq(abi.decode(logs[0].data, (uint256)), 10);
    }
}
