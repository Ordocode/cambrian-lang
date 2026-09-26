// ExprFuzz.sol
// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;
// Auto-generated stub: re-exports ExprFuzz from the combined project file.
import "./_not_3_project.sol";


// _not_3_project.sol
// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

contract ExprFuzz {
    uint64 public m_count;
    bool public m_flag;

    constructor() payable {
        uint64 next_m_count = 0;
        bool next_m_flag = false;

        m_count = next_m_count;
        m_flag = next_m_flag;
    }

    function probe(uint64 n) external returns (uint64) {
        uint256 x = (!m_flag);
        return m_count;
    }

}

