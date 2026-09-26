// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pn-diff-extern-evm_project.sol";

contract RevertingToken {
    function transfer(address, uint64) external pure returns (bool) {
        revert("Token: forced revert");
    }
}

/// T-PN-DIFF-001 / PN-106: EVM must propagate extern revert (control leg).
contract Pn106ExternRevertTest is Test {
    CambrianFactory internal factory;

    function setUp() public {
        factory = new CambrianFactory();
    }

    function test_PN106_evmPayRevertsWhenTokenReverts() public {
        RevertingToken tok = new RevertingToken();
        Wallet w = Wallet(factory.deployWallet(1, address(tok)));
        vm.expectRevert();
        w.pay(address(0xBEEF), 1);
    }
}
