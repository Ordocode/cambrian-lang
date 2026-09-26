// ExprProbe.sol
// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;
// Auto-generated stub: re-exports ExprProbe from the combined project file.
import "./_T-EVM-EX-007_project.sol";


// _T-EVM-EX-007_project.sol
// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

contract ExprProbe {
    uint64 public m_count;
    bool public m_flag;

    constructor() payable {
        bool next_m_flag = false;
        uint64 next_m_count = 0;

        m_flag = next_m_flag;
        m_count = next_m_count;
    }

    function flip() external {
        uint256 b = (!m_flag);
    }

}

