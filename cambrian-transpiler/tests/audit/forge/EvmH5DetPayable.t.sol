// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_evm-h5-audit_project.sol";

/// T-EVM-005 / EVM-H5: deterministic routes reading msg::value must be payable.
contract EvmH5DetPayableTest is Test {
    CambrianFactory internal factory;
    Recorder internal recorder;

    uint64 internal constant REC_ID = 1;
    uint256 internal constant PAYMENT = 0.5 ether;

    function setUp() public {
        factory = new CambrianFactory();
        recorder = Recorder(payable(factory.deployRecorder(REC_ID)));
    }

    function test_EVM_H5_receiveRecordsPayment() public {
        (bool ok,) = address(recorder).call{value: PAYMENT}("");
        assertTrue(ok, "EVM-H5: payable receive must accept ETH");

        assertEq(
            recorder.getTotal(),
            PAYMENT,
            "EVM-H5: msg::value must be recorded"
        );
    }
}
