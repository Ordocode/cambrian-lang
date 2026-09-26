// ExprProbe.sol
// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;
// Auto-generated stub: re-exports ExprProbe from the combined project file.
import "./_T-EVM-EX-006_project.sol";


// _T-EVM-EX-006_project.sol
// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

contract ExprProbe {
    uint64 public m_count;

    constructor() payable {
        uint64 next_m_count = 0;

        m_count = next_m_count;
    }

    function eq(uint64 n) external {
        uint64 b = uint64((m_count == n));
    }

}

