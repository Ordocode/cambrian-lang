// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_arch-ax03-03-evm_project.sol";

/// T-ARCH-007 / H-AX-03-03: div-by-zero in a `match` arm must revert on EVM.
contract Ax03MatchDivZeroTest is Test {
    CambrianFactory internal factory;
    MatchDivZero internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = MatchDivZero(address(factory.deployMatchDivZero()));
    }

    function test_ARCH_AX03_03_matchDivZeroReverts() public {
        DivMode memory mode = DivMode({tag: DivMode_Tag.Zero, safe_0: 0});
        vm.expectRevert(stdError.divisionError);
        target.pick(mode);
    }
}
