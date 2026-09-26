// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax08-02-evm_project.sol";

/// T-ARCH-024 / H-AX-08-02: non-det `from Token(m_token)` accepts spoof sender on EVM.
contract Ax08FromEntityNonDetTest is Test {
    Vault internal vault;
    address internal constant SPOOF = address(uint160(0xBEEF));

    function setUp() public {
        new Token();
        vault = new Vault(SPOOF);
    }

    function test_ARCH_AX08_02_spoofSenderPassesOnEvm() public {
        vm.prank(SPOOF);
        vault.onlyToken();
        assertEq(vault.m_hit(), 1);
    }
}
