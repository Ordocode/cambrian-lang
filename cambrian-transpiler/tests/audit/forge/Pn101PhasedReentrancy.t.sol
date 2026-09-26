// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pn-cei-001_project.sol";

contract Attacker is ICallback {
    Vault internal vault;
    uint256 public reenterCount;
    uint64 public payouts;

    function onPay(uint64 amount) external returns (bool) {
        payouts += amount;
        if (reenterCount == 0) {
            reenterCount = 1;
            vault.withdraw();
        }
        return true;
    }

    function attack(Vault v) external {
        vault = v;
        v.withdraw();
    }
}

/// T-PN-CEI-001 / PN-101: phased var-call must not allow reentrancy double-pay.
contract Pn101PhasedReentrancyTest is Test {
    CambrianFactory internal factory;

    function setUp() public {
        factory = new CambrianFactory();
    }

    function test_PN101_attackerCannotDoubleWithdraw() public {
        Attacker atk = new Attacker();
        Vault v = Vault(factory.deployVault(1, address(atk), 100));
        atk.attack(v);
        assertEq(v.getBal(), 0, "balance cleared after withdraw");
        assertEq(atk.payouts(), 100, "must not pay twice via reentrancy");
    }
}
