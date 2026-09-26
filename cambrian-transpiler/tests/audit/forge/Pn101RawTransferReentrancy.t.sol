// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pn-cei-raw_project.sol";

contract Attacker {
    Vault internal vault;

    function attack(Vault v) external payable {
        vault = v;
        vault.deposit{value: msg.value}();
        vault.withdraw(uint128(msg.value));
    }

    receive() external payable {
        uint256 bal = address(vault).balance;
        if (bal > 0) {
            uint128 slice = bal > msg.value ? uint128(msg.value) : uint128(bal);
            if (slice > 0) {
                vault.withdraw(slice);
            }
        }
    }
}

/// T-PN-CEI-001 / PN-101: phased raw ETH transfer reentrancy.
contract Pn101RawTransferReentrancyTest is Test {
    function test_PN101_rawPhasedTransferResistsReentrancy() public {
        Vault v = new Vault(1);
        vm.deal(address(this), 6 ether);
        v.deposit{value: 5 ether}();

        Attacker atk = new Attacker();
        vm.deal(address(atk), 1 ether);
        atk.attack{value: 1 ether}(v);

        assertEq(v.getBal(), 0, "vault balance cleared");
        assertLe(
            address(atk).balance,
            1 ether,
            "PN-101: attacker must not drain more than its 1 ETH credit via phased reentrancy"
        );
    }
}
