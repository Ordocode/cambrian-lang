// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/_std-crypto-matrix_project.sol";

/// T-STD-EVM-003: semantic gate for phase-1 `std::crypto` on EVM.
contract StdCryptoMatrixTest is Test {
    CambrianFactory internal factory;
    StdCryptoMatrix internal matrix;

    bytes internal constant ABC_SHA256 =
        hex"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    function setUp() public {
        factory = new CambrianFactory();
        matrix = StdCryptoMatrix(factory.deployStdCryptoMatrix());
    }

    function test_std_crypto_sha256_nist_abc() public {
        assertEq(matrix.runStdSha256(), ABC_SHA256);
    }

    function test_std_crypto_sha256_matches_evm() public {
        uint256 stdAsU256 = uint256(bytes32(matrix.runStdSha256()));
        assertEq(stdAsU256, matrix.runEvmSha256());
        assertEq(stdAsU256, uint256(bytes32(ABC_SHA256)));
    }
}
