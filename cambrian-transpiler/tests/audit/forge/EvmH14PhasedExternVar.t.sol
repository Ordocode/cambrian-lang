// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_evm-h14-audit_project.sol";

contract MockOracle {
    bool internal alive;

    constructor(bool v) {
        alive = v;
    }

    function isAlive() external view returns (bool) {
        return alive;
    }
}

/// T-EVM-014 / EVM-H14: phased extern VarCall must declare bool, not uint256.
contract EvmH14PhasedExternVarTest is Test {
    CambrianFactory internal factory;
    Caller internal caller;
    MockOracle internal oracle;

    uint64 internal constant CALLER_ID = 1;

    function setUp() public {
        factory = new CambrianFactory();
        oracle = new MockOracle(true);
        caller = Caller(factory.deployCaller(CALLER_ID, address(oracle)));
    }

    function test_EVM_H14_phasedExternVarCallReturnsBool() public {
        assertTrue(caller.check(), "EVM-H14: isAlive() capture must be bool");
    }
}
