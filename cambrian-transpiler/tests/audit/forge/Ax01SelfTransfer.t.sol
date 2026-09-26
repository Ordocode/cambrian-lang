// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax01-05-evm_project.sol";

/// T-ARCH-031 / H-AX-01-05: EVM permits self-transfer and runs `receive()`.
contract Ax01SelfTransferTest is Test {
    CambrianFactory internal factory;
    SelfReceiver internal receiver;

    function setUp() public {
        factory = new CambrianFactory();
        receiver = SelfReceiver(payable(factory.deploySelfReceiver()));
        vm.deal(address(receiver), 1 ether);
    }

    function test_ARCH_AX01_05_selfTransferRunsReceiveTransform() public {
        assertEq(receiver.getDeposits(), 0, "baseline");
        receiver.paySelf(5);
        assertEq(receiver.getDeposits(), 5, "EVM: self-transfer must run receive transform");
    }
}
