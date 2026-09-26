// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_pw3-stdlib-abs-evm_project.sol";

/// PW3-G-009: `std::math::abs(i64::MIN)` returns MIN on EVM (negation overflow not surfaced).
contract Pw3StdAbsI64MinTest is Test {
    CambrianFactory internal factory;
    StdAbsI64Min internal target;

    function setUp() public {
        factory = new CambrianFactory();
        target = StdAbsI64Min(address(factory.deployStdAbsI64Min()));
    }

    function test_PW3_G009_stdAbsI64MinReturnsSelf() public {
        assertEq(target.absMin(), type(int64).min);
    }
}
