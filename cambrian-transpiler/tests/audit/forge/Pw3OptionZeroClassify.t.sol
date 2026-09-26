// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-option-zero-evm_project.sol";

/// T-PW3-O-001 / PW3-O-001: `some(0)` must classify as `some`, not `none`.
/// EVM ground truth (desired): `classify() == 222` after `setSome(0)`.
contract Pw3OptionZeroClassifyTest is Test {
    CambrianFactory internal factory;
    OptZero internal opt;

    function setUp() public {
        factory = new CambrianFactory();
        opt = OptZero(address(factory.deployOptZero()));
    }

    function test_PW3_O001_setSomeZeroClassifiesAsSome() public {
        opt.setSome(0);
        assertEq(
            opt.classify(),
            222,
            "PW3-O-001: some(0) must not collapse to none (EVM 0-sentinel bug)"
        );
    }
}
