// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_evm-h6-audit_project.sol";

/// T-EVM-006 / EVM-H6: phased receive member transforms must record msg.value.
contract EvmH6PhasedReceiveTest is Test {
    CambrianFactory internal factory;
    Recorder internal recorder;

    uint64 internal constant REC_ID = 1;
    uint256 internal constant PAYMENT = 0.75 ether;

    function setUp() public {
        factory = new CambrianFactory();
        recorder = Recorder(payable(factory.deployRecorder(REC_ID)));
    }

    function test_EVM_H6_phasedReceiveRecordsPayment() public {
        (bool ok,) = address(recorder).call{value: PAYMENT}("");
        assertTrue(ok, "EVM-H6: phased receive must accept ETH");

        assertEq(
            recorder.getTotal(),
            PAYMENT,
            "EVM-H6: phased receive transform must credit msg.value"
        );
    }
}
