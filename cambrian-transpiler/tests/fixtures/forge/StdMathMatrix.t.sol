// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_std-math-matrix_project.sol";

/// STD-EVM-MATRIX-1: semantic gate for phase-1 `std::math` on EVM.
contract StdMathMatrixTest is Test {
    CambrianFactory internal factory;
    StdMathMatrix internal matrix;

    function setUp() public {
        factory = new CambrianFactory();
        matrix = StdMathMatrix(factory.deployStdMathMatrix());
    }

    function test_std_math_min() public {
        assertEq(matrix.runMin(3, 7), 3);
        assertEq(matrix.runMin(9, 2), 2);
    }

    function test_std_math_max() public {
        assertEq(matrix.runMax(3, 7), 7);
        assertEq(matrix.runMax(9, 2), 9);
    }

    function test_std_math_abs() public {
        assertEq(matrix.runAbsI(42), 42);
        assertEq(matrix.runAbsI(-42), 42);
        assertEq(matrix.runAbsI(0), 0);
    }

    function test_std_math_clamp() public {
        assertEq(matrix.runClamp(5, 1, 10), 5);
        assertEq(matrix.runClamp(0, 1, 10), 1);
        assertEq(matrix.runClamp(99, 1, 10), 10);
    }

    function test_std_math_muldiv() public {
        assertEq(matrix.runMuldiv(10, 20, 4), 50);
        assertEq(matrix.runMuldiv(4000000000, 4000000000, 2000000000), 8000000000);
    }

    function test_std_math_muldivmod() public {
        (uint64 q, uint64 r) = matrix.runMuldivmod(17, 5, 3);
        assertEq(q, 28);
        assertEq(r, 1);
        (q, r) = matrix.runMuldivmod(1048576, 1048576, 1048576);
        assertEq(q, 1048576);
        assertEq(r, 0);
    }

    function test_std_math_divmod() public {
        (uint64 q, uint64 r) = matrix.runDivmod(17, 5);
        assertEq(q, 3);
        assertEq(r, 2);
    }

    function test_std_math_divc() public {
        assertEq(matrix.runDivc(10, 3), 4);
        assertEq(matrix.runDivc(10, 5), 2);
    }

    function test_std_math_divr() public {
        assertEq(matrix.runDivr(10, 4), 3);
        assertEq(matrix.runDivr(7, 4), 2);
    }

    function test_std_math_sign() public {
        assertEq(matrix.runSign(0), 0);
        assertEq(matrix.runSign(5), 1);
        assertEq(matrix.runSign(-3), -1);
    }

    function test_std_math_minmax() public {
        (uint64 lo, uint64 hi) = matrix.runMinmax(3, 7);
        assertEq(lo, 3);
        assertEq(hi, 7);
        (lo, hi) = matrix.runMinmax(9, 2);
        assertEq(lo, 2);
        assertEq(hi, 9);
    }

    function test_std_math_modpow2() public {
        assertEq(matrix.runModpow2(255, 4), 15);
        assertEq(matrix.runModpow2(19, 3), 3);
    }

    function test_std_math_pow() public {
        assertEq(matrix.runPow(2, 10), 1024);
        assertEq(matrix.runPow(5, 0), 1);
    }

    function test_std_math_muldiv_u256_wide() public {
        assertEq(matrix.runMuldivU256Wide(), uint256(1) << 128);
    }
}
