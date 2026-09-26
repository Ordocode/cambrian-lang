// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_std-str-matrix_project.sol";

/// T-STD-EVM-002: semantic gate for phase-1 `std::str` on EVM.
contract StdStrMatrixTest is Test {
    CambrianFactory internal factory;
    StdStrMatrix internal matrix;

    function setUp() public {
        factory = new CambrianFactory();
        matrix = StdStrMatrix(factory.deployStdStrMatrix());
    }

    function test_std_str_format_default() public {
        assertEq(matrix.runFormatDefault(), "42");
    }

    function test_std_str_format_pad6() public {
        assertEq(matrix.runFormatPad6(), "000042");
    }

    function test_std_str_parse_uint_dec() public {
        assertEq(matrix.runParseDec(), 123);
    }

    function test_std_str_parse_uint_hex() public {
        assertEq(matrix.runParseHex(), 255);
    }

    function test_std_str_parse_u8_overflow_none() public {
        assertTrue(matrix.runParseU8Overflow());
    }

    function test_std_str_parse_u8_max() public {
        assertEq(matrix.runParseU8Max(), 255);
    }

    function test_std_str_parse_u8_zero() public {
        assertEq(matrix.runParseU8Zero(), 0);
    }

    function test_std_str_parse_u16_max() public {
        assertEq(matrix.runParseU16Max(), 65535);
    }

    function test_std_str_parse_u16_overflow_none() public {
        assertTrue(matrix.runParseU16Overflow());
    }

    function test_std_str_parse_u32_max() public {
        assertEq(matrix.runParseU32Max(), 4294967295);
    }

    function test_std_str_parse_u32_overflow_none() public {
        assertTrue(matrix.runParseU32Overflow());
    }

    function test_std_str_parse_u128_max() public {
        assertEq(matrix.runParseU128Max(), 340282366920938463463374607431768211455);
    }

    function test_std_str_parse_u128_overflow_none() public {
        assertTrue(matrix.runParseU128Overflow());
    }

    function test_std_str_parse_u256_max() public {
        assertEq(
            matrix.runParseU256Max(),
            type(uint256).max
        );
    }

    function test_std_str_parse_u256_overflow_none() public {
        assertTrue(matrix.runParseU256Overflow());
    }

    function test_std_str_parse_i8_min() public {
        assertEq(matrix.runParseI8Min(), -128);
    }

    function test_std_str_parse_i8_overflow_none() public {
        assertTrue(matrix.runParseI8Overflow());
    }

    function test_std_str_parse_i16_min() public {
        assertEq(matrix.runParseI16Min(), -32768);
    }

    function test_std_str_parse_i16_overflow_none() public {
        assertTrue(matrix.runParseI16Overflow());
    }

    function test_std_str_parse_i32_min() public {
        assertEq(matrix.runParseI32Min(), -2147483648);
    }

    function test_std_str_parse_i32_overflow_none() public {
        assertTrue(matrix.runParseI32Overflow());
    }

    function test_std_str_parse_i64_min() public {
        assertEq(matrix.runParseI64Min(), -9223372036854775808);
    }

    function test_std_str_parse_i64_overflow_none() public {
        assertTrue(matrix.runParseI64Overflow());
    }

    function test_std_str_parse_i128_overflow_none() public {
        assertTrue(matrix.runParseI128Overflow());
    }

    function test_std_str_parse_radix2() public {
        assertEq(matrix.runParseRadix2(), 255);
    }

    function test_std_str_parse_radix36() public {
        assertEq(matrix.runParseRadix36(), 35);
    }

    function test_std_str_parse_bad_radix_none() public {
        assertTrue(matrix.runParseBadRadix());
    }

    function test_std_str_parse_empty_none() public {
        assertTrue(matrix.runParseEmpty());
    }

    function test_std_str_parse_invalid_digit_none() public {
        assertTrue(matrix.runParseInvalidDigit());
    }

    function test_std_str_parse_0x_wrong_radix_none() public {
        assertTrue(matrix.runParse0xWrongRadix());
    }

    function test_std_str_parse_uint_alias() public {
        assertEq(matrix.runParseUintAlias(), 99);
    }

    function test_std_str_parse_int_alias() public {
        assertEq(matrix.runParseIntAlias(), -7);
    }

    function test_std_str_round_trip() public {
        assertEq(matrix.runRoundTrip(), 42);
    }
}
