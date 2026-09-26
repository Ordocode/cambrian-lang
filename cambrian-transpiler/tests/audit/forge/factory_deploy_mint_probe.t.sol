// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

// BUG-U4: factory.deploy*(holder) must credit holder, not factory.

pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/MintProbe.sol";
import "../src/_factory_deploy_mint_project.sol";

contract FactoryDeployMintProbeTest is Test {
    function test_factory_deploy_mints_to_holder_not_factory() public {
        address holder = address(0xD1);
        CambrianFactory factory = new CambrianFactory();
        address token = factory.deployMintProbe(holder);
        MintProbe probe = MintProbe(token);
        assertEq(probe.holder(), holder, "holder param must be stored");
        assertEq(probe.supply(), 1000, "holder must receive SUPPLY");
    }
}
