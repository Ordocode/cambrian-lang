// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_evm-h10-audit_project.sol";

/// T-EVM-010 / EVM-H10: member-transform tuple destructure must compile and yield divmod(100,7).
contract EvmH10TupleDestructureTest is Test {
    CambrianFactory internal factory;
    TupleTransform internal tt;

    function setUp() public {
        factory = new CambrianFactory();
        tt = TupleTransform(factory.deployTupleTransform());
    }

    function test_EVM_H10_memberTransformTupleDestructureCompilesAndSplits() public {
        tt.split();
        assertEq(tt.getQ(), 14, "EVM-H10: quotient of divmod(100,7)");
        assertEq(tt.getR(), 2, "EVM-H10: remainder of divmod(100,7)");
    }
}
